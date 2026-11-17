//! Port of packages/mcp/src/protocol/jsonrpc.ts

#![allow(dead_code, unused_variables)]

use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use pi_js::{Error, Result};

// PORT: `Eq` + `Hash` treat `0` and `-0` as the same key, matching JS `Map`.
/// `string | number` JSON-RPC id. Numeric ids are finite JSON numbers (JS `number`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcId {
    String(String),
    Number(f64),
}

fn canonical_number_bits(n: f64) -> u64 {
    if n == 0.0 { 0.0_f64.to_bits() } else { n.to_bits() }
}

impl PartialEq for JsonRpcId {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Number(a), Self::Number(b)) => canonical_number_bits(*a) == canonical_number_bits(*b),
            _ => false,
        }
    }
}

impl Eq for JsonRpcId {}

impl Hash for JsonRpcId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::String(s) => {
                0u8.hash(state);
                s.hash(state);
            }
            Self::Number(n) => {
                1u8.hash(state);
                canonical_number_bits(*n).hash(state);
            }
        }
    }
}

/// Wire `"jsonrpc": "2.0"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JsonRpcVersion {
    #[serde(rename = "2.0")]
    V2,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcRequest {
    pub jsonrpc: JsonRpcVersion,
    pub id: JsonRpcId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcNotification {
    pub jsonrpc: JsonRpcVersion,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcSuccessResponse {
    pub jsonrpc: JsonRpcVersion,
    pub id: JsonRpcId,
    pub result: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcErrorResponse {
    pub jsonrpc: JsonRpcVersion,
    pub id: JsonRpcId,
    pub error: JsonRpcErrorObject,
}

/// Checked in TS order: a `result` member, then an `error` member.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcResponse {
    Success(JsonRpcSuccessResponse),
    Error(JsonRpcErrorResponse),
}

/// Checked in TS order: request, notification, response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcMessage {
    Request(JsonRpcRequest),
    Notification(JsonRpcNotification),
    Response(JsonRpcResponse),
}

#[derive(Clone, Copy, Debug)]
pub struct JsonRpcErrorCodes {
    pub parse_error: i64,
    pub invalid_request: i64,
    pub method_not_found: i64,
    pub invalid_params: i64,
    pub internal_error: i64,
}

pub const JSON_RPC_ERROR_CODES: JsonRpcErrorCodes = JsonRpcErrorCodes {
    parse_error: -32700,
    invalid_request: -32600,
    method_not_found: -32601,
    invalid_params: -32602,
    internal_error: -32603,
};

/// JS name `McpError`.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct McpError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl McpError {
    pub fn new(code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            data,
        }
    }
}

/// JS name `McpConnectionClosedError`.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct McpConnectionClosedError {
    pub message: String,
}

impl McpConnectionClosedError {
    pub fn new(message: Option<&str>) -> Self {
        Self {
            message: message.unwrap_or("MCP connection closed").to_string(),
        }
    }
}

/// JS name `McpTimeoutError`.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct McpTimeoutError {
    pub message: String,
    pub timeout_ms: i64,
}

impl McpTimeoutError {
    pub fn new(timeout_ms: i64) -> Self {
        Self {
            message: format!("MCP request timed out after {timeout_ms}ms"),
            timeout_ms,
        }
    }
}

/// JS name is `AbortError`, not `McpAbortError`. `instanceof McpAbortError` is `downcast_ref::<McpAbortError>`.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct McpAbortError {
    pub message: String,
}

impl McpAbortError {
    /// JS name passed to `Error::typed`.
    pub const NAME: &'static str = "AbortError";

    pub fn new(message: Option<&str>) -> Self {
        Self {
            message: message.unwrap_or("MCP request aborted").to_string(),
        }
    }
}

pub fn is_object(value: &Value) -> bool {
    value.is_object()
}

pub fn to_error(value: &(dyn std::error::Error + Send + Sync + 'static)) -> Error {
    if let Some(err) = value.downcast_ref::<Error>() {
        return match err {
            Error::Js(j) => Error::Js(pi_js::JsError {
                name: j.name.clone(),
                message: j.message.clone(),
                code: j.code.clone(),
                cause: j.cause.as_deref().map(|c| Box::new(to_error(c))),
            }),
            Error::Abort(reason) => Error::Abort(reason.clone()),
            Error::Node(node) => Error::Node(node.clone()),
            Error::Exit(code) => Error::Exit(*code),
            Error::Typed(name, inner) => {
                if let Some(e) = inner.downcast_ref::<McpError>() {
                    Error::typed(name, e.clone())
                } else if let Some(e) = inner.downcast_ref::<McpConnectionClosedError>() {
                    Error::typed(name, e.clone())
                } else if let Some(e) = inner.downcast_ref::<McpTimeoutError>() {
                    Error::typed(name, e.clone())
                } else if let Some(e) = inner.downcast_ref::<McpAbortError>() {
                    Error::typed(name, e.clone())
                } else {
                    Error::js(name, inner.to_string())
                }
            }
        };
    }
    if let Some(e) = value.downcast_ref::<McpError>() {
        return Error::typed("McpError", e.clone());
    }
    if let Some(e) = value.downcast_ref::<McpConnectionClosedError>() {
        return Error::typed("McpConnectionClosedError", e.clone());
    }
    if let Some(e) = value.downcast_ref::<McpTimeoutError>() {
        return Error::typed("McpTimeoutError", e.clone());
    }
    if let Some(e) = value.downcast_ref::<McpAbortError>() {
        return Error::typed(McpAbortError::NAME, e.clone());
    }
    if let Some(j) = value.downcast_ref::<pi_js::JsError>() {
        return Error::Js(pi_js::JsError {
            name: j.name.clone(),
            message: j.message.clone(),
            code: j.code.clone(),
            cause: j.cause.as_deref().map(|c| Box::new(to_error(c))),
        });
    }
    if let Some(node) = value.downcast_ref::<pi_js::NodeError>() {
        return Error::Node(node.clone());
    }
    Error::msg(value.to_string())
}

pub fn is_json_rpc_id(value: &Value) -> bool {
    value.is_string() || value.as_f64().is_some_and(f64::is_finite)
}

pub fn is_json_rpc_request(message: &Value) -> bool {
    let Some(obj) = message.as_object() else {
        return false;
    };
    obj.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && obj.get("id").is_some_and(is_json_rpc_id)
        && obj.get("method").is_some_and(Value::is_string)
}

pub fn is_json_rpc_notification(message: &Value) -> bool {
    let Some(obj) = message.as_object() else {
        return false;
    };
    obj.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && !obj.contains_key("id")
        && obj.get("method").is_some_and(Value::is_string)
}

pub fn is_json_rpc_response(message: &Value) -> bool {
    let Some(obj) = message.as_object() else {
        return false;
    };
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return false;
    }
    if !obj.get("id").is_some_and(is_json_rpc_id) {
        return false;
    }
    if obj.contains_key("result") {
        return !obj.contains_key("error");
    }
    let Some(err) = obj.get("error").and_then(Value::as_object) else {
        return false;
    };
    err.get("code").and_then(Value::as_f64).is_some_and(f64::is_finite)
        && err.get("message").is_some_and(Value::is_string)
}

pub fn parse_json_rpc_message(value: &Value) -> Result<JsonRpcMessage> {
    if is_json_rpc_request(value) || is_json_rpc_notification(value) || is_json_rpc_response(value) {
        if let Ok(message) = serde_json::from_value::<JsonRpcMessage>(value.clone()) {
            return Ok(message);
        }
    }
    Err(Error::typed(
        "McpError",
        McpError::new(JSON_RPC_ERROR_CODES.invalid_request, "Invalid JSON-RPC message", None),
    ))
}
