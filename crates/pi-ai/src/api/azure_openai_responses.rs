//! Port of packages/ai/src/api/azure-openai-responses.ts

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};
use pi_js::abort::AbortSignal;
use pi_js::error::{Error, Result};
use pi_js::fetch::{Body, Headers, Request, Response, fetch as default_fetch};
use serde_json::{Map, Value};
use url::Url;

use crate::api::constrained_sampling::create_grammar_tool_input_properties;
use crate::api::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::api::openai_responses::OpenAIReasoningEffort;
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
    SimpleStreamOptions, StreamOptions, ThinkingLevel, ToolChoice, TranscriptContext,
};
use crate::utils::abort_signals::combine_abort_signals;
use crate::utils::diagnostics::{append_assistant_message_diagnostic, create_assistant_message_diagnostic};
use crate::utils::error_body::{format_provider_error, normalize_provider_error, truncate_error_text};
use crate::utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use crate::utils::pi_user_agent::get_pi_user_agent;
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{ProviderHttpError, ProviderRetryOptions, retry_provider_request};
use crate::utils::transcript::{get_declared_tools, resolve_transcript};

pub const DEFAULT_AZURE_API_VERSION: &str = "v1";

/// Options for Azure OpenAI Responses API requests.
#[derive(Debug, Clone, Default)]
pub struct AzureOpenAIResponsesOptions {
    pub base: StreamOptions,
    pub reasoning_effort: Option<OpenAIReasoningEffort>,
    pub reasoning_summary: Option<String>,
    pub azure_api_version: Option<String>,
    pub azure_resource_name: Option<String>,
    pub azure_base_url: Option<String>,
    pub azure_deployment_name: Option<String>,
    pub tool_choice: Option<Value>,
}

impl Deref for AzureOpenAIResponsesOptions {
    type Target = StreamOptions;
    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for AzureOpenAIResponsesOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl From<&AzureOpenAIResponsesOptions> for ApiStreamOptions {
    fn from(options: &AzureOpenAIResponsesOptions) -> Self {
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
        if let Some(v) = &options.azure_api_version {
            extra.insert("azureApiVersion".to_string(), Value::String(v.clone()));
        }
        if let Some(v) = &options.azure_resource_name {
            extra.insert("azureResourceName".to_string(), Value::String(v.clone()));
        }
        if let Some(v) = &options.azure_base_url {
            extra.insert("azureBaseUrl".to_string(), Value::String(v.clone()));
        }
        if let Some(v) = &options.azure_deployment_name {
            extra.insert("azureDeploymentName".to_string(), Value::String(v.clone()));
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

impl From<AzureOpenAIResponsesOptions> for ApiStreamOptions {
    fn from(options: AzureOpenAIResponsesOptions) -> Self {
        Self::from(&options)
    }
}

impl AzureOpenAIResponsesOptions {
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
                .and_then(OpenAIReasoningEffort::from_str_opt),
            reasoning_summary: get_str("reasoningSummary"),
            azure_api_version: get_str("azureApiVersion"),
            azure_resource_name: get_str("azureResourceName"),
            azure_base_url: get_str("azureBaseUrl"),
            azure_deployment_name: get_str("azureDeploymentName"),
            tool_choice: api_opts.extra.get("toolChoice").cloned(),
            base: api_opts.base,
        }
    }
}

fn is_azure_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    lower.ends_with(".cognitiveservices.azure.com")
        || lower.ends_with(".openai.azure.com")
        || lower.ends_with(".ai.azure.com")
}

/// Normalize Azure OpenAI base URL according to host and path conventions.
pub fn normalize_azure_base_url(raw_url: &str) -> Result<String> {
    let trimmed = pi_js::str16::trim(raw_url);
    let mut parsed =
        Url::parse(trimmed).map_err(|e| Error::msg(format!("Invalid Azure OpenAI base URL \"{raw_url}\": {e}")))?;
    let host = parsed.host_str().unwrap_or("");
    if is_azure_host(host) {
        parsed.set_query(None);
        parsed.set_fragment(None);
        let path_trimmed = parsed.path().trim_end_matches('/').to_string();
        let normalized_path = match path_trimmed.as_str() {
            "" | "/openai" | "/openai/v1" | "/openai/v1/responses" => "/openai/v1",
            other => other,
        };
        parsed.set_path(normalized_path);
        Ok(parsed.to_string().trim_end_matches('/').to_string())
    } else {
        let query = parsed.query().map(str::to_string);
        let path_trimmed = parsed.path().trim_end_matches('/').to_string();
        parsed.set_path(&path_trimmed);
        parsed.set_query(query.as_deref());
        let s = parsed.to_string();
        Ok(if query.is_none() {
            s.trim_end_matches('/').to_string()
        } else {
            s
        })
    }
}

fn parse_deployment_name_map(raw: &str) -> IndexMap<String, String> {
    let mut map = IndexMap::new();
    for entry in raw.split(',') {
        let trimmed = pi_js::str16::trim(entry);
        if let Some((k, v)) = trimmed.split_once('=') {
            let key = pi_js::str16::trim(k);
            let val = pi_js::str16::trim(v);
            if !key.is_empty() && !val.is_empty() {
                map.insert(key.to_string(), val.to_string());
            }
        }
    }
    map
}

fn resolve_azure_config(
    model: &Model,
    options: &AzureOpenAIResponsesOptions,
) -> Result<(String, String, String, String)> {
    let env: Option<&ProviderEnv> = options.base.env.as_ref();

    let api_key = options
        .base
        .api_key
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| get_env_api_key("azure-openai-responses", env))
        .ok_or_else(|| Error::msg("Azure OpenAI API key is required (AZURE_OPENAI_API_KEY)"))?;

    let api_version = options
        .azure_api_version
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| get_provider_env_value("AZURE_OPENAI_API_VERSION", env).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| DEFAULT_AZURE_API_VERSION.to_string());

    let deployment_name = options
        .azure_deployment_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            get_provider_env_value("AZURE_OPENAI_DEPLOYMENT_NAME_MAP", env)
                .and_then(|raw| parse_deployment_name_map(&raw).get(&model.id).cloned())
        })
        .unwrap_or_else(|| model.id.clone());

    let raw_base_url = options
        .azure_base_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| get_provider_env_value("AZURE_OPENAI_BASE_URL", env).filter(|s| !s.is_empty()))
        .or_else(|| {
            let res = options
                .azure_resource_name
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| get_provider_env_value("AZURE_OPENAI_RESOURCE_NAME", env).filter(|s| !s.is_empty()));
            res.map(|name| format!("https://{name}.openai.azure.com"))
        })
        .unwrap_or_else(|| model.base_url.clone());

    let base_url = normalize_azure_base_url(&raw_base_url)?;
    Ok((api_key, base_url, api_version, deployment_name))
}

fn build_azure_responses_url(normalized_base: &str, api_version: &str) -> Result<String> {
    let mut url = Url::parse(normalized_base)
        .map_err(|e| Error::msg(format!("Invalid Azure OpenAI base URL \"{normalized_base}\": {e}")))?;
    let base_path = url.path().trim_end_matches('/').to_string();
    url.set_path(&format!("{base_path}/responses"));
    url.query_pairs_mut().append_pair("api-version", api_version);
    Ok(url.to_string())
}

fn allowed_tool_call_providers() -> IndexSet<String> {
    let mut set = IndexSet::new();
    set.insert("openai".to_string());
    set.insert("openai-codex".to_string());
    set.insert("opencode".to_string());
    set.insert("azure-openai-responses".to_string());
    set
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

fn build_azure_headers(model: &Model, api_key: &str, options_headers: Option<&ProviderHeaders>) -> Headers {
    let mut headers = Headers::new();
    headers.set("accept", "text/event-stream");
    headers.set("content-type", "application/json");
    headers.set("api-key", api_key);
    headers.set("user-agent", &get_pi_user_agent());
    if let Some(mh) = &model.headers {
        apply_raw_header_layer(&mut headers, mh);
    }
    if let Some(oh) = options_headers {
        apply_header_layer(&mut headers, oh);
    }
    headers
}

fn tool_choice_to_value(tc: &ToolChoice) -> Value {
    match tc {
        ToolChoice::Auto => Value::String("auto".to_string()),
        ToolChoice::None => Value::String("none".to_string()),
    }
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

fn build_params(
    model: &Model,
    deployment_name: &str,
    context: &TranscriptContext,
    options: &AzureOpenAIResponsesOptions,
    cache_retention: CacheRetention,
) -> Result<(Value, IndexMap<String, String>)> {
    let providers = allowed_tool_call_providers();
    let compat = model.compat.as_ref().and_then(|c| c.as_openai_responses());
    let supports_strict_mode = compat.and_then(|c| c.supports_strict_mode).unwrap_or(true);
    let supports_max_output_tokens = compat.and_then(|c| c.supports_max_output_tokens).unwrap_or(true);
    let supports_grammar_tools = compat.and_then(|c| c.supports_open_ai_grammar_tools).unwrap_or(false);
    let supports_mid_convo = compat.and_then(|c| c.supports_mid_convo_system_messages);
    let resolved_ctx = resolve_transcript(context, supports_mid_convo);
    let declared_tools = get_declared_tools(&resolved_ctx.messages);
    let grammar_props = create_grammar_tool_input_properties(Some(&declared_tools), supports_grammar_tools)?;

    let msg_opts = ConvertResponsesMessagesOptions {
        include_system_prompt: Some(true),
        grammar_tool_input_properties: if grammar_props.is_empty() {
            None
        } else {
            Some(grammar_props.clone())
        },
        ..Default::default()
    };
    let messages = convert_responses_messages(model, &resolved_ctx, &providers, Some(&msg_opts));

    let mut params = Map::new();
    params.insert("model".to_string(), Value::String(deployment_name.to_string()));
    params.insert("input".to_string(), Value::Array(messages));
    params.insert("stream".to_string(), Value::Bool(true));
    params.insert("store".to_string(), Value::Bool(false));

    if cache_retention != CacheRetention::None {
        if let Some(cache_key) = clamp_openai_prompt_cache_key(options.base.session_id.as_deref()) {
            if !cache_key.is_empty() {
                params.insert("prompt_cache_key".to_string(), Value::String(cache_key));
            }
        }
    }

    if supports_max_output_tokens {
        if let Some(max_tokens) = options.base.max_tokens {
            params.insert("max_output_tokens".to_string(), Value::from(max_tokens));
        }
    }
    if let Some(temp) = options.base.temperature {
        if let Some(num) = serde_json::Number::from_f64(temp) {
            params.insert("temperature".to_string(), Value::Number(num));
        }
    }

    if !declared_tools.is_empty() {
        let tool_opts = ConvertResponsesToolsOptions {
            strict: Some(supports_strict_mode),
            supports_openai_grammar_tools: Some(supports_grammar_tools),
            ..Default::default()
        };
        let converted_tools = convert_responses_tools(&declared_tools, Some(&tool_opts))?;
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

fn format_status_error(status: u16, body: &str) -> String {
    let trimmed = pi_js::str16::trim(body);
    if trimmed.is_empty() {
        format!("{status} status code (no body)")
    } else {
        format!("{status} {body}")
    }
}

async fn execute_request(
    endpoint_url: &str,
    options: &AzureOpenAIResponsesOptions,
    headers: &Headers,
    body_json: &str,
) -> Result<Response> {
    let fetch_fn = options.base.fetch.clone();
    let timeout_ms = options.base.timeout_ms;
    let signal = options.base.signal.clone();

    retry_provider_request(
        || {
            let url_c = endpoint_url.to_string();
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

async fn run_azure_responses_stream(
    model: Model,
    context: TranscriptContext,
    options: AzureOpenAIResponsesOptions,
    output: &mut crate::types::AssistantMessage,
    event_stream: &AssistantMessageEventStream,
) -> Result<()> {
    let (api_key, normalized_base, api_version, deployment_name) = resolve_azure_config(&model, &options)?;
    let endpoint_url = build_azure_responses_url(&normalized_base, &api_version)?;
    let cache_retention = resolve_responses_cache_retention(options.base.cache_retention, options.base.env.as_ref());
    let headers = build_azure_headers(&model, &api_key, options.base.headers.as_ref());

    let (mut params, grammar_props) = build_params(&model, &deployment_name, &context, &options, cache_retention)?;

    if let Some(on_payload) = &options.base.on_payload {
        if let Some(next_params) = on_payload(params.clone(), &model).await? {
            let mut next_obj = next_params.as_object().cloned().unwrap_or_default();
            next_obj.insert("stream".to_string(), Value::Bool(true));
            params = Value::Object(next_obj);
        }
    }

    let body_json = pi_js::json::stringify(&params);
    let response = execute_request(&endpoint_url, &options, &headers, &body_json).await?;
    let event_json_stream =
        response_to_sse_json_stream(response, model.clone(), options.base.on_provider_stream_event.clone())?;

    event_stream.push(crate::types::AssistantMessageEvent::Start {
        partial: output.clone(),
    });

    let process_opts = ProcessResponsesStreamOptions {
        service_tier: None,
        resolve_service_tier_on_default: false,
        grammar_tool_input_properties: if grammar_props.is_empty() {
            None
        } else {
            Some(grammar_props)
        },
    };
    process_responses_stream(event_json_stream, output, event_stream, &model, Some(&process_opts)).await
}

fn format_azure_error(err: &Error, output: &mut crate::types::AssistantMessage) -> String {
    let norm = normalize_provider_error(err);
    let formatted = format_provider_error(&norm, None);
    let enriched = enrich_chatgpt_usage_limit_error(&formatted);
    let truncated = truncate_error_text(&enriched, 4000);
    append_assistant_message_diagnostic(output, create_assistant_message_diagnostic("provider_error", err, None));
    truncated
}

/// Stream responses using Azure OpenAI Responses API (`azure-openai-responses`).
pub fn stream_azure_openai_responses(
    model: &Model,
    context: &TranscriptContext,
    options: Option<ApiStreamOptions>,
) -> AssistantMessageEventStream {
    let model_owned = model.clone();
    let context_owned = context.clone();
    let typed_options = AzureOpenAIResponsesOptions::from_api_options(options);
    let stream = create_assistant_message_event_stream();
    let stream_handle = stream.clone();

    tokio::spawn(async move {
        let mut output = create_pending_assistant_message(&model_owned);
        match run_azure_responses_stream(
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
                output.error_message = Some(format_azure_error(&err, &mut output));
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
    stream_azure_openai_responses(model, context, options)
}

/// Simplified streaming wrapper for Azure OpenAI Responses API.
pub fn stream_simple_azure_openai_responses(
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

    let typed = AzureOpenAIResponsesOptions {
        base,
        reasoning_effort,
        reasoning_summary: None,
        azure_api_version: None,
        azure_resource_name: None,
        azure_base_url: None,
        azure_deployment_name: None,
        tool_choice,
    };
    stream_azure_openai_responses(model, context, Some(typed.into()))
}

pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    stream_simple_azure_openai_responses(model, context, options)
}

/// Construct `ProviderStreams` descriptor for Azure OpenAI Responses API.
pub fn provider_streams() -> ProviderStreams {
    ProviderStreams {
        stream: Arc::new(|model, context, options| stream(model, context, options)),
        stream_simple: Arc::new(|model, context, options| stream_simple(model, context, options)),
        fetch_deferred: None,
        cancel_deferred: None,
    }
}

/// Alias matching `azure_openai_responses_api()` naming convention.
pub fn azure_openai_responses_api() -> ProviderStreams {
    provider_streams()
}
