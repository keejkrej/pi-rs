//! Port of packages/mcp/src/transports/streamable-http.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;

use crate::auth_provider::{AuthProvider, McpFetch};
use crate::protocol::jsonrpc::{JsonRpcId, JsonRpcMessage};
use crate::transports::transport::{
    McpTransport, McpTransportCloseListener, McpTransportErrorListener, McpTransportMessageListener, TransportEvents,
};
use pi_js::Result;
use pi_js::fetch::{BodyStream, Headers, Response};

const MAX_ERROR_BODY_BYTES: i64 = 8 * 1024;
const ERROR_MESSAGE_BODY_CHARS: i64 = 500;
const DEFAULT_RECONNECT_INITIAL_DELAY_MS: i64 = 1_000;
const DEFAULT_RECONNECT_MAX_DELAY_MS: i64 = 30_000;
const DEFAULT_RECONNECT_MAX_RETRIES: i64 = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
}

pub struct ConsumeSseOptions {
    pub max_event_bytes: Option<i64>,
    pub on_event: Arc<dyn Fn(SseEvent) + Send + Sync>,
    /// Called for every `id` field, including events without data (for example resumption priming events).
    pub on_id: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// Called for every valid `retry` field, in milliseconds.
    pub on_retry: Option<Arc<dyn Fn(i64) + Send + Sync>>,
}

pub async fn consume_sse_stream(stream: BodyStream, options: ConsumeSseOptions) -> Result<()> {
    todo!("port: consume_sse_stream")
}

#[derive(Clone, Debug, Default)]
pub struct StreamableHttpReconnectOptions {
    /// Delay before the first reconnection attempt, unless the server sent a `retry` field. Default: 1000.
    pub initial_delay_ms: Option<i64>,
    /// Upper bound for the exponential backoff. Default: 30000.
    pub max_delay_ms: Option<i64>,
    /// Consecutive failed attempts before giving up on a stream. Default: 5.
    pub max_retries: Option<i64>,
}

#[derive(Clone)]
pub struct StreamableHttpTransportOptions {
    pub url: String,
    pub headers: Option<IndexMap<String, String>>,
    pub fetch: Option<McpFetch>,
    /// Open the server-to-client GET stream after initialization. Default: true.
    pub open_get_stream: Option<bool>,
    pub max_message_bytes: Option<i64>,
    pub auth_provider: Option<Arc<dyn AuthProvider + Send + Sync>>,
    /// Reconnection of dropped SSE streams (the GET stream, and response streams that carry event IDs).
    pub reconnect: Option<StreamableHttpReconnectOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct McpHttpError {
    pub message: String,
    pub status: i64,
    pub body: String,
}

impl McpHttpError {
    pub fn new(status: i64, message: impl Into<String>, body: Option<String>) -> Self {
        Self {
            message: message.into(),
            status,
            body: body.unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct McpAuthRequiredError {
    pub message: String,
    pub status: i64,
    pub body: String,
    pub www_authenticate: Option<String>,
}

impl McpAuthRequiredError {
    pub fn new(response: &Response, body: Option<&str>) -> Self {
        todo!("port: McpAuthRequiredError::new")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct McpSessionExpiredError {
    pub message: String,
    pub status: i64,
    pub body: String,
}

impl McpSessionExpiredError {
    pub fn new(body: Option<String>) -> Self {
        Self {
            message: "MCP session expired".to_string(),
            status: 404,
            body: body.unwrap_or_default(),
        }
    }
}

fn content_type(response: &Response) -> Option<String> {
    todo!("port: content_type")
}

/// 401, or 403 with an `insufficient_scope` bearer challenge (step-up authorization).
fn needs_authorization(response: &Response) -> bool {
    todo!("port: needs_authorization")
}

/// Statuses worth retrying when a stream fails to (re)open.
fn is_transient_status(status: i64) -> bool {
    todo!("port: is_transient_status")
}

async fn discard(response: &Response) {
    todo!("port: discard")
}

fn describe_http_failure(status: i64, body: &str) -> String {
    todo!("port: describe_http_failure")
}

struct StreamCursor {
    last_event_id: Option<String>,
    retry_ms: Option<i64>,
    /// Whether the stream delivered any event since it was (re)opened.
    received: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FetchMethod {
    Get,
    Post,
}

struct PreparedHeaders {
    headers: Headers,
    token: Option<String>,
}

#[derive(Clone)]
pub struct StreamableHttpTransport {
    inner: Arc<StreamableHttpTransportInner>,
}

struct StreamableHttpTransportInner {
    url: url::Url,
    options: StreamableHttpTransportOptions,
    /// Call fetch without a receiver. `this.fetch(...)` and `context.fetch(...)` would pass the transport
    /// or the auth context as `this`, which Cloudflare Workers reject for the platform fetch
    /// ("Illegal invocation").
    fetch: McpFetch,
    events: TransportEvents,
    controller: pi_js::abort::AbortController,
    state: Mutex<StreamableHttpState>,
}

struct StreamableHttpState {
    started: bool,
    closed: bool,
    session_id: Option<String>,
    protocol_version: Option<String>,
    get_stream_started: bool,
}

impl StreamableHttpTransport {
    pub fn new(options: StreamableHttpTransportOptions) -> Self {
        todo!("port: StreamableHttpTransport::new")
    }

    pub fn url(&self) -> &url::Url {
        &self.inner.url
    }

    pub fn options(&self) -> &StreamableHttpTransportOptions {
        &self.inner.options
    }

    pub fn session_id(&self) -> Option<String> {
        self.inner.state.lock().unwrap().session_id.clone()
    }

    /// Fetch with auth headers. A 401 (or a 403 asking for more scope) is handed to the auth provider
    /// once, and the request is retried with whatever credentials it left behind.
    async fn authorized_fetch(
        &self,
        method: FetchMethod,
        headers: IndexMap<String, String>,
        body: Option<String>,
    ) -> Result<Response> {
        todo!("port: StreamableHttpTransport::authorized_fetch")
    }

    async fn headers(&self, extra: &IndexMap<String, String>) -> Result<PreparedHeaders> {
        todo!("port: StreamableHttpTransport::headers")
    }

    fn capture_session(&self, response: &Response) {
        todo!("port: StreamableHttpTransport::capture_session")
    }

    async fn check_response(&self, response: &Response) -> Result<()> {
        todo!("port: StreamableHttpTransport::check_response")
    }

    async fn consume_sse(
        &self,
        stream: BodyStream,
        cursor: &mut StreamCursor,
        on_message: Option<Arc<dyn Fn(&JsonRpcMessage) + Send + Sync>>,
    ) -> Result<()> {
        todo!("port: StreamableHttpTransport::consume_sse")
    }

    /// Read the SSE stream answering one request. When the stream ends or breaks before the response
    /// arrives and the server assigned event IDs, resume it with GET and `Last-Event-ID`, as the
    /// server may close response streams at will. Otherwise only this request fails.
    async fn consume_response_stream(&self, body: BodyStream, request_id: JsonRpcId) -> Result<()> {
        todo!("port: StreamableHttpTransport::consume_response_stream")
    }

    fn start_get_stream(&self) {
        todo!("port: StreamableHttpTransport::start_get_stream")
    }

    /// Keep the server-to-client stream open, reconnecting with backoff when it drops.
    async fn run_get_stream(&self) -> Result<()> {
        todo!("port: StreamableHttpTransport::run_get_stream")
    }

    /// Open a GET SSE stream. Resolves to undefined when the server answers 405 (no GET stream).
    async fn open_sse_stream(&self, last_event_id: Option<&str>) -> Result<Option<BodyStream>> {
        todo!("port: StreamableHttpTransport::open_sse_stream")
    }

    /// Network failures and transient statuses are retried; auth, session, and protocol errors are not.
    /// `fetch` reports network failures, including a connection dropped mid-body, as `TypeError`.
    fn is_retryable(&self, error: &pi_js::Error) -> bool {
        todo!("port: StreamableHttpTransport::is_retryable")
    }

    fn reconnect_delay(&self, attempt: i64, server_delay_ms: Option<i64>) -> i64 {
        todo!("port: StreamableHttpTransport::reconnect_delay")
    }

    fn max_delay(&self) -> i64 {
        todo!("port: StreamableHttpTransport::max_delay")
    }

    fn max_retries(&self) -> i64 {
        todo!("port: StreamableHttpTransport::max_retries")
    }

    /// Resolves false when the transport closed while waiting.
    async fn sleep(&self, ms: i64) -> bool {
        todo!("port: StreamableHttpTransport::sleep")
    }
}

#[async_trait]
impl McpTransport for StreamableHttpTransport {
    async fn start(&self) -> Result<()> {
        todo!("port: StreamableHttpTransport::start")
    }

    async fn send(&self, message: JsonRpcMessage) -> Result<()> {
        todo!("port: StreamableHttpTransport::send")
    }

    async fn close(&self) -> Result<()> {
        todo!("port: StreamableHttpTransport::close")
    }

    fn on_message(&self, listener: McpTransportMessageListener) -> pi_js::Unsubscribe {
        self.inner.events.on_message(listener)
    }

    fn on_error(&self, listener: McpTransportErrorListener) -> pi_js::Unsubscribe {
        self.inner.events.on_error(listener)
    }

    fn on_close(&self, listener: McpTransportCloseListener) -> pi_js::Unsubscribe {
        self.inner.events.on_close(listener)
    }

    fn set_protocol_version(&self, version: &str) {
        todo!("port: StreamableHttpTransport::set_protocol_version")
    }
}
