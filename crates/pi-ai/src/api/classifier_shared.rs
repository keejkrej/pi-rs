//! Port of packages/ai/src/api/classifier-shared.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde::Serialize;
use serde_json::Value;
use url::Url;

use pi_js::abort::AbortSignal;
use pi_js::error::{Error, Result};
use pi_js::fetch::{Body, Headers, Request, Response};

use crate::models::calculate_cost;
use crate::types::{
    AnyModel, ClassifierModel, ClassifierOptions, JsonValue, ProviderHeaders, ProviderResponse, Usage, UsageCost,
};
use crate::utils::abort::race_with_abort_signal;
use crate::utils::abort_signals::combine_abort_signals;
use crate::utils::error_body::SdkErrorInfo;
use crate::utils::headers::{headers_to_record, provider_headers_to_record};
use crate::utils::identity::{ClientIdentity, ClientIdentityTarget, get_identity_headers};
use crate::utils::provider_retry::{
    ProviderHttpError, ProviderRetryOptions, retry_provider_request_with_no_retry_statuses,
};

pub(crate) fn serialize_to_value<T: Serialize + ?Sized>(value: &T) -> JsonValue {
    match pi_js::json::parse(&pi_js::json::stringify(value)) {
        Ok(parsed) => parsed,
        Err(_) => JsonValue::Null,
    }
}

pub(crate) fn model_headers_to_provider_headers(headers: Option<&IndexMap<String, String>>) -> Option<ProviderHeaders> {
    headers.map(|map| {
        map.iter()
            .map(|(k, v)| (k.clone(), Some(v.clone())))
            .collect::<ProviderHeaders>()
    })
}

/// Extract HTTP status code from a classifier request error, if any.
pub fn classifier_http_status(error: &Error) -> Option<u16> {
    error
        .downcast_ref::<SdkErrorInfo>()
        .and_then(|e| e.status.or(e.status_code))
        .or_else(|| error.downcast_ref::<ProviderHttpError>().and_then(|e| e.status))
}

pub fn is_record(value: &Value) -> bool {
    value.is_object()
}

pub fn required_number(label: &str, value: Option<&Value>, field: &str) -> Result<f64> {
    match value.and_then(Value::as_f64) {
        Some(n) if n.is_finite() => Ok(n),
        _ => Err(Error::msg(format!("{label} returned an invalid {field}"))),
    }
}

fn request_headers(
    model: &ClassifierModel,
    api_key: &str,
    options_headers: Option<&ProviderHeaders>,
    identity: Option<&ClientIdentity>,
) -> IndexMap<String, String> {
    let target = ClientIdentityTarget {
        provider: &model.provider,
        base_url: &model.base_url,
    };
    let identity_headers: ProviderHeaders = get_identity_headers(target, identity)
        .into_iter()
        .map(|(k, v)| (k, Some(v)))
        .collect();
    let mut auth_headers = ProviderHeaders::new();
    auth_headers.insert("authorization".to_owned(), Some(format!("Bearer {api_key}")));
    auth_headers.insert("content-type".to_owned(), Some("application/json".to_owned()));
    let model_hdrs = model_headers_to_provider_headers(model.headers.as_ref());
    provider_headers_to_record([
        Some(&identity_headers),
        Some(&auth_headers),
        model_hdrs.as_ref(),
        options_headers,
    ])
    .unwrap_or_default()
}

/// Posts one JSON classifier request with bearer auth, `on_payload`/`on_response` hooks, a fresh
/// timeout per attempt, and provider retries. Returns the parsed response body.
/// `no_retry_statuses` lists HTTP statuses that fail at once although they are normally retried.
pub async fn post_classifier_request(
    label: &str,
    url: &Url,
    model: &ClassifierModel,
    body: Value,
    options: Option<&ClassifierOptions>,
    no_retry_statuses: Option<&[u16]>,
) -> Result<Value> {
    let api_key = options
        .and_then(|o| o.base.api_key.as_deref())
        .filter(|k| !k.is_empty())
        .ok_or_else(|| Error::msg(format!("No API key for provider: {}", model.provider)))?;

    let mut payload = body;
    if let Some(on_payload) = options.and_then(|o| o.base.on_payload.as_ref())
        && let Some(next_payload) = on_payload(payload.clone(), model).await?
    {
        payload = next_payload;
    }

    let request_url = url.to_string();
    let hdr_map = request_headers(
        model,
        api_key,
        options.and_then(|o| o.base.headers.as_ref()),
        options.and_then(|o| o.base.identity.as_ref()),
    );
    let body_text = pi_js::json::stringify(&payload);
    let caller_signal = options.and_then(|o| o.base.signal.clone());
    let timeout_ms = options.and_then(|o| o.base.timeout_ms);
    let max_retries = options.and_then(|o| o.base.max_retries).or(Some(2));
    let max_retry_delay_ms = options.and_then(|o| o.base.max_retry_delay_ms);
    let custom_fetch = options.and_then(|o| o.base.fetch.clone());

    let last_http_body: Arc<Mutex<Option<(u16, String)>>> = Arc::new(Mutex::new(None));

    let attempt_result = retry_provider_request_with_no_retry_statuses(
        || {
            let request_url = request_url.clone();
            let hdr_map = hdr_map.clone();
            let body_text = body_text.clone();
            let caller_signal = caller_signal.clone();
            let custom_fetch = custom_fetch.clone();
            let last_http_body = Arc::clone(&last_http_body);
            let label_owned = label.to_owned();
            async move {
                let timeout_signal = timeout_ms.map(AbortSignal::timeout);
                let combined = combine_abort_signals([caller_signal.as_ref(), timeout_signal.as_ref()]);

                let mut hdrs = Headers::new();
                for (k, v) in &hdr_map {
                    hdrs.set(k, v);
                }
                let mut req = Request::post(request_url)
                    .with_headers(hdrs)
                    .body(Body::Text(body_text));
                if let Some(sig) = &combined.signal {
                    req = req.signal(sig.clone());
                }

                let perform = async {
                    let next = if let Some(fetch_fn) = &custom_fetch {
                        fetch_fn(req).await?
                    } else {
                        pi_js::fetch::fetch(req).await?
                    };
                    if !next.ok() {
                        let status = next.status();
                        let resp_headers = next.headers().clone();
                        let text = next.text().await.unwrap_or_default();
                        {
                            let mut guard = last_http_body.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            *guard = Some((status, text));
                        }
                        return Err(ProviderHttpError::new(
                            Some(status),
                            Some(resp_headers),
                            format!("{label_owned} returned {status}"),
                        )
                        .into_js_error());
                    }
                    let text = next.text().await?;
                    let parsed = pi_js::json::parse(&text)?;
                    Ok((next, parsed))
                };

                let res = match &combined.signal {
                    Some(sig) => race_with_abort_signal(perform, sig).await,
                    None => perform.await,
                };
                combined.cleanup();

                match res {
                    Ok(pair) => Ok(pair),
                    Err(err) => {
                        if let Some(ms) = timeout_ms {
                            let timed_out = timeout_signal.as_ref().is_some_and(AbortSignal::aborted);
                            let caller_aborted = caller_signal.as_ref().is_some_and(AbortSignal::aborted);
                            if timed_out && !caller_aborted {
                                return Err(ProviderHttpError::new(
                                    None,
                                    None,
                                    format!("Request timed out after {ms}ms"),
                                )
                                .into_js_error());
                            }
                        }
                        Err(err)
                    }
                }
            }
        },
        ProviderRetryOptions {
            max_retries,
            max_retry_delay_ms,
            signal: caller_signal.clone(),
        },
        no_retry_statuses,
    )
    .await;

    let (response, json): (Response, Value) = match attempt_result {
        Ok(pair) => pair,
        Err(err) => {
            if let Some(http_err) = err.downcast_ref::<ProviderHttpError>() {
                let captured = {
                    let guard = last_http_body.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    guard.as_ref().and_then(|(st, text)| {
                        if Some(*st) == http_err.status {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                };
                let sdk = SdkErrorInfo {
                    status: http_err.status,
                    body: captured,
                    message: http_err.message.clone(),
                    ..Default::default()
                };
                return Err(sdk.into_js_error());
            }
            return Err(err);
        }
    };

    if let Some(on_response) = options.and_then(|o| o.base.on_response.as_ref()) {
        on_response(
            ProviderResponse {
                status: response.status(),
                headers: headers_to_record(response.headers()),
            },
            model,
        )
        .await?;
    }

    Ok(json)
}

fn token_count(value: Option<&Value>) -> i64 {
    let Some(val) = value else {
        return 0;
    };
    if let Some(n) = val.as_i64() {
        return if n > 0 { n } else { 0 };
    }
    match val.as_f64() {
        Some(n) if n.is_finite() && n > 0.0 => n as i64,
        _ => 0,
    }
}

/// Usage from a `{ input_tokens, output_tokens }` object, priced from the model catalog like chat
/// usage. A missing or malformed usage object leaves the result without usage instead of failing it.
pub fn parse_classifier_usage(value: Option<&Value>, model: &ClassifierModel) -> Option<Usage> {
    let obj = value.and_then(Value::as_object)?;
    if !obj.contains_key("input_tokens") && !obj.contains_key("output_tokens") {
        return None;
    }
    let input = token_count(obj.get("input_tokens"));
    let output = token_count(obj.get("output_tokens"));
    let mut usage = Usage {
        input,
        output,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: input + output,
        cost: UsageCost::default(),
    };
    calculate_cost(&AnyModel::Classifier(model.clone()), &mut usage);
    Some(usage)
}
