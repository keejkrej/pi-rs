//! Port of packages/ai/src/api/system-one-shared.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{Map, Value};

use pi_js::abort::AbortSignal;
use pi_js::fetch::{Body, Headers, Request};
use pi_js::{Error, Result};

use crate::models::calculate_cost;
use crate::types::{
    AnyModel, ClassifierAnswer, ClassifierBoolCriteria, ClassifierContext, ClassifierModel, ClassifierOptions,
    ClassifierQuestion, ClassifierResult, ClassifierStopReason, JsonObject, JsonValue, ProviderHeaders,
    ProviderResponse, Usage, UsageCost,
};
use crate::utils::abort::race_with_abort_signal;
use crate::utils::abort_signals::combine_abort_signals;
use crate::utils::error_body::{SdkErrorInfo, format_provider_error, normalize_provider_error};
use crate::utils::headers::{headers_to_record, provider_headers_to_record};
use crate::utils::provider_retry::{ProviderHttpError, ProviderRetryOptions, retry_provider_request};

/// TypeSafe System One request body without the transport-specific envelope.
#[derive(Clone, Debug, Serialize)]
pub struct SystemOneWireRequest {
    pub state: JsonObject,
    pub questions: IndexMap<String, JsonValue>,
}

/// Differences between services that serve System One models.
pub trait SystemOneTransport: Send + Sync {
    /// Classifier API implemented by this transport.
    fn api(&self) -> &str;

    /// Service name used in error messages.
    fn label(&self) -> &str;

    /// Absolute request URL.
    fn url(&self, model: &ClassifierModel) -> Result<String>;

    /// Wraps the System One request in the service's request envelope.
    fn payload(&self, model: &ClassifierModel, request: SystemOneWireRequest) -> JsonValue;

    /// Extracts the System One output (`{ answers, usage }`) from the service's response envelope.
    fn output<'a>(&self, body: &'a JsonValue) -> Result<&'a Map<String, JsonValue>>;
}

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

pub fn is_record(value: &JsonValue) -> bool {
    value.is_object()
}

fn required_number(label: &str, value: Option<&JsonValue>, field: &str) -> Result<f64> {
    match value.and_then(Value::as_f64) {
        Some(n) if n.is_finite() => Ok(n),
        _ => Err(Error::msg(format!("{label} returned an invalid {field}"))),
    }
}

fn parse_probabilities(label: &str, value: Option<&JsonValue>, id: &str) -> Result<IndexMap<String, f64>> {
    let Some(obj) = value.and_then(Value::as_object) else {
        return Err(Error::msg(format!("{label} returned invalid probabilities for {id}")));
    };
    let mut out = IndexMap::with_capacity(obj.len());
    for (key, prob) in obj {
        let parsed = required_number(label, Some(prob), &format!("probability for {id}.{key}"))?;
        out.insert(key.clone(), parsed);
    }
    Ok(out)
}

fn parse_answers(
    label: &str,
    value: Option<&JsonValue>,
    context: &ClassifierContext,
) -> Result<IndexMap<String, ClassifierAnswer>> {
    let Some(obj) = value.and_then(Value::as_object) else {
        return Err(Error::msg(format!("{label} returned an unexpected response")));
    };
    let mut answers = IndexMap::with_capacity(context.questions.len());
    for (id, question) in &context.questions {
        let Some(answer) = obj.get(id).and_then(Value::as_object) else {
            return Err(Error::msg(format!("{label} did not return an answer for {id}")));
        };
        let answer_type = answer.get("type").and_then(Value::as_str);
        match question {
            ClassifierQuestion::Choice { .. } => {
                let Some(choice) = (answer_type == Some("choice"))
                    .then(|| answer.get("choice").and_then(Value::as_str))
                    .flatten()
                else {
                    return Err(Error::msg(format!("{label} did not return a choice answer for {id}")));
                };
                answers.insert(
                    id.clone(),
                    ClassifierAnswer::Choice {
                        choice: choice.to_owned(),
                        probabilities: parse_probabilities(label, answer.get("probabilities"), id)?,
                        confidence: required_number(label, answer.get("confidence"), &format!("confidence for {id}"))?,
                    },
                );
            }
            ClassifierQuestion::Score { .. } => {
                if answer_type != Some("score") {
                    return Err(Error::msg(format!("{label} did not return a score answer for {id}")));
                }
                answers.insert(
                    id.clone(),
                    ClassifierAnswer::Score {
                        score: required_number(label, answer.get("score"), &format!("score for {id}"))?,
                        confidence: required_number(label, answer.get("confidence"), &format!("confidence for {id}"))?,
                    },
                );
            }
            ClassifierQuestion::Bool { .. } => {
                if answer_type != Some("noul") {
                    return Err(Error::msg(format!("{label} did not return a bool answer for {id}")));
                }
                answers.insert(
                    id.clone(),
                    ClassifierAnswer::Bool {
                        probability: required_number(label, answer.get("noul"), &format!("probability for {id}"))?,
                    },
                );
            }
        }
    }
    Ok(answers)
}

fn token_count(value: Option<&JsonValue>) -> i64 {
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

/// Usage from System One's `{ input_tokens, output_tokens }`, priced from the model catalog like
/// chat usage. A missing or malformed usage object leaves the result without usage instead of
/// failing it.
fn parse_usage(value: Option<&JsonValue>, model: &ClassifierModel) -> Option<Usage> {
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

#[derive(Serialize)]
struct WireNoulQuestion<'a> {
    #[serde(rename = "type")]
    r#type: &'static str,
    instructions: &'a str,
    criteria: &'a ClassifierBoolCriteria,
}

/// Maps public `bool` questions to TypeSafe's wire-level `noul` type.
fn wire_request(context: &ClassifierContext) -> SystemOneWireRequest {
    let mut questions = IndexMap::with_capacity(context.questions.len());
    for (id, question) in &context.questions {
        let wired = match question {
            ClassifierQuestion::Bool { instructions, criteria } => serialize_to_value(&WireNoulQuestion {
                r#type: "noul",
                instructions,
                criteria,
            }),
            other => serialize_to_value(other),
        };
        questions.insert(id.clone(), wired);
    }
    SystemOneWireRequest {
        state: context.state.clone(),
        questions,
    }
}

fn request_headers(
    model: &ClassifierModel,
    api_key: &str,
    options_headers: Option<&ProviderHeaders>,
) -> IndexMap<String, String> {
    let mut defaults = ProviderHeaders::new();
    defaults.insert("authorization".to_owned(), Some(format!("Bearer {api_key}")));
    defaults.insert("content-type".to_owned(), Some("application/json".to_owned()));
    let model_hdrs = model_headers_to_provider_headers(model.headers.as_ref());
    match provider_headers_to_record([Some(&defaults), model_hdrs.as_ref(), options_headers]) {
        Some(map) => map,
        None => IndexMap::new(),
    }
}

/// Runs one System One classification over the given transport.
pub async fn classify_system_one(
    transport: &dyn SystemOneTransport,
    model: &ClassifierModel,
    context: &ClassifierContext,
    options: Option<ClassifierOptions>,
) -> ClassifierResult {
    let mut output = ClassifierResult {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        answers: IndexMap::new(),
        usage: None,
        stop_reason: ClassifierStopReason::Stop,
        error_message: None,
        timestamp: pi_js::time::now_ms(),
    };

    let caller_signal = options.as_ref().and_then(|o| o.base.signal.clone());

    let run: Result<()> = async {
        if model.api != transport.api() {
            return Err(Error::msg(format!("Unsupported classifier API: {}", model.api)));
        }
        let api_key = options
            .as_ref()
            .and_then(|o| o.base.api_key.as_deref())
            .filter(|k| !k.is_empty())
            .ok_or_else(|| Error::msg(format!("No API key for provider: {}", model.provider)))?;

        let mut payload = transport.payload(model, wire_request(context));
        if let Some(on_payload) = options.as_ref().and_then(|o| o.base.on_payload.as_ref()) {
            if let Some(transformed) = on_payload(payload.clone(), model).await? {
                payload = transformed;
            }
        }

        let request_url = transport.url(model)?;
        let hdr_map = request_headers(model, api_key, options.as_ref().and_then(|o| o.base.headers.as_ref()));
        let body_text = pi_js::json::stringify(&payload);
        let timeout_ms = options.as_ref().and_then(|o| o.base.timeout_ms);
        let max_retries = options.as_ref().and_then(|o| o.base.max_retries).or(Some(2));
        let max_retry_delay_ms = options.as_ref().and_then(|o| o.base.max_retry_delay_ms);
        let custom_fetch = options.as_ref().and_then(|o| o.base.fetch.clone());
        let label = transport.label();

        let last_http_body: Arc<Mutex<Option<(u16, String)>>> = Arc::new(Mutex::new(None));

        let attempt_result = retry_provider_request(
            || {
                let request_url = request_url.clone();
                let hdr_map = hdr_map.clone();
                let body_text = body_text.clone();
                let caller_signal = caller_signal.clone();
                let custom_fetch = custom_fetch.clone();
                let last_http_body = Arc::clone(&last_http_body);
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
                            let text = match next.text().await {
                                Ok(t) => t,
                                Err(_) => String::new(),
                            };
                            {
                                let mut guard = match last_http_body.lock() {
                                    Ok(g) => g,
                                    Err(p) => p.into_inner(),
                                };
                                *guard = Some((status, text));
                            }
                            return Err(ProviderHttpError::new(
                                Some(status),
                                Some(resp_headers),
                                format!("{label} returned {status}"),
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
        )
        .await;

        let (response, body) = match attempt_result {
            Ok(pair) => pair,
            Err(err) => {
                if let Some(http_err) = err.downcast_ref::<ProviderHttpError>() {
                    let captured = {
                        let guard = match last_http_body.lock() {
                            Ok(g) => g,
                            Err(p) => p.into_inner(),
                        };
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

        if let Some(on_response) = options.as_ref().and_then(|o| o.base.on_response.as_ref()) {
            on_response(
                ProviderResponse {
                    status: response.status(),
                    headers: headers_to_record(response.headers()),
                },
                model,
            )
            .await?;
        }

        let result = transport.output(&body)?;
        // Set before parsing answers: a request with malformed answers was still billed.
        if let Some(usage) = parse_usage(result.get("usage"), model) {
            output.usage = Some(usage);
        }
        output.answers = parse_answers(label, result.get("answers"), context)?;
        Ok(())
    }
    .await;

    match run {
        Ok(()) => output,
        Err(error) => {
            output.stop_reason = if caller_signal.as_ref().is_some_and(AbortSignal::aborted) {
                ClassifierStopReason::Aborted
            } else {
                ClassifierStopReason::Error
            };
            let prefix = format!("{} error", transport.label());
            output.error_message = Some(format_provider_error(&normalize_provider_error(&error), Some(&prefix)));
            output
        }
    }
}
