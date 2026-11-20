//! Port of packages/mcp/src/transports/streamable-http.ts

#![allow(dead_code, unused_variables)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use indexmap::IndexMap;
use serde_json::Value;

use crate::auth_provider::{AuthProvider, McpFetch, McpFetchInit, UnauthorizedContext};
use crate::protocol::jsonrpc::{
    JSON_RPC_ERROR_CODES, JsonRpcErrorObject, JsonRpcErrorResponse, JsonRpcId, JsonRpcMessage, JsonRpcResponse,
    JsonRpcVersion, McpAbortError, McpConnectionClosedError, parse_json_rpc_message,
};
use crate::transports::transport::{
    DEFAULT_MAX_MESSAGE_BYTES, McpTransport, McpTransportCloseListener, McpTransportErrorListener,
    McpTransportMessageListener, McpTransportSendOptions, TransportEvents,
};
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::fetch::{Body, BodyStream, Headers, Request, Response};
use pi_js::{Error, Result};

const MAX_ERROR_BODY_BYTES: i64 = 8 * 1024;
const ERROR_MESSAGE_BODY_CHARS: i64 = 500;
const DEFAULT_RECONNECT_INITIAL_DELAY_MS: i64 = 1_000;
const DEFAULT_RECONNECT_MAX_DELAY_MS: i64 = 30_000;
const DEFAULT_RECONNECT_MAX_RETRIES: i64 = 5;

static INSUFFICIENT_SCOPE_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"(?i)(?:^|[\s,])error="?insufficient_scope"?"#).expect("valid regex"));

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

pub async fn consume_sse_stream(mut stream: BodyStream, options: ConsumeSseOptions) -> Result<()> {
    let max_event_bytes = options.max_event_bytes.unwrap_or(DEFAULT_MAX_MESSAGE_BYTES).max(0) as usize;
    let mut decoder = pi_js::text::Utf8StreamDecoder::new();
    let mut buffered = String::new();
    let mut event_name: Option<String> = None;
    let mut event_id: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();
    let mut data_bytes: usize = 0;

    let dispatch = |event_name: &mut Option<String>,
                    event_id: &mut Option<String>,
                    data_lines: &mut Vec<String>,
                    data_bytes: &mut usize| {
        if data_lines.is_empty() {
            *event_name = None;
            *event_id = None;
            return;
        }
        let data = data_lines.join("\n");
        (options.on_event)(SseEvent {
            event: event_name.take(),
            data,
            id: event_id.take(),
        });
        data_lines.clear();
        *data_bytes = 0;
    };

    let process_line = |line: &str,
                        event_name: &mut Option<String>,
                        event_id: &mut Option<String>,
                        data_lines: &mut Vec<String>,
                        data_bytes: &mut usize|
     -> Result<()> {
        if line.is_empty() {
            dispatch(event_name, event_id, data_lines, data_bytes);
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, raw_val) = match line.find(':') {
            Some(idx) => (&line[..idx], &line[idx + 1..]),
            None => (line, ""),
        };
        let value = raw_val.strip_prefix(' ').unwrap_or(raw_val);
        match field {
            "event" => {
                *event_name = Some(value.to_string());
            }
            "data" => {
                let added = value.len() + usize::from(!data_lines.is_empty());
                *data_bytes += added;
                if *data_bytes > max_event_bytes {
                    return Err(Error::msg(format!("MCP SSE event exceeds {max_event_bytes} bytes")));
                }
                data_lines.push(value.to_string());
            }
            "id" => {
                if !value.contains('\0') {
                    *event_id = Some(value.to_string());
                    if let Some(cb) = &options.on_id {
                        cb(value);
                    }
                }
            }
            "retry" => {
                if !value.is_empty()
                    && value.bytes().all(|b| b.is_ascii_digit())
                    && let Ok(ms) = value.parse::<i64>()
                    && let Some(cb) = &options.on_retry
                {
                    cb(ms);
                }
            }
            _ => {}
        }
        Ok(())
    };

    // SSE lines end with CRLF, LF, or a lone CR.
    let mut skip_leading_lf = false;
    let consume_lines = |buffered: &mut String,
                         skip_leading_lf: &mut bool,
                         event_name: &mut Option<String>,
                         event_id: &mut Option<String>,
                         data_lines: &mut Vec<String>,
                         data_bytes: &mut usize|
     -> Result<()> {
        let mut start = 0usize;
        let bytes = buffered.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            let b = bytes[i];
            if b != b'\r' && b != b'\n' {
                i += 1;
                continue;
            }
            process_line(&buffered[start..i], event_name, event_id, data_lines, data_bytes)?;
            if b == b'\r' {
                if i + 1 < bytes.len() {
                    if bytes[i + 1] == b'\n' {
                        i += 1;
                    }
                } else {
                    *skip_leading_lf = true;
                }
            }
            i += 1;
            start = i;
        }
        buffered.drain(..start);
        Ok(())
    };

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res?;
        let decoded = decoder.decode(&chunk, true);
        let mut text = decoded.as_str();
        // A CRLF split across chunks: the CR already ended the line, so drop the LF.
        if skip_leading_lf && !text.is_empty() {
            if let Some(rest) = text.strip_prefix('\n') {
                text = rest;
            }
            skip_leading_lf = false;
        }
        buffered.push_str(text);
        if let Err(err) = consume_lines(
            &mut buffered,
            &mut skip_leading_lf,
            &mut event_name,
            &mut event_id,
            &mut data_lines,
            &mut data_bytes,
        ) {
            drop(stream);
            return Err(err);
        }
        if buffered.len() > max_event_bytes {
            drop(stream);
            return Err(Error::msg(format!("MCP SSE event exceeds {max_event_bytes} bytes")));
        }
    }
    let tail_decoded = decoder.decode(&[], false);
    let mut tail = tail_decoded.as_str();
    if skip_leading_lf && let Some(rest) = tail.strip_prefix('\n') {
        tail = rest;
    }
    buffered.push_str(tail);
    consume_lines(
        &mut buffered,
        &mut skip_leading_lf,
        &mut event_name,
        &mut event_id,
        &mut data_lines,
        &mut data_bytes,
    )?;
    if !buffered.is_empty() {
        process_line(
            &buffered,
            &mut event_name,
            &mut event_id,
            &mut data_lines,
            &mut data_bytes,
        )?;
    }
    dispatch(&mut event_name, &mut event_id, &mut data_lines, &mut data_bytes);
    Ok(())
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
        Self {
            message: "MCP server requires authentication".to_string(),
            status: 401,
            body: body.unwrap_or("").to_string(),
            www_authenticate: response.headers().get("www-authenticate"),
        }
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
    response
        .headers()
        .get("content-type")
        .map(|raw| raw.split(';').next().unwrap_or("").trim().to_ascii_lowercase())
}

/// 401, or 403 with an `insufficient_scope` bearer challenge (step-up authorization).
fn needs_authorization(response: &Response) -> bool {
    if response.status() == 401 {
        return true;
    }
    if response.status() != 403 {
        return false;
    }
    let header = response.headers().get("www-authenticate").unwrap_or_default();
    INSUFFICIENT_SCOPE_RE.is_match(&header)
}

/// Statuses worth retrying when a stream fails to (re)open.
fn is_transient_status(status: i64) -> bool {
    status == 408 || status == 429 || status >= 500
}

async fn discard(response: &Response) {
    response.cancel_body();
}

struct ReadBodyTextResult {
    text: String,
    truncated: bool,
}

/// Reads a response body as text, decoding chunks as they arrive. Once the body exceeds `max_bytes`,
/// the rest is cancelled and the text holds only the first `max_bytes` bytes.
async fn read_body_text(response: &Response, max_bytes: usize) -> Result<ReadBodyTextResult> {
    if !response.has_body() {
        return Ok(ReadBodyTextResult {
            text: String::new(),
            truncated: false,
        });
    }
    let mut stream = response.body_stream()?;
    let mut decoder = pi_js::text::Utf8StreamDecoder::new();
    let mut text = String::new();
    let mut remaining = max_bytes;
    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res?;
        if chunk.len() > remaining {
            drop(stream);
            text.push_str(&decoder.decode(&chunk[..remaining], true));
            return Ok(ReadBodyTextResult { text, truncated: true });
        }
        remaining -= chunk.len();
        text.push_str(&decoder.decode(&chunk, true));
    }
    text.push_str(&decoder.decode(&[], false));
    Ok(ReadBodyTextResult { text, truncated: false })
}

fn describe_http_failure(status: i64, body: &str) -> String {
    let text = pi_js::str16::trim(body);
    let max_chars = ERROR_MESSAGE_BODY_CHARS as usize;
    let snippet = if pi_js::str16::len(text) > max_chars {
        format!(
            "{}...",
            pi_js::str16::slice(text, 0, Some(ERROR_MESSAGE_BODY_CHARS - 3))
        )
    } else {
        text.to_string()
    };
    if snippet.is_empty() {
        format!("MCP HTTP request failed with status {status}")
    } else {
        format!("MCP HTTP request failed with status {status}: {snippet}")
    }
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

impl FetchMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
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
    controller: AbortController,
    state: Mutex<StreamableHttpState>,
}

struct StreamableHttpState {
    started: bool,
    closed: bool,
    session_id: Option<String>,
    protocol_version: Option<String>,
    get_stream_started: bool,
    /// Access token of the latest request, which `close()` reuses instead of asking the auth provider.
    last_token: Option<String>,
}

impl StreamableHttpTransport {
    pub fn new(options: StreamableHttpTransportOptions) -> Self {
        let url = url::Url::parse(&options.url)
            .unwrap_or_else(|e| panic!("Invalid MCP Streamable HTTP URL '{}': {e}", options.url));
        let fetch: McpFetch = options.fetch.clone().unwrap_or_else(|| {
            Arc::new(|url, init| {
                let url = url.to_string();
                Box::pin(async move {
                    let init = init.unwrap_or_default();
                    let mut req = Request::new(init.method.unwrap_or_else(|| "GET".to_string()), url);
                    if let Some(headers) = init.headers {
                        req = req.with_headers(headers);
                    }
                    if let Some(body) = init.body {
                        req = req.body(body);
                    }
                    if let Some(signal) = init.signal {
                        req = req.signal(signal);
                    }
                    pi_js::fetch::fetch(req).await
                })
            })
        });
        Self {
            inner: Arc::new(StreamableHttpTransportInner {
                url,
                options,
                fetch,
                events: TransportEvents::new(),
                controller: AbortController::new(),
                state: Mutex::new(StreamableHttpState {
                    started: false,
                    closed: false,
                    session_id: None,
                    protocol_version: None,
                    get_stream_started: false,
                    last_token: None,
                }),
            }),
        }
    }

    pub fn url(&self) -> &url::Url {
        &self.inner.url
    }

    pub fn options(&self) -> &StreamableHttpTransportOptions {
        &self.inner.options
    }

    pub fn session_id(&self) -> Option<String> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .session_id
            .clone()
    }

    /// Fetch with auth headers. A 401 (or a 403 asking for more scope) is handed to the auth provider
    /// once, and the request is retried with whatever credentials it left behind.
    ///
    /// `signal` only stops dispatch: getting a token or the authorization recovery can take a while, and a
    /// request its sender gave up on in the meantime must not reach the server. It does not abort the shared
    /// recovery, which other requests may wait for.
    async fn authorized_fetch(
        &self,
        method: FetchMethod,
        headers: IndexMap<String, String>,
        body: Option<String>,
        signal: Option<&AbortSignal>,
    ) -> Result<Response> {
        let mut attempt = 0u32;
        loop {
            let prepared = self.headers(&headers).await?;
            if signal.is_some_and(AbortSignal::aborted) {
                return Err(Error::typed(McpAbortError::NAME, McpAbortError::new(None)));
            }
            let init = McpFetchInit {
                method: Some(method.as_str().to_string()),
                headers: Some(prepared.headers),
                body: body.as_ref().map(|b| Body::Text(b.clone())),
                signal: Some(self.inner.controller.signal()),
            };
            let response = (self.inner.fetch)(self.inner.url.as_str(), Some(init)).await?;
            if attempt > 0 || !needs_authorization(&response) {
                return Ok(response);
            }
            let Some(auth_provider) = &self.inner.options.auth_provider else {
                return Ok(response);
            };
            let saved_status = response.status();
            let saved_headers = response.headers().clone();
            let ctx = UnauthorizedContext {
                response,
                server_url: self.inner.url.clone(),
                fetch: self.inner.fetch.clone(),
                token: prepared.token,
            };
            let Some(fut) = auth_provider.on_unauthorized(ctx) else {
                return Ok(Response::null_body(saved_status, saved_headers));
            };
            fut.await?;
            attempt += 1;
        }
    }

    async fn headers(&self, extra: &IndexMap<String, String>) -> Result<PreparedHeaders> {
        let token = if let Some(auth_provider) = &self.inner.options.auth_provider {
            auth_provider.token().await?.filter(|t| !t.is_empty())
        } else {
            None
        };
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).last_token = token.clone();
        let headers = self.build_headers(extra, token.as_deref());
        Ok(PreparedHeaders { headers, token })
    }

    fn build_headers(&self, extra: &IndexMap<String, String>, token: Option<&str>) -> Headers {
        let mut headers = Headers::new();
        if let Some(custom) = &self.inner.options.headers {
            for (name, value) in custom {
                headers.set(name, value);
            }
        }
        for (name, value) in extra {
            headers.set(name, value);
        }
        let (session_id, protocol_version) = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (state.session_id.clone(), state.protocol_version.clone())
        };
        if let Some(sid) = session_id {
            headers.set("Mcp-Session-Id", &sid);
        }
        if let Some(pv) = protocol_version {
            headers.set("MCP-Protocol-Version", &pv);
        }
        if let Some(t) = token {
            headers.set("Authorization", &format!("Bearer {t}"));
        }
        headers
    }

    fn capture_session(&self, response: &Response) {
        if let Some(session_id) = response.headers().get("mcp-session-id").filter(|s| !s.is_empty()) {
            self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).session_id = Some(session_id);
        }
    }

    async fn check_response(&self, response: &Response) -> Result<()> {
        if response.ok() {
            return Ok(());
        }
        let status = i64::from(response.status());
        let had_session = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .session_id
            .is_some();
        // Reads only the excerpt the error keeps and cancels the rest, so a huge error body is never buffered.
        let body = read_body_text(response, MAX_ERROR_BODY_BYTES as usize)
            .await
            .map(|r| r.text)
            .unwrap_or_default();
        if status == 401 {
            return Err(Error::typed(
                "McpAuthRequiredError",
                McpAuthRequiredError::new(response, Some(&body)),
            ));
        }
        if status == 404 && had_session {
            return Err(Error::typed(
                "McpSessionExpiredError",
                McpSessionExpiredError::new(Some(body)),
            ));
        }
        Err(Error::typed(
            "McpHttpError",
            McpHttpError::new(status, describe_http_failure(status, &body), Some(body)),
        ))
    }

    async fn consume_sse(
        &self,
        stream: BodyStream,
        cursor: &mut StreamCursor,
        on_message: Option<Arc<dyn Fn(&JsonRpcMessage) + Send + Sync>>,
    ) -> Result<()> {
        let cursor_state = Arc::new(Mutex::new((
            cursor.last_event_id.clone(),
            cursor.retry_ms,
            cursor.received,
        )));
        let events = self.inner.events.clone();
        let max_event_bytes = self.inner.options.max_message_bytes;

        let res = consume_sse_stream(
            stream,
            ConsumeSseOptions {
                max_event_bytes,
                on_id: Some(Arc::new({
                    let cursor_state = Arc::clone(&cursor_state);
                    move |id| {
                        cursor_state.lock().unwrap_or_else(|e| e.into_inner()).0 = Some(id.to_string());
                    }
                })),
                on_retry: Some(Arc::new({
                    let cursor_state = Arc::clone(&cursor_state);
                    move |delay_ms| {
                        cursor_state.lock().unwrap_or_else(|e| e.into_inner()).1 = Some(delay_ms);
                    }
                })),
                on_event: Arc::new({
                    let cursor_state = Arc::clone(&cursor_state);
                    let events = events.clone();
                    move |event| {
                        cursor_state.lock().unwrap_or_else(|e| e.into_inner()).2 = true;
                        // Events without data prime resumption; other event types are not JSON-RPC.
                        if pi_js::str16::trim(&event.data).is_empty()
                            || event.event.as_deref().is_some_and(|e| e != "message")
                        {
                            return;
                        }
                        let message = match pi_js::json::parse(&event.data).and_then(|v| parse_json_rpc_message(&v)) {
                            Ok(m) => m,
                            Err(err) => {
                                events.emit_error(&err);
                                return;
                            }
                        };
                        if let Some(cb) = &on_message {
                            cb(&message);
                        }
                        events.emit_message(&message);
                    }
                }),
            },
        )
        .await;

        let guard = cursor_state.lock().unwrap_or_else(|e| e.into_inner());
        cursor.last_event_id = guard.0.clone();
        cursor.retry_ms = guard.1;
        cursor.received = guard.2;
        res
    }

    fn is_closed(&self) -> bool {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).closed
    }

    /// Read the SSE stream answering one request. When the stream ends or breaks before the response
    /// arrives and the server assigned event IDs, resume it with GET and `Last-Event-ID`, as the
    /// server may close response streams at will. Otherwise only this request fails.
    async fn consume_response_stream(&self, body: BodyStream, request_id: JsonRpcId) -> Result<()> {
        let mut cursor = StreamCursor {
            last_event_id: None,
            retry_ms: None,
            received: false,
        };
        let answered = Arc::new(AtomicBool::new(false));
        let on_message: Arc<dyn Fn(&JsonRpcMessage) + Send + Sync> = Arc::new({
            let answered = Arc::clone(&answered);
            let target_id = request_id.clone();
            move |message| {
                if let JsonRpcMessage::Response(resp) = message {
                    let resp_id = match resp {
                        JsonRpcResponse::Success(s) => &s.id,
                        JsonRpcResponse::Error(e) => &e.id,
                    };
                    if resp_id == &target_id {
                        answered.store(true, Ordering::SeqCst);
                    }
                }
            }
        });

        let mut stream = Some(body);
        let mut failure: Option<Error> = None;
        let mut attempt = 0i64;
        loop {
            if let Some(s) = stream.take() {
                match self.consume_sse(s, &mut cursor, Some(Arc::clone(&on_message))).await {
                    Ok(()) => failure = None,
                    Err(err) => failure = Some(err),
                }
            }
            if answered.load(Ordering::SeqCst) || self.is_closed() {
                return Ok(());
            }
            if let Some(ref err) = failure
                && !self.is_retryable(err)
            {
                break;
            }
            if cursor.last_event_id.is_none() || attempt >= self.max_retries() {
                break;
            }
            if cursor.received {
                attempt = 0;
            }
            cursor.received = false;
            let delay = self.reconnect_delay(attempt, cursor.retry_ms);
            attempt += 1;
            if !self.sleep(delay).await {
                return Ok(());
            }
            match self.open_sse_stream(cursor.last_event_id.as_deref()).await {
                Ok(next) => stream = next,
                Err(err) => {
                    let retryable = self.is_retryable(&err);
                    failure = Some(err);
                    if !retryable {
                        break;
                    }
                    stream = None;
                }
            }
        }
        if self.is_closed() {
            return Ok(());
        }
        let reason = failure
            .as_ref()
            .map_or_else(|| "stream ended without a response".to_string(), ToString::to_string);
        self.inner
            .events
            .emit_message(&JsonRpcMessage::Response(JsonRpcResponse::Error(
                JsonRpcErrorResponse {
                    jsonrpc: JsonRpcVersion::V2,
                    id: request_id,
                    error: JsonRpcErrorObject {
                        code: JSON_RPC_ERROR_CODES.internal_error,
                        message: format!("MCP response stream failed: {reason}"),
                        data: None,
                    },
                },
            )));
        Ok(())
    }

    fn start_get_stream(&self) {
        if self.inner.options.open_get_stream == Some(false) {
            return;
        }
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.get_stream_started || state.closed {
                return;
            }
            state.get_stream_started = true;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let _ = this.run_get_stream().await;
        });
    }

    /// Keep the server-to-client stream open, reconnecting with backoff when it drops.
    async fn run_get_stream(&self) -> Result<()> {
        let mut cursor = StreamCursor {
            last_event_id: None,
            retry_ms: None,
            received: false,
        };
        let mut attempt = 0i64;
        while !self.is_closed() {
            match self.open_sse_stream(cursor.last_event_id.as_deref()).await {
                Ok(None) => return Ok(()),
                Ok(Some(stream)) => {
                    let opened_at = pi_js::time::now_ms();
                    if let Err(err) = self.consume_sse(stream, &mut cursor, None).await {
                        if self.is_closed() {
                            return Ok(());
                        }
                        if !self.is_retryable(&err) {
                            self.inner.events.emit_error(&err);
                            return Ok(());
                        }
                    } else if cursor.received || pi_js::time::now_ms() - opened_at > self.max_delay() {
                        attempt = 0;
                    }
                }
                Err(err) => {
                    if self.is_closed() {
                        return Ok(());
                    }
                    if !self.is_retryable(&err) {
                        self.inner.events.emit_error(&err);
                        return Ok(());
                    }
                }
            }
            cursor.received = false;
            if attempt >= self.max_retries() {
                self.inner.events.emit_error(&Error::msg(
                    "MCP server-to-client stream dropped and could not be reopened",
                ));
                return Ok(());
            }
            let delay = self.reconnect_delay(attempt, cursor.retry_ms);
            attempt += 1;
            if !self.sleep(delay).await {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Open a GET SSE stream. Resolves to undefined when the server answers 405 (no GET stream).
    async fn open_sse_stream(&self, last_event_id: Option<&str>) -> Result<Option<BodyStream>> {
        let mut extra = IndexMap::new();
        extra.insert("accept".to_string(), "text/event-stream".to_string());
        if let Some(id) = last_event_id {
            extra.insert("last-event-id".to_string(), id.to_string());
        }
        let response = self.authorized_fetch(FetchMethod::Get, extra, None, None).await?;
        if response.status() == 405 {
            discard(&response).await;
            return Ok(None);
        }
        self.check_response(&response).await?;
        self.capture_session(&response);
        let ct = content_type(&response);
        if ct.as_deref() != Some("text/event-stream") || !response.has_body() {
            let status = i64::from(response.status());
            discard(&response).await;
            return Err(Error::typed(
                "McpHttpError",
                McpHttpError::new(
                    status,
                    format!(
                        "Unsupported MCP GET response content type: {}",
                        ct.as_deref().unwrap_or("missing")
                    ),
                    None,
                ),
            ));
        }
        Ok(Some(response.body_stream()?))
    }

    /// Network failures and transient statuses are retried; auth, session, and protocol errors are not.
    /// `fetch` reports network failures, including a connection dropped mid-body, as `TypeError`.
    fn is_retryable(&self, error: &pi_js::Error) -> bool {
        if let Some(http) = error.downcast_ref::<McpHttpError>() {
            return is_transient_status(http.status);
        }
        if error.name() == "TypeError" {
            return true;
        }
        error
            .code()
            .is_some_and(|code| code.starts_with('E') || code.starts_with("UND_ERR"))
    }

    fn reconnect_delay(&self, attempt: i64, server_delay_ms: Option<i64>) -> i64 {
        if let Some(delay) = server_delay_ms {
            return delay;
        }
        let initial = self
            .inner
            .options
            .reconnect
            .as_ref()
            .and_then(|r| r.initial_delay_ms)
            .unwrap_or(DEFAULT_RECONNECT_INITIAL_DELAY_MS);
        let max = self.max_delay();
        let exp = (initial as f64) * 2f64.powi(attempt.clamp(0, 62) as i32);
        exp.min(max as f64) as i64
    }

    fn max_delay(&self) -> i64 {
        self.inner
            .options
            .reconnect
            .as_ref()
            .and_then(|r| r.max_delay_ms)
            .unwrap_or(DEFAULT_RECONNECT_MAX_DELAY_MS)
    }

    fn max_retries(&self) -> i64 {
        self.inner
            .options
            .reconnect
            .as_ref()
            .and_then(|r| r.max_retries)
            .unwrap_or(DEFAULT_RECONNECT_MAX_RETRIES)
    }

    /// Resolves false when the transport closed while waiting.
    async fn sleep(&self, ms: i64) -> bool {
        let signal = self.inner.controller.signal();
        if signal.aborted() {
            return false;
        }
        pi_js::time::sleep(ms.max(0) as u64, Some(&signal)).await.is_ok()
    }
}

impl StreamableHttpTransport {
    async fn send_inner(&self, message: JsonRpcMessage, signal: Option<&AbortSignal>) -> Result<()> {
        {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.started || state.closed {
                return Err(Error::typed(
                    "McpConnectionClosedError",
                    McpConnectionClosedError::new(None),
                ));
            }
        }
        let mut extra = IndexMap::new();
        extra.insert("accept".to_string(), "application/json, text/event-stream".to_string());
        extra.insert("content-type".to_string(), "application/json".to_string());
        let body = pi_js::json::stringify(&message);
        let response = self
            .authorized_fetch(FetchMethod::Post, extra, Some(body), signal)
            .await?;
        self.check_response(&response).await?;
        self.capture_session(&response);

        let request_info = match &message {
            JsonRpcMessage::Request(r) => Some((r.id.clone(), r.method.clone())),
            _ => None,
        };
        let is_initialized = matches!(
            &message,
            JsonRpcMessage::Notification(n) if n.method == "notifications/initialized"
        );
        let Some((request_id, request_method)) = request_info else {
            // Notifications and responses are acknowledged with 202 and carry no reply; ignore any body.
            discard(&response).await;
            // The server-to-client stream may only open once the session is initialized.
            if is_initialized {
                self.start_get_stream();
            }
            return Ok(());
        };
        let status = i64::from(response.status());
        if status == 202 || status == 204 {
            discard(&response).await;
            return Err(Error::typed(
                "McpHttpError",
                McpHttpError::new(
                    status,
                    format!("MCP server accepted request {request_method} without a response"),
                    None,
                ),
            ));
        }
        let ct = content_type(&response);
        if ct.as_deref() == Some("application/json") {
            let max_bytes = self
                .inner
                .options
                .max_message_bytes
                .unwrap_or(DEFAULT_MAX_MESSAGE_BYTES)
                .max(0) as usize;
            let read = read_body_text(&response, max_bytes).await?;
            if read.truncated {
                return Err(Error::msg(format!("MCP JSON response exceeds {max_bytes} bytes")));
            }
            let parsed = pi_js::json::parse(&read.text)?;
            match parsed {
                Value::Array(items) => {
                    for item in items {
                        self.inner.events.emit_message(&parse_json_rpc_message(&item)?);
                    }
                }
                other => {
                    self.inner.events.emit_message(&parse_json_rpc_message(&other)?);
                }
            }
            return Ok(());
        }
        if ct.as_deref() == Some("text/event-stream") && response.has_body() {
            let stream = response.body_stream()?;
            let this = self.clone();
            tokio::spawn(async move {
                let _ = this.consume_response_stream(stream, request_id).await;
            });
            return Ok(());
        }
        discard(&response).await;
        Err(Error::typed(
            "McpHttpError",
            McpHttpError::new(
                status,
                format!(
                    "Unsupported MCP response content type: {}",
                    ct.as_deref().unwrap_or("missing")
                ),
                None,
            ),
        ))
    }
}

#[async_trait]
impl McpTransport for StreamableHttpTransport {
    async fn start(&self) -> Result<()> {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.started {
            return Err(Error::msg("MCP Streamable HTTP transport already started"));
        }
        if state.closed {
            return Err(Error::typed(
                "McpConnectionClosedError",
                McpConnectionClosedError::new(None),
            ));
        }
        state.started = true;
        Ok(())
    }

    async fn send(&self, message: JsonRpcMessage) -> Result<()> {
        self.send_inner(message, None).await
    }

    async fn send_with_options(&self, message: JsonRpcMessage, options: McpTransportSendOptions) -> Result<()> {
        self.send_inner(message, options.signal.as_ref()).await
    }

    async fn close(&self) -> Result<()> {
        let (should_delete, last_token) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed {
                return Ok(());
            }
            state.closed = true;
            (state.started && state.session_id.is_some(), state.last_token.clone())
        };
        self.inner.controller.abort(None);
        if should_delete {
            // Best effort: the session expires on the server anyway. The auth provider may refresh tokens
            // over the network, so it is not asked here and closing never waits for a refresh.
            let headers = self.build_headers(&IndexMap::new(), last_token.as_deref());
            let timeout_signal = AbortSignal::timeout(1_000);
            let init = McpFetchInit {
                method: Some("DELETE".to_string()),
                headers: Some(headers),
                body: None,
                signal: Some(timeout_signal),
            };
            if let Ok(response) = (self.inner.fetch)(self.inner.url.as_str(), Some(init)).await {
                discard(&response).await;
            }
        }
        self.inner.events.emit_close();
        Ok(())
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
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .protocol_version = Some(version.to_string());
    }
}
