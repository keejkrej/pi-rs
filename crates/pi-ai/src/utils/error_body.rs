//! Port of packages/ai/src/utils/error-body.ts

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::types::JsonValue;
use crate::utils::diagnostics::format_thrown_value;
use crate::utils::provider_retry::ProviderHttpError;

pub const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 4000;

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

pub fn normalize_provider_error(error: &pi_js::Error) -> NormalizedProviderError {
    if let Some(sdk) = error.downcast_ref::<SdkErrorInfo>() {
        return normalize_sdk_error(sdk);
    }
    if let Some(http) = error.downcast_ref::<ProviderHttpError>() {
        return NormalizedProviderError {
            status: http.status,
            body: None,
            message: http.message.clone(),
            message_carries_body: true,
        };
    }

    NormalizedProviderError {
        status: None,
        body: None,
        message: format_thrown_value(error),
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
    let head = pi_js::str16::slice(text, 0, Some(max_chars as i64));
    format!("{head}... [truncated {} chars]", total_chars - max_chars)
}

pub fn safe_json_stringify(value: &JsonValue) -> String {
    pi_js::json::stringify(value)
}
