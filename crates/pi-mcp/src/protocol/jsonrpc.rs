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
        todo!("port: McpTimeoutError::new")
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
    todo!("port: is_object")
}

pub fn to_error(value: &(dyn std::error::Error + Send + Sync + 'static)) -> Error {
    todo!("port: to_error")
}

pub fn is_json_rpc_id(value: &Value) -> bool {
    todo!("port: is_json_rpc_id")
}

pub fn is_json_rpc_request(message: &Value) -> bool {
    todo!("port: is_json_rpc_request")
}

pub fn is_json_rpc_notification(message: &Value) -> bool {
    todo!("port: is_json_rpc_notification")
}

pub fn is_json_rpc_response(message: &Value) -> bool {
    todo!("port: is_json_rpc_response")
}

pub fn parse_json_rpc_message(value: &Value) -> Result<JsonRpcMessage> {
    todo!("port: parse_json_rpc_message")
}
