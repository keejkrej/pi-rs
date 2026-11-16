//! Port of packages/ai/src/api/openrouter-images.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::{Value, json};

use pi_js::abort::AbortSignal;
use pi_js::fetch::{Body, Headers, Request};
use pi_js::{Error, Result};

use crate::api::system_one_shared::model_headers_to_provider_headers;
use crate::types::{
    AssistantImages, ImageContent, ImageModel, ImagesContext, ImagesOptions, ImagesStopReason, ModelModality,
    ProviderHeaders, ProviderResponse, TextContent, Usage, UsageCost, UserContentBlock,
};
use crate::utils::abort::race_with_abort_signal;
use crate::utils::abort_signals::combine_abort_signals;
use crate::utils::error_body::{SdkErrorInfo, format_provider_error, normalize_provider_error};
use crate::utils::headers::{headers_to_record, provider_headers_to_record};
use crate::utils::provider_retry::{ProviderHttpError, ProviderRetryOptions, retry_provider_request};
use crate::utils::sanitize_unicode::sanitize_surrogates;

const OPENAI_JS_VERSION: &str = "7.19.0";
const DEFAULT_OPENAI_TIMEOUT_MS: u64 = 600_000;

fn stainless_os() -> &'static str {
    match pi_js::env::platform() {
        "darwin" | "macos" => "MacOS",
        "win32" | "windows" => "Windows",
        "linux" => "Linux",
        "freebsd" => "FreeBSD",
        "openbsd" => "OpenBSD",
        "android" => "Android",
        _ => "Unknown",
    }
}

fn stainless_arch() -> String {
    match pi_js::env::arch() {
        "x64" | "x86_64" => "x64".to_owned(),
        "arm64" | "aarch64" => "arm64".to_owned(),
        "arm" => "arm".to_owned(),
        "ia32" | "x86" => "x32".to_owned(),
        other => format!("other:{other}"),
    }
}

fn build_params(model: &ImageModel, context: &ImagesContext) -> Value {
    let content: Vec<Value> = context
        .input
        .iter()
        .map(|item| match item {
            UserContentBlock::Text(text) => json!({
                "type": "text",
                "text": sanitize_surrogates(&text.text),
            }),
            UserContentBlock::Image(image) => json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{};base64,{}", image.mime_type, image.data),
                },
            }),
        })
        .collect();

    let modalities: Vec<&str> = if model.output.contains(&ModelModality::Text) {
        vec!["image", "text"]
    } else {
        vec!["image"]
    };

    json!({
        "model": model.id,
        "messages": [
            {
                "role": "user",
                "content": content,
            }
        ],
        "stream": false,
        "modalities": modalities,
    })
}

fn usage_count(value: Option<&Value>) -> i64 {
    let Some(val) = value else {
        return 0;
    };
    if let Some(n) = val.as_i64() {
        return n;
    }
    match val.as_f64() {
        Some(n) if n.is_finite() => n as i64,
        _ => 0,
    }
}

fn parse_usage(raw_usage: &Value, model: &ImageModel) -> Usage {
    let prompt_tokens = usage_count(raw_usage.get("prompt_tokens"));
    let details = raw_usage.get("prompt_tokens_details");
    let reported_cached_tokens = usage_count(details.and_then(|d| d.get("cached_tokens")));
    let cache_write_tokens = usage_count(details.and_then(|d| d.get("cache_write_tokens")));
    let cache_read_tokens = if cache_write_tokens > 0 {
        0_i64.max(reported_cached_tokens - cache_write_tokens)
    } else {
        reported_cached_tokens
    };
    let input = 0_i64.max(prompt_tokens - cache_read_tokens - cache_write_tokens);
    let output = usage_count(raw_usage.get("completion_tokens"));

    let input_cost = (model.cost.input / 1_000_000.0) * (input as f64);
    let output_cost = (model.cost.output / 1_000_000.0) * (output as f64);
    let cache_read_cost = (model.cost.cache_read / 1_000_000.0) * (cache_read_tokens as f64);
    let cache_write_cost = (model.cost.cache_write / 1_000_000.0) * (cache_write_tokens as f64);

    Usage {
        input,
        output,
        cache_read: cache_read_tokens,
        cache_write: cache_write_tokens,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: input + output + cache_read_tokens + cache_write_tokens,
        cost: UsageCost {
            input: input_cost,
            output: output_cost,
            cache_read: cache_read_cost,
            cache_write: cache_write_cost,
            total: input_cost + output_cost + cache_read_cost + cache_write_cost,
        },
    }
}

fn build_request_headers(
    model: &ImageModel,
    api_key: &str,
    options_headers: Option<&ProviderHeaders>,
    timeout_ms: Option<u64>,
) -> IndexMap<String, String> {
    let mut sdk_defaults = ProviderHeaders::new();
    sdk_defaults.insert("accept".to_owned(), Some("application/json".to_owned()));
    sdk_defaults.insert("content-type".to_owned(), Some("application/json".to_owned()));
    sdk_defaults.insert("user-agent".to_owned(), Some(format!("OpenAI/JS {OPENAI_JS_VERSION}")));
    sdk_defaults.insert("x-stainless-retry-count".to_owned(), Some("0".to_owned()));
    let timeout_secs = match timeout_ms {
        Some(ms) => ms / 1000,
        None => DEFAULT_OPENAI_TIMEOUT_MS / 1000,
    };
    sdk_defaults.insert("x-stainless-timeout".to_owned(), Some(timeout_secs.to_string()));
    sdk_defaults.insert("x-stainless-lang".to_owned(), Some("js".to_owned()));
    sdk_defaults.insert(
        "x-stainless-package-version".to_owned(),
        Some(OPENAI_JS_VERSION.to_owned()),
    );
    sdk_defaults.insert("x-stainless-os".to_owned(), Some(stainless_os().to_owned()));
    sdk_defaults.insert("x-stainless-arch".to_owned(), Some(stainless_arch()));
    sdk_defaults.insert("x-stainless-runtime".to_owned(), Some("node".to_owned()));
    sdk_defaults.insert("x-stainless-runtime-version".to_owned(), Some("v24.21.0".to_owned()));
    sdk_defaults.insert("authorization".to_owned(), Some(format!("Bearer {api_key}")));

    let model_hdrs = model_headers_to_provider_headers(model.headers.as_ref());
    match provider_headers_to_record([Some(&sdk_defaults), model_hdrs.as_ref(), options_headers]) {
        Some(map) => map,
        None => IndexMap::new(),
    }
}

fn chat_completions_url(base_url: &str) -> String {
    let trimmed = match base_url.strip_suffix('/') {
        Some(s) => s,
        None => base_url,
    };
    format!("{trimmed}/chat/completions")
}

fn make_openai_api_message(status: u16, parsed: Option<&Value>, raw_body: &str) -> String {
    let extracted_msg = parsed
        .and_then(|v| {
            let err_obj = v.as_object().and_then(|o| o.get("error")).map_or(v, |err| err);
            match err_obj.as_object() {
                Some(o) => o
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| Some(pi_js::json::stringify(err_obj))),
                None => err_obj.as_str().map(str::to_owned),
            }
        })
        .filter(|s| !s.is_empty());

    match extracted_msg {
        Some(msg) => format!("{status} {msg}"),
        None if !raw_body.trim().is_empty() => format!("{status} {}", raw_body.trim()),
        None => format!("{status} status code (no body)"),
    }
}

fn extract_data_url_image(image_url: &str) -> Option<(String, String)> {
    let rest = image_url.strip_prefix("data:")?;
    let (mime, data) = rest.split_once(";base64,")?;
    if mime.is_empty()
        || mime.contains(';')
        || data.is_empty()
        || mime.contains(['\n', '\r'])
        || data.contains(['\n', '\r'])
    {
        return None;
    }
    Some((mime.to_owned(), data.to_owned()))
}

/// Image generation over OpenRouter's chat completions endpoint.
pub async fn generate_images(
    model: &ImageModel,
    context: &ImagesContext,
    options: Option<ImagesOptions>,
) -> AssistantImages {
    let mut output = AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: Vec::new(),
        response_id: None,
        usage: None,
        stop_reason: ImagesStopReason::Stop,
        error_message: None,
        timestamp: pi_js::time::now_ms(),
    };

    let caller_signal = options.as_ref().and_then(|o| o.base.signal.clone());

    let run: Result<()> = async {
        let api_key = options
            .as_ref()
            .and_then(|o| o.base.api_key.as_deref())
            .filter(|k| !k.is_empty())
            .ok_or_else(|| Error::msg(format!("No API key for provider: {}", model.provider)))?;

        let mut params = build_params(model, context);
        if let Some(on_payload) = options.as_ref().and_then(|o| o.base.on_payload.as_ref()) {
            if let Some(next_params) = on_payload(params.clone(), model).await? {
                params = next_params;
            }
        }

        let request_url = chat_completions_url(&model.base_url);
        let timeout_ms = options.as_ref().and_then(|o| o.base.timeout_ms);
        let hdr_map = build_request_headers(
            model,
            api_key,
            options.as_ref().and_then(|o| o.base.headers.as_ref()),
            timeout_ms,
        );
        let body_text = pi_js::json::stringify(&params);
        let max_retries = options.as_ref().and_then(|o| o.base.max_retries);
        let max_retry_delay_ms = options.as_ref().and_then(|o| o.base.max_retry_delay_ms);
        let custom_fetch = options.as_ref().and_then(|o| o.base.fetch.clone());

        let last_http_error: Arc<Mutex<Option<(u16, String, Option<Value>)>>> = Arc::new(Mutex::new(None));

        let attempt_result = retry_provider_request(
            || {
                let request_url = request_url.clone();
                let hdr_map = hdr_map.clone();
                let body_text = body_text.clone();
                let caller_signal = caller_signal.clone();
                let custom_fetch = custom_fetch.clone();
                let last_http_error = Arc::clone(&last_http_error);
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
                            let raw_body = match next.text().await {
                                Ok(t) => t,
                                Err(_) => String::new(),
                            };
                            let parsed_body = pi_js::json::parse(&raw_body).ok();
                            let message = make_openai_api_message(status, parsed_body.as_ref(), &raw_body);
                            {
                                let mut guard = match last_http_error.lock() {
                                    Ok(g) => g,
                                    Err(p) => p.into_inner(),
                                };
                                *guard = Some((status, raw_body, parsed_body));
                            }
                            return Err(
                                ProviderHttpError::new(Some(status), Some(resp_headers), message).into_js_error()
                            );
                        }
                        let text = next.text().await?;
                        let parsed = pi_js::json::parse(&text)?;
                        Ok((next, parsed))
                    };

                    let already_aborted = combined.signal.as_ref().is_some_and(AbortSignal::aborted);
                    let res = if already_aborted {
                        match perform.await {
                            Ok(val) => {
                                if let Some(sig) = &combined.signal {
                                    if let Err(err) = sig.throw_if_aborted() {
                                        Err(err)
                                    } else {
                                        Ok(val)
                                    }
                                } else {
                                    Ok(val)
                                }
                            }
                            Err(err) => Err(err),
                        }
                    } else {
                        match &combined.signal {
                            Some(sig) => race_with_abort_signal(perform, sig).await,
                            None => perform.await,
                        }
                    };
                    combined.cleanup();

                    match res {
                        Ok(pair) => Ok(pair),
                        Err(err) => {
                            if timeout_signal.as_ref().is_some_and(AbortSignal::aborted)
                                && !caller_signal.as_ref().is_some_and(AbortSignal::aborted)
                            {
                                return Err(ProviderHttpError::new(None, None, "Request timed out.").into_js_error());
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

        let (raw_response, image_response) = match attempt_result {
            Ok(pair) => pair,
            Err(err) => {
                if let Some(http_err) = err.downcast_ref::<ProviderHttpError>() {
                    let captured = {
                        let guard = match last_http_error.lock() {
                            Ok(g) => g,
                            Err(p) => p.into_inner(),
                        };
                        guard.as_ref().and_then(|(st, raw, parsed)| {
                            if Some(*st) == http_err.status {
                                Some((raw.clone(), parsed.clone()))
                            } else {
                                None
                            }
                        })
                    };
                    let (body, response_body) = match captured {
                        Some((raw, parsed)) => (Some(raw), parsed),
                        None => (None, None),
                    };
                    let sdk = SdkErrorInfo {
                        status: http_err.status,
                        body,
                        response_body,
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
                    status: raw_response.status(),
                    headers: headers_to_record(raw_response.headers()),
                },
                model,
            )
            .await?;
        }

        output.response_id = image_response
            .as_object()
            .and_then(|o| o.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned);

        if let Some(raw_usage) = image_response
            .as_object()
            .and_then(|o| o.get("usage"))
            .filter(|v| !v.is_null())
        {
            output.usage = Some(parse_usage(raw_usage, model));
        }

        if let Some(message) = image_response
            .as_object()
            .and_then(|o| o.get("choices"))
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.as_object())
            .and_then(|choice| choice.get("message"))
            .and_then(Value::as_object)
        {
            if let Some(content) = message.get("content").and_then(Value::as_str) {
                if !content.is_empty() {
                    output.output.push(UserContentBlock::Text(TextContent::new(content)));
                }
            }
            if let Some(images) = message.get("images").and_then(Value::as_array) {
                for image in images {
                    let image_url = image
                        .as_object()
                        .and_then(|o| o.get("image_url"))
                        .and_then(|v| match v {
                            Value::String(s) => Some(s.as_str()),
                            Value::Object(obj) => obj.get("url").and_then(Value::as_str),
                            _ => None,
                        });
                    if let Some(url_str) = image_url {
                        if let Some((mime_type, data)) = extract_data_url_image(url_str) {
                            output
                                .output
                                .push(UserContentBlock::Image(ImageContent::new(data, mime_type)));
                        }
                    }
                }
            }
        }

        Ok(())
    }
    .await;

    match run {
        Ok(()) => output,
        Err(error) => {
            output.stop_reason = if caller_signal.as_ref().is_some_and(AbortSignal::aborted) {
                ImagesStopReason::Aborted
            } else {
                ImagesStopReason::Error
            };
            output.error_message = Some(format_provider_error(&normalize_provider_error(&error), None));
            output
        }
    }
}
