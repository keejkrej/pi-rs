//! Port of packages/ai/src/api/anthropic-messages.ts

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, OnceLock};

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
    UnsupportedStrictSchemaKeywordCheck, get_json_schema_tool_parameters, resolve_json_schema_strict_sampling,
};
use crate::api::lazy::lazy_stream;
use crate::api::simple_options::{adjust_max_tokens_for_thinking, build_base_options, clamp_max_tokens_to_context};
use crate::api::transform_messages::transform_messages;
use crate::env_api_keys::{
    ANTHROPIC_FEDERATION_RULE_ID_ENV, ANTHROPIC_IDENTITY_TOKEN_FILE_ENV, ANTHROPIC_ORGANIZATION_ID_ENV,
    ANTHROPIC_SERVICE_ACCOUNT_ID_ENV, ANTHROPIC_WORKSPACE_ID_ENV,
};
use crate::models::calculate_cost;
use crate::types::{
    AnthropicAllowedFallbackModel, AnyModel, ApiStreamOptions, AssistantContentBlock, AssistantMessage,
    AssistantMessageEvent, CacheRetention, FetchFunction, JsonObject, JsonValue, Message, Model, ModelCompat,
    ModelThinkingLevel, ProviderEnv, ProviderHeaders, ProviderResponse, ProviderStreams, SessionAffinityFormat,
    SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingContent, ThinkingLevel, Tool, ToolCall,
    ToolChoice, ToolResultMessage, TranscriptContext, Usage, UsageCost, UserContentBlock, UserMessageContent,
};
use crate::utils::abort_signals::combine_abort_signals;
use crate::utils::diagnostics::{AssistantMessageDiagnostic, append_assistant_message_diagnostic};
use crate::utils::error_body::safe_json_stringify;
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::headers::headers_to_record;
use crate::utils::json_parse::{parse_json_with_repair, parse_streaming_json};
use crate::utils::pi_user_agent::get_pi_user_agent;
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{ProviderHttpError, ProviderRetryOptions, retry_provider_request};
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::text::{get_system_message_text, render_system_message_update};
use crate::utils::transcript::{
    get_current_tools, get_declared_tools, get_initial_system_message, has_tool_redefinitions, resolve_transcript,
};

use crate::api::github_copilot_headers::{build_copilot_dynamic_headers, has_copilot_vision_input};

const CLAUDE_CODE_VERSION: &str = "2.1.280";

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
const MID_CONVERSATION_TOOL_CHANGES_BETA: &str = "mid-conversation-tool-changes-2026-07-01";

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
        force_adaptive_thinking: c
            .and_then(anthropic_compat)
            .and_then(|c| c.force_adaptive_thinking)
            .unwrap_or(false),
        allowed_fallback_models: c
            .and_then(anthropic_compat)
            .and_then(|c| c.allowed_fallback_models.as_deref()),
    }
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
) -> (Headers, bool) {
    let mut headers = Headers::new();
    headers.set("accept", "application/json");
    headers.set("content-type", "application/json");
    headers.set("anthropic-version", "2023-06-01");
    headers.set("user-agent", &get_pi_user_agent());
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

fn get_beta_features(
    model: &Model,
    context: &TranscriptContext,
    is_oauth: bool,
    native_tool_changes: bool,
    options: &AnthropicOptions,
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
    if is_oauth {
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
    if native_tool_changes {
        set.insert(MID_CONVERSATION_TOOL_CHANGES_BETA.to_string());
    }
    set.into_iter().collect()
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
            UserContentBlock::Image(img) => serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": img.mime_type,
                    "data": img.data,
                }
            }),
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

fn convert_tool_result(msg: &ToolResultMessage) -> Value {
    serde_json::json!({
        "type": "tool_result",
        "tool_use_id": msg.tool_call_id,
        "content": convert_content_blocks(&msg.content),
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

fn convert_messages(
    transformed_messages: &[Message],
    is_oauth: bool,
    cache_control: Option<&Value>,
    allow_empty_signature: bool,
    managed_provider: Option<&str>,
    native_tool_changes: bool,
) -> ConvertedAnthropicMessages {
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
                if native_tool_changes {
                    if let Some(removed) = &sys.tools_removed {
                        for tool in removed {
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
                    if let Some(added) = &sys.tools_added {
                        for tool in added {
                            let name = if is_oauth {
                                to_claude_code_name(&tool.name)
                            } else {
                                tool.name.clone()
                            };
                            blocks.push(serde_json::json!({
                                "type": "tool_addition",
                                "tool": { "type": "tool_reference", "name": name },
                            }));
                        }
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
                            UserContentBlock::Image(img) => Some(serde_json::json!({
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": img.mime_type,
                                    "data": img.data,
                                },
                            })),
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
                params.append(&mut pending_system_messages);
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
                let mut j = i;
                while j < transformed_messages.len() {
                    if let Message::ToolResult(tr) = &transformed_messages[j] {
                        tool_results.push(convert_tool_result(tr));
                        j += 1;
                    } else {
                        break;
                    }
                }
                i = j - 1;
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

    ConvertedAnthropicMessages {
        messages: params,
        assistant_levels,
    }
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
) -> Result<Vec<Value>> {
    if tools.is_empty() {
        return Ok(Vec::new());
    }

    let check: UnsupportedStrictSchemaKeywordCheck<'_> = &is_anthropic_strict_unsupported_keyword;
    let mut converted = Vec::with_capacity(tools.len());

    for (index, tool) in tools.iter().enumerate() {
        let strict = resolve_json_schema_strict_sampling(tool, supports_strict_tools, Some(check))?;
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

    Ok(converted)
}

fn deferred_tool_placeholder() -> Value {
    serde_json::json!({
        "name": "__pi_deferred_placeholder__",
        "description": "Reserved placeholder. Never available. Never call this.",
        "input_schema": { "type": "object", "properties": {}, "required": [] },
        "defer_loading": true,
    })
}

fn build_params(
    model: &Model,
    context: &TranscriptContext,
    is_oauth: bool,
    options: &AnthropicOptions,
) -> Result<Value> {
    let (_, cache_control) = get_cache_control(model, options.base.cache_retention, options.base.env.as_ref());
    let compat = get_anthropic_compat(model);
    let initial_system_message = get_initial_system_message(&context.messages);
    let initial_system_text = initial_system_message.map(get_system_message_text).unwrap_or_default();

    let transformed = transform_messages(&context.messages, model, Some(&normalize_tool_call_id));
    let conversation_messages = if initial_system_message.is_some() {
        &transformed[1..]
    } else {
        &transformed[..]
    };

    let initial_tools = initial_system_message
        .and_then(|s| s.tools_added.as_deref())
        .unwrap_or(&[]);
    let native_tool_changes = compat.supports_mid_convo_system_messages
        && compat.supports_mid_convo_tool_changes
        && !initial_tools.is_empty()
        && !has_tool_redefinitions(&context.messages);

    let converted = convert_messages(
        conversation_messages,
        is_oauth,
        cache_control.as_ref(),
        compat.allow_empty_signature,
        if compat.supports_mid_convo_effort {
            Some(model.provider.as_str())
        } else {
            None
        },
        native_tool_changes,
    );

    let active_effort = options.effort.unwrap_or(AnthropicEffort::High);
    let beta_features = get_beta_features(model, context, is_oauth, native_tool_changes, options);
    let messages_val = if compat.supports_mid_convo_effort {
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
        let initial_names: IndexSet<&str> = initial_tools.iter().map(|t| t.name.as_str()).collect();
        let later_tools: Vec<Tool> = get_declared_tools(&context.messages)
            .into_iter()
            .filter(|t| !initial_names.contains(t.name.as_str()))
            .collect();

        let mut all_tools = convert_tools(
            initial_tools,
            is_oauth,
            compat.supports_eager_tool_input_streaming,
            compat.supports_strict_tools,
            tool_cache_control,
        )?;
        all_tools.push(deferred_tool_placeholder());
        for mut later in convert_tools(
            &later_tools,
            is_oauth,
            compat.supports_eager_tool_input_streaming,
            compat.supports_strict_tools,
            None,
        )? {
            if let Some(obj) = later.as_object_mut() {
                obj.insert("defer_loading".to_string(), Value::Bool(true));
            }
            all_tools.push(later);
        }
        params.insert("tools".to_string(), Value::Array(all_tools));
    } else {
        let tools = get_current_tools(&context.messages);
        if !tools.is_empty() {
            let converted_tools = convert_tools(
                &tools,
                is_oauth,
                compat.supports_eager_tool_input_streaming,
                compat.supports_strict_tools,
                tool_cache_control,
            )?;
            params.insert("tools".to_string(), Value::Array(converted_tools));
        }
    }

    if compat.supports_mid_convo_effort {
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
        params.insert("output_config".to_string(), serde_json::json!({ "effort": "high" }));
    } else if model.reasoning {
        if options.thinking_enabled == Some(true) {
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
        } else if options.thinking_enabled == Some(false) {
            let off_disabled_in_map = model
                .thinking_level_map
                .as_ref()
                .and_then(|m| m.get(&ModelThinkingLevel::Off))
                .is_some_and(Option::is_none);
            if !off_disabled_in_map {
                params.insert("thinking".to_string(), serde_json::json!({ "type": "disabled" }));
            }
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

    Ok(Value::Object(params))
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

fn apply_anthropic_event(
    event: &Value,
    model: &Model,
    usage_model: &mut Model,
    is_oauth: bool,
    current_tools: &[Tool],
    output: &mut AssistantMessage,
    block_indices: &mut IndexMap<usize, usize>,
    partial_jsons: &mut IndexMap<usize, String>,
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
                    block_indices.insert(sse_index, content_index);
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
                    block_indices.insert(sse_index, content_index);
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
                    block_indices.insert(sse_index, content_index);
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
                        thought_signature: None,
                        namespace: None,
                    }));
                    block_indices.insert(sse_index, content_index);
                    partial_jsons.insert(content_index, String::new());
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
            let Some(&content_index) = block_indices.get(&sse_index) else {
                return Ok(());
            };
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
                    if let Some(AssistantContentBlock::ToolCall(tc)) = output.content.get_mut(content_index) {
                        let buf = partial_jsons.entry(content_index).or_default();
                        buf.push_str(partial_delta);
                        tc.arguments = parse_streaming_json(Some(buf));
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
            let Some(content_index) = block_indices.shift_remove(&sse_index) else {
                return Ok(());
            };
            match output.content.get_mut(content_index) {
                Some(AssistantContentBlock::Text(t)) => {
                    let content = t.text.clone();
                    event_stream.push(AssistantMessageEvent::TextEnd {
                        content_index,
                        content,
                        partial: output.clone(),
                    });
                }
                Some(AssistantContentBlock::Thinking(th)) => {
                    let content = th.thinking.clone();
                    event_stream.push(AssistantMessageEvent::ThinkingEnd {
                        content_index,
                        content,
                        partial: output.clone(),
                    });
                }
                Some(AssistantContentBlock::ToolCall(tc)) => {
                    let buf = partial_jsons.shift_remove(&content_index).unwrap_or_default();
                    tc.arguments = parse_streaming_json(Some(&buf));
                    let tool_call = tc.clone();
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
    let mut block_indices = IndexMap::new();
    let mut partial_jsons = IndexMap::new();

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
                        &mut block_indices,
                        &mut partial_jsons,
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
                    &mut block_indices,
                    &mut partial_jsons,
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
                    &mut block_indices,
                    &mut partial_jsons,
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
                &mut block_indices,
                &mut partial_jsons,
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

async fn run_anthropic_stream(
    model: Model,
    normalized_context: TranscriptContext,
    current_tools: Vec<Tool>,
    options: AnthropicOptions,
    output: &mut AssistantMessage,
    event_stream: &AssistantMessageEventStream,
) -> Result<()> {
    let custom_client = options.client.clone();
    let mut is_oauth = false;
    let mut federation_token: Option<String> = None;
    let mut copilot_dynamic_headers: Option<IndexMap<String, String>> = None;
    let mut cache_session_id: Option<String> = None;

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

        let retention = resolve_cache_retention(options.base.cache_retention, env);
        if retention != CacheRetention::None {
            cache_session_id = options.base.session_id.clone();
        }
    }

    let mut params = build_params(&model, &normalized_context, is_oauth, &options)?;
    if let Some(on_payload) = &options.base.on_payload {
        if let Some(next_params) = on_payload(params.clone(), &model).await? {
            let mut next_obj = next_params.as_object().cloned().unwrap_or_default();
            next_obj.insert("stream".to_string(), Value::Bool(true));
            params = Value::Object(next_obj);
        }
    }

    let used_federation = federation_token.is_some();

    let response = if let Some(client) = custom_client {
        let params_for_retry = params.clone();
        let signal_for_retry = options.base.signal.clone();
        let timeout_ms = options.base.timeout_ms;
        retry_provider_request(
            move || {
                let c = client.clone();
                let p = params_for_retry.clone();
                let s = signal_for_retry.clone();
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
                        return Err(ProviderHttpError::new(
                            Some(status),
                            Some(resp_headers),
                            format_api_status_error(status, &body),
                        )
                        .into_js_error());
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
        .await?
    } else {
        let mut body_obj = params.as_object().cloned().unwrap_or_default();
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
            &model,
            options.base.api_key.as_deref(),
            options.base.headers.as_ref(),
            copilot_dynamic_headers.as_ref(),
            cache_session_id.as_deref(),
            federation_token.as_deref(),
            &betas,
        );

        let base_trimmed = model.base_url.trim_end_matches('/');
        let url_str = format!("{base_trimmed}/v1/messages?beta=true");
        let _parsed =
            Url::parse(&url_str).map_err(|e| Error::msg(format!("Invalid Anthropic baseUrl \"{url_str}\": {e}")))?;

        let custom_fetch = options.base.fetch.clone();
        let caller_signal = options.base.signal.clone();
        let timeout_ms = options.base.timeout_ms;

        retry_provider_request(
            move || {
                let url = url_str.clone();
                let hdrs = headers.clone();
                let body = body_json.clone();
                let fetch_fn = custom_fetch.clone();
                let sig = caller_signal.clone();
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
                        return Err(ProviderHttpError::new(
                            Some(status),
                            Some(resp_headers),
                            format_api_status_error(status, &err_body),
                        )
                        .into_js_error());
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
        .await?
    };

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

    let provider_thinking_level = if compat.supports_mid_convo_effort {
        Some(match options_owned.effort.unwrap_or(AnthropicEffort::High) {
            AnthropicEffort::Low => "low".to_string(),
            AnthropicEffort::Medium => "medium".to_string(),
            AnthropicEffort::High => "high".to_string(),
            AnthropicEffort::XHigh => "xhigh".to_string(),
            AnthropicEffort::Max => "max".to_string(),
        })
    } else {
        None
    };

    tokio::spawn(async move {
        let mut output = AssistantMessage {
            content: Vec::new(),
            api: model_owned.api.clone(),
            provider: model_owned.provider.clone(),
            model: model_owned.id.clone(),
            response_model: None,
            response_id: None,
            provider_thinking_level,
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
            let err_msg = err.to_string();
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
