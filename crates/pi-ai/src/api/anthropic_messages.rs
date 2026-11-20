//! Port of packages/ai/src/api/anthropic-messages.ts

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, LazyLock, Once, OnceLock};

use futures::StreamExt;
use indexmap::{IndexMap, IndexSet};
use pi_js::abort::AbortSignal;
use pi_js::fetch::{Body, Headers, Request, Response};
use pi_js::{BoxFuture, Error, Result};
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use url::Url;

use crate::api::constrained_sampling::{
    UnsupportedStrictSchemaKeywordCheck, get_json_schema_tool_parameters, prefers_json_schema_strict_sampling,
    resolve_json_schema_strict_sampling,
};
use crate::api::lazy::lazy_stream;
use crate::api::simple_options::{adjust_max_tokens_for_thinking, build_base_options, clamp_max_tokens_to_context};
use crate::api::strict_tool_limits::{
    has_exceeded_strict_tool_limits, is_strict_tool_limit_error_message, mark_strict_tool_limits_exceeded,
};
use crate::api::transform_messages::{split_error_tool_result_images, transform_messages};
use crate::env_api_keys::{
    ANTHROPIC_FEDERATION_RULE_ID_ENV, ANTHROPIC_IDENTITY_TOKEN_FILE_ENV, ANTHROPIC_ORGANIZATION_ID_ENV,
    ANTHROPIC_SERVICE_ACCOUNT_ID_ENV, ANTHROPIC_WORKSPACE_ID_ENV,
};
use crate::models::calculate_cost;
use crate::session_resources::register_session_resource_cleanup;
use crate::types::{
    AnthropicAllowedFallbackModel, AnyModel, ApiStreamOptions, AssistantContentBlock, AssistantMessage,
    AssistantMessageEvent, CacheRetention, FetchFunction, ImageContent, JsonObject, JsonValue, Message, Model,
    ModelCompat, ModelThinkingLevel, ProviderEnv, ProviderHeaders, ProviderResponse, ProviderStreams,
    SessionAffinityFormat, SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingContent, ThinkingLevel,
    Tool, ToolCall, ToolChoice, ToolResultMessage, TranscriptContext, Usage, UsageCost, UserContentBlock,
    UserMessageContent,
};
use crate::utils::abort_signals::combine_abort_signals;
use crate::utils::diagnostics::{AssistantMessageDiagnostic, DiagnosticErrorInfo, append_assistant_message_diagnostic};
use crate::utils::error_body::{normalize_provider_error, safe_json_stringify, with_cause_detail};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::hash::short_hash;
use crate::utils::headers::headers_to_record;
use crate::utils::identity::{ClientIdentity, ClientIdentityTarget, get_identity_headers};
use crate::utils::json_parse::parse_json_with_repair;
use crate::utils::openrouter_cost::{
    OpenRouterCostDetails, OpenRouterUsageAccounting, apply_openrouter_reported_cost, is_openrouter_model,
};
use crate::utils::pi_user_agent::get_pi_user_agent;
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{ProviderHttpError, ProviderRetryOptions, retry_provider_request};
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::streaming_json::ToolCallArgumentsStream;
use crate::utils::text::{get_system_message_text, render_system_message_update};
use crate::utils::transcript::{get_current_tools, get_initial_system_message, resolve_transcript};

use crate::api::github_copilot_headers::{build_copilot_dynamic_headers, has_copilot_vision_input};

const CLAUDE_CODE_VERSION: &str = "2.1.322";

const CLAUDE_CODE_TOOLS: [&str; 17] = [
    "Read",
    "Write",
    "Edit",
    "Bash",
    "Grep",
    "Glob",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
    "KillShell",
    "NotebookEdit",
    "Skill",
    "Task",
    "TaskOutput",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
];

const FINE_GRAINED_TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";
const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";
const SERVER_SIDE_FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const MID_CONVERSATION_OUTPUT_CONFIG_BETA: &str = "mid-conversation-output-config-2026-07-01";
const THINKING_BINDING_CONTROLS_BETA: &str = "thinking-binding-controls-2026-08-01";
const INLINE_TOOLS_BETA: &str = "inline-tools-2026-09-15";
const MESSAGE_THREADS_BETA: &str = "message-threads-2026-08-12";

const GRANT_TYPE_JWT_BEARER: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
const TOKEN_ENDPOINT: &str = "/v1/oauth/token";
const OAUTH_API_BETA_HEADER: &str = "oauth-2025-04-20";
const FEDERATION_BETA_HEADER: &str = "oidc-federation-2026-04-01";
const MANDATORY_REFRESH_THRESHOLD_SECS: i64 = 30;
const MAX_ASSERTION_BYTES: usize = 16 * 1024;
const MAX_ERROR_BODY_CHARS: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicEffort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl AnthropicEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicThinkingDisplay {
    Summarized,
    Omitted,
}

impl AnthropicThinkingDisplay {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Summarized => "summarized",
            Self::Omitted => "omitted",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnthropicToolChoice {
    Auto,
    Any,
    None,
    Tool { name: String },
}

impl From<ToolChoice> for AnthropicToolChoice {
    fn from(value: ToolChoice) -> Self {
        match value {
            ToolChoice::Auto => Self::Auto,
            ToolChoice::None => Self::None,
        }
    }
}

impl Serialize for AnthropicToolChoice {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Auto => serializer.serialize_str("auto"),
            Self::Any => serializer.serialize_str("any"),
            Self::None => serializer.serialize_str("none"),
            Self::Tool { name } => {
                let mut map = Map::new();
                map.insert("type".to_string(), Value::String("tool".to_string()));
                map.insert("name".to_string(), Value::String(name.clone()));
                Value::Object(map).serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for AnthropicToolChoice {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let val = Value::deserialize(deserializer)?;
        match &val {
            Value::String(s) => match s.as_str() {
                "auto" => Ok(Self::Auto),
                "any" => Ok(Self::Any),
                "none" => Ok(Self::None),
                other => Err(de::Error::custom(format!("unknown Anthropic toolChoice: {other}"))),
            },
            Value::Object(obj) => match obj.get("type").and_then(Value::as_str) {
                Some("auto") => Ok(Self::Auto),
                Some("any") => Ok(Self::Any),
                Some("none") => Ok(Self::None),
                Some("tool") => {
                    let name = obj
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| de::Error::missing_field("name"))?;
                    Ok(Self::Tool { name: name.to_string() })
                }
                other => Err(de::Error::custom(format!(
                    "unknown Anthropic toolChoice type: {:?}",
                    other
                ))),
            },
            _ => Err(de::Error::custom("Anthropic toolChoice must be a string or object")),
        }
    }
}

impl AnthropicToolChoice {
    fn to_api_value(&self) -> Value {
        let mut map = Map::new();
        match self {
            Self::Auto => {
                map.insert("type".to_string(), Value::String("auto".to_string()));
            }
            Self::Any => {
                map.insert("type".to_string(), Value::String("any".to_string()));
            }
            Self::None => {
                map.insert("type".to_string(), Value::String("none".to_string()));
            }
            Self::Tool { name } => {
                map.insert("type".to_string(), Value::String("tool".to_string()));
                map.insert("name".to_string(), Value::String(name.clone()));
            }
        }
        Value::Object(map)
    }
}

#[derive(Clone, Debug, Default)]
pub struct AnthropicClientRequestOptions {
    pub signal: Option<AbortSignal>,
    pub timeout_ms: Option<u64>,
    pub max_retries: u32,
}

pub type AnthropicCreateMessagesFn =
    Arc<dyn Fn(JsonValue, AnthropicClientRequestOptions) -> BoxFuture<Result<Response>> + Send + Sync>;

#[derive(Clone)]
pub struct AnthropicClient {
    create_messages: AnthropicCreateMessagesFn,
}

impl AnthropicClient {
    pub fn new(create_messages: AnthropicCreateMessagesFn) -> Self {
        Self { create_messages }
    }

    pub fn from_handler<F, Fut>(handler: F) -> Self
    where
        F: Fn(JsonValue, AnthropicClientRequestOptions) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Response>> + Send + 'static,
    {
        Self {
            create_messages: Arc::new(move |params, opts| Box::pin(handler(params, opts))),
        }
    }

    pub async fn create_beta_messages(
        &self,
        params: JsonValue,
        options: AnthropicClientRequestOptions,
    ) -> Result<Response> {
        (self.create_messages)(params, options).await
    }
}

impl fmt::Debug for AnthropicClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<AnthropicClient>")
    }
}

#[derive(Clone, Default)]
pub struct AnthropicOptions {
    pub base: StreamOptions,
    pub thinking_enabled: Option<bool>,
    pub thinking_budget_tokens: Option<i64>,
    pub effort: Option<AnthropicEffort>,
    pub thinking_display: Option<AnthropicThinkingDisplay>,
    pub interleaved_thinking: Option<bool>,
    pub tool_choice: Option<AnthropicToolChoice>,
    pub client: Option<AnthropicClient>,
}

impl fmt::Debug for AnthropicOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicOptions")
            .field("base", &self.base)
            .field("thinking_enabled", &self.thinking_enabled)
            .field("thinking_budget_tokens", &self.thinking_budget_tokens)
            .field("effort", &self.effort)
            .field("thinking_display", &self.thinking_display)
            .field("interleaved_thinking", &self.interleaved_thinking)
            .field("tool_choice", &self.tool_choice)
            .field("client", &self.client.as_ref().map(|_| "<AnthropicClient>"))
            .finish()
    }
}

impl Deref for AnthropicOptions {
    type Target = StreamOptions;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for AnthropicOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl From<StreamOptions> for AnthropicOptions {
    fn from(base: StreamOptions) -> Self {
        Self {
            base,
            ..Default::default()
        }
    }
}

impl From<AnthropicOptions> for ApiStreamOptions {
    fn from(options: AnthropicOptions) -> Self {
        let mut extra = JsonObject::new();
        if let Some(thinking_enabled) = options.thinking_enabled {
            extra.insert("thinkingEnabled".to_string(), Value::Bool(thinking_enabled));
        }
        if let Some(budget) = options.thinking_budget_tokens {
            extra.insert("thinkingBudgetTokens".to_string(), Value::from(budget));
        }
        if let Some(effort) = options.effort {
            extra.insert("effort".to_string(), Value::String(effort.as_str().to_string()));
        }
        if let Some(display) = options.thinking_display {
            extra.insert(
                "thinkingDisplay".to_string(),
                Value::String(display.as_str().to_string()),
            );
        }
        if let Some(interleaved) = options.interleaved_thinking {
            extra.insert("interleavedThinking".to_string(), Value::Bool(interleaved));
        }
        if let Some(tool_choice) = &options.tool_choice {
            let v = match tool_choice {
                AnthropicToolChoice::Auto => serde_json::json!({ "type": "auto" }),
                AnthropicToolChoice::Any => serde_json::json!({ "type": "any" }),
                AnthropicToolChoice::None => serde_json::json!({ "type": "none" }),
                AnthropicToolChoice::Tool { name } => serde_json::json!({
                    "type": "tool",
                    "name": name,
                }),
            };
            extra.insert("toolChoice".to_string(), v);
        }
        let extension: Arc<dyn std::any::Any + Send + Sync> = Arc::new(options.clone());
        Self {
            base: options.base,
            extra,
            extension: Some(extension),
        }
    }
}

impl AnthropicOptions {
    fn from_api_options(options: Option<ApiStreamOptions>) -> Self {
        let Some(opts) = options else {
            return Self::default();
        };
        if let Some(ext) = opts
            .extension
            .as_ref()
            .and_then(|e| e.downcast_ref::<AnthropicOptions>())
        {
            let mut merged = ext.clone();
            merged.base = opts.base;
            return merged;
        }
        let thinking_enabled = opts.extra.get("thinkingEnabled").and_then(Value::as_bool);
        let thinking_budget_tokens = opts.extra.get("thinkingBudgetTokens").and_then(Value::as_i64);
        let effort = opts
            .extra
            .get("effort")
            .and_then(Value::as_str)
            .and_then(AnthropicEffort::from_str_opt);
        let thinking_display = opts
            .extra
            .get("thinkingDisplay")
            .cloned()
            .and_then(|v| serde_json::from_value::<AnthropicThinkingDisplay>(v).ok());
        let interleaved_thinking = opts.extra.get("interleavedThinking").and_then(Value::as_bool);
        let tool_choice = opts
            .extra
            .get("toolChoice")
            .cloned()
            .and_then(|v| serde_json::from_value::<AnthropicToolChoice>(v).ok());
        Self {
            base: opts.base,
            thinking_enabled,
            thinking_budget_tokens,
            effort,
            thinking_display,
            interleaved_thinking,
            tool_choice,
            client: None,
        }
    }
}

struct ResolvedAnthropicCompat<'a> {
    supports_eager_tool_input_streaming: bool,
    supports_long_cache_retention: bool,
    send_session_affinity_headers: bool,
    session_affinity_format: Option<SessionAffinityFormat>,
    supports_cache_control_on_tools: bool,
    supports_temperature: bool,
    allow_empty_signature: bool,
    supports_strict_tools: bool,
    supports_mid_convo_system_messages: bool,
    supports_mid_convo_tool_changes: bool,
    supports_mid_convo_effort: bool,
    supports_message_threads: bool,
    force_adaptive_thinking: bool,
    allowed_fallback_models: Option<&'a [AnthropicAllowedFallbackModel]>,
}

fn anthropic_compat(c: &ModelCompat) -> Option<&crate::types::AnthropicMessagesCompat> {
    match c {
        ModelCompat::AnthropicMessages(compat) => Some(compat),
        _ => None,
    }
}

fn get_anthropic_compat<'a>(model: &'a Model) -> ResolvedAnthropicCompat<'a> {
    let is_openrouter = model.provider == "openrouter" || model.base_url.contains("openrouter.ai");
    let c = model.compat.as_ref();
    ResolvedAnthropicCompat {
        supports_eager_tool_input_streaming: c
            .and_then(anthropic_compat)
            .and_then(|c| c.supports_eager_tool_input_streaming)
            .unwrap_or(true),
        supports_long_cache_retention: c
            .and_then(anthropic_compat)
            .and_then(|c| c.supports_long_cache_retention)
            .unwrap_or(true),
        send_session_affinity_headers: c
            .and_then(anthropic_compat)
            .and_then(|c| c.send_session_affinity_headers)
            .unwrap_or(is_openrouter),
        session_affinity_format: c.and_then(anthropic_compat).and_then(|c| c.session_affinity_format).or(
            if is_openrouter {
                Some(SessionAffinityFormat::OpenRouter)
            } else {
                None
            },
        ),
        supports_cache_control_on_tools: c
            .and_then(anthropic_compat)
            .and_then(|c| c.supports_cache_control_on_tools)
            .unwrap_or(true),
        supports_temperature: c
            .and_then(anthropic_compat)
            .and_then(|c| c.supports_temperature)
            .unwrap_or(true),
        allow_empty_signature: c
            .and_then(anthropic_compat)
            .and_then(|c| c.allow_empty_signature)
            .unwrap_or(false),
        supports_strict_tools: c
            .and_then(anthropic_compat)
            .and_then(|c| c.supports_strict_tools)
            .unwrap_or(false),
        supports_mid_convo_system_messages: c
            .and_then(anthropic_compat)
            .and_then(|c| c.supports_mid_convo_system_messages)
            .unwrap_or(false),
        supports_mid_convo_tool_changes: c
            .and_then(anthropic_compat)
            .and_then(|c| c.supports_mid_convo_tool_changes)
            .unwrap_or(false),
        supports_mid_convo_effort: c
            .and_then(anthropic_compat)
            .and_then(|c| c.supports_mid_convo_effort)
            .unwrap_or(false),
        supports_message_threads: c
            .and_then(anthropic_compat)
            .and_then(|c| c.supports_message_threads)
            .unwrap_or_else(|| model.provider == "anthropic" && model.base_url.contains("api.anthropic.com")),
        force_adaptive_thinking: c
            .and_then(anthropic_compat)
            .and_then(|c| c.force_adaptive_thinking)
            .unwrap_or(false),
        allowed_fallback_models: c
            .and_then(anthropic_compat)
            .and_then(|c| c.allowed_fallback_models.as_deref()),
    }
}

#[derive(Clone, Debug)]
struct ThreadEntry {
    prefix_count: usize,
    prefix_hash: String,
    reply_id: String,
}

#[derive(Clone, Debug)]
enum ThreadStateValue {
    Entry(ThreadEntry),
    Off,
}

const MAX_THREAD_STATE_ENTRIES: usize = 10_000;

static THREAD_STATE: LazyLock<std::sync::Mutex<IndexMap<String, ThreadStateValue>>> =
    LazyLock::new(|| std::sync::Mutex::new(IndexMap::new()));
static THREAD_CLEANUP_REGISTERED: Once = Once::new();

fn ensure_thread_cleanup_registered() {
    THREAD_CLEANUP_REGISTERED.call_once(|| {
        let _ = register_session_resource_cleanup(|session_id| {
            let mut guard = THREAD_STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match session_id {
                Some(id) => {
                    guard.shift_remove(id);
                }
                None => guard.clear(),
            }
        });
    });
}

fn set_thread_state(session_id: &str, value: ThreadStateValue) {
    ensure_thread_cleanup_registered();
    let mut guard = THREAD_STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.shift_remove(session_id);
    guard.insert(session_id.to_string(), value);
    while guard.len() > MAX_THREAD_STATE_ENTRIES {
        guard.shift_remove_index(0);
    }
}

fn is_new_thread_error(code: &str) -> bool {
    matches!(
        code,
        "thread_not_found" | "thread_already_continued" | "thread_continue_in_flight" | "thread_fingerprint_mismatch"
    )
}

fn normalize_thread_messages_value(messages: &[Value]) -> Value {
    fn walk(val: &Value) -> Value {
        match val {
            Value::Array(arr) => Value::Array(arr.iter().map(walk).collect()),
            Value::Object(map) => {
                let mut out = Map::new();
                for (k, v) in map {
                    if k == "cache_control" {
                        continue;
                    }
                    if k == "content"
                        && let Some(s) = v.as_str()
                    {
                        let mut blk = Map::new();
                        blk.insert("type".to_string(), Value::String("text".to_string()));
                        blk.insert("text".to_string(), Value::String(s.to_string()));
                        out.insert(k.clone(), Value::Array(vec![Value::Object(blk)]));
                        continue;
                    }
                    out.insert(k.clone(), walk(v));
                }
                Value::Object(out)
            }
            other => other.clone(),
        }
    }
    Value::Array(messages.iter().map(walk).collect())
}

fn hash_thread_prefix(model: &Model, params: &Map<String, Value>, messages: &[Value]) -> String {
    let settings = Value::Array(vec![
        Value::String(model.provider.clone()),
        Value::String(model.base_url.clone()),
        params.get("model").cloned().unwrap_or(Value::Null),
        params.get("system").cloned().unwrap_or(Value::Null),
        params.get("tools").cloned().unwrap_or(Value::Null),
        params.get("thinking").cloned().unwrap_or(Value::Null),
        params.get("output_config").cloned().unwrap_or(Value::Null),
        params.get("tool_choice").cloned().unwrap_or(Value::Null),
        params.get("betas").cloned().unwrap_or(Value::Null),
        params.get("fallbacks").cloned().unwrap_or(Value::Null),
    ]);
    let conversation = normalize_thread_messages_value(messages);
    let combined = format!(
        "{}{}",
        pi_js::json::stringify(&settings),
        pi_js::json::stringify(&conversation)
    );
    short_hash(&combined)
}

#[derive(Clone, Debug)]
struct PreparedThreadRequest {
    session_id: String,
    body: Map<String, Value>,
}

fn create_thread_request(session_id: &str, params: &Map<String, Value>) -> PreparedThreadRequest {
    let mut body = params.clone();
    body.insert("thread".to_string(), serde_json::json!({ "type": "create" }));
    PreparedThreadRequest {
        session_id: session_id.to_string(),
        body,
    }
}

fn prepare_thread_request(
    model: &Model,
    session_id: &str,
    params: &Map<String, Value>,
) -> Option<PreparedThreadRequest> {
    ensure_thread_cleanup_registered();
    let messages = params.get("messages").and_then(Value::as_array)?;
    let state = {
        let mut guard = THREAD_STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match guard.get(session_id) {
            Some(ThreadStateValue::Off) => return None,
            Some(ThreadStateValue::Entry(entry)) => {
                if messages.len() > entry.prefix_count
                    && hash_thread_prefix(model, params, &messages[..entry.prefix_count]) == entry.prefix_hash
                {
                    let cloned = entry.clone();
                    guard.shift_remove(session_id);
                    Some(cloned)
                } else {
                    None
                }
            }
            None => None,
        }
    };

    if let Some(entry) = state {
        let mut body = params.clone();
        body.shift_remove("system");
        body.shift_remove("tools");
        body.insert(
            "messages".to_string(),
            Value::Array(messages[entry.prefix_count..].to_vec()),
        );
        body.insert(
            "thread".to_string(),
            serde_json::json!({
                "type": "continue",
                "previous_message_id": entry.reply_id,
            }),
        );
        return Some(PreparedThreadRequest {
            session_id: session_id.to_string(),
            body,
        });
    }

    Some(create_thread_request(session_id, params))
}

fn record_thread_reply(
    model: &Model,
    thread: &PreparedThreadRequest,
    params: &Map<String, Value>,
    output: &AssistantMessage,
    is_oauth: bool,
    allow_empty_signature: bool,
) {
    let Some(reply_id) = output.response_id.as_deref() else {
        return;
    };
    if output.response_model.is_some() {
        return;
    }
    let single_reply = [Message::Assistant(output.clone())];
    let transformed = transform_messages(&single_reply, model, Some(&normalize_tool_call_id));
    let mut dummy_has_preferred = false;
    let converted = convert_messages(
        &transformed,
        is_oauth,
        None,
        allow_empty_signature,
        None,
        None,
        &mut dummy_has_preferred,
    )
    .unwrap_or(ConvertedAnthropicMessages {
        messages: Vec::new(),
        assistant_levels: IndexMap::new(),
    });
    let reply_messages: Vec<Value> = converted
        .messages
        .into_iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .collect();
    let mut prefix: Vec<Value> = params
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    prefix.extend(reply_messages);
    let prefix_count = prefix.len();
    let prefix_hash = hash_thread_prefix(model, params, &prefix);
    set_thread_state(
        &thread.session_id,
        ThreadStateValue::Entry(ThreadEntry {
            prefix_count,
            prefix_hash,
            reply_id: reply_id.to_string(),
        }),
    );
}

fn resolve_cache_retention(cache_retention: Option<CacheRetention>, env: Option<&ProviderEnv>) -> CacheRetention {
    if let Some(retention) = cache_retention {
        return retention;
    }
    if get_provider_env_value("PI_CACHE_RETENTION", env).as_deref() == Some("long") {
        return CacheRetention::Long;
    }
    CacheRetention::Short
}

fn get_cache_control(
    model: &Model,
    cache_retention: Option<CacheRetention>,
    env: Option<&ProviderEnv>,
) -> (CacheRetention, Option<Value>) {
    let retention = resolve_cache_retention(cache_retention, env);
    if retention == CacheRetention::None {
        return (retention, None);
    }
    let mut obj = Map::new();
    obj.insert("type".to_string(), Value::String("ephemeral".to_string()));
    if retention == CacheRetention::Long && get_anthropic_compat(model).supports_long_cache_retention {
        obj.insert("ttl".to_string(), Value::String("1h".to_string()));
    }
    (retention, Some(Value::Object(obj)))
}

fn to_claude_code_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    for canonical in CLAUDE_CODE_TOOLS {
        if canonical.to_ascii_lowercase() == lower {
            return canonical.to_string();
        }
    }
    name.to_string()
}

fn from_claude_code_name(name: &str, tools: &[Tool]) -> String {
    if !tools.is_empty() {
        let lower = name.to_ascii_lowercase();
        if let Some(matched) = tools.iter().find(|tool| tool.name.to_ascii_lowercase() == lower) {
            return matched.name.clone();
        }
    }
    name.to_string()
}

fn has_header(headers: Option<&ProviderHeaders>, name: &str) -> bool {
    let Some(headers) = headers else {
        return false;
    };
    let expected = name.to_ascii_lowercase();
    let mut found = false;
    for (key, value) in headers {
        if key.to_ascii_lowercase() == expected {
            found = value.as_deref().is_some_and(|v| !pi_js::str16::trim(v).is_empty());
        }
    }
    found
}

fn has_request_auth(api_key: Option<&str>, headers: Option<&ProviderHeaders>) -> bool {
    api_key.is_some_and(|k| !k.is_empty())
        || has_header(headers, "authorization")
        || has_header(headers, "x-api-key")
        || has_header(headers, "cf-aig-authorization")
}

fn assert_request_auth(provider: &str, api_key: Option<&str>, headers: Option<&ProviderHeaders>) -> Result<()> {
    if !has_request_auth(api_key, headers) {
        return Err(Error::msg(format!("No API key for provider: {provider}")));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AnthropicFederationConfig {
    organization_id: String,
    workspace_id: Option<String>,
    federation_rule_id: String,
    service_account_id: Option<String>,
    identity_token_file: String,
}

fn get_anthropic_federation(
    model: &Model,
    api_key: Option<&str>,
    headers: Option<&ProviderHeaders>,
    env: Option<&ProviderEnv>,
) -> Option<AnthropicFederationConfig> {
    if model.provider != "anthropic" || has_request_auth(api_key, headers) {
        return None;
    }
    let federation_rule_id = get_provider_env_value(ANTHROPIC_FEDERATION_RULE_ID_ENV, env)?;
    let organization_id = get_provider_env_value(ANTHROPIC_ORGANIZATION_ID_ENV, env)?;
    let identity_token_file = get_provider_env_value(ANTHROPIC_IDENTITY_TOKEN_FILE_ENV, env)?;
    Some(AnthropicFederationConfig {
        organization_id,
        workspace_id: get_provider_env_value(ANTHROPIC_WORKSPACE_ID_ENV, env),
        federation_rule_id,
        service_account_id: get_provider_env_value(ANTHROPIC_SERVICE_ACCOUNT_ID_ENV, env),
        identity_token_file,
    })
}

#[derive(Clone)]
struct CachedFederationToken {
    key: String,
    fetch_ptr: Option<usize>,
    token: String,
    expires_at_secs: Option<i64>,
}

static FEDERATION_CACHE: OnceLock<Mutex<Option<CachedFederationToken>>> = OnceLock::new();

fn federation_cache() -> &'static Mutex<Option<CachedFederationToken>> {
    FEDERATION_CACHE.get_or_init(|| Mutex::new(None))
}

fn fetch_fn_ptr(fetch: Option<&FetchFunction>) -> Option<usize> {
    fetch.map(|f| Arc::as_ptr(f) as *const () as usize)
}

fn require_secure_token_endpoint(base_url: &str) -> Result<()> {
    if base_url.is_empty() {
        return Ok(());
    }
    let parsed =
        Url::parse(base_url).map_err(|e| Error::msg(format!("Invalid token endpoint base URL \"{base_url}\": {e}")))?;
    if parsed.scheme() == "https" {
        return Ok(());
    }
    let host = parsed
        .host_str()
        .unwrap_or("")
        .to_ascii_lowercase()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    if parsed.scheme() == "http" && matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1") {
        return Ok(());
    }
    Err(Error::msg(format!(
        "Refusing to send credential over non-https token endpoint \"{base_url}\""
    )))
}

fn redact_sensitive_text(raw: &str) -> String {
    if let Ok(parsed) = pi_js::json::parse(raw) {
        return pi_js::json::stringify(&redact_sensitive_value(&parsed));
    }
    let char_count = raw.chars().count();
    if char_count <= MAX_ERROR_BODY_CHARS {
        return raw.to_string();
    }
    let prefix: String = raw.chars().take(MAX_ERROR_BODY_CHARS).collect();
    format!("{prefix}... <{} more chars>", char_count - MAX_ERROR_BODY_CHARS)
}

fn redact_sensitive_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for key in ["error", "error_description", "error_uri"] {
                if let Some(v) = map.get(key) {
                    out.insert(key.to_string(), v.clone());
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

async fn resolve_federation_token(
    base_url: &str,
    federation: &AnthropicFederationConfig,
    fetch: Option<&FetchFunction>,
    signal: Option<&AbortSignal>,
) -> Result<String> {
    let base_trimmed = base_url.trim_end_matches('/');
    let key = pi_js::json::stringify(&serde_json::json!([
        base_trimmed,
        federation.organization_id,
        federation.workspace_id,
        federation.federation_rule_id,
        federation.service_account_id,
        federation.identity_token_file,
    ]));
    let fptr = fetch_fn_ptr(fetch);
    let now_secs = pi_js::time::now_ms() / 1000;

    {
        let guard = federation_cache().lock().await;
        if let Some(cached) = guard.as_ref() {
            if cached.key == key && cached.fetch_ptr == fptr {
                let valid = match cached.expires_at_secs {
                    None => true,
                    Some(exp) => (exp - now_secs) > MANDATORY_REFRESH_THRESHOLD_SECS,
                };
                if valid {
                    return Ok(cached.token.clone());
                }
            }
        }
    }

    require_secure_token_endpoint(base_trimmed)?;
    let jwt_raw = tokio::fs::read_to_string(&federation.identity_token_file)
        .await
        .map_err(|e| {
            Error::msg(format!(
                "Failed to read identity token file \"{}\": {e}",
                federation.identity_token_file
            ))
        })?;
    let jwt = pi_js::str16::trim(&jwt_raw);
    if jwt.is_empty() {
        return Err(Error::msg(format!(
            "Identity token file \"{}\" is empty",
            federation.identity_token_file
        )));
    }
    if jwt.len() > MAX_ASSERTION_BYTES {
        let kib = jwt.len().div_ceil(1024);
        return Err(Error::msg(format!(
            "Identity token is {kib} KiB, exceeds the 16 KiB assertion limit"
        )));
    }

    let mut body = Map::new();
    body.insert(
        "grant_type".to_string(),
        Value::String(GRANT_TYPE_JWT_BEARER.to_string()),
    );
    body.insert("assertion".to_string(), Value::String(jwt.to_string()));
    body.insert(
        "federation_rule_id".to_string(),
        Value::String(federation.federation_rule_id.clone()),
    );
    body.insert(
        "organization_id".to_string(),
        Value::String(federation.organization_id.clone()),
    );
    if let Some(sa) = &federation.service_account_id {
        body.insert("service_account_id".to_string(), Value::String(sa.clone()));
    }
    if let Some(ws) = &federation.workspace_id {
        body.insert("workspace_id".to_string(), Value::String(ws.clone()));
    }

    let token_url = format!("{base_trimmed}{TOKEN_ENDPOINT}");
    let mut headers = Headers::new();
    headers.set("content-type", "application/json");
    headers.set(
        "anthropic-beta",
        &format!("{OAUTH_API_BETA_HEADER},{FEDERATION_BETA_HEADER}"),
    );
    headers.set("user-agent", &get_pi_user_agent());

    let mut request = Request::post(&token_url)
        .with_headers(headers)
        .body(Body::Text(pi_js::json::stringify(&Value::Object(body))));
    if let Some(sig) = signal {
        request = request.signal(sig.clone());
    }

    let resp = match fetch {
        Some(f) => f(request).await,
        None => pi_js::fetch::fetch(request).await,
    }
    .map_err(|err| Error::msg(format!("Failed to reach token endpoint {token_url}: {err}")))?;

    let status = resp.status();
    let request_id = resp.headers().get("request-id");
    let text = resp.text().await.unwrap_or_default();
    if status < 200 || status >= 300 {
        let redacted = redact_sensitive_text(&text);
        let hint = if status == 401 {
            let middle = if federation.workspace_id.is_some() {
                ""
            } else {
                "If your federation rule is scoped to multiple workspaces, set the ANTHROPIC_WORKSPACE_ID environment variable, the 'workspace_id' config key, or the `workspaceId` option. "
            };
            format!(
                " Ensure your federation rule matches your identity token. {middle}View your authentication events in the Workload identity page of Claude Console for more details."
            )
        } else {
            String::new()
        };
        let req_id_part = request_id
            .as_deref()
            .map(|id| format!(" (request-id {id})"))
            .unwrap_or_default();
        return Err(Error::msg(format!(
            "Token exchange failed with status {status}{req_id_part}: {redacted}{hint}"
        )));
    }

    let data = pi_js::json::parse(&text)
        .map_err(|_| Error::msg(format!("Token endpoint returned non-JSON response (status {status})")))?;
    let Some(obj) = data.as_object() else {
        return Err(Error::msg(format!(
            "Token endpoint returned non-object response (status {status})"
        )));
    };
    let Some(access_token) = obj.get("access_token").and_then(Value::as_str) else {
        return Err(Error::msg(format!(
            "Token endpoint response missing access_token: {}",
            pi_js::json::stringify(&redact_sensitive_value(&data))
        )));
    };
    if let Some(token_type) = obj.get("token_type").and_then(Value::as_str) {
        if token_type.to_ascii_lowercase() != "bearer" {
            return Err(Error::msg(format!(
                "Token endpoint response: unsupported token_type \"{token_type}\" (want Bearer)"
            )));
        }
    }
    let Some(expires_in) = obj.get("expires_in").and_then(Value::as_f64).filter(|n| n.is_finite()) else {
        return Err(Error::msg(format!(
            "Token endpoint response missing required fields: {}",
            pi_js::json::stringify(&redact_sensitive_value(&data))
        )));
    };

    let expires_at_secs = (pi_js::time::now_ms() / 1000) + (expires_in.floor() as i64);
    let token_str = access_token.to_string();

    {
        let mut guard = federation_cache().lock().await;
        *guard = Some(CachedFederationToken {
            key,
            fetch_ptr: fptr,
            token: token_str.clone(),
            expires_at_secs: Some(expires_at_secs),
        });
    }

    Ok(token_str)
}

async fn invalidate_federation_cache() {
    let mut guard = federation_cache().lock().await;
    *guard = None;
}

fn is_oauth_token(api_key: &str) -> bool {
    api_key.contains("sk-ant-oat")
}

fn apply_header_layer(headers: &mut Headers, source: &IndexMap<String, Option<String>>) {
    for (k, v) in source {
        match v {
            Some(val) => headers.set(k, val),
            None => headers.delete(k),
        }
    }
}

fn apply_raw_header_layer(headers: &mut Headers, source: &IndexMap<String, String>) {
    for (k, v) in source {
        headers.set(k, v);
    }
}

fn build_request_headers(
    model: &Model,
    api_key: Option<&str>,
    options_headers: Option<&ProviderHeaders>,
    dynamic_headers: Option<&IndexMap<String, String>>,
    session_id: Option<&str>,
    federation_token: Option<&str>,
    betas: &[String],
    identity: Option<&ClientIdentity>,
) -> (Headers, bool) {
    let mut headers = Headers::new();
    let target = ClientIdentityTarget {
        provider: &model.provider,
        base_url: &model.base_url,
    };
    let is_oauth_call =
        model.provider != "github-copilot" && federation_token.is_none() && api_key.is_some_and(is_oauth_token);
    if !is_oauth_call {
        apply_raw_header_layer(&mut headers, &get_identity_headers(target, identity));
    }

    headers.set("accept", "application/json");
    headers.set("content-type", "application/json");
    headers.set("anthropic-version", "2023-06-01");
    headers.set("anthropic-dangerous-direct-browser-access", "true");

    let mut is_oauth = false;

    if model.provider == "github-copilot" {
        if let Some(key) = api_key.filter(|k| !k.is_empty()) {
            headers.set("authorization", &format!("Bearer {key}"));
        }
        if let Some(mh) = &model.headers {
            apply_raw_header_layer(&mut headers, mh);
        }
        if let Some(dyn_h) = dynamic_headers {
            apply_raw_header_layer(&mut headers, dyn_h);
        }
        if let Some(oh) = options_headers {
            apply_header_layer(&mut headers, oh);
        }
    } else if let Some(key) = api_key.filter(|k| is_oauth_token(k)) {
        is_oauth = true;
        headers.set("authorization", &format!("Bearer {key}"));
        headers.set("user-agent", &format!("claude-cli/{CLAUDE_CODE_VERSION}"));
        headers.set("x-app", "cli");
        if let Some(mh) = &model.headers {
            apply_raw_header_layer(&mut headers, mh);
        }
        if let Some(oh) = options_headers {
            apply_header_layer(&mut headers, oh);
        }
    } else if let Some(fed_token) = federation_token {
        let compat = get_anthropic_compat(model);
        if let Some(sid) = session_id {
            if compat.send_session_affinity_headers {
                let name = match compat.session_affinity_format {
                    Some(SessionAffinityFormat::OpenRouter) => "x-session-id",
                    _ => "x-session-affinity",
                };
                headers.set(name, sid);
            }
        }
        headers.set("authorization", &format!("Bearer {fed_token}"));
        if let Some(mh) = &model.headers {
            apply_raw_header_layer(&mut headers, mh);
        }
        if let Some(oh) = options_headers {
            apply_header_layer(&mut headers, oh);
        }
    } else {
        let compat = get_anthropic_compat(model);
        if let Some(sid) = session_id {
            if compat.send_session_affinity_headers {
                let name = match compat.session_affinity_format {
                    Some(SessionAffinityFormat::OpenRouter) => "x-session-id",
                    _ => "x-session-affinity",
                };
                headers.set(name, sid);
            }
        }
        if let Some(key) = api_key.filter(|k| !k.is_empty()) {
            headers.set("x-api-key", key);
        }
        if let Some(mh) = &model.headers {
            apply_raw_header_layer(&mut headers, mh);
        }
        if let Some(oh) = options_headers {
            apply_header_layer(&mut headers, oh);
        }
    }

    if !betas.is_empty() {
        headers.set("anthropic-beta", &betas.join(","));
    }

    if federation_token.is_some() {
        let mut features: IndexSet<String> = IndexSet::new();
        if let Some(existing) = headers.get("anthropic-beta") {
            for part in existing.split(',') {
                let trimmed = pi_js::str16::trim(part);
                if !trimmed.is_empty() {
                    features.insert(trimmed.to_string());
                }
            }
        }
        features.insert(OAUTH_API_BETA_HEADER.to_string());
        let joined: Vec<&str> = features.iter().map(String::as_str).collect();
        headers.set("anthropic-beta", &joined.join(","));
    }

    (headers, is_oauth)
}

fn should_use_server_side_fallback_beta(model: &Model) -> bool {
    get_anthropic_compat(model)
        .allowed_fallback_models
        .is_some_and(|list| !list.is_empty())
}

fn should_use_fine_grained_tool_streaming_beta(model: &Model, context: &TranscriptContext) -> bool {
    !get_current_tools(&context.messages).is_empty() && !get_anthropic_compat(model).supports_eager_tool_input_streaming
}

struct BetaFeatureFlags {
    is_oauth: bool,
    native_tool_changes: bool,
    message_threads: bool,
}

fn get_beta_features(
    model: &Model,
    context: &TranscriptContext,
    options: &AnthropicOptions,
    flags: BetaFeatureFlags,
) -> Vec<String> {
    let mut has_configured = false;
    let mut configured_value: Option<String> = None;

    if let Some(headers) = model.headers.as_ref() {
        for (name, value) in headers {
            if name.to_ascii_lowercase() == "anthropic-beta" {
                has_configured = true;
                configured_value = Some(value.clone());
            }
        }
    }
    if let Some(headers) = options.base.headers.as_ref() {
        for (name, value) in headers {
            if name.to_ascii_lowercase() == "anthropic-beta" {
                has_configured = true;
                configured_value = value.clone();
            }
        }
    }

    if has_configured {
        let Some(raw) = configured_value else {
            return Vec::new();
        };
        let mut set = IndexSet::new();
        for feature in raw.split(',') {
            let trimmed = pi_js::str16::trim(feature);
            if !trimmed.is_empty() {
                set.insert(trimmed.to_string());
            }
        }
        return set.into_iter().collect();
    }

    let compat = get_anthropic_compat(model);
    let mut set = IndexSet::new();
    if flags.is_oauth {
        set.insert("claude-code-20250219".to_string());
        set.insert(OAUTH_API_BETA_HEADER.to_string());
    }
    if should_use_fine_grained_tool_streaming_beta(model, context) {
        set.insert(FINE_GRAINED_TOOL_STREAMING_BETA.to_string());
    }
    if model.reasoning
        && options.thinking_enabled == Some(true)
        && options.interleaved_thinking.unwrap_or(true)
        && !compat.force_adaptive_thinking
    {
        set.insert(INTERLEAVED_THINKING_BETA.to_string());
    }
    if should_use_server_side_fallback_beta(model) {
        set.insert(SERVER_SIDE_FALLBACK_BETA.to_string());
    }
    if compat.supports_mid_convo_effort {
        set.insert(MID_CONVERSATION_OUTPUT_CONFIG_BETA.to_string());
        set.insert(THINKING_BINDING_CONTROLS_BETA.to_string());
    }
    if flags.native_tool_changes {
        set.insert(INLINE_TOOLS_BETA.to_string());
    }
    if flags.message_threads {
        set.insert(MESSAGE_THREADS_BETA.to_string());
    }
    set.into_iter().collect()
}

fn convert_image_block(img: &ImageContent) -> Value {
    serde_json::json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": img.mime_type,
            "data": img.data,
        }
    })
}

fn convert_content_blocks(content: &[UserContentBlock]) -> Value {
    let has_images = content.iter().any(|c| matches!(c, UserContentBlock::Image(_)));
    if !has_images {
        let joined: Vec<&str> = content
            .iter()
            .filter_map(|c| match c {
                UserContentBlock::Text(t) => Some(t.text.as_str()),
                UserContentBlock::Image(_) => None,
            })
            .collect();
        return Value::String(sanitize_surrogates(&joined.join("\n")));
    }

    let mut blocks: Vec<Value> = content
        .iter()
        .map(|block| match block {
            UserContentBlock::Text(t) => serde_json::json!({
                "type": "text",
                "text": sanitize_surrogates(&t.text),
            }),
            UserContentBlock::Image(img) => convert_image_block(img),
        })
        .collect();

    let has_text = blocks
        .iter()
        .any(|b| b.get("type").and_then(Value::as_str) == Some("text"));
    if !has_text {
        blocks.insert(
            0,
            serde_json::json!({
                "type": "text",
                "text": "(see attached image)",
            }),
        );
    }

    Value::Array(blocks)
}

fn convert_tool_result_with_content(msg: &ToolResultMessage, content: &[UserContentBlock]) -> Value {
    serde_json::json!({
        "type": "tool_result",
        "tool_use_id": msg.tool_call_id,
        "content": convert_content_blocks(content),
        "is_error": msg.is_error,
    })
}

fn normalize_tool_call_id(id: &str, _model: &Model, _assistant: &AssistantMessage) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

struct ConvertedAnthropicMessages {
    messages: Vec<Value>,
    assistant_levels: IndexMap<usize, AnthropicEffort>,
}

#[derive(Clone, Copy)]
struct AnthropicInlineToolConfig {
    supports_eager_tool_input_streaming: bool,
    supports_strict_tools: bool,
    disable_preferred_strict_tools: bool,
}

fn convert_messages(
    transformed_messages: &[Message],
    is_oauth: bool,
    cache_control: Option<&Value>,
    allow_empty_signature: bool,
    managed_provider: Option<&str>,
    inline_tools: Option<AnthropicInlineToolConfig>,
    has_preferred_strict_tools: &mut bool,
) -> Result<ConvertedAnthropicMessages> {
    let mut params: Vec<Value> = Vec::new();
    let mut assistant_levels: IndexMap<usize, AnthropicEffort> = IndexMap::new();
    let mut pending_system_messages: Vec<Value> = Vec::new();

    let mut i = 0;
    while i < transformed_messages.len() {
        let msg = &transformed_messages[i];
        match msg {
            Message::System(sys) => {
                let text = render_system_message_update(sys);
                let mut blocks: Vec<Value> = Vec::new();
                if !text.is_empty() {
                    blocks.push(serde_json::json!({
                        "type": "text",
                        "text": sanitize_surrogates(&text),
                    }));
                }
                if let Some(cfg) = inline_tools {
                    let added = sys.tools_added.as_deref().unwrap_or(&[]);
                    let redefined: IndexSet<&str> = added.iter().map(|t| t.name.as_str()).collect();
                    if let Some(removed) = &sys.tools_removed {
                        for tool in removed {
                            // A new definition under the same name replaces the old one, so no removal is needed.
                            if redefined.contains(tool.name.as_str()) {
                                continue;
                            }
                            let name = if is_oauth {
                                to_claude_code_name(&tool.name)
                            } else {
                                tool.name.clone()
                            };
                            blocks.push(serde_json::json!({
                                "type": "tool_removal",
                                "tool": { "type": "tool_reference", "name": name },
                            }));
                        }
                    }
                    let (definitions, added_has_preferred) = convert_tools(
                        added,
                        is_oauth,
                        cfg.supports_eager_tool_input_streaming,
                        cfg.supports_strict_tools,
                        None,
                        cfg.disable_preferred_strict_tools,
                    )?;
                    if added_has_preferred {
                        *has_preferred_strict_tools = true;
                    }
                    for definition in definitions {
                        blocks.push(serde_json::json!({
                            "type": "tool_addition",
                            "tool": { "type": "tool_definition", "definition": definition },
                        }));
                    }
                }
                if !blocks.is_empty() {
                    pending_system_messages.push(serde_json::json!({
                        "role": "system",
                        "content": blocks,
                    }));
                }
            }
            Message::User(user) => match &user.content {
                UserMessageContent::Text(text) => {
                    if !pi_js::str16::trim(text).is_empty() {
                        params.push(serde_json::json!({
                            "role": "user",
                            "content": sanitize_surrogates(text),
                        }));
                    }
                }
                UserMessageContent::Blocks(items) => {
                    let filtered_blocks: Vec<Value> = items
                        .iter()
                        .filter_map(|item| match item {
                            UserContentBlock::Text(t) => {
                                let cleaned = sanitize_surrogates(&t.text);
                                if pi_js::str16::trim(&cleaned).is_empty() {
                                    None
                                } else {
                                    Some(serde_json::json!({
                                        "type": "text",
                                        "text": cleaned,
                                    }))
                                }
                            }
                            UserContentBlock::Image(img) => Some(convert_image_block(img)),
                        })
                        .collect();
                    if !filtered_blocks.is_empty() {
                        params.push(serde_json::json!({
                            "role": "user",
                            "content": filtered_blocks,
                        }));
                    }
                }
            },
            Message::Assistant(assistant) => {
                let mut blocks: Vec<Value> = Vec::new();
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Text(t) => {
                            if pi_js::str16::trim(&t.text).is_empty() {
                                continue;
                            }
                            blocks.push(serde_json::json!({
                                "type": "text",
                                "text": sanitize_surrogates(&t.text),
                            }));
                        }
                        AssistantContentBlock::Thinking(th) => {
                            if th.redacted == Some(true) {
                                blocks.push(serde_json::json!({
                                    "type": "redacted_thinking",
                                    "data": th.thinking_signature.as_deref().unwrap_or(""),
                                }));
                                continue;
                            }
                            let sig = th.thinking_signature.as_deref().unwrap_or("");
                            let has_sig = !pi_js::str16::trim(sig).is_empty();
                            if pi_js::str16::trim(&th.thinking).is_empty() && !has_sig {
                                continue;
                            }
                            if !has_sig {
                                if allow_empty_signature {
                                    blocks.push(serde_json::json!({
                                        "type": "thinking",
                                        "thinking": sanitize_surrogates(&th.thinking),
                                        "signature": "",
                                    }));
                                } else {
                                    blocks.push(serde_json::json!({
                                        "type": "text",
                                        "text": sanitize_surrogates(&th.thinking),
                                    }));
                                }
                            } else {
                                blocks.push(serde_json::json!({
                                    "type": "thinking",
                                    "thinking": sanitize_surrogates(&th.thinking),
                                    "signature": sig,
                                }));
                            }
                        }
                        AssistantContentBlock::ToolCall(tc) => {
                            let name = if is_oauth {
                                to_claude_code_name(&tc.name)
                            } else {
                                tc.name.clone()
                            };
                            let input =
                                Value::Object(tc.arguments.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
                            blocks.push(serde_json::json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": name,
                                "input": input,
                            }));
                        }
                    }
                }
                if !blocks.is_empty() {
                    // Hold pending system updates past empty assistant blocks (#11091).
                    params.append(&mut pending_system_messages);
                    let message_index = params.len();
                    params.push(serde_json::json!({
                        "role": "assistant",
                        "content": blocks,
                    }));
                    if let Some(mp) = managed_provider {
                        if assistant.api == "anthropic-messages" && assistant.provider == mp {
                            if let Some(ref level) = assistant.provider_thinking_level {
                                if let Some(effort) = AnthropicEffort::from_str_opt(level.as_str()) {
                                    assistant_levels.insert(message_index, effort);
                                }
                            }
                        }
                    }
                }
            }
            Message::ToolResult(_) => {
                let mut tool_results: Vec<Value> = Vec::new();
                // Anthropic rejects error tool results with non-text content; images follow tool_results (#10794).
                let mut error_images: Vec<Value> = Vec::new();
                let mut j = i;
                while j < transformed_messages.len() {
                    if let Message::ToolResult(tr) = &transformed_messages[j] {
                        let split = split_error_tool_result_images(tr);
                        let content_slice = split.as_ref().map(|s| s.content.as_slice()).unwrap_or(&tr.content);
                        tool_results.push(convert_tool_result_with_content(tr, content_slice));
                        if let Some(split) = split {
                            error_images.push(serde_json::json!({ "type": "text", "text": split.caption }));
                            for img in split.images {
                                error_images.push(convert_image_block(&img));
                            }
                        }
                        j += 1;
                    } else {
                        break;
                    }
                }
                i = j - 1;
                tool_results.extend(error_images);
                params.push(serde_json::json!({
                    "role": "user",
                    "content": tool_results,
                }));
            }
        }
        i += 1;
    }

    params.append(&mut pending_system_messages);

    if let (Some(cc), Some(last_msg)) = (cache_control, params.last_mut()) {
        if let Some(obj) = last_msg.as_object_mut() {
            let role = obj.get("role").and_then(Value::as_str);
            if matches!(role, Some("user" | "system")) {
                if let Some(content) = obj.get_mut("content") {
                    if let Some(arr) = content.as_array_mut() {
                        if let Some(last_block) = arr.last_mut().and_then(Value::as_object_mut) {
                            let block_type = last_block.get("type").and_then(Value::as_str);
                            if matches!(
                                block_type,
                                Some("text" | "image" | "tool_result" | "tool_addition" | "tool_removal")
                            ) {
                                last_block.insert("cache_control".to_string(), cc.clone());
                            }
                        }
                    } else if let Some(text) = content.as_str() {
                        let text_owned = text.to_string();
                        *content = Value::Array(vec![serde_json::json!({
                            "type": "text",
                            "text": text_owned,
                            "cache_control": cc,
                        })]);
                    }
                }
            }
        }
    }

    Ok(ConvertedAnthropicMessages {
        messages: params,
        assistant_levels,
    })
}

fn insert_thinking_level_messages(converted: ConvertedAnthropicMessages, active_effort: AnthropicEffort) -> Vec<Value> {
    let mut messages = Vec::with_capacity(converted.messages.len() + converted.assistant_levels.len() + 1);
    for (index, msg) in converted.messages.into_iter().enumerate() {
        if let Some(historical_effort) = converted.assistant_levels.get(&index) {
            messages.push(serde_json::json!({
                "role": "system",
                "content": [],
                "output_config": { "effort": historical_effort.as_str() },
            }));
        }
        messages.push(msg);
    }
    messages.push(serde_json::json!({
        "role": "system",
        "content": [],
        "output_config": { "effort": active_effort.as_str() },
    }));
    messages
}

fn is_anthropic_strict_unsupported_keyword(key: &str, value: &JsonValue) -> bool {
    const UNSUPPORTED_KEYS: [&str; 11] = [
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
        "maxItems",
        "uniqueItems",
        "minContains",
        "maxContains",
        "minProperties",
        "maxProperties",
    ];
    const STRICT_STRING_FORMATS: [&str; 10] = [
        "date-time",
        "time",
        "date",
        "duration",
        "email",
        "hostname",
        "uri",
        "ipv4",
        "ipv6",
        "uuid",
    ];
    if UNSUPPORTED_KEYS.contains(&key) {
        return true;
    }
    if key == "minItems" {
        return value.as_i64() != Some(0) && value.as_i64() != Some(1);
    }
    if key == "format" {
        return value.as_str().is_none_or(|s| !STRICT_STRING_FORMATS.contains(&s));
    }
    false
}

fn convert_tools(
    tools: &[Tool],
    is_oauth: bool,
    supports_eager_tool_input_streaming: bool,
    supports_strict_tools: bool,
    cache_control: Option<&Value>,
    disable_preferred_strict_tools: bool,
) -> Result<(Vec<Value>, bool)> {
    if tools.is_empty() {
        return Ok((Vec::new(), false));
    }

    let check: UnsupportedStrictSchemaKeywordCheck<'_> = &is_anthropic_strict_unsupported_keyword;
    let mut converted = Vec::with_capacity(tools.len());
    let mut has_preferred_strict_tools = false;

    for (index, tool) in tools.iter().enumerate() {
        let preferred = prefers_json_schema_strict_sampling(tool);
        let strict = if disable_preferred_strict_tools && preferred {
            None
        } else {
            resolve_json_schema_strict_sampling(tool, supports_strict_tools, Some(check))?
        };
        if strict == Some(true) && preferred {
            has_preferred_strict_tools = true;
        }
        let parameters = get_json_schema_tool_parameters(tool, strict)?;
        let param_value = parameters.to_value();
        let param_obj = param_value.as_object();

        let props = param_obj
            .and_then(|o| o.get("properties"))
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        let req = param_obj
            .and_then(|o| o.get("required"))
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));

        let mut input_schema_map = Map::new();
        if strict == Some(true) {
            if let Some(o) = param_obj {
                for (k, v) in o {
                    input_schema_map.insert(k.clone(), v.clone());
                }
            }
        } else if let Some(o) = param_obj {
            // Keep shared definitions so property $refs still resolve. Other root keywords stay out:
            // Anthropic rejects root anyOf/oneOf/allOf with a 400.
            if let Some(defs) = o.get("$defs") {
                input_schema_map.insert("$defs".to_string(), defs.clone());
            }
            if let Some(defs) = o.get("definitions") {
                input_schema_map.insert("definitions".to_string(), defs.clone());
            }
        }
        input_schema_map.insert("type".to_string(), Value::String("object".to_string()));
        input_schema_map.insert("properties".to_string(), props);
        input_schema_map.insert("required".to_string(), req);

        let mut tool_obj = Map::new();
        tool_obj.insert(
            "name".to_string(),
            Value::String(if is_oauth {
                to_claude_code_name(&tool.name)
            } else {
                tool.name.clone()
            }),
        );
        tool_obj.insert("description".to_string(), Value::String(tool.description.clone()));
        if supports_eager_tool_input_streaming {
            tool_obj.insert("eager_input_streaming".to_string(), Value::Bool(true));
        }
        if strict == Some(true) {
            tool_obj.insert("strict".to_string(), Value::Bool(true));
        }
        tool_obj.insert("input_schema".to_string(), Value::Object(input_schema_map));
        if index + 1 == tools.len() {
            if let Some(cc) = cache_control {
                tool_obj.insert("cache_control".to_string(), cc.clone());
            }
        }

        converted.push(Value::Object(tool_obj));
    }

    Ok((converted, has_preferred_strict_tools))
}

fn deferred_tool_placeholder() -> Value {
    serde_json::json!({
        "name": "__pi_deferred_placeholder__",
        "description": "Reserved placeholder. Never available. Never call this.",
        "input_schema": { "type": "object", "properties": {}, "required": [] },
        "defer_loading": true,
    })
}

struct BuildParamsFlags {
    disable_preferred_strict_tools: bool,
    message_threads: bool,
}

struct BuiltParams {
    params: Map<String, Value>,
    has_preferred_strict_tools: bool,
    provider_thinking_level: Option<String>,
}

fn build_params(
    model: &Model,
    context: &TranscriptContext,
    is_oauth: bool,
    options: &AnthropicOptions,
    flags: BuildParamsFlags,
) -> Result<BuiltParams> {
    let (_, cache_control) = get_cache_control(model, options.base.cache_retention, options.base.env.as_ref());
    let compat = get_anthropic_compat(model);
    let initial_system_message = get_initial_system_message(&context.messages);
    let initial_system_text = initial_system_message.map(get_system_message_text).unwrap_or_default();

    let transformed = transform_messages(&context.messages, model, Some(&normalize_tool_call_id));
    let conversation_messages = if initial_system_message.is_some() {
        transformed.get(1..).unwrap_or(&[])
    } else {
        &transformed[..]
    };

    let initial_tools = initial_system_message
        .and_then(|s| s.tools_added.as_deref())
        .unwrap_or(&[]);
    let native_tool_changes = compat.supports_mid_convo_system_messages
        && compat.supports_mid_convo_tool_changes
        && !initial_tools.is_empty();

    let off_disabled_in_map = model
        .thinking_level_map
        .as_ref()
        .and_then(|m| m.get(&ModelThinkingLevel::Off))
        .is_some_and(Option::is_none);
    let thinking_disabled = model.reasoning && options.thinking_enabled == Some(false) && !off_disabled_in_map;
    // Effort cannot change between messages while thinking is disabled, so "off" sends no effort markers.
    let effort_messages = compat.supports_mid_convo_effort && !thinking_disabled;

    let mut has_preferred_strict_tools = false;
    let inline_tools = native_tool_changes.then_some(AnthropicInlineToolConfig {
        supports_eager_tool_input_streaming: compat.supports_eager_tool_input_streaming,
        supports_strict_tools: compat.supports_strict_tools,
        disable_preferred_strict_tools: flags.disable_preferred_strict_tools,
    });

    let converted = convert_messages(
        conversation_messages,
        is_oauth,
        cache_control.as_ref(),
        compat.allow_empty_signature,
        if effort_messages {
            Some(model.provider.as_str())
        } else {
            None
        },
        inline_tools,
        &mut has_preferred_strict_tools,
    )?;

    let active_effort = options.effort.unwrap_or(AnthropicEffort::High);
    let beta_features = get_beta_features(
        model,
        context,
        options,
        BetaFeatureFlags {
            is_oauth,
            native_tool_changes,
            message_threads: flags.message_threads,
        },
    );
    let messages_val = if effort_messages {
        insert_thinking_level_messages(converted, active_effort)
    } else {
        converted.messages
    };

    let mut params = Map::new();
    params.insert("model".to_string(), Value::String(model.id.clone()));
    params.insert("messages".to_string(), Value::Array(messages_val));
    params.insert(
        "max_tokens".to_string(),
        Value::from(options.base.max_tokens.unwrap_or(model.max_tokens)),
    );
    params.insert("stream".to_string(), Value::Bool(true));
    if !beta_features.is_empty() {
        params.insert(
            "betas".to_string(),
            Value::Array(beta_features.into_iter().map(Value::String).collect()),
        );
    }

    if is_oauth {
        let mut system_blocks = vec![{
            let mut b = Map::new();
            b.insert("type".to_string(), Value::String("text".to_string()));
            b.insert(
                "text".to_string(),
                Value::String("You are Claude Code, Anthropic's official CLI for Claude.".to_string()),
            );
            if let Some(cc) = &cache_control {
                b.insert("cache_control".to_string(), cc.clone());
            }
            Value::Object(b)
        }];
        if !initial_system_text.is_empty() {
            let mut b = Map::new();
            b.insert("type".to_string(), Value::String("text".to_string()));
            b.insert(
                "text".to_string(),
                Value::String(sanitize_surrogates(&initial_system_text)),
            );
            if let Some(cc) = &cache_control {
                b.insert("cache_control".to_string(), cc.clone());
            }
            system_blocks.push(Value::Object(b));
        }
        params.insert("system".to_string(), Value::Array(system_blocks));
    } else if !initial_system_text.is_empty() {
        let mut b = Map::new();
        b.insert("type".to_string(), Value::String("text".to_string()));
        b.insert(
            "text".to_string(),
            Value::String(sanitize_surrogates(&initial_system_text)),
        );
        if let Some(cc) = &cache_control {
            b.insert("cache_control".to_string(), cc.clone());
        }
        params.insert("system".to_string(), Value::Array(vec![Value::Object(b)]));
    }

    if let Some(temperature) = options.base.temperature {
        if options.thinking_enabled != Some(true) && !compat.supports_mid_convo_effort && compat.supports_temperature {
            params.insert("temperature".to_string(), pi_js::json::number_value(temperature));
        }
    }

    let tool_cache_control = if compat.supports_cache_control_on_tools {
        cache_control.as_ref()
    } else {
        None
    };

    if native_tool_changes {
        // Initial tools stay active with the cache breakpoint on the last one, followed by the
        // placeholder. Later tools are defined by value in `tool_addition` blocks and withdrawn
        // by `tool_removal`, so the cached prefix survives every tool change.
        let (mut all_tools, initial_preferred) = convert_tools(
            initial_tools,
            is_oauth,
            compat.supports_eager_tool_input_streaming,
            compat.supports_strict_tools,
            tool_cache_control,
            flags.disable_preferred_strict_tools,
        )?;
        if initial_preferred {
            has_preferred_strict_tools = true;
        }
        all_tools.push(deferred_tool_placeholder());
        params.insert("tools".to_string(), Value::Array(all_tools));
    } else {
        let tools = get_current_tools(&context.messages);
        if !tools.is_empty() {
            let (converted_tools, tools_preferred) = convert_tools(
                &tools,
                is_oauth,
                compat.supports_eager_tool_input_streaming,
                compat.supports_strict_tools,
                tool_cache_control,
                flags.disable_preferred_strict_tools,
            )?;
            if tools_preferred {
                has_preferred_strict_tools = true;
            }
            params.insert("tools".to_string(), Value::Array(converted_tools));
        }
    }

    if thinking_disabled {
        // No effort is sent, so the model default applies. Anthropic rejects `block_binding` with disabled thinking.
        params.insert("thinking".to_string(), serde_json::json!({ "type": "disabled" }));
    } else if effort_messages {
        let display = options
            .thinking_display
            .unwrap_or(AnthropicThinkingDisplay::Summarized)
            .as_str();
        params.insert(
            "thinking".to_string(),
            serde_json::json!({
                "type": "adaptive",
                "display": display,
                "block_binding": { "prefix_mismatch_behavior": "drop_block" },
            }),
        );
        // Top-level effort stays fixed at "low" so org caps don't reject requests; per-reply markers set the level.
        params.insert("output_config".to_string(), serde_json::json!({ "effort": "low" }));
    } else if model.reasoning && options.thinking_enabled == Some(true) {
        let display = options
            .thinking_display
            .unwrap_or(AnthropicThinkingDisplay::Summarized)
            .as_str();
        if compat.force_adaptive_thinking {
            params.insert(
                "thinking".to_string(),
                serde_json::json!({
                    "type": "adaptive",
                    "display": display,
                }),
            );
            if let Some(effort) = options.effort {
                params.insert(
                    "output_config".to_string(),
                    serde_json::json!({ "effort": effort.as_str() }),
                );
            }
        } else {
            let budget = match options.thinking_budget_tokens {
                Some(b) if b != 0 => b,
                _ => 1024,
            };
            params.insert(
                "thinking".to_string(),
                serde_json::json!({
                    "type": "enabled",
                    "budget_tokens": budget,
                    "display": display,
                }),
            );
        }
    }

    if let Some(metadata) = &options.base.metadata {
        if let Some(user_id) = metadata.get("user_id").and_then(Value::as_str) {
            params.insert("metadata".to_string(), serde_json::json!({ "user_id": user_id }));
        }
    }

    if let Some(tool_choice) = &options.tool_choice {
        params.insert("tool_choice".to_string(), tool_choice.to_api_value());
    }

    if let Some(fallbacks) = compat.allowed_fallback_models {
        if !fallbacks.is_empty() {
            let arr: Vec<Value> = fallbacks
                .iter()
                .map(|f| serde_json::json!({ "model": f.model }))
                .collect();
            params.insert("fallbacks".to_string(), Value::Array(arr));
        }
    }

    Ok(BuiltParams {
        params,
        has_preferred_strict_tools,
        provider_thinking_level: effort_messages.then(|| active_effort.as_str().to_string()),
    })
}

#[derive(Default)]
struct SseDecoderState {
    event: Option<String>,
    data: Vec<String>,
    raw: Vec<String>,
}

struct ServerSentEvent {
    event: Option<String>,
    data: String,
    raw: Vec<String>,
}

fn flush_sse_event(state: &mut SseDecoderState) -> Option<ServerSentEvent> {
    if state.event.is_none() && state.data.is_empty() {
        return None;
    }
    let event = ServerSentEvent {
        event: state.event.take(),
        data: state.data.join("\n"),
        raw: std::mem::take(&mut state.raw),
    };
    state.data.clear();
    Some(event)
}

fn decode_sse_line(line: &str, state: &mut SseDecoderState) -> Option<ServerSentEvent> {
    if line.is_empty() {
        return flush_sse_event(state);
    }
    state.raw.push(line.to_string());
    if line.starts_with(':') {
        return None;
    }
    let (field_name, mut value) = match line.find(':') {
        Some(idx) => (&line[..idx], &line[idx + 1..]),
        None => (line, ""),
    };
    if let Some(stripped) = value.strip_prefix(' ') {
        value = stripped;
    }
    match field_name {
        "event" => state.event = Some(value.to_string()),
        "data" => state.data.push(value.to_string()),
        _ => {}
    }
    None
}

fn consume_sse_line(buffer: &mut String, flush: bool) -> Option<String> {
    let cr_idx = buffer.find('\r');
    let lf_idx = buffer.find('\n');
    let line_break_idx = match (cr_idx, lf_idx) {
        (None, None) => return None,
        (Some(cr), None) => {
            if !flush && cr + 1 == buffer.len() {
                return None;
            }
            cr
        }
        (None, Some(lf)) => lf,
        (Some(cr), Some(lf)) => cr.min(lf),
    };
    let line = buffer[..line_break_idx].to_string();
    let mut next_idx = line_break_idx + 1;
    if buffer.as_bytes().get(line_break_idx) == Some(&b'\r') && buffer.as_bytes().get(next_idx) == Some(&b'\n') {
        next_idx += 1;
    }
    buffer.drain(..next_idx);
    Some(line)
}

fn decode_utf8_chunk(pending: &mut Vec<u8>, chunk: &[u8], flush: bool) -> String {
    pending.extend_from_slice(chunk);
    let mut out = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(valid) => {
                out.push_str(valid);
                pending.clear();
                break;
            }
            Err(err) => {
                let valid_up_to = err.valid_up_to();
                if valid_up_to > 0 {
                    if let Ok(valid) = std::str::from_utf8(&pending[..valid_up_to]) {
                        out.push_str(valid);
                    }
                    pending.drain(..valid_up_to);
                }
                if let Some(error_len) = err.error_len() {
                    out.push('\u{FFFD}');
                    pending.drain(..error_len);
                } else if flush {
                    out.push('\u{FFFD}');
                    pending.clear();
                    break;
                } else {
                    break;
                }
            }
        }
    }
    out
}

fn is_anthropic_message_event(event: Option<&str>) -> bool {
    matches!(
        event,
        Some(
            "message_start"
                | "message_delta"
                | "message_stop"
                | "content_block_start"
                | "content_block_delta"
                | "content_block_stop"
        )
    )
}

async fn read_all_sse_events(response: Response, signal: Option<&AbortSignal>) -> Result<Vec<ServerSentEvent>> {
    if !response.has_body() {
        return Err(Error::msg(
            "Attempted to iterate over an Anthropic response with no body",
        ));
    }
    let mut body_stream = response.body_stream()?;
    let mut pending_utf8 = Vec::new();
    let mut text_buffer = String::new();
    let mut state = SseDecoderState::default();
    let mut events = Vec::new();

    while let Some(chunk_res) = body_stream.next().await {
        if signal.is_some_and(|s| s.aborted()) {
            return Err(Error::msg("Request was aborted"));
        }
        let chunk = chunk_res?;
        text_buffer.push_str(&decode_utf8_chunk(&mut pending_utf8, &chunk, false));
        while let Some(line) = consume_sse_line(&mut text_buffer, false) {
            if let Some(ev) = decode_sse_line(&line, &mut state) {
                events.push(ev);
            }
        }
    }

    text_buffer.push_str(&decode_utf8_chunk(&mut pending_utf8, &[], true));
    while let Some(line) = consume_sse_line(&mut text_buffer, true) {
        if let Some(ev) = decode_sse_line(&line, &mut state) {
            events.push(ev);
        }
    }
    if !text_buffer.is_empty() {
        if let Some(ev) = decode_sse_line(&text_buffer, &mut state) {
            events.push(ev);
        }
    }
    if let Some(ev) = flush_sse_event(&mut state) {
        events.push(ev);
    }

    Ok(events)
}

fn map_stop_reason(reason: &str, stop_details: Option<&Value>) -> Result<(StopReason, Option<String>)> {
    match reason {
        "end_turn" | "pause_turn" | "stop_sequence" => Ok((StopReason::Stop, None)),
        "max_tokens" => Ok((StopReason::Length, None)),
        "tool_use" => Ok((StopReason::ToolUse, None)),
        "refusal" => {
            let explanation = stop_details
                .and_then(|d| d.get("explanation"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("The model refused to complete the request");
            Ok((StopReason::Error, Some(explanation.to_string())))
        }
        "sensitive" => Ok((StopReason::Error, Some("Provider stopped with: sensitive".to_string()))),
        other => Err(Error::msg(format!("Unhandled stop reason: {other}"))),
    }
}

struct OpenBlock {
    content_index: usize,
    arguments: Option<ToolCallArgumentsStream>,
}

type OpenBlocks = IndexMap<usize, OpenBlock>;

fn apply_anthropic_event(
    event: &Value,
    model: &Model,
    usage_model: &mut Model,
    is_oauth: bool,
    current_tools: &[Tool],
    output: &mut AssistantMessage,
    open_blocks: &mut OpenBlocks,
    input_transformations: &mut Option<Vec<Value>>,
    event_stream: &AssistantMessageEventStream,
) -> Result<()> {
    let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
    match event_type {
        "message_start" => {
            if let Some(msg) = event.get("message").and_then(Value::as_object) {
                if let Some(id) = msg.get("id").and_then(Value::as_str) {
                    output.response_id = Some(id.to_string());
                }
                if let Some(transformations) = msg.get("input_transformations").and_then(Value::as_array) {
                    *input_transformations = Some(transformations.clone());
                }
                if let Some(response_model) = msg.get("model").and_then(Value::as_str) {
                    if response_model != model.id {
                        output.response_model = Some(response_model.to_string());
                        let compat = get_anthropic_compat(model);
                        if let Some(fallback_cost) = compat.allowed_fallback_models.and_then(|list| {
                            list.iter()
                                .find(|f| f.provider == model.provider && f.model == response_model)
                                .map(|f| f.cost.clone())
                        }) {
                            let mut next = model.clone();
                            next.id = response_model.to_string();
                            next.cost = fallback_cost;
                            *usage_model = next;
                        } else {
                            *usage_model = model.clone();
                        }
                    } else {
                        *usage_model = model.clone();
                    }
                }
                if let Some(usage) = msg.get("usage").and_then(Value::as_object) {
                    output.usage.input = usage.get("input_tokens").and_then(Value::as_i64).unwrap_or(0);
                    output.usage.output = usage.get("output_tokens").and_then(Value::as_i64).unwrap_or(0);
                    output.usage.cache_read = usage
                        .get("cache_read_input_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    output.usage.cache_write = usage
                        .get("cache_creation_input_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    let write_1h = usage
                        .get("cache_creation")
                        .and_then(Value::as_object)
                        .and_then(|cc| cc.get("ephemeral_1h_input_tokens"))
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    output.usage.cache_write_1h = Some(write_1h);
                    output.usage.total_tokens =
                        output.usage.input + output.usage.output + output.usage.cache_read + output.usage.cache_write;
                    calculate_cost(&AnyModel::Chat(usage_model.clone()), &mut output.usage);
                }
            }
        }
        "content_block_start" => {
            let sse_index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let Some(cb) = event.get("content_block").and_then(Value::as_object) else {
                return Ok(());
            };
            let cb_type = cb.get("type").and_then(Value::as_str).unwrap_or("");
            match cb_type {
                "fallback" => {
                    if !output.content.is_empty() {
                        return Err(Error::msg(
                            "Anthropic performed an unsupported mid-output model fallback",
                        ));
                    }
                }
                "text" => {
                    let text = cb.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                    let content_index = output.content.len();
                    output.content.push(AssistantContentBlock::Text(TextContent::new(text)));
                    open_blocks.insert(
                        sse_index,
                        OpenBlock {
                            content_index,
                            arguments: None,
                        },
                    );
                    event_stream.push(AssistantMessageEvent::TextStart {
                        content_index,
                        partial: output.clone(),
                    });
                }
                "thinking" => {
                    let thinking = cb.get("thinking").and_then(Value::as_str).unwrap_or("").to_string();
                    let sig = cb.get("signature").and_then(Value::as_str).unwrap_or("").to_string();
                    let content_index = output.content.len();
                    output.content.push(AssistantContentBlock::Thinking(ThinkingContent {
                        thinking,
                        thinking_signature: Some(sig),
                        redacted: None,
                    }));
                    open_blocks.insert(
                        sse_index,
                        OpenBlock {
                            content_index,
                            arguments: None,
                        },
                    );
                    event_stream.push(AssistantMessageEvent::ThinkingStart {
                        content_index,
                        partial: output.clone(),
                    });
                }
                "redacted_thinking" => {
                    let data = cb.get("data").and_then(Value::as_str).unwrap_or("").to_string();
                    let content_index = output.content.len();
                    output.content.push(AssistantContentBlock::Thinking(ThinkingContent {
                        thinking: "[Reasoning redacted]".to_string(),
                        thinking_signature: Some(data),
                        redacted: Some(true),
                    }));
                    open_blocks.insert(
                        sse_index,
                        OpenBlock {
                            content_index,
                            arguments: None,
                        },
                    );
                    event_stream.push(AssistantMessageEvent::ThinkingStart {
                        content_index,
                        partial: output.clone(),
                    });
                }
                "tool_use" => {
                    let id = cb.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                    let raw_name = cb.get("name").and_then(Value::as_str).unwrap_or("");
                    let name = if is_oauth {
                        from_claude_code_name(raw_name, current_tools)
                    } else {
                        raw_name.to_string()
                    };
                    let arguments: JsonObject = cb
                        .get("input")
                        .and_then(Value::as_object)
                        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                        .unwrap_or_default();
                    let content_index = output.content.len();
                    output.content.push(AssistantContentBlock::ToolCall(ToolCall {
                        id,
                        name,
                        arguments,
                        arguments_error: None,
                        thought_signature: None,
                        namespace: None,
                    }));
                    open_blocks.insert(
                        sse_index,
                        OpenBlock {
                            content_index,
                            arguments: Some(ToolCallArgumentsStream::new()),
                        },
                    );
                    event_stream.push(AssistantMessageEvent::ToolCallStart {
                        content_index,
                        partial: output.clone(),
                    });
                }
                _ => {}
            }
        }
        "content_block_delta" => {
            let sse_index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let Some(open) = open_blocks.get_mut(&sse_index) else {
                return Ok(());
            };
            let content_index = open.content_index;
            let Some(delta) = event.get("delta").and_then(Value::as_object) else {
                return Ok(());
            };
            let delta_type = delta.get("type").and_then(Value::as_str).unwrap_or("");
            match delta_type {
                "text_delta" => {
                    let text_delta = delta.get("text").and_then(Value::as_str).unwrap_or("");
                    if let Some(AssistantContentBlock::Text(t)) = output.content.get_mut(content_index) {
                        t.text.push_str(text_delta);
                        event_stream.push(AssistantMessageEvent::TextDelta {
                            content_index,
                            delta: text_delta.to_string(),
                            partial: output.clone(),
                        });
                    }
                }
                "thinking_delta" => {
                    let thinking_delta = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
                    if let Some(AssistantContentBlock::Thinking(th)) = output.content.get_mut(content_index) {
                        th.thinking.push_str(thinking_delta);
                        event_stream.push(AssistantMessageEvent::ThinkingDelta {
                            content_index,
                            delta: thinking_delta.to_string(),
                            partial: output.clone(),
                        });
                    }
                }
                "input_json_delta" => {
                    let partial_delta = delta.get("partial_json").and_then(Value::as_str).unwrap_or("");
                    if let Some(args) = open.arguments.as_mut()
                        && let Some(AssistantContentBlock::ToolCall(tc)) = output.content.get_mut(content_index)
                    {
                        tc.arguments = args.append(partial_delta);
                        event_stream.push(AssistantMessageEvent::ToolCallDelta {
                            content_index,
                            delta: partial_delta.to_string(),
                            partial: output.clone(),
                        });
                    }
                }
                "signature_delta" => {
                    let sig_delta = delta.get("signature").and_then(Value::as_str).unwrap_or("");
                    if let Some(AssistantContentBlock::Thinking(th)) = output.content.get_mut(content_index) {
                        th.thinking_signature
                            .get_or_insert_with(String::new)
                            .push_str(sig_delta);
                    }
                }
                _ => {}
            }
        }
        "content_block_stop" => {
            let sse_index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let Some(mut open) = open_blocks.swap_remove(&sse_index) else {
                return Ok(());
            };
            let content_index = open.content_index;
            enum StopBlock {
                Text(String),
                Thinking(String),
                ToolCall(ToolCall),
            }
            let stop_block = match output.content.get_mut(content_index) {
                Some(AssistantContentBlock::Text(t)) => Some(StopBlock::Text(t.text.clone())),
                Some(AssistantContentBlock::Thinking(th)) => Some(StopBlock::Thinking(th.thinking.clone())),
                Some(AssistantContentBlock::ToolCall(tc)) => {
                    if let Some(args) = open.arguments.take() {
                        let parsed = args.finish();
                        tc.arguments = parsed.arguments;
                        tc.arguments_error = parsed.arguments_error;
                    }
                    Some(StopBlock::ToolCall(tc.clone()))
                }
                None => None,
            };
            match stop_block {
                Some(StopBlock::Text(content)) => {
                    event_stream.push(AssistantMessageEvent::TextEnd {
                        content_index,
                        content,
                        partial: output.clone(),
                    });
                }
                Some(StopBlock::Thinking(content)) => {
                    event_stream.push(AssistantMessageEvent::ThinkingEnd {
                        content_index,
                        content,
                        partial: output.clone(),
                    });
                }
                Some(StopBlock::ToolCall(tool_call)) => {
                    event_stream.push(AssistantMessageEvent::ToolCallEnd {
                        content_index,
                        tool_call,
                        partial: output.clone(),
                    });
                }
                None => {}
            }
        }
        "message_delta" => {
            if let Some(transformations) = event.get("input_transformations").and_then(Value::as_array) {
                *input_transformations = Some(transformations.clone());
            }
            if let Some(delta) = event.get("delta").and_then(Value::as_object) {
                if let Some(raw_reason) = delta.get("stop_reason").and_then(Value::as_str) {
                    output.raw_stop_reason = Some(raw_reason.to_string());
                    let stop_details = delta.get("stop_details");
                    let (stop_reason, err_msg) = map_stop_reason(raw_reason, stop_details)?;
                    output.stop_reason = stop_reason;
                    if let Some(msg) = err_msg {
                        output.error_message = Some(msg);
                    }
                }
            }
            if let Some(usage) = event.get("usage").and_then(Value::as_object) {
                if let Some(v) = usage.get("input_tokens").and_then(Value::as_i64) {
                    output.usage.input = v;
                }
                if let Some(v) = usage.get("output_tokens").and_then(Value::as_i64) {
                    output.usage.output = v;
                }
                if let Some(v) = usage.get("cache_read_input_tokens").and_then(Value::as_i64) {
                    output.usage.cache_read = v;
                }
                if let Some(v) = usage.get("cache_creation_input_tokens").and_then(Value::as_i64) {
                    output.usage.cache_write = v;
                }
                if let Some(v) = usage
                    .get("cache_creation")
                    .and_then(Value::as_object)
                    .and_then(|cc| cc.get("ephemeral_1h_input_tokens"))
                    .and_then(Value::as_i64)
                {
                    output.usage.cache_write_1h = Some(v);
                }
                if let Some(v) = usage
                    .get("output_tokens_details")
                    .and_then(Value::as_object)
                    .and_then(|d| d.get("thinking_tokens"))
                    .and_then(Value::as_i64)
                {
                    output.usage.reasoning = Some(v);
                }
            }
            output.usage.total_tokens =
                output.usage.input + output.usage.output + output.usage.cache_read + output.usage.cache_write;
            calculate_cost(&AnyModel::Chat(usage_model.clone()), &mut output.usage);
            if is_openrouter_model(model)
                && let Some(usage) = event.get("usage").and_then(Value::as_object)
            {
                let cost = usage.get("cost").and_then(Value::as_f64);
                let is_byok = usage.get("is_byok").and_then(Value::as_bool).unwrap_or(false);
                let cost_details =
                    usage
                        .get("cost_details")
                        .and_then(Value::as_object)
                        .map(|d| OpenRouterCostDetails {
                            upstream_inference_cost: d.get("upstream_inference_cost").and_then(Value::as_f64),
                            upstream_inference_prompt_cost: d
                                .get("upstream_inference_prompt_cost")
                                .and_then(Value::as_f64)
                                .unwrap_or(f64::NAN),
                            upstream_inference_completions_cost: d
                                .get("upstream_inference_completions_cost")
                                .and_then(Value::as_f64)
                                .unwrap_or(f64::NAN),
                            server_tool_cost: d.get("server_tool_cost").and_then(Value::as_f64),
                        });
                apply_openrouter_reported_cost(
                    &mut output.usage,
                    &OpenRouterUsageAccounting {
                        cost,
                        is_byok,
                        cost_details,
                    },
                );
            }
        }
        _ => {}
    }
    Ok(())
}

async fn iterate_and_process_sse(
    response: Response,
    model: &Model,
    usage_model: &mut Model,
    is_oauth: bool,
    current_tools: &[Tool],
    options: &AnthropicOptions,
    output: &mut AssistantMessage,
    input_transformations: &mut Option<Vec<Value>>,
    event_stream: &AssistantMessageEventStream,
) -> Result<()> {
    let signal = options.base.signal.as_ref();
    if !response.has_body() {
        return Err(Error::msg(
            "Attempted to iterate over an Anthropic response with no body",
        ));
    }
    let mut body_stream = response.body_stream()?;
    let mut pending_utf8 = Vec::new();
    let mut text_buffer = String::new();
    let mut state = SseDecoderState::default();
    let mut saw_message_start = false;
    let mut saw_message_end = false;
    let mut open_blocks = OpenBlocks::new();

    let mut handle_sse = |sse: ServerSentEvent| -> Result<Option<Value>> {
        if sse.event.as_deref() == Some("error") {
            return Err(Error::msg(sse.data));
        }
        if !is_anthropic_message_event(sse.event.as_deref()) {
            return Ok(None);
        }
        match parse_json_with_repair(&sse.data) {
            Ok(parsed) => {
                match parsed.get("type").and_then(Value::as_str) {
                    Some("message_start") => saw_message_start = true,
                    Some("message_stop") => saw_message_end = true,
                    _ => {}
                }
                Ok(Some(parsed))
            }
            Err(err) => {
                let ev_name = sse.event.as_deref().unwrap_or("null");
                Err(Error::msg(format!(
                    "Could not parse Anthropic SSE event {ev_name}: {err}; data={}; raw={}",
                    sse.data,
                    sse.raw.join("\\n")
                )))
            }
        }
    };

    while let Some(chunk_res) = body_stream.next().await {
        if signal.is_some_and(|s| s.aborted()) {
            return Err(Error::msg("Request was aborted"));
        }
        let chunk = chunk_res?;
        text_buffer.push_str(&decode_utf8_chunk(&mut pending_utf8, &chunk, false));
        while let Some(line) = consume_sse_line(&mut text_buffer, false) {
            if let Some(sse) = decode_sse_line(&line, &mut state) {
                if let Some(parsed) = handle_sse(sse)? {
                    if let Some(on_event) = &options.base.on_provider_stream_event {
                        on_event(&parsed, model).await?;
                    }
                    apply_anthropic_event(
                        &parsed,
                        model,
                        usage_model,
                        is_oauth,
                        current_tools,
                        output,
                        &mut open_blocks,
                        input_transformations,
                        event_stream,
                    )?;
                }
            }
        }
    }

    text_buffer.push_str(&decode_utf8_chunk(&mut pending_utf8, &[], true));
    while let Some(line) = consume_sse_line(&mut text_buffer, true) {
        if let Some(sse) = decode_sse_line(&line, &mut state) {
            if let Some(parsed) = handle_sse(sse)? {
                if let Some(on_event) = &options.base.on_provider_stream_event {
                    on_event(&parsed, model).await?;
                }
                apply_anthropic_event(
                    &parsed,
                    model,
                    usage_model,
                    is_oauth,
                    current_tools,
                    output,
                    &mut open_blocks,
                    input_transformations,
                    event_stream,
                )?;
            }
        }
    }
    if !text_buffer.is_empty() {
        if let Some(sse) = decode_sse_line(&text_buffer, &mut state) {
            if let Some(parsed) = handle_sse(sse)? {
                if let Some(on_event) = &options.base.on_provider_stream_event {
                    on_event(&parsed, model).await?;
                }
                apply_anthropic_event(
                    &parsed,
                    model,
                    usage_model,
                    is_oauth,
                    current_tools,
                    output,
                    &mut open_blocks,
                    input_transformations,
                    event_stream,
                )?;
            }
        }
    }
    if let Some(sse) = flush_sse_event(&mut state) {
        if let Some(parsed) = handle_sse(sse)? {
            if let Some(on_event) = &options.base.on_provider_stream_event {
                on_event(&parsed, model).await?;
            }
            apply_anthropic_event(
                &parsed,
                model,
                usage_model,
                is_oauth,
                current_tools,
                output,
                &mut open_blocks,
                input_transformations,
                event_stream,
            )?;
        }
    }

    if saw_message_start && !saw_message_end {
        return Err(Error::msg("Anthropic stream ended before message_stop"));
    }

    let _ = read_all_sse_events;
    Ok(())
}

fn to_model_thinking_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::XHigh => ModelThinkingLevel::XHigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

fn format_api_status_error(status: u16, body: &str) -> String {
    let trimmed = pi_js::str16::trim(body);
    if trimmed.is_empty() {
        format!("{status} status code (no body)")
    } else {
        format!("{status} {body}")
    }
}

#[derive(Clone, Debug)]
struct AnthropicApiErrorDetails {
    status: u16,
    error_type: Option<String>,
    error_code: Option<String>,
    message: String,
}

fn parse_anthropic_api_error_details(status: u16, body: &str, message: String) -> AnthropicApiErrorDetails {
    let parsed = pi_js::json::parse(body).ok();
    let error_obj = parsed.as_ref().and_then(|v| v.get("error").or(Some(v)));
    let error_type = error_obj
        .and_then(|e| e.get("type"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let error_code = error_obj
        .and_then(|e| e.get("details"))
        .and_then(|d| d.get("error_code"))
        .and_then(Value::as_str)
        .map(str::to_string);
    AnthropicApiErrorDetails {
        status,
        error_type,
        error_code,
        message,
    }
}

fn extract_anthropic_api_error_details(
    err: &Error,
    last_api_error: &Arc<std::sync::Mutex<Option<AnthropicApiErrorDetails>>>,
) -> Option<AnthropicApiErrorDetails> {
    if let Some(captured) = last_api_error
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return Some(captured);
    }
    let norm = normalize_provider_error(err);
    let status = norm.status?;
    let body = norm.body.as_deref().unwrap_or("");
    Some(parse_anthropic_api_error_details(status, body, norm.message))
}

async fn send_anthropic_request(
    body_map: Map<String, Value>,
    custom_client: Option<&AnthropicClient>,
    model: &Model,
    options: &AnthropicOptions,
    copilot_dynamic_headers: Option<&IndexMap<String, String>>,
    cache_session_id: Option<&str>,
    federation_token: Option<&str>,
    used_federation: bool,
    last_api_error: &Arc<std::sync::Mutex<Option<AnthropicApiErrorDetails>>>,
) -> Result<Response> {
    {
        let mut slot = last_api_error.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = None;
    }

    if let Some(client) = custom_client {
        let client_owned = client.clone();
        let params_for_retry = Value::Object(body_map);
        let signal_for_retry = options.base.signal.clone();
        let timeout_ms = options.base.timeout_ms;
        let last_err_slot = Arc::clone(last_api_error);
        return retry_provider_request(
            move || {
                let c = client_owned.clone();
                let p = params_for_retry.clone();
                let s = signal_for_retry.clone();
                let slot = Arc::clone(&last_err_slot);
                async move {
                    let req_opts = AnthropicClientRequestOptions {
                        signal: s,
                        timeout_ms,
                        max_retries: 0,
                    };
                    let resp = c.create_beta_messages(p, req_opts).await?;
                    if !resp.ok() {
                        let status = resp.status();
                        let resp_headers = resp.headers().clone();
                        let body = resp.text().await.unwrap_or_default();
                        let msg = format_api_status_error(status, &body);
                        *slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some(parse_anthropic_api_error_details(status, &body, msg.clone()));
                        return Err(ProviderHttpError::new(Some(status), Some(resp_headers), msg).into_js_error());
                    }
                    Ok(resp)
                }
            },
            ProviderRetryOptions {
                max_retries: options.base.max_retries,
                max_retry_delay_ms: options.base.max_retry_delay_ms,
                signal: options.base.signal.clone(),
            },
        )
        .await;
    }

    let mut body_obj = body_map;
    let betas: Vec<String> = body_obj
        .shift_remove("betas")
        .and_then(|v| match v {
            Value::Array(arr) => Some(arr.into_iter().filter_map(|x| x.as_str().map(str::to_string)).collect()),
            Value::String(s) => Some(vec![s]),
            _ => None,
        })
        .unwrap_or_default();
    let body_json = pi_js::json::stringify(&Value::Object(body_obj));

    let (headers, _) = build_request_headers(
        model,
        options.base.api_key.as_deref(),
        options.base.headers.as_ref(),
        copilot_dynamic_headers,
        cache_session_id,
        federation_token,
        &betas,
        options.base.identity.as_ref(),
    );

    let base_trimmed = model.base_url.trim_end_matches('/');
    let url_str = format!("{base_trimmed}/v1/messages?beta=true");
    let _parsed =
        Url::parse(&url_str).map_err(|e| Error::msg(format!("Invalid Anthropic baseUrl \"{url_str}\": {e}")))?;

    let custom_fetch = options.base.fetch.clone();
    let caller_signal = options.base.signal.clone();
    let timeout_ms = options.base.timeout_ms;
    let last_err_slot = Arc::clone(last_api_error);

    retry_provider_request(
        move || {
            let url = url_str.clone();
            let hdrs = headers.clone();
            let body = body_json.clone();
            let fetch_fn = custom_fetch.clone();
            let sig = caller_signal.clone();
            let slot = Arc::clone(&last_err_slot);
            async move {
                let timeout_sig = timeout_ms.map(AbortSignal::timeout);
                let op_sig = combine_abort_signals([sig.as_ref(), timeout_sig.as_ref()]);
                let mut req = Request::post(&url).with_headers(hdrs).body(Body::Text(body));
                if let Some(combined) = &op_sig.signal {
                    req = req.signal(combined.clone());
                }
                let resp_res = match fetch_fn {
                    Some(f) => f(req).await,
                    None => pi_js::fetch::fetch(req).await,
                };
                let resp = match resp_res {
                    Ok(r) => r,
                    Err(err) => {
                        if sig.as_ref().is_some_and(|s| s.aborted()) {
                            return Err(err);
                        }
                        return Err(ProviderHttpError::new(None, None, err.to_string()).into_js_error());
                    }
                };
                if !resp.ok() {
                    let status = resp.status();
                    let resp_headers = resp.headers().clone();
                    let err_body = resp.text().await.unwrap_or_default();
                    if status == 401 && used_federation {
                        invalidate_federation_cache().await;
                    }
                    let msg = format_api_status_error(status, &err_body);
                    *slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(parse_anthropic_api_error_details(status, &err_body, msg.clone()));
                    return Err(ProviderHttpError::new(Some(status), Some(resp_headers), msg).into_js_error());
                }
                Ok(resp)
            }
        },
        ProviderRetryOptions {
            max_retries: options.base.max_retries,
            max_retry_delay_ms: options.base.max_retry_delay_ms,
            signal: options.base.signal.clone(),
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn send_thread_request(
    thread: PreparedThreadRequest,
    params: &Map<String, Value>,
    custom_client: Option<&AnthropicClient>,
    model: &Model,
    options: &AnthropicOptions,
    copilot_dynamic_headers: Option<&IndexMap<String, String>>,
    cache_session_id: Option<&str>,
    federation_token: Option<&str>,
    used_federation: bool,
    last_api_error: &Arc<std::sync::Mutex<Option<AnthropicApiErrorDetails>>>,
    output: &mut AssistantMessage,
) -> Result<(Response, Option<PreparedThreadRequest>)> {
    match send_anthropic_request(
        thread.body.clone(),
        custom_client,
        model,
        options,
        copilot_dynamic_headers,
        cache_session_id,
        federation_token,
        used_federation,
        last_api_error,
    )
    .await
    {
        Ok(response) => Ok((response, Some(thread))),
        Err(err) => {
            if options.base.signal.as_ref().is_some_and(AbortSignal::aborted) {
                return Err(err);
            }
            let Some(api_err) = extract_anthropic_api_error_details(&err, last_api_error) else {
                return Err(err);
            };
            let error_code = api_err.error_code.as_deref();
            let start_new_thread = error_code.is_some_and(is_new_thread_error);
            let threads_unsupported = error_code == Some("thread_unsupported_request")
                || (error_code.is_none() && api_err.status == 400 && api_err.message.contains("thread"));
            if !start_new_thread && !threads_unsupported {
                return Err(err);
            }
            let mut details = JsonObject::new();
            details.insert(
                "reason".to_string(),
                JsonValue::String(
                    error_code
                        .map(str::to_string)
                        .unwrap_or_else(|| api_err.status.to_string()),
                ),
            );
            append_assistant_message_diagnostic(
                output,
                AssistantMessageDiagnostic {
                    r#type: "anthropic_thread_fallback".to_string(),
                    timestamp: pi_js::time::now_ms(),
                    error: Some(DiagnosticErrorInfo {
                        name: None,
                        message: api_err.message,
                        stack: None,
                        code: None,
                    }),
                    details: Some(details),
                },
            );
            if start_new_thread {
                let created = create_thread_request(&thread.session_id, params);
                let response = send_anthropic_request(
                    created.body.clone(),
                    custom_client,
                    model,
                    options,
                    copilot_dynamic_headers,
                    cache_session_id,
                    federation_token,
                    used_federation,
                    last_api_error,
                )
                .await?;
                return Ok((response, Some(created)));
            }
            let response = send_anthropic_request(
                params.clone(),
                custom_client,
                model,
                options,
                copilot_dynamic_headers,
                cache_session_id,
                federation_token,
                used_federation,
                last_api_error,
            )
            .await?;
            set_thread_state(&thread.session_id, ThreadStateValue::Off);
            Ok((response, None))
        }
    }
}

async fn run_anthropic_stream(
    model: Model,
    normalized_context: TranscriptContext,
    current_tools: Vec<Tool>,
    options: AnthropicOptions,
    output: &mut AssistantMessage,
    event_stream: &AssistantMessageEventStream,
) -> Result<()> {
    let custom_client = options.client.as_ref();
    let compat = get_anthropic_compat(&model);
    let mut is_oauth = false;
    let mut federation_token: Option<String> = None;
    let mut copilot_dynamic_headers: Option<IndexMap<String, String>> = None;

    let cache_retention = resolve_cache_retention(options.base.cache_retention, options.base.env.as_ref());
    let cache_session_id: Option<String> = if cache_retention == CacheRetention::None {
        None
    } else {
        options.base.session_id.clone()
    };
    let thread_session_id: Option<String> = if compat.supports_message_threads {
        cache_session_id.clone()
    } else {
        None
    };

    if custom_client.is_none() {
        let api_key = options.base.api_key.as_deref();
        let headers = options.base.headers.as_ref();
        let env = options.base.env.as_ref();
        let federation = get_anthropic_federation(&model, api_key, headers, env);
        if let Some(fed) = federation {
            let fed_token = resolve_federation_token(
                &model.base_url,
                &fed,
                options.base.fetch.as_ref(),
                options.base.signal.as_ref(),
            )
            .await?;
            federation_token = Some(fed_token);
        } else {
            assert_request_auth(&model.provider, api_key, headers)?;
        }

        if let Some(key) = api_key {
            if model.provider != "github-copilot" && is_oauth_token(key) {
                is_oauth = true;
            }
        }

        if model.provider == "github-copilot" {
            let has_images = has_copilot_vision_input(&normalized_context.messages);
            copilot_dynamic_headers = Some(build_copilot_dynamic_headers(&normalized_context.messages, has_images));
        }
    }

    let used_federation = federation_token.is_some();
    let last_api_error = Arc::new(std::sync::Mutex::new(None));
    let mut disable_preferred_strict_tools = has_exceeded_strict_tool_limits(options.base.session_id.as_deref());

    let response: Response;
    let params_map: Map<String, Value>;
    let thread: Option<PreparedThreadRequest>;

    loop {
        if options.base.signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Err(Error::msg("Request was aborted"));
        }
        let built = build_params(
            &model,
            &normalized_context,
            is_oauth,
            &options,
            BuildParamsFlags {
                disable_preferred_strict_tools,
                message_threads: thread_session_id.is_some(),
            },
        )?;
        if let Some(level) = built.provider_thinking_level {
            output.provider_thinking_level = Some(level);
        }
        let mut next_params_map = built.params;
        if let Some(on_payload) = &options.base.on_payload {
            if let Some(next_params) = on_payload(Value::Object(next_params_map.clone()), &model).await? {
                let mut next_obj = next_params.as_object().cloned().unwrap_or_default();
                next_obj.insert("stream".to_string(), Value::Bool(true));
                next_params_map = next_obj;
            }
        }

        let prepared = if let Some(ref ts_id) = thread_session_id
            && options.base.disable_continuation != Some(true)
        {
            prepare_thread_request(&model, ts_id, &next_params_map)
        } else {
            None
        };

        let send_res = match prepared {
            Some(prep) => {
                send_thread_request(
                    prep,
                    &next_params_map,
                    custom_client,
                    &model,
                    &options,
                    copilot_dynamic_headers.as_ref(),
                    cache_session_id.as_deref(),
                    federation_token.as_deref(),
                    used_federation,
                    &last_api_error,
                    output,
                )
                .await
            }
            None => send_anthropic_request(
                next_params_map.clone(),
                custom_client,
                &model,
                &options,
                copilot_dynamic_headers.as_ref(),
                cache_session_id.as_deref(),
                federation_token.as_deref(),
                used_federation,
                &last_api_error,
            )
            .await
            .map(|r| (r, None)),
        };

        match send_res {
            Ok((resp, used_thread)) => {
                response = resp;
                params_map = next_params_map;
                thread = used_thread;
                break;
            }
            Err(err) => {
                if options.base.signal.as_ref().is_some_and(AbortSignal::aborted)
                    || disable_preferred_strict_tools
                    || !built.has_preferred_strict_tools
                {
                    return Err(err);
                }
                let Some(api_err) = extract_anthropic_api_error_details(&err, &last_api_error) else {
                    return Err(err);
                };
                if api_err.status != 400
                    || api_err.error_type.as_deref() != Some("invalid_request_error")
                    || !is_strict_tool_limit_error_message(&api_err.message)
                {
                    return Err(err);
                }
                disable_preferred_strict_tools = true;
                mark_strict_tool_limits_exceeded(options.base.session_id.as_deref());
                let mut details = JsonObject::new();
                details.insert(
                    "reason".to_string(),
                    JsonValue::String("strict_tool_limits_exceeded".to_string()),
                );
                append_assistant_message_diagnostic(
                    output,
                    AssistantMessageDiagnostic {
                        r#type: "anthropic_strict_tools_fallback".to_string(),
                        timestamp: pi_js::time::now_ms(),
                        error: Some(DiagnosticErrorInfo {
                            name: None,
                            message: api_err.message,
                            stack: None,
                            code: None,
                        }),
                        details: Some(details),
                    },
                );
            }
        }
    }

    if let Some(on_response) = &options.base.on_response {
        on_response(
            ProviderResponse {
                status: response.status(),
                headers: headers_to_record(response.headers()),
            },
            &model,
        )
        .await?;
    }

    event_stream.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });

    let mut usage_model = model.clone();
    let mut input_transformations: Option<Vec<Value>> = None;

    iterate_and_process_sse(
        response,
        &model,
        &mut usage_model,
        is_oauth,
        &current_tools,
        &options,
        output,
        &mut input_transformations,
        event_stream,
    )
    .await?;

    if options.base.signal.as_ref().is_some_and(|s| s.aborted()) {
        return Err(Error::msg("Request was aborted"));
    }

    if output.stop_reason == StopReason::Pending {
        return Err(Error::msg("Anthropic stream ended without a stop reason"));
    }

    if matches!(output.stop_reason, StopReason::Aborted | StopReason::Error) {
        return Err(Error::msg(
            output.error_message.as_deref().unwrap_or("An unknown error occurred"),
        ));
    }

    if let Some(ref th) = thread {
        record_thread_reply(&model, th, &params_map, output, is_oauth, compat.allow_empty_signature);
    }

    if let Some(transformations) = input_transformations {
        if !transformations.is_empty() {
            let mapped: Vec<Value> = transformations
                .into_iter()
                .map(|t| {
                    let mut m = Map::new();
                    if let Some(v) = t.get("type") {
                        m.insert("type".to_string(), v.clone());
                    }
                    if let Some(v) = t.get("path") {
                        m.insert("path".to_string(), v.clone());
                    }
                    if let Some(v) = t.get("reason") {
                        m.insert("reason".to_string(), v.clone());
                    }
                    Value::Object(m)
                })
                .collect();
            let mut details = JsonObject::new();
            details.insert("transformations".to_string(), Value::Array(mapped));
            append_assistant_message_diagnostic(
                output,
                AssistantMessageDiagnostic {
                    r#type: "anthropic_input_transformations".to_string(),
                    timestamp: pi_js::time::now_ms(),
                    error: None,
                    details: Some(details),
                },
            );
        }
    }

    event_stream.push(AssistantMessageEvent::Done {
        reason: output.stop_reason,
        message: output.clone(),
    });
    event_stream.end(Some(output.clone()));
    Ok(())
}

pub fn stream_with_options(
    model: &Model,
    context: &TranscriptContext,
    options: Option<AnthropicOptions>,
) -> AssistantMessageEventStream {
    let event_stream = AssistantMessageEventStream::new();
    let stream_clone = event_stream.clone();
    let model_owned = model.clone();
    let compat = get_anthropic_compat(&model_owned);
    let normalized_context = resolve_transcript(context, Some(compat.supports_mid_convo_system_messages));
    let current_tools = get_current_tools(&normalized_context.messages);
    let options_owned = options.unwrap_or_default();

    tokio::spawn(async move {
        let mut output = AssistantMessage {
            content: Vec::new(),
            api: model_owned.api.clone(),
            provider: model_owned.provider.clone(),
            model: model_owned.id.clone(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: 0,
                cost: UsageCost::default(),
            },
            stop_reason: StopReason::Pending,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: pi_js::time::now_ms(),
            duration_ms: None,
        };

        let signal = options_owned.base.signal.clone();
        if let Err(err) = run_anthropic_stream(
            model_owned,
            normalized_context,
            current_tools,
            options_owned,
            &mut output,
            &stream_clone,
        )
        .await
        {
            let aborted = signal.as_ref().is_some_and(|s| s.aborted());
            output.stop_reason = if aborted {
                StopReason::Aborted
            } else {
                StopReason::Error
            };
            let err_msg = with_cause_detail(&err);
            output.error_message = Some(if err_msg.is_empty() {
                safe_json_stringify(&Value::String(err_msg))
            } else {
                err_msg
            });
            stream_clone.push(AssistantMessageEvent::Error {
                reason: output.stop_reason,
                error: output.clone(),
            });
            stream_clone.end(Some(output));
        }
    });

    event_stream
}

pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: Option<ApiStreamOptions>,
) -> AssistantMessageEventStream {
    stream_with_options(model, context, Some(AnthropicOptions::from_api_options(options)))
}

fn map_thinking_level_to_effort(model: &Model, level: Option<ThinkingLevel>) -> AnthropicEffort {
    if let Some(lvl) = level {
        if let Some(mapped) = model
            .thinking_level_map
            .as_ref()
            .and_then(|m| m.get(&to_model_thinking_level(lvl)))
            .and_then(|v| v.as_deref())
        {
            if let Some(effort) = AnthropicEffort::from_str_opt(mapped) {
                return effort;
            }
        }
    }
    match level {
        Some(ThinkingLevel::Minimal | ThinkingLevel::Low) => AnthropicEffort::Low,
        Some(ThinkingLevel::Medium) => AnthropicEffort::Medium,
        Some(ThinkingLevel::High | ThinkingLevel::XHigh | ThinkingLevel::Max) | None => AnthropicEffort::High,
    }
}

pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let opts_ref = options.as_ref();
    let api_key = opts_ref.and_then(|o| o.base.api_key.as_deref());
    let headers = opts_ref.and_then(|o| o.base.headers.as_ref());
    let env = opts_ref.and_then(|o| o.base.env.as_ref());

    if get_anthropic_federation(model, api_key, headers, env).is_none() {
        if let Err(err) = assert_request_auth(&model.provider, api_key, headers) {
            return lazy_stream(model, async move { Err(err) });
        }
    }

    let base = build_base_options(model, context, opts_ref, api_key);
    let tool_choice = opts_ref.and_then(|o| o.tool_choice).map(Into::into);
    let Some(reasoning) = opts_ref.and_then(|o| o.reasoning) else {
        return stream_with_options(
            model,
            context,
            Some(AnthropicOptions {
                base,
                thinking_enabled: Some(false),
                tool_choice,
                ..Default::default()
            }),
        );
    };

    if get_anthropic_compat(model).force_adaptive_thinking {
        let effort = map_thinking_level_to_effort(model, Some(reasoning));
        return stream_with_options(
            model,
            context,
            Some(AnthropicOptions {
                base,
                thinking_enabled: Some(true),
                effort: Some(effort),
                tool_choice,
                ..Default::default()
            }),
        );
    }

    let adjusted = adjust_max_tokens_for_thinking(
        base.max_tokens,
        model.max_tokens,
        reasoning,
        opts_ref.and_then(|o| o.thinking_budgets.as_ref()),
    );
    let max_tokens = clamp_max_tokens_to_context(model, context, adjusted.max_tokens);
    let mut next_base = base;
    next_base.max_tokens = Some(max_tokens);
    let clamped_budget = adjusted.thinking_budget.min((max_tokens - 1024).max(0));

    stream_with_options(
        model,
        context,
        Some(AnthropicOptions {
            base: next_base,
            thinking_enabled: Some(true),
            thinking_budget_tokens: Some(clamped_budget),
            tool_choice,
            ..Default::default()
        }),
    )
}

pub fn provider_streams() -> ProviderStreams {
    ProviderStreams {
        stream: Arc::new(|model, context, options| stream(model, context, options)),
        stream_simple: Arc::new(|model, context, options| stream_simple(model, context, options)),
        fetch_deferred: None,
        cancel_deferred: None,
    }
}
