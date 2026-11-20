//! Port of packages/ai/src/api/openai-codex-responses.ts

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, LazyLock, Once};

use base64::Engine as _;
use futures::{SinkExt, StreamExt};
use indexmap::{IndexMap, IndexSet};
use pi_js::abort::AbortSignal;
use pi_js::error::{Error, Result};
use pi_js::fetch::{Body, Headers, Request, Response, fetch as default_fetch};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use url::Url;

use crate::api::constrained_sampling::create_grammar_tool_input_properties;
use crate::api::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::api::openai_responses_shared::{
    ConvertResponsesMessagesOptions, ConvertResponsesToolsOptions, ProcessResponsesStreamOptions,
    ResponsesEventJsonStream, convert_responses_messages, convert_responses_tools, create_pending_assistant_message,
    get_responses_tool_support, process_responses_stream, response_to_sse_json_stream,
};
use crate::api::simple_options::build_base_options;
use crate::models::clamp_thinking_level;
use crate::session_resources::register_session_resource_cleanup;
use crate::types::{
    ApiStreamOptions, AssistantMessage, AssistantMessageEvent, CacheRetention, Model, ModelThinkingLevel,
    ProviderHeaders, ProviderResponse, ProviderStreams, SimpleStreamOptions, StopReason, StreamOptions, ThinkingLevel,
    TranscriptContext, Transport,
};
use crate::utils::abort_signals::combine_abort_signals;
use crate::utils::diagnostics::{
    append_assistant_message_diagnostic, create_assistant_message_diagnostic, format_thrown_value,
};
use crate::utils::error_body::{format_provider_error, normalize_provider_error};
use crate::utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use crate::utils::headers::headers_to_record;
use crate::utils::identity::{ClientIdentityTarget, get_identity_headers};
use crate::utils::text::get_system_message_text;
use crate::utils::transcript::{
    get_declared_tools, get_initial_system_message, resolve_transcript, resolve_transcript_tools_with_support,
};
use crate::utils::uuid::uuidv7;

const DEFAULT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";
const DEFAULT_MAX_RETRIES: u32 = 0;
const BASE_DELAY_MS: u64 = 1000;
const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;
const DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS: u64 = 15_000;
const REQUEST_COMPRESSION_ZSTD_LEVEL: i32 = 3;
const OPENAI_BETA_RESPONSES_WEBSOCKETS: &str = "responses_websockets=2026-02-06";
const SESSION_WEBSOCKET_CACHE_TTL_MS: u64 = 5 * 60 * 1000;
const SESSION_WEBSOCKET_MAX_AGE_MS: i64 = 55 * 60 * 1000;
const WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE: &str = "websocket_connection_limit_reached";
const PREVIOUS_RESPONSE_NOT_FOUND_CODE: &str = "previous_response_not_found";

/// Codex reasoning effort values (`none` through `max`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenAICodexReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl OpenAICodexReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "off" => Some(Self::None),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }

    pub fn to_model_thinking_level(self) -> ModelThinkingLevel {
        match self {
            Self::None => ModelThinkingLevel::Off,
            Self::Minimal => ModelThinkingLevel::Minimal,
            Self::Low => ModelThinkingLevel::Low,
            Self::Medium => ModelThinkingLevel::Medium,
            Self::High => ModelThinkingLevel::High,
            Self::XHigh => ModelThinkingLevel::XHigh,
            Self::Max => ModelThinkingLevel::Max,
        }
    }
}

/// Options for OpenAI Codex Responses API requests.
#[derive(Debug, Clone, Default)]
pub struct OpenAICodexResponsesOptions {
    pub base: StreamOptions,
    pub reasoning_effort: Option<OpenAICodexReasoningEffort>,
    pub reasoning_summary: Option<String>,
    pub service_tier: Option<String>,
    pub text_verbosity: Option<String>,
    pub tool_choice: Option<String>,
}

impl Deref for OpenAICodexResponsesOptions {
    type Target = StreamOptions;
    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for OpenAICodexResponsesOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl From<&OpenAICodexResponsesOptions> for ApiStreamOptions {
    fn from(options: &OpenAICodexResponsesOptions) -> Self {
        let mut extra: IndexMap<String, Value> = IndexMap::new();
        if let Some(effort) = options.reasoning_effort {
            extra.insert(
                "reasoningEffort".to_string(),
                Value::String(effort.as_str().to_string()),
            );
        }
        if let Some(summary) = &options.reasoning_summary {
            extra.insert("reasoningSummary".to_string(), Value::String(summary.clone()));
        }
        if let Some(tier) = &options.service_tier {
            extra.insert("serviceTier".to_string(), Value::String(tier.clone()));
        }
        if let Some(verbosity) = &options.text_verbosity {
            extra.insert("textVerbosity".to_string(), Value::String(verbosity.clone()));
        }
        if let Some(tc) = &options.tool_choice {
            extra.insert("toolChoice".to_string(), Value::String(tc.clone()));
        }
        let extension: Arc<dyn std::any::Any + Send + Sync> = Arc::new(options.clone());
        Self {
            base: options.base.clone(),
            extra,
            extension: Some(extension),
        }
    }
}

impl From<OpenAICodexResponsesOptions> for ApiStreamOptions {
    fn from(options: OpenAICodexResponsesOptions) -> Self {
        Self::from(&options)
    }
}

impl OpenAICodexResponsesOptions {
    fn from_api_options(options: Option<ApiStreamOptions>) -> Self {
        let Some(api_opts) = options else {
            return Self::default();
        };
        if let Some(ext) = &api_opts.extension {
            if let Some(typed) = ext.downcast_ref::<Self>() {
                return typed.clone();
            }
        }
        let get_str = |k: &str| api_opts.extra.get(k).and_then(Value::as_str).map(str::to_string);
        Self {
            reasoning_effort: get_str("reasoningEffort")
                .as_deref()
                .and_then(OpenAICodexReasoningEffort::from_str_opt),
            reasoning_summary: get_str("reasoningSummary"),
            service_tier: get_str("serviceTier"),
            text_verbosity: get_str("textVerbosity"),
            tool_choice: get_str("toolChoice"),
            base: api_opts.base,
        }
    }
}

/// Session WebSocket debug counters and continuation telemetry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAICodexWebSocketDebugStats {
    pub requests: usize,
    pub connections_created: usize,
    pub connections_reused: usize,
    pub cached_context_requests: usize,
    pub store_true_requests: usize,
    pub full_context_requests: usize,
    pub delta_requests: usize,
    pub last_input_items: usize,
    pub last_delta_input_items: Option<usize>,
    pub last_previous_response_id: Option<String>,
    pub websocket_failures: usize,
    pub sse_fallbacks: usize,
    pub websocket_fallback_active: Option<bool>,
    pub last_web_socket_error: Option<String>,
}

type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Debug, Clone)]
struct CachedWebSocketContinuationState {
    last_request_body: Value,
    last_response_id: String,
    last_response_items: Vec<Value>,
}

struct CachedWebSocketConnection {
    socket: Option<WsStream>,
    busy: bool,
    created_at: i64,
    idle_abort: Option<tokio::task::JoinHandle<()>>,
    continuation: Option<CachedWebSocketContinuationState>,
}

#[derive(Default)]
struct WebSocketRegistry {
    sessions: IndexMap<String, IndexMap<String, Arc<Mutex<CachedWebSocketConnection>>>>,
    stats: IndexMap<String, OpenAICodexWebSocketDebugStats>,
    fallback_sessions: IndexSet<String>,
}

static WS_REGISTRY: LazyLock<Mutex<WebSocketRegistry>> = LazyLock::new(|| Mutex::new(WebSocketRegistry::default()));

static CLEANUP_REGISTERED: Once = Once::new();

fn ensure_cleanup_registered() {
    CLEANUP_REGISTERED.call_once(|| {
        let _ = register_session_resource_cleanup(|session_id| {
            close_openai_codex_websocket_sessions(session_id);
        });
    });
}

/// Snapshot debug statistics for a Codex session ID.
pub fn get_openai_codex_websocket_debug_stats(session_id: &str) -> Option<OpenAICodexWebSocketDebugStats> {
    if let Ok(guard) = WS_REGISTRY.try_lock() {
        return guard.stats.get(session_id).cloned();
    }
    None
}

/// Reset debug counters and SSE fallback state for one session or all sessions.
pub fn reset_openai_codex_websocket_debug_stats(session_id: Option<&str>) {
    if let Ok(mut guard) = WS_REGISTRY.try_lock() {
        if let Some(sid) = session_id {
            guard.stats.shift_remove(sid);
            guard.fallback_sessions.shift_remove(sid);
        } else {
            guard.stats.clear();
            guard.fallback_sessions.clear();
        }
    }
}

/// Close cached Codex WebSocket sessions for one session or all sessions.
pub fn close_openai_codex_websocket_sessions(session_id: Option<&str>) {
    let entries_to_close = if let Ok(mut guard) = WS_REGISTRY.try_lock() {
        if let Some(sid) = session_id {
            guard
                .sessions
                .shift_remove(sid)
                .map(|m| m.into_values().collect::<Vec<_>>())
                .unwrap_or_default()
        } else {
            let mut all = Vec::new();
            for (_, map) in guard.sessions.drain(..) {
                all.extend(map.into_values());
            }
            all
        }
    } else {
        Vec::new()
    };

    for entry in entries_to_close {
        if let Ok(mut conn) = entry.try_lock() {
            if let Some(handle) = conn.idle_abort.take() {
                handle.abort();
            }
            if let Some(mut sock) = conn.socket.take() {
                if let Ok(rt) = tokio::runtime::Handle::try_current() {
                    rt.spawn(async move {
                        let _ = sock.close(None).await;
                    });
                }
            }
        }
    }
}

async fn is_websocket_sse_fallback_active(session_id: Option<&str>) -> bool {
    let Some(sid) = session_id else {
        return false;
    };
    WS_REGISTRY.lock().await.fallback_sessions.contains(sid)
}

async fn record_websocket_sse_fallback(session_id: Option<&str>) {
    let Some(sid) = session_id else {
        return;
    };
    let mut guard = WS_REGISTRY.lock().await;
    let active = guard.fallback_sessions.contains(sid);
    let stats = guard.stats.entry(sid.to_string()).or_default();
    stats.sse_fallbacks += 1;
    stats.websocket_fallback_active = Some(active);
}

async fn record_websocket_failure(session_id: Option<&str>, error_msg: &str) {
    let Some(sid) = session_id else {
        return;
    };
    let mut guard = WS_REGISTRY.lock().await;
    guard.fallback_sessions.insert(sid.to_string());
    let stats = guard.stats.entry(sid.to_string()).or_default();
    stats.websocket_failures += 1;
    stats.last_web_socket_error = Some(error_msg.to_string());
    stats.websocket_fallback_active = Some(true);
}

fn codex_tool_call_providers() -> IndexSet<String> {
    let mut set = IndexSet::new();
    set.insert("openai".to_string());
    set.insert("openai-codex".to_string());
    set.insert("opencode".to_string());
    set
}

fn resolve_codex_url(base_url: Option<&str>) -> String {
    let raw = base_url
        .map(pi_js::str16::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_CODEX_BASE_URL);
    let normalized = raw.trim_end_matches('/');
    if normalized.ends_with("/codex/responses") {
        normalized.to_string()
    } else if normalized.ends_with("/codex") {
        format!("{normalized}/responses")
    } else {
        format!("{normalized}/codex/responses")
    }
}

fn resolve_codex_websocket_url(base_url: Option<&str>) -> String {
    let http_url = resolve_codex_url(base_url);
    if let Ok(mut parsed) = Url::parse(&http_url) {
        if parsed.scheme() == "https" {
            let _ = parsed.set_scheme("wss");
        } else if parsed.scheme() == "http" {
            let _ = parsed.set_scheme("ws");
        }
        return parsed.to_string();
    }
    http_url
}

fn extract_account_id(token: &str) -> Result<String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(Error::msg("Failed to extract accountId from token"));
    }
    let payload_b64 = parts[1];
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(payload_b64))
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(payload_b64))
        .map_err(|_| Error::msg("Failed to extract accountId from token"))?;
    let payload_str =
        std::str::from_utf8(&decoded).map_err(|_| Error::msg("Failed to extract accountId from token"))?;
    let parsed = pi_js::json::parse(payload_str).map_err(|_| Error::msg("Failed to extract accountId from token"))?;
    let account_id = parsed
        .get(JWT_CLAIM_PATH)
        .and_then(|v| v.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::msg("Failed to extract accountId from token"))?;
    Ok(account_id.to_string())
}

fn apply_header_layer(headers: &mut Headers, layer: &ProviderHeaders) {
    for (k, v) in layer {
        match v {
            Some(val) => headers.set(k, val),
            None => headers.delete(k),
        }
    }
}

fn apply_raw_header_layer(headers: &mut Headers, layer: &IndexMap<String, String>) {
    for (k, v) in layer {
        headers.set(k, v);
    }
}

fn build_base_codex_headers(
    default_headers: &IndexMap<String, String>,
    model_headers: Option<&IndexMap<String, String>>,
    additional_headers: Option<&ProviderHeaders>,
    account_id: &str,
    token: &str,
) -> Headers {
    // Defaults first so model and caller headers can override them, matching the other providers.
    let mut headers = Headers::new();
    apply_raw_header_layer(&mut headers, default_headers);
    if let Some(mh) = model_headers {
        apply_raw_header_layer(&mut headers, mh);
    }
    if let Some(oh) = additional_headers {
        apply_header_layer(&mut headers, oh);
    }
    headers.set("Authorization", &format!("Bearer {token}"));
    headers.set("chatgpt-account-id", account_id);
    headers
}

fn build_sse_headers(
    default_headers: &IndexMap<String, String>,
    model_headers: Option<&IndexMap<String, String>>,
    additional_headers: Option<&ProviderHeaders>,
    account_id: &str,
    token: &str,
    session_id: Option<&str>,
) -> Headers {
    let mut headers = build_base_codex_headers(default_headers, model_headers, additional_headers, account_id, token);
    headers.set("OpenAI-Beta", "responses=experimental");
    headers.set("accept", "text/event-stream");
    headers.set("content-type", "application/json");
    if let Some(sid) = session_id {
        if !sid.is_empty() {
            headers.set("session-id", sid);
            headers.set("x-client-request-id", sid);
        }
    }
    headers
}

fn build_websocket_headers(
    default_headers: &IndexMap<String, String>,
    model_headers: Option<&IndexMap<String, String>>,
    additional_headers: Option<&ProviderHeaders>,
    account_id: &str,
    token: &str,
    request_id: &str,
) -> Headers {
    let mut headers = build_base_codex_headers(default_headers, model_headers, additional_headers, account_id, token);
    headers.delete("accept");
    headers.delete("content-type");
    headers.delete("OpenAI-Beta");
    headers.delete("openai-beta");
    headers.set("OpenAI-Beta", OPENAI_BETA_RESPONSES_WEBSOCKETS);
    headers.set("x-client-request-id", request_id);
    headers.set("session-id", request_id);
    headers
}

fn resolve_codex_effort(model: &Model, effort: OpenAICodexReasoningEffort) -> Option<String> {
    if let Some(map) = &model.thinking_level_map {
        if let Some(entry) = map.get(&effort.to_model_thinking_level()) {
            return entry.clone();
        }
    }
    Some(effort.as_str().to_string())
}

fn build_request_body(
    model: &Model,
    context: &TranscriptContext,
    options: &OpenAICodexResponsesOptions,
    cache_session_id: Option<&str>,
    grammar_props: &IndexMap<String, String>,
) -> Result<Value> {
    let compat = model.compat.as_ref().and_then(|c| c.as_openai_responses());
    let supports_strict_mode = compat.and_then(|c| c.supports_strict_mode).unwrap_or(true);
    let supports_grammar_tools = compat.and_then(|c| c.supports_open_ai_grammar_tools).unwrap_or(false);
    let supports_mid_convo = compat.and_then(|c| c.supports_mid_convo_system_messages);
    let supports_additional_tools = compat.and_then(|c| c.supports_additional_tools).unwrap_or(false);
    let supports_tool_search = compat.and_then(|c| c.supports_tool_search).unwrap_or(false);
    let tool_support = get_responses_tool_support(
        supports_mid_convo == Some(true),
        supports_additional_tools,
        supports_tool_search,
    );
    let transcript_tools = resolve_transcript_tools_with_support(&context.messages, tool_support);

    let tool_opts = ConvertResponsesToolsOptions {
        strict: Some(supports_strict_mode),
        supports_openai_grammar_tools: Some(supports_grammar_tools),
        ..Default::default()
    };
    let msg_opts = ConvertResponsesMessagesOptions {
        include_system_prompt: Some(false),
        grammar_tool_input_properties: if grammar_props.is_empty() {
            None
        } else {
            Some(grammar_props.clone())
        },
        supports_mid_convo_system_messages: supports_mid_convo,
        supports_additional_tools,
        supports_tool_search,
        tool_options: Some(tool_opts.clone()),
        configuration_update_effort: None,
    };
    let providers = codex_tool_call_providers();
    let messages = convert_responses_messages(model, context, &providers, Some(&msg_opts));

    let initial_sys = get_initial_system_message(&context.messages);
    let instructions = initial_sys
        .map(get_system_message_text)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "You are a helpful assistant.".to_string());

    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.id.clone()));
    body.insert("store".to_string(), Value::Bool(false));
    body.insert("stream".to_string(), Value::Bool(true));
    body.insert("instructions".to_string(), Value::String(instructions));
    body.insert("input".to_string(), Value::Array(messages));

    let mut text_obj = Map::new();
    text_obj.insert(
        "verbosity".to_string(),
        Value::String(
            options
                .text_verbosity
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("low")
                .to_string(),
        ),
    );
    body.insert("text".to_string(), Value::Object(text_obj));
    body.insert(
        "include".to_string(),
        Value::Array(vec![Value::String("reasoning.encrypted_content".to_string())]),
    );

    if let Some(csid) = cache_session_id {
        if !csid.is_empty() {
            body.insert("prompt_cache_key".to_string(), Value::String(csid.to_string()));
        }
    }
    body.insert(
        "tool_choice".to_string(),
        Value::String(options.tool_choice.as_deref().unwrap_or("auto").to_string()),
    );
    body.insert("parallel_tool_calls".to_string(), Value::Bool(true));

    if let Some(temp) = options.base.temperature {
        if let Some(num) = serde_json::Number::from_f64(temp) {
            body.insert("temperature".to_string(), Value::Number(num));
        }
    }
    if let Some(tier) = &options.service_tier {
        body.insert("service_tier".to_string(), Value::String(tier.clone()));
    }

    if !transcript_tools.request_tools.is_empty() {
        let mut converted = convert_responses_tools(&transcript_tools.request_tools, Some(&tool_opts))?;
        if supports_strict_mode {
            // Codex expects `strict: null` rather than `false` when strict mode is enabled on the
            // endpoint but a tool does not opt in.
            for tool_val in &mut converted {
                if let Some(obj) = tool_val.as_object_mut() {
                    if obj.get("type").and_then(Value::as_str) == Some("function")
                        && obj.get("strict").and_then(Value::as_bool) == Some(false)
                    {
                        obj.insert("strict".to_string(), Value::Null);
                    }
                }
            }
        }
        body.insert("tools".to_string(), Value::Array(converted));
    }

    if let Some(req_effort) = options.reasoning_effort {
        if let Some(resolved_effort) = resolve_codex_effort(model, req_effort) {
            let mut r_obj = Map::new();
            r_obj.insert("effort".to_string(), Value::String(resolved_effort));
            r_obj.insert(
                "summary".to_string(),
                Value::String(options.reasoning_summary.as_deref().unwrap_or("auto").to_string()),
            );
            body.insert("reasoning".to_string(), Value::Object(r_obj));
        }
    } else if model.reasoning {
        let off_entry = model
            .thinking_level_map
            .as_ref()
            .and_then(|m| m.get(&ModelThinkingLevel::Off));
        match off_entry {
            Some(None) => {}
            Some(Some(val)) => {
                let mut r_obj = Map::new();
                r_obj.insert("effort".to_string(), Value::String(val.clone()));
                body.insert("reasoning".to_string(), Value::Object(r_obj));
            }
            None => {
                let mut r_obj = Map::new();
                r_obj.insert("effort".to_string(), Value::String("none".to_string()));
                body.insert("reasoning".to_string(), Value::Object(r_obj));
            }
        }
    }

    Ok(Value::Object(body))
}

fn is_terminal_rate_limit_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("gousagelimiterror")
        || lower.contains("freeusagelimiterror")
        || lower.contains("monthly usage limit reached")
        || lower.contains("available balance")
        || lower.contains("insufficient_quota")
        || lower.contains("out of budget")
        || lower.contains("quota exceeded")
        || lower.contains("billing")
}

fn is_retryable_status(status: u16, body: &str) -> bool {
    if status == 429 && is_terminal_rate_limit_error(body) {
        return false;
    }
    if matches!(status, 429 | 500 | 502 | 503 | 504) {
        return true;
    }
    let lower = body.to_ascii_lowercase();
    lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("overloaded")
        || lower.contains("service unavailable")
        || lower.contains("service_unavailable")
        || lower.contains("upstream connect")
        || lower.contains("upstream_connect")
        || lower.contains("connection refused")
        || lower.contains("connection_refused")
}

fn get_retry_after_delay_ms(headers: &Headers) -> Option<u64> {
    if let Some(ms_str) = headers.get("retry-after-ms") {
        if let Ok(ms) = ms_str.trim().parse::<f64>() {
            if ms.is_finite() && ms >= 0.0 {
                return Some(ms.round() as u64);
            }
        }
    }
    if let Some(ra_str) = headers.get("retry-after") {
        let trimmed = ra_str.trim();
        if let Ok(secs) = trimmed.parse::<f64>() {
            if secs.is_finite() && secs >= 0.0 {
                return Some((secs * 1000.0).round() as u64);
            }
        } else if let Ok(sys_time) = httpdate::parse_http_date(trimmed) {
            if let Ok(dur) = sys_time.duration_since(std::time::UNIX_EPOCH) {
                let target_ms = dur.as_millis() as i64;
                let diff = (target_ms - pi_js::time::now_ms()).max(0);
                return Some(diff as u64);
            }
        } else if let Some(target_ms) = pi_js::time::parse_date(trimmed) {
            let diff = (target_ms - pi_js::time::now_ms()).max(0);
            return Some(diff as u64);
        }
    }
    None
}

fn parse_error_response(status: u16, status_text: &str, raw: &str) -> String {
    let mut message = if !raw.is_empty() {
        raw.to_string()
    } else if !status_text.is_empty() {
        status_text.to_string()
    } else {
        "Request failed".to_string()
    };

    if let Ok(parsed) = pi_js::json::parse(raw) {
        if let Some(err) = parsed.get("error") {
            let code = err
                .get("code")
                .and_then(Value::as_str)
                .or_else(|| err.get("type").and_then(Value::as_str))
                .unwrap_or("");
            let usage_code = code == "usage_limit_reached"
                || code == "usage_not_included"
                || code == "rate_limit_exceeded"
                || status == 429;
            if usage_code {
                let plan = err
                    .get("plan_type")
                    .and_then(Value::as_str)
                    .map(|p| format!(" ({} plan)", p.to_ascii_lowercase()))
                    .unwrap_or_default();
                let when = err
                    .get("resets_at")
                    .and_then(Value::as_i64)
                    .map(|ts_secs| {
                        let mins = ((ts_secs * 1000 - pi_js::time::now_ms()) as f64 / 60000.0)
                            .round()
                            .max(0.0) as i64;
                        format!(" Try again in ~{mins} min.")
                    })
                    .unwrap_or_default();
                let friendly = format!("You have hit your ChatGPT usage limit{plan}.{when}");
                return friendly.trim().to_string();
            }
            if let Some(msg) = err.get("message").and_then(Value::as_str) {
                if !msg.is_empty() {
                    message = msg.to_string();
                }
            }
        }
    }

    message
}

fn compress_zstd(body_json: &str) -> Option<Vec<u8>> {
    zstd::bulk::compress(body_json.as_bytes(), REQUEST_COMPRESSION_ZSTD_LEVEL).ok()
}

fn request_body_without_input(body: &Value) -> Value {
    let mut obj = body.as_object().cloned().unwrap_or_default();
    obj.shift_remove("input");
    obj.shift_remove("previous_response_id");
    Value::Object(obj)
}

fn build_cached_ws_request_body(conn: &mut CachedWebSocketConnection, full_body: &Value) -> Value {
    let Some(cont) = conn.continuation.as_ref() else {
        return full_body.clone();
    };
    if pi_js::json::stringify(&request_body_without_input(full_body))
        != pi_js::json::stringify(&request_body_without_input(&cont.last_request_body))
    {
        conn.continuation = None;
        return full_body.clone();
    }
    let current_input = full_body
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let last_input = cont
        .last_request_body
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut baseline = last_input;
    baseline.extend(cont.last_response_items.iter().cloned());
    if current_input.len() < baseline.len() {
        conn.continuation = None;
        return full_body.clone();
    }
    for (a, b) in current_input.iter().zip(baseline.iter()) {
        if pi_js::json::stringify(a) != pi_js::json::stringify(b) {
            conn.continuation = None;
            return full_body.clone();
        }
    }
    let delta: Vec<Value> = current_input[baseline.len()..].to_vec();
    let mut next_obj = full_body.as_object().cloned().unwrap_or_default();
    next_obj.insert(
        "previous_response_id".to_string(),
        Value::String(cont.last_response_id.clone()),
    );
    next_obj.insert("input".to_string(), Value::Array(delta));
    Value::Object(next_obj)
}

async fn connect_websocket(
    ws_url: &str,
    headers: &Headers,
    signal: Option<&AbortSignal>,
    connect_timeout_ms: u64,
) -> Result<WsStream> {
    if signal.is_some_and(AbortSignal::aborted) {
        return Err(Error::msg("Request was aborted"));
    }
    let mut req = ws_url
        .into_client_request()
        .map_err(|e| Error::msg(format!("Invalid Codex WebSocket URL \"{ws_url}\": {e}")))?;

    for (k, v) in headers_to_record(headers) {
        if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(&v)) {
            req.headers_mut().insert(name, val);
        }
    }

    let timeout_dur = std::time::Duration::from_millis(connect_timeout_ms);
    let connect_fut = tokio::time::timeout(timeout_dur, connect_async(req));

    let res = match signal {
        Some(sig) => {
            let mut abort_fut = Box::pin(sig.cancelled());
            tokio::select! {
                _ = &mut abort_fut => {
                    return Err(Error::msg("Request was aborted"));
                }
                r = connect_fut => r,
            }
        }
        None => connect_fut.await,
    };

    let (ws_stream, _) = res
        .map_err(|_| Error::msg(format!("WebSocket connect timeout after {connect_timeout_ms}ms")))?
        .map_err(|e| Error::msg(format!("WebSocket error: {e}")))?;
    Ok(ws_stream)
}

#[derive(Debug)]
struct CodexErrorMeta {
    code: Option<String>,
    non_transport: bool,
    message: String,
}

async fn try_websocket_turn(
    model: &Model,
    context_template: &TranscriptContext,
    full_body: &Value,
    ws_headers: &Headers,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    start_emitted: &mut bool,
    cache_session_id: Option<&str>,
    account_id: &str,
    grammar_props: &IndexMap<String, String>,
    options: &OpenAICodexResponsesOptions,
) -> std::result::Result<(), CodexErrorMeta> {
    let ws_url = resolve_codex_websocket_url(Some(&model.base_url));
    let connect_timeout_ms = options
        .base
        .websocket_connect_timeout_ms
        .unwrap_or(DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS);

    let (cached_entry, reused, mut raw_socket) = if let Some(csid) = cache_session_id {
        let existing = {
            let guard = WS_REGISTRY.lock().await;
            guard.sessions.get(csid).and_then(|m| m.get(account_id)).cloned()
        };
        if let Some(entry_arc) = existing {
            let mut entry = entry_arc.lock().await;
            if let Some(h) = entry.idle_abort.take() {
                h.abort();
            }
            let expired = pi_js::time::now_ms() - entry.created_at >= SESSION_WEBSOCKET_MAX_AGE_MS;
            if !entry.busy && !expired && entry.socket.is_some() {
                entry.busy = true;
                (Some(entry_arc.clone()), true, None)
            } else if entry.busy {
                let s = connect_websocket(&ws_url, ws_headers, options.base.signal.as_ref(), connect_timeout_ms)
                    .await
                    .map_err(|e| CodexErrorMeta {
                        code: None,
                        non_transport: false,
                        message: e.to_string(),
                    })?;
                (None, false, Some(s))
            } else {
                entry.socket.take();
                let s = connect_websocket(&ws_url, ws_headers, options.base.signal.as_ref(), connect_timeout_ms)
                    .await
                    .map_err(|e| CodexErrorMeta {
                        code: None,
                        non_transport: false,
                        message: e.to_string(),
                    })?;
                entry.socket = Some(s);
                entry.created_at = pi_js::time::now_ms();
                entry.busy = true;
                entry.continuation = None;
                (Some(entry_arc.clone()), false, None)
            }
        } else {
            let s = connect_websocket(&ws_url, ws_headers, options.base.signal.as_ref(), connect_timeout_ms)
                .await
                .map_err(|e| CodexErrorMeta {
                    code: None,
                    non_transport: false,
                    message: e.to_string(),
                })?;
            let new_entry = Arc::new(Mutex::new(CachedWebSocketConnection {
                socket: Some(s),
                busy: true,
                created_at: pi_js::time::now_ms(),
                idle_abort: None,
                continuation: None,
            }));
            let mut guard = WS_REGISTRY.lock().await;
            guard
                .sessions
                .entry(csid.to_string())
                .or_default()
                .insert(account_id.to_string(), new_entry.clone());
            (Some(new_entry), false, None)
        }
    } else {
        let s = connect_websocket(&ws_url, ws_headers, options.base.signal.as_ref(), connect_timeout_ms)
            .await
            .map_err(|e| CodexErrorMeta {
                code: None,
                non_transport: false,
                message: e.to_string(),
            })?;
        (None, false, Some(s))
    };

    let use_cached_context = matches!(
        options.base.transport,
        None | Some(Transport::Auto) | Some(Transport::WebsocketCached)
    );

    let mut request_body = if let (true, Some(entry_arc)) = (use_cached_context, &cached_entry) {
        let mut entry = entry_arc.lock().await;
        build_cached_ws_request_body(&mut entry, full_body)
    } else {
        full_body.clone()
    };

    if let Some(csid) = cache_session_id {
        let mut guard = WS_REGISTRY.lock().await;
        let stats = guard.stats.entry(csid.to_string()).or_default();
        stats.requests += 1;
        if reused {
            stats.connections_reused += 1;
        } else {
            stats.connections_created += 1;
        }
        if use_cached_context {
            stats.cached_context_requests += 1;
        }
        if request_body.get("store").and_then(Value::as_bool) == Some(true) {
            stats.store_true_requests += 1;
        }
        let input_len = request_body.get("input").and_then(Value::as_array).map_or(0, Vec::len);
        stats.last_input_items = input_len;
        if let Some(prev_id) = request_body.get("previous_response_id").and_then(Value::as_str) {
            stats.delta_requests += 1;
            stats.last_delta_input_items = Some(input_len);
            stats.last_previous_response_id = Some(prev_id.to_string());
        } else {
            stats.full_context_requests += 1;
            stats.last_delta_input_items = None;
            stats.last_previous_response_id = None;
        }
    }

    let mut frame_obj = request_body.as_object().cloned().unwrap_or_default();
    frame_obj.insert("type".to_string(), Value::String("response.create".to_string()));
    request_body = Value::Object(frame_obj);
    let frame_json = pi_js::json::stringify(&request_body);

    let mut ws = if let Some(entry_arc) = &cached_entry {
        entry_arc.lock().await.socket.take().ok_or_else(|| CodexErrorMeta {
            code: None,
            non_transport: false,
            message: "Missing cached WebSocket".to_string(),
        })?
    } else {
        raw_socket.take().ok_or_else(|| CodexErrorMeta {
            code: None,
            non_transport: false,
            message: "Missing WebSocket".to_string(),
        })?
    };

    if let Err(err) = ws.send(WsMessage::Text(frame_json.into())).await {
        if let Some(entry_arc) = &cached_entry {
            let mut entry = entry_arc.lock().await;
            entry.busy = false;
            entry.continuation = None;
        }
        return Err(CodexErrorMeta {
            code: None,
            non_transport: false,
            message: format!("WebSocket send error: {err}"),
        });
    }

    let mut events_queue: Vec<Value> = Vec::new();
    let idle_timeout_ms = options.base.timeout_ms;
    let on_event = options.base.on_provider_stream_event.clone();

    let mut loop_err: Option<CodexErrorMeta> = None;
    let mut saw_terminal = false;

    loop {
        if options.base.signal.as_ref().is_some_and(AbortSignal::aborted) {
            loop_err = Some(CodexErrorMeta {
                code: None,
                non_transport: false,
                message: "Request was aborted".to_string(),
            });
            break;
        }

        let next_msg = if let Some(timeout_ms) = idle_timeout_ms {
            let dur = std::time::Duration::from_millis(timeout_ms);
            match tokio::time::timeout(dur, ws.next()).await {
                Ok(msg) => msg,
                Err(_) => {
                    loop_err = Some(CodexErrorMeta {
                        code: None,
                        non_transport: false,
                        message: format!("WebSocket idle timeout after {timeout_ms}ms"),
                    });
                    break;
                }
            }
        } else {
            ws.next().await
        };

        let Some(msg_res) = next_msg else {
            break;
        };

        let msg = match msg_res {
            Ok(m) => m,
            Err(e) => {
                loop_err = Some(CodexErrorMeta {
                    code: None,
                    non_transport: false,
                    message: format!("WebSocket error: {e}"),
                });
                break;
            }
        };

        let text = match msg {
            WsMessage::Text(t) => t.to_string(),
            WsMessage::Binary(b) => match String::from_utf8(b.to_vec()) {
                Ok(s) => s,
                Err(e) => {
                    loop_err = Some(CodexErrorMeta {
                        code: None,
                        non_transport: true,
                        message: format!("Invalid Codex WebSocket UTF-8: {e}"),
                    });
                    break;
                }
            },
            WsMessage::Close(frame) => {
                if saw_terminal {
                    break;
                }
                let msg_txt = if let Some(cf) = frame {
                    format!("WebSocket closed {} {}", u16::from(cf.code), cf.reason)
                } else {
                    "WebSocket closed".to_string()
                };
                loop_err = Some(CodexErrorMeta {
                    code: None,
                    non_transport: false,
                    message: msg_txt.trim().to_string(),
                });
                break;
            }
            WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_) => continue,
        };

        let parsed = match pi_js::json::parse(&text) {
            Ok(v) => v,
            Err(e) => {
                loop_err = Some(CodexErrorMeta {
                    code: None,
                    non_transport: true,
                    message: format!("Invalid Codex WebSocket JSON: {e}"),
                });
                break;
            }
        };

        if let Some(cb) = &on_event {
            if let Err(cb_err) = cb(&parsed, model).await {
                loop_err = Some(CodexErrorMeta {
                    code: None,
                    non_transport: true,
                    message: format_thrown_value(&cb_err),
                });
                break;
            }
        }

        let evt_type = parsed.get("type").and_then(Value::as_str).unwrap_or("");
        match evt_type {
            "error" => {
                let code = parsed
                    .get("code")
                    .and_then(Value::as_str)
                    .or_else(|| parsed.get("error").and_then(|e| e.get("code")).and_then(Value::as_str))
                    .map(str::to_string);
                let msg = parsed
                    .get("message")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        parsed
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(Value::as_str)
                    })
                    .unwrap_or("")
                    .to_string();
                let formatted = if !msg.is_empty() {
                    format!("Codex error: {msg}")
                } else if let Some(ref c) = code {
                    format!("Codex error: {c}")
                } else {
                    format!("Codex error: {}", pi_js::json::stringify(&parsed))
                };
                loop_err = Some(CodexErrorMeta {
                    code,
                    non_transport: true,
                    message: formatted,
                });
                break;
            }
            "response.failed" => {
                let err_val = parsed.get("response").and_then(|r| r.get("error"));
                let code = err_val
                    .and_then(|e| e.get("code"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let msg = err_val
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Codex response failed")
                    .to_string();
                loop_err = Some(CodexErrorMeta {
                    code,
                    non_transport: true,
                    message: msg,
                });
                break;
            }
            "response.completed" | "response.done" | "response.incomplete" => {
                if !*start_emitted {
                    *start_emitted = true;
                    stream.push(AssistantMessageEvent::Start {
                        partial: output.clone(),
                    });
                }
                saw_terminal = true;
                events_queue.push(parsed);
                break;
            }
            _ => {
                if !*start_emitted {
                    *start_emitted = true;
                    stream.push(AssistantMessageEvent::Start {
                        partial: output.clone(),
                    });
                }
                events_queue.push(parsed);
            }
        }
    }

    if loop_err.is_none() && !saw_terminal {
        loop_err = Some(CodexErrorMeta {
            code: None,
            non_transport: false,
            message: "WebSocket stream closed before response.completed".to_string(),
        });
    }

    if let Some(err_meta) = loop_err {
        let _ = ws.close(None).await;
        if let Some(entry_arc) = &cached_entry {
            let mut entry = entry_arc.lock().await;
            entry.busy = false;
            entry.continuation = None;
            entry.socket = None;
        }
        return Err(err_meta);
    }

    let event_stream: ResponsesEventJsonStream = Box::pin(futures::stream::iter(events_queue.into_iter().map(Ok)));
    let process_opts = ProcessResponsesStreamOptions {
        service_tier: options.service_tier.clone(),
        resolve_service_tier_on_default: true,
        grammar_tool_input_properties: if grammar_props.is_empty() {
            None
        } else {
            Some(grammar_props.clone())
        },
    };
    if let Err(err) = process_responses_stream(event_stream, output, stream, model, Some(&process_opts)).await {
        let _ = ws.close(None).await;
        if let Some(entry_arc) = &cached_entry {
            let mut entry = entry_arc.lock().await;
            entry.busy = false;
            entry.continuation = None;
            entry.socket = None;
        }
        return Err(CodexErrorMeta {
            code: None,
            non_transport: true,
            message: err.to_string(),
        });
    }

    let keep = !options.base.signal.as_ref().is_some_and(AbortSignal::aborted);
    if let Some(entry_arc) = &cached_entry {
        let mut entry = entry_arc.lock().await;
        if !keep {
            let _ = ws.close(None).await;
            entry.socket = None;
            entry.busy = false;
            entry.continuation = None;
        } else {
            if use_cached_context {
                if let Some(resp_id) = output.response_id.clone() {
                    let mut msg_ctx = context_template.clone();
                    msg_ctx.messages = vec![crate::types::Message::Assistant(output.clone())];
                    let providers = codex_tool_call_providers();
                    let msg_opts = ConvertResponsesMessagesOptions {
                        include_system_prompt: Some(false),
                        grammar_tool_input_properties: if grammar_props.is_empty() {
                            None
                        } else {
                            Some(grammar_props.clone())
                        },
                        ..Default::default()
                    };
                    let resp_items: Vec<Value> =
                        convert_responses_messages(model, &msg_ctx, &providers, Some(&msg_opts))
                            .into_iter()
                            .filter(|item| {
                                let t = item.get("type").and_then(Value::as_str);
                                t != Some("function_call_output") && t != Some("custom_tool_call_output")
                            })
                            .collect();
                    entry.continuation = Some(CachedWebSocketContinuationState {
                        last_request_body: full_body.clone(),
                        last_response_id: resp_id,
                        last_response_items: resp_items,
                    });
                }
            }
            entry.socket = Some(ws);
            entry.busy = false;
            if let Some(csid) = cache_session_id {
                let csid_owned = csid.to_string();
                let acct_owned = account_id.to_string();
                let weak_entry = Arc::downgrade(entry_arc);
                entry.idle_abort = Some(tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(SESSION_WEBSOCKET_CACHE_TTL_MS)).await;
                    if let Some(arc) = weak_entry.upgrade() {
                        let mut guard = arc.lock().await;
                        if guard.busy {
                            return;
                        }
                        if let Some(mut sock) = guard.socket.take() {
                            let _ = sock.close(None).await;
                        }
                        drop(guard);
                        let mut reg = WS_REGISTRY.lock().await;
                        if let Some(acct_map) = reg.sessions.get_mut(&csid_owned) {
                            acct_map.shift_remove(&acct_owned);
                            if acct_map.is_empty() {
                                reg.sessions.shift_remove(&csid_owned);
                            }
                        }
                    }
                }));
            }
        }
    } else {
        let _ = ws.close(None).await;
    }

    Ok(())
}

async fn execute_sse_request(
    model: &Model,
    sse_headers: &Headers,
    body_json: &str,
    options: &OpenAICodexResponsesOptions,
) -> Result<Response> {
    let mut final_headers = sse_headers.clone();
    let request_body = if let Some(compressed) = compress_zstd(body_json) {
        final_headers.set("content-encoding", "zstd");
        Body::Bytes(compressed.into())
    } else {
        Body::Text(body_json.to_string())
    };

    let url = resolve_codex_url(Some(&model.base_url));
    let max_retries = options.base.max_retries.unwrap_or(DEFAULT_MAX_RETRIES);
    let max_retry_delay_ms = options.base.max_retry_delay_ms.unwrap_or(DEFAULT_MAX_RETRY_DELAY_MS);
    let http_timeout_ms = options.base.timeout_ms;

    let mut last_err: Option<Error> = None;
    for attempt in 0..=max_retries {
        if options.base.signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Err(Error::msg("Request was aborted"));
        }

        let header_timeout_sig = http_timeout_ms.filter(|&m| m > 0).map(AbortSignal::timeout);
        let combined = combine_abort_signals([options.base.signal.as_ref(), header_timeout_sig.as_ref()]);

        let mut req = Request::post(&url)
            .with_headers(final_headers.clone())
            .body(request_body.clone());
        if let Some(sig) = &combined.signal {
            req = req.signal(sig.clone());
        }

        let fetch_res = match &options.base.fetch {
            Some(f) => f(req).await,
            None => default_fetch(req).await,
        };

        let resp = match fetch_res {
            Ok(r) => r,
            Err(err) => {
                if options.base.signal.as_ref().is_some_and(AbortSignal::aborted) {
                    return Err(Error::msg("Request was aborted"));
                }
                if header_timeout_sig.as_ref().is_some_and(AbortSignal::aborted) {
                    return Err(Error::msg(format!(
                        "Codex SSE response headers timed out after {}ms",
                        http_timeout_ms.unwrap_or(0)
                    )));
                }
                let msg = err.to_string();
                last_err = Some(Error::msg(msg.clone()));
                if attempt < max_retries && !msg.contains("usage limit") {
                    let delay_ms = BASE_DELAY_MS.saturating_mul(2u64.saturating_pow(attempt));
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    continue;
                }
                return Err(Error::msg(msg));
            }
        };

        if let Some(on_resp) = &options.base.on_response {
            let meta = ProviderResponse {
                status: resp.status(),
                headers: headers_to_record(resp.headers()),
            };
            on_resp(meta, model).await?;
        }

        if resp.ok() {
            return Ok(resp);
        }

        let status = resp.status();
        let status_text = resp.status_text().to_string();
        let resp_headers = resp.headers().clone();
        let err_text = resp.text().await.unwrap_or_default();

        if attempt < max_retries && is_retryable_status(status, &err_text) {
            let delay_ms = match get_retry_after_delay_ms(&resp_headers) {
                Some(ra) => {
                    if max_retry_delay_ms > 0 && ra > max_retry_delay_ms {
                        return Err(Error::msg(format!(
                            "Server requested {}s retry delay (max: {}s)",
                            (ra + 999) / 1000,
                            (max_retry_delay_ms + 999) / 1000
                        )));
                    }
                    ra
                }
                None => BASE_DELAY_MS.saturating_mul(2u64.saturating_pow(attempt)),
            };
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            continue;
        }

        return Err(Error::msg(parse_error_response(status, &status_text, &err_text)));
    }

    Err(last_err.unwrap_or_else(|| Error::msg("Failed after retries")))
}

async fn run_codex_stream(
    model: Model,
    context: TranscriptContext,
    options: OpenAICodexResponsesOptions,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) -> Result<()> {
    ensure_cleanup_registered();
    let api_key = options
        .base
        .api_key
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::msg(format!("No API key for provider: {}", model.provider)))?;

    let account_id = extract_account_id(api_key)?;
    let compat = model.compat.as_ref().and_then(|c| c.as_openai_responses());
    let supports_mid_convo = compat.and_then(|c| c.supports_mid_convo_system_messages);
    let normalized_context = resolve_transcript(&context, supports_mid_convo);
    let supports_grammar_tools = compat.and_then(|c| c.supports_open_ai_grammar_tools).unwrap_or(false);
    let declared_tools = get_declared_tools(&normalized_context.messages);
    let grammar_props = create_grammar_tool_input_properties(Some(&declared_tools), supports_grammar_tools)?;

    let cache_session_id = if options.base.cache_retention == Some(CacheRetention::None) {
        None
    } else {
        options.base.session_id.as_deref()
    };
    let codex_session_id = clamp_openai_prompt_cache_key(cache_session_id);

    let mut body = build_request_body(
        &model,
        &normalized_context,
        &options,
        codex_session_id.as_deref(),
        &grammar_props,
    )?;

    if let Some(on_payload) = &options.base.on_payload {
        if let Some(next_body) = on_payload(body.clone(), &model).await? {
            body = next_body;
        }
    }

    let ws_request_id = match codex_session_id.clone() {
        Some(id) if !id.is_empty() => id,
        _ => uuidv7(None)?,
    };
    // Codex expects an originator on every request; it is not attribution.
    let mut identity_headers = IndexMap::new();
    identity_headers.insert("originator".to_string(), "pi".to_string());
    for (k, v) in get_identity_headers(
        ClientIdentityTarget {
            provider: &model.provider,
            base_url: &model.base_url,
        },
        options.base.identity.as_ref(),
    ) {
        identity_headers.insert(k, v);
    }
    let sse_headers = build_sse_headers(
        &identity_headers,
        model.headers.as_ref(),
        options.base.headers.as_ref(),
        &account_id,
        api_key,
        codex_session_id.as_deref(),
    );
    let ws_headers = build_websocket_headers(
        &identity_headers,
        model.headers.as_ref(),
        options.base.headers.as_ref(),
        &account_id,
        api_key,
        &ws_request_id,
    );

    let body_json = pi_js::json::stringify(&body);
    let transport = options.base.transport.unwrap_or(Transport::Auto);
    let mut start_emitted = false;
    let ws_disabled = transport != Transport::Sse && is_websocket_sse_fallback_active(cache_session_id).await;
    if ws_disabled {
        record_websocket_sse_fallback(cache_session_id).await;
    }

    if transport != Transport::Sse && !ws_disabled {
        let mut retried_conn_limit = false;
        let mut retried_prev_resp = false;
        loop {
            let started_before = start_emitted;
            match try_websocket_turn(
                &model,
                &normalized_context,
                &body,
                &ws_headers,
                output,
                stream,
                &mut start_emitted,
                cache_session_id,
                &account_id,
                &grammar_props,
                &options,
            )
            .await
            {
                Ok(()) => return Ok(()),
                Err(err_meta) => {
                    let aborted = options.base.signal.as_ref().is_some_and(AbortSignal::aborted);
                    let conn_limit = started_before == start_emitted
                        && err_meta.code.as_deref() == Some(WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE);
                    let prev_not_found = err_meta.code.as_deref() == Some(PREVIOUS_RESPONSE_NOT_FOUND_CODE);
                    if !aborted && prev_not_found && !retried_prev_resp {
                        retried_prev_resp = true;
                        continue;
                    }
                    if !aborted && conn_limit && !retried_conn_limit {
                        retried_conn_limit = true;
                        continue;
                    }
                    if aborted || (err_meta.non_transport && !conn_limit) {
                        return Err(Error::msg(err_meta.message));
                    }
                    let ws_started = start_emitted && !started_before;
                    let mut details = Map::new();
                    details.insert(
                        "configuredTransport".to_string(),
                        Value::String(format!("{transport:?}").to_ascii_lowercase()),
                    );
                    if !ws_started {
                        details.insert("fallbackTransport".to_string(), Value::String("sse".to_string()));
                    }
                    details.insert("eventsEmitted".to_string(), Value::Bool(ws_started));
                    details.insert(
                        "phase".to_string(),
                        Value::String(if ws_started {
                            "after_message_stream_start".to_string()
                        } else {
                            "before_message_stream_start".to_string()
                        }),
                    );
                    details.insert("requestBytes".to_string(), Value::from(body_json.len() as u64));
                    let dummy_err = Error::msg(err_meta.message.clone());
                    append_assistant_message_diagnostic(
                        output,
                        create_assistant_message_diagnostic(
                            "provider_transport_failure",
                            &dummy_err,
                            Some(details.into_iter().collect()),
                        ),
                    );
                    record_websocket_failure(cache_session_id, &err_meta.message).await;
                    if ws_started {
                        return Err(Error::msg(err_meta.message));
                    }
                    record_websocket_sse_fallback(cache_session_id).await;
                    break;
                }
            }
        }
    }

    let response = execute_sse_request(&model, &sse_headers, &body_json, &options).await?;
    let raw_sse_stream =
        response_to_sse_json_stream(response, model.clone(), options.base.on_provider_stream_event.clone())?;
    let sse_stream: ResponsesEventJsonStream = match options.base.signal.clone() {
        Some(sig) => Box::pin(futures::stream::unfold(
            (Some(raw_sse_stream), sig),
            |(mut inner_opt, sig)| async move {
                let inner = inner_opt.as_mut()?;
                if sig.aborted() {
                    drop(inner_opt.take());
                    return Some((Err(Error::msg("Request was aborted")), (None, sig)));
                }
                tokio::select! {
                    _ = sig.cancelled() => {
                        drop(inner_opt.take());
                        Some((Err(Error::msg("Request was aborted")), (None, sig)))
                    }
                    item = inner.next() => {
                        item.map(|val| (val, (inner_opt, sig)))
                    }
                }
            },
        )),
        None => raw_sse_stream,
    };

    if !start_emitted {
        stream.push(AssistantMessageEvent::Start {
            partial: output.clone(),
        });
    }

    let process_opts = ProcessResponsesStreamOptions {
        service_tier: options.service_tier.clone(),
        resolve_service_tier_on_default: true,
        grammar_tool_input_properties: if grammar_props.is_empty() {
            None
        } else {
            Some(grammar_props)
        },
    };
    process_responses_stream(sse_stream, output, stream, &model, Some(&process_opts)).await
}

/// Stream responses using OpenAI Codex Responses API (`openai-codex-responses`).
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: Option<ApiStreamOptions>,
) -> AssistantMessageEventStream {
    let model_owned = model.clone();
    let context_owned = context.clone();
    let typed_options = OpenAICodexResponsesOptions::from_api_options(options);
    let stream = create_assistant_message_event_stream();
    let stream_handle = stream.clone();

    tokio::spawn(async move {
        let mut output = create_pending_assistant_message(&model_owned);
        match run_codex_stream(
            model_owned.clone(),
            context_owned,
            typed_options.clone(),
            &mut output,
            &stream_handle,
        )
        .await
        {
            Ok(()) => {
                if options_aborted(&typed_options) {
                    output.stop_reason = StopReason::Aborted;
                    if output.error_message.is_none() {
                        output.error_message = Some("Request was aborted".to_string());
                    }
                    stream_handle.push(AssistantMessageEvent::Error {
                        reason: StopReason::Aborted,
                        error: output,
                    });
                } else {
                    let reason = output.stop_reason;
                    if reason == StopReason::Pending {
                        output.stop_reason = StopReason::Error;
                        output.error_message = Some("Codex stream ended without a stop reason".to_string());
                        stream_handle.push(AssistantMessageEvent::Error {
                            reason: StopReason::Error,
                            error: output,
                        });
                    } else if reason == StopReason::Error || reason == StopReason::Aborted {
                        stream_handle.push(AssistantMessageEvent::Error { reason, error: output });
                    } else {
                        stream_handle.push(AssistantMessageEvent::Done {
                            reason,
                            message: output,
                        });
                    }
                }
            }
            Err(err) => {
                output.stop_reason = if options_aborted(&typed_options) {
                    StopReason::Aborted
                } else {
                    StopReason::Error
                };
                output.error_message = Some(format_provider_error(&normalize_provider_error(&err), None));
                let reason = output.stop_reason;
                stream_handle.push(AssistantMessageEvent::Error { reason, error: output });
            }
        }
        stream_handle.end(None);
    });

    stream
}

fn options_aborted(options: &OpenAICodexResponsesOptions) -> bool {
    options.base.signal.as_ref().is_some_and(AbortSignal::aborted)
}

/// Simplified streaming wrapper for OpenAI Codex Responses API.
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let api_key = options.as_ref().and_then(|o| o.base.api_key.as_deref());
    let base = build_base_options(model, context, options.as_ref(), api_key);
    let clamped = options.as_ref().and_then(|o| o.reasoning).map(|lvl| {
        let model_lvl = match lvl {
            ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
            ThinkingLevel::Low => ModelThinkingLevel::Low,
            ThinkingLevel::Medium => ModelThinkingLevel::Medium,
            ThinkingLevel::High => ModelThinkingLevel::High,
            ThinkingLevel::XHigh => ModelThinkingLevel::XHigh,
            ThinkingLevel::Max => ModelThinkingLevel::Max,
        };
        clamp_thinking_level(model, model_lvl)
    });
    let reasoning_effort = clamped.and_then(|m| match m {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(OpenAICodexReasoningEffort::Minimal),
        ModelThinkingLevel::Low => Some(OpenAICodexReasoningEffort::Low),
        ModelThinkingLevel::Medium => Some(OpenAICodexReasoningEffort::Medium),
        ModelThinkingLevel::High => Some(OpenAICodexReasoningEffort::High),
        ModelThinkingLevel::XHigh => Some(OpenAICodexReasoningEffort::XHigh),
        ModelThinkingLevel::Max => Some(OpenAICodexReasoningEffort::Max),
    });
    let tool_choice = options
        .as_ref()
        .and_then(|o| o.tool_choice.as_ref())
        .map(|tc| match tc {
            crate::types::ToolChoice::Auto => "auto".to_string(),
            crate::types::ToolChoice::None => "none".to_string(),
        });

    let typed = OpenAICodexResponsesOptions {
        base,
        reasoning_effort,
        reasoning_summary: None,
        service_tier: None,
        text_verbosity: None,
        tool_choice,
    };
    stream(model, context, Some(typed.into()))
}

/// Construct `ProviderStreams` descriptor for OpenAI Codex Responses API.
pub fn provider_streams() -> ProviderStreams {
    ProviderStreams {
        stream: Arc::new(|model, context, options| stream(model, context, options)),
        stream_simple: Arc::new(|model, context, options| stream_simple(model, context, options)),
        fetch_deferred: None,
        cancel_deferred: None,
    }
}
