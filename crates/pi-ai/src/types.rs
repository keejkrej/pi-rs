//! Port of packages/ai/src/types.ts

use std::any::Any;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use indexmap::IndexMap;
use pi_js::abort::AbortSignal;
use pi_js::fetch::{Request as FetchRequest, Response as FetchResponse};
use pi_js::vendor::typebox::Schema as TSchema;
use pi_js::{BoxFuture, Result};
use pi_telemetry::{TelemetryContext as TelemetryContextTrait, TelemetrySpan as TelemetrySpanTrait};
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

use crate::utils::diagnostics::AssistantMessageDiagnostic;
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::identity::ClientIdentity;

// ============================================================================
// JSON Types
// ============================================================================

pub type JsonValue = serde_json::Value;
pub type JsonObject = IndexMap<String, JsonValue>;

fn deserialize_some_value<'de, D>(deserializer: D) -> std::result::Result<Option<JsonValue>, D::Error>
where
    D: Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

// ============================================================================
// Known APIs & Providers
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KnownApi {
    #[serde(rename = "openai-completions")]
    OpenAICompletions,
    #[serde(rename = "mistral-conversations")]
    MistralConversations,
    #[serde(rename = "openai-responses")]
    OpenAIResponses,
    #[serde(rename = "azure-openai-responses")]
    AzureOpenAIResponses,
    #[serde(rename = "openai-codex-responses")]
    OpenAICodexResponses,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    #[serde(rename = "bedrock-converse-stream")]
    BedrockConverseStream,
    #[serde(rename = "google-generative-ai")]
    GoogleGenerativeAi,
    #[serde(rename = "google-vertex")]
    GoogleVertex,
    #[serde(rename = "pi-messages")]
    PiMessages,
}

impl KnownApi {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAICompletions => "openai-completions",
            Self::MistralConversations => "mistral-conversations",
            Self::OpenAIResponses => "openai-responses",
            Self::AzureOpenAIResponses => "azure-openai-responses",
            Self::OpenAICodexResponses => "openai-codex-responses",
            Self::AnthropicMessages => "anthropic-messages",
            Self::BedrockConverseStream => "bedrock-converse-stream",
            Self::GoogleGenerativeAi => "google-generative-ai",
            Self::GoogleVertex => "google-vertex",
            Self::PiMessages => "pi-messages",
        }
    }
}

impl fmt::Display for KnownApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<KnownApi> for String {
    fn from(value: KnownApi) -> Self {
        value.as_str().to_string()
    }
}

pub type Api = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KnownImageApi {
    #[serde(rename = "openrouter-images")]
    OpenRouterImages,
}

impl KnownImageApi {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenRouterImages => "openrouter-images",
        }
    }
}

impl fmt::Display for KnownImageApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<KnownImageApi> for String {
    fn from(value: KnownImageApi) -> Self {
        value.as_str().to_string()
    }
}

pub type ImageApi = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KnownClassifierApi {
    #[serde(rename = "typesafe-system-one")]
    TypesafeSystemOne,
    #[serde(rename = "cloudflare-workers-ai-system-one")]
    CloudflareWorkersAiSystemOne,
    #[serde(rename = "llama-cpp-classify")]
    LlamaCppClassify,
    #[serde(rename = "openai-decisions")]
    OpenAIDecisions,
}

impl KnownClassifierApi {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TypesafeSystemOne => "typesafe-system-one",
            Self::CloudflareWorkersAiSystemOne => "cloudflare-workers-ai-system-one",
            Self::LlamaCppClassify => "llama-cpp-classify",
            Self::OpenAIDecisions => "openai-decisions",
        }
    }
}

impl fmt::Display for KnownClassifierApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<KnownClassifierApi> for String {
    fn from(value: KnownClassifierApi) -> Self {
        value.as_str().to_string()
    }
}

pub type ClassifierApi = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KnownProvider {
    AmazonBedrock,
    AntLing,
    Anthropic,
    Google,
    GoogleVertex,
    Openai,
    Azure,
    OpenaiCodex,
    Radius,
    Typesafe,
    Nvidia,
    Deepseek,
    GithubCopilot,
    Xai,
    Groq,
    Cerebras,
    Openrouter,
    VercelAiGateway,
    Zai,
    ZaiCodingCn,
    Mistral,
    Minimax,
    MinimaxCn,
    Moonshotai,
    MoonshotaiCn,
    Huggingface,
    Fireworks,
    Together,
    Baseten,
    Opencode,
    OpencodeGo,
    KimiCoding,
    Meta,
    CloudflareWorkersAi,
    CloudflareAiGateway,
    QwenTokenPlan,
    QwenTokenPlanCn,
    QwenTokenPlanIndividual,
    QwenTokenPlanIndividualCn,
    Xiaomi,
    XiaomiTokenPlanCn,
    XiaomiTokenPlanAms,
    XiaomiTokenPlanSgp,
}

impl KnownProvider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AmazonBedrock => "amazon-bedrock",
            Self::AntLing => "ant-ling",
            Self::Anthropic => "anthropic",
            Self::Google => "google",
            Self::GoogleVertex => "google-vertex",
            Self::Openai => "openai",
            Self::Azure => "azure",
            Self::OpenaiCodex => "openai-codex",
            Self::Radius => "radius",
            Self::Typesafe => "typesafe",
            Self::Nvidia => "nvidia",
            Self::Deepseek => "deepseek",
            Self::GithubCopilot => "github-copilot",
            Self::Xai => "xai",
            Self::Groq => "groq",
            Self::Cerebras => "cerebras",
            Self::Openrouter => "openrouter",
            Self::VercelAiGateway => "vercel-ai-gateway",
            Self::Zai => "zai",
            Self::ZaiCodingCn => "zai-coding-cn",
            Self::Mistral => "mistral",
            Self::Minimax => "minimax",
            Self::MinimaxCn => "minimax-cn",
            Self::Moonshotai => "moonshotai",
            Self::MoonshotaiCn => "moonshotai-cn",
            Self::Huggingface => "huggingface",
            Self::Fireworks => "fireworks",
            Self::Together => "together",
            Self::Baseten => "baseten",
            Self::Opencode => "opencode",
            Self::OpencodeGo => "opencode-go",
            Self::KimiCoding => "kimi-coding",
            Self::Meta => "meta",
            Self::CloudflareWorkersAi => "cloudflare-workers-ai",
            Self::CloudflareAiGateway => "cloudflare-ai-gateway",
            Self::QwenTokenPlan => "qwen-token-plan",
            Self::QwenTokenPlanCn => "qwen-token-plan-cn",
            Self::QwenTokenPlanIndividual => "qwen-token-plan-individual",
            Self::QwenTokenPlanIndividualCn => "qwen-token-plan-individual-cn",
            Self::Xiaomi => "xiaomi",
            Self::XiaomiTokenPlanCn => "xiaomi-token-plan-cn",
            Self::XiaomiTokenPlanAms => "xiaomi-token-plan-ams",
            Self::XiaomiTokenPlanSgp => "xiaomi-token-plan-sgp",
        }
    }
}

impl fmt::Display for KnownProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<KnownProvider> for String {
    fn from(value: KnownProvider) -> Self {
        value.as_str().to_string()
    }
}

pub type ProviderId = String;

// ============================================================================
// Thinking, Caching & Transport Options
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    Auto,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ThinkingLevel {
    #[serde(rename = "minimal")]
    Minimal,
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "high")]
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    #[serde(rename = "max")]
    Max,
}

impl ThinkingLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

impl fmt::Display for ThinkingLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ModelThinkingLevel {
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "minimal")]
    Minimal,
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "high")]
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    #[serde(rename = "max")]
    Max,
}

impl ModelThinkingLevel {
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
        Self::Max,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub const fn as_thinking_level(self) -> Option<ThinkingLevel> {
        match self {
            Self::Off => None,
            Self::Minimal => Some(ThinkingLevel::Minimal),
            Self::Low => Some(ThinkingLevel::Low),
            Self::Medium => Some(ThinkingLevel::Medium),
            Self::High => Some(ThinkingLevel::High),
            Self::XHigh => Some(ThinkingLevel::XHigh),
            Self::Max => Some(ThinkingLevel::Max),
        }
    }
}

impl fmt::Display for ModelThinkingLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<ThinkingLevel> for ModelThinkingLevel {
    fn from(value: ThinkingLevel) -> Self {
        match value {
            ThinkingLevel::Minimal => Self::Minimal,
            ThinkingLevel::Low => Self::Low,
            ThinkingLevel::Medium => Self::Medium,
            ThinkingLevel::High => Self::High,
            ThinkingLevel::XHigh => Self::XHigh,
            ThinkingLevel::Max => Self::Max,
        }
    }
}

pub type SamplingParams = JsonObject;
pub type SamplingParamsByThinkingLevel = IndexMap<ModelThinkingLevel, SamplingParams>;
pub type ThinkingLevelMap = IndexMap<ModelThinkingLevel, Option<String>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChatTemplateVarKind {
    #[serde(rename = "thinking.enabled")]
    ThinkingEnabled,
    #[serde(rename = "thinking.effort")]
    ThinkingEffort,
    #[serde(rename = "thinking.budget")]
    ThinkingBudget,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatTemplateKwargValue {
    Var {
        #[serde(rename = "$var")]
        var: ChatTemplateVarKind,
        #[serde(rename = "omitWhenOff", default, skip_serializing_if = "Option::is_none")]
        omit_when_off: Option<bool>,
    },
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
}

/// Top-level request field used to cap reasoning tokens on OpenAI-compatible servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ThinkingTokenBudgetField {
    #[serde(rename = "thinking_token_budget")]
    ThinkingTokenBudget,
    #[serde(rename = "thinking_budget")]
    ThinkingBudget,
    #[serde(rename = "thinking_budget_tokens")]
    ThinkingBudgetTokens,
}

impl ThinkingTokenBudgetField {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ThinkingTokenBudget => "thinking_token_budget",
            Self::ThinkingBudget => "thinking_budget",
            Self::ThinkingBudgetTokens => "thinking_budget_tokens",
        }
    }
}

/// Token budgets for each thinking level (token-based providers only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingBudgets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimal: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medium: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheRetention {
    None,
    Short,
    Long,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct ModelPromptCache {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long: Option<i64>,
}

impl ModelPromptCache {
    pub fn get(&self, retention: CacheRetention) -> Option<i64> {
        match retention {
            CacheRetention::None => None,
            CacheRetention::Short => self.short,
            CacheRetention::Long => self.long,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Transport {
    #[serde(rename = "sse")]
    Sse,
    #[serde(rename = "websocket")]
    Websocket,
    #[serde(rename = "websocket-cached")]
    WebsocketCached,
    #[serde(rename = "auto")]
    Auto,
}

/// Provider-scoped environment overrides. Values take precedence over process env.
pub type ProviderEnv = IndexMap<String, String>;
pub type ProviderHeaders = IndexMap<String, Option<String>>;
pub type FetchFunction = Arc<dyn Fn(FetchRequest) -> BoxFuture<Result<FetchResponse>> + Send + Sync>;
pub type TelemetryContextHandle = Arc<dyn TelemetryContextTrait + Send + Sync>;
pub type TelemetrySpanHandle = Arc<dyn TelemetrySpanTrait + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionAffinityFormat {
    #[serde(rename = "openai")]
    OpenAI,
    #[serde(rename = "openai-nosession")]
    OpenAINosession,
    #[serde(rename = "openrouter")]
    OpenRouter,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderResponse {
    pub status: u16,
    pub headers: IndexMap<String, String>,
}

pub type OnPayloadFn<TModel> = Arc<dyn Fn(JsonValue, &TModel) -> BoxFuture<Result<Option<JsonValue>>> + Send + Sync>;
pub type OnResponseFn<TModel> = Arc<dyn Fn(ProviderResponse, &TModel) -> BoxFuture<Result<()>> + Send + Sync>;
pub type OnProviderStreamEventFn = Arc<dyn Fn(&JsonValue, &Model) -> BoxFuture<Result<()>> + Send + Sync>;

/// Authentication, HTTP transport, and lifecycle callbacks shared by provider requests.
pub struct ProviderRequestOptions<TModel = Model> {
    pub signal: Option<AbortSignal>,
    /// Legacy v1.0.0 telemetry context handle preserved for additive compatibility until Stage 2 migrates `src/api/*`.
    pub telemetry_context: Option<TelemetryContextHandle>,
    /// Explicit parent span for telemetry produced by this logical request (`v1.5.2`).
    pub telemetry: Option<TelemetrySpanHandle>,
    pub api_key: Option<String>,
    pub fetch: Option<FetchFunction>,
    pub env: Option<ProviderEnv>,
    pub on_payload: Option<OnPayloadFn<TModel>>,
    pub on_response: Option<OnResponseFn<TModel>>,
    pub headers: Option<ProviderHeaders>,
    pub identity: Option<ClientIdentity>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
}

impl<TModel> Clone for ProviderRequestOptions<TModel> {
    fn clone(&self) -> Self {
        Self {
            signal: self.signal.clone(),
            telemetry_context: self.telemetry_context.clone(),
            telemetry: self.telemetry.clone(),
            api_key: self.api_key.clone(),
            fetch: self.fetch.clone(),
            env: self.env.clone(),
            on_payload: self.on_payload.clone(),
            on_response: self.on_response.clone(),
            headers: self.headers.clone(),
            identity: self.identity.clone(),
            timeout_ms: self.timeout_ms,
            max_retries: self.max_retries,
            max_retry_delay_ms: self.max_retry_delay_ms,
        }
    }
}

impl<TModel> Default for ProviderRequestOptions<TModel> {
    fn default() -> Self {
        Self {
            signal: None,
            telemetry_context: None,
            telemetry: None,
            api_key: None,
            fetch: None,
            env: None,
            on_payload: None,
            on_response: None,
            headers: None,
            identity: None,
            timeout_ms: None,
            max_retries: None,
            max_retry_delay_ms: None,
        }
    }
}

impl<TModel> fmt::Debug for ProviderRequestOptions<TModel> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderRequestOptions")
            .field("signal", &self.signal.as_ref().map(|s| s.aborted()))
            .field("telemetry_context", &self.telemetry_context.is_some())
            .field("telemetry", &self.telemetry.is_some())
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("fetch", &self.fetch.is_some())
            .field("env", &self.env)
            .field("on_payload", &self.on_payload.is_some())
            .field("on_response", &self.on_response.is_some())
            .field("headers", &self.headers)
            .field("identity", &self.identity)
            .field("timeout_ms", &self.timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("max_retry_delay_ms", &self.max_retry_delay_ms)
            .finish()
    }
}

#[derive(Clone, Default)]
pub struct StreamOptions {
    pub signal: Option<AbortSignal>,
    /// Legacy v1.0.0 telemetry context handle preserved for additive compatibility until Stage 2 migrates `src/api/*`.
    pub telemetry_context: Option<TelemetryContextHandle>,
    /// Explicit parent span for telemetry produced by this logical request (`v1.5.2`).
    pub telemetry: Option<TelemetrySpanHandle>,
    pub api_key: Option<String>,
    pub fetch: Option<FetchFunction>,
    pub env: Option<ProviderEnv>,
    pub on_payload: Option<OnPayloadFn<Model>>,
    pub on_response: Option<OnResponseFn<Model>>,
    pub headers: Option<ProviderHeaders>,
    pub identity: Option<ClientIdentity>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
    pub on_provider_stream_event: Option<OnProviderStreamEventFn>,
    pub temperature: Option<f64>,
    pub sampling_params: Option<SamplingParams>,
    pub max_tokens: Option<i64>,
    pub transport: Option<Transport>,
    pub cache_retention: Option<CacheRetention>,
    pub session_id: Option<String>,
    pub disable_continuation: Option<bool>,
    pub websocket_connect_timeout_ms: Option<u64>,
    pub metadata: Option<JsonObject>,
}

impl StreamOptions {
    pub fn as_request_options(&self) -> ProviderRequestOptions<Model> {
        ProviderRequestOptions {
            signal: self.signal.clone(),
            telemetry_context: self.telemetry_context.clone(),
            telemetry: self.telemetry.clone(),
            api_key: self.api_key.clone(),
            fetch: self.fetch.clone(),
            env: self.env.clone(),
            on_payload: self.on_payload.clone(),
            on_response: self.on_response.clone(),
            headers: self.headers.clone(),
            identity: self.identity.clone(),
            timeout_ms: self.timeout_ms,
            max_retries: self.max_retries,
            max_retry_delay_ms: self.max_retry_delay_ms,
        }
    }
}

impl From<ProviderRequestOptions<Model>> for StreamOptions {
    fn from(req: ProviderRequestOptions<Model>) -> Self {
        Self {
            signal: req.signal,
            telemetry_context: req.telemetry_context,
            telemetry: req.telemetry,
            api_key: req.api_key,
            fetch: req.fetch,
            env: req.env,
            on_payload: req.on_payload,
            on_response: req.on_response,
            headers: req.headers,
            identity: req.identity,
            timeout_ms: req.timeout_ms,
            max_retries: req.max_retries,
            max_retry_delay_ms: req.max_retry_delay_ms,
            ..Default::default()
        }
    }
}

impl fmt::Debug for StreamOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamOptions")
            .field("signal", &self.signal.as_ref().map(|s| s.aborted()))
            .field("telemetry_context", &self.telemetry_context.is_some())
            .field("telemetry", &self.telemetry.is_some())
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("fetch", &self.fetch.is_some())
            .field("env", &self.env)
            .field("on_payload", &self.on_payload.is_some())
            .field("on_response", &self.on_response.is_some())
            .field("headers", &self.headers)
            .field("identity", &self.identity)
            .field("timeout_ms", &self.timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("max_retry_delay_ms", &self.max_retry_delay_ms)
            .field("on_provider_stream_event", &self.on_provider_stream_event.is_some())
            .field("temperature", &self.temperature)
            .field("sampling_params", &self.sampling_params)
            .field("max_tokens", &self.max_tokens)
            .field("transport", &self.transport)
            .field("cache_retention", &self.cache_retention)
            .field("session_id", &self.session_id)
            .field("disable_continuation", &self.disable_continuation)
            .field("websocket_connect_timeout_ms", &self.websocket_connect_timeout_ms)
            .field("metadata", &self.metadata)
            .finish()
    }
}

/// Erased per-API stream options (`StreamOptions & Record<string, unknown>`).
///
/// PORT: Concrete per-API options structs (`AnthropicOptions`, `GoogleOptions`, etc.) convert
/// cleanly into [`ProviderStreamOptions`] / [`ApiStreamOptions`] via `From`, carrying common
/// transport/auth/callback fields in `base` and API-specific options in `extra` (plus `extension`
/// for non-JSON overrides such as an injected SDK client).
#[derive(Clone, Default)]
pub struct ProviderStreamOptions {
    pub base: StreamOptions,
    pub extra: JsonObject,
    pub extension: Option<Arc<dyn Any + Send + Sync>>,
}

impl ProviderStreamOptions {
    pub fn with_extra(mut self, key: impl Into<String>, value: JsonValue) -> Self {
        self.extra.insert(key.into(), value);
        self
    }

    pub fn with_extension(mut self, ext: Arc<dyn Any + Send + Sync>) -> Self {
        self.extension = Some(ext);
        self
    }
}

impl Deref for ProviderStreamOptions {
    type Target = StreamOptions;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for ProviderStreamOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl From<StreamOptions> for ProviderStreamOptions {
    fn from(base: StreamOptions) -> Self {
        Self {
            base,
            extra: JsonObject::new(),
            extension: None,
        }
    }
}

impl fmt::Debug for ProviderStreamOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderStreamOptions")
            .field("base", &self.base)
            .field("extra", &self.extra)
            .field("extension", &self.extension.is_some())
            .finish()
    }
}

pub type ApiStreamOptions = ProviderStreamOptions;

#[derive(Clone, Default, Debug)]
pub struct DeferredFetchOptions {
    pub base: ProviderRequestOptions<Model>,
    pub wait: Option<u64>,
}

impl Deref for DeferredFetchOptions {
    type Target = ProviderRequestOptions<Model>;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for DeferredFetchOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl From<ProviderRequestOptions<Model>> for DeferredFetchOptions {
    fn from(base: ProviderRequestOptions<Model>) -> Self {
        Self { base, wait: None }
    }
}

pub type DeferredCancelOptions = ProviderRequestOptions<Model>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeferredWindow {
    #[serde(rename = "15m")]
    M15,
    #[serde(rename = "1h")]
    H1,
    #[serde(rename = "24h")]
    H24,
}

impl DeferredWindow {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::M15 => "15m",
            Self::H1 => "1h",
            Self::H24 => "24h",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DeferredOption {
    Bool(bool),
    Window {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<DeferredWindow>,
    },
}

#[derive(Clone, Default, Debug)]
pub struct SimpleStreamOptions {
    pub base: StreamOptions,
    pub tool_choice: Option<ToolChoice>,
    pub reasoning: Option<ThinkingLevel>,
    pub deferred: Option<DeferredOption>,
    pub thinking_budgets: Option<ThinkingBudgets>,
}

impl Deref for SimpleStreamOptions {
    type Target = StreamOptions;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for SimpleStreamOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl From<StreamOptions> for SimpleStreamOptions {
    fn from(base: StreamOptions) -> Self {
        Self {
            base,
            ..Default::default()
        }
    }
}

#[derive(Clone, Default, Debug)]
pub struct ClassifierOptions {
    pub base: ProviderRequestOptions<ClassifierModel>,
    pub temperature: Option<f64>,
}

impl Deref for ClassifierOptions {
    type Target = ProviderRequestOptions<ClassifierModel>;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for ClassifierOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl From<ProviderRequestOptions<ClassifierModel>> for ClassifierOptions {
    fn from(base: ProviderRequestOptions<ClassifierModel>) -> Self {
        Self {
            base,
            temperature: None,
        }
    }
}

#[derive(Clone, Default, Debug)]
pub struct ImagesOptions {
    pub base: ProviderRequestOptions<ImageModel>,
    pub metadata: Option<JsonObject>,
}

impl Deref for ImagesOptions {
    type Target = ProviderRequestOptions<ImageModel>;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for ImagesOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl From<ProviderRequestOptions<ImageModel>> for ImagesOptions {
    fn from(base: ProviderRequestOptions<ImageModel>) -> Self {
        Self { base, metadata: None }
    }
}

#[derive(Clone, Default, Debug)]
pub struct ProviderImagesOptions {
    pub base: ImagesOptions,
    pub extra: JsonObject,
}

impl Deref for ProviderImagesOptions {
    type Target = ImagesOptions;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for ProviderImagesOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl From<ImagesOptions> for ProviderImagesOptions {
    fn from(base: ImagesOptions) -> Self {
        Self {
            base,
            extra: JsonObject::new(),
        }
    }
}

// ============================================================================
// Provider Operation Contracts
// ============================================================================

pub type StreamFunction =
    Arc<dyn Fn(&Model, &TranscriptContext, Option<ApiStreamOptions>) -> AssistantMessageEventStream + Send + Sync>;
pub type SimpleStreamFunction =
    Arc<dyn Fn(&Model, &TranscriptContext, Option<SimpleStreamOptions>) -> AssistantMessageEventStream + Send + Sync>;
pub type FetchDeferredFunction =
    Arc<dyn Fn(&Model, &DeferredHandle, Option<DeferredFetchOptions>) -> AssistantMessageEventStream + Send + Sync>;
pub type CancelDeferredFunction =
    Arc<dyn Fn(&Model, &DeferredHandle, Option<DeferredCancelOptions>) -> BoxFuture<Result<()>> + Send + Sync>;

#[derive(Clone)]
pub struct ProviderStreams {
    pub stream: StreamFunction,
    pub stream_simple: SimpleStreamFunction,
    pub fetch_deferred: Option<FetchDeferredFunction>,
    pub cancel_deferred: Option<CancelDeferredFunction>,
}

impl ProviderStreams {
    pub fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        (self.stream)(model, context, options)
    }

    pub fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        (self.stream_simple)(model, context, options)
    }

    pub fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        self.fetch_deferred.as_ref().map(|f| f(model, handle, options))
    }

    pub fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredCancelOptions>,
    ) -> Option<BoxFuture<Result<()>>> {
        self.cancel_deferred.as_ref().map(|f| f(model, handle, options))
    }
}

impl fmt::Debug for ProviderStreams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderStreams")
            .field("fetch_deferred", &self.fetch_deferred.is_some())
            .field("cancel_deferred", &self.cancel_deferred.is_some())
            .finish()
    }
}

pub type ImagesFunction =
    Arc<dyn Fn(&ImageModel, &ImagesContext, Option<ImagesOptions>) -> BoxFuture<AssistantImages> + Send + Sync>;

#[derive(Clone)]
pub struct ProviderImages {
    pub generate_images: ImagesFunction,
}

impl ProviderImages {
    pub fn generate_images(
        &self,
        model: &ImageModel,
        context: &ImagesContext,
        options: Option<ImagesOptions>,
    ) -> BoxFuture<AssistantImages> {
        (self.generate_images)(model, context, options)
    }
}

impl fmt::Debug for ProviderImages {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderImages").finish()
    }
}

pub type ClassifierFunction = Arc<
    dyn Fn(&ClassifierModel, &ClassifierContext, Option<ClassifierOptions>) -> BoxFuture<ClassifierResult>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct ProviderClassifier {
    pub classify: ClassifierFunction,
}

impl ProviderClassifier {
    pub fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<ClassifierOptions>,
    ) -> BoxFuture<ClassifierResult> {
        (self.classify)(model, context, options)
    }
}

impl fmt::Debug for ProviderClassifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderClassifier").finish()
    }
}

// ============================================================================
// Anthropic Fallback Model Metadata
// ============================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnthropicAllowedFallbackModel {
    pub provider: ProviderId,
    pub model: String,
    pub cost: ModelCost,
}

// ============================================================================
// Content Blocks
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextSignaturePhase {
    Commentary,
    FinalAnswer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSignatureV1 {
    pub v: i64,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<TextSignaturePhase>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextContent {
    pub text: String,
    pub text_signature: Option<String>,
}

impl TextContent {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            text_signature: None,
        }
    }
}

impl Serialize for TextContent {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let len = 2 + usize::from(self.text_signature.is_some());
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry("type", "text")?;
        map.serialize_entry("text", &self.text)?;
        if let Some(sig) = &self.text_signature {
            map.serialize_entry("textSignature", sig)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for TextContent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(rename = "type")]
            kind: Option<String>,
            text: String,
            #[serde(default)]
            text_signature: Option<String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if let Some(kind) = raw.kind.as_deref() {
            if kind != "text" {
                return Err(de::Error::custom(format!(
                    "expected content type \"text\", got \"{kind}\""
                )));
            }
        }
        Ok(Self {
            text: raw.text,
            text_signature: raw.text_signature,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThinkingContent {
    pub thinking: String,
    pub thinking_signature: Option<String>,
    pub redacted: Option<bool>,
}

impl ThinkingContent {
    pub fn new(thinking: impl Into<String>) -> Self {
        Self {
            thinking: thinking.into(),
            thinking_signature: None,
            redacted: None,
        }
    }
}

impl Serialize for ThinkingContent {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let len = 2 + usize::from(self.thinking_signature.is_some()) + usize::from(self.redacted.is_some());
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry("type", "thinking")?;
        map.serialize_entry("thinking", &self.thinking)?;
        if let Some(sig) = &self.thinking_signature {
            map.serialize_entry("thinkingSignature", sig)?;
        }
        if let Some(redacted) = self.redacted {
            map.serialize_entry("redacted", &redacted)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ThinkingContent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(rename = "type")]
            kind: Option<String>,
            thinking: String,
            #[serde(default)]
            thinking_signature: Option<String>,
            #[serde(default)]
            redacted: Option<bool>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if let Some(kind) = raw.kind.as_deref() {
            if kind != "thinking" {
                return Err(de::Error::custom(format!(
                    "expected content type \"thinking\", got \"{kind}\""
                )));
            }
        }
        Ok(Self {
            thinking: raw.thinking,
            thinking_signature: raw.thinking_signature,
            redacted: raw.redacted,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageContent {
    pub data: String,
    pub mime_type: String,
}

impl ImageContent {
    pub fn new(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self {
            data: data.into(),
            mime_type: mime_type.into(),
        }
    }
}

impl Serialize for ImageContent {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("type", "image")?;
        map.serialize_entry("data", &self.data)?;
        map.serialize_entry("mimeType", &self.mime_type)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for ImageContent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(rename = "type")]
            kind: Option<String>,
            data: String,
            mime_type: String,
        }
        let raw = Raw::deserialize(deserializer)?;
        if let Some(kind) = raw.kind.as_deref() {
            if kind != "image" {
                return Err(de::Error::custom(format!(
                    "expected content type \"image\", got \"{kind}\""
                )));
            }
        }
        Ok(Self {
            data: raw.data,
            mime_type: raw.mime_type,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: JsonObject,
    pub thought_signature: Option<String>,
    pub namespace: Option<String>,
    pub arguments_error: Option<String>,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: JsonObject) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        }
    }
}

impl Serialize for ToolCall {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let len = 4
            + usize::from(self.thought_signature.is_some())
            + usize::from(self.namespace.is_some())
            + usize::from(self.arguments_error.is_some());
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry("type", "toolCall")?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("name", &self.name)?;
        map.serialize_entry("arguments", &self.arguments)?;
        if let Some(sig) = &self.thought_signature {
            map.serialize_entry("thoughtSignature", sig)?;
        }
        if let Some(ns) = &self.namespace {
            map.serialize_entry("namespace", ns)?;
        }
        if let Some(err) = &self.arguments_error {
            map.serialize_entry("argumentsError", err)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ToolCall {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(rename = "type")]
            kind: Option<String>,
            id: String,
            name: String,
            arguments: JsonObject,
            #[serde(default)]
            thought_signature: Option<String>,
            #[serde(default)]
            namespace: Option<String>,
            #[serde(default)]
            arguments_error: Option<String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if let Some(kind) = raw.kind.as_deref() {
            if kind != "toolCall" {
                return Err(de::Error::custom(format!(
                    "expected content type \"toolCall\", got \"{kind}\""
                )));
            }
        }
        Ok(Self {
            id: raw.id,
            name: raw.name,
            arguments: raw.arguments,
            thought_signature: raw.thought_signature,
            namespace: raw.namespace,
            arguments_error: raw.arguments_error,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum UserContentBlock {
    Text(TextContent),
    Image(ImageContent),
}

impl From<TextContent> for UserContentBlock {
    fn from(value: TextContent) -> Self {
        Self::Text(value)
    }
}

impl From<ImageContent> for UserContentBlock {
    fn from(value: ImageContent) -> Self {
        Self::Image(value)
    }
}

impl Serialize for UserContentBlock {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Text(v) => v.serialize(serializer),
            Self::Image(v) => v.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for UserContentBlock {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let val = Value::deserialize(deserializer)?;
        let kind = val
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| de::Error::missing_field("type"))?;
        match kind {
            "text" => serde_json::from_value(val).map(Self::Text).map_err(de::Error::custom),
            "image" => serde_json::from_value(val).map(Self::Image).map_err(de::Error::custom),
            other => Err(de::Error::custom(format!(
                "unknown user/toolResult content type: {other}"
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AssistantContentBlock {
    Text(TextContent),
    Thinking(ThinkingContent),
    ToolCall(ToolCall),
}

impl From<TextContent> for AssistantContentBlock {
    fn from(value: TextContent) -> Self {
        Self::Text(value)
    }
}

impl From<ThinkingContent> for AssistantContentBlock {
    fn from(value: ThinkingContent) -> Self {
        Self::Thinking(value)
    }
}

impl From<ToolCall> for AssistantContentBlock {
    fn from(value: ToolCall) -> Self {
        Self::ToolCall(value)
    }
}

impl Serialize for AssistantContentBlock {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Text(v) => v.serialize(serializer),
            Self::Thinking(v) => v.serialize(serializer),
            Self::ToolCall(v) => v.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for AssistantContentBlock {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let val = Value::deserialize(deserializer)?;
        let kind = val
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| de::Error::missing_field("type"))?;
        match kind {
            "text" => serde_json::from_value(val).map(Self::Text).map_err(de::Error::custom),
            "thinking" => serde_json::from_value(val)
                .map(Self::Thinking)
                .map_err(de::Error::custom),
            "toolCall" => serde_json::from_value(val)
                .map(Self::ToolCall)
                .map_err(de::Error::custom),
            other => Err(de::Error::custom(format!("unknown assistant content type: {other}"))),
        }
    }
}

// ============================================================================
// Usage & StopReason
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_from_provider: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    #[serde(rename = "cacheWrite1h", default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<i64>,
    pub total_tokens: i64,
    pub cost: UsageCost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StopReason {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "stop")]
    Stop,
    #[serde(rename = "length")]
    Length,
    #[serde(rename = "toolUse")]
    ToolUse,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "aborted")]
    Aborted,
    #[serde(rename = "deferred")]
    Deferred,
}

impl StopReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Stop => "stop",
            Self::Length => "length",
            Self::ToolUse => "toolUse",
            Self::Error => "error",
            Self::Aborted => "aborted",
            Self::Deferred => "deferred",
        }
    }
}

// ============================================================================
// Deferred Handle & Messages
// ============================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredHandle {
    pub provider: String,
    pub model_id: String,
    pub api: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_after_ms: Option<i64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_some_value"
    )]
    pub data: Option<JsonValue>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemMessageContent {
    Text(String),
    Blocks(Vec<TextContent>),
}

impl Default for SystemMessageContent {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

impl From<String> for SystemMessageContent {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for SystemMessageContent {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<Vec<TextContent>> for SystemMessageContent {
    fn from(value: Vec<TextContent>) -> Self {
        Self::Blocks(value)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SystemMessage {
    pub content: SystemMessageContent,
    pub sections: Option<IndexMap<String, Option<String>>>,
    pub tools_added: Option<Vec<Tool>>,
    pub tools_removed: Option<Vec<ToolReference>>,
    pub timestamp: i64,
}

impl Serialize for SystemMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let len = 3
            + usize::from(self.sections.is_some())
            + usize::from(self.tools_added.is_some())
            + usize::from(self.tools_removed.is_some());
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry("role", "system")?;
        map.serialize_entry("content", &self.content)?;
        if let Some(sections) = &self.sections {
            map.serialize_entry("sections", sections)?;
        }
        if let Some(tools_added) = &self.tools_added {
            map.serialize_entry("toolsAdded", tools_added)?;
        }
        if let Some(tools_removed) = &self.tools_removed {
            map.serialize_entry("toolsRemoved", tools_removed)?;
        }
        map.serialize_entry("timestamp", &self.timestamp)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for SystemMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            role: Option<String>,
            content: SystemMessageContent,
            #[serde(default)]
            sections: Option<IndexMap<String, Option<String>>>,
            #[serde(default)]
            tools_added: Option<Vec<Tool>>,
            #[serde(default)]
            tools_removed: Option<Vec<ToolReference>>,
            timestamp: i64,
        }
        let raw = Raw::deserialize(deserializer)?;
        if let Some(role) = raw.role.as_deref() {
            if role != "system" {
                return Err(de::Error::custom(format!(
                    "expected message role \"system\", got \"{role}\""
                )));
            }
        }
        Ok(Self {
            content: raw.content,
            sections: raw.sections,
            tools_added: raw.tools_added,
            tools_removed: raw.tools_removed,
            timestamp: raw.timestamp,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserMessageContent {
    Text(String),
    Blocks(Vec<UserContentBlock>),
}

impl Default for UserMessageContent {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

impl From<String> for UserMessageContent {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for UserMessageContent {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<Vec<UserContentBlock>> for UserMessageContent {
    fn from(value: Vec<UserContentBlock>) -> Self {
        Self::Blocks(value)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UserMessage {
    pub content: UserMessageContent,
    pub timestamp: i64,
}

impl Serialize for UserMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("role", "user")?;
        map.serialize_entry("content", &self.content)?;
        map.serialize_entry("timestamp", &self.timestamp)?;
        map.end()
    }
}

fn deserialize_lax_user_content<'de, D>(deserializer: D) -> std::result::Result<UserMessageContent, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<UserMessageContent>::deserialize(deserializer)?
        .unwrap_or_else(|| UserMessageContent::Blocks(Vec::new())))
}

fn deserialize_lax_vec<'de, D, T>(deserializer: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

impl<'de> Deserialize<'de> for UserMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            role: Option<String>,
            #[serde(default, deserialize_with = "deserialize_lax_user_content")]
            content: UserMessageContent,
            timestamp: i64,
        }
        let raw = Raw::deserialize(deserializer)?;
        if let Some(role) = raw.role.as_deref() {
            if role != "user" {
                return Err(de::Error::custom(format!(
                    "expected message role \"user\", got \"{role}\""
                )));
            }
        }
        Ok(Self {
            content: raw.content,
            timestamp: raw.timestamp,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AssistantMessage {
    pub content: Vec<AssistantContentBlock>,
    pub api: Api,
    pub provider: ProviderId,
    pub model: String,
    pub response_model: Option<String>,
    pub response_id: Option<String>,
    pub provider_thinking_level: Option<String>,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
    pub usage: Usage,
    pub stop_reason: StopReason,
    pub deferred: Option<DeferredHandle>,
    pub error_message: Option<String>,
    pub raw_stop_reason: Option<String>,
    pub end_turn: Option<bool>,
    pub timestamp: i64,
    pub duration_ms: Option<i64>,
}

impl Serialize for AssistantMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let len = 8
            + usize::from(self.response_model.is_some())
            + usize::from(self.response_id.is_some())
            + usize::from(self.provider_thinking_level.is_some())
            + usize::from(self.thinking_level.is_some())
            + usize::from(self.diagnostics.is_some())
            + usize::from(self.deferred.is_some())
            + usize::from(self.error_message.is_some())
            + usize::from(self.raw_stop_reason.is_some())
            + usize::from(self.end_turn.is_some())
            + usize::from(self.duration_ms.is_some());
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry("role", "assistant")?;
        map.serialize_entry("content", &self.content)?;
        map.serialize_entry("api", &self.api)?;
        map.serialize_entry("provider", &self.provider)?;
        map.serialize_entry("model", &self.model)?;
        if let Some(v) = &self.response_model {
            map.serialize_entry("responseModel", v)?;
        }
        if let Some(v) = &self.response_id {
            map.serialize_entry("responseId", v)?;
        }
        if let Some(v) = &self.provider_thinking_level {
            map.serialize_entry("providerThinkingLevel", v)?;
        }
        if let Some(v) = &self.thinking_level {
            map.serialize_entry("thinkingLevel", v)?;
        }
        if let Some(v) = &self.diagnostics {
            map.serialize_entry("diagnostics", v)?;
        }
        map.serialize_entry("usage", &self.usage)?;
        map.serialize_entry("stopReason", &self.stop_reason)?;
        if let Some(v) = &self.deferred {
            map.serialize_entry("deferred", v)?;
        }
        if let Some(v) = &self.error_message {
            map.serialize_entry("errorMessage", v)?;
        }
        if let Some(v) = &self.raw_stop_reason {
            map.serialize_entry("rawStopReason", v)?;
        }
        if let Some(v) = self.end_turn {
            map.serialize_entry("endTurn", &v)?;
        }
        map.serialize_entry("timestamp", &self.timestamp)?;
        if let Some(v) = self.duration_ms {
            map.serialize_entry("durationMs", &v)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for AssistantMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            role: Option<String>,
            #[serde(default, deserialize_with = "deserialize_lax_vec")]
            content: Vec<AssistantContentBlock>,
            api: Api,
            provider: ProviderId,
            model: String,
            #[serde(default)]
            response_model: Option<String>,
            #[serde(default)]
            response_id: Option<String>,
            #[serde(default)]
            provider_thinking_level: Option<String>,
            #[serde(default)]
            thinking_level: Option<ModelThinkingLevel>,
            #[serde(default)]
            diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
            usage: Usage,
            stop_reason: StopReason,
            #[serde(default)]
            deferred: Option<DeferredHandle>,
            #[serde(default)]
            error_message: Option<String>,
            #[serde(default)]
            raw_stop_reason: Option<String>,
            #[serde(default)]
            end_turn: Option<bool>,
            timestamp: i64,
            #[serde(default)]
            duration_ms: Option<i64>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if let Some(role) = raw.role.as_deref() {
            if role != "assistant" {
                return Err(de::Error::custom(format!(
                    "expected message role \"assistant\", got \"{role}\""
                )));
            }
        }
        Ok(Self {
            content: raw.content,
            api: raw.api,
            provider: raw.provider,
            model: raw.model,
            response_model: raw.response_model,
            response_id: raw.response_id,
            provider_thinking_level: raw.provider_thinking_level,
            thinking_level: raw.thinking_level,
            diagnostics: raw.diagnostics,
            usage: raw.usage,
            stop_reason: raw.stop_reason,
            deferred: raw.deferred,
            error_message: raw.error_message,
            raw_stop_reason: raw.raw_stop_reason,
            end_turn: raw.end_turn,
            timestamp: raw.timestamp,
            duration_ms: raw.duration_ms,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NestedToolCallStatus {
    Ok,
    Error,
    Unfinished,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolCallRecord {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments_bytes: Option<i64>,
    pub status: NestedToolCallStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolCalls {
    pub calls: Vec<NestedToolCallRecord>,
    pub complete: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: Vec<UserContentBlock>,
    pub details: Option<JsonValue>,
    pub usage: Option<Usage>,
    pub nested_calls: Option<NestedToolCalls>,
    pub is_error: bool,
    pub timestamp: i64,
    pub duration_ms: Option<i64>,
}

impl Serialize for ToolResultMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let len = 6
            + usize::from(self.details.is_some())
            + usize::from(self.usage.is_some())
            + usize::from(self.nested_calls.is_some())
            + usize::from(self.duration_ms.is_some());
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry("role", "toolResult")?;
        map.serialize_entry("toolCallId", &self.tool_call_id)?;
        map.serialize_entry("toolName", &self.tool_name)?;
        map.serialize_entry("content", &self.content)?;
        if let Some(details) = &self.details {
            map.serialize_entry("details", details)?;
        }
        if let Some(usage) = &self.usage {
            map.serialize_entry("usage", usage)?;
        }
        if let Some(nested) = &self.nested_calls {
            map.serialize_entry("nestedCalls", nested)?;
        }
        map.serialize_entry("isError", &self.is_error)?;
        map.serialize_entry("timestamp", &self.timestamp)?;
        if let Some(duration_ms) = self.duration_ms {
            map.serialize_entry("durationMs", &duration_ms)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ToolResultMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            role: Option<String>,
            tool_call_id: String,
            tool_name: String,
            #[serde(default, deserialize_with = "deserialize_lax_vec")]
            content: Vec<UserContentBlock>,
            #[serde(default, deserialize_with = "deserialize_some_value")]
            details: Option<JsonValue>,
            #[serde(default)]
            usage: Option<Usage>,
            #[serde(default)]
            nested_calls: Option<NestedToolCalls>,
            is_error: bool,
            timestamp: i64,
            #[serde(default)]
            duration_ms: Option<i64>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if let Some(role) = raw.role.as_deref() {
            if role != "toolResult" {
                return Err(de::Error::custom(format!(
                    "expected message role \"toolResult\", got \"{role}\""
                )));
            }
        }
        Ok(Self {
            tool_call_id: raw.tool_call_id,
            tool_name: raw.tool_name,
            content: raw.content,
            details: raw.details,
            usage: raw.usage,
            nested_calls: raw.nested_calls,
            is_error: raw.is_error,
            timestamp: raw.timestamp,
            duration_ms: raw.duration_ms,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    System(SystemMessage),
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
}

impl Message {
    pub const fn role(&self) -> &'static str {
        match self {
            Self::System(_) => "system",
            Self::User(_) => "user",
            Self::Assistant(_) => "assistant",
            Self::ToolResult(_) => "toolResult",
        }
    }

    pub const fn timestamp(&self) -> i64 {
        match self {
            Self::System(m) => m.timestamp,
            Self::User(m) => m.timestamp,
            Self::Assistant(m) => m.timestamp,
            Self::ToolResult(m) => m.timestamp,
        }
    }
}

impl From<SystemMessage> for Message {
    fn from(value: SystemMessage) -> Self {
        Self::System(value)
    }
}

impl From<UserMessage> for Message {
    fn from(value: UserMessage) -> Self {
        Self::User(value)
    }
}

impl From<AssistantMessage> for Message {
    fn from(value: AssistantMessage) -> Self {
        Self::Assistant(value)
    }
}

impl From<ToolResultMessage> for Message {
    fn from(value: ToolResultMessage) -> Self {
        Self::ToolResult(value)
    }
}

impl Serialize for Message {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::System(m) => m.serialize(serializer),
            Self::User(m) => m.serialize(serializer),
            Self::Assistant(m) => m.serialize(serializer),
            Self::ToolResult(m) => m.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let val = Value::deserialize(deserializer)?;
        let role = val
            .get("role")
            .and_then(|v| v.as_str())
            .ok_or_else(|| de::Error::missing_field("role"))?;
        match role {
            "system" => serde_json::from_value(val).map(Self::System).map_err(de::Error::custom),
            "user" => serde_json::from_value(val).map(Self::User).map_err(de::Error::custom),
            "assistant" => serde_json::from_value(val)
                .map(Self::Assistant)
                .map_err(de::Error::custom),
            "toolResult" => serde_json::from_value(val)
                .map(Self::ToolResult)
                .map_err(de::Error::custom),
            other => Err(de::Error::custom(format!("unknown message role: {other}"))),
        }
    }
}

// ============================================================================
// Images & Classifier Types
// ============================================================================

pub type ImagesInputContent = UserContentBlock;
pub type ImagesOutputContent = UserContentBlock;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagesContext {
    pub input: Vec<ImagesInputContent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImagesStopReason {
    Stop,
    Error,
    Aborted,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantImages {
    pub api: ImageApi,
    pub provider: ProviderId,
    pub model: String,
    pub output: Vec<ImagesOutputContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub stop_reason: ImagesStopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub timestamp: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierBoolCriteria {
    #[serde(rename = "true")]
    pub r#true: String,
    #[serde(rename = "false")]
    pub r#false: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClassifierQuestion {
    #[serde(rename = "choice")]
    Choice {
        instructions: String,
        criteria: IndexMap<String, String>,
    },
    #[serde(rename = "score")]
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
    #[serde(rename = "bool")]
    Bool {
        instructions: String,
        criteria: ClassifierBoolCriteria,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierContext {
    pub state: JsonObject,
    pub questions: IndexMap<String, ClassifierQuestion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageContent>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClassifierAnswer {
    #[serde(rename = "choice")]
    Choice {
        choice: String,
        probabilities: IndexMap<String, f64>,
        confidence: f64,
    },
    #[serde(rename = "score")]
    Score { score: f64, confidence: f64 },
    #[serde(rename = "bool")]
    Bool { probability: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClassifierStopReason {
    Stop,
    Error,
    Aborted,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierResult {
    pub api: ClassifierApi,
    pub provider: ProviderId,
    pub model: String,
    pub answers: IndexMap<String, ClassifierAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub stop_reason: ClassifierStopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub timestamp: i64,
}

// ============================================================================
// Tools & Contexts
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GrammarFormat {
    #[serde(rename = "openai_lark")]
    OpenAILark,
    #[serde(rename = "openai_regex")]
    OpenAIRegex,
}

pub type GrammarVariants = IndexMap<GrammarFormat, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JsonSchemaStrict {
    Prefer,
    Require,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ConstrainedSamplingConfig {
    #[serde(rename = "json_schema")]
    JsonSchema { strict: JsonSchemaStrict },
    #[serde(rename = "grammar")]
    Grammar { variants: GrammarVariants },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConstrainedSampling {
    Disabled,
    Config(ConstrainedSamplingConfig),
}

impl From<ConstrainedSamplingConfig> for ConstrainedSampling {
    fn from(value: ConstrainedSamplingConfig) -> Self {
        Self::Config(value)
    }
}

impl Serialize for ConstrainedSampling {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Disabled => serializer.serialize_bool(false),
            Self::Config(cfg) => cfg.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ConstrainedSampling {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let val = Value::deserialize(deserializer)?;
        match val {
            Value::Bool(false) => Ok(Self::Disabled),
            Value::Bool(true) => Err(de::Error::custom("constrainedSampling boolean can only be false")),
            other => serde_json::from_value(other)
                .map(Self::Config)
                .map_err(de::Error::custom),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: TSchema,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constrained_sampling: Option<ConstrainedSampling>,
}

impl PartialEq for Tool {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.description == other.description
            && self.parameters.to_value() == other.parameters.to_value()
            && self.constrained_sampling == other.constrained_sampling
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolReference {
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptContext {
    pub messages: Vec<Message>,
    #[serde(skip)]
    pub(crate) _brand: (),
}

impl TranscriptContext {
    pub(crate) fn from_normalized(messages: Vec<Message>) -> Self {
        Self { messages, _brand: () }
    }
}

// ============================================================================
// AssistantMessageEvent
// ============================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AssistantMessageEvent {
    #[serde(rename = "start")]
    Start { partial: AssistantMessage },
    #[serde(rename = "text_start")]
    TextStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        partial: AssistantMessage,
    },
    #[serde(rename = "text_delta")]
    TextDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "text_end")]
    TextEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        content: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "thinking_start")]
    ThinkingStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        partial: AssistantMessage,
    },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "thinking_end")]
    ThinkingEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        content: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        #[serde(rename = "toolCall")]
        tool_call: ToolCall,
        partial: AssistantMessage,
    },
    #[serde(rename = "done")]
    Done {
        reason: StopReason,
        message: AssistantMessage,
    },
    #[serde(rename = "error")]
    Error {
        reason: StopReason,
        error: AssistantMessage,
    },
}

// ============================================================================
// Compat & Routing Configurations
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MaxTokensField {
    #[serde(rename = "max_completion_tokens")]
    MaxCompletionTokens,
    #[serde(rename = "max_tokens")]
    MaxTokens,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ThinkingFormat {
    #[serde(rename = "openai")]
    OpenAI,
    #[serde(rename = "openrouter")]
    OpenRouter,
    #[serde(rename = "deepseek")]
    DeepSeek,
    #[serde(rename = "together")]
    Together,
    #[serde(rename = "baseten")]
    Baseten,
    #[serde(rename = "zai")]
    Zai,
    #[serde(rename = "qwen")]
    Qwen,
    #[serde(rename = "chat-template")]
    ChatTemplate,
    #[serde(rename = "qwen-chat-template")]
    QwenChatTemplate,
    #[serde(rename = "string-thinking")]
    StringThinking,
    #[serde(rename = "ant-ling")]
    AntLing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheControlFormat {
    Anthropic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenRouterDataCollection {
    Deny,
    Allow,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpenRouterSort {
    String(String),
    Object {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "pi_js::json::double_option"
        )]
        partition: Option<Option<String>>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NumberOrString {
    Int(i64),
    Float(f64),
    String(String),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenRouterMaxPrice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<NumberOrString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<NumberOrString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<NumberOrString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<NumberOrString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<NumberOrString>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenRouterPercentiles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p50: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p75: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p90: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p99: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NumberOrPercentiles {
    Number(f64),
    Percentiles(OpenRouterPercentiles),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenRouterRouting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_fallbacks: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_parameters: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_collection: Option<OpenRouterDataCollection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zdr: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforce_distillable_text: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantizations: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<OpenRouterSort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_price: Option<OpenRouterMaxPrice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_min_throughput: Option<NumberOrPercentiles>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_max_latency: Option<NumberOrPercentiles>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VercelGatewayRouting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptCacheKeyMode {
    Auto,
    Enabled,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SystemOneImageInput {
    #[serde(rename = "assets")]
    Assets,
    #[serde(rename = "images")]
    Images,
    #[serde(rename = "state-image-urls")]
    StateImageUrls,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAICompletionsCompat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_store: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_developer_role: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reasoning_effort: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_usage_in_streaming: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_finish_reason: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_field: Option<MaxTokensField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_tool_result_name: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_assistant_after_tool_result: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_non_null_assistant_content: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_tools_with_tool_history: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_thinking_as_text: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_reasoning_content_on_assistant_messages: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_format: Option<ThinkingFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<IndexMap<String, ChatTemplateKwargValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template_args: Option<IndexMap<String, ChatTemplateKwargValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_router_routing: Option<OpenRouterRouting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vercel_gateway_routing: Option<VercelGatewayRouting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zai_tool_stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_thinking_token_budget: Option<bool>,
    #[serde(
        rename = "supportsOpenAIGrammarTools",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_open_ai_grammar_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_system_messages: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_tool_additions: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_strict_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control_format: Option<CacheControlFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_session_affinity_headers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_affinity_format: Option<SessionAffinityFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key_mode: Option<PromptCacheKeyMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_long_cache_retention: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vllm_priority: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAIResponsesCompat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_developer_role: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_system_messages: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_affinity_format: Option<SessionAffinityFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key_mode: Option<PromptCacheKeyMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_long_cache_retention: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_strict_mode: Option<bool>,
    #[serde(
        rename = "supportsOpenAIGrammarTools",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_open_ai_grammar_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_additional_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tool_search: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_explicit_prompt_cache_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_effort: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_max_output_tokens: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnthropicMessagesCompat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_eager_tool_input_streaming: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_long_cache_retention: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_session_affinity_headers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_affinity_format: Option<SessionAffinityFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_cache_control_on_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_temperature: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_adaptive_thinking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_empty_signature: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_strict_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_effort: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_system_messages: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_tool_changes: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_message_threads: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_fallback_models: Option<Vec<AnthropicAllowedFallbackModel>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BedrockCompat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_strict_mode: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MistralConversationsCompat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_system_messages: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemOneCompat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_input: Option<SystemOneImageInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_image_opt_in_header: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModelCompat {
    OpenAICompletions(OpenAICompletionsCompat),
    OpenAIResponses(OpenAIResponsesCompat),
    AnthropicMessages(AnthropicMessagesCompat),
    Bedrock(BedrockCompat),
    MistralConversations(MistralConversationsCompat),
    SystemOne(SystemOneCompat),
    Other(JsonValue),
}

impl ModelCompat {
    pub fn as_openai_completions(&self) -> Option<&OpenAICompletionsCompat> {
        match self {
            Self::OpenAICompletions(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_openai_responses(&self) -> Option<&OpenAIResponsesCompat> {
        match self {
            Self::OpenAIResponses(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_anthropic_messages(&self) -> Option<&AnthropicMessagesCompat> {
        match self {
            Self::AnthropicMessages(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_bedrock(&self) -> Option<&BedrockCompat> {
        match self {
            Self::Bedrock(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_mistral_conversations(&self) -> Option<&MistralConversationsCompat> {
        match self {
            Self::MistralConversations(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_system_one(&self) -> Option<&SystemOneCompat> {
        match self {
            Self::SystemOne(c) => Some(c),
            _ => None,
        }
    }

    pub fn deserialize_for_api(api: &str, value: JsonValue) -> std::result::Result<Self, String> {
        match api {
            "openai-completions" => serde_json::from_value(value)
                .map(Self::OpenAICompletions)
                .map_err(|e| e.to_string()),
            "openai-responses" | "azure-openai-responses" | "openai-codex-responses" => serde_json::from_value(value)
                .map(Self::OpenAIResponses)
                .map_err(|e| e.to_string()),
            "anthropic-messages" => serde_json::from_value(value)
                .map(Self::AnthropicMessages)
                .map_err(|e| e.to_string()),
            "bedrock-converse-stream" => serde_json::from_value(value)
                .map(Self::Bedrock)
                .map_err(|e| e.to_string()),
            "mistral-conversations" => serde_json::from_value(value)
                .map(Self::MistralConversations)
                .map_err(|e| e.to_string()),
            "typesafe-system-one" | "cloudflare-workers-ai-system-one" => serde_json::from_value(value)
                .map(Self::SystemOne)
                .map_err(|e| e.to_string()),
            _ => Ok(Self::Other(value)),
        }
    }
}

impl From<SystemOneCompat> for ModelCompat {
    fn from(value: SystemOneCompat) -> Self {
        Self::SystemOne(value)
    }
}

impl From<OpenAICompletionsCompat> for ModelCompat {
    fn from(value: OpenAICompletionsCompat) -> Self {
        Self::OpenAICompletions(value)
    }
}

impl From<OpenAIResponsesCompat> for ModelCompat {
    fn from(value: OpenAIResponsesCompat) -> Self {
        Self::OpenAIResponses(value)
    }
}

impl From<AnthropicMessagesCompat> for ModelCompat {
    fn from(value: AnthropicMessagesCompat) -> Self {
        Self::AnthropicMessages(value)
    }
}

impl From<BedrockCompat> for ModelCompat {
    fn from(value: BedrockCompat) -> Self {
        Self::Bedrock(value)
    }
}

impl From<MistralConversationsCompat> for ModelCompat {
    fn from(value: MistralConversationsCompat) -> Self {
        Self::MistralConversations(value)
    }
}

// ============================================================================
// Cost & Input Limits
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostRates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostTier {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub input_tokens_above: i64,
}

impl ModelCostTier {
    pub const fn rates(&self) -> ModelCostRates {
        ModelCostRates {
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            cache_write: self.cache_write,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<ModelCostTier>>,
}

impl ModelCost {
    pub const fn rates(&self) -> ModelCostRates {
        ModelCostRates {
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            cache_write: self.cache_write,
        }
    }
}

impl From<ModelCostRates> for ModelCost {
    fn from(rates: ModelCostRates) -> Self {
        Self {
            input: rates.input,
            output: rates.output,
            cache_read: rates.cache_read,
            cache_write: rates.cache_write,
            tiers: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelImageResizeOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_height: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jpeg_quality: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelImageInputLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resize: Option<ModelImageResizeOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_per_message: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_per_request: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInputLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_request_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<ModelImageInputLimits>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelModality {
    Text,
    Image,
}

pub type ModelInput = ModelModality;
pub type ModelOutput = ModelModality;

// ============================================================================
// Models & AnyModel
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatModelType {
    Chat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelType {
    Chat,
    Image,
    Classifier,
}

impl ModelType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Image => "image",
            Self::Classifier => "classifier",
        }
    }
}

impl fmt::Display for ModelType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<ChatModelType>,
    pub id: String,
    pub name: String,
    pub api: Api,
    pub provider: ProviderId,
    pub base_url: String,
    pub reasoning: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<ModelPromptCache>,
    pub input: Vec<ModelInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    pub cost: ModelCost,
    pub context_window: i64,
    pub max_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<SamplingParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params_by_thinking_level: Option<SamplingParamsByThinkingLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<ModelCompat>,
}

impl<'de> Deserialize<'de> for Model {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RawModel {
            #[serde(rename = "type", default)]
            r#type: Option<ChatModelType>,
            id: String,
            name: String,
            api: Api,
            provider: ProviderId,
            base_url: String,
            reasoning: bool,
            #[serde(default)]
            thinking_level_map: Option<ThinkingLevelMap>,
            #[serde(default)]
            prompt_cache: Option<ModelPromptCache>,
            input: Vec<ModelInput>,
            #[serde(default)]
            input_limits: Option<ModelInputLimits>,
            cost: ModelCost,
            context_window: i64,
            max_tokens: i64,
            #[serde(default)]
            headers: Option<IndexMap<String, String>>,
            #[serde(default)]
            sampling_params: Option<SamplingParams>,
            #[serde(default)]
            sampling_params_by_thinking_level: Option<SamplingParamsByThinkingLevel>,
            #[serde(default)]
            compat: Option<JsonValue>,
        }

        let raw = RawModel::deserialize(deserializer)?;
        let compat = match raw.compat {
            None | Some(Value::Null) => None,
            Some(val) => Some(ModelCompat::deserialize_for_api(&raw.api, val).map_err(de::Error::custom)?),
        };

        Ok(Self {
            r#type: raw.r#type,
            id: raw.id,
            name: raw.name,
            api: raw.api,
            provider: raw.provider,
            base_url: raw.base_url,
            reasoning: raw.reasoning,
            thinking_level_map: raw.thinking_level_map,
            prompt_cache: raw.prompt_cache,
            input: raw.input,
            input_limits: raw.input_limits,
            cost: raw.cost,
            context_window: raw.context_window,
            max_tokens: raw.max_tokens,
            headers: raw.headers,
            sampling_params: raw.sampling_params,
            sampling_params_by_thinking_level: raw.sampling_params_by_thinking_level,
            compat,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageModel {
    pub id: String,
    pub name: String,
    pub api: ImageApi,
    pub provider: ProviderId,
    pub base_url: String,
    pub input: Vec<ModelInput>,
    pub input_limits: Option<ModelInputLimits>,
    pub output: Vec<ModelOutput>,
    pub cost: ModelCost,
    pub headers: Option<IndexMap<String, String>>,
}

impl Serialize for ImageModel {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let len = 9 + usize::from(self.input_limits.is_some()) + usize::from(self.headers.is_some());
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry("type", "image")?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("name", &self.name)?;
        map.serialize_entry("api", &self.api)?;
        map.serialize_entry("provider", &self.provider)?;
        map.serialize_entry("baseUrl", &self.base_url)?;
        map.serialize_entry("input", &self.input)?;
        if let Some(limits) = &self.input_limits {
            map.serialize_entry("inputLimits", limits)?;
        }
        map.serialize_entry("output", &self.output)?;
        map.serialize_entry("cost", &self.cost)?;
        if let Some(headers) = &self.headers {
            map.serialize_entry("headers", headers)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ImageModel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(rename = "type", default)]
            r#type: Option<String>,
            id: String,
            name: String,
            api: ImageApi,
            provider: ProviderId,
            base_url: String,
            input: Vec<ModelInput>,
            #[serde(default)]
            input_limits: Option<ModelInputLimits>,
            output: Vec<ModelOutput>,
            cost: ModelCost,
            #[serde(default)]
            headers: Option<IndexMap<String, String>>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if let Some(kind) = raw.r#type.as_deref() {
            if kind != "image" {
                return Err(de::Error::custom(format!(
                    "expected model type \"image\", got \"{kind}\""
                )));
            }
        }
        Ok(Self {
            id: raw.id,
            name: raw.name,
            api: raw.api,
            provider: raw.provider,
            base_url: raw.base_url,
            input: raw.input,
            input_limits: raw.input_limits,
            output: raw.output,
            cost: raw.cost,
            headers: raw.headers,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClassifierModel {
    pub id: String,
    pub name: String,
    pub api: ClassifierApi,
    pub provider: ProviderId,
    pub base_url: String,
    pub input: Vec<ModelInput>,
    pub input_limits: Option<ModelInputLimits>,
    pub context_window: i64,
    pub cost: ModelCost,
    pub headers: Option<IndexMap<String, String>>,
    pub compat: Option<SystemOneCompat>,
}

impl Serialize for ClassifierModel {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let len = 9
            + usize::from(self.input_limits.is_some())
            + usize::from(self.headers.is_some())
            + usize::from(self.compat.is_some());
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry("type", "classifier")?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("name", &self.name)?;
        map.serialize_entry("api", &self.api)?;
        map.serialize_entry("provider", &self.provider)?;
        map.serialize_entry("baseUrl", &self.base_url)?;
        map.serialize_entry("input", &self.input)?;
        if let Some(limits) = &self.input_limits {
            map.serialize_entry("inputLimits", limits)?;
        }
        map.serialize_entry("contextWindow", &self.context_window)?;
        map.serialize_entry("cost", &self.cost)?;
        if let Some(headers) = &self.headers {
            map.serialize_entry("headers", headers)?;
        }
        if let Some(compat) = &self.compat {
            map.serialize_entry("compat", compat)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ClassifierModel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(rename = "type", default)]
            r#type: Option<String>,
            id: String,
            name: String,
            api: ClassifierApi,
            provider: ProviderId,
            base_url: String,
            input: Vec<ModelInput>,
            #[serde(default)]
            input_limits: Option<ModelInputLimits>,
            context_window: i64,
            cost: ModelCost,
            #[serde(default)]
            headers: Option<IndexMap<String, String>>,
            #[serde(default)]
            compat: Option<SystemOneCompat>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if let Some(kind) = raw.r#type.as_deref() {
            if kind != "classifier" {
                return Err(de::Error::custom(format!(
                    "expected model type \"classifier\", got \"{kind}\""
                )));
            }
        }
        Ok(Self {
            id: raw.id,
            name: raw.name,
            api: raw.api,
            provider: raw.provider,
            base_url: raw.base_url,
            input: raw.input,
            input_limits: raw.input_limits,
            context_window: raw.context_window,
            cost: raw.cost,
            headers: raw.headers,
            compat: raw.compat,
        })
    }
}

/// Anything a provider or store can list, including future unknown model types (`Unknown`).
#[derive(Clone, Debug, PartialEq)]
pub enum AnyModel {
    Chat(Model),
    Image(ImageModel),
    Classifier(ClassifierModel),
    Unknown(JsonValue),
}

impl AnyModel {
    pub fn id(&self) -> &str {
        match self {
            Self::Chat(m) => &m.id,
            Self::Image(m) => &m.id,
            Self::Classifier(m) => &m.id,
            Self::Unknown(v) => v.get("id").and_then(|x| x.as_str()).unwrap_or(""),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Chat(m) => &m.name,
            Self::Image(m) => &m.name,
            Self::Classifier(m) => &m.name,
            Self::Unknown(v) => v.get("name").and_then(|x| x.as_str()).unwrap_or(""),
        }
    }

    pub fn api(&self) -> &str {
        match self {
            Self::Chat(m) => &m.api,
            Self::Image(m) => &m.api,
            Self::Classifier(m) => &m.api,
            Self::Unknown(v) => v.get("api").and_then(|x| x.as_str()).unwrap_or(""),
        }
    }

    pub fn provider(&self) -> &str {
        match self {
            Self::Chat(m) => &m.provider,
            Self::Image(m) => &m.provider,
            Self::Classifier(m) => &m.provider,
            Self::Unknown(v) => v.get("provider").and_then(|x| x.as_str()).unwrap_or(""),
        }
    }

    pub fn base_url(&self) -> &str {
        match self {
            Self::Chat(m) => &m.base_url,
            Self::Image(m) => &m.base_url,
            Self::Classifier(m) => &m.base_url,
            Self::Unknown(v) => v.get("baseUrl").and_then(|x| x.as_str()).unwrap_or(""),
        }
    }

    pub fn cost(&self) -> Option<&ModelCost> {
        match self {
            Self::Chat(m) => Some(&m.cost),
            Self::Image(m) => Some(&m.cost),
            Self::Classifier(m) => Some(&m.cost),
            Self::Unknown(_) => None,
        }
    }

    pub fn headers(&self) -> Option<&IndexMap<String, String>> {
        match self {
            Self::Chat(m) => m.headers.as_ref(),
            Self::Image(m) => m.headers.as_ref(),
            Self::Classifier(m) => m.headers.as_ref(),
            Self::Unknown(_) => None,
        }
    }

    pub fn model_type(&self) -> Option<ModelType> {
        match self {
            Self::Chat(_) => Some(ModelType::Chat),
            Self::Image(_) => Some(ModelType::Image),
            Self::Classifier(_) => Some(ModelType::Classifier),
            Self::Unknown(_) => None,
        }
    }

    pub fn raw_type_str(&self) -> Option<&str> {
        match self {
            Self::Chat(_) => Some("chat"),
            Self::Image(_) => Some("image"),
            Self::Classifier(_) => Some("classifier"),
            Self::Unknown(v) => v.get("type").and_then(|x| x.as_str()),
        }
    }

    pub fn as_chat(&self) -> Option<&Model> {
        match self {
            Self::Chat(m) => Some(m),
            _ => None,
        }
    }

    pub fn into_chat(self) -> Option<Model> {
        match self {
            Self::Chat(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_image(&self) -> Option<&ImageModel> {
        match self {
            Self::Image(m) => Some(m),
            _ => None,
        }
    }

    pub fn into_image(self) -> Option<ImageModel> {
        match self {
            Self::Image(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_classifier(&self) -> Option<&ClassifierModel> {
        match self {
            Self::Classifier(m) => Some(m),
            _ => None,
        }
    }

    pub fn into_classifier(self) -> Option<ClassifierModel> {
        match self {
            Self::Classifier(m) => Some(m),
            _ => None,
        }
    }
}

impl From<Model> for AnyModel {
    fn from(value: Model) -> Self {
        Self::Chat(value)
    }
}

impl From<ImageModel> for AnyModel {
    fn from(value: ImageModel) -> Self {
        Self::Image(value)
    }
}

impl From<ClassifierModel> for AnyModel {
    fn from(value: ClassifierModel) -> Self {
        Self::Classifier(value)
    }
}

impl Serialize for AnyModel {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Chat(m) => m.serialize(serializer),
            Self::Image(m) => m.serialize(serializer),
            Self::Classifier(m) => m.serialize(serializer),
            Self::Unknown(v) => v.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for AnyModel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct AnyModelVisitor;

        impl<'de> Visitor<'de> for AnyModelVisitor {
            type Value = AnyModel;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a model object")
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> std::result::Result<Self::Value, A::Error> {
                let val = Value::deserialize(de::value::MapAccessDeserializer::new(map))?;
                match val.get("type") {
                    None | Some(Value::Null) => serde_json::from_value(val)
                        .map(AnyModel::Chat)
                        .map_err(de::Error::custom),
                    Some(Value::String(s)) if s == "chat" => serde_json::from_value(val)
                        .map(AnyModel::Chat)
                        .map_err(de::Error::custom),
                    Some(Value::String(s)) if s == "image" => serde_json::from_value(val)
                        .map(AnyModel::Image)
                        .map_err(de::Error::custom),
                    Some(Value::String(s)) if s == "classifier" => serde_json::from_value(val)
                        .map(AnyModel::Classifier)
                        .map_err(de::Error::custom),
                    Some(_) => Ok(AnyModel::Unknown(val)),
                }
            }
        }

        deserializer.deserialize_map(AnyModelVisitor)
    }
}
