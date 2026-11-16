//! Port of packages/ai/src/api/openai-completions.ts

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use indexmap::IndexMap;
use pi_js::abort::AbortSignal;
use pi_js::error::{Error, Result};
use pi_js::fetch::{Body, Headers, Request, Response, fetch as default_fetch};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::api::constrained_sampling::{
    GrammarConstrainedSampling, GrammarSamplingFormat, GrammarToolInputJsonBuffer,
    append_grammar_tool_input_json_delta, create_grammar_tool_input_properties, get_json_schema_tool_parameters,
    resolve_grammar_constrained_sampling, resolve_json_schema_strict_sampling,
};
use crate::api::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::api::openai_responses_shared::{create_pending_assistant_message, response_to_sse_json_stream};
use crate::api::simple_options::{build_base_options, clamp_thinking_budget_to_answer_room, thinking_budget_for_level};
use crate::api::transform_messages::transform_messages;
use crate::models::{calculate_cost, clamp_thinking_level};
use crate::types::{
    AnyModel, ApiStreamOptions, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, CacheControlFormat,
    CacheRetention, ChatTemplateKwargValue, ChatTemplateVarKind, MaxTokensField, Message, Model, ModelModality,
    ModelThinkingLevel, OpenAICompletionsCompat, ProviderEnv, ProviderHeaders, ProviderResponse, ProviderStreams,
    SessionAffinityFormat, SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingBudgets,
    ThinkingContent, ThinkingFormat, ThinkingLevel, ThinkingTokenBudgetField, Tool, ToolCall, ToolChoice,
    TranscriptContext, Usage, UserContentBlock, UserMessageContent,
};
use crate::utils::abort_signals::combine_abort_signals;
use crate::utils::error_body::{format_provider_error, normalize_provider_error};
use crate::utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use crate::utils::headers::headers_to_record;
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::pi_user_agent::get_pi_user_agent;
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{ProviderHttpError, ProviderRetryOptions, retry_provider_request};
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::text::{get_system_message_text, render_system_message_update};
use crate::utils::transcript::{
    get_declared_tools, get_initial_system_message, resolve_transcript, resolve_transcript_tools,
    without_initial_system_message,
};

/// Reasoning effort values accepted by OpenAI Chat Completions requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenAICompletionsReasoningEffort {
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl OpenAICompletionsReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
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
            Self::Minimal => ModelThinkingLevel::Minimal,
            Self::Low => ModelThinkingLevel::Low,
            Self::Medium => ModelThinkingLevel::Medium,
            Self::High => ModelThinkingLevel::High,
            Self::XHigh => ModelThinkingLevel::XHigh,
            Self::Max => ModelThinkingLevel::Max,
        }
    }

    pub fn to_thinking_level(self) -> ThinkingLevel {
        match self {
            Self::Minimal => ThinkingLevel::Minimal,
            Self::Low => ThinkingLevel::Low,
            Self::Medium => ThinkingLevel::Medium,
            Self::High => ThinkingLevel::High,
            Self::XHigh => ThinkingLevel::XHigh,
            Self::Max => ThinkingLevel::Max,
        }
    }
}

/// Options for OpenAI Chat Completions requests.
#[derive(Debug, Clone, Default)]
pub struct OpenAICompletionsOptions {
    pub base: StreamOptions,
    pub tool_choice: Option<Value>,
    pub reasoning_effort: Option<OpenAICompletionsReasoningEffort>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub sampling_params: Option<IndexMap<String, Value>>,
}

impl Deref for OpenAICompletionsOptions {
    type Target = StreamOptions;
    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for OpenAICompletionsOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl From<&OpenAICompletionsOptions> for ApiStreamOptions {
    fn from(options: &OpenAICompletionsOptions) -> Self {
        let mut extra: IndexMap<String, Value> = IndexMap::new();
        if let Some(tc) = &options.tool_choice {
            extra.insert("toolChoice".to_string(), tc.clone());
        }
        if let Some(effort) = options.reasoning_effort {
            extra.insert(
                "reasoningEffort".to_string(),
                Value::String(effort.as_str().to_string()),
            );
        }
        if let Some(budgets) = &options.thinking_budgets {
            let mut m = Map::new();
            if let Some(v) = budgets.minimal {
                m.insert("minimal".to_string(), Value::from(v));
            }
            if let Some(v) = budgets.low {
                m.insert("low".to_string(), Value::from(v));
            }
            if let Some(v) = budgets.medium {
                m.insert("medium".to_string(), Value::from(v));
            }
            if let Some(v) = budgets.high {
                m.insert("high".to_string(), Value::from(v));
            }
            extra.insert("thinkingBudgets".to_string(), Value::Object(m));
        }
        if let Some(sp) = &options.sampling_params {
            let m: Map<String, Value> = sp.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            extra.insert("samplingParams".to_string(), Value::Object(m));
        }
        let extension: Arc<dyn std::any::Any + Send + Sync> = Arc::new(options.clone());
        Self {
            base: options.base.clone(),
            extra,
            extension: Some(extension),
        }
    }
}

impl From<OpenAICompletionsOptions> for ApiStreamOptions {
    fn from(options: OpenAICompletionsOptions) -> Self {
        Self::from(&options)
    }
}

impl OpenAICompletionsOptions {
    fn from_api_options(options: Option<ApiStreamOptions>) -> Self {
        let Some(api_opts) = options else {
            return Self::default();
        };
        if let Some(ext) = &api_opts.extension {
            if let Some(typed) = ext.downcast_ref::<Self>() {
                return typed.clone();
            }
        }
        let reasoning_effort = api_opts
            .extra
            .get("reasoningEffort")
            .and_then(Value::as_str)
            .and_then(OpenAICompletionsReasoningEffort::from_str_opt);
        let tool_choice = api_opts.extra.get("toolChoice").cloned();
        let thinking_budgets = api_opts
            .extra
            .get("thinkingBudgets")
            .and_then(Value::as_object)
            .map(|obj| ThinkingBudgets {
                minimal: obj.get("minimal").and_then(Value::as_i64),
                low: obj.get("low").and_then(Value::as_i64),
                medium: obj.get("medium").and_then(Value::as_i64),
                high: obj.get("high").and_then(Value::as_i64),
            });
        let mut merged_sampling: IndexMap<String, Value> = IndexMap::new();
        if let Some(sp) = &api_opts.base.sampling_params {
            for (k, v) in sp {
                merged_sampling.insert(k.clone(), v.clone());
            }
        }
        if let Some(obj) = api_opts.extra.get("samplingParams").and_then(Value::as_object) {
            for (k, v) in obj {
                merged_sampling.insert(k.clone(), v.clone());
            }
        }
        let sampling_params = (!merged_sampling.is_empty()).then_some(merged_sampling);
        Self {
            base: api_opts.base,
            tool_choice,
            reasoning_effort,
            thinking_budgets,
            sampling_params,
        }
    }
}

/// Options controlling `convert_messages`.
#[derive(Debug, Clone, Default)]
pub struct ConvertCompletionsMessagesOptions {
    pub grammar_tool_input_properties: Option<IndexMap<String, String>>,
}

/// Resolved OpenAI Chat Completions compatibility flags.
#[derive(Debug, Clone)]
pub struct ResolvedOpenAICompletionsCompat {
    pub supports_store: bool,
    pub supports_developer_role: bool,
    pub supports_reasoning_effort: bool,
    pub supports_usage_in_streaming: bool,
    pub max_tokens_field: MaxTokensField,
    pub requires_tool_result_name: bool,
    pub requires_assistant_after_tool_result: bool,
    pub requires_thinking_as_text: bool,
    pub requires_mistral_tool_ids: bool,
    pub requires_reasoning_content_on_assistant_messages: Option<bool>,
    pub thinking_format: ThinkingFormat,
    pub supports_strict_mode: bool,
    pub zai_tool_stream: bool,
    pub supports_finish_reason: bool,
    pub supports_long_cache_retention: bool,
    pub send_session_affinity_headers: bool,
    pub session_affinity_format: SessionAffinityFormat,
    pub supports_open_ai_grammar_tools: bool,
    pub supports_open_ai_custom_tools: bool,
    pub cache_control_format: Option<CacheControlFormat>,
    pub supports_thinking_token_budget: Option<bool>,
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    pub supports_mid_convo_system_messages: Option<bool>,
    pub supports_mid_convo_tool_additions: Option<bool>,
    pub vllm_priority: Option<i64>,
    pub chat_template_kwargs: Option<IndexMap<String, ChatTemplateKwargValue>>,
    pub chat_template_args: Option<IndexMap<String, ChatTemplateKwargValue>>,
}

// WIRE(@mcp-w2): use crate::api::github_copilot_headers::{build_copilot_dynamic_headers, has_copilot_vision_input}
mod copilot_headers_fallback {
    use super::*;

    pub fn has_copilot_vision_input(messages: &[Message]) -> bool {
        messages.iter().any(|m| match m {
            Message::User(u) => matches!(&u.content, UserMessageContent::Blocks(b) if b.iter().any(|x| matches!(x, UserContentBlock::Image(_)))),
            Message::ToolResult(tr) => tr.content.iter().any(|x| matches!(x, UserContentBlock::Image(_))),
            _ => false,
        })
    }

    pub fn build_copilot_dynamic_headers(messages: &[Message], has_images: bool) -> IndexMap<String, String> {
        let mut h = IndexMap::new();
        let is_agent = messages.last().is_some_and(|m| !matches!(m, Message::User(_)));
        h.insert(
            "X-Initiator".to_string(),
            if is_agent { "agent" } else { "user" }.to_string(),
        );
        h.insert("Openai-Intent".to_string(), "conversation-edits".to_string());
        if has_images {
            h.insert("Copilot-Vision-Request".to_string(), "true".to_string());
        }
        h
    }
}

/// Resolve compatibility settings for an `openai-completions` model.
pub fn get_compat(model: &Model) -> ResolvedOpenAICompletionsCompat {
    let base_url = model.base_url.to_ascii_lowercase();
    let provider = model.provider.as_str();

    let is_zai = provider == "zai"
        || provider == "zai-coding-cn"
        || base_url.contains("api.z.ai")
        || base_url.contains("open.bigmodel.cn");
    let is_mistral = provider == "mistral" || base_url.contains("mistral.ai");
    let is_grok = provider == "xai" || base_url.contains("api.x.ai");
    let is_together =
        provider == "together" || base_url.contains("api.together.ai") || base_url.contains("api.together.xyz");
    let is_cloudflare_workers_ai = provider == "cloudflare-workers-ai" || base_url.contains("api.cloudflare.com");
    let is_cloudflare_ai_gateway =
        provider == "cloudflare-ai-gateway" || base_url.contains("gateway.ai.cloudflare.com");
    let is_nvidia = provider == "nvidia" || base_url.contains("integrate.api.nvidia.com");
    let is_ant_ling = provider == "ant-ling" || base_url.contains("api.ant-ling.com");
    let is_deepseek = provider == "deepseek" || base_url.contains("deepseek.com");
    let is_moonshot = provider == "moonshotai" || provider == "moonshotai-cn" || base_url.contains("api.moonshot.");
    let is_non_standard = provider == "cerebras"
        || base_url.contains("cerebras.ai")
        || is_grok
        || is_mistral
        || base_url.contains("chutes.ai")
        || is_deepseek
        || is_moonshot
        || is_zai
        || provider == "opencode"
        || base_url.contains("opencode.ai")
        || is_together
        || is_cloudflare_workers_ai
        || is_cloudflare_ai_gateway
        || is_nvidia
        || is_ant_ling;
    let use_max_tokens = is_mistral
        || base_url.contains("chutes.ai")
        || is_deepseek
        || is_moonshot
        || is_cloudflare_ai_gateway
        || is_together
        || is_nvidia
        || is_ant_ling
        || is_zai;

    let default_affinity = if provider == "openrouter" || base_url.contains("openrouter.ai") {
        SessionAffinityFormat::OpenRouter
    } else if provider == "opencode" {
        SessionAffinityFormat::OpenAINosession
    } else {
        SessionAffinityFormat::OpenAI
    };

    let send_affinity_default = provider == "openai"
        || base_url.contains("api.openai.com")
        || provider == "openrouter"
        || base_url.contains("openrouter.ai");

    let c: Option<&OpenAICompletionsCompat> = model.compat.as_ref().and_then(|mc| mc.as_openai_completions());

    ResolvedOpenAICompletionsCompat {
        supports_store: c.and_then(|x| x.supports_store).unwrap_or(!is_non_standard),
        supports_developer_role: c.and_then(|x| x.supports_developer_role).unwrap_or(!is_non_standard),
        supports_reasoning_effort: c
            .and_then(|x| x.supports_reasoning_effort)
            .unwrap_or(!is_grok && !is_zai),
        supports_usage_in_streaming: c.and_then(|x| x.supports_usage_in_streaming).unwrap_or(true),
        max_tokens_field: c.and_then(|x| x.max_tokens_field).unwrap_or(if use_max_tokens {
            MaxTokensField::MaxTokens
        } else {
            MaxTokensField::MaxCompletionTokens
        }),
        requires_tool_result_name: c.and_then(|x| x.requires_tool_result_name).unwrap_or(is_mistral),
        requires_assistant_after_tool_result: c.and_then(|x| x.requires_assistant_after_tool_result).unwrap_or(false),
        requires_thinking_as_text: c.and_then(|x| x.requires_thinking_as_text).unwrap_or(false),
        requires_mistral_tool_ids: is_mistral,
        requires_reasoning_content_on_assistant_messages: c
            .and_then(|x| x.requires_reasoning_content_on_assistant_messages),
        thinking_format: c.and_then(|x| x.thinking_format).unwrap_or(if is_zai {
            ThinkingFormat::Zai
        } else {
            ThinkingFormat::OpenAI
        }),
        supports_strict_mode: c.and_then(|x| x.supports_strict_mode).unwrap_or(false),
        zai_tool_stream: c.and_then(|x| x.zai_tool_stream).unwrap_or(false),
        supports_finish_reason: c.and_then(|x| x.supports_finish_reason).unwrap_or(true),
        supports_long_cache_retention: c.and_then(|x| x.supports_long_cache_retention).unwrap_or(
            !(is_together || is_cloudflare_workers_ai || is_cloudflare_ai_gateway || is_nvidia || is_ant_ling),
        ),
        send_session_affinity_headers: c
            .and_then(|x| x.send_session_affinity_headers)
            .unwrap_or(send_affinity_default),
        session_affinity_format: c.and_then(|x| x.session_affinity_format).unwrap_or(default_affinity),
        supports_open_ai_grammar_tools: c.and_then(|x| x.supports_open_ai_grammar_tools).unwrap_or(false),
        supports_open_ai_custom_tools: false,
        cache_control_format: c.and_then(|x| x.cache_control_format),
        supports_thinking_token_budget: c.and_then(|x| x.supports_thinking_token_budget),
        thinking_token_budget_field: c.and_then(|x| x.thinking_token_budget_field),
        supports_mid_convo_system_messages: c.and_then(|x| x.supports_mid_convo_system_messages),
        supports_mid_convo_tool_additions: c.and_then(|x| x.supports_mid_convo_tool_additions),
        vllm_priority: c.and_then(|x| x.vllm_priority),
        chat_template_kwargs: c.and_then(|x| x.chat_template_kwargs.clone()),
        chat_template_args: c.and_then(|x| x.chat_template_args.clone()),
    }
}

fn resolve_cache_retention(cache_retention: Option<CacheRetention>, env: Option<&ProviderEnv>) -> CacheRetention {
    if let Some(cr) = cache_retention {
        return cr;
    }
    if get_provider_env_value("PI_CACHE_RETENTION", env).is_some_and(|v| v.eq_ignore_ascii_case("long")) {
        return CacheRetention::Long;
    }
    CacheRetention::Short
}

fn has_header_non_empty(headers: Option<&ProviderHeaders>, name: &str) -> bool {
    let Some(map) = headers else {
        return false;
    };
    map.iter()
        .any(|(k, v)| k.eq_ignore_ascii_case(name) && v.as_deref().is_some_and(|s| !pi_js::str16::trim(s).is_empty()))
}

fn get_client_api_key(provider: &str, api_key: Option<&str>, headers: Option<&ProviderHeaders>) -> Result<String> {
    if let Some(k) = api_key {
        if !k.is_empty() {
            return Ok(k.to_string());
        }
    }
    if has_header_non_empty(headers, "authorization") || has_header_non_empty(headers, "cf-aig-authorization") {
        return Ok("unused".to_string());
    }
    Err(Error::msg(format!("No API key for provider: {provider}")))
}

fn has_tool_history(messages: &[Message]) -> bool {
    messages.iter().any(|m| match m {
        Message::ToolResult(_) => true,
        Message::Assistant(a) => a
            .content
            .iter()
            .any(|b| matches!(b, AssistantContentBlock::ToolCall(_))),
        _ => false,
    })
}

fn is_reasoning_field(field: &str) -> bool {
    matches!(field, "reasoning" | "reasoning_content" | "reasoning_text")
}

fn normalize_completions_tool_call_id(id: &str, target: &Model, compat: &ResolvedOpenAICompletionsCompat) -> String {
    if compat.requires_mistral_tool_ids {
        let mut alnum: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).take(9).collect();
        while alnum.len() < 9 {
            alnum.push('0');
        }
        return alnum;
    }
    if let Some((call_id, _)) = id.split_once('|') {
        if target.provider == "openai" && call_id.len() > 40 {
            return call_id.chars().take(40).collect();
        }
        return call_id.to_string();
    }
    if target.provider == "openai" && id.len() > 40 {
        return id.chars().take(40).collect();
    }
    id.to_string()
}

fn parse_openai_reasoning_details(signature: Option<&str>) -> Option<Vec<Value>> {
    let sig = signature?;
    let Value::Array(arr) = pi_js::json::parse(sig).ok()? else {
        return None;
    };
    if arr.is_empty() {
        return None;
    }
    let valid = arr.iter().all(|d| {
        let Some(obj) = d.as_object() else {
            return false;
        };
        match obj.get("type").and_then(Value::as_str) {
            Some("reasoning.summary") => obj.get("summary").and_then(Value::as_str).is_some(),
            Some("reasoning.encrypted") => obj.get("data").and_then(Value::as_str).is_some(),
            Some("reasoning.text") => obj.get("text").and_then(Value::as_str).is_some(),
            _ => false,
        }
    });
    if valid { Some(arr) } else { None }
}

fn parse_legacy_encrypted_reasoning_detail(signature: Option<&str>) -> Option<Value> {
    let sig = signature?;
    let parsed = pi_js::json::parse(sig).ok()?;
    let obj = parsed.as_object()?;
    if obj.get("type").and_then(Value::as_str) != Some("reasoning.encrypted") {
        return None;
    }
    let id_ok = obj.get("id").and_then(Value::as_str).is_some_and(|s| !s.is_empty());
    let data_ok = obj.get("data").and_then(Value::as_str).is_some_and(|s| !s.is_empty());
    if id_ok && data_ok { Some(parsed) } else { None }
}

/// Convert `TranscriptContext` messages into Chat Completions `messages` JSON items.
pub fn convert_messages(
    model: &Model,
    context: &TranscriptContext,
    compat: &ResolvedOpenAICompletionsCompat,
    options: Option<&ConvertCompletionsMessagesOptions>,
) -> Vec<Value> {
    let grammar_props = options.and_then(|o| o.grammar_tool_input_properties.as_ref());
    let compat_clone = compat.clone();
    let normalize_cb = |id: &str, target: &Model, _src: &crate::types::AssistantMessage| -> String {
        normalize_completions_tool_call_id(id, target, &compat_clone)
    };

    let allow_tool_changes = compat.supports_mid_convo_system_messages == Some(true)
        && compat.supports_mid_convo_tool_additions == Some(true);
    let transcript_tools = resolve_transcript_tools(&context.messages, allow_tool_changes);
    let initial_sys = get_initial_system_message(&context.messages);
    let body_msgs = without_initial_system_message(&context.messages);
    let transformed = transform_messages(&body_msgs, model, Some(&normalize_cb));

    let mut params: Vec<Value> = Vec::new();
    let allow_images = model.input.contains(&ModelModality::Image);

    if let Some(sys) = initial_sys {
        let role = if model.reasoning && compat.supports_developer_role {
            "developer"
        } else {
            "system"
        };
        let mut obj = Map::new();
        obj.insert("role".to_string(), Value::String(role.to_string()));
        obj.insert(
            "content".to_string(),
            Value::String(sanitize_surrogates(&get_system_message_text(sys))),
        );
        params.push(Value::Object(obj));
    }

    let mut last_role: Option<String> = params
        .last()
        .and_then(|v| v.get("role").and_then(Value::as_str).map(str::to_string));

    let mut i = 0;
    while i < transformed.len() {
        let msg = &transformed[i];
        match msg {
            Message::System(sys) => {
                if compat.supports_mid_convo_system_messages == Some(true) {
                    if transcript_tools.anchors_additions {
                        if let Some(added) = sys.tools_added.as_deref().filter(|t| !t.is_empty()) {
                            if let Ok(converted) = convert_tools(added, compat) {
                                let mut tool_msg = Map::new();
                                tool_msg.insert("role".to_string(), Value::String("system".to_string()));
                                tool_msg.insert("tools".to_string(), Value::Array(converted));
                                params.push(Value::Object(tool_msg));
                                last_role = Some("system".to_string());
                            }
                        }
                    }
                    let role = if model.reasoning && compat.supports_developer_role {
                        "developer"
                    } else {
                        "system"
                    };
                    let text = render_system_message_update(sys);
                    if !text.is_empty() {
                        let mut obj = Map::new();
                        obj.insert("role".to_string(), Value::String(role.to_string()));
                        obj.insert("content".to_string(), Value::String(sanitize_surrogates(&text)));
                        last_role = Some(role.to_string());
                        params.push(Value::Object(obj));
                    }
                }
                i += 1;
            }
            Message::User(user) => {
                if compat.requires_assistant_after_tool_result && last_role.as_deref() == Some("tool") {
                    let mut ack = Map::new();
                    ack.insert("role".to_string(), Value::String("assistant".to_string()));
                    ack.insert(
                        "content".to_string(),
                        Value::String("I have processed the tool results.".to_string()),
                    );
                    params.push(Value::Object(ack));
                }
                match &user.content {
                    UserMessageContent::Text(text) => {
                        let mut obj = Map::new();
                        obj.insert("role".to_string(), Value::String("user".to_string()));
                        obj.insert("content".to_string(), Value::String(sanitize_surrogates(text)));
                        last_role = Some("user".to_string());
                        params.push(Value::Object(obj));
                    }
                    UserMessageContent::Blocks(blocks) => {
                        let mut parts = Vec::new();
                        for b in blocks {
                            match b {
                                UserContentBlock::Text(t) => {
                                    if !t.text.is_empty() {
                                        let mut p = Map::new();
                                        p.insert("type".to_string(), Value::String("text".to_string()));
                                        p.insert("text".to_string(), Value::String(sanitize_surrogates(&t.text)));
                                        parts.push(Value::Object(p));
                                    }
                                }
                                UserContentBlock::Image(img) if allow_images => {
                                    let mut iu = Map::new();
                                    iu.insert(
                                        "url".to_string(),
                                        Value::String(format!("data:{};base64,{}", img.mime_type, img.data)),
                                    );
                                    let mut p = Map::new();
                                    p.insert("type".to_string(), Value::String("image_url".to_string()));
                                    p.insert("image_url".to_string(), Value::Object(iu));
                                    parts.push(Value::Object(p));
                                }
                                UserContentBlock::Image(_) => {}
                            }
                        }
                        let filtered_parts = if allow_images {
                            parts
                        } else {
                            parts
                                .into_iter()
                                .filter(|p| p.get("type").and_then(Value::as_str) != Some("image_url"))
                                .collect()
                        };
                        if !filtered_parts.is_empty() {
                            let mut obj = Map::new();
                            obj.insert("role".to_string(), Value::String("user".to_string()));
                            obj.insert("content".to_string(), Value::Array(filtered_parts));
                            last_role = Some("user".to_string());
                            params.push(Value::Object(obj));
                        }
                    }
                }
                i += 1;
            }
            Message::Assistant(assistant) => {
                let mut msg_obj = Map::new();
                msg_obj.insert("role".to_string(), Value::String("assistant".to_string()));

                let mut content_parts: Vec<Value> = Vec::new();
                let mut tool_calls_json: Vec<Value> = Vec::new();
                let mut reasoning_details: Option<Vec<Value>> = None;
                let mut legacy_reasoning_details: Vec<Value> = Vec::new();
                let mut reasoning_fields: IndexMap<String, String> = IndexMap::new();
                let is_same_model =
                    assistant.api == model.api && assistant.provider == model.provider && assistant.model == model.id;

                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Thinking(thinking) => {
                            if thinking.thinking.is_empty() && thinking.thinking_signature.is_none() {
                                continue;
                            }
                            if compat.requires_thinking_as_text {
                                if !thinking.thinking.is_empty() {
                                    let mut p = Map::new();
                                    p.insert("type".to_string(), Value::String("text".to_string()));
                                    p.insert(
                                        "text".to_string(),
                                        Value::String(sanitize_surrogates(&thinking.thinking)),
                                    );
                                    content_parts.push(Value::Object(p));
                                }
                            } else if let Some(details) =
                                parse_openai_reasoning_details(thinking.thinking_signature.as_deref())
                            {
                                reasoning_details = Some(details);
                            } else if let Some(sig) = thinking.thinking_signature.as_deref() {
                                if is_reasoning_field(sig) && is_same_model {
                                    let target_field = if model.provider == "opencode-go" && sig == "reasoning" {
                                        "reasoning_content"
                                    } else {
                                        sig
                                    };
                                    reasoning_fields
                                        .insert(target_field.to_string(), sanitize_surrogates(&thinking.thinking));
                                } else if !thinking.thinking.is_empty() {
                                    let mut p = Map::new();
                                    p.insert("type".to_string(), Value::String("text".to_string()));
                                    p.insert(
                                        "text".to_string(),
                                        Value::String(sanitize_surrogates(&thinking.thinking)),
                                    );
                                    content_parts.push(Value::Object(p));
                                }
                            } else if !thinking.thinking.is_empty() {
                                let mut p = Map::new();
                                p.insert("type".to_string(), Value::String("text".to_string()));
                                p.insert(
                                    "text".to_string(),
                                    Value::String(sanitize_surrogates(&thinking.thinking)),
                                );
                                content_parts.push(Value::Object(p));
                            }
                        }
                        AssistantContentBlock::Text(text) => {
                            if !text.text.is_empty() {
                                let mut p = Map::new();
                                p.insert("type".to_string(), Value::String("text".to_string()));
                                p.insert("text".to_string(), Value::String(sanitize_surrogates(&text.text)));
                                content_parts.push(Value::Object(p));
                            }
                        }
                        AssistantContentBlock::ToolCall(tc) => {
                            if let Some(detail) =
                                parse_legacy_encrypted_reasoning_detail(tc.thought_signature.as_deref())
                            {
                                legacy_reasoning_details.push(detail);
                            }
                            let call_id = normalize_completions_tool_call_id(&tc.id, model, compat);
                            let custom_prop = grammar_props.and_then(|m| m.get(&tc.name)).map(String::as_str);
                            let mut tc_obj = Map::new();
                            tc_obj.insert("id".to_string(), Value::String(call_id));
                            if let Some(prop) = custom_prop {
                                let input_str = tc.arguments.get(prop).and_then(Value::as_str).unwrap_or("");
                                tc_obj.insert("type".to_string(), Value::String("custom".to_string()));
                                let mut custom_inner = Map::new();
                                custom_inner.insert("name".to_string(), Value::String(tc.name.clone()));
                                custom_inner.insert("input".to_string(), Value::String(input_str.to_string()));
                                tc_obj.insert("custom".to_string(), Value::Object(custom_inner));
                            } else {
                                tc_obj.insert("type".to_string(), Value::String("function".to_string()));
                                let arg_map: Map<String, Value> =
                                    tc.arguments.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                                let mut fn_inner = Map::new();
                                fn_inner.insert("name".to_string(), Value::String(tc.name.clone()));
                                fn_inner.insert(
                                    "arguments".to_string(),
                                    Value::String(pi_js::json::stringify(&Value::Object(arg_map))),
                                );
                                tc_obj.insert("function".to_string(), Value::Object(fn_inner));
                            }
                            tool_calls_json.push(Value::Object(tc_obj));
                        }
                    }
                }

                if reasoning_details.is_none() && !legacy_reasoning_details.is_empty() {
                    reasoning_details = Some(legacy_reasoning_details);
                }
                if reasoning_details.is_some() {
                    reasoning_fields.clear();
                }

                if content_parts.is_empty() {
                    if tool_calls_json.is_empty() && reasoning_details.is_none() && reasoning_fields.is_empty() {
                        i += 1;
                        continue;
                    }
                    msg_obj.insert("content".to_string(), Value::Null);
                } else if content_parts.len() == 1
                    && !compat.requires_thinking_as_text
                    && content_parts[0].get("type").and_then(Value::as_str) == Some("text")
                {
                    if let Some(txt) = content_parts[0].get("text").cloned() {
                        msg_obj.insert("content".to_string(), txt);
                    }
                } else {
                    msg_obj.insert("content".to_string(), Value::Array(content_parts));
                }

                if !tool_calls_json.is_empty() {
                    msg_obj.insert("tool_calls".to_string(), Value::Array(tool_calls_json));
                }
                for (field, val) in reasoning_fields {
                    msg_obj.insert(field, Value::String(val));
                }
                if let Some(details) = reasoning_details {
                    msg_obj.insert("reasoning_details".to_string(), Value::Array(details));
                }
                if compat.requires_reasoning_content_on_assistant_messages == Some(true)
                    && !msg_obj.contains_key("reasoning_content")
                {
                    msg_obj.insert("reasoning_content".to_string(), Value::String(String::new()));
                }

                last_role = Some("assistant".to_string());
                params.push(Value::Object(msg_obj));
                i += 1;
            }
            Message::ToolResult(_) => {
                let mut image_blocks: Vec<Value> = Vec::new();
                while i < transformed.len() {
                    let Message::ToolResult(tr) = &transformed[i] else {
                        break;
                    };
                    let mut texts: Vec<String> = Vec::new();
                    for b in &tr.content {
                        match b {
                            UserContentBlock::Text(t) => texts.push(sanitize_surrogates(&t.text)),
                            UserContentBlock::Image(img) if allow_images => {
                                let mut iu = Map::new();
                                iu.insert(
                                    "url".to_string(),
                                    Value::String(format!("data:{};base64,{}", img.mime_type, img.data)),
                                );
                                let mut p = Map::new();
                                p.insert("type".to_string(), Value::String("image_url".to_string()));
                                p.insert("image_url".to_string(), Value::Object(iu));
                                image_blocks.push(Value::Object(p));
                            }
                            UserContentBlock::Image(_) => {}
                        }
                    }
                    let joined = texts.join("\n");
                    let content_str = if !joined.is_empty() {
                        joined
                    } else if !image_blocks.is_empty() {
                        "(see attached image)".to_string()
                    } else {
                        "(no tool output)".to_string()
                    };
                    let mut tr_obj = Map::new();
                    tr_obj.insert("role".to_string(), Value::String("tool".to_string()));
                    tr_obj.insert(
                        "tool_call_id".to_string(),
                        Value::String(normalize_completions_tool_call_id(&tr.tool_call_id, model, compat)),
                    );
                    tr_obj.insert("content".to_string(), Value::String(content_str));
                    if compat.requires_tool_result_name {
                        tr_obj.insert("name".to_string(), Value::String(tr.tool_name.clone()));
                    }
                    last_role = Some("tool".to_string());
                    params.push(Value::Object(tr_obj));
                    i += 1;
                }

                if !image_blocks.is_empty() {
                    if compat.requires_assistant_after_tool_result {
                        let mut ack = Map::new();
                        ack.insert("role".to_string(), Value::String("assistant".to_string()));
                        ack.insert(
                            "content".to_string(),
                            Value::String("I have processed the tool results.".to_string()),
                        );
                        params.push(Value::Object(ack));
                    }
                    let mut intro = Map::new();
                    intro.insert("type".to_string(), Value::String("text".to_string()));
                    intro.insert(
                        "text".to_string(),
                        Value::String("Attached image(s) from tool result:".to_string()),
                    );
                    let mut parts = vec![Value::Object(intro)];
                    parts.extend(image_blocks);
                    let mut user_img = Map::new();
                    user_img.insert("role".to_string(), Value::String("user".to_string()));
                    user_img.insert("content".to_string(), Value::Array(parts));
                    last_role = Some("user".to_string());
                    params.push(Value::Object(user_img));
                }
            }
        }
    }

    params
}

fn convert_tools(tools: &[Tool], compat: &ResolvedOpenAICompletionsCompat) -> Result<Vec<Value>> {
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        if let Some(GrammarConstrainedSampling {
            format,
            definition,
            input_property: _,
        }) = resolve_grammar_constrained_sampling(tool, compat.supports_open_ai_grammar_tools)?
        {
            let mut fmt_obj = Map::new();
            fmt_obj.insert("type".to_string(), Value::String("grammar".to_string()));
            match format {
                GrammarSamplingFormat::Lark => {
                    fmt_obj.insert("syntax".to_string(), Value::String("lark".to_string()));
                    fmt_obj.insert("definition".to_string(), Value::String(definition));
                }
                GrammarSamplingFormat::Regex => {
                    fmt_obj.insert("syntax".to_string(), Value::String("regex".to_string()));
                    fmt_obj.insert("definition".to_string(), Value::String(definition));
                }
            }
            let mut custom_obj = Map::new();
            custom_obj.insert("name".to_string(), Value::String(tool.name.clone()));
            custom_obj.insert("description".to_string(), Value::String(tool.description.clone()));
            custom_obj.insert("format".to_string(), Value::Object(fmt_obj));
            let mut wrapper = Map::new();
            wrapper.insert("type".to_string(), Value::String("custom".to_string()));
            wrapper.insert("custom".to_string(), Value::Object(custom_obj));
            out.push(Value::Object(wrapper));
            continue;
        }

        let strict_opt = resolve_json_schema_strict_sampling(tool, compat.supports_strict_mode, None)?;
        let params_schema = get_json_schema_tool_parameters(tool, strict_opt)?;

        let mut fn_obj = Map::new();
        fn_obj.insert("name".to_string(), Value::String(tool.name.clone()));
        fn_obj.insert("description".to_string(), Value::String(tool.description.clone()));
        fn_obj.insert("parameters".to_string(), params_schema.to_value());
        if let Some(strict_val) = strict_opt {
            fn_obj.insert("strict".to_string(), Value::Bool(strict_val));
        }
        let mut wrapper = Map::new();
        wrapper.insert("type".to_string(), Value::String("function".to_string()));
        wrapper.insert("function".to_string(), Value::Object(fn_obj));
        out.push(Value::Object(wrapper));
    }
    Ok(out)
}

fn cache_control_json(cache_retention: CacheRetention) -> Value {
    let mut obj = Map::new();
    obj.insert("type".to_string(), Value::String("ephemeral".to_string()));
    if cache_retention == CacheRetention::Long {
        obj.insert("ttl".to_string(), Value::String("1h".to_string()));
    }
    Value::Object(obj)
}

fn mark_last_text_cache_control(msg: &mut Value, marker: &Value) {
    let Some(obj) = msg.as_object_mut() else {
        return;
    };
    match obj.get_mut("content") {
        Some(Value::String(s)) => {
            let text = s.clone();
            let mut part = Map::new();
            part.insert("type".to_string(), Value::String("text".to_string()));
            part.insert("text".to_string(), Value::String(text));
            part.insert("cache_control".to_string(), marker.clone());
            obj.insert("content".to_string(), Value::Array(vec![Value::Object(part)]));
        }
        Some(Value::Array(arr)) => {
            if let Some(last) = arr.iter_mut().rev().find(|v| v.is_object()) {
                if let Some(part_obj) = last.as_object_mut() {
                    part_obj.insert("cache_control".to_string(), marker.clone());
                }
            }
        }
        _ => {}
    }
}

fn apply_anthropic_cache_control(messages: &mut [Value], tools: Option<&mut [Value]>, marker: &Value) {
    if let Some(first_inst) = messages
        .iter_mut()
        .find(|m| matches!(m.get("role").and_then(Value::as_str), Some("system" | "developer")))
    {
        mark_last_text_cache_control(first_inst, marker);
    }
    if let Some(tool_slice) = tools {
        if let Some(Value::Object(last_tool)) = tool_slice.last_mut() {
            last_tool.insert("cache_control".to_string(), marker.clone());
        }
    }
    if let Some(last_msg) = messages.last_mut() {
        mark_last_text_cache_control(last_msg, marker);
    }
}

fn resolve_clamped_thinking_budget(
    model: &Model,
    options: &OpenAICompletionsOptions,
    max_output_cap: Option<i64>,
) -> Option<i64> {
    let effort = options.reasoning_effort?;
    if !model.reasoning {
        return None;
    }
    let clamped = clamp_thinking_level(model, effort.to_model_thinking_level());
    if clamped == ModelThinkingLevel::Off {
        return None;
    }
    let thinking_level = match clamped {
        ModelThinkingLevel::Off => return None,
        ModelThinkingLevel::Minimal => ThinkingLevel::Minimal,
        ModelThinkingLevel::Low => ThinkingLevel::Low,
        ModelThinkingLevel::Medium => ThinkingLevel::Medium,
        ModelThinkingLevel::High => ThinkingLevel::High,
        ModelThinkingLevel::XHigh => ThinkingLevel::XHigh,
        ModelThinkingLevel::Max => ThinkingLevel::Max,
    };
    let raw_budget = thinking_budget_for_level(thinking_level, options.thinking_budgets.as_ref());
    let ceiling = max_output_cap.unwrap_or(model.max_tokens).min(model.max_tokens);
    Some(clamp_thinking_budget_to_answer_room(raw_budget, ceiling))
}

fn resolve_thinking_token_budget_field(compat: &ResolvedOpenAICompletionsCompat) -> Option<&'static str> {
    if let Some(field) = compat.thinking_token_budget_field {
        return Some(match field {
            ThinkingTokenBudgetField::ThinkingTokenBudget => "thinking_token_budget",
            ThinkingTokenBudgetField::ThinkingBudget => "thinking_budget",
            ThinkingTokenBudgetField::ThinkingBudgetTokens => "thinking_budget_tokens",
        });
    }
    if compat.supports_thinking_token_budget == Some(true) {
        return Some("thinking_token_budget");
    }
    None
}

fn build_chat_template_values(
    model: &Model,
    options: &OpenAICompletionsOptions,
    template: Option<&IndexMap<String, ChatTemplateKwargValue>>,
    thinking_budget: Option<i64>,
) -> Option<Map<String, Value>> {
    let template = template?;
    let is_thinking_on = model.reasoning && options.reasoning_effort.is_some();
    let effort_str = options.reasoning_effort.and_then(|e| resolve_mapped_effort(model, e));

    let mut out = Map::new();
    for (k, v) in template {
        match v {
            ChatTemplateKwargValue::String(s) => {
                out.insert(k.clone(), Value::String(s.clone()));
            }
            ChatTemplateKwargValue::Number(n) => {
                out.insert(k.clone(), Value::Number(n.clone()));
            }
            ChatTemplateKwargValue::Bool(b) => {
                out.insert(k.clone(), Value::Bool(*b));
            }
            ChatTemplateKwargValue::Null => {
                out.insert(k.clone(), Value::Null);
            }
            ChatTemplateKwargValue::Var { var, omit_when_off } => match *var {
                ChatTemplateVarKind::ThinkingEnabled => {
                    if !is_thinking_on && *omit_when_off == Some(true) {
                        continue;
                    }
                    out.insert(k.clone(), Value::Bool(is_thinking_on));
                }
                ChatTemplateVarKind::ThinkingEffort => {
                    if !is_thinking_on {
                        if *omit_when_off == Some(true) {
                            continue;
                        }
                    } else if let Some(ref eff) = effort_str {
                        out.insert(k.clone(), Value::String(eff.clone()));
                    }
                }
                ChatTemplateVarKind::ThinkingBudget => {
                    if is_thinking_on {
                        if let Some(b) = thinking_budget {
                            out.insert(k.clone(), Value::from(b));
                        }
                    }
                }
            },
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn resolve_mapped_effort(model: &Model, effort: OpenAICompletionsReasoningEffort) -> Option<String> {
    if let Some(map) = &model.thinking_level_map {
        if let Some(entry) = map.get(&effort.to_model_thinking_level()) {
            return entry.clone();
        }
    }
    Some(effort.as_str().to_string())
}

fn build_params(
    model: &Model,
    context: &TranscriptContext,
    options: &OpenAICompletionsOptions,
    compat: &ResolvedOpenAICompletionsCompat,
    cache_retention: CacheRetention,
    grammar_props: &IndexMap<String, String>,
) -> Result<Value> {
    let allow_tool_changes = compat.supports_mid_convo_system_messages == Some(true)
        && compat.supports_mid_convo_tool_additions == Some(true);
    let transcript_tools = resolve_transcript_tools(&context.messages, allow_tool_changes);
    let msg_opts = ConvertCompletionsMessagesOptions {
        grammar_tool_input_properties: if grammar_props.is_empty() {
            None
        } else {
            Some(grammar_props.clone())
        },
    };
    let mut messages = convert_messages(model, context, compat, Some(&msg_opts));

    let mut params = Map::new();
    params.insert("model".to_string(), Value::String(model.id.clone()));
    params.insert("stream".to_string(), Value::Bool(true));

    if (model.base_url.contains("api.openai.com") && cache_retention != CacheRetention::None)
        || (cache_retention == CacheRetention::Long && compat.supports_long_cache_retention)
    {
        if let Some(key) = clamp_openai_prompt_cache_key(options.base.session_id.as_deref()) {
            if !key.is_empty() {
                params.insert("prompt_cache_key".to_string(), Value::String(key));
            }
        }
    }
    if cache_retention == CacheRetention::Long && compat.supports_long_cache_retention {
        params.insert("prompt_cache_retention".to_string(), Value::String("24h".to_string()));
    }

    if compat.supports_usage_in_streaming {
        let mut so = Map::new();
        so.insert("include_usage".to_string(), Value::Bool(true));
        params.insert("stream_options".to_string(), Value::Object(so));
    }

    if compat.supports_store {
        params.insert("store".to_string(), Value::Bool(false));
    }

    if let Some(mt) = options.base.max_tokens {
        match compat.max_tokens_field {
            MaxTokensField::MaxTokens => {
                params.insert("max_tokens".to_string(), Value::from(mt));
            }
            MaxTokensField::MaxCompletionTokens => {
                params.insert("max_completion_tokens".to_string(), Value::from(mt));
            }
        }
    }

    if let Some(temp) = options.base.temperature {
        if let Some(num) = serde_json::Number::from_f64(temp) {
            params.insert("temperature".to_string(), Value::Number(num));
        }
    }

    let mut tools_json = if !transcript_tools.request_tools.is_empty() {
        Some(convert_tools(&transcript_tools.request_tools, compat)?)
    } else if has_tool_history(&context.messages) {
        Some(Vec::new())
    } else {
        None
    };

    if compat.cache_control_format == Some(CacheControlFormat::Anthropic) && cache_retention != CacheRetention::None {
        let marker = cache_control_json(cache_retention);
        apply_anthropic_cache_control(&mut messages, tools_json.as_deref_mut(), &marker);
    }

    params.insert("messages".to_string(), Value::Array(messages));
    if let Some(t_arr) = tools_json {
        let is_non_empty = !t_arr.is_empty();
        params.insert("tools".to_string(), Value::Array(t_arr));
        if is_non_empty && compat.zai_tool_stream {
            params.insert("tool_stream".to_string(), Value::Bool(true));
        }
    }

    if let Some(tc) = &options.tool_choice {
        params.insert("tool_choice".to_string(), tc.clone());
    }
    if let Some(prio) = compat.vllm_priority {
        params.insert("priority".to_string(), Value::from(prio));
    }

    let thinking_budget_field = resolve_thinking_token_budget_field(compat);
    let thinking_budget = resolve_clamped_thinking_budget(model, options, options.base.max_tokens);

    if model.reasoning {
        match compat.thinking_format {
            ThinkingFormat::Zai => {
                let mut t_obj = Map::new();
                if options.reasoning_effort.is_some() {
                    t_obj.insert("type".to_string(), Value::String("enabled".to_string()));
                    t_obj.insert("clear_thinking".to_string(), Value::Bool(false));
                } else {
                    t_obj.insert("type".to_string(), Value::String("disabled".to_string()));
                }
                params.insert("thinking".to_string(), Value::Object(t_obj));
                if let Some(eff) = options.reasoning_effort {
                    if compat.supports_reasoning_effort {
                        if let Some(mapped) = resolve_mapped_effort(model, eff) {
                            params.insert("reasoning_effort".to_string(), Value::String(mapped));
                        }
                    }
                }
            }
            ThinkingFormat::Qwen => {
                params.insert(
                    "enable_thinking".to_string(),
                    Value::Bool(options.reasoning_effort.is_some()),
                );
                if let Some(eff) = options.reasoning_effort {
                    if compat.supports_reasoning_effort {
                        if let Some(mapped) = resolve_mapped_effort(model, eff) {
                            params.insert("reasoning_effort".to_string(), Value::String(mapped));
                        }
                    }
                }
            }
            ThinkingFormat::QwenChatTemplate => {
                let mut kw = Map::new();
                kw.insert(
                    "enable_thinking".to_string(),
                    Value::Bool(options.reasoning_effort.is_some()),
                );
                kw.insert("preserve_thinking".to_string(), Value::Bool(true));
                params.insert("chat_template_kwargs".to_string(), Value::Object(kw));
            }
            ThinkingFormat::ChatTemplate => {
                if let Some(kw) =
                    build_chat_template_values(model, options, compat.chat_template_kwargs.as_ref(), thinking_budget)
                {
                    params.insert("chat_template_kwargs".to_string(), Value::Object(kw));
                }
            }
            ThinkingFormat::Baseten => {
                if let Some(args) =
                    build_chat_template_values(model, options, compat.chat_template_args.as_ref(), thinking_budget)
                {
                    params.insert("chat_template_args".to_string(), Value::Object(args));
                }
                if compat.supports_reasoning_effort {
                    let eff_val = if let Some(eff) = options.reasoning_effort {
                        resolve_mapped_effort(model, eff)
                    } else {
                        model
                            .thinking_level_map
                            .as_ref()
                            .and_then(|m| m.get(&ModelThinkingLevel::Off))
                            .and_then(|v| v.clone())
                    };
                    if let Some(s) = eff_val {
                        params.insert("reasoning_effort".to_string(), Value::String(s));
                    }
                }
            }
            ThinkingFormat::DeepSeek => {
                if options.reasoning_effort.is_some() {
                    let mut t = Map::new();
                    t.insert("type".to_string(), Value::String("enabled".to_string()));
                    params.insert("thinking".to_string(), Value::Object(t));
                    if compat.supports_reasoning_effort {
                        if let Some(eff) = options.reasoning_effort {
                            if let Some(s) = resolve_mapped_effort(model, eff) {
                                params.insert("reasoning_effort".to_string(), Value::String(s));
                            }
                        }
                    }
                } else if model
                    .thinking_level_map
                    .as_ref()
                    .and_then(|m| m.get(&ModelThinkingLevel::Off))
                    != Some(&None)
                {
                    let mut t = Map::new();
                    t.insert("type".to_string(), Value::String("disabled".to_string()));
                    params.insert("thinking".to_string(), Value::Object(t));
                }
            }
            ThinkingFormat::OpenRouter => {
                if let Some(eff) = options.reasoning_effort {
                    if let Some(s) = resolve_mapped_effort(model, eff) {
                        let mut r = Map::new();
                        r.insert("effort".to_string(), Value::String(s));
                        params.insert("reasoning".to_string(), Value::Object(r));
                    }
                } else {
                    let off = model
                        .thinking_level_map
                        .as_ref()
                        .and_then(|m| m.get(&ModelThinkingLevel::Off));
                    if off != Some(&None) {
                        let val = off.and_then(|x| x.clone()).unwrap_or_else(|| "none".to_string());
                        let mut r = Map::new();
                        r.insert("effort".to_string(), Value::String(val));
                        params.insert("reasoning".to_string(), Value::Object(r));
                    }
                }
            }
            ThinkingFormat::AntLing => {
                if let Some(eff) = options.reasoning_effort {
                    if let Some(Some(s)) = model
                        .thinking_level_map
                        .as_ref()
                        .and_then(|m| m.get(&eff.to_model_thinking_level()))
                    {
                        let mut r = Map::new();
                        r.insert("effort".to_string(), Value::String(s.clone()));
                        params.insert("reasoning".to_string(), Value::Object(r));
                    }
                }
            }
            ThinkingFormat::Together => {
                let mut r = Map::new();
                r.insert("enabled".to_string(), Value::Bool(options.reasoning_effort.is_some()));
                if let Some(eff) = options.reasoning_effort {
                    if compat.supports_reasoning_effort {
                        if let Some(s) = resolve_mapped_effort(model, eff) {
                            params.insert("reasoning_effort".to_string(), Value::String(s));
                        }
                    }
                }
                params.insert("reasoning".to_string(), Value::Object(r));
            }
            ThinkingFormat::StringThinking => {
                if let Some(eff) = options.reasoning_effort {
                    if let Some(s) = resolve_mapped_effort(model, eff) {
                        params.insert("thinking".to_string(), Value::String(s));
                    }
                } else {
                    let off = model
                        .thinking_level_map
                        .as_ref()
                        .and_then(|m| m.get(&ModelThinkingLevel::Off));
                    if off != Some(&None) {
                        let val = off.and_then(|x| x.clone()).unwrap_or_else(|| "none".to_string());
                        params.insert("thinking".to_string(), Value::String(val));
                    }
                }
            }
            ThinkingFormat::OpenAI => {
                if compat.supports_reasoning_effort {
                    if let Some(eff) = options.reasoning_effort {
                        if let Some(s) = resolve_mapped_effort(model, eff) {
                            params.insert("reasoning_effort".to_string(), Value::String(s));
                        }
                    } else if let Some(Some(off_val)) = model
                        .thinking_level_map
                        .as_ref()
                        .and_then(|m| m.get(&ModelThinkingLevel::Off))
                    {
                        params.insert("reasoning_effort".to_string(), Value::String(off_val.clone()));
                    }
                }
            }
        }
    }

    if let (Some(field), Some(budget)) = (thinking_budget_field, thinking_budget) {
        params.insert(field.to_string(), Value::from(budget));
    }

    if let Some(compat_raw) = model.compat.as_ref().and_then(|c| c.as_openai_completions()) {
        if let Some(or_routing) = &compat_raw.open_router_routing {
            let mut r = Map::new();
            if let Some(only) = &or_routing.only {
                r.insert(
                    "only".to_string(),
                    Value::Array(only.iter().cloned().map(Value::String).collect()),
                );
            }
            if let Some(order) = &or_routing.order {
                r.insert(
                    "order".to_string(),
                    Value::Array(order.iter().cloned().map(Value::String).collect()),
                );
            }
            if !r.is_empty() {
                params.insert("provider".to_string(), Value::Object(r));
            }
        }
        if let Some(vg_routing) = &compat_raw.vercel_gateway_routing {
            let mut gw = Map::new();
            if let Some(only) = &vg_routing.only {
                gw.insert(
                    "only".to_string(),
                    Value::Array(only.iter().cloned().map(Value::String).collect()),
                );
            }
            if let Some(order) = &vg_routing.order {
                gw.insert(
                    "order".to_string(),
                    Value::Array(order.iter().cloned().map(Value::String).collect()),
                );
            }
            if !gw.is_empty() {
                let mut po = Map::new();
                po.insert("gateway".to_string(), Value::Object(gw));
                params.insert("providerOptions".to_string(), Value::Object(po));
            }
        }
    }

    if let Some(model_sp) = &model.sampling_params {
        for (k, v) in model_sp {
            params.insert(k.clone(), v.clone());
        }
    }
    if let Some(base_sp) = &options.base.sampling_params {
        for (k, v) in base_sp {
            params.insert(k.clone(), v.clone());
        }
    }
    if let Some(opt_sp) = &options.sampling_params {
        for (k, v) in opt_sp {
            params.insert(k.clone(), v.clone());
        }
    }

    Ok(Value::Object(params))
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

fn build_headers(
    model: &Model,
    context: &TranscriptContext,
    api_key: &str,
    options_headers: Option<&ProviderHeaders>,
    cache_session_id: Option<&str>,
    compat: &ResolvedOpenAICompletionsCompat,
) -> Headers {
    let mut headers = Headers::new();
    headers.set("accept", "text/event-stream");
    headers.set("content-type", "application/json");
    headers.set("user-agent", &get_pi_user_agent());
    headers.set("authorization", &format!("Bearer {api_key}"));

    if let Some(mh) = &model.headers {
        apply_raw_header_layer(&mut headers, mh);
    }

    if model.provider == "github-copilot" {
        let has_images = copilot_headers_fallback::has_copilot_vision_input(&context.messages);
        let copilot = copilot_headers_fallback::build_copilot_dynamic_headers(&context.messages, has_images);
        apply_raw_header_layer(&mut headers, &copilot);
    }

    if compat.send_session_affinity_headers {
        if let Some(sid) = cache_session_id {
            if !sid.is_empty() {
                match compat.session_affinity_format {
                    SessionAffinityFormat::OpenRouter => {
                        headers.set("x-session-id", sid);
                    }
                    SessionAffinityFormat::OpenAI => {
                        headers.set("session_id", sid);
                        headers.set("x-client-request-id", sid);
                        headers.set("x-session-affinity", sid);
                    }
                    SessionAffinityFormat::OpenAINosession => {
                        headers.set("x-client-request-id", sid);
                        headers.set("x-session-affinity", sid);
                    }
                }
            }
        }
    }

    if let Some(oh) = options_headers {
        apply_header_layer(&mut headers, oh);
    }
    headers
}

fn map_stop_reason(reason: &str) -> (StopReason, Option<String>) {
    match reason {
        "stop" => (StopReason::Stop, None),
        "length" => (StopReason::Length, None),
        "tool_calls" | "function_call" => (StopReason::ToolUse, None),
        "content_filter" => (
            StopReason::Error,
            Some("Provider finish_reason: content_filter".to_string()),
        ),
        other => (StopReason::Error, Some(format!("Provider finish_reason: {other}"))),
    }
}

fn parse_chunk_usage(usage_val: &Value, model: &Model) -> Usage {
    let prompt_tokens = usage_val.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0);
    let completion_tokens = usage_val.get("completion_tokens").and_then(Value::as_i64).unwrap_or(0);
    let total_tokens = usage_val.get("total_tokens").and_then(Value::as_i64).unwrap_or(0);
    let cached_tokens = usage_val
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_i64)
        .or_else(|| usage_val.get("cache_read_tokens").and_then(Value::as_i64))
        .unwrap_or(0);
    let cache_write_tokens = usage_val
        .get("cache_creation_tokens")
        .and_then(Value::as_i64)
        .or_else(|| {
            usage_val
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cache_write_tokens"))
                .and_then(Value::as_i64)
        })
        .unwrap_or(0);

    let input = prompt_tokens
        .saturating_sub(cached_tokens)
        .saturating_sub(cache_write_tokens);
    let total = if total_tokens > 0 {
        total_tokens
    } else {
        input + completion_tokens + cached_tokens + cache_write_tokens
    };
    let mut usage = Usage {
        input,
        output: completion_tokens,
        cache_read: cached_tokens,
        cache_write: cache_write_tokens,
        total_tokens: total,
        ..Usage::default()
    };
    calculate_cost(&AnyModel::Chat(model.clone()), &mut usage);
    usage
}

#[derive(Debug, Clone)]
struct ToolCallScratch {
    block_idx: usize,
    stream_index: Option<usize>,
    partial_args: Option<String>,
    custom_input_prop: Option<String>,
    custom_input_val: String,
    custom_json_buffer: Option<GrammarToolInputJsonBuffer>,
}

fn fill_missing_common_reasoning_detail_fields(last: &mut Map<String, Value>, source: &Map<String, Value>) {
    if !last.contains_key("id") || last.get("id").is_some_and(Value::is_null) {
        if let Some(id) = source.get("id").filter(|v| !v.is_null()) {
            last.insert("id".to_string(), id.clone());
        }
    }
    if last.get("format").and_then(Value::as_str).is_none_or(str::is_empty) {
        if let Some(fmt) = source
            .get("format")
            .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
        {
            last.insert("format".to_string(), fmt.clone());
        }
    }
    if !last.contains_key("index") || last.get("index").is_some_and(Value::is_null) {
        if let Some(idx) = source.get("index").filter(|v| !v.is_null()) {
            last.insert("index".to_string(), idx.clone());
        }
    }
}

fn append_openai_reasoning_detail(details: &mut Vec<Value>, detail: &Value) {
    let Some(detail_obj) = detail.as_object() else {
        return;
    };
    let dtype = detail_obj.get("type").and_then(Value::as_str);
    if !matches!(
        dtype,
        Some("reasoning.summary" | "reasoning.encrypted" | "reasoning.text")
    ) {
        return;
    }
    if let Some(last) = details.last_mut().and_then(Value::as_object_mut) {
        let ltype = last.get("type").and_then(Value::as_str);
        if dtype == Some("reasoning.text") && ltype == Some("reasoning.text") {
            let prev = last.get("text").and_then(Value::as_str).unwrap_or("").to_string();
            let next = detail_obj.get("text").and_then(Value::as_str).unwrap_or("");
            last.insert("text".to_string(), Value::String(format!("{prev}{next}")));
            if last.get("signature").and_then(Value::as_str).is_none_or(str::is_empty) {
                if let Some(sig) = detail_obj
                    .get("signature")
                    .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                {
                    last.insert("signature".to_string(), sig.clone());
                }
            }
            fill_missing_common_reasoning_detail_fields(last, detail_obj);
            return;
        }
        if dtype == Some("reasoning.summary") && ltype == Some("reasoning.summary") {
            let prev = last.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
            let next = detail_obj.get("summary").and_then(Value::as_str).unwrap_or("");
            last.insert("summary".to_string(), Value::String(format!("{prev}{next}")));
            fill_missing_common_reasoning_detail_fields(last, detail_obj);
            return;
        }
    }
    details.push(detail.clone());
}

fn format_status_error(status: u16, body: &str) -> String {
    let trimmed = pi_js::str16::trim(body);
    if trimmed.is_empty() {
        format!("{status} status code (no body)")
    } else {
        format!("{status} {body}")
    }
}

async fn execute_request(
    model: &Model,
    options: &OpenAICompletionsOptions,
    headers: &Headers,
    body_json: &str,
) -> Result<Response> {
    let base = model.base_url.trim_end_matches('/');
    let url = if base.ends_with("/chat/completions") {
        base.to_string()
    } else {
        format!("{base}/chat/completions")
    };
    let fetch_fn = options.base.fetch.clone();
    let timeout_ms = options.base.timeout_ms;
    let signal = options.base.signal.clone();

    retry_provider_request(
        || {
            let url_c = url.clone();
            let headers_c = headers.clone();
            let body_c = body_json.to_string();
            let fetch_c = fetch_fn.clone();
            let sig_c = signal.clone();
            async move {
                let mut req = Request::post(&url_c).with_headers(headers_c).body(Body::Text(body_c));
                let timeout_sig = timeout_ms.map(AbortSignal::timeout);
                let combined = combine_abort_signals([sig_c.as_ref(), timeout_sig.as_ref()]);
                if let Some(sig) = &combined.signal {
                    req = req.signal(sig.clone());
                }
                let resp = if let Some(custom_fetch) = fetch_c {
                    custom_fetch(req).await?
                } else {
                    default_fetch(req).await?
                };
                if !resp.ok() {
                    let status = resp.status();
                    let resp_headers = resp.headers().clone();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(ProviderHttpError::new(
                        Some(status),
                        Some(resp_headers),
                        format_status_error(status, &body),
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
    .await
}

async fn run_completions_stream(
    model: Model,
    context: TranscriptContext,
    options: OpenAICompletionsOptions,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) -> Result<()> {
    let api_key = get_client_api_key(
        &model.provider,
        options.base.api_key.as_deref(),
        options.base.headers.as_ref(),
    )?;
    let compat = get_compat(&model);
    let normalized_context = resolve_transcript(&context, compat.supports_mid_convo_system_messages);
    let declared_tools = get_declared_tools(&normalized_context.messages);
    let grammar_props =
        create_grammar_tool_input_properties(Some(&declared_tools), compat.supports_open_ai_grammar_tools)?;

    let cache_retention = resolve_cache_retention(options.base.cache_retention, options.base.env.as_ref());
    let cache_session_id = if cache_retention == CacheRetention::None {
        None
    } else {
        options.base.session_id.as_deref()
    };

    let headers = build_headers(
        &model,
        &normalized_context,
        &api_key,
        options.base.headers.as_ref(),
        cache_session_id,
        &compat,
    );

    let mut params = build_params(
        &model,
        &normalized_context,
        &options,
        &compat,
        cache_retention,
        &grammar_props,
    )?;

    if let Some(on_payload) = &options.base.on_payload {
        if let Some(next_params) = on_payload(params.clone(), &model).await? {
            params = next_params;
        }
    }

    let body_json = pi_js::json::stringify(&params);
    let response = execute_request(&model, &options, &headers, &body_json).await?;
    if let Some(on_resp) = &options.base.on_response {
        on_resp(
            ProviderResponse {
                status: response.status(),
                headers: headers_to_record(response.headers()),
            },
            &model,
        )
        .await?;
    }

    let mut sse_stream =
        response_to_sse_json_stream(response, model.clone(), options.base.on_provider_stream_event.clone())?;

    stream.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });

    use futures::StreamExt;
    let mut text_block_idx: Option<usize> = None;
    let mut thinking_block_idx: Option<usize> = None;
    let mut streamed_reasoning_details: Option<Vec<Value>> = None;
    let mut has_finish_reason = false;
    let mut tc_scratches: Vec<ToolCallScratch> = Vec::new();
    let mut by_stream_index: IndexMap<usize, usize> = IndexMap::new();
    let mut by_call_id: IndexMap<String, usize> = IndexMap::new();

    while let Some(chunk_res) = sse_stream.next().await {
        let chunk = chunk_res?;
        if output.response_id.is_none() {
            if let Some(id) = chunk.get("id").and_then(Value::as_str) {
                if !id.is_empty() {
                    output.response_id = Some(id.to_string());
                }
            }
        }
        if output.response_model.is_none() {
            if let Some(cm) = chunk.get("model").and_then(Value::as_str) {
                if !cm.is_empty() && cm != model.id {
                    output.response_model = Some(cm.to_string());
                }
            }
        }
        if let Some(usage_val) = chunk.get("usage").filter(|v| v.is_object()) {
            output.usage = parse_chunk_usage(usage_val, &model);
        }

        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
        else {
            continue;
        };

        if !chunk.get("usage").is_some_and(Value::is_object) {
            if let Some(c_usage) = choice.get("usage").filter(|v| v.is_object()) {
                output.usage = parse_chunk_usage(c_usage, &model);
            }
        }

        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
            if !fr.is_empty() {
                output.raw_stop_reason = Some(fr.to_string());
                let (sr, err_msg) = map_stop_reason(fr);
                output.stop_reason = sr;
                if let Some(msg) = err_msg {
                    output.error_message = Some(msg);
                }
                has_finish_reason = true;
            }
        }

        let Some(delta) = choice.get("delta") else {
            continue;
        };

        if let Some(content_delta) = delta.get("content").and_then(Value::as_str) {
            if !content_delta.is_empty() {
                let idx = if let Some(i) = text_block_idx {
                    i
                } else {
                    let i = output.content.len();
                    output.content.push(AssistantContentBlock::Text(TextContent {
                        text: String::new(),
                        text_signature: None,
                    }));
                    text_block_idx = Some(i);
                    stream.push(AssistantMessageEvent::TextStart {
                        content_index: i,
                        partial: output.clone(),
                    });
                    i
                };
                if let Some(AssistantContentBlock::Text(t)) = output.content.get_mut(idx) {
                    t.text.push_str(content_delta);
                }
                stream.push(AssistantMessageEvent::TextDelta {
                    content_index: idx,
                    delta: content_delta.to_string(),
                    partial: output.clone(),
                });
            }
        }

        let mut found_field: Option<&str> = None;
        for field in ["reasoning_content", "reasoning", "reasoning_text"] {
            if delta.get(field).and_then(Value::as_str).is_some_and(|s| !s.is_empty()) {
                found_field = Some(field);
                break;
            }
        }
        if let Some(field) = found_field {
            if let Some(r_delta) = delta.get(field).and_then(Value::as_str) {
                let sig = if model.provider == "opencode-go" && field == "reasoning" {
                    "reasoning_content"
                } else {
                    field
                };
                let idx = if let Some(i) = thinking_block_idx {
                    i
                } else {
                    let i = output.content.len();
                    output.content.push(AssistantContentBlock::Thinking(ThinkingContent {
                        thinking: String::new(),
                        thinking_signature: Some(sig.to_string()),
                        redacted: None,
                    }));
                    thinking_block_idx = Some(i);
                    stream.push(AssistantMessageEvent::ThinkingStart {
                        content_index: i,
                        partial: output.clone(),
                    });
                    i
                };
                if let Some(AssistantContentBlock::Thinking(t)) = output.content.get_mut(idx) {
                    t.thinking.push_str(r_delta);
                }
                stream.push(AssistantMessageEvent::ThinkingDelta {
                    content_index: idx,
                    delta: r_delta.to_string(),
                    partial: output.clone(),
                });
            }
        }

        if let Some(tc_arr) = delta.get("tool_calls").and_then(Value::as_array) {
            for tc_delta in tc_arr {
                let s_idx = tc_delta.get("index").and_then(Value::as_u64).map(|u| u as usize);
                let tc_id = tc_delta.get("id").and_then(Value::as_str);
                let fn_name = tc_delta
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        tc_delta
                            .get("custom")
                            .and_then(|c| c.get("name"))
                            .and_then(Value::as_str)
                    })
                    .unwrap_or("");

                let scratch_idx_opt = s_idx
                    .and_then(|si| by_stream_index.get(&si).copied())
                    .or_else(|| tc_id.and_then(|id| by_call_id.get(id).copied()));

                let scratch_idx = if let Some(si) = scratch_idx_opt {
                    si
                } else {
                    let is_custom = tc_delta.get("custom").is_some() && tc_delta.get("function").is_none();
                    let custom_prop = if is_custom {
                        Some(
                            grammar_props
                                .get(fn_name)
                                .cloned()
                                .unwrap_or_else(|| "input".to_string()),
                        )
                    } else {
                        None
                    };
                    let block_idx = output.content.len();
                    let mut init_args = IndexMap::new();
                    if let Some(ref prop) = custom_prop {
                        init_args.insert(prop.clone(), Value::String(String::new()));
                    }
                    output.content.push(AssistantContentBlock::ToolCall(ToolCall {
                        id: tc_id.unwrap_or("").to_string(),
                        name: fn_name.to_string(),
                        arguments: init_args,
                        thought_signature: None,
                        namespace: None,
                    }));
                    let new_si = tc_scratches.len();
                    tc_scratches.push(ToolCallScratch {
                        block_idx,
                        stream_index: s_idx,
                        partial_args: if custom_prop.is_none() {
                            Some(String::new())
                        } else {
                            None
                        },
                        custom_json_buffer: if custom_prop.is_some() {
                            Some(GrammarToolInputJsonBuffer {
                                input: String::new(),
                                started: false,
                                closed: false,
                            })
                        } else {
                            None
                        },
                        custom_input_prop: custom_prop,
                        custom_input_val: String::new(),
                    });
                    if let Some(i) = s_idx {
                        by_stream_index.insert(i, new_si);
                    }
                    if let Some(id) = tc_id {
                        if !id.is_empty() {
                            by_call_id.insert(id.to_string(), new_si);
                        }
                    }
                    stream.push(AssistantMessageEvent::ToolCallStart {
                        content_index: block_idx,
                        partial: output.clone(),
                    });
                    new_si
                };

                let block_idx = tc_scratches[scratch_idx].block_idx;
                if let Some(AssistantContentBlock::ToolCall(tc)) = output.content.get_mut(block_idx) {
                    if tc.id.is_empty() {
                        if let Some(id) = tc_id {
                            if !id.is_empty() {
                                tc.id = id.to_string();
                                by_call_id.insert(id.to_string(), scratch_idx);
                            }
                        }
                    }
                    if tc.name.is_empty() && !fn_name.is_empty() {
                        tc.name = fn_name.to_string();
                    }
                }
                if let Some(i) = s_idx {
                    if tc_scratches[scratch_idx].stream_index.is_none() {
                        tc_scratches[scratch_idx].stream_index = Some(i);
                        by_stream_index.insert(i, scratch_idx);
                    }
                }

                let mut emitted_delta = String::new();
                if let Some(arg_delta) = tc_delta
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                {
                    emitted_delta = arg_delta.to_string();
                    let next_partial = {
                        let s = &mut tc_scratches[scratch_idx];
                        let buf = s.partial_args.get_or_insert_with(String::new);
                        buf.push_str(arg_delta);
                        buf.clone()
                    };
                    let parsed = parse_streaming_json(Some(&next_partial));
                    if let Some(AssistantContentBlock::ToolCall(tc)) = output.content.get_mut(block_idx) {
                        tc.arguments = parsed;
                    }
                } else if let Some(custom_delta) = tc_delta
                    .get("custom")
                    .and_then(|c| c.get("input"))
                    .and_then(Value::as_str)
                {
                    let scratch = &mut tc_scratches[scratch_idx];
                    if let (Some(prop), Some(jbuf)) =
                        (scratch.custom_input_prop.clone(), scratch.custom_json_buffer.as_mut())
                    {
                        scratch.custom_input_val.push_str(custom_delta);
                        emitted_delta =
                            append_grammar_tool_input_json_delta(jbuf, &prop, &scratch.custom_input_val, false)?
                                .unwrap_or_default();
                        if let Some(AssistantContentBlock::ToolCall(tc)) = output.content.get_mut(block_idx) {
                            let mut map = IndexMap::new();
                            map.insert(prop, Value::String(scratch.custom_input_val.clone()));
                            tc.arguments = map;
                        }
                    }
                }

                stream.push(AssistantMessageEvent::ToolCallDelta {
                    content_index: block_idx,
                    delta: emitted_delta,
                    partial: output.clone(),
                });
            }
        }

        if let Some(details) = delta.get("reasoning_details").and_then(Value::as_array) {
            for d in details {
                let idx = if let Some(i) = thinking_block_idx {
                    i
                } else {
                    let i = output.content.len();
                    output.content.push(AssistantContentBlock::Thinking(ThinkingContent {
                        thinking: String::new(),
                        thinking_signature: Some(String::new()),
                        redacted: None,
                    }));
                    thinking_block_idx = Some(i);
                    stream.push(AssistantMessageEvent::ThinkingStart {
                        content_index: i,
                        partial: output.clone(),
                    });
                    i
                };
                let _ = idx;
                let vec_ref = streamed_reasoning_details.get_or_insert_with(Vec::new);
                append_openai_reasoning_detail(vec_ref, d);
            }
        }
    }

    if let Some(idx) = thinking_block_idx {
        if let Some(ref details) = streamed_reasoning_details {
            if let Some(AssistantContentBlock::Thinking(t)) = output.content.get_mut(idx) {
                t.thinking_signature = Some(pi_js::json::stringify(&Value::Array(details.clone())));
            }
        }
        let text = match output.content.get(idx) {
            Some(AssistantContentBlock::Thinking(t)) => t.thinking.clone(),
            _ => String::new(),
        };
        stream.push(AssistantMessageEvent::ThinkingEnd {
            content_index: idx,
            content: text,
            partial: output.clone(),
        });
    }
    if let Some(idx) = text_block_idx {
        let text = match output.content.get(idx) {
            Some(AssistantContentBlock::Text(t)) => t.text.clone(),
            _ => String::new(),
        };
        stream.push(AssistantMessageEvent::TextEnd {
            content_index: idx,
            content: text,
            partial: output.clone(),
        });
    }
    for mut scratch in tc_scratches {
        if let (Some(prop), Some(ref mut jbuf)) =
            (scratch.custom_input_prop.take(), scratch.custom_json_buffer.as_mut())
        {
            if let Some(delta) = append_grammar_tool_input_json_delta(jbuf, &prop, &scratch.custom_input_val, true)? {
                stream.push(AssistantMessageEvent::ToolCallDelta {
                    content_index: scratch.block_idx,
                    delta,
                    partial: output.clone(),
                });
            }
        } else if let Some(ref pa) = scratch.partial_args {
            let parsed = parse_streaming_json(Some(pa));
            if let Some(AssistantContentBlock::ToolCall(tc)) = output.content.get_mut(scratch.block_idx) {
                tc.arguments = parsed;
            }
        }
        if let Some(AssistantContentBlock::ToolCall(tc)) = output.content.get(scratch.block_idx) {
            stream.push(AssistantMessageEvent::ToolCallEnd {
                content_index: scratch.block_idx,
                tool_call: tc.clone(),
                partial: output.clone(),
            });
        }
    }

    if options.base.signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Err(Error::msg("Request was aborted"));
    }
    if output.stop_reason == StopReason::Aborted {
        return Err(Error::msg("Request was aborted"));
    }
    if !has_finish_reason && !compat.supports_finish_reason {
        let has_tc = output
            .content
            .iter()
            .any(|b| matches!(b, AssistantContentBlock::ToolCall(_)));
        output.stop_reason = if has_tc { StopReason::ToolUse } else { StopReason::Stop };
    }
    if output.stop_reason == StopReason::Error {
        return Err(Error::msg(
            output
                .error_message
                .clone()
                .unwrap_or_else(|| "Provider returned an error stop reason".to_string()),
        ));
    }
    if (compat.supports_finish_reason && !has_finish_reason) || output.stop_reason == StopReason::Pending {
        return Err(Error::msg("Stream ended without finish_reason"));
    }

    Ok(())
}

/// Stream responses using OpenAI Chat Completions API (`openai-completions`).
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: Option<ApiStreamOptions>,
) -> AssistantMessageEventStream {
    let model_owned = model.clone();
    let context_owned = context.clone();
    let typed_options = OpenAICompletionsOptions::from_api_options(options);
    let stream = create_assistant_message_event_stream();
    let stream_handle = stream.clone();

    tokio::spawn(async move {
        let mut output = create_pending_assistant_message(&model_owned);
        match run_completions_stream(
            model_owned.clone(),
            context_owned,
            typed_options.clone(),
            &mut output,
            &stream_handle,
        )
        .await
        {
            Ok(()) => {
                let reason = output.stop_reason;
                stream_handle.push(AssistantMessageEvent::Done {
                    reason,
                    message: output,
                });
            }
            Err(err) => {
                let aborted = typed_options.base.signal.as_ref().is_some_and(AbortSignal::aborted);
                output.stop_reason = if aborted {
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

/// Simplified streaming wrapper for OpenAI Chat Completions API.
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
        ModelThinkingLevel::Minimal => Some(OpenAICompletionsReasoningEffort::Minimal),
        ModelThinkingLevel::Low => Some(OpenAICompletionsReasoningEffort::Low),
        ModelThinkingLevel::Medium => Some(OpenAICompletionsReasoningEffort::Medium),
        ModelThinkingLevel::High => Some(OpenAICompletionsReasoningEffort::High),
        ModelThinkingLevel::XHigh => Some(OpenAICompletionsReasoningEffort::XHigh),
        ModelThinkingLevel::Max => Some(OpenAICompletionsReasoningEffort::Max),
    });
    let tool_choice = options
        .as_ref()
        .and_then(|o| o.tool_choice.as_ref())
        .map(|tc| match tc {
            ToolChoice::Auto => Value::String("auto".to_string()),
            ToolChoice::None => Value::String("none".to_string()),
        });

    let typed = OpenAICompletionsOptions {
        base,
        tool_choice,
        reasoning_effort,
        thinking_budgets: options.as_ref().and_then(|o| o.thinking_budgets.clone()),
        sampling_params: None,
    };
    stream(model, context, Some(typed.into()))
}

/// Construct `ProviderStreams` descriptor for OpenAI Chat Completions API.
pub fn provider_streams() -> ProviderStreams {
    ProviderStreams {
        stream: Arc::new(|model, context, options| stream(model, context, options)),
        stream_simple: Arc::new(|model, context, options| stream_simple(model, context, options)),
        fetch_deferred: None,
        cancel_deferred: None,
    }
}
