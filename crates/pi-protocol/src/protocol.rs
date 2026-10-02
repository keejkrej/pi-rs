//! Port of packages/protocol/src/protocol.ts

#![allow(dead_code, unused_variables)]

use std::sync::LazyLock;

use pi_chord::JsonValue;
use pi_js::vendor::typebox::{Schema, t};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: i64 = 8;

pub type ServerId = String;

pub type ProtocolErrorCode = String;

/// Key order is `code`, `message`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolError {
    pub code: ProtocolErrorCode,
    pub message: String,
}

/// Must be the first frame sent by a client.
///
/// Key order is `type`, `version`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientHello {
    pub version: i64,
}

/// A server-wide call, fenced to one logical server.
///
/// Key order is `serverId`.
///
/// PORT: not exported from `protocol.ts`. Public so [`RpcTarget::Server`] can be named.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerTarget {
    pub server_id: ServerId,
}

/// A session call, fenced to one logical server, durable session, and live attachment.
///
/// Key order is `serverId`, `sessionId`, `attachmentId`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTarget {
    pub server_id: ServerId,
    pub session_id: String,
    pub attachment_id: String,
}

// PORT: TS checks the server arm first and rejects extra keys. Serde has no
// `deny_unknown_fields`, so the session arm (more required keys) is first.
// `{serverId}` fails that arm and matches [`RpcTarget::Server`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcTarget {
    Session(SessionTarget),
    Server(ServerTarget),
}

/// Key order is `type`, `id`, `target`, `call`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestEnvelope {
    pub id: String,
    pub target: RpcTarget,
    pub call: JsonValue,
}

/// Key order is `type`, `id`, `target`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelEnvelope {
    pub id: String,
    pub target: RpcTarget,
}

/// Checked in schema order: hello, request, cancel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum ClientMessage {
    #[serde(rename = "hello")]
    Hello(ClientHello),
    #[serde(rename = "request")]
    Request(RequestEnvelope),
    #[serde(rename = "cancel")]
    Cancel(CancelEnvelope),
}

/// Key order is `type`, `version`, `serverId`. `version` is [`PROTOCOL_VERSION`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerHello {
    pub version: i64,
    pub server_id: ServerId,
}

/// Key order is `type`, `error`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerHelloError {
    pub error: ProtocolError,
}

/// Successful (`ok: true`, optional `result`) or failed (`ok: false`, `error`) response.
///
/// Key order is `type`, `id`, `ok`, then `result` or `error`.
///
/// PORT: both arms share `type: "response"`, so they are one struct flattened under
/// [`ServerMessage`]'s tag. `None` omits the key. TypeBox still rejects an arm that carries
/// the wrong combination of `ok`, `result`, and `error`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseEnvelope {
    pub id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

/// Key order is `type`, `subscriptionId`, `update`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceEventEnvelope {
    pub subscription_id: String,
    pub update: JsonValue,
}

/// Out-of-band update to this presentation's selected Session route.
///
/// Key order is `type`, `attachment`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentEnvelope {
    // PORT: `SessionTarget | null`. `None` is JSON null and the key is required, so it is not skipped.
    pub attachment: Option<SessionTarget>,
}

/// Checked in schema order: hello, hello_error, response, service_update, attachment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum ServerMessage {
    #[serde(rename = "hello")]
    Hello(ServerHello),
    #[serde(rename = "hello_error")]
    HelloError(ServerHelloError),
    #[serde(rename = "response")]
    Response(ResponseEnvelope),
    #[serde(rename = "service_update")]
    ServiceUpdate(ServiceEventEnvelope),
    #[serde(rename = "attachment")]
    Attachment(AttachmentEnvelope),
}

pub fn is_server_id(value: &Value) -> bool {
    todo!("port: is_server_id")
}

fn strict_object<I, K>(properties: I) -> Schema
where
    I: IntoIterator<Item = (K, Schema)>,
    K: Into<String>,
{
    t::object(properties).with("additionalProperties", false)
}

fn id_schema() -> Schema {
    t::string().with("minLength", 1)
}

fn opaque_json_value_schema() -> Schema {
    t::unsafe_(t::unknown())
}

fn server_id_schema() -> Schema {
    t::string().with(
        "pattern",
        "^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$",
    )
}

fn protocol_error_schema() -> Schema {
    strict_object([("code", id_schema()), ("message", t::string())])
}

fn client_hello_schema() -> Schema {
    strict_object([
        ("type", t::literal("hello")),
        ("version", t::integer().with("minimum", 0)),
    ])
}

fn server_target_schema() -> Schema {
    strict_object([("serverId", server_id_schema())])
}

fn session_target_schema() -> Schema {
    strict_object([
        ("serverId", server_id_schema()),
        ("sessionId", id_schema()),
        ("attachmentId", id_schema()),
    ])
}

fn rpc_target_schema() -> Schema {
    t::union([server_target_schema(), session_target_schema()])
}

fn request_envelope_schema() -> Schema {
    strict_object([
        ("type", t::literal("request")),
        ("id", id_schema()),
        ("target", rpc_target_schema()),
        ("call", opaque_json_value_schema()),
    ])
}

fn cancel_envelope_schema() -> Schema {
    strict_object([
        ("type", t::literal("cancel")),
        ("id", id_schema()),
        ("target", rpc_target_schema()),
    ])
}

fn server_hello_schema() -> Schema {
    strict_object([
        ("type", t::literal("hello")),
        ("version", t::literal(PROTOCOL_VERSION)),
        ("serverId", server_id_schema()),
    ])
}

fn server_hello_error_schema() -> Schema {
    strict_object([("type", t::literal("hello_error")), ("error", protocol_error_schema())])
}

fn response_envelope_schema() -> Schema {
    t::union([
        strict_object([
            ("type", t::literal("response")),
            ("id", id_schema()),
            ("ok", t::literal(true)),
            ("result", opaque_json_value_schema().optional()),
        ]),
        strict_object([
            ("type", t::literal("response")),
            ("id", id_schema()),
            ("ok", t::literal(false)),
            ("error", protocol_error_schema()),
        ]),
    ])
}

fn service_event_envelope_schema() -> Schema {
    strict_object([
        ("type", t::literal("service_update")),
        ("subscriptionId", id_schema()),
        ("update", opaque_json_value_schema()),
    ])
}

fn attachment_envelope_schema() -> Schema {
    strict_object([
        ("type", t::literal("attachment")),
        ("attachment", t::union([session_target_schema(), t::null()])),
    ])
}

/// `ClientMessageSchema`.
pub static client_message_schema: LazyLock<Schema> = LazyLock::new(|| {
    t::union([
        client_hello_schema(),
        request_envelope_schema(),
        cancel_envelope_schema(),
    ])
});

/// `ServerMessageSchema`.
pub static server_message_schema: LazyLock<Schema> = LazyLock::new(|| {
    t::union([
        server_hello_schema(),
        server_hello_error_schema(),
        response_envelope_schema(),
        service_event_envelope_schema(),
        attachment_envelope_schema(),
    ])
});
