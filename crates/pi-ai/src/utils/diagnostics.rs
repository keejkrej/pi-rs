//! Port of packages/ai/src/utils/diagnostics.ts

use pi_js::Error;
use serde::{Deserialize, Serialize};

use crate::types::{AssistantMessage, JsonObject, NumberOrString};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticErrorInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<NumberOrString>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessageDiagnostic {
    #[serde(rename = "type")]
    pub r#type: String,
    pub timestamp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<DiagnosticErrorInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonObject>,
}

pub fn format_thrown_value(error: &Error) -> String {
    let msg = error.to_string();
    if msg.is_empty() { error.name().to_string() } else { msg }
}

pub fn extract_diagnostic_error(error: &Error) -> DiagnosticErrorInfo {
    let name = error.name();
    let raw_msg = error.to_string();
    let message = if raw_msg.is_empty() { name.to_string() } else { raw_msg };
    let code = error.code().map(|c| NumberOrString::String(c.to_string()));
    DiagnosticErrorInfo {
        name: if name.is_empty() { None } else { Some(name.to_string()) },
        message,
        stack: None,
        code,
    }
}

pub fn create_assistant_message_diagnostic(
    r#type: impl Into<String>,
    error: &Error,
    details: Option<JsonObject>,
) -> AssistantMessageDiagnostic {
    AssistantMessageDiagnostic {
        r#type: r#type.into(),
        timestamp: pi_js::time::now_ms(),
        error: Some(extract_diagnostic_error(error)),
        details,
    }
}

pub fn create_assistant_message_diagnostic_message(
    r#type: impl Into<String>,
    message: impl Into<String>,
    details: Option<JsonObject>,
) -> AssistantMessageDiagnostic {
    let msg = message.into();
    AssistantMessageDiagnostic {
        r#type: r#type.into(),
        timestamp: pi_js::time::now_ms(),
        error: Some(DiagnosticErrorInfo {
            name: Some("Error".to_string()),
            message: msg,
            stack: None,
            code: None,
        }),
        details,
    }
}

pub fn append_assistant_message_diagnostic(message: &mut AssistantMessage, diagnostic: AssistantMessageDiagnostic) {
    message.diagnostics.get_or_insert_with(Vec::new).push(diagnostic);
}
