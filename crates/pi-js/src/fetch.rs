//! pi_js::fetch (Rust-only; API contract: PORTING.md Appendix A).
//!
//! The WHATWG `fetch` that every pi crate calls (PORTING.md §7). The network path reproduces
//! undici 8 as Node 24 runs it for pi:
//!
//! - Request validation and its `TypeError` messages (URL parsing, credentials in the URL, method
//!   normalization, forbidden methods, header names and values, GET/HEAD bodies).
//! - The headers undici adds (`accept`, `accept-language`, `sec-fetch-mode`, `accept-encoding`,
//!   `content-type` for text bodies, `content-length: 0` for empty POST/PUT); the user agent comes
//!   from the client (`node`, see [`client_builder`]).
//! - Redirects (`redirect: "follow"`): at most 20, 301/302 POST and 303 non-GET/HEAD become GET
//!   without body headers, cross-origin hops drop `authorization`, `proxy-authorization`, `cookie`.
//! - Errors: network failures reject with `TypeError: fetch failed` whose `cause` is the undici or
//!   Node error (`connect ECONNREFUSED 127.0.0.1:1`, `getaddrinfo ENOTFOUND host`, `bad port`,
//!   `redirect count exceeded`, ...); a failing body stream yields `TypeError: terminated`.
//! - Abort: an aborted signal rejects with its reason, before and during the request and while the
//!   body is read.
//!
//! Tests replace the network with [`testing::mock_fetch`] (thread-local, like
//! `vi.stubGlobal("fetch")`) or [`testing::deny_network`].

use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::task::{Context, Poll};
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use futures::stream::{self, BoxStream, Stream, StreamExt};
use reqwest::header::{CONNECTION, HeaderMap, HeaderName, HeaderValue, LOCATION};
use tokio_util::sync::WaitForCancellationFutureOwned;
use url::{Host, Url};

use crate::BoxFuture;
use crate::abort::{AbortReason, AbortSignal};
use crate::error::{Error, JsError, NodeError, Result};
use crate::seam::{Guard, Slot};

// ---------------------------------------------------------------------------------------------
// Headers
// ---------------------------------------------------------------------------------------------

/// WHATWG `Headers`: an ordered header list with case-insensitive names.
///
/// `append` / `set` normalize the value (strip leading and trailing tab, LF, CR, space) but do not
/// reject invalid input; [`fetch`] validates every header with undici's messages before sending,
/// and [`Headers::try_append`] / [`Headers::try_set`] validate eagerly like the JS methods.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Headers {
    list: Vec<(String, String)>,
}

impl fmt::Debug for Headers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.list.iter().map(|(k, v)| (k, v))).finish()
    }
}

impl<K: AsRef<str>, V: AsRef<str>> FromIterator<(K, V)> for Headers {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Headers {
        let mut headers = Headers::new();
        for (k, v) in iter {
            headers.append(k.as_ref(), v.as_ref());
        }
        headers
    }
}

impl Headers {
    pub fn new() -> Headers {
        Headers::default()
    }

    /// `new Headers(pairs)` without validation (see [`Headers::try_from_pairs`]).
    pub fn from_pairs<K: AsRef<str>, V: AsRef<str>>(pairs: impl IntoIterator<Item = (K, V)>) -> Headers {
        pairs.into_iter().collect()
    }

    /// `new Headers(pairs)`: fails with the `TypeError` JS throws for an invalid name or value.
    pub fn try_from_pairs<K: AsRef<str>, V: AsRef<str>>(pairs: impl IntoIterator<Item = (K, V)>) -> Result<Headers> {
        let mut headers = Headers::new();
        for (k, v) in pairs {
            headers.try_append(k.as_ref(), v.as_ref())?;
        }
        Ok(headers)
    }

    /// `headers.append(name, value)`. A repeated name reuses the casing of its first occurrence.
    pub fn append(&mut self, name: &str, value: &str) {
        let value = normalize_header_value(value).to_string();
        let name = match self.list.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
            Some((first, _)) => first.clone(),
            None => name.to_string(),
        };
        self.list.push((name, value));
    }

    /// `headers.append(name, value)` with JS validation.
    pub fn try_append(&mut self, name: &str, value: &str) -> Result<()> {
        validate_header("append", name, value)?;
        self.append(name, value);
        Ok(())
    }

    /// `headers.set(name, value)`: replaces the first entry and removes the others.
    pub fn set(&mut self, name: &str, value: &str) {
        let value = normalize_header_value(value).to_string();
        match self.list.iter().position(|(n, _)| n.eq_ignore_ascii_case(name)) {
            Some(first) => {
                self.list[first].1 = value;
                let mut i = 0;
                self.list.retain(|(n, _)| {
                    let keep = i <= first || !n.eq_ignore_ascii_case(name);
                    i += 1;
                    keep
                });
            }
            None => self.list.push((name.to_string(), value)),
        }
    }

    /// `headers.set(name, value)` with JS validation.
    pub fn try_set(&mut self, name: &str, value: &str) -> Result<()> {
        validate_header("set", name, value)?;
        self.set(name, value);
        Ok(())
    }

    /// `headers.get(name)`: every value for `name` joined with `", "` (set-cookie included).
    pub fn get(&self, name: &str) -> Option<String> {
        let mut values = self
            .list
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str());
        let first = values.next()?;
        let mut out = first.to_string();
        for v in values {
            out.push_str(", ");
            out.push_str(v);
        }
        Some(out)
    }

    /// `headers.getSetCookie()`.
    pub fn get_set_cookie(&self) -> Vec<String> {
        self.list
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v.clone())
            .collect()
    }

    /// `headers.has(name)`.
    pub fn has(&self, name: &str) -> bool {
        self.list.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
    }

    /// `headers.delete(name)`.
    pub fn delete(&mut self, name: &str) {
        self.list.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    }

    /// `[...headers]` / `headers.entries()`: lowercased names in sorted order, values combined with
    /// `", "` except `set-cookie`, which yields one entry per value.
    pub fn entries(&self) -> Vec<(String, String)> {
        let mut names: Vec<String> = Vec::new();
        for (n, _) in &self.list {
            let lower = n.to_ascii_lowercase();
            if !names.contains(&lower) {
                names.push(lower);
            }
        }
        names.sort();
        let mut out = Vec::with_capacity(self.list.len());
        for name in names {
            if name == "set-cookie" {
                out.extend(self.get_set_cookie().into_iter().map(|v| (name.clone(), v)));
            } else if let Some(v) = self.get(&name) {
                out.push((name, v));
            }
        }
        out
    }

    /// `[...headers.keys()]`.
    pub fn keys(&self) -> Vec<String> {
        self.entries().into_iter().map(|(k, _)| k).collect()
    }

    /// `[...headers.values()]`.
    pub fn values(&self) -> Vec<String> {
        self.entries().into_iter().map(|(_, v)| v).collect()
    }

    /// The underlying header list in insertion order, names as given.
    pub fn raw(&self) -> &[(String, String)] {
        &self.list
    }

    /// Number of entries in the underlying header list.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Distinct names in insertion order (casing of the first occurrence).
    fn distinct_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = Vec::new();
        for (n, _) in &self.list {
            if !names.iter().any(|m| m.eq_ignore_ascii_case(n)) {
                names.push(n);
            }
        }
        names
    }
}

fn is_http_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | ' ')
}

fn normalize_header_value(v: &str) -> &str {
    v.trim_matches(is_http_whitespace)
}

fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// WebIDL `ByteString` conversion: every UTF-16 code unit must be <= 0xFF.
fn check_byte_string(s: &str) -> Result<()> {
    match s.encode_utf16().enumerate().find(|&(_, u)| u > 0xFF) {
        Some((index, unit)) => Err(type_error(format!(
            "Cannot convert argument to a ByteString because the character at index {index} has a value of {unit} which is greater than 255."
        ))),
        None => Ok(()),
    }
}

fn validate_header(op: &str, name: &str, value: &str) -> Result<()> {
    check_byte_string(name)?;
    check_byte_string(value)?;
    if !is_token(name) {
        return Err(type_error(format!(
            "Headers.{op}: \"{name}\" is an invalid header name."
        )));
    }
    let value = normalize_header_value(value);
    if value.contains(['\0', '\r', '\n']) {
        return Err(type_error(format!(
            "Headers.{op}: \"{value}\" is an invalid header value."
        )));
    }
    Ok(())
}

/// Latin-1 bytes of a validated `ByteString`.
fn isomorphic_encode(s: &str) -> Vec<u8> {
    s.chars().map(|c| c as u32 as u8).collect()
}

fn isomorphic_decode(b: &[u8]) -> String {
    b.iter().map(|&b| char::from(b)).collect()
}

// ---------------------------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------------------------

/// A request body (`BodyInit` restricted to what pi sends).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    /// `Uint8Array` / `ArrayBuffer` / `Buffer`: sent without a default content type.
    Bytes(Bytes),
    /// A string: sent as UTF-8 with `content-type: text/plain;charset=UTF-8` unless set.
    Text(String),
}

impl Body {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Body::Bytes(b) => b,
            Body::Text(s) => s.as_bytes(),
        }
    }

    pub fn len(&self) -> usize {
        self.as_bytes().len()
    }

    pub fn is_empty(&self) -> bool {
        self.as_bytes().is_empty()
    }

    /// The body decoded as UTF-8 (invalid sequences become U+FFFD).
    pub fn to_text_lossy(&self) -> String {
        match self {
            Body::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
            Body::Text(s) => s.clone(),
        }
    }

    fn to_bytes(&self) -> Bytes {
        match self {
            Body::Bytes(b) => b.clone(),
            Body::Text(s) => Bytes::from(s.clone()),
        }
    }
}

impl From<String> for Body {
    fn from(s: String) -> Body {
        Body::Text(s)
    }
}

impl From<&str> for Body {
    fn from(s: &str) -> Body {
        Body::Text(s.to_string())
    }
}

impl From<Bytes> for Body {
    fn from(b: Bytes) -> Body {
        Body::Bytes(b)
    }
}

impl From<Vec<u8>> for Body {
    fn from(b: Vec<u8>) -> Body {
        Body::Bytes(Bytes::from(b))
    }
}

impl From<&[u8]> for Body {
    fn from(b: &[u8]) -> Body {
        Body::Bytes(Bytes::copy_from_slice(b))
    }
}

/// `fetch(url, { method, headers, body, signal })`.
#[derive(Clone)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Headers,
    pub body: Option<Body>,
    pub signal: Option<AbortSignal>,
}

impl Default for Request {
    fn default() -> Request {
        Request {
            method: "GET".to_string(),
            url: String::new(),
            headers: Headers::new(),
            body: None,
            signal: None,
        }
    }
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("body", &self.body)
            .field(
                "signal",
                &self
                    .signal
                    .as_ref()
                    .map(|s| if s.aborted() { "aborted" } else { "active" }),
            )
            .finish()
    }
}

impl Request {
    pub fn new(method: impl Into<String>, url: impl Into<String>) -> Request {
        Request {
            method: method.into(),
            url: url.into(),
            ..Request::default()
        }
    }

    pub fn get(url: impl Into<String>) -> Request {
        Request::new("GET", url)
    }

    pub fn post(url: impl Into<String>) -> Request {
        Request::new("POST", url)
    }

    /// Appends a header (`headers: { name: value }`).
    pub fn header(mut self, name: &str, value: &str) -> Request {
        self.headers.append(name, value);
        self
    }

    pub fn with_headers(mut self, headers: Headers) -> Request {
        self.headers = headers;
        self
    }

    pub fn body(mut self, body: impl Into<Body>) -> Request {
        self.body = Some(body.into());
        self
    }

    pub fn signal(mut self, signal: AbortSignal) -> Request {
        self.signal = Some(signal);
        self
    }

    /// The body as UTF-8 text (for assertions in `mock_fetch` handlers).
    pub fn body_text(&self) -> Option<String> {
        self.body.as_ref().map(Body::to_text_lossy)
    }

    /// The body parsed as JSON (for assertions in `mock_fetch` handlers).
    pub fn body_json(&self) -> Option<serde_json::Value> {
        crate::json::parse(&self.body_text()?).ok()
    }
}

// ---------------------------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------------------------

enum BodyState {
    /// `response.body === null` (null-body status, HEAD, `new Response(null)`).
    Null,
    Unread(BodyStream),
    /// Read, locked or cancelled.
    Disturbed,
}

/// A fetch `Response`. Body readers take `&self` and fail like JS once the body was consumed.
pub struct Response {
    status: u16,
    status_text: String,
    headers: Headers,
    url: String,
    redirected: bool,
    body: Mutex<BodyState>,
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("status_text", &self.status_text)
            .field("url", &self.url)
            .field("redirected", &self.redirected)
            .field("headers", &self.headers)
            .field("body_used", &self.body_used())
            .finish()
    }
}

impl Response {
    /// `new Response(bytes, { status, headers })`.
    pub fn new(status: u16, headers: Headers, body: impl Into<Bytes>) -> Response {
        let body: Bytes = body.into();
        let stream = if body.is_empty() {
            BodyStream::empty()
        } else {
            BodyStream::new(stream::once(async move { Ok(body) }))
        };
        Response::with_state(status, headers, BodyState::Unread(stream))
    }

    /// A response whose body arrives in the chunks of `body` (a streaming `ReadableStream`).
    pub fn from_stream(
        status: u16,
        headers: Headers,
        body: impl Stream<Item = Result<Bytes>> + Send + 'static,
    ) -> Response {
        Response::with_state(status, headers, BodyState::Unread(BodyStream::new(body)))
    }

    /// `new Response(null, { status, headers })`.
    pub fn null_body(status: u16, headers: Headers) -> Response {
        Response::with_state(status, headers, BodyState::Null)
    }

    fn with_state(status: u16, headers: Headers, body: BodyState) -> Response {
        Response {
            status,
            status_text: String::new(),
            headers,
            url: String::new(),
            redirected: false,
            body: Mutex::new(body),
        }
    }

    pub fn with_status_text(mut self, status_text: impl Into<String>) -> Response {
        self.status_text = status_text.into();
        self
    }

    pub fn with_url(mut self, url: impl Into<String>) -> Response {
        self.url = url.into();
        self
    }

    pub fn with_redirected(mut self, redirected: bool) -> Response {
        self.redirected = redirected;
        self
    }

    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn status_text(&self) -> &str {
        &self.status_text
    }

    /// `response.ok`: status in 200..=299.
    pub fn ok(&self) -> bool {
        (200..=299).contains(&self.status)
    }

    pub fn headers(&self) -> &Headers {
        &self.headers
    }

    /// `response.url`: the final URL without fragment (`""` for constructed responses).
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn redirected(&self) -> bool {
        self.redirected
    }

    /// `response.body !== null`.
    pub fn has_body(&self) -> bool {
        !matches!(*self.lock_body(), BodyState::Null)
    }

    /// `response.bodyUsed`.
    pub fn body_used(&self) -> bool {
        matches!(*self.lock_body(), BodyState::Disturbed)
    }

    /// `await response.bytes()`.
    pub async fn bytes(&self) -> Result<Bytes> {
        let Some(mut stream) = self.take_body()? else {
            return Ok(Bytes::new());
        };
        let mut chunks: Vec<Bytes> = Vec::new();
        while let Some(chunk) = stream.next().await {
            chunks.push(chunk?);
        }
        Ok(match chunks.len() {
            0 => Bytes::new(),
            1 => chunks.pop().unwrap_or_default(),
            _ => Bytes::from(chunks.concat()),
        })
    }

    /// `await response.text()`: UTF-8 decode with BOM removal and U+FFFD replacement.
    pub async fn text(&self) -> Result<String> {
        let bytes = self.bytes().await?;
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    /// `await response.json()`.
    pub async fn json(&self) -> Result<serde_json::Value> {
        crate::json::parse(&self.text().await?)
    }

    /// `response.body` as a byte stream. Fails like `getReader()` on a locked or read body; a null
    /// body yields an empty stream (check [`Response::has_body`] for `response.body === null`).
    pub fn body_stream(&self) -> Result<BodyStream> {
        let mut state = self.lock_body();
        match std::mem::replace(&mut *state, BodyState::Disturbed) {
            BodyState::Null => {
                *state = BodyState::Null;
                Ok(BodyStream::empty())
            }
            BodyState::Unread(stream) => Ok(stream),
            BodyState::Disturbed => Err(js_error(
                "TypeError",
                "Invalid state: ReadableStream is locked",
                Some("ERR_INVALID_STATE"),
                None,
            )),
        }
    }

    /// The body parsed as a `text/event-stream` (see [`SseParser`]).
    pub fn sse(&self) -> Result<SseStream> {
        Ok(SseStream::new(self.body_stream()?))
    }

    /// `await response.body?.cancel()`: drops the body (closing the connection) and marks it used.
    pub fn cancel_body(&self) {
        let mut state = self.lock_body();
        if matches!(*state, BodyState::Unread(_)) {
            *state = BodyState::Disturbed;
        }
    }

    fn lock_body(&self) -> std::sync::MutexGuard<'_, BodyState> {
        self.body.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn take_body(&self) -> Result<Option<BodyStream>> {
        let mut state = self.lock_body();
        match std::mem::replace(&mut *state, BodyState::Disturbed) {
            BodyState::Null => {
                *state = BodyState::Null;
                Ok(None)
            }
            BodyState::Unread(stream) => Ok(Some(stream)),
            BodyState::Disturbed => Err(type_error("Body is unusable: Body has already been read")),
        }
    }
}

/// A response body as a stream of chunks. Aborting the request signal ends it with the reason.
pub struct BodyStream {
    inner: BoxStream<'static, Result<Bytes>>,
    abort: Option<(AbortSignal, Pin<Box<WaitForCancellationFutureOwned>>)>,
    done: bool,
}

impl BodyStream {
    pub fn new(stream: impl Stream<Item = Result<Bytes>> + Send + 'static) -> BodyStream {
        BodyStream {
            inner: stream.boxed(),
            abort: None,
            done: false,
        }
    }

    pub fn empty() -> BodyStream {
        BodyStream::new(stream::empty())
    }

    fn with_signal(mut self, signal: Option<AbortSignal>) -> BodyStream {
        self.abort = signal.map(|s| {
            let cancelled = Box::pin(s.token().cancelled_owned());
            (s, cancelled)
        });
        self
    }
}

impl fmt::Debug for BodyStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BodyStream")
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl Stream for BodyStream {
    type Item = Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes>>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        if let Some((signal, cancelled)) = &mut this.abort
            && (signal.aborted() || cancelled.as_mut().poll(cx).is_ready())
        {
            this.done = true;
            return Poll::Ready(Some(Err(Error::Abort(
                signal.reason().unwrap_or_else(AbortReason::abort),
            ))));
        }
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(None) => {
                this.done = true;
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(e))) => {
                this.done = true;
                Poll::Ready(Some(Err(e)))
            }
            other => other,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Server-sent events
// ---------------------------------------------------------------------------------------------

/// One dispatched `text/event-stream` event (an EventSource `MessageEvent`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event` field, `"message"` when absent.
    pub event: String,
    /// The `data` lines joined with `\n`.
    pub data: String,
    /// The last event ID (`lastEventId`); it persists across events until another `id` field.
    /// `None` when it is empty.
    pub id: Option<String>,
    /// A valid `retry` field seen since the previous dispatched event.
    pub retry: Option<u64>,
}

/// The WHATWG EventSource stream parser (HTML §9.2.6): UTF-8 decoding with BOM removal, CR / LF /
/// CRLF line endings, `event` / `data` / `id` / `retry` fields, comments ignored, and an event that
/// is not terminated by a blank line before the end of the stream is discarded.
#[derive(Debug, Default)]
pub struct SseParser {
    pending_utf8: Vec<u8>,
    started: bool,
    after_cr: bool,
    line: String,
    data: String,
    event: String,
    last_event_id: String,
    retry: Option<u64>,
}

impl SseParser {
    pub fn new() -> SseParser {
        SseParser::default()
    }

    /// Feeds raw bytes; UTF-8 sequences may be split across calls.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        let text = self.decode(chunk);
        let mut out = Vec::new();
        self.push_text(&text, &mut out);
        out
    }

    /// Feeds already decoded text.
    pub fn feed_str(&mut self, text: &str) -> Vec<SseEvent> {
        let mut out = Vec::new();
        self.push_text(text, &mut out);
        out
    }

    /// The current last event ID buffer.
    pub fn last_event_id(&self) -> &str {
        &self.last_event_id
    }

    fn decode(&mut self, chunk: &[u8]) -> String {
        let joined;
        let mut rest: &[u8] = if self.pending_utf8.is_empty() {
            chunk
        } else {
            let mut buf = std::mem::take(&mut self.pending_utf8);
            buf.extend_from_slice(chunk);
            joined = buf;
            &joined
        };
        let mut out = String::with_capacity(rest.len());
        loop {
            match std::str::from_utf8(rest) {
                Ok(s) => {
                    out.push_str(s);
                    break;
                }
                Err(e) => {
                    let (valid, after) = rest.split_at(e.valid_up_to());
                    out.push_str(std::str::from_utf8(valid).unwrap_or_default());
                    match e.error_len() {
                        Some(n) => {
                            out.push('\u{FFFD}');
                            rest = &after[n..];
                        }
                        None => {
                            self.pending_utf8 = after.to_vec();
                            break;
                        }
                    }
                }
            }
        }
        out
    }

    fn push_text(&mut self, text: &str, out: &mut Vec<SseEvent>) {
        let mut rest = text;
        if !self.started && !rest.is_empty() {
            self.started = true;
            rest = rest.strip_prefix('\u{FEFF}').unwrap_or(rest);
        }
        while !rest.is_empty() {
            if self.after_cr {
                self.after_cr = false;
                if let Some(r) = rest.strip_prefix('\n') {
                    rest = r;
                    continue;
                }
            }
            match rest.find(['\r', '\n']) {
                None => {
                    self.line.push_str(rest);
                    break;
                }
                Some(i) => {
                    self.line.push_str(&rest[..i]);
                    let line = std::mem::take(&mut self.line);
                    if let Some(event) = self.process_line(&line) {
                        out.push(event);
                    }
                    self.after_cr = rest.as_bytes()[i] == b'\r';
                    rest = &rest[i + 1..];
                }
            }
        }
    }

    fn process_line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_string(),
            "data" => {
                self.data.push_str(value);
                self.data.push('\n');
            }
            "id" if !value.contains('\0') => self.last_event_id = value.to_string(),
            "retry" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                self.retry = Some(value.parse().unwrap_or(u64::MAX));
            }
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        if self.data.is_empty() {
            self.event.clear();
            return None;
        }
        let mut data = std::mem::take(&mut self.data);
        if data.ends_with('\n') {
            data.pop();
        }
        let event = std::mem::take(&mut self.event);
        Some(SseEvent {
            event: if event.is_empty() { "message".to_string() } else { event },
            data,
            id: (!self.last_event_id.is_empty()).then(|| self.last_event_id.clone()),
            retry: self.retry.take(),
        })
    }
}

/// [`Response::sse`]: the events of a `text/event-stream` body.
pub struct SseStream {
    body: BodyStream,
    parser: SseParser,
    queue: VecDeque<SseEvent>,
    done: bool,
}

impl SseStream {
    pub fn new(body: BodyStream) -> SseStream {
        SseStream {
            body,
            parser: SseParser::new(),
            queue: VecDeque::new(),
            done: false,
        }
    }
}

impl fmt::Debug for SseStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SseStream")
            .field("queued", &self.queue.len())
            .field("done", &self.done)
            .finish()
    }
}

impl Stream for SseStream {
    type Item = Result<SseEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<SseEvent>>> {
        let this = self.get_mut();
        loop {
            if let Some(event) = this.queue.pop_front() {
                return Poll::Ready(Some(Ok(event)));
            }
            if this.done {
                return Poll::Ready(None);
            }
            match Pin::new(&mut this.body).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => this.queue.extend(this.parser.feed(&chunk)),
                Poll::Ready(Some(Err(e))) => {
                    this.done = true;
                    return Poll::Ready(Some(Err(e)));
                }
                // An unterminated trailing event is discarded (spec).
                Poll::Ready(None) => this.done = true,
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// fetch
// ---------------------------------------------------------------------------------------------

type FetchHandler = dyn Fn(Request) -> BoxFuture<Result<Response>> + Send + Sync;

thread_local! {
    static FETCH_OVERRIDE: Slot<FetchHandler> = Slot::new();
}

static DEFAULT_CLIENT: RwLock<Option<reqwest::Client>> = RwLock::new(None);

/// The `user-agent` Node 24's built-in fetch sends. npm undici (after `undici.install()` in
/// `http-dispatcher.ts`) sends `undici`; that port sets it with `client_builder().user_agent(..)`.
pub const DEFAULT_USER_AGENT: &str = "node";

/// Redirect hops before `redirect count exceeded`.
pub const MAX_REDIRECTS: u32 = 20;

/// undici `connectTimeout`.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// undici `bodyTimeout` / `headersTimeout` (also pi's `DEFAULT_HTTP_IDLE_TIMEOUT_MS`).
pub const READ_TIMEOUT: Duration = Duration::from_secs(300);

/// undici `keepAliveTimeout` for idle pooled sockets.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(4);

const REDIRECT_STATUSES: [u16; 5] = [301, 302, 303, 307, 308];
const NULL_BODY_STATUSES: [u16; 4] = [101, 204, 205, 304];
const REQUEST_BODY_HEADERS: [&str; 4] = [
    "content-encoding",
    "content-language",
    "content-location",
    "content-type",
];

/// Fetch "bad ports" (undici `badPorts`).
const BAD_PORTS: [u16; 81] = [
    1, 7, 9, 11, 13, 15, 17, 19, 20, 21, 22, 23, 25, 37, 42, 43, 53, 69, 77, 79, 87, 95, 101, 102, 103, 104, 109, 110,
    111, 113, 115, 117, 119, 123, 135, 137, 139, 143, 161, 179, 389, 427, 465, 512, 513, 514, 515, 526, 530, 531, 532,
    540, 548, 554, 556, 563, 587, 601, 636, 989, 990, 993, 995, 1719, 1720, 1723, 2049, 3659, 4045, 4190, 5060, 5061,
    6000, 6566, 6665, 6666, 6667, 6668, 6669, 6679, 6697,
];

/// The reqwest configuration matching undici's defaults (PORTING.md §7): HTTP/1.1 only, rustls,
/// gzip/br/deflate/zstd decoding, 10 s connect timeout, 300 s read timeout, no total timeout,
/// no proxy, no automatic redirects or retries (fetch handles redirects itself).
pub fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .http1_only()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .gzip(true)
        .brotli(true)
        .deflate(true)
        .zstd(true)
        .user_agent(DEFAULT_USER_AGENT)
}

/// Replaces the process-wide client used by [`fetch`] (`undici.setGlobalDispatcher`).
pub fn set_default_client(c: reqwest::Client) {
    *DEFAULT_CLIENT.write().unwrap_or_else(PoisonError::into_inner) = Some(c);
}

/// The process-wide client, built from [`client_builder`] on first use.
pub fn default_client() -> reqwest::Client {
    if let Some(c) = DEFAULT_CLIENT.read().unwrap_or_else(PoisonError::into_inner).as_ref() {
        return c.clone();
    }
    DEFAULT_CLIENT
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(|| {
            client_builder()
                .build()
                .expect("default reqwest client (TLS backend) failed to initialize")
        })
        .clone()
}

/// `await fetch(url, init)`.
pub async fn fetch(req: Request) -> Result<Response> {
    if let Some(handler) = FETCH_OVERRIDE.with(|s| s.get()) {
        return handler(req).await;
    }
    fetch_with_client(&default_client(), req).await
}

/// [`fetch`] over an explicit client (a per-call undici `dispatcher`). Ignores `mock_fetch`.
pub async fn fetch_with_client(client: &reqwest::Client, req: Request) -> Result<Response> {
    let Request {
        method,
        url: input,
        mut headers,
        mut body,
        signal,
    } = req;

    // new Request(input, init)
    let mut url = Url::parse(&input).map_err(|_| {
        js_error(
            "TypeError",
            format!("Failed to parse URL from {input}"),
            None,
            Some(js_error("TypeError", "Invalid URL", Some("ERR_INVALID_URL"), None)),
        )
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(type_error(format!(
            "Request cannot be constructed from a URL that includes credentials: {input}"
        )));
    }
    let mut method = normalize_method(&method)?;
    for (name, value) in headers.raw() {
        validate_header("append", name, value)?;
    }
    if body.is_some() && (method == "GET" || method == "HEAD") {
        return Err(type_error("Request with GET/HEAD method cannot have body."));
    }
    if matches!(body, Some(Body::Text(_))) && !headers.has("content-type") {
        headers.append("content-type", "text/plain;charset=UTF-8");
    }

    // fetch()
    if let Some(signal) = &signal {
        signal.throw_if_aborted()?;
    }
    let mut redirects = 0;
    loop {
        if url.scheme() == "data" {
            return fetch_data_url(&url);
        }
        let res = send(client, &method, &url, &headers, body.as_ref(), signal.as_ref()).await?;
        let status = res.status().as_u16();
        if REDIRECT_STATUSES.contains(&status)
            && let Some(location) = redirect_location(res.headers(), &url)?
        {
            drop(res);
            if !matches!(location.scheme(), "http" | "https") {
                return Err(network_error(Error::msg("URL scheme must be a HTTP(S) scheme")));
            }
            if redirects == MAX_REDIRECTS {
                return Err(network_error(Error::msg("redirect count exceeded")));
            }
            redirects += 1;
            if (matches!(status, 301 | 302) && method == "POST")
                || (status == 303 && method != "GET" && method != "HEAD")
            {
                method = "GET".to_string();
                body = None;
                for name in REQUEST_BODY_HEADERS {
                    headers.delete(name);
                }
            }
            if url.origin() != location.origin() {
                for name in ["authorization", "proxy-authorization", "cookie", "host"] {
                    headers.delete(name);
                }
            }
            url = location;
            continue;
        }
        return Ok(network_response(res, url, redirects > 0, &method, signal));
    }
}

/// Method normalization and validation (undici `normalizedMethodRecords`).
fn normalize_method(method: &str) -> Result<String> {
    const NORMALIZED: [&str; 7] = ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT", "QUERY"];
    if method == "patch" || method == "PATCH" {
        return Ok(method.to_string());
    }
    if let Some(m) = NORMALIZED
        .iter()
        .find(|m| **m == method || m.to_ascii_lowercase() == method)
    {
        return Ok((*m).to_string());
    }
    if !is_token(method) {
        return Err(type_error(format!("'{method}' is not a valid HTTP method.")));
    }
    let upper = method.to_ascii_uppercase();
    if matches!(upper.as_str(), "CONNECT" | "TRACE" | "TRACK") {
        return Err(type_error(format!("'{method}' HTTP method is unsupported.")));
    }
    Ok(NORMALIZED
        .iter()
        .find(|m| **m == upper)
        .map_or_else(|| method.to_string(), |m| (*m).to_string()))
}

async fn send(
    client: &reqwest::Client,
    method: &str,
    url: &Url,
    headers: &Headers,
    body: Option<&Body>,
    signal: Option<&AbortSignal>,
) -> Result<reqwest::Response> {
    match url.scheme() {
        "http" | "https" => {}
        "about" => return Err(network_error(Error::msg("about scheme is not supported"))),
        // There is no object-URL registry (`URL.createObjectURL`), so every blob: URL is unknown.
        "blob" if url.query().is_some_and(|q| !q.is_empty()) => {
            return Err(network_error(Error::msg(
                "NetworkError when attempting to fetch resource.",
            )));
        }
        "blob" => return Err(network_error(Error::msg("invalid method"))),
        "file" => return Err(network_error(Error::msg("not implemented... yet..."))),
        _ => return Err(network_error(Error::msg("unknown scheme"))),
    }
    if url.port().is_some_and(|p| BAD_PORTS.contains(&p)) {
        return Err(network_error(Error::msg("bad port")));
    }
    let header_map = wire_headers(url, method, headers, body)?;
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| type_error(format!("'{method}' is not a valid HTTP method.")))?;
    let mut target = url.clone();
    target.set_fragment(None);
    let mut builder = client.request(method, target).headers(header_map);
    if let Some(body) = body {
        builder = builder.body(body.to_bytes());
    }
    let sent = builder.send();
    let result = match signal {
        Some(signal) => tokio::select! {
            biased;
            () = signal.cancelled() => return Err(Error::Abort(signal.reason().unwrap_or_else(AbortReason::abort))),
            r = sent => r,
        },
        None => sent.await,
    };
    result.map_err(|e| network_error(send_error_cause(&e, url)))
}

/// The header list undici writes: `connection`, the caller's headers (one line per name, values
/// combined), then the defaults fetch appends when absent.
fn wire_headers(url: &Url, method: &str, headers: &Headers, body: Option<&Body>) -> Result<HeaderMap> {
    let invalid = |name: &str| {
        network_error(js_error(
            "InvalidArgumentError",
            format!("invalid {name} header"),
            Some("UND_ERR_INVALID_ARG"),
            None,
        ))
    };
    let mut map = HeaderMap::new();
    // undici consumes a caller `connection` header and writes `keep-alive` or `close` itself.
    let close = headers
        .get("connection")
        .is_some_and(|v| v.to_ascii_lowercase().split(',').any(|t| t.trim() == "close"));
    map.insert(
        CONNECTION,
        HeaderValue::from_static(if close { "close" } else { "keep-alive" }),
    );
    for name in headers.distinct_names() {
        let lower = name.to_ascii_lowercase();
        let value = headers.get(name).unwrap_or_default();
        match lower.as_str() {
            "host" => continue,
            "transfer-encoding" | "keep-alive" | "upgrade" => return Err(invalid(&lower)),
            "expect" => {
                return Err(network_error(js_error(
                    "NotSupportedError",
                    "expect header not supported",
                    Some("UND_ERR_NOT_SUPPORTED"),
                    None,
                )));
            }
            "connection" if !value.to_ascii_lowercase().split(',').all(|t| is_token(t.trim())) => {
                return Err(invalid("connection"));
            }
            "connection" => continue,
            "content-length" if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) => {
                return Err(invalid("content-length"));
            }
            _ => {}
        }
        let header_name = HeaderName::from_bytes(lower.as_bytes()).map_err(|_| invalid(name))?;
        let header_value = HeaderValue::from_bytes(&isomorphic_encode(&value)).map_err(|_| invalid(name))?;
        map.insert(header_name, header_value);
    }
    let mut add_default = |name: &'static str, value: &str| {
        if !map.contains_key(name) {
            map.insert(
                HeaderName::from_static(name),
                HeaderValue::from_str(value).unwrap_or(HeaderValue::from_static("")),
            );
        }
    };
    if body.is_none() && (method == "POST" || method == "PUT") {
        add_default("content-length", "0");
    }
    add_default("accept", "*/*");
    add_default("accept-language", "*");
    add_default("sec-fetch-mode", "cors");
    let accept_encoding = match headers.get("accept-encoding") {
        Some(existing) if headers.has("range") => Some(format!("{existing}, identity")),
        Some(_) => None,
        None if headers.has("range") => Some("identity".to_string()),
        None if url.scheme() == "https" => Some("br, gzip, deflate, zstd".to_string()),
        None => Some("gzip, deflate".to_string()),
    };
    if let Some(v) = accept_encoding
        && let Ok(v) = HeaderValue::from_str(&v)
    {
        map.insert(reqwest::header::ACCEPT_ENCODING, v);
    }
    Ok(map)
}

/// `Location` resolved against the current URL (undici `responseLocationURL`).
fn redirect_location(headers: &HeaderMap, base: &Url) -> Result<Option<Url>> {
    let values: Vec<String> = headers
        .get_all(LOCATION)
        .iter()
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .collect();
    if values.is_empty() {
        return Ok(None);
    }
    let location = values.join(", ");
    let mut next = base
        .join(&location)
        .map_err(|_| network_error(js_error("TypeError", "Invalid URL", Some("ERR_INVALID_URL"), None)))?;
    if next.fragment().is_none() {
        next.set_fragment(base.fragment());
    }
    Ok(Some(next))
}

fn network_response(
    res: reqwest::Response,
    mut url: Url,
    redirected: bool,
    method: &str,
    signal: Option<AbortSignal>,
) -> Response {
    let status = res.status();
    let mut headers = Headers::new();
    for (name, value) in res.headers() {
        headers
            .list
            .push((name.as_str().to_string(), isomorphic_decode(value.as_bytes())));
    }
    url.set_fragment(None);
    let body = if method == "HEAD" || NULL_BODY_STATUSES.contains(&status.as_u16()) {
        BodyState::Null
    } else {
        let stream = res.bytes_stream().map(|chunk| chunk.map_err(body_error));
        BodyState::Unread(BodyStream::new(stream).with_signal(signal))
    };
    Response {
        status: status.as_u16(),
        // PORT: undici reports the server's reason phrase; hyper only exposes it through a hyper
        // type, so non-canonical phrases ("200 Fine Thanks") read as the canonical one.
        status_text: status.canonical_reason().unwrap_or("").to_string(),
        headers,
        url: url.to_string(),
        redirected,
        body: Mutex::new(body),
    }
}

/// `data:` URLs (WHATWG data: URL processor), as undici serves them.
fn fetch_data_url(url: &Url) -> Result<Response> {
    let mut serialized = url.clone();
    serialized.set_fragment(None);
    let input = &serialized.as_str()["data:".len()..];
    let Some((mime, encoded)) = input.split_once(',') else {
        return Err(network_error(Error::msg("failed to fetch the data URL")));
    };
    let mut mime = mime.trim_matches(|c: char| c.is_ascii_whitespace()).to_string();
    let mut body: Vec<u8> = percent_encoding::percent_decode(encoded.as_bytes()).collect();
    let lower = mime.to_ascii_lowercase();
    if let Some(prefix) = lower.strip_suffix("base64")
        && prefix.trim_end_matches(' ').ends_with(';')
    {
        body =
            forgiving_base64_decode(&body).ok_or_else(|| network_error(Error::msg("failed to fetch the data URL")))?;
        mime.truncate(prefix.trim_end_matches(' ').len() - 1);
    }
    if mime.starts_with(';') {
        mime.insert_str(0, "text/plain");
    }
    let content_type = parse_mime_type(&mime).unwrap_or_else(|| "text/plain;charset=US-ASCII".to_string());
    let mut headers = Headers::new();
    headers.append("content-type", &content_type);
    Ok(Response::new(200, headers, body)
        .with_status_text("OK")
        .with_url(serialized.to_string()))
}

fn forgiving_base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut data: Vec<u8> = input
        .iter()
        .copied()
        .filter(|b| !matches!(b, b'\t' | b'\n' | b'\x0c' | b'\r' | b' '))
        .collect();
    if data.len().is_multiple_of(4) {
        for _ in 0..2 {
            if data.last() == Some(&b'=') {
                data.pop();
            }
        }
    }
    if data.len() % 4 == 1
        || !data
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/')
    {
        return None;
    }
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::RequireNone)
            .with_decode_allow_trailing_bits(true),
    );
    engine.decode(&data).ok()
}

/// WHATWG "parse a MIME type" followed by "serialize a MIME type".
fn parse_mime_type(input: &str) -> Option<String> {
    let input = input.trim_matches(is_http_whitespace);
    let (type_, rest) = input.split_once('/')?;
    let (subtype, mut params) = match rest.find(';') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let subtype = subtype.trim_end_matches(is_http_whitespace);
    if !is_token(type_) || !is_token(subtype) {
        return None;
    }
    let mut out = format!("{}/{}", type_.to_ascii_lowercase(), subtype.to_ascii_lowercase());
    let mut seen: Vec<String> = Vec::new();
    while let Some(after_semicolon) = params.strip_prefix(';') {
        let p = after_semicolon.trim_start_matches(is_http_whitespace);
        let name_end = p.find([';', '=']).unwrap_or(p.len());
        let name = p[..name_end].to_ascii_lowercase();
        let mut rest = &p[name_end..];
        if rest.starts_with(';') {
            params = rest;
            continue;
        }
        if rest.is_empty() {
            break;
        }
        rest = &rest[1..];
        let value;
        if let Some(quoted) = rest.strip_prefix('"') {
            let mut v = String::new();
            let mut chars = quoted.char_indices();
            let mut consumed = quoted.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '"' => {
                        consumed = i + 1;
                        break;
                    }
                    '\\' => match chars.next() {
                        Some((_, escaped)) => v.push(escaped),
                        None => v.push('\\'),
                    },
                    c => v.push(c),
                }
            }
            let tail = &quoted[consumed..];
            params = &tail[tail.find(';').unwrap_or(tail.len())..];
            value = v;
        } else {
            let end = rest.find(';').unwrap_or(rest.len());
            value = rest[..end].trim_end_matches(is_http_whitespace).to_string();
            params = &rest[end..];
            if value.is_empty() {
                continue;
            }
        }
        let value_ok = value
            .chars()
            .all(|c| c == '\t' || (' '..='~').contains(&c) || ('\u{80}'..='\u{ff}').contains(&c));
        if is_token(&name) && value_ok && !seen.contains(&name) {
            out.push(';');
            out.push_str(&name);
            out.push('=');
            if value.is_empty() || !is_token(&value) {
                out.push('"');
                for c in value.chars() {
                    if c == '"' || c == '\\' {
                        out.push('\\');
                    }
                    out.push(c);
                }
                out.push('"');
            } else {
                out.push_str(&value);
            }
            seen.push(name);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

fn type_error(message: impl Into<String>) -> Error {
    Error::js("TypeError", message)
}

fn js_error(name: &str, message: impl Into<String>, code: Option<&str>, cause: Option<Error>) -> Error {
    Error::Js(JsError {
        name: name.to_string(),
        message: message.into(),
        code: code.map(str::to_string),
        cause: cause.map(Box::new),
    })
}

/// `TypeError: fetch failed` with `cause`.
fn network_error(cause: Error) -> Error {
    js_error("TypeError", "fetch failed", None, Some(cause))
}

/// `TypeError: terminated` (a body stream that failed after the response arrived).
fn body_error(e: reqwest::Error) -> Error {
    // reqwest reports body failures behind the decompression layer as decode errors, so look at
    // the underlying cause first.
    let cause = if is_closed_by_peer(&e) {
        socket_closed()
    } else if e.is_timeout() {
        js_error(
            "BodyTimeoutError",
            "Body Timeout Error",
            Some("UND_ERR_BODY_TIMEOUT"),
            None,
        )
    } else {
        Error::msg(innermost_message(&e))
    };
    js_error("TypeError", "terminated", None, Some(cause))
}

fn socket_closed() -> Error {
    js_error("SocketError", "other side closed", Some("UND_ERR_SOCKET"), None)
}

fn source_chain<'a>(e: &'a (dyn std::error::Error + 'static)) -> Vec<&'a (dyn std::error::Error + 'static)> {
    let mut out = Vec::new();
    let mut cur = Some(e);
    while let Some(err) = cur {
        out.push(err);
        // `io::Error::source` skips the wrapped error; step into it explicitly.
        cur = match err.downcast_ref::<std::io::Error>().and_then(|io| io.get_ref()) {
            Some(inner) => Some(inner as &(dyn std::error::Error + 'static)),
            None => err.source(),
        };
    }
    out
}

fn find_source<'a, T: std::error::Error + 'static>(e: &'a (dyn std::error::Error + 'static)) -> Option<&'a T> {
    source_chain(e).into_iter().find_map(|s| s.downcast_ref::<T>())
}

fn innermost_message(e: &(dyn std::error::Error + 'static)) -> String {
    source_chain(e).last().map(|s| s.to_string()).unwrap_or_default()
}

fn is_closed_by_peer(e: &(dyn std::error::Error + 'static)) -> bool {
    source_chain(e).iter().any(|s| {
        let text = s.to_string();
        text.contains("connection closed before message completed")
            || text.contains("end of file before message length reached")
            || s.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::UnexpectedEof)
    })
}

fn host_for_message(url: &Url) -> String {
    match url.host() {
        Some(Host::Ipv6(addr)) => addr.to_string(),
        Some(host) => host.to_string(),
        None => String::new(),
    }
}

/// The `cause` undici/Node attach to `fetch failed` for a request that got no response.
fn send_error_cause(e: &reqwest::Error, url: &Url) -> Error {
    let host = host_for_message(url);
    let port = url.port_or_known_default().unwrap_or(0);
    if let Some(tls) = find_source::<rustls::Error>(e) {
        return tls_error(tls, &host);
    }
    if e.is_timeout() {
        return if e.is_connect() {
            js_error(
                "ConnectTimeoutError",
                format!(
                    "Connect Timeout Error (attempted address: {host}:{port}, timeout: {}ms)",
                    CONNECT_TIMEOUT.as_millis()
                ),
                Some("UND_ERR_CONNECT_TIMEOUT"),
                None,
            )
        } else {
            js_error(
                "HeadersTimeoutError",
                "Headers Timeout Error",
                Some("UND_ERR_HEADERS_TIMEOUT"),
                None,
            )
        };
    }
    let chain = source_chain(e);
    if chain.iter().any(|s| {
        let text = s.to_string();
        text == "dns error" || text.starts_with("failed to lookup address information")
    }) {
        let detail = innermost_message(e);
        let (code, errno) = if detail.contains("Temporary failure") {
            ("EAI_AGAIN", -3001)
        } else {
            ("ENOTFOUND", -3008)
        };
        return Error::Node(NodeError {
            code: code.to_string(),
            errno,
            syscall: "getaddrinfo".to_string(),
            path: None,
            dest: None,
            message: format!("getaddrinfo {code} {host}"),
        });
    }
    if let Some(io) = find_source::<std::io::Error>(e) {
        let syscall = if e.is_connect() { "connect" } else { "read" };
        if let Some((code, errno)) = io.raw_os_error().and_then(errno_code) {
            // PORT: Node prints the resolved address it tried; for host names we print the name.
            let message = if syscall == "connect" {
                format!("connect {code} {host}:{port}")
            } else {
                format!("read {code}")
            };
            return Error::Node(NodeError {
                code: code.to_string(),
                errno,
                syscall: syscall.to_string(),
                path: None,
                dest: None,
                message,
            });
        }
    }
    if is_closed_by_peer(e) {
        return socket_closed();
    }
    Error::msg(innermost_message(e))
}

/// Node's (OpenSSL) codes for the common certificate failures.
fn tls_error(e: &rustls::Error, host: &str) -> Error {
    use rustls::CertificateError as C;
    let (code, message) = match e {
        rustls::Error::InvalidCertificate(cert) => match cert {
            C::Expired | C::ExpiredContext { .. } => ("CERT_HAS_EXPIRED", "certificate has expired".to_string()),
            C::NotValidYet | C::NotValidYetContext { .. } => {
                ("CERT_NOT_YET_VALID", "certificate is not yet valid".to_string())
            }
            C::Revoked => ("CERT_REVOKED", "certificate revoked".to_string()),
            C::UnknownIssuer => (
                "UNABLE_TO_GET_ISSUER_CERT_LOCALLY",
                "unable to get local issuer certificate".to_string(),
            ),
            C::NotValidForName | C::NotValidForNameContext { .. } => (
                "ERR_TLS_CERT_ALTNAME_INVALID",
                format!(
                    "Hostname/IP does not match certificate's altnames: Host: {host}. is not in the cert's altnames"
                ),
            ),
            other => ("", format!("{other:?}")),
        },
        other => ("", other.to_string()),
    };
    // PORT: rustls reports fewer distinct failures than OpenSSL; unmapped ones keep rustls' text.
    js_error("Error", message, (!code.is_empty()).then_some(code), None)
}

/// (Node `code`, libuv `errno`) for the socket errors fetch can surface.
#[cfg(unix)]
fn errno_code(raw: i32) -> Option<(&'static str, i32)> {
    let code = match raw {
        libc::ECONNREFUSED => "ECONNREFUSED",
        libc::ECONNRESET => "ECONNRESET",
        libc::ECONNABORTED => "ECONNABORTED",
        libc::ETIMEDOUT => "ETIMEDOUT",
        libc::EHOSTUNREACH => "EHOSTUNREACH",
        libc::EHOSTDOWN => "EHOSTDOWN",
        libc::ENETUNREACH => "ENETUNREACH",
        libc::ENETDOWN => "ENETDOWN",
        libc::EADDRNOTAVAIL => "EADDRNOTAVAIL",
        libc::EADDRINUSE => "EADDRINUSE",
        libc::EACCES => "EACCES",
        libc::EPERM => "EPERM",
        libc::EPIPE => "EPIPE",
        _ => return None,
    };
    Some((code, -raw))
}

/// (Node `code`, libuv `errno`) for the Winsock errors fetch can surface.
#[cfg(windows)]
fn errno_code(raw: i32) -> Option<(&'static str, i32)> {
    Some(match raw {
        10061 => ("ECONNREFUSED", -4078),
        10054 => ("ECONNRESET", -4077),
        10053 => ("ECONNABORTED", -4079),
        10060 => ("ETIMEDOUT", -4039),
        10065 => ("EHOSTUNREACH", -4065),
        10051 => ("ENETUNREACH", -4062),
        10050 => ("ENETDOWN", -4063),
        10049 => ("EADDRNOTAVAIL", -4090),
        10048 => ("EADDRINUSE", -4091),
        10013 => ("EACCES", -4092),
        _ => return None,
    })
}

#[cfg(not(any(unix, windows)))]
fn errno_code(_raw: i32) -> Option<(&'static str, i32)> {
    None
}

// ---------------------------------------------------------------------------------------------
// Test seams
// ---------------------------------------------------------------------------------------------

pub mod testing {
    use super::*;

    /// `vi.stubGlobal("fetch", handler)` for the current thread until the guard drops. The handler
    /// sees the request exactly as the caller built it (no undici defaults or validation).
    pub fn mock_fetch(h: impl Fn(Request) -> crate::BoxFuture<Result<Response>> + Send + Sync + 'static) -> Guard {
        FETCH_OVERRIDE.with(|s| s.set(Arc::new(h)))
    }

    /// Makes every [`fetch`] on this thread fail with `TypeError: fetch failed`.
    pub fn deny_network() -> Guard {
        mock_fetch(|req| {
            Box::pin(async move {
                Err(network_error(Error::msg(format!(
                    "network access denied by pi_js::fetch::testing::deny_network: {} {}",
                    req.method, req.url
                ))))
            })
        })
    }

    /// `new Response(body, { status, headers })`.
    pub fn response(status: u16, headers: &[(&str, &str)], body: impl Into<Bytes>) -> Response {
        Response::new(status, Headers::from_pairs(headers.iter().copied()), body)
    }

    /// A response whose body is delivered in `chunks` (for streaming / SSE tests).
    pub fn chunked_response<B: Into<Bytes>>(status: u16, headers: &[(&str, &str)], chunks: Vec<B>) -> Response {
        let chunks: Vec<Result<Bytes>> = chunks.into_iter().map(|c| Ok(c.into())).collect();
        Response::from_stream(
            status,
            Headers::from_pairs(headers.iter().copied()),
            stream::iter(chunks),
        )
    }
}
