//! Port of packages/ai/src/api/openai-responses.ts

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};
use pi_js::abort::AbortSignal;
use pi_js::error::{Error, Result};
use pi_js::fetch::{Body, Headers, Request, Response, fetch as default_fetch};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::api::constrained_sampling::create_grammar_tool_input_properties;
use crate::api::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::api::openai_responses_shared::{
    ConvertResponsesMessagesOptions, ConvertResponsesToolsOptions, ProcessResponsesStreamOptions,
    convert_responses_messages, convert_responses_tools, create_pending_assistant_message,
    enrich_chatgpt_usage_limit_error, process_responses_stream, resolve_responses_cache_retention,
    response_to_sse_json_stream,
};
use crate::api::simple_options::{build_base_options, clamp_reasoning};
use crate::env_api_keys::get_env_api_key;
use crate::types::{
    ApiStreamOptions, CacheRetention, Model, ModelThinkingLevel, ProviderEnv, ProviderHeaders, ProviderStreams,
    SessionAffinityFormat, SimpleStreamOptions, StreamOptions, ThinkingLevel, ToolChoice, TranscriptContext,
};
use crate::utils::abort_signals::combine_abort_signals;
use crate::utils::diagnostics::{append_assistant_message_diagnostic, create_assistant_message_diagnostic};
use crate::utils::error_body::{format_provider_error, normalize_provider_error, truncate_error_text};
use crate::utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use crate::utils::provider_retry::{ProviderHttpError, ProviderRetryOptions, retry_provider_request};
use crate::utils::transcript::{get_declared_tools, resolve_transcript, resolve_transcript_tools};

/// OpenAI Responses reasoning effort values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenAIReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
}

impl OpenAIReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
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
        }
    }
}

/// Options for OpenAI Responses API requests.
#[derive(Debug, Clone, Default)]
pub struct OpenAIResponsesOptions {
    pub base: StreamOptions,
    pub reasoning_effort: Option<OpenAIReasoningEffort>,
    pub reasoning_summary: Option<String>,
    pub service_tier: Option<String>,
    pub tool_choice: Option<Value>,
}

impl Deref for OpenAIResponsesOptions {
    type Target = StreamOptions;
    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for OpenAIResponsesOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl From<&OpenAIResponsesOptions> for ApiStreamOptions {
    fn from(options: &OpenAIResponsesOptions) -> Self {
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
        if let Some(tc) = &options.tool_choice {
            extra.insert("toolChoice".to_string(), tc.clone());
        }
        let extension: Arc<dyn std::any::Any + Send + Sync> = Arc::new(options.clone());
        Self {
            base: options.base.clone(),
            extra,
            extension: Some(extension),
        }
    }
}

impl From<OpenAIResponsesOptions> for ApiStreamOptions {
    fn from(options: OpenAIResponsesOptions) -> Self {
        Self::from(&options)
    }
}

impl OpenAIResponsesOptions {
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
            .and_then(OpenAIReasoningEffort::from_str_opt);
        let reasoning_summary = api_opts
            .extra
            .get("reasoningSummary")
            .and_then(Value::as_str)
            .map(str::to_string);
        let service_tier = api_opts
            .extra
            .get("serviceTier")
            .and_then(Value::as_str)
            .map(str::to_string);
        let tool_choice = api_opts.extra.get("toolChoice").cloned();
        Self {
            base: api_opts.base,
            reasoning_effort,
            reasoning_summary,
            service_tier,
            tool_choice,
        }
    }
}

use crate::api::github_copilot_headers::{build_copilot_dynamic_headers, has_copilot_vision_input};

#[derive(Debug, Clone)]
struct ResolvedOpenAIResponsesCompat {
    supports_strict_mode: bool,
    supports_max_output_tokens: bool,
    supports_mid_convo_system_messages: Option<bool>,
    supports_openai_grammar_tools: bool,
    supports_additional_tools: bool,
    supports_tool_search: bool,
    supports_long_cache_retention: bool,
    supports_explicit_prompt_cache_mode: bool,
    session_affinity_format: Option<SessionAffinityFormat>,
}

fn resolve_compat(model: &Model) -> ResolvedOpenAIResponsesCompat {
    let rc = model.compat.as_ref().and_then(|c| c.as_openai_responses());
    let is_openrouter = model.provider == "openrouter" || model.base_url.to_ascii_lowercase().contains("openrouter.ai");
    let default_affinity = if is_openrouter {
        Some(SessionAffinityFormat::OpenRouter)
    } else {
        Some(SessionAffinityFormat::OpenAI)
    };

    ResolvedOpenAIResponsesCompat {
        supports_strict_mode: rc.and_then(|c| c.supports_strict_mode).unwrap_or(true),
        supports_max_output_tokens: rc.and_then(|c| c.supports_max_output_tokens).unwrap_or(true),
        supports_mid_convo_system_messages: rc.and_then(|c| c.supports_mid_convo_system_messages),
        supports_openai_grammar_tools: rc.and_then(|c| c.supports_open_ai_grammar_tools).unwrap_or(false),
        supports_additional_tools: rc.and_then(|c| c.supports_additional_tools).unwrap_or(false),
        supports_tool_search: rc.and_then(|c| c.supports_tool_search).unwrap_or(false),
        supports_long_cache_retention: rc.and_then(|c| c.supports_long_cache_retention).unwrap_or(true),
        supports_explicit_prompt_cache_mode: rc.and_then(|c| c.supports_explicit_prompt_cache_mode).unwrap_or(false),
        session_affinity_format: rc.and_then(|c| c.session_affinity_format).or(default_affinity),
    }
}

fn allowed_tool_call_providers() -> IndexSet<String> {
    let mut set = IndexSet::new();
    set.insert("openai".to_string());
    set.insert("openai-codex".to_string());
    set.insert("opencode".to_string());
    set
}

fn resolve_api_key(model: &Model, options: &OpenAIResponsesOptions) -> Result<String> {
    if let Some(key) = &options.base.api_key {
        if !key.is_empty() {
            return Ok(key.clone());
        }
    }
    let env_ref: Option<&ProviderEnv> = options.base.env.as_ref();
    if let Some(key) = get_env_api_key(&model.provider, env_ref) {
        if !key.is_empty() {
            return Ok(key);
        }
    }
    Err(Error::msg(format!("No API key for provider: {}", model.provider)))
}

fn tool_choice_to_value(tc: &ToolChoice) -> Value {
    match tc {
        ToolChoice::Auto => Value::String("auto".to_string()),
        ToolChoice::None => Value::String("none".to_string()),
    }
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
    api_key: &str,
    compat: &ResolvedOpenAIResponsesCompat,
    cache_retention: CacheRetention,
    session_id: Option<&str>,
    copilot_dynamic: Option<&IndexMap<String, String>>,
    options_headers: Option<&ProviderHeaders>,
) -> Headers {
    let mut headers = Headers::new();
    headers.set("accept", "text/event-stream");
    headers.set("content-type", "application/json");
    headers.set("authorization", &format!("Bearer {api_key}"));

    if let Some(mh) = &model.headers {
        apply_raw_header_layer(&mut headers, mh);
    }

    if cache_retention != CacheRetention::None {
        if let Some(sid) = session_id {
            if !sid.is_empty() {
                if let Some(fmt) = compat.session_affinity_format {
                    match fmt {
                        SessionAffinityFormat::OpenAI => {
                            headers.set("session_id", sid);
                            headers.set("x-client-request-id", sid);
                        }
                        SessionAffinityFormat::OpenAINosession => {
                            headers.set("x-client-request-id", sid);
                        }
                        SessionAffinityFormat::OpenRouter => {
                            headers.set("x-session-id", sid);
                        }
                    }
                } else {
                    headers.set("session_id", sid);
                    headers.set("x-client-request-id", sid);
                }
            }
        }
    }

    if let Some(dyn_h) = copilot_dynamic {
        apply_raw_header_layer(&mut headers, dyn_h);
    }
    if let Some(oh) = options_headers {
        apply_header_layer(&mut headers, oh);
    }
    headers
}

fn build_params(
    model: &Model,
    context: &TranscriptContext,
    options: &OpenAIResponsesOptions,
    compat: &ResolvedOpenAIResponsesCompat,
    cache_retention: CacheRetention,
) -> Result<(Value, IndexMap<String, String>)> {
    let providers = allowed_tool_call_providers();
    let declared_tools = get_declared_tools(&context.messages);
    let transcript_tools = resolve_transcript_tools(
        &context.messages,
        compat.supports_additional_tools || compat.supports_tool_search,
    );
    let grammar_props =
        create_grammar_tool_input_properties(Some(&declared_tools), compat.supports_openai_grammar_tools)?;

    let tool_opts = ConvertResponsesToolsOptions {
        strict: Some(compat.supports_strict_mode),
        supports_openai_grammar_tools: Some(compat.supports_openai_grammar_tools),
        ..Default::default()
    };

    let msg_opts = ConvertResponsesMessagesOptions {
        include_system_prompt: Some(true),
        grammar_tool_input_properties: if grammar_props.is_empty() {
            None
        } else {
            Some(grammar_props.clone())
        },
        supports_mid_convo_system_messages: compat.supports_mid_convo_system_messages,
        supports_additional_tools: compat.supports_additional_tools,
        supports_tool_search: compat.supports_tool_search,
        tool_options: Some(tool_opts.clone()),
    };
    let messages = convert_responses_messages(model, context, &providers, Some(&msg_opts));

    let mut params = Map::new();
    params.insert("model".to_string(), Value::String(model.id.clone()));
    params.insert("input".to_string(), Value::Array(messages));
    params.insert("stream".to_string(), Value::Bool(true));

    if model.provider != "openai-codex" {
        params.insert("store".to_string(), Value::Bool(false));
    }

    let omit_unsupported_fields = model.provider == "openai"
        && model.base_url == "https://api.openai.com/v1"
        && options
            .base
            .api_key
            .as_deref()
            .is_some_and(|key| !key.starts_with("sk-"));

    if cache_retention != CacheRetention::None {
        if let Some(cache_key) = clamp_openai_prompt_cache_key(options.base.session_id.as_deref()) {
            if !cache_key.is_empty() {
                params.insert("prompt_cache_key".to_string(), Value::String(cache_key));
            }
        }
    }
    if !omit_unsupported_fields {
        if cache_retention == CacheRetention::Long
            && compat.supports_long_cache_retention
            && !compat.supports_explicit_prompt_cache_mode
        {
            params.insert("prompt_cache_retention".to_string(), Value::String("24h".to_string()));
        }
        if compat.supports_explicit_prompt_cache_mode {
            if cache_retention == CacheRetention::None {
                let mut opts_map = Map::new();
                opts_map.insert("mode".to_string(), Value::String("explicit".to_string()));
                params.insert("prompt_cache_options".to_string(), Value::Object(opts_map));
            } else if cache_retention == CacheRetention::Long && compat.supports_long_cache_retention {
                let mut opts_map = Map::new();
                opts_map.insert("ttl".to_string(), Value::String("30m".to_string()));
                params.insert("prompt_cache_options".to_string(), Value::Object(opts_map));
            }
        }

        if compat.supports_max_output_tokens {
            if let Some(max_tokens) = options.base.max_tokens {
                params.insert("max_output_tokens".to_string(), Value::from(max_tokens));
            }
        }
        if let Some(temp) = options.base.temperature {
            if let Some(num) = serde_json::Number::from_f64(temp) {
                params.insert("temperature".to_string(), Value::Number(num));
            }
        }
    }
    if let Some(tier) = &options.service_tier {
        params.insert("service_tier".to_string(), Value::String(tier.clone()));
    }

    if !transcript_tools.request_tools.is_empty() {
        let converted_tools = convert_responses_tools(&transcript_tools.request_tools, Some(&tool_opts))?;
        params.insert("tools".to_string(), Value::Array(converted_tools));
    }

    if let Some(tc_val) = &options.tool_choice {
        params.insert("tool_choice".to_string(), tc_val.clone());
    }

    if model.reasoning {
        let resolved_effort = resolve_effort_for_model(model, options.reasoning_effort);
        let summary_opt = options.reasoning_summary.as_deref().filter(|s| *s != "off");

        if resolved_effort.is_some() || summary_opt.is_some() {
            let mut reasoning_obj = Map::new();
            if let Some(ref effort_str) = resolved_effort {
                reasoning_obj.insert("effort".to_string(), Value::String(effort_str.clone()));
            }
            if let Some(summary) = summary_opt {
                reasoning_obj.insert("summary".to_string(), Value::String(summary.to_string()));
            }
            params.insert("reasoning".to_string(), Value::Object(reasoning_obj));
        }

        if resolved_effort.as_deref().is_some_and(|e| e != "none") {
            params.insert(
                "include".to_string(),
                Value::Array(vec![Value::String("reasoning.encrypted_content".to_string())]),
            );
        }
    }

    if let Some(model_sp) = &model.sampling_params {
        for (k, v) in model_sp {
            params.insert(k.clone(), v.clone());
        }
    }
    if let Some(opt_sp) = &options.base.sampling_params {
        for (k, v) in opt_sp {
            params.insert(k.clone(), v.clone());
        }
    }

    Ok((Value::Object(params), grammar_props))
}

fn resolve_effort_for_model(model: &Model, requested: Option<OpenAIReasoningEffort>) -> Option<String> {
    if let Some(effort) = requested {
        if let Some(entry) = model
            .thinking_level_map
            .as_ref()
            .and_then(|m| m.get(&effort.to_model_thinking_level()))
        {
            return entry.clone();
        }
        return Some(effort.as_str().to_string());
    }
    model
        .thinking_level_map
        .as_ref()
        .and_then(|m| m.get(&ModelThinkingLevel::Off))
        .and_then(|v| v.clone())
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
    options: &OpenAIResponsesOptions,
    headers: &Headers,
    body_json: &str,
) -> Result<Response> {
    let url = format!("{}/responses", model.base_url.trim_end_matches('/'));
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

async fn run_openai_responses_stream(
    model: Model,
    context: TranscriptContext,
    options: OpenAIResponsesOptions,
    output: &mut crate::types::AssistantMessage,
    event_stream: &AssistantMessageEventStream,
) -> Result<()> {
    let api_key = resolve_api_key(&model, &options)?;
    let compat = resolve_compat(&model);
    let cache_retention = resolve_responses_cache_retention(options.base.cache_retention, options.base.env.as_ref());

    let copilot_dynamic = if model.provider == "github-copilot" {
        let has_images = has_copilot_vision_input(&context.messages);
        Some(build_copilot_dynamic_headers(&context.messages, has_images))
    } else {
        None
    };

    let headers = build_headers(
        &model,
        &api_key,
        &compat,
        cache_retention,
        options.base.session_id.as_deref(),
        copilot_dynamic.as_ref(),
        options.base.headers.as_ref(),
    );

    let resolved_context = resolve_transcript(&context, compat.supports_mid_convo_system_messages);

    let (mut params, grammar_props) = build_params(&model, &resolved_context, &options, &compat, cache_retention)?;

    if let Some(on_payload) = &options.base.on_payload {
        if let Some(next_params) = on_payload(params.clone(), &model).await? {
            let mut next_obj = next_params.as_object().cloned().unwrap_or_default();
            next_obj.insert("stream".to_string(), Value::Bool(true));
            params = Value::Object(next_obj);
        }
    }

    let body_json = pi_js::json::stringify(&params);
    let response = execute_request(&model, &options, &headers, &body_json).await?;
    let event_json_stream =
        response_to_sse_json_stream(response, model.clone(), options.base.on_provider_stream_event.clone())?;

    event_stream.push(crate::types::AssistantMessageEvent::Start {
        partial: output.clone(),
    });

    let process_opts = ProcessResponsesStreamOptions {
        service_tier: options.service_tier.clone(),
        resolve_service_tier_on_default: false,
        grammar_tool_input_properties: if grammar_props.is_empty() {
            None
        } else {
            Some(grammar_props)
        },
    };
    process_responses_stream(event_json_stream, output, event_stream, &model, Some(&process_opts)).await
}

fn format_openai_error(err: &Error, output: &mut crate::types::AssistantMessage) -> String {
    let norm = normalize_provider_error(err);
    let prefix = if output.provider.as_str() == "openai" {
        "OpenAI API error".to_string()
    } else {
        format!("{} API error", output.provider)
    };
    let formatted = format_provider_error(&norm, Some(&prefix));
    let enriched = enrich_chatgpt_usage_limit_error(&formatted);
    let truncated = truncate_error_text(&enriched, 4000);
    append_assistant_message_diagnostic(output, create_assistant_message_diagnostic("provider_error", err, None));
    truncated
}

/// Stream responses using OpenAI Responses API (`openai-responses`).
pub fn stream_openai_responses(
    model: &Model,
    context: &TranscriptContext,
    options: Option<ApiStreamOptions>,
) -> AssistantMessageEventStream {
    let model_owned = model.clone();
    let context_owned = context.clone();
    let typed_options = OpenAIResponsesOptions::from_api_options(options);
    let stream = create_assistant_message_event_stream();
    let stream_handle = stream.clone();

    tokio::spawn(async move {
        let mut output = create_pending_assistant_message(&model_owned);
        match run_openai_responses_stream(
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
                if reason == crate::types::StopReason::Error || reason == crate::types::StopReason::Aborted {
                    stream_handle.push(crate::types::AssistantMessageEvent::Error { reason, error: output });
                } else {
                    stream_handle.push(crate::types::AssistantMessageEvent::Done {
                        reason,
                        message: output,
                    });
                }
            }
            Err(err) => {
                let aborted = typed_options.base.signal.as_ref().is_some_and(AbortSignal::aborted);
                output.stop_reason = if aborted {
                    crate::types::StopReason::Aborted
                } else {
                    crate::types::StopReason::Error
                };
                output.error_message = Some(format_openai_error(&err, &mut output));
                let reason = output.stop_reason;
                stream_handle.push(crate::types::AssistantMessageEvent::Error { reason, error: output });
            }
        }
        stream_handle.end(None);
    });

    stream
}

pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: Option<ApiStreamOptions>,
) -> AssistantMessageEventStream {
    stream_openai_responses(model, context, options)
}

/// Simplified streaming wrapper for OpenAI Responses API.
pub fn stream_simple_openai_responses(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let api_key = options.as_ref().and_then(|o| o.base.api_key.as_deref());
    let base = build_base_options(model, context, options.as_ref(), api_key);
    let reasoning_effort = if model.reasoning {
        options
            .as_ref()
            .and_then(|o| clamp_reasoning(o.reasoning))
            .map(|lvl| match lvl {
                ThinkingLevel::Minimal => OpenAIReasoningEffort::Minimal,
                ThinkingLevel::Low => OpenAIReasoningEffort::Low,
                ThinkingLevel::Medium => OpenAIReasoningEffort::Medium,
                ThinkingLevel::High => OpenAIReasoningEffort::High,
                ThinkingLevel::XHigh | ThinkingLevel::Max => OpenAIReasoningEffort::XHigh,
            })
    } else {
        None
    };

    let tool_choice = options
        .as_ref()
        .and_then(|o| o.tool_choice.as_ref())
        .map(tool_choice_to_value);

    let typed = OpenAIResponsesOptions {
        base,
        reasoning_effort,
        reasoning_summary: None,
        service_tier: None,
        tool_choice,
    };
    stream_openai_responses(model, context, Some(typed.into()))
}

pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    stream_simple_openai_responses(model, context, options)
}

/// Construct `ProviderStreams` descriptor for OpenAI Responses API.
pub fn provider_streams() -> ProviderStreams {
    ProviderStreams {
        stream: Arc::new(|model, context, options| stream(model, context, options)),
        stream_simple: Arc::new(|model, context, options| stream_simple(model, context, options)),
        fetch_deferred: None,
        cancel_deferred: None,
    }
}

/// Alias matching `openai_responses_api()` naming convention.
pub fn openai_responses_api() -> ProviderStreams {
    provider_streams()
}
