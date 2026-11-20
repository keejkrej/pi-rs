//! Port of packages/ai/src/utils/error-body.ts

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::types::JsonValue;
use crate::utils::diagnostics::format_thrown_value;
use crate::utils::provider_retry::ProviderHttpError;
use crate::utils::text::owned_slice;

pub const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 4000;
const MAX_CAUSE_DEPTH: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedProviderError {
    /// HTTP status code, when one could be extracted from the SDK error object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Raw HTTP body reason, already trimmed and truncated to the cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// `error.message`, or serialized value for non-error throws.
    pub message: String,
    /// True when `message` already contains the body (no separate body to add).
    pub message_carries_body: bool,
}

/// Typed SDK-style HTTP error carrier allowing [`normalize_provider_error`] to inspect
/// provider status codes and raw/parsed error bodies consistently with TS SDK error shapes.
#[derive(Clone, Debug, Default)]
pub struct SdkErrorInfo {
    pub status_code: Option<u16>,
    pub status: Option<u16>,
    pub metadata_http_status_code: Option<u16>,
    pub response_status_code: Option<u16>,
    pub body: Option<String>,
    pub error: Option<JsonValue>,
    pub response_body: Option<JsonValue>,
    pub message: String,
}

impl SdkErrorInfo {
    pub fn into_js_error(self) -> pi_js::Error {
        pi_js::Error::typed("SdkErrorInfo", self)
    }
}

impl fmt::Display for SdkErrorInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SdkErrorInfo {}

impl From<SdkErrorInfo> for pi_js::Error {
    fn from(value: SdkErrorInfo) -> Self {
        value.into_js_error()
    }
}

fn is_plain_non_empty_object(value: &JsonValue) -> bool {
    matches!(value, JsonValue::Object(map) if !map.is_empty())
}

fn extract_status(info: &SdkErrorInfo) -> Option<u16> {
    info.status_code
        .or(info.status)
        .or(info.metadata_http_status_code)
        .or(info.response_status_code)
}

fn pick_body_text(info: &SdkErrorInfo) -> Option<String> {
    if let Some(body) = &info.body {
        return Some(body.clone());
    }
    if let Some(err_val) = &info.error {
        if is_plain_non_empty_object(err_val) {
            return Some(safe_json_stringify(err_val));
        }
    }
    if let Some(resp_body) = &info.response_body {
        match resp_body {
            JsonValue::String(s) => return Some(s.clone()),
            val if is_plain_non_empty_object(val) => return Some(safe_json_stringify(val)),
            _ => {}
        }
    }
    None
}

fn extract_body(info: &SdkErrorInfo) -> Option<String> {
    let body_text = pick_body_text(info)?;
    let trimmed = pi_js::str16::trim(&body_text);
    if trimmed.is_empty() {
        return None;
    }
    Some(truncate_error_text(trimmed, MAX_PROVIDER_ERROR_BODY_CHARS))
}

pub fn normalize_sdk_error(info: &SdkErrorInfo) -> NormalizedProviderError {
    let status = extract_status(info);
    let body = extract_body(info);
    let message_carries_body = match &body {
        None => true,
        Some(b) => info.message.contains(b.as_str()),
    };

    NormalizedProviderError {
        status,
        body,
        message: info.message.clone(),
        message_carries_body,
    }
}

fn unwrap_error_source<'a>(
    err: &'a (dyn std::error::Error + 'static),
) -> Option<&'a (dyn std::error::Error + 'static)> {
    if let Some(js) = err.downcast_ref::<pi_js::Error>() {
        return match js {
            pi_js::Error::Js(j) => j.cause.as_deref().map(|e| e as &(dyn std::error::Error + 'static)),
            pi_js::Error::Typed(_, inner) => inner.source(),
            pi_js::Error::Node(n) => std::error::Error::source(n),
            pi_js::Error::Abort(_) | pi_js::Error::Exit(_) => None,
        };
    }
    err.source()
}

pub fn with_cause_detail(error: &pi_js::Error) -> String {
    let base_message = format_thrown_value(error);
    let mut deepest: Option<&(dyn std::error::Error + 'static)> = None;
    let mut current: Option<&(dyn std::error::Error + 'static)> = unwrap_error_source(error);
    for _ in 0..MAX_CAUSE_DEPTH {
        let Some(err) = current else {
            break;
        };
        deepest = Some(err);
        current = unwrap_error_source(err);
    }
    let Some(deepest_err) = deepest else {
        return base_message;
    };

    let mut detail = deepest_err.to_string().trim().to_owned();
    let code = deepest_err
        .downcast_ref::<pi_js::Error>()
        .and_then(|e| e.code().map(str::to_owned))
        .or_else(|| {
            deepest_err
                .downcast_ref::<pi_js::error::JsError>()
                .and_then(|e| e.code.clone())
        })
        .or_else(|| {
            deepest_err
                .downcast_ref::<pi_js::error::NodeError>()
                .map(|e| e.code.clone())
        });
    if detail.is_empty()
        && let Some(js) = deepest_err.downcast_ref::<pi_js::Error>()
    {
        detail = js.name().to_owned();
    }
    if let Some(c) = code
        && !detail.contains(&c)
    {
        detail = if detail.is_empty() {
            c
        } else {
            format!("{detail} ({c})")
        };
    }
    if detail.is_empty() || base_message.contains(&detail) {
        return base_message;
    }
    if base_message.is_empty() {
        detail
    } else {
        format!("{base_message}: {detail}")
    }
}

pub fn normalize_provider_error(error: &pi_js::Error) -> NormalizedProviderError {
    if let Some(sdk) = error.downcast_ref::<SdkErrorInfo>() {
        let mut norm = normalize_sdk_error(sdk);
        if norm.status.is_none() && norm.body.is_none() {
            norm.message = with_cause_detail(error);
        }
        return norm;
    }
    if let Some(http) = error.downcast_ref::<ProviderHttpError>() {
        let message = if http.status.is_none() {
            with_cause_detail(error)
        } else {
            http.message.clone()
        };
        return NormalizedProviderError {
            status: http.status,
            body: None,
            message,
            message_carries_body: true,
        };
    }

    NormalizedProviderError {
        status: None,
        body: None,
        message: with_cause_detail(error),
        message_carries_body: true,
    }
}

/// Compose a display string from a normalized error.
pub fn format_provider_error(norm: &NormalizedProviderError, prefix: Option<&str>) -> String {
    if norm.message_carries_body || norm.status.is_none() || norm.body.is_none() {
        if let (Some(pfx), Some(status)) = (prefix, norm.status) {
            return format!("{pfx} ({status}): {}", norm.message);
        }
        return norm.message.clone();
    }

    let status = norm.status.unwrap_or_default();
    let body = norm.body.as_deref().unwrap_or("");
    match prefix {
        Some(pfx) => format!("{pfx} ({status}): {body}"),
        None => format!("{status}: {body}"),
    }
}

pub fn truncate_error_text(text: &str, max_chars: usize) -> String {
    let total_chars = pi_js::str16::len(text);
    if total_chars <= max_chars {
        return text.to_string();
    }
    let head = owned_slice(text, 0, Some(max_chars));
    format!("{head}... [truncated {} chars]", total_chars - max_chars)
}

pub fn safe_json_stringify(value: &JsonValue) -> String {
    pi_js::json::stringify(value)
}
