//! Port of packages/ai/src/api/bedrock-converse-stream.ts

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use aws_config::BehaviorVersion;
use aws_sdk_bedrockruntime::Client as BedrockRuntimeClient;
use aws_sdk_bedrockruntime::config::http::HttpRequest;
use aws_sdk_bedrockruntime::config::interceptors::{
    BeforeDeserializationInterceptorContextRef, BeforeTransmitInterceptorContextMut,
};
use aws_sdk_bedrockruntime::config::timeout::TimeoutConfig;
use aws_sdk_bedrockruntime::config::{
    Config as BedrockSdkConfig, ConfigBag, Credentials, Intercept, Region, RuntimeComponents, Token,
};
use aws_sdk_bedrockruntime::error::{BoxError, DisplayErrorContext, ProvideErrorMetadata, SdkError};
use aws_sdk_bedrockruntime::operation::converse_stream::{ConverseStreamError, ConverseStreamInput};
use aws_sdk_bedrockruntime::types::error::ConverseStreamOutputError;
use aws_sdk_bedrockruntime::types::{
    AnyToolChoice, AutoToolChoice, CachePointBlock, CachePointType, CacheTtl, ContentBlock, ConversationRole,
    ConverseStreamMetadataEvent, ConverseStreamOutput, ImageBlock, ImageFormat, ImageSource, InferenceConfiguration,
    Message as BedrockMessage, ReasoningContentBlock, ReasoningContentBlockDelta, ReasoningTextBlock,
    SpecificToolChoice, StopReason as BedrockStopReason, SystemContentBlock, Tool as BedrockTool,
    ToolChoice as BedrockSdkToolChoice, ToolConfiguration, ToolInputSchema, ToolResultBlock, ToolResultContentBlock,
    ToolResultStatus, ToolSpecification, ToolUseBlock,
};
use aws_smithy_types::{Blob, Document, Number};
use indexmap::IndexMap;
use pi_js::{BoxFuture, Error, Result, b64, json, seam, time};
use serde::de::{self, Deserializer};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};
use url::Url;

use crate::api::constrained_sampling::{get_json_schema_tool_parameters, resolve_json_schema_strict_sampling};
use crate::api::simple_options::{
    adjust_max_tokens_for_thinking, build_base_options, clamp_max_tokens_to_context, clamp_reasoning,
};
use crate::api::transform_messages::transform_messages;
use crate::models::calculate_cost;
use crate::types::{
    AnyModel, Api, ApiStreamOptions, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, CacheRetention,
    ImageContent, JsonObject, JsonValue, KnownApi, Message, Model, ModelThinkingLevel, ProviderEnv, ProviderResponse,
    ProviderStreams, SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingBudgets, ThinkingContent,
    ThinkingLevel, Tool, ToolCall, ToolChoice, ToolResultMessage, TranscriptContext, Usage, UsageCost,
};
use crate::utils::diagnostics::{AssistantMessageDiagnostic, append_assistant_message_diagnostic};
use crate::utils::error_body::{SdkErrorInfo, normalize_provider_error};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::headers::provider_headers_to_record;
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::node_http_proxy::resolve_http_proxy_url_for_target;
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::text::get_system_message_text;
use crate::utils::transcript::{
    collapse_system_messages, get_current_tools, get_initial_system_message, without_initial_system_message,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BedrockThinkingDisplay {
    Summarized,
    Omitted,
}

impl BedrockThinkingDisplay {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Summarized => "summarized",
            Self::Omitted => "omitted",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BedrockToolChoice {
    Auto,
    Any,
    None,
    Tool { name: String },
}

impl From<ToolChoice> for BedrockToolChoice {
    fn from(value: ToolChoice) -> Self {
        match value {
            ToolChoice::Auto => Self::Auto,
            ToolChoice::None => Self::None,
        }
    }
}

impl Serialize for BedrockToolChoice {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Auto => serializer.serialize_str("auto"),
            Self::Any => serializer.serialize_str("any"),
            Self::None => serializer.serialize_str("none"),
            Self::Tool { name } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "tool")?;
                map.serialize_entry("name", name)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for BedrockToolChoice {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        match &value {
            Value::String(s) => match s.as_str() {
                "auto" => Ok(Self::Auto),
                "any" => Ok(Self::Any),
                "none" => Ok(Self::None),
                other => Err(de::Error::custom(format!("unknown bedrock toolChoice: {other}"))),
            },
            Value::Object(map) => {
                if map.get("type").and_then(Value::as_str) != Some("tool") {
                    return Err(de::Error::custom("bedrock toolChoice object must have type \"tool\""));
                }
                let name = map
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| de::Error::missing_field("name"))?;
                Ok(Self::Tool { name: name.to_string() })
            }
            _ => Err(de::Error::custom("bedrock toolChoice must be a string or tool object")),
        }
    }
}

#[derive(Clone, Default)]
pub struct BedrockOptions {
    pub base: StreamOptions,
    pub region: Option<String>,
    pub profile: Option<String>,
    pub tool_choice: Option<BedrockToolChoice>,
    /// See <https://docs.aws.amazon.com/bedrock/latest/userguide/inference-reasoning.html> for supported models.
    pub reasoning: Option<ThinkingLevel>,
    /// Custom token budgets per thinking level. Overrides default budgets.
    pub thinking_budgets: Option<ThinkingBudgets>,
    /// Only supported by Claude 4.x models, see <https://docs.aws.amazon.com/bedrock/latest/userguide/claude-messages-extended-thinking.html#claude-messages-extended-thinking-tool-use-interleaved>
    pub interleaved_thinking: Option<bool>,
    /// Controls how Claude's thinking content is returned in responses.
    /// - `"summarized"`: Thinking blocks contain summarized thinking text (default here).
    /// - `"omitted"`: Thinking content is redacted but the signature still travels back
    ///   for multi-turn continuity, reducing time-to-first-text-token.
    ///
    /// Note: Anthropic's API default for Claude Opus 4.8 and Mythos Preview is
    /// `"omitted"`. We default to `"summarized"` here to keep behavior consistent with
    /// older Claude 4 models. Only applies to Claude models on Bedrock.
    pub thinking_display: Option<BedrockThinkingDisplay>,
    /// Key-value pairs attached to the inference request for cost allocation tagging.
    /// Keys: max 64 chars, no `aws:` prefix. Values: max 256 chars. Max 50 pairs.
    /// Tags appear in AWS Cost Explorer split cost allocation data.
    /// See <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ConverseStream.html>
    pub request_metadata: Option<IndexMap<String, String>>,
    /// Bearer token for Bedrock API key authentication.
    /// When set, bypasses SigV4 signing and sends Authorization: Bearer `<token>` instead.
    /// Requires `bedrock:CallWithBearerToken` IAM permission on the token's identity.
    /// Set via `AWS_BEARER_TOKEN_BEDROCK` env var or pass directly.
    /// See <https://docs.aws.amazon.com/service-authorization/latest/reference/list_amazonbedrock.html>
    pub bearer_token: Option<String>,
}

impl fmt::Debug for BedrockOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BedrockOptions")
            .field("base", &self.base)
            .field("region", &self.region)
            .field("profile", &self.profile)
            .field("tool_choice", &self.tool_choice)
            .field("reasoning", &self.reasoning)
            .field("thinking_budgets", &self.thinking_budgets)
            .field("interleaved_thinking", &self.interleaved_thinking)
            .field("thinking_display", &self.thinking_display)
            .field("request_metadata", &self.request_metadata)
            .field("bearer_token", &self.bearer_token.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl Deref for BedrockOptions {
    type Target = StreamOptions;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for BedrockOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl From<StreamOptions> for BedrockOptions {
    fn from(base: StreamOptions) -> Self {
        Self {
            base,
            ..Self::default()
        }
    }
}

impl From<BedrockOptions> for ApiStreamOptions {
    fn from(opts: BedrockOptions) -> Self {
        let base = opts.base.clone();
        ApiStreamOptions {
            base,
            extra: JsonObject::new(),
            extension: Some(Arc::new(opts)),
        }
    }
}

fn parse_extra_field<T: serde::de::DeserializeOwned>(extra: &JsonObject, camel: &str, snake: &str) -> Option<T> {
    let val = extra.get(camel).or_else(|| extra.get(snake))?;
    serde_json::from_value(val.clone()).ok()
}

impl BedrockOptions {
    pub fn into_api_options(self) -> ApiStreamOptions {
        self.into()
    }

    pub fn from_api_options(options: Option<ApiStreamOptions>) -> Self {
        let Some(opts) = options else {
            return Self::default();
        };

        if let Some(ext) = &opts.extension
            && let Some(typed) = ext.downcast_ref::<BedrockOptions>()
        {
            let mut merged = typed.clone();
            merged.base = opts.base;
            return merged;
        }

        let extra = &opts.extra;
        Self {
            base: opts.base,
            region: parse_extra_field(extra, "region", "region"),
            profile: parse_extra_field(extra, "profile", "profile"),
            tool_choice: parse_extra_field(extra, "toolChoice", "tool_choice"),
            reasoning: parse_extra_field(extra, "reasoning", "reasoning"),
            thinking_budgets: parse_extra_field(extra, "thinkingBudgets", "thinking_budgets"),
            interleaved_thinking: parse_extra_field(extra, "interleavedThinking", "interleaved_thinking"),
            thinking_display: parse_extra_field(extra, "thinkingDisplay", "thinking_display"),
            request_metadata: parse_extra_field(extra, "requestMetadata", "request_metadata"),
            bearer_token: parse_extra_field(extra, "bearerToken", "bearer_token"),
        }
    }
}

const EMPTY_TEXT_PLACEHOLDER: &str = "<empty>";

/// Matches the placeholder the Anthropic API path uses for redacted thinking.
const REDACTED_THINKING_PLACEHOLDER: &str = "[Reasoning redacted]";

/// Over-long header values are dropped rather than truncated: a truncated request id is not a request id.
const MAX_BEDROCK_DIAGNOSTIC_VALUE_CHARS: usize = 200;

/// Some models reject the account/profile's configured Bedrock data retention mode
/// (e.g. "data retention mode 'default' is not available for this model"). Point
/// users at the AWS docs explaining how to configure a supported mode.
const BEDROCK_DATA_RETENTION_DOCS_URL: &str =
    "https://docs.aws.amazon.com/bedrock/latest/userguide/data-retention.html";

pub const CUSTOM_HEADERS_INTERCEPTOR_NAME: &str = "pi-ai-custom-headers";
pub const RESPONSE_HEADERS_INTERCEPTOR_NAME: &str = "pi-ai-response-headers";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BedrockCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BedrockClientConfig {
    pub region: Option<String>,
    pub profile: Option<String>,
    pub endpoint: Option<String>,
    pub credentials: Option<BedrockCredentials>,
    pub bearer_token: Option<String>,
    pub auth_scheme_preference: Option<Vec<String>>,
    pub proxy_url: Option<String>,
    pub force_http1: bool,
    pub timeout_ms: Option<u64>,
    pub skip_auth: bool,
    pub use_bearer_token: bool,
    pub has_options_profile: bool,
}

#[derive(Clone, Debug)]
pub struct BedrockTransportRequest {
    pub client_config: BedrockClientConfig,
    pub custom_headers: Option<IndexMap<String, String>>,
    pub interceptor_names: Vec<String>,
    pub command_input: JsonValue,
    pub input: ConverseStreamInput,
}

#[derive(Clone, Debug, Default)]
pub struct BedrockTransportError {
    pub message: String,
    pub status: Option<u16>,
    pub body: Option<String>,
    pub error_code: Option<String>,
    pub request_id: Option<String>,
    pub is_service_exception: bool,
}

#[derive(Clone, Debug)]
pub enum BedrockTransportEvent {
    Sdk(ConverseStreamOutput),
    StreamError(BedrockTransportError),
}

#[derive(Clone, Debug, Default)]
pub struct BedrockTransportStreamResponse {
    pub http_status: Option<u16>,
    pub request_id: Option<String>,
    pub headers: IndexMap<String, String>,
    pub events: Vec<BedrockTransportEvent>,
}

#[derive(Clone, Debug)]
pub enum BedrockTransportOutcome {
    Stream(BedrockTransportStreamResponse),
    Error(BedrockTransportError),
}

pub type BedrockTransportFn =
    dyn Fn(BedrockTransportRequest) -> BoxFuture<Result<BedrockTransportOutcome>> + Send + Sync;

thread_local! {
    static BEDROCK_TRANSPORT: seam::Slot<BedrockTransportFn> = seam::Slot::new();
}

/// Installs a thread-local transport override for offline Bedrock testing (`PORTING.md` §12.3).
pub fn override_bedrock_transport(
    f: impl Fn(BedrockTransportRequest) -> BoxFuture<Result<BedrockTransportOutcome>> + Send + Sync + 'static,
) -> seam::Guard {
    BEDROCK_TRANSPORT.with(|s| s.set(Arc::new(f)))
}

#[derive(Clone, Default)]
struct BlockScratch {
    index: Option<i32>,
    partial_json: Option<String>,
    redacted_chunks: Option<Vec<Vec<u8>>>,
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value.filter(|s| !s.is_empty()).map(str::to_string)
}

fn non_empty_env(name: &str, env: Option<&ProviderEnv>) -> Option<String> {
    get_provider_env_value(name, env).filter(|s| !s.is_empty())
}

/// Header keys that must never be overwritten by caller-supplied headers.
/// `host` and `x-amz-*` participate in the SigV4 canonical request; `authorization`
/// is owned by SigV4 or the bearer-token path (`token` + `authSchemePreference`).
/// Compared case-insensitively.
pub fn is_reserved_bedrock_header(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.starts_with("x-amz-") || lower == "authorization" || lower == "host"
}

/// Smithy `Intercept` implementation that attaches caller-supplied custom headers
/// in `modify_before_transmit` while silently skipping reserved SigV4 / auth headers
/// (`x-amz-*`, `authorization`, `host`).
#[derive(Clone, Debug)]
pub struct BedrockCustomHeadersInterceptor {
    headers: IndexMap<String, String>,
}

impl BedrockCustomHeadersInterceptor {
    pub fn new(headers: IndexMap<String, String>) -> Self {
        Self { headers }
    }

    pub fn headers(&self) -> &IndexMap<String, String> {
        &self.headers
    }

    pub fn apply_to_http_request(&self, request: &mut HttpRequest) -> std::result::Result<(), BoxError> {
        let request_headers = request.headers_mut();
        for (key, value) in &self.headers {
            if !is_reserved_bedrock_header(key) {
                request_headers.try_insert(key.clone(), value.clone())?;
            }
        }
        Ok(())
    }
}

impl Intercept for BedrockCustomHeadersInterceptor {
    fn name(&self) -> &'static str {
        CUSTOM_HEADERS_INTERCEPTOR_NAME
    }

    fn modify_before_transmit(
        &self,
        context: &mut BeforeTransmitInterceptorContextMut<'_>,
        _runtime_components: &RuntimeComponents,
        _cfg: &mut ConfigBag,
    ) -> std::result::Result<(), BoxError> {
        self.apply_to_http_request(context.request_mut())
    }
}

#[derive(Clone, Debug)]
struct BedrockResponseHeadersInterceptor {
    captured: Arc<Mutex<Option<ProviderResponse>>>,
}

impl BedrockResponseHeadersInterceptor {
    fn new(captured: Arc<Mutex<Option<ProviderResponse>>>) -> Self {
        Self { captured }
    }
}

impl Intercept for BedrockResponseHeadersInterceptor {
    fn name(&self) -> &'static str {
        RESPONSE_HEADERS_INTERCEPTOR_NAME
    }

    fn read_after_transmit(
        &self,
        context: &BeforeDeserializationInterceptorContextRef<'_>,
        _runtime_components: &RuntimeComponents,
        _cfg: &mut ConfigBag,
    ) -> std::result::Result<(), BoxError> {
        let response = context.response();
        let status = response.status().as_u16();
        let mut headers = IndexMap::new();
        for (name, value) in response.headers().iter() {
            headers.insert(name.to_ascii_lowercase(), value.to_string());
        }
        let mut slot = self.captured.lock().unwrap_or_else(PoisonError::into_inner);
        *slot = Some(ProviderResponse { status, headers });
        Ok(())
    }
}

fn take_captured_response(slot: &Arc<Mutex<Option<ProviderResponse>>>) -> Option<ProviderResponse> {
    slot.lock().unwrap_or_else(PoisonError::into_inner).take()
}

fn ascii_lowercase_slug(s: &str) -> String {
    s.to_ascii_lowercase()
        .chars()
        .map(|c| match c {
            c if c.is_ascii_whitespace() || matches!(c, '_' | '.' | ':') => '-',
            c => c,
        })
        .collect()
}

/// Collapses runs matching `/[\s_.:]+/g` into single `-`, matching TS `getModelMatchCandidates`.
fn collapse_slug_separators(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut prev_sep = false;
    for ch in lower.chars() {
        let is_sep = ch.is_ascii_whitespace() || matches!(ch, '_' | '.' | ':');
        if is_sep {
            if !prev_sep {
                out.push('-');
                prev_sep = true;
            }
        } else {
            out.push(ch);
            prev_sep = false;
        }
    }
    out
}

fn get_model_match_candidates(model_id: &str, model_name: &str) -> Vec<String> {
    let _ = ascii_lowercase_slug;
    let mut candidates = Vec::with_capacity(4);
    candidates.push(model_id.to_ascii_lowercase());
    candidates.push(collapse_slug_separators(model_id));
    if !model_name.is_empty() {
        candidates.push(model_name.to_ascii_lowercase());
        candidates.push(collapse_slug_separators(model_name));
    }
    candidates
}

fn supports_adaptive_thinking(model_id: &str, model_name: &str) -> bool {
    let candidates = get_model_match_candidates(model_id, model_name);
    candidates.iter().any(|s| {
        s.contains("opus-4-6")
            || s.contains("opus-4-7")
            || s.contains("opus-4-8")
            || s.contains("opus-5")
            || s.contains("sonnet-4-6")
            || s.contains("sonnet-5")
            || s.contains("fable-5")
    })
}

fn supports_native_xhigh_effort(model: &Model) -> bool {
    let candidates = get_model_match_candidates(&model.id, &model.name);
    candidates.iter().any(|s| {
        s.contains("opus-4-7")
            || s.contains("opus-4-8")
            || s.contains("opus-5")
            || s.contains("sonnet-5")
            || s.contains("fable-5")
    })
}

fn thinking_level_to_model_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::XHigh => ModelThinkingLevel::XHigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

fn map_thinking_level_to_effort(model: &Model, level: Option<ThinkingLevel>) -> String {
    if level == Some(ThinkingLevel::XHigh) && supports_native_xhigh_effort(model) {
        return "xhigh".to_string();
    }

    if let Some(lvl) = level
        && let Some(map) = &model.thinking_level_map
        && let Some(Some(mapped)) = map.get(&thinking_level_to_model_level(lvl))
    {
        return mapped.clone();
    }

    match level {
        Some(ThinkingLevel::Minimal | ThinkingLevel::Low) => "low".to_string(),
        Some(ThinkingLevel::Medium) => "medium".to_string(),
        Some(ThinkingLevel::High) | None => "high".to_string(),
        Some(ThinkingLevel::XHigh | ThinkingLevel::Max) => "high".to_string(),
    }
}

/// Resolve cache retention preference.
/// Defaults to `"short"` and uses `PI_CACHE_RETENTION` for backward compatibility.
fn resolve_cache_retention(cache_retention: Option<CacheRetention>, env: Option<&ProviderEnv>) -> CacheRetention {
    if let Some(retention) = cache_retention {
        return retention;
    }
    if get_provider_env_value("PI_CACHE_RETENTION", env).as_deref() == Some("long") {
        return CacheRetention::Long;
    }
    CacheRetention::Short
}

/// Check if the model is an Anthropic Claude model on Bedrock.
/// Checks both model ID and model name to support application inference profiles
/// whose ARNs don't contain the model name.
fn is_anthropic_claude_model(model: &Model) -> bool {
    let id = model.id.to_ascii_lowercase();
    let name = model.name.to_ascii_lowercase();
    id.contains("anthropic.claude")
        || id.contains("anthropic/claude")
        || name.contains("anthropic.claude")
        || name.contains("anthropic/claude")
        || name.contains("claude")
}

/// Check if the model supports prompt caching.
/// Supported: Claude 3.5 Haiku, Claude 3.7 Sonnet, Claude 4.x models, Claude 5 models.
fn supports_prompt_caching(model: &Model, env: Option<&ProviderEnv>) -> bool {
    let candidates = get_model_match_candidates(&model.id, &model.name);
    let has_claude_ref = candidates.iter().any(|s| s.contains("claude"));
    if !has_claude_ref {
        if get_provider_env_value("AWS_BEDROCK_FORCE_CACHE", env).as_deref() == Some("1") {
            return true;
        }
        return false;
    }
    if candidates
        .iter()
        .any(|s| s.contains("fable-5") || s.contains("opus-5") || s.contains("sonnet-5"))
    {
        return true;
    }
    if candidates.iter().any(|s| s.contains("-4-")) {
        return true;
    }
    if candidates.iter().any(|s| s.contains("claude-3-7-sonnet")) {
        return true;
    }
    if candidates.iter().any(|s| s.contains("claude-3-5-haiku")) {
        return true;
    }
    false
}

/// Check if the model supports thinking signatures in `reasoningContent`.
/// Only Anthropic Claude models support the signature field.
fn supports_thinking_signature(model: &Model) -> bool {
    is_anthropic_claude_model(model)
}

fn cache_point_json(cache_retention: CacheRetention) -> Value {
    let mut inner = Map::new();
    inner.insert("type".to_string(), Value::String("default".to_string()));
    if cache_retention == CacheRetention::Long {
        inner.insert("ttl".to_string(), Value::String("1h".to_string()));
    }
    let mut obj = Map::new();
    obj.insert("cachePoint".to_string(), Value::Object(inner));
    Value::Object(obj)
}

fn build_system_prompt(
    system_prompt: Option<&str>,
    model: &Model,
    cache_retention: CacheRetention,
    env: Option<&ProviderEnv>,
) -> Option<Vec<Value>> {
    let prompt = system_prompt.filter(|s| !s.is_empty())?;
    let mut text_block = Map::new();
    text_block.insert("text".to_string(), Value::String(sanitize_surrogates(prompt)));
    let mut blocks = vec![Value::Object(text_block)];

    if cache_retention != CacheRetention::None && supports_prompt_caching(model, env) {
        blocks.push(cache_point_json(cache_retention));
    }

    Some(blocks)
}

fn normalize_tool_call_id(id: &str) -> String {
    let sanitized: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.len() > 64 {
        sanitized[..64].to_string()
    } else {
        sanitized
    }
}

fn text_block_json(text: impl Into<String>) -> Value {
    let mut map = Map::new();
    map.insert("text".to_string(), Value::String(text.into()));
    Value::Object(map)
}

fn create_non_blank_text_block(text: &str) -> Option<Value> {
    let sanitized = sanitize_surrogates(text);
    if sanitized.trim().is_empty() {
        None
    } else {
        Some(text_block_json(sanitized))
    }
}

fn create_required_text_block(text: &str) -> Value {
    create_non_blank_text_block(text).unwrap_or_else(|| text_block_json(EMPTY_TEXT_PLACEHOLDER))
}

fn sanitize_bedrock_value(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::Array(arr) => JsonValue::Array(arr.iter().map(sanitize_bedrock_value).collect()),
        JsonValue::Object(map) => {
            let mut out = Map::new();
            for (key, nested) in map {
                if !key.is_empty() {
                    out.insert(key.clone(), sanitize_bedrock_value(nested));
                }
            }
            JsonValue::Object(out)
        }
        other => other.clone(),
    }
}

fn json_object_to_sanitized_value(obj: &JsonObject) -> JsonValue {
    let mut out = Map::new();
    for (key, nested) in obj {
        if !key.is_empty() {
            out.insert(key.clone(), sanitize_bedrock_value(nested));
        }
    }
    JsonValue::Object(out)
}

fn create_image_block_json(mime_type: &str, data: &str) -> Result<Value> {
    let format = match mime_type {
        "image/jpeg" | "image/jpg" => "jpeg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        other => return Err(Error::msg(format!("Unknown image type: {other}"))),
    };
    let bytes = b64::atob_bytes(data)?;
    let bytes_val = Value::Array(bytes.into_iter().map(Value::from).collect());
    let mut source = Map::new();
    source.insert("bytes".to_string(), bytes_val);

    let mut image = Map::new();
    image.insert("source".to_string(), Value::Object(source));
    image.insert("format".to_string(), Value::String(format.to_string()));
    Ok(Value::Object(image))
}

fn decode_redacted_content(signature: Option<&str>) -> Option<Vec<u8>> {
    let sig = signature?;
    if sig.is_empty() {
        return None;
    }
    b64::atob_bytes(sig).ok()
}

fn encode_redacted_content(chunks: &[Vec<u8>]) -> String {
    let total_len: usize = chunks.iter().map(Vec::len).sum();
    let mut joined = Vec::with_capacity(total_len);
    for chunk in chunks {
        joined.extend_from_slice(chunk);
    }
    b64::encode(&joined)
}

fn convert_tool_result_content(content: &[crate::types::UserContentBlock]) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    for block in content {
        match block {
            crate::types::UserContentBlock::Image(img) => {
                let mut wrapper = Map::new();
                wrapper.insert("image".to_string(), create_image_block_json(&img.mime_type, &img.data)?);
                result.push(Value::Object(wrapper));
            }
            crate::types::UserContentBlock::Text(text) => {
                if let Some(text_block) = create_non_blank_text_block(&text.text) {
                    result.push(text_block);
                }
            }
        }
    }
    if result.is_empty() {
        result.push(text_block_json(EMPTY_TEXT_PLACEHOLDER));
    }
    Ok(result)
}

fn make_tool_result_member(tool_result: &ToolResultMessage) -> Result<Value> {
    let mut inner = Map::new();
    inner.insert("toolUseId".to_string(), Value::String(tool_result.tool_call_id.clone()));
    inner.insert(
        "content".to_string(),
        Value::Array(convert_tool_result_content(&tool_result.content)?),
    );
    inner.insert(
        "status".to_string(),
        Value::String(if tool_result.is_error { "error" } else { "success" }.to_string()),
    );
    let mut wrapper = Map::new();
    wrapper.insert("toolResult".to_string(), Value::Object(inner));
    Ok(Value::Object(wrapper))
}

fn make_bedrock_message_json(role: &str, content: Vec<Value>) -> Value {
    let mut msg = Map::new();
    msg.insert("role".to_string(), Value::String(role.to_string()));
    msg.insert("content".to_string(), Value::Array(content));
    Value::Object(msg)
}

fn convert_messages(
    context: &TranscriptContext,
    model: &Model,
    cache_retention: CacheRetention,
    env: Option<&ProviderEnv>,
) -> Result<Vec<Value>> {
    let mut result: Vec<Value> = Vec::new();
    let stripped = without_initial_system_message(&context.messages);
    let transformed_messages = transform_messages(&stripped, model, Some(&normalize_tool_call_id));

    let mut i = 0;
    while i < transformed_messages.len() {
        match &transformed_messages[i] {
            Message::User(m) => {
                let mut content = Vec::new();
                match &m.content {
                    crate::types::UserMessageContent::Text(s) => {
                        content.push(create_required_text_block(s));
                    }
                    crate::types::UserMessageContent::Blocks(blocks) => {
                        for block in blocks {
                            match block {
                                crate::types::UserContentBlock::Text(text) => {
                                    if let Some(text_block) = create_non_blank_text_block(&text.text) {
                                        content.push(text_block);
                                    }
                                }
                                crate::types::UserContentBlock::Image(img) => {
                                    let mut wrapper = Map::new();
                                    wrapper.insert(
                                        "image".to_string(),
                                        create_image_block_json(&img.mime_type, &img.data)?,
                                    );
                                    content.push(Value::Object(wrapper));
                                }
                            }
                        }
                        if content.is_empty() {
                            content.push(text_block_json(EMPTY_TEXT_PLACEHOLDER));
                        }
                    }
                }
                result.push(make_bedrock_message_json("user", content));
            }
            Message::Assistant(m) => {
                if m.content.is_empty() {
                    i += 1;
                    continue;
                }
                let mut content_blocks = Vec::new();
                for block in &m.content {
                    match block {
                        AssistantContentBlock::Text(text) => {
                            if let Some(text_block) = create_non_blank_text_block(&text.text) {
                                content_blocks.push(text_block);
                            }
                        }
                        AssistantContentBlock::ToolCall(tc) => {
                            let mut tool_use = Map::new();
                            tool_use.insert("toolUseId".to_string(), Value::String(tc.id.clone()));
                            tool_use.insert("name".to_string(), Value::String(tc.name.clone()));
                            tool_use.insert("input".to_string(), json_object_to_sanitized_value(&tc.arguments));
                            let mut wrapper = Map::new();
                            wrapper.insert("toolUse".to_string(), Value::Object(tool_use));
                            content_blocks.push(Value::Object(wrapper));
                        }
                        AssistantContentBlock::Thinking(th) => {
                            if th.redacted == Some(true) {
                                if let Some(redacted_bytes) = decode_redacted_content(th.thinking_signature.as_deref())
                                    && !redacted_bytes.is_empty()
                                {
                                    let mut rc = Map::new();
                                    rc.insert(
                                        "redactedContent".to_string(),
                                        Value::Array(redacted_bytes.into_iter().map(Value::from).collect()),
                                    );
                                    let mut wrapper = Map::new();
                                    wrapper.insert("reasoningContent".to_string(), Value::Object(rc));
                                    content_blocks.push(Value::Object(wrapper));
                                }
                                continue;
                            }

                            let thinking = sanitize_surrogates(&th.thinking);
                            if thinking.trim().is_empty() {
                                continue;
                            }

                            if supports_thinking_signature(model) {
                                let sig = th.thinking_signature.as_deref().filter(|s| !s.trim().is_empty());
                                if let Some(signature) = sig {
                                    let mut rt = Map::new();
                                    rt.insert("text".to_string(), Value::String(thinking));
                                    rt.insert("signature".to_string(), Value::String(signature.to_string()));
                                    let mut rc = Map::new();
                                    rc.insert("reasoningText".to_string(), Value::Object(rt));
                                    let mut wrapper = Map::new();
                                    wrapper.insert("reasoningContent".to_string(), Value::Object(rc));
                                    content_blocks.push(Value::Object(wrapper));
                                } else {
                                    content_blocks.push(text_block_json(thinking));
                                }
                            } else {
                                let mut rt = Map::new();
                                rt.insert("text".to_string(), Value::String(thinking));
                                let mut rc = Map::new();
                                rc.insert("reasoningText".to_string(), Value::Object(rt));
                                let mut wrapper = Map::new();
                                wrapper.insert("reasoningContent".to_string(), Value::Object(rc));
                                content_blocks.push(Value::Object(wrapper));
                            }
                        }
                    }
                }
                if content_blocks.is_empty() {
                    i += 1;
                    continue;
                }
                result.push(make_bedrock_message_json("assistant", content_blocks));
            }
            Message::ToolResult(tr) => {
                let mut tool_results = Vec::new();
                tool_results.push(make_tool_result_member(tr)?);

                let mut j = i + 1;
                while j < transformed_messages.len() {
                    if let Message::ToolResult(next_tr) = &transformed_messages[j] {
                        tool_results.push(make_tool_result_member(next_tr)?);
                        j += 1;
                    } else {
                        break;
                    }
                }
                i = j - 1;
                result.push(make_bedrock_message_json("user", tool_results));
            }
            Message::System(_) => {
                // Mid-conversation system messages are collapsed before conversion.
            }
        }
        i += 1;
    }

    if cache_retention != CacheRetention::None
        && supports_prompt_caching(model, env)
        && let Some(Value::Object(last_msg)) = result.last_mut()
        && last_msg.get("role").and_then(Value::as_str) == Some("user")
        && let Some(Value::Array(last_content)) = last_msg.get_mut("content")
    {
        last_content.push(cache_point_json(cache_retention));
    }

    Ok(result)
}

fn convert_tool_config(
    tools: &[Tool],
    tool_choice: Option<&BedrockToolChoice>,
    supports_strict_mode: bool,
) -> Result<Option<Value>> {
    if tools.is_empty() || matches!(tool_choice, Some(BedrockToolChoice::None)) {
        return Ok(None);
    }

    let mut bedrock_tools = Vec::with_capacity(tools.len());
    for tool in tools {
        let strict = resolve_json_schema_strict_sampling(tool, supports_strict_mode, None)?;
        let params = get_json_schema_tool_parameters(tool, strict)?;
        let mut input_schema = Map::new();
        input_schema.insert("json".to_string(), params.to_value());

        let mut tool_spec = Map::new();
        tool_spec.insert("name".to_string(), Value::String(tool.name.clone()));
        tool_spec.insert("description".to_string(), Value::String(tool.description.clone()));
        tool_spec.insert("inputSchema".to_string(), Value::Object(input_schema));
        if strict == Some(true) {
            tool_spec.insert("strict".to_string(), Value::Bool(true));
        }

        let mut entry = Map::new();
        entry.insert("toolSpec".to_string(), Value::Object(tool_spec));
        bedrock_tools.push(Value::Object(entry));
    }

    let mut tool_config = Map::new();
    tool_config.insert("tools".to_string(), Value::Array(bedrock_tools));

    let bedrock_choice = match tool_choice {
        Some(BedrockToolChoice::Auto) => {
            let mut choice = Map::new();
            choice.insert("auto".to_string(), Value::Object(Map::new()));
            Some(Value::Object(choice))
        }
        Some(BedrockToolChoice::Any) => {
            let mut choice = Map::new();
            choice.insert("any".to_string(), Value::Object(Map::new()));
            Some(Value::Object(choice))
        }
        Some(BedrockToolChoice::Tool { name }) => {
            let mut inner = Map::new();
            inner.insert("name".to_string(), Value::String(name.clone()));
            let mut choice = Map::new();
            choice.insert("tool".to_string(), Value::Object(inner));
            Some(Value::Object(choice))
        }
        Some(BedrockToolChoice::None) | None => None,
    };
    if let Some(choice) = bedrock_choice {
        tool_config.insert("toolChoice".to_string(), choice);
    }

    Ok(Some(Value::Object(tool_config)))
}

fn is_lower_alnum_dash(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

fn extract_arn_bedrock_region(model_id: &str) -> Option<String> {
    let rest = model_id.strip_prefix("arn:aws")?;
    let after_partition = if let Some(after_dash) = rest.strip_prefix('-') {
        let (partition, tail) = after_dash.split_once(':')?;
        if !is_lower_alnum_dash(partition) {
            return None;
        }
        tail
    } else if let Some(tail) = rest.strip_prefix(':') {
        tail
    } else {
        return None;
    };
    let after_bedrock = after_partition.strip_prefix("bedrock:")?;
    let (region, _) = after_bedrock.split_once(':')?;
    if is_lower_alnum_dash(region) {
        Some(region.to_string())
    } else {
        None
    }
}

fn get_configured_bedrock_region(options: &BedrockOptions) -> Option<String> {
    let env = options.base.env.as_ref();
    non_empty_string(options.region.as_deref())
        .or_else(|| non_empty_env("AWS_REGION", env))
        .or_else(|| non_empty_env("AWS_DEFAULT_REGION", env))
}

fn get_configured_bedrock_credentials(env: Option<&ProviderEnv>) -> Option<BedrockCredentials> {
    let access_key_id = non_empty_env("AWS_ACCESS_KEY_ID", env)?;
    let secret_access_key = non_empty_env("AWS_SECRET_ACCESS_KEY", env)?;
    let session_token = non_empty_env("AWS_SESSION_TOKEN", env);
    Some(BedrockCredentials {
        access_key_id,
        secret_access_key,
        session_token,
    })
}

fn get_standard_bedrock_endpoint_region(base_url: Option<&str>) -> Option<String> {
    let base = base_url.filter(|s| !s.is_empty())?;
    let parsed = Url::parse(base).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let without_cn = host.strip_suffix(".cn").unwrap_or(&host);
    let prefix = without_cn.strip_suffix(".amazonaws.com")?;
    let region = prefix
        .strip_prefix("bedrock-runtime-fips.")
        .or_else(|| prefix.strip_prefix("bedrock-runtime."))?;
    if is_lower_alnum_dash(region) {
        Some(region.to_string())
    } else {
        None
    }
}

fn should_use_explicit_bedrock_endpoint(
    base_url: &str,
    configured_region: Option<&str>,
    has_ambient_configured_profile: bool,
) -> bool {
    let Some(_) = get_standard_bedrock_endpoint_region(Some(base_url)) else {
        return true;
    };
    configured_region.is_none() && !has_ambient_configured_profile
}

fn is_gov_cloud_bedrock_target(model: &Model, options: &BedrockOptions) -> bool {
    if let Some(region) = get_configured_bedrock_region(options)
        && region.to_ascii_lowercase().starts_with("us-gov-")
    {
        return true;
    }
    let model_id = model.id.to_ascii_lowercase();
    model_id.starts_with("us-gov.") || model_id.starts_with("arn:aws-us-gov:")
}

fn build_additional_model_request_fields(model: &Model, options: &BedrockOptions) -> Option<Value> {
    let reasoning = options.reasoning?;
    if !model.reasoning {
        return None;
    }

    if is_anthropic_claude_model(model) {
        // GovCloud Bedrock currently rejects the Claude thinking.display field.
        // Omit it there until the GovCloud Converse schema catches up.
        let display = if is_gov_cloud_bedrock_target(model, options) {
            None
        } else {
            Some(options.thinking_display.unwrap_or(BedrockThinkingDisplay::Summarized))
        };

        let mut result = Map::new();
        let adaptive = supports_adaptive_thinking(&model.id, &model.name);
        if adaptive {
            let mut thinking = Map::new();
            thinking.insert("type".to_string(), Value::String("adaptive".to_string()));
            if let Some(d) = display {
                thinking.insert("display".to_string(), Value::String(d.as_str().to_string()));
            }
            let mut output_config = Map::new();
            output_config.insert(
                "effort".to_string(),
                Value::String(map_thinking_level_to_effort(model, Some(reasoning))),
            );
            result.insert("thinking".to_string(), Value::Object(thinking));
            result.insert("output_config".to_string(), Value::Object(output_config));
        } else {
            let default_budget = match reasoning {
                ThinkingLevel::Minimal => 1024,
                ThinkingLevel::Low => 2048,
                ThinkingLevel::Medium => 8192,
                ThinkingLevel::High | ThinkingLevel::XHigh | ThinkingLevel::Max => 16384,
            };
            let custom_budget = options.thinking_budgets.as_ref().and_then(|b| match reasoning {
                ThinkingLevel::Minimal => b.minimal,
                ThinkingLevel::Low => b.low,
                ThinkingLevel::Medium => b.medium,
                ThinkingLevel::High | ThinkingLevel::XHigh | ThinkingLevel::Max => b.high,
            });
            let budget = custom_budget.unwrap_or(default_budget);

            let mut thinking = Map::new();
            thinking.insert("type".to_string(), Value::String("enabled".to_string()));
            thinking.insert("budget_tokens".to_string(), Value::from(budget));
            if let Some(d) = display {
                thinking.insert("display".to_string(), Value::String(d.as_str().to_string()));
            }
            result.insert("thinking".to_string(), Value::Object(thinking));

            if options.interleaved_thinking.unwrap_or(true) {
                result.insert(
                    "anthropic_beta".to_string(),
                    Value::Array(vec![Value::String("interleaved-thinking-2025-05-14".to_string())]),
                );
            }
        }

        return Some(Value::Object(result));
    }

    None
}

pub fn resolve_bedrock_client_config(model: &Model, options: &BedrockOptions) -> Result<BedrockClientConfig> {
    let env = options.base.env.as_ref();
    let options_profile = non_empty_string(options.profile.as_deref())
        .or_else(|| env.and_then(|e| non_empty_string(e.get("AWS_PROFILE").map(String::as_str))));
    let profile = options_profile.clone().or_else(|| non_empty_env("AWS_PROFILE", env));

    let configured_region = get_configured_bedrock_region(options);
    let has_ambient_configured_profile = non_empty_env("AWS_PROFILE", None).is_some();
    let endpoint_region = get_standard_bedrock_endpoint_region(Some(&model.base_url));
    let use_explicit_endpoint = should_use_explicit_bedrock_endpoint(
        &model.base_url,
        configured_region.as_deref(),
        has_ambient_configured_profile,
    );

    let endpoint = if use_explicit_endpoint {
        Some(model.base_url.clone())
    } else {
        None
    };

    let skip_auth = get_provider_env_value("AWS_BEDROCK_SKIP_AUTH", env).as_deref() == Some("1");
    let bearer_token = non_empty_string(options.bearer_token.as_deref())
        .or_else(|| non_empty_string(options.base.api_key.as_deref()))
        .or_else(|| non_empty_env("AWS_BEARER_TOKEN_BEDROCK", env));
    let use_bearer_token = bearer_token.is_some() && !skip_auth;

    let region = if let Some(arn_region) = extract_arn_bedrock_region(&model.id) {
        Some(arn_region)
    } else if let Some(reg) = configured_region {
        Some(reg)
    } else if endpoint_region.is_some() && use_explicit_endpoint {
        endpoint_region
    } else if !has_ambient_configured_profile {
        Some("us-east-1".to_string())
    } else {
        None
    };

    let env_credentials = get_configured_bedrock_credentials(env);
    let credentials = if skip_auth {
        Some(BedrockCredentials {
            access_key_id: "dummy-access-key".to_string(),
            secret_access_key: "dummy-secret-key".to_string(),
            session_token: None,
        })
    } else if env_credentials.is_some() && options_profile.is_none() {
        env_credentials
    } else {
        None
    };

    let proxy_url = resolve_http_proxy_url_for_target(&model.base_url, env)?.map(|u| u.to_string());
    let force_http1 = get_provider_env_value("AWS_BEDROCK_FORCE_HTTP1", env).as_deref() == Some("1");

    Ok(BedrockClientConfig {
        region,
        profile,
        endpoint,
        credentials,
        bearer_token: if use_bearer_token { bearer_token } else { None },
        auth_scheme_preference: if use_bearer_token {
            Some(vec!["httpBearerAuth".to_string()])
        } else {
            None
        },
        proxy_url,
        force_http1,
        timeout_ms: options.base.timeout_ms,
        skip_auth,
        use_bearer_token,
        has_options_profile: options_profile.is_some(),
    })
}

fn json_to_document(value: &Value) -> Document {
    match value {
        Value::Null => Document::Null,
        Value::Bool(b) => Document::Bool(*b),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Document::Number(Number::PosInt(u))
            } else if let Some(i) = n.as_i64() {
                Document::Number(Number::NegInt(i))
            } else {
                Document::Number(Number::Float(n.as_f64().unwrap_or(0.0)))
            }
        }
        Value::String(s) => Document::String(s.clone()),
        Value::Array(arr) => Document::Array(arr.iter().map(json_to_document).collect()),
        Value::Object(map) => Document::Object(map.iter().map(|(k, v)| (k.clone(), json_to_document(v))).collect()),
    }
}

fn document_to_json(doc: &Document) -> Value {
    match doc {
        Document::Null => Value::Null,
        Document::Bool(b) => Value::Bool(*b),
        Document::Number(num) => match num {
            Number::PosInt(u) => Value::from(*u),
            Number::NegInt(i) => Value::from(*i),
            Number::Float(f) => serde_json::Number::from_f64(*f)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        },
        Document::String(s) => Value::String(s.clone()),
        Document::Array(arr) => Value::Array(arr.iter().map(document_to_json).collect()),
        Document::Object(map) => {
            let mut entries: Vec<(&String, &Document)> = map.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            let mut out = Map::with_capacity(entries.len());
            for (k, v) in entries {
                out.insert(k.clone(), document_to_json(v));
            }
            Value::Object(out)
        }
    }
}

fn decode_bytes_value(value: &Value) -> Result<Vec<u8>> {
    match value {
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                let Some(byte) = item.as_u64().and_then(|v| u8::try_from(v).ok()) else {
                    return Err(Error::msg("Invalid byte value in Bedrock binary payload"));
                };
                out.push(byte);
            }
            Ok(out)
        }
        Value::String(s) => b64::atob_bytes(s),
        _ => Err(Error::msg(
            "Bedrock binary payload must be a byte array or base64 string",
        )),
    }
}

fn build_cache_point_block(obj: &Map<String, Value>) -> Result<CachePointBlock> {
    let type_str = obj.get("type").and_then(Value::as_str).unwrap_or("default");
    let cp_type = CachePointType::try_parse(type_str)
        .map_err(|e| Error::msg(format!("Invalid Bedrock cache point type '{type_str}': {e}")))?;
    let mut builder = CachePointBlock::builder().r#type(cp_type);
    if let Some(ttl_str) = obj.get("ttl").and_then(Value::as_str) {
        let ttl = CacheTtl::try_parse(ttl_str)
            .map_err(|e| Error::msg(format!("Invalid Bedrock cache TTL '{ttl_str}': {e}")))?;
        builder = builder.ttl(ttl);
    }
    builder
        .build()
        .map_err(|e| Error::msg(format!("Failed to build Bedrock cache point: {e}")))
}

fn build_image_block(obj: &Map<String, Value>) -> Result<ImageBlock> {
    let format_str = obj
        .get("format")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::msg("Missing image.format in Bedrock image block"))?;
    let format = ImageFormat::try_parse(format_str)
        .map_err(|e| Error::msg(format!("Invalid Bedrock image format '{format_str}': {e}")))?;
    let source_bytes = obj
        .get("source")
        .and_then(Value::as_object)
        .and_then(|src| src.get("bytes"))
        .ok_or_else(|| Error::msg("Missing image.source.bytes in Bedrock image block"))?;
    let bytes = decode_bytes_value(source_bytes)?;
    ImageBlock::builder()
        .format(format)
        .source(ImageSource::Bytes(Blob::new(bytes)))
        .build()
        .map_err(|e| Error::msg(format!("Failed to build Bedrock image block: {e}")))
}

fn value_to_converse_stream_input(value: &Value) -> Result<ConverseStreamInput> {
    let obj = value
        .as_object()
        .ok_or_else(|| Error::msg("Bedrock ConverseStream payload must be a JSON object"))?;

    let mut builder = ConverseStreamInput::builder();

    if let Some(model_id) = obj.get("modelId").and_then(Value::as_str) {
        builder = builder.model_id(model_id.to_string());
    }

    if let Some(messages) = obj.get("messages").and_then(Value::as_array) {
        for msg_val in messages {
            let msg_obj = msg_val
                .as_object()
                .ok_or_else(|| Error::msg("Bedrock message must be an object"))?;
            let role_str = msg_obj
                .get("role")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::msg("Bedrock message missing role"))?;
            let role = ConversationRole::try_parse(role_str)
                .map_err(|e| Error::msg(format!("Invalid Bedrock role '{role_str}': {e}")))?;
            let mut msg_builder = BedrockMessage::builder().role(role);
            if let Some(blocks) = msg_obj.get("content").and_then(Value::as_array) {
                for block_val in blocks {
                    let block_obj = block_val
                        .as_object()
                        .ok_or_else(|| Error::msg("Bedrock content block must be an object"))?;
                    if let Some(text) = block_obj.get("text").and_then(Value::as_str) {
                        msg_builder = msg_builder.content(ContentBlock::Text(text.to_string()));
                    } else if let Some(cp) = block_obj.get("cachePoint").and_then(Value::as_object) {
                        msg_builder = msg_builder.content(ContentBlock::CachePoint(build_cache_point_block(cp)?));
                    } else if let Some(img) = block_obj.get("image").and_then(Value::as_object) {
                        msg_builder = msg_builder.content(ContentBlock::Image(build_image_block(img)?));
                    } else if let Some(tu) = block_obj.get("toolUse").and_then(Value::as_object) {
                        let tool_use_id = tu.get("toolUseId").and_then(Value::as_str).unwrap_or_default();
                        let name = tu.get("name").and_then(Value::as_str).unwrap_or_default();
                        let input_doc = tu
                            .get("input")
                            .map(json_to_document)
                            .unwrap_or_else(|| Document::Object(std::iter::empty().collect()));
                        let built = ToolUseBlock::builder()
                            .tool_use_id(tool_use_id)
                            .name(name)
                            .input(input_doc)
                            .build()
                            .map_err(|e| Error::msg(format!("Failed to build Bedrock toolUse block: {e}")))?;
                        msg_builder = msg_builder.content(ContentBlock::ToolUse(built));
                    } else if let Some(tr) = block_obj.get("toolResult").and_then(Value::as_object) {
                        let tool_use_id = tr.get("toolUseId").and_then(Value::as_str).unwrap_or_default();
                        let mut tr_builder = ToolResultBlock::builder().tool_use_id(tool_use_id);
                        if let Some(items) = tr.get("content").and_then(Value::as_array) {
                            for item in items {
                                if let Some(item_obj) = item.as_object() {
                                    if let Some(t) = item_obj.get("text").and_then(Value::as_str) {
                                        tr_builder = tr_builder.content(ToolResultContentBlock::Text(t.to_string()));
                                    } else if let Some(img) = item_obj.get("image").and_then(Value::as_object) {
                                        tr_builder =
                                            tr_builder.content(ToolResultContentBlock::Image(build_image_block(img)?));
                                    }
                                }
                            }
                        }
                        if let Some(status_str) = tr.get("status").and_then(Value::as_str) {
                            let status = ToolResultStatus::try_parse(status_str).map_err(|e| {
                                Error::msg(format!("Invalid Bedrock toolResult status '{status_str}': {e}"))
                            })?;
                            tr_builder = tr_builder.status(status);
                        }
                        let built = tr_builder
                            .build()
                            .map_err(|e| Error::msg(format!("Failed to build Bedrock toolResult block: {e}")))?;
                        msg_builder = msg_builder.content(ContentBlock::ToolResult(built));
                    } else if let Some(rc) = block_obj.get("reasoningContent").and_then(Value::as_object) {
                        if let Some(redacted) = rc.get("redactedContent") {
                            let bytes = decode_bytes_value(redacted)?;
                            msg_builder = msg_builder.content(ContentBlock::ReasoningContent(
                                ReasoningContentBlock::RedactedContent(Blob::new(bytes)),
                            ));
                        } else if let Some(rt) = rc.get("reasoningText").and_then(Value::as_object) {
                            let text = rt.get("text").and_then(Value::as_str).unwrap_or_default();
                            let mut rt_builder = ReasoningTextBlock::builder().text(text);
                            if let Some(sig) = rt.get("signature").and_then(Value::as_str) {
                                rt_builder = rt_builder.signature(sig);
                            }
                            let built = rt_builder
                                .build()
                                .map_err(|e| Error::msg(format!("Failed to build Bedrock reasoningText block: {e}")))?;
                            msg_builder = msg_builder.content(ContentBlock::ReasoningContent(
                                ReasoningContentBlock::ReasoningText(built),
                            ));
                        }
                    }
                }
            }
            builder = builder.messages(
                msg_builder
                    .build()
                    .map_err(|e| Error::msg(format!("Failed to build Bedrock message: {e}")))?,
            );
        }
    }

    if let Some(system_blocks) = obj.get("system").and_then(Value::as_array) {
        for block_val in system_blocks {
            if let Some(block_obj) = block_val.as_object() {
                if let Some(text) = block_obj.get("text").and_then(Value::as_str) {
                    builder = builder.system(SystemContentBlock::Text(text.to_string()));
                } else if let Some(cp) = block_obj.get("cachePoint").and_then(Value::as_object) {
                    builder = builder.system(SystemContentBlock::CachePoint(build_cache_point_block(cp)?));
                }
            }
        }
    }

    if let Some(ic_obj) = obj.get("inferenceConfig").and_then(Value::as_object) {
        let mut ic_builder = InferenceConfiguration::builder();
        if let Some(max_tokens) = ic_obj.get("maxTokens").and_then(Value::as_i64) {
            let clamped = i32::try_from(max_tokens).unwrap_or(i32::MAX);
            ic_builder = ic_builder.max_tokens(clamped);
        }
        if let Some(temp) = ic_obj.get("temperature").and_then(Value::as_f64) {
            ic_builder = ic_builder.temperature(temp as f32);
        }
        if let Some(top_p) = ic_obj.get("topP").and_then(Value::as_f64) {
            ic_builder = ic_builder.top_p(top_p as f32);
        }
        if let Some(stops) = ic_obj.get("stopSequences").and_then(Value::as_array) {
            for s in stops.iter().filter_map(Value::as_str) {
                ic_builder = ic_builder.stop_sequences(s);
            }
        }
        builder = builder.inference_config(ic_builder.build());
    }

    if let Some(tc_obj) = obj.get("toolConfig").and_then(Value::as_object) {
        let mut tc_builder = ToolConfiguration::builder();
        if let Some(tools) = tc_obj.get("tools").and_then(Value::as_array) {
            for tool_val in tools {
                if let Some(tool_obj) = tool_val.as_object() {
                    if let Some(spec_obj) = tool_obj.get("toolSpec").and_then(Value::as_object) {
                        let name = spec_obj.get("name").and_then(Value::as_str).unwrap_or_default();
                        let mut spec_builder = ToolSpecification::builder().name(name);
                        if let Some(desc) = spec_obj.get("description").and_then(Value::as_str) {
                            spec_builder = spec_builder.description(desc);
                        }
                        if let Some(schema_val) = spec_obj
                            .get("inputSchema")
                            .and_then(Value::as_object)
                            .and_then(|s| s.get("json"))
                        {
                            spec_builder =
                                spec_builder.input_schema(ToolInputSchema::Json(json_to_document(schema_val)));
                        }
                        if let Some(strict) = spec_obj.get("strict").and_then(Value::as_bool) {
                            spec_builder = spec_builder.strict(strict);
                        }
                        let spec = spec_builder
                            .build()
                            .map_err(|e| Error::msg(format!("Failed to build Bedrock toolSpec: {e}")))?;
                        tc_builder = tc_builder.tools(BedrockTool::ToolSpec(spec));
                    } else if let Some(cp) = tool_obj.get("cachePoint").and_then(Value::as_object) {
                        tc_builder = tc_builder.tools(BedrockTool::CachePoint(build_cache_point_block(cp)?));
                    }
                }
            }
        }
        if let Some(choice_obj) = tc_obj.get("toolChoice").and_then(Value::as_object) {
            if choice_obj.contains_key("auto") {
                tc_builder = tc_builder.tool_choice(BedrockSdkToolChoice::Auto(AutoToolChoice::builder().build()));
            } else if choice_obj.contains_key("any") {
                tc_builder = tc_builder.tool_choice(BedrockSdkToolChoice::Any(AnyToolChoice::builder().build()));
            } else if let Some(specific) = choice_obj.get("tool").and_then(Value::as_object) {
                let name = specific.get("name").and_then(Value::as_str).unwrap_or_default();
                let sc = SpecificToolChoice::builder()
                    .name(name)
                    .build()
                    .map_err(|e| Error::msg(format!("Failed to build Bedrock toolChoice: {e}")))?;
                tc_builder = tc_builder.tool_choice(BedrockSdkToolChoice::Tool(sc));
            }
        }
        let tc = tc_builder
            .build()
            .map_err(|e| Error::msg(format!("Failed to build Bedrock toolConfig: {e}")))?;
        builder = builder.tool_config(tc);
    }

    if let Some(additional) = obj.get("additionalModelRequestFields").filter(|v| !v.is_null()) {
        builder = builder.additional_model_request_fields(json_to_document(additional));
    }

    if let Some(meta_obj) = obj.get("requestMetadata").and_then(Value::as_object) {
        for (k, v) in meta_obj {
            if let Some(s) = v.as_str() {
                builder = builder.request_metadata(k.clone(), s.to_string());
            }
        }
    }

    builder
        .build()
        .map_err(|e| Error::msg(format!("Failed to build ConverseStreamInput: {e}")))
}

fn converse_stream_output_to_json(event: &ConverseStreamOutput) -> Value {
    let mut root = Map::new();
    match event {
        ConverseStreamOutput::MessageStart(ev) => {
            let mut inner = Map::new();
            inner.insert("role".to_string(), Value::String(ev.role().as_str().to_string()));
            root.insert("messageStart".to_string(), Value::Object(inner));
        }
        ConverseStreamOutput::ContentBlockStart(ev) => {
            let mut inner = Map::new();
            inner.insert("contentBlockIndex".to_string(), Value::from(ev.content_block_index()));
            if let Some(start) = ev.start() {
                let mut start_map = Map::new();
                if let Ok(tu) = start.as_tool_use() {
                    let mut tu_map = Map::new();
                    tu_map.insert("toolUseId".to_string(), Value::String(tu.tool_use_id().to_string()));
                    tu_map.insert("name".to_string(), Value::String(tu.name().to_string()));
                    start_map.insert("toolUse".to_string(), Value::Object(tu_map));
                }
                inner.insert("start".to_string(), Value::Object(start_map));
            }
            root.insert("contentBlockStart".to_string(), Value::Object(inner));
        }
        ConverseStreamOutput::ContentBlockDelta(ev) => {
            let mut inner = Map::new();
            inner.insert("contentBlockIndex".to_string(), Value::from(ev.content_block_index()));
            if let Some(delta) = ev.delta() {
                let mut delta_map = Map::new();
                if let Ok(text) = delta.as_text() {
                    delta_map.insert("text".to_string(), Value::String(text.clone()));
                } else if let Ok(tu) = delta.as_tool_use() {
                    let mut tu_map = Map::new();
                    tu_map.insert("input".to_string(), Value::String(tu.input().to_string()));
                    delta_map.insert("toolUse".to_string(), Value::Object(tu_map));
                } else if let Ok(rc) = delta.as_reasoning_content() {
                    let mut rc_map = Map::new();
                    if let Ok(text) = rc.as_text() {
                        rc_map.insert("text".to_string(), Value::String(text.clone()));
                    } else if let Ok(sig) = rc.as_signature() {
                        rc_map.insert("signature".to_string(), Value::String(sig.clone()));
                    } else if let Ok(blob) = rc.as_redacted_content() {
                        rc_map.insert(
                            "redactedContent".to_string(),
                            Value::Array(blob.as_ref().iter().copied().map(Value::from).collect()),
                        );
                    }
                    delta_map.insert("reasoningContent".to_string(), Value::Object(rc_map));
                }
                inner.insert("delta".to_string(), Value::Object(delta_map));
            }
            root.insert("contentBlockDelta".to_string(), Value::Object(inner));
        }
        ConverseStreamOutput::ContentBlockStop(ev) => {
            let mut inner = Map::new();
            inner.insert("contentBlockIndex".to_string(), Value::from(ev.content_block_index()));
            root.insert("contentBlockStop".to_string(), Value::Object(inner));
        }
        ConverseStreamOutput::MessageStop(ev) => {
            let mut inner = Map::new();
            inner.insert(
                "stopReason".to_string(),
                Value::String(ev.stop_reason().as_str().to_string()),
            );
            if let Some(fields) = ev.additional_model_response_fields() {
                inner.insert("additionalModelResponseFields".to_string(), document_to_json(fields));
            }
            root.insert("messageStop".to_string(), Value::Object(inner));
        }
        ConverseStreamOutput::Metadata(ev) => {
            let mut inner = Map::new();
            if let Some(usage) = ev.usage() {
                let mut u = Map::new();
                u.insert("inputTokens".to_string(), Value::from(usage.input_tokens()));
                u.insert("outputTokens".to_string(), Value::from(usage.output_tokens()));
                u.insert("totalTokens".to_string(), Value::from(usage.total_tokens()));
                if let Some(cr) = usage.cache_read_input_tokens() {
                    u.insert("cacheReadInputTokens".to_string(), Value::from(cr));
                }
                if let Some(cw) = usage.cache_write_input_tokens() {
                    u.insert("cacheWriteInputTokens".to_string(), Value::from(cw));
                }
                if !usage.cache_details().is_empty() {
                    let details: Vec<Value> = usage
                        .cache_details()
                        .iter()
                        .map(|d| {
                            let mut dm = Map::new();
                            dm.insert("ttl".to_string(), Value::String(d.ttl().as_str().to_string()));
                            dm.insert("inputTokens".to_string(), Value::from(d.input_tokens()));
                            Value::Object(dm)
                        })
                        .collect();
                    u.insert("cacheDetails".to_string(), Value::Array(details));
                }
                inner.insert("usage".to_string(), Value::Object(u));
            }
            root.insert("metadata".to_string(), Value::Object(inner));
        }
        _ => {}
    }
    Value::Object(root)
}

fn stream_error_to_json(code: Option<&str>, message: &str) -> Value {
    let field_name = match code {
        Some("InternalServerException") => "internalServerException",
        Some("ModelStreamErrorException") => "modelStreamErrorException",
        Some("ValidationException") => "validationException",
        Some("ThrottlingException") => "throttlingException",
        Some("ServiceUnavailableException") => "serviceUnavailableException",
        _ => "internalServerException",
    };
    let mut inner = Map::new();
    inner.insert("message".to_string(), Value::String(message.to_string()));
    let mut root = Map::new();
    root.insert(field_name.to_string(), Value::Object(inner));
    Value::Object(root)
}

fn handle_content_block_start(
    start_val: &aws_sdk_bedrockruntime::types::ContentBlockStartEvent,
    scratches: &mut Vec<BlockScratch>,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) {
    let index = start_val.content_block_index();
    let Some(start) = start_val.start() else {
        return;
    };
    if let Ok(tu) = start.as_tool_use() {
        let block = ToolCall::new(tu.tool_use_id(), tu.name(), JsonObject::new());
        output.content.push(AssistantContentBlock::ToolCall(block));
        scratches.push(BlockScratch {
            index: Some(index),
            partial_json: Some(String::new()),
            redacted_chunks: None,
        });
        let content_index = output.content.len() - 1;
        stream.push(AssistantMessageEvent::ToolCallStart {
            content_index,
            partial: output.clone(),
        });
    }
}

fn handle_content_block_delta(
    event: &aws_sdk_bedrockruntime::types::ContentBlockDeltaEvent,
    scratches: &mut Vec<BlockScratch>,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) {
    let content_block_index = event.content_block_index();
    let Some(delta) = event.delta() else {
        return;
    };
    let found_idx = scratches.iter().position(|s| s.index == Some(content_block_index));

    if let Ok(delta_text) = delta.as_text() {
        let index = match found_idx {
            Some(idx) => idx,
            None => {
                output.content.push(AssistantContentBlock::Text(TextContent::new("")));
                scratches.push(BlockScratch {
                    index: Some(content_block_index),
                    partial_json: None,
                    redacted_chunks: None,
                });
                let idx = output.content.len() - 1;
                stream.push(AssistantMessageEvent::TextStart {
                    content_index: idx,
                    partial: output.clone(),
                });
                idx
            }
        };
        if let Some(AssistantContentBlock::Text(tb)) = output.content.get_mut(index) {
            tb.text.push_str(delta_text);
            stream.push(AssistantMessageEvent::TextDelta {
                content_index: index,
                delta: delta_text.clone(),
                partial: output.clone(),
            });
        }
    } else if let Ok(tool_use_delta) = delta.as_tool_use() {
        if let Some(index) = found_idx
            && let Some(AssistantContentBlock::ToolCall(tc)) = output.content.get_mut(index)
        {
            let input_delta = tool_use_delta.input();
            let buf = scratches[index].partial_json.get_or_insert_with(String::new);
            buf.push_str(input_delta);
            tc.arguments = parse_streaming_json(Some(buf.as_str()));
            stream.push(AssistantMessageEvent::ToolCallDelta {
                content_index: index,
                delta: input_delta.to_string(),
                partial: output.clone(),
            });
        }
    } else if let Ok(rc_delta) = delta.as_reasoning_content() {
        let thinking_index = match found_idx {
            Some(idx) => idx,
            None => {
                output.content.push(AssistantContentBlock::Thinking(ThinkingContent {
                    thinking: String::new(),
                    thinking_signature: Some(String::new()),
                    redacted: None,
                }));
                scratches.push(BlockScratch {
                    index: Some(content_block_index),
                    partial_json: None,
                    redacted_chunks: None,
                });
                let idx = output.content.len() - 1;
                stream.push(AssistantMessageEvent::ThinkingStart {
                    content_index: idx,
                    partial: output.clone(),
                });
                idx
            }
        };

        if let Some(AssistantContentBlock::Thinking(th)) = output.content.get_mut(thinking_index) {
            let mut emitted_delta: Option<String> = None;
            match rc_delta {
                ReasoningContentBlockDelta::Text(text) => {
                    if !text.is_empty() {
                        th.thinking.push_str(text);
                        emitted_delta = Some(text.clone());
                    }
                }
                ReasoningContentBlockDelta::Signature(sig) => {
                    if !sig.is_empty() && th.redacted != Some(true) {
                        th.thinking_signature.get_or_insert_with(String::new).push_str(sig);
                    }
                }
                ReasoningContentBlockDelta::RedactedContent(blob) => {
                    let bytes = blob.as_ref();
                    if !bytes.is_empty() {
                        if th.redacted != Some(true) {
                            th.redacted = Some(true);
                            th.thinking_signature = Some(String::new());
                            th.thinking.push_str(REDACTED_THINKING_PLACEHOLDER);
                            emitted_delta = Some(REDACTED_THINKING_PLACEHOLDER.to_string());
                        }
                        scratches[thinking_index]
                            .redacted_chunks
                            .get_or_insert_with(Vec::new)
                            .push(bytes.to_vec());
                    }
                }
                _ => {}
            }
            if let Some(delta_str) = emitted_delta {
                stream.push(AssistantMessageEvent::ThinkingDelta {
                    content_index: thinking_index,
                    delta: delta_str,
                    partial: output.clone(),
                });
            }
        }
    }
}

/// Encodes buffered encrypted reasoning into `thinking_signature` and drops the scratch
/// buffer, mirroring TS `flushRedactedContent`.
fn flush_redacted_content(block: &mut AssistantContentBlock, scratch: &mut BlockScratch) {
    let AssistantContentBlock::Thinking(th) = block else {
        return;
    };
    let Some(chunks) = scratch.redacted_chunks.take() else {
        return;
    };
    th.thinking_signature = Some(encode_redacted_content(&chunks));
}

/// Strips every streaming scratch field. Runs from the terminal paths as well as
/// `contentBlockStop`, because a stream can settle without stopping each block.
fn finalize_streaming_block(block: &mut AssistantContentBlock, scratch: &mut BlockScratch) {
    scratch.index = None;
    scratch.partial_json = None;
    flush_redacted_content(block, scratch);
}

fn handle_content_block_stop(
    event: &aws_sdk_bedrockruntime::types::ContentBlockStopEvent,
    scratches: &mut [BlockScratch],
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) {
    let Some(index) = scratches
        .iter()
        .position(|s| s.index == Some(event.content_block_index()))
    else {
        return;
    };
    scratches[index].index = None;

    let Some(block) = output.content.get_mut(index) else {
        return;
    };
    match block {
        AssistantContentBlock::Text(tb) => {
            let content = tb.text.clone();
            stream.push(AssistantMessageEvent::TextEnd {
                content_index: index,
                content,
                partial: output.clone(),
            });
        }
        AssistantContentBlock::Thinking(_) => {
            flush_redacted_content(block, &mut scratches[index]);
            let content = match block {
                AssistantContentBlock::Thinking(th) => th.thinking.clone(),
                _ => String::new(),
            };
            stream.push(AssistantMessageEvent::ThinkingEnd {
                content_index: index,
                content,
                partial: output.clone(),
            });
        }
        AssistantContentBlock::ToolCall(tc) => {
            let partial_json = scratches[index].partial_json.take();
            tc.arguments = parse_streaming_json(partial_json.as_deref());
            let finalized = tc.clone();
            stream.push(AssistantMessageEvent::ToolCallEnd {
                content_index: index,
                tool_call: finalized,
                partial: output.clone(),
            });
        }
    }
}

fn handle_metadata(event: &ConverseStreamMetadataEvent, model: &Model, output: &mut AssistantMessage) {
    let Some(usage) = event.usage() else {
        return;
    };
    output.usage.input = i64::from(usage.input_tokens());
    output.usage.output = i64::from(usage.output_tokens());
    output.usage.cache_read = i64::from(usage.cache_read_input_tokens().unwrap_or(0));
    output.usage.cache_write = i64::from(usage.cache_write_input_tokens().unwrap_or(0));
    output.usage.cache_write_1h = if usage.cache_details().is_empty() {
        None
    } else {
        Some(
            usage
                .cache_details()
                .iter()
                .filter(|detail| *detail.ttl() == CacheTtl::OneHour)
                .map(|detail| i64::from(detail.input_tokens()))
                .sum(),
        )
    };
    let total = i64::from(usage.total_tokens());
    output.usage.total_tokens = if total != 0 {
        total
    } else {
        output.usage.input + output.usage.output
    };
    calculate_cost(&AnyModel::Chat(model.clone()), &mut output.usage);
}

fn map_stop_reason(reason: &BedrockStopReason) -> (StopReason, Option<String>) {
    match reason {
        BedrockStopReason::EndTurn | BedrockStopReason::StopSequence => (StopReason::Stop, None),
        BedrockStopReason::MaxTokens | BedrockStopReason::ModelContextWindowExceeded => (StopReason::Length, None),
        BedrockStopReason::ToolUse => (StopReason::ToolUse, None),
        other => (
            StopReason::Error,
            Some(format!("Provider stopped with: {}", other.as_str())),
        ),
    }
}

/// Human-readable prefixes for Bedrock SDK exception names.
fn bedrock_error_prefix(name: &str) -> &str {
    match name {
        "InternalServerException" => "Internal server error",
        "ModelStreamErrorException" => "Model stream error",
        "ValidationException" => "Validation error",
        "ThrottlingException" => "Throttling error",
        "ServiceUnavailableException" => "Service unavailable",
        other => other,
    }
}

#[derive(Clone, Debug)]
struct BedrockFailureInfo {
    message: String,
    status: Option<u16>,
    body: Option<String>,
    error_code: Option<String>,
    request_id: Option<String>,
    is_service_exception: bool,
}

impl From<BedrockTransportError> for BedrockFailureInfo {
    fn from(value: BedrockTransportError) -> Self {
        Self {
            message: value.message,
            status: value.status,
            body: value.body,
            error_code: value.error_code,
            request_id: value.request_id,
            is_service_exception: value.is_service_exception,
        }
    }
}

impl From<Error> for BedrockFailureInfo {
    fn from(err: Error) -> Self {
        let norm = normalize_provider_error(&err);
        let name = err.name();
        let error_code = (!name.is_empty() && name != "Error").then(|| name.to_string());
        Self {
            message: norm.message,
            status: norm.status,
            body: norm.body,
            error_code,
            request_id: None,
            is_service_exception: false,
        }
    }
}

fn extract_request_id_from_response(raw: &aws_sdk_bedrockruntime::config::http::HttpResponse) -> Option<String> {
    raw.headers()
        .get("x-amzn-requestid")
        .or_else(|| raw.headers().get("x-amz-request-id"))
        .map(str::to_string)
}

fn failure_from_converse_stream_sdk_error(
    err: SdkError<ConverseStreamError, aws_sdk_bedrockruntime::config::http::HttpResponse>,
) -> BedrockFailureInfo {
    match &err {
        SdkError::ServiceError(ctx) => {
            let service_err = ctx.err();
            let meta = ProvideErrorMetadata::meta(service_err);
            let raw = ctx.raw();
            let status = Some(raw.status().as_u16());
            let body = raw
                .body()
                .bytes()
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(str::to_string)
                .filter(|s| !s.trim().is_empty());
            let error_code = meta.code().map(str::to_string);
            let message = meta
                .message()
                .map(str::to_string)
                .unwrap_or_else(|| service_err.to_string());
            let request_id = meta
                .request_id()
                .map(str::to_string)
                .or_else(|| extract_request_id_from_response(raw));
            BedrockFailureInfo {
                message,
                status,
                body,
                error_code,
                request_id,
                is_service_exception: true,
            }
        }
        SdkError::ResponseError(ctx) => {
            let raw = ctx.raw();
            let status = Some(raw.status().as_u16());
            let body = raw
                .body()
                .bytes()
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(str::to_string)
                .filter(|s| !s.trim().is_empty());
            let request_id = extract_request_id_from_response(raw);
            BedrockFailureInfo {
                message: format!("{}", DisplayErrorContext(&err)),
                status,
                body,
                error_code: None,
                request_id,
                is_service_exception: false,
            }
        }
        _ => BedrockFailureInfo {
            message: format!("{}", DisplayErrorContext(&err)),
            status: None,
            body: None,
            error_code: None,
            request_id: None,
            is_service_exception: false,
        },
    }
}

fn failure_from_stream_output_error(
    err: SdkError<ConverseStreamOutputError, aws_sdk_bedrockruntime::primitives::event_stream::RawMessage>,
) -> BedrockFailureInfo {
    match &err {
        SdkError::ServiceError(ctx) => {
            let service_err = ctx.err();
            let meta = ProvideErrorMetadata::meta(service_err);
            let error_code = meta.code().map(str::to_string);
            let message = meta
                .message()
                .map(str::to_string)
                .unwrap_or_else(|| service_err.to_string());
            let request_id = meta.request_id().map(str::to_string);
            BedrockFailureInfo {
                message,
                status: None,
                body: None,
                error_code,
                request_id,
                is_service_exception: true,
            }
        }
        _ => BedrockFailureInfo {
            message: format!("{}", DisplayErrorContext(&err)),
            status: None,
            body: None,
            error_code: None,
            request_id: None,
            is_service_exception: false,
        },
    }
}

fn contains_ascii_case_insensitive(haystack: &str, needle: &str) -> bool {
    haystack.to_ascii_lowercase().contains(&needle.to_ascii_lowercase())
}

fn format_bedrock_failure(failure: &BedrockFailureInfo) -> String {
    let sdk_err = SdkErrorInfo {
        status_code: failure.status,
        metadata_http_status_code: failure.status,
        body: failure.body.clone(),
        message: failure.message.clone(),
        ..SdkErrorInfo::default()
    }
    .into_js_error();
    let norm = normalize_provider_error(&sdk_err);
    let core = match (norm.message_carries_body, norm.status, norm.body.as_deref()) {
        (false, Some(status), Some(body)) => format!("{status}: {body}"),
        _ => norm.message,
    };
    let data_retention_hint = if contains_ascii_case_insensitive(&core, "data retention mode") {
        format!(" See {BEDROCK_DATA_RETENTION_DOCS_URL} for supported data retention modes.")
    } else {
        String::new()
    };
    if failure.is_service_exception
        && let Some(code) = failure.error_code.as_deref()
    {
        let prefix = bedrock_error_prefix(code);
        return format!("{prefix}: {core}{data_retention_hint}");
    }
    format!("{core}{data_retention_hint}")
}

fn normalize_diagnostic_value(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_BEDROCK_DIAGNOSTIC_VALUE_CHARS {
        return None;
    }
    Some(trimmed.to_string())
}

fn extract_bedrock_error_code(error_code: Option<&str>) -> Option<String> {
    let code = error_code?;
    if code == "Unknown" || !code.ends_with("Exception") {
        return None;
    }
    normalize_diagnostic_value(Some(code))
}

fn append_bedrock_failure_diagnostic(
    output: &mut AssistantMessage,
    failure: &BedrockFailureInfo,
    fallback_request_id: Option<&str>,
) {
    let mut details = JsonObject::new();
    if let Some(status) = failure.status {
        details.insert("status".to_string(), JsonValue::from(status));
    }
    if let Some(code) = extract_bedrock_error_code(failure.error_code.as_deref()) {
        details.insert("errorCode".to_string(), JsonValue::String(code));
    }
    let request_id =
        normalize_diagnostic_value(failure.request_id.as_deref()).or_else(|| fallback_request_id.map(str::to_string));
    if let Some(req_id) = request_id {
        details.insert("requestId".to_string(), JsonValue::String(req_id));
    }
    if details.is_empty() {
        return;
    }
    append_assistant_message_diagnostic(
        output,
        AssistantMessageDiagnostic {
            r#type: "bedrock_response_failure".to_string(),
            timestamp: time::now_ms(),
            error: None,
            details: Some(details),
        },
    );
}

async fn build_bedrock_sdk_client(
    resolved: &BedrockClientConfig,
    custom_headers: Option<&IndexMap<String, String>>,
    response_capture: Option<Arc<Mutex<Option<ProviderResponse>>>>,
) -> BedrockRuntimeClient {
    let mut builder = if resolved.skip_auth {
        // Build directly with explicit dummy credentials so ambient host env vars
        // (such as AWS_BEARER_TOKEN_BEDROCK) cannot trigger bearer auth when skip_auth=1.
        let mut b = BedrockSdkConfig::builder().behavior_version(BehaviorVersion::latest());
        if let Some(creds) = &resolved.credentials {
            b = b.credentials_provider(Credentials::new(
                creds.access_key_id.clone(),
                creds.secret_access_key.clone(),
                creds.session_token.clone(),
                None,
                "pi-ai",
            ));
        }
        b
    } else if let Some(token) = &resolved.bearer_token {
        // Use no_credentials() so DefaultAuthSchemeResolver skips SigV4 (NoIdentityResolver)
        // and selects HttpBearerAuth with our token even if ambient AWS creds exist.
        let mut loader = aws_config::defaults(BehaviorVersion::latest()).no_credentials();
        if let Some(region) = &resolved.region {
            loader = loader.region(aws_config::Region::new(region.clone()));
        }
        let sdk_config = loader.load().await;
        aws_sdk_bedrockruntime::config::Builder::from(&sdk_config).bearer_token(Token::new(token.clone(), None))
    } else if !resolved.has_options_profile
        && let Some(creds) = &resolved.credentials
    {
        let mut b = BedrockSdkConfig::builder().behavior_version(BehaviorVersion::latest());
        b = b.credentials_provider(Credentials::new(
            creds.access_key_id.clone(),
            creds.secret_access_key.clone(),
            creds.session_token.clone(),
            None,
            "pi-ai",
        ));
        b
    } else {
        let mut loader = aws_config::defaults(BehaviorVersion::latest());
        if let Some(profile) = &resolved.profile {
            loader = loader.profile_name(profile);
        }
        if let Some(region) = &resolved.region {
            loader = loader.region(aws_config::Region::new(region.clone()));
        }
        let sdk_config = loader.load().await;
        aws_sdk_bedrockruntime::config::Builder::from(&sdk_config)
    };

    if let Some(region) = &resolved.region {
        builder = builder.region(Region::new(region.clone()));
    }
    if let Some(endpoint) = &resolved.endpoint {
        builder = builder.endpoint_url(endpoint.clone());
    }
    if let Some(timeout_ms) = resolved.timeout_ms {
        builder = builder.timeout_config(
            TimeoutConfig::builder()
                .operation_timeout(Duration::from_millis(timeout_ms))
                .build(),
        );
    }
    // PORT: `aws-sdk-bedrockruntime` enables `default-https-client` (hyper 1.x supporting
    // HTTP/1.1 on plaintext HTTP and ALPN on TLS) and proxy env resolution via `hyper-util`;
    // `aws-smithy-http-client` / `aws-smithy-runtime-api` are not direct Cargo deps of `pi-ai`,
    // so `proxy_url` and `force_http1` are preserved in `BedrockClientConfig` rather than
    // constructing an explicit `HttpConnector`.
    if let Some(slot) = response_capture {
        builder = builder.interceptor(BedrockResponseHeadersInterceptor::new(slot));
    }
    if let Some(headers) = custom_headers
        && !headers.is_empty()
    {
        builder = builder.interceptor(BedrockCustomHeadersInterceptor::new(headers.clone()));
    }

    BedrockRuntimeClient::from_conf(builder.build())
}

fn check_aborted(signal: Option<&pi_js::AbortSignal>) -> Result<()> {
    if let Some(sig) = signal
        && sig.aborted()
    {
        return Err(Error::msg("Request was aborted"));
    }
    Ok(())
}

async fn race_abort<T>(
    signal: Option<&pi_js::AbortSignal>,
    fut: impl std::future::Future<Output = T>,
) -> std::result::Result<T, Error> {
    match signal {
        Some(sig) => {
            if sig.aborted() {
                return Err(Error::msg("Request was aborted"));
            }
            tokio::select! {
                _ = sig.cancelled() => Err(Error::msg("Request was aborted")),
                val = fut => Ok(val),
            }
        }
        None => Ok(fut.await),
    }
}

fn finalize_terminal_failure(
    scratches: &mut [BlockScratch],
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    signal: Option<&pi_js::AbortSignal>,
    failure: &BedrockFailureInfo,
    response_request_id: Option<&str>,
) {
    for (block, scratch) in output.content.iter_mut().zip(scratches.iter_mut()) {
        finalize_streaming_block(block, scratch);
    }
    let aborted = signal.is_some_and(pi_js::AbortSignal::aborted);
    output.stop_reason = if aborted {
        StopReason::Aborted
    } else {
        StopReason::Error
    };
    output.error_message = Some(format_bedrock_failure(failure));
    if output.stop_reason == StopReason::Error {
        append_bedrock_failure_diagnostic(output, failure, response_request_id);
    }
    stream.push(AssistantMessageEvent::Error {
        reason: output.stop_reason,
        error: output.clone(),
    });
    stream.end(None);
}

#[allow(clippy::too_many_arguments)]
async fn run_bedrock_stream(
    model: Model,
    normalized_context: TranscriptContext,
    options: BedrockOptions,
    pre_resolved_config: Result<BedrockClientConfig>,
    cache_retention: CacheRetention,
    transport_override: Option<Arc<BedrockTransportFn>>,
    stream: AssistantMessageEventStream,
) {
    let mut output = AssistantMessage {
        content: Vec::new(),
        api: Api::from(KnownApi::BedrockConverseStream),
        provider: model.provider.clone(),
        model: model.id.clone(),
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
        timestamp: time::now_ms(),
    };
    let mut scratches: Vec<BlockScratch> = Vec::new();
    let mut response_request_id: Option<String> = None;

    let step: std::result::Result<(), BedrockFailureInfo> = async {
        let client_config = pre_resolved_config.map_err(BedrockFailureInfo::from)?;
        let custom_headers = provider_headers_to_record([options.base.headers.as_ref()]);
        let response_capture = options.base.on_response.as_ref().map(|_| Arc::new(Mutex::new(None)));

        let mut interceptor_names = Vec::new();
        if response_capture.is_some() {
            interceptor_names.push(RESPONSE_HEADERS_INTERCEPTOR_NAME.to_string());
        }
        if custom_headers.as_ref().is_some_and(|h| !h.is_empty()) {
            interceptor_names.push(CUSTOM_HEADERS_INTERCEPTOR_NAME.to_string());
        }

        let supports_strict_mode = model
            .compat
            .as_ref()
            .and_then(|c| c.as_bedrock())
            .and_then(|b| b.supports_strict_mode)
            .unwrap_or(false);
        let inference_max_tokens = options.base.max_tokens.or_else(|| {
            if is_anthropic_claude_model(&model) {
                Some(model.max_tokens)
            } else {
                None
            }
        });
        let initial_system_prompt =
            get_initial_system_message(&normalized_context.messages).map(get_system_message_text);

        let mut cmd_map = Map::new();
        cmd_map.insert("modelId".to_string(), Value::String(model.id.clone()));
        cmd_map.insert(
            "messages".to_string(),
            Value::Array(
                convert_messages(&normalized_context, &model, cache_retention, options.base.env.as_ref())
                    .map_err(BedrockFailureInfo::from)?,
            ),
        );
        if let Some(sys) = build_system_prompt(
            initial_system_prompt.as_deref(),
            &model,
            cache_retention,
            options.base.env.as_ref(),
        ) {
            cmd_map.insert("system".to_string(), Value::Array(sys));
        }
        let mut ic_map = Map::new();
        if let Some(max_tok) = inference_max_tokens {
            ic_map.insert("maxTokens".to_string(), Value::from(max_tok));
        }
        if let Some(temp) = options.base.temperature {
            if let Some(num) = serde_json::Number::from_f64(temp) {
                ic_map.insert("temperature".to_string(), Value::Number(num));
            }
        }
        cmd_map.insert("inferenceConfig".to_string(), Value::Object(ic_map));

        let current_tools = get_current_tools(&normalized_context.messages);
        if let Some(tc) = convert_tool_config(&current_tools, options.tool_choice.as_ref(), supports_strict_mode)
            .map_err(BedrockFailureInfo::from)?
        {
            cmd_map.insert("toolConfig".to_string(), tc);
        }
        if let Some(additional) = build_additional_model_request_fields(&model, &options) {
            cmd_map.insert("additionalModelRequestFields".to_string(), additional);
        }
        if let Some(req_meta) = &options.request_metadata {
            let mut meta_map = Map::new();
            for (k, v) in req_meta {
                meta_map.insert(k.clone(), Value::String(v.clone()));
            }
            cmd_map.insert("requestMetadata".to_string(), Value::Object(meta_map));
        }

        let mut command_input = Value::Object(cmd_map);
        if let Some(on_payload) = &options.base.on_payload {
            let next = on_payload(command_input.clone(), &model)
                .await
                .map_err(BedrockFailureInfo::from)?;
            if let Some(replaced) = next {
                command_input = replaced;
            }
        }

        let input = value_to_converse_stream_input(&command_input).map_err(BedrockFailureInfo::from)?;

        if let Some(transport) = transport_override {
            let outcome = race_abort(
                options.base.signal.as_ref(),
                transport(BedrockTransportRequest {
                    client_config,
                    custom_headers,
                    interceptor_names,
                    command_input,
                    input,
                }),
            )
            .await
            .map_err(BedrockFailureInfo::from)?
            .map_err(BedrockFailureInfo::from)?;

            match outcome {
                BedrockTransportOutcome::Error(err) => return Err(err.into()),
                BedrockTransportOutcome::Stream(resp) => {
                    response_request_id = normalize_diagnostic_value(resp.request_id.as_deref());
                    if let Some(on_response) = &options.base.on_response {
                        if let Some(status) = resp.http_status {
                            let mut headers = resp.headers.clone();
                            if headers.is_empty()
                                && let Some(req_id) = &resp.request_id
                            {
                                headers.insert("x-amzn-requestid".to_string(), req_id.clone());
                            }
                            on_response(ProviderResponse { status, headers }, &model)
                                .await
                                .map_err(BedrockFailureInfo::from)?;
                        }
                    }

                    for item in resp.events {
                        check_aborted(options.base.signal.as_ref()).map_err(BedrockFailureInfo::from)?;
                        match item {
                            BedrockTransportEvent::Sdk(ev) => {
                                if let Some(on_event) = &options.base.on_provider_stream_event {
                                    let raw = converse_stream_output_to_json(&ev);
                                    on_event(&raw, &model).await.map_err(BedrockFailureInfo::from)?;
                                }
                                process_converse_event(&ev, &model, &mut scratches, &mut output, &stream)
                                    .map_err(BedrockFailureInfo::from)?;
                            }
                            BedrockTransportEvent::StreamError(stream_err) => {
                                if let Some(on_event) = &options.base.on_provider_stream_event {
                                    let raw =
                                        stream_error_to_json(stream_err.error_code.as_deref(), &stream_err.message);
                                    on_event(&raw, &model).await.map_err(BedrockFailureInfo::from)?;
                                }
                                return Err(stream_err.into());
                            }
                        }
                    }
                }
            }
        } else {
            check_aborted(options.base.signal.as_ref()).map_err(BedrockFailureInfo::from)?;
            let client =
                build_bedrock_sdk_client(&client_config, custom_headers.as_ref(), response_capture.clone()).await;

            let send_fut = client
                .converse_stream()
                .set_model_id(input.model_id)
                .set_messages(input.messages)
                .set_system(input.system)
                .set_inference_config(input.inference_config)
                .set_tool_config(input.tool_config)
                .set_additional_model_request_fields(input.additional_model_request_fields)
                .set_request_metadata(input.request_metadata)
                .send();

            let send_res = race_abort(options.base.signal.as_ref(), send_fut)
                .await
                .map_err(BedrockFailureInfo::from)?;

            let mut observed_raw_response = false;
            if let Some(slot) = &response_capture
                && let Some(raw_resp) = take_captured_response(slot)
                && let Some(on_response) = &options.base.on_response
            {
                observed_raw_response = true;
                on_response(raw_resp, &model).await.map_err(BedrockFailureInfo::from)?;
            }

            let mut response = send_res.map_err(failure_from_converse_stream_sdk_error)?;
            let meta_req_id = response_request_id_from_output(&response);
            response_request_id = normalize_diagnostic_value(meta_req_id.as_deref());

            if !observed_raw_response
                && let Some(on_response) = &options.base.on_response
                && let Some(req_id) = meta_req_id
            {
                let mut headers = IndexMap::new();
                headers.insert("x-amzn-requestid".to_string(), req_id);
                on_response(ProviderResponse { status: 200, headers }, &model)
                    .await
                    .map_err(BedrockFailureInfo::from)?;
            }

            loop {
                let next_res = race_abort(options.base.signal.as_ref(), response.stream.recv())
                    .await
                    .map_err(BedrockFailureInfo::from)?;

                match next_res {
                    Ok(Some(ev)) => {
                        if let Some(on_event) = &options.base.on_provider_stream_event {
                            let raw = converse_stream_output_to_json(&ev);
                            on_event(&raw, &model).await.map_err(BedrockFailureInfo::from)?;
                        }
                        process_converse_event(&ev, &model, &mut scratches, &mut output, &stream)
                            .map_err(BedrockFailureInfo::from)?;
                    }
                    Ok(None) => break,
                    Err(err) => {
                        let failure = failure_from_stream_output_error(err);
                        if let Some(on_event) = &options.base.on_provider_stream_event {
                            let raw = stream_error_to_json(failure.error_code.as_deref(), &failure.message);
                            on_event(&raw, &model).await.map_err(BedrockFailureInfo::from)?;
                        }
                        return Err(failure);
                    }
                }
            }
        }

        check_aborted(options.base.signal.as_ref()).map_err(BedrockFailureInfo::from)?;

        if output.stop_reason == StopReason::Pending {
            return Err(BedrockFailureInfo::from(Error::msg(
                "Bedrock stream ended without a stop reason",
            )));
        }
        if matches!(output.stop_reason, StopReason::Error | StopReason::Aborted) {
            return Err(BedrockFailureInfo::from(Error::msg(
                output
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "An unknown error occurred".to_string()),
            )));
        }

        for (block, scratch) in output.content.iter_mut().zip(scratches.iter_mut()) {
            finalize_streaming_block(block, scratch);
        }
        stream.push(AssistantMessageEvent::Done {
            reason: output.stop_reason,
            message: output.clone(),
        });
        stream.end(None);
        Ok(())
    }
    .await;

    if let Err(failure) = step {
        finalize_terminal_failure(
            &mut scratches,
            &mut output,
            &stream,
            options.base.signal.as_ref(),
            &failure,
            response_request_id.as_deref(),
        );
    }
}

fn response_request_id_from_output(
    output: &aws_sdk_bedrockruntime::operation::converse_stream::ConverseStreamOutput,
) -> Option<String> {
    use aws_sdk_bedrockruntime::operation::RequestId;
    output.request_id().map(str::to_string)
}

fn process_converse_event(
    item: &ConverseStreamOutput,
    model: &Model,
    scratches: &mut Vec<BlockScratch>,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) -> Result<()> {
    match item {
        ConverseStreamOutput::MessageStart(ev) => {
            if *ev.role() != ConversationRole::Assistant {
                return Err(Error::msg(
                    "Unexpected assistant message start but got user message start instead",
                ));
            }
            stream.push(AssistantMessageEvent::Start {
                partial: output.clone(),
            });
        }
        ConverseStreamOutput::ContentBlockStart(ev) => {
            handle_content_block_start(ev, scratches, output, stream);
        }
        ConverseStreamOutput::ContentBlockDelta(ev) => {
            handle_content_block_delta(ev, scratches, output, stream);
        }
        ConverseStreamOutput::ContentBlockStop(ev) => {
            handle_content_block_stop(ev, scratches, output, stream);
        }
        ConverseStreamOutput::MessageStop(ev) => {
            let reason = ev.stop_reason();
            output.raw_stop_reason = Some(reason.as_str().to_string());
            let (stop_reason, error_message) = map_stop_reason(reason);
            output.stop_reason = stop_reason;
            if let Some(msg) = error_message {
                output.error_message = Some(msg);
            }
        }
        ConverseStreamOutput::Metadata(ev) => {
            handle_metadata(ev, model, output);
        }
        _ => {}
    }
    Ok(())
}

pub fn stream_with_options(
    model: &Model,
    context: &TranscriptContext,
    options: Option<BedrockOptions>,
) -> AssistantMessageEventStream {
    let stream_out = AssistantMessageEventStream::new();
    let stream_clone = stream_out.clone();
    // Bedrock has no mid-conversation system messages; fold them into the leading prompt.
    let normalized_context = collapse_system_messages(context);
    let options_owned = options.unwrap_or_default();
    let model_owned = model.clone();

    // Resolve thread-local env and test seam synchronously on the caller thread
    // before spawning onto Tokio.
    let pre_resolved_config = resolve_bedrock_client_config(&model_owned, &options_owned);
    let cache_retention = resolve_cache_retention(options_owned.base.cache_retention, options_owned.base.env.as_ref());
    let transport_override = BEDROCK_TRANSPORT.with(seam::Slot::get);

    tokio::spawn(async move {
        run_bedrock_stream(
            model_owned,
            normalized_context,
            options_owned,
            pre_resolved_config,
            cache_retention,
            transport_override,
            stream_clone,
        )
        .await;
    });

    stream_out
}

pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: Option<ApiStreamOptions>,
) -> AssistantMessageEventStream {
    stream_with_options(model, context, Some(BedrockOptions::from_api_options(options)))
}

fn clamp_and_override_budget(
    existing: Option<ThinkingBudgets>,
    level: ThinkingLevel,
    clamped_budget: i64,
) -> ThinkingBudgets {
    let mut out = existing.unwrap_or_default();
    match level {
        ThinkingLevel::Minimal => out.minimal = Some(clamped_budget),
        ThinkingLevel::Low => out.low = Some(clamped_budget),
        ThinkingLevel::Medium => out.medium = Some(clamped_budget),
        ThinkingLevel::High | ThinkingLevel::XHigh | ThinkingLevel::Max => {
            out.high = Some(clamped_budget);
        }
    }
    out
}

pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let base = build_base_options(model, context, options.as_ref(), None);
    let tool_choice = options
        .as_ref()
        .and_then(|o| o.tool_choice)
        .map(BedrockToolChoice::from);

    let Some(reasoning) = options.as_ref().and_then(|o| o.reasoning) else {
        return stream_with_options(
            model,
            context,
            Some(BedrockOptions {
                base,
                tool_choice,
                reasoning: None,
                ..BedrockOptions::default()
            }),
        );
    };

    if is_anthropic_claude_model(model) {
        if supports_adaptive_thinking(&model.id, &model.name) {
            return stream_with_options(
                model,
                context,
                Some(BedrockOptions {
                    base,
                    tool_choice,
                    reasoning: Some(reasoning),
                    thinking_budgets: options.as_ref().and_then(|o| o.thinking_budgets.clone()),
                    ..BedrockOptions::default()
                }),
            );
        }

        // Undefined means the caller did not request an output cap; let the helper use the model cap.
        // Do not coerce to 0 here, or the thinking budget would become the entire maxTokens value.
        let thinking_budgets = options.as_ref().and_then(|o| o.thinking_budgets.as_ref());
        let adjusted = adjust_max_tokens_for_thinking(base.max_tokens, model.max_tokens, reasoning, thinking_budgets);
        let max_tokens = clamp_max_tokens_to_context(model, context, adjusted.max_tokens);
        let clamped_level = clamp_reasoning(Some(reasoning)).unwrap_or(ThinkingLevel::High);
        let capped_budget = adjusted.thinking_budget.min(0_i64.max(max_tokens - 1024));
        let next_budgets = clamp_and_override_budget(
            options.as_ref().and_then(|o| o.thinking_budgets.clone()),
            clamped_level,
            capped_budget,
        );

        let mut next_base = base;
        next_base.max_tokens = Some(max_tokens);
        return stream_with_options(
            model,
            context,
            Some(BedrockOptions {
                base: next_base,
                tool_choice,
                reasoning: Some(reasoning),
                thinking_budgets: Some(next_budgets),
                ..BedrockOptions::default()
            }),
        );
    }

    stream_with_options(
        model,
        context,
        Some(BedrockOptions {
            base,
            tool_choice,
            reasoning: Some(reasoning),
            thinking_budgets: options.as_ref().and_then(|o| o.thinking_budgets.clone()),
            ..BedrockOptions::default()
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
