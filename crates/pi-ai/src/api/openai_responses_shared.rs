//! Port of packages/ai/src/api/openai-responses-shared.ts

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;

use futures::StreamExt;
use indexmap::{IndexMap, IndexSet};
use pi_js::BoxFuture;
use pi_js::error::{Error, Result};
use pi_js::fetch::{BodyStream, Response};
use serde_json::{Map, Value};

use crate::api::constrained_sampling::{
    GrammarConstrainedSampling, GrammarSamplingFormat, GrammarToolInputJsonBuffer,
    append_grammar_tool_input_json_delta, get_json_schema_tool_parameters, resolve_grammar_constrained_sampling,
    resolve_json_schema_strict_sampling,
};
use crate::api::transform_messages::transform_messages;
use crate::models::calculate_cost;
use crate::types::{
    AnyModel, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Message, Model, ModelModality,
    StopReason, TextContent, ThinkingContent, Tool, ToolCall, TranscriptContext, Usage, UserContentBlock,
    UserMessageContent,
};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::hash::short_hash;
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::text::{get_system_message_text, render_system_message_update};
use crate::utils::transcript::{
    get_initial_system_message, resolve_transcript, resolve_transcript_tools, without_initial_system_message,
};

pub const CHATGPT_USAGE_LIMIT_URL: &str = "https://chatgpt.com/settings/usage";

/// Boxed async stream of parsed OpenAI Responses SSE JSON payloads.
pub type ResponsesEventJsonStream = Pin<Box<dyn futures::stream::Stream<Item = Result<Value>> + Send>>;

/// Parsed v1 text signature attached to assistant text blocks in Responses APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSignatureV1 {
    pub id: String,
    pub phase: Option<String>,
}

/// Options controlling conversion of conversation messages into OpenAI Responses input items.
#[derive(Debug, Clone, Default)]
pub struct ConvertResponsesMessagesOptions {
    pub include_system_prompt: Option<bool>,
    pub grammar_tool_input_properties: Option<IndexMap<String, String>>,
    pub supports_mid_convo_system_messages: Option<bool>,
    pub supports_additional_tools: bool,
    pub supports_tool_search: bool,
    pub tool_options: Option<ConvertResponsesToolsOptions>,
}

/// Options controlling conversion of tools into OpenAI Responses tool definitions.
#[derive(Debug, Clone, Default)]
pub struct ConvertResponsesToolsOptions {
    pub strict: Option<bool>,
    pub supports_openai_grammar_tools: Option<bool>,
    pub tool_search_result: bool,
}

/// Options controlling `process_responses_stream`.
#[derive(Clone, Default)]
pub struct ProcessResponsesStreamOptions {
    pub service_tier: Option<String>,
    pub resolve_service_tier_on_default: bool,
    pub grammar_tool_input_properties: Option<IndexMap<String, String>>,
}

impl std::fmt::Debug for ProcessResponsesStreamOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessResponsesStreamOptions")
            .field("service_tier", &self.service_tier)
            .field("resolve_service_tier_on_default", &self.resolve_service_tier_on_default)
            .field("grammar_tool_input_properties", &self.grammar_tool_input_properties)
            .finish()
    }
}

/// Encode a Responses message ID and optional phase into `TextContent.text_signature`.
pub fn encode_text_signature_v1(id: &str, phase: Option<&str>) -> String {
    let mut obj = Map::new();
    obj.insert("v".to_string(), Value::from(1));
    obj.insert("id".to_string(), Value::String(id.to_string()));
    if let Some(phase) = phase {
        if !phase.is_empty() {
            obj.insert("phase".to_string(), Value::String(phase.to_string()));
        }
    }
    pi_js::json::stringify(&Value::Object(obj))
}

/// Parse `TextContent.text_signature` as either JSON v1 `{ v: 1, id, phase? }` or a legacy plain ID.
pub fn parse_text_signature_v1(signature: Option<&str>) -> Option<TextSignatureV1> {
    let sig = signature?;
    if sig.is_empty() {
        return None;
    }
    if sig.starts_with('{') {
        if let Ok(Value::Object(obj)) = pi_js::json::parse(sig) {
            if obj.get("v").and_then(Value::as_i64) == Some(1) {
                if let Some(id) = obj.get("id").and_then(Value::as_str) {
                    if !id.is_empty() {
                        let phase = obj.get("phase").and_then(Value::as_str).map(str::to_string);
                        return Some(TextSignatureV1 {
                            id: id.to_string(),
                            phase,
                        });
                    }
                }
            }
        }
    }
    Some(TextSignatureV1 {
        id: sig.to_string(),
        phase: None,
    })
}

/// Return the pricing multiplier for OpenAI Responses service tiers.
pub fn get_service_tier_cost_multiplier(service_tier: Option<&str>, model: Option<&Model>) -> f64 {
    let Some(tier) = service_tier else {
        return 1.0;
    };
    match tier.to_ascii_lowercase().as_str() {
        "flex" => 0.5,
        "priority" | "fast" => {
            if model.is_some_and(|m| m.id == "gpt-5.5") {
                2.5
            } else {
                2.0
            }
        }
        _ => 1.0,
    }
}

/// Apply service tier cost multipliers in place on `Usage.cost`.
pub fn apply_service_tier_pricing(usage: &mut Usage, service_tier: Option<&str>, model: Option<&Model>) {
    let multiplier = get_service_tier_cost_multiplier(service_tier, model);
    if (multiplier - 1.0).abs() < f64::EPSILON {
        return;
    }
    usage.cost.input *= multiplier;
    usage.cost.output *= multiplier;
    usage.cost.cache_read *= multiplier;
    usage.cost.cache_write *= multiplier;
    usage.cost.total = usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
}

/// Append a ChatGPT usage limit link when the error mentions `subscription_sharing_usage_limit_exceeded`.
pub fn enrich_chatgpt_usage_limit_error(message: &str) -> String {
    if message.contains("subscription_sharing_usage_limit_exceeded") && !message.contains(CHATGPT_USAGE_LIMIT_URL) {
        format!("{message} Check your ChatGPT usage: {CHATGPT_USAGE_LIMIT_URL}")
    } else {
        message.to_string()
    }
}

/// Sanitize a foreign tool call ID into `(call_id, item_id)` safe for OpenAI/Codex Responses.
pub fn normalize_responses_tool_call_id(id: &str, target_model: &Model, source: &AssistantMessage) -> String {
    let is_foreign = source.provider != target_model.provider || source.api != target_model.api;
    if !id.contains('|') {
        let sanitized = sanitize_id_chars(id, 64);
        return if sanitized.is_empty() {
            format!("call_{}", short_hash(id))
        } else {
            sanitized
        };
    }

    let mut parts = id.splitn(2, '|');
    let raw_call_id = parts.next().unwrap_or("");
    let raw_item_id = parts.next().unwrap_or("");

    let mut sanitized_call_id = sanitize_id_chars(raw_call_id, 64);
    if sanitized_call_id.is_empty() {
        sanitized_call_id = format!("call_{}", short_hash(raw_call_id));
    }

    let mut sanitized_item_id = if is_foreign {
        format!("fc_{}", short_hash(raw_item_id))
    } else {
        sanitize_id_chars(raw_item_id, 64)
    };
    let valid_item_prefix = sanitized_item_id.starts_with("fc_") || sanitized_item_id.starts_with("ctc_");
    if !valid_item_prefix || (!is_foreign && sanitized_item_id.len() != raw_item_id.len()) {
        sanitized_item_id = format!("fc_{}", short_hash(raw_item_id));
    }

    format!("{sanitized_call_id}|{sanitized_item_id}")
}

fn sanitize_id_chars(raw: &str, max_len: usize) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(max_len)
        .collect();
    mapped.trim_end_matches('_').to_string()
}

fn split_tool_call_id(id: &str) -> (&str, Option<&str>) {
    if let Some((call_id, item_id)) = id.split_once('|') {
        let item_opt = if item_id.is_empty() { None } else { Some(item_id) };
        (call_id, item_opt)
    } else {
        (id, None)
    }
}

fn format_user_content_blocks(blocks: &[UserContentBlock], allow_images: bool) -> Vec<Value> {
    let mut out = Vec::new();
    for block in blocks {
        match block {
            UserContentBlock::Text(text) => {
                let mut obj = Map::new();
                obj.insert("type".to_string(), Value::String("input_text".to_string()));
                obj.insert("text".to_string(), Value::String(sanitize_surrogates(&text.text)));
                out.push(Value::Object(obj));
            }
            UserContentBlock::Image(img) if allow_images => {
                let mut obj = Map::new();
                obj.insert("type".to_string(), Value::String("input_image".to_string()));
                obj.insert("detail".to_string(), Value::String("auto".to_string()));
                obj.insert(
                    "image_url".to_string(),
                    Value::String(format!("data:{};base64,{}", img.mime_type, img.data)),
                );
                out.push(Value::Object(obj));
            }
            UserContentBlock::Image(_) => {}
        }
    }
    out
}

/// Convert `TranscriptContext` messages into an array of OpenAI Responses `input` items.
pub fn convert_responses_messages(
    model: &Model,
    context: &TranscriptContext,
    allowed_tool_call_providers: &IndexSet<String>,
    options: Option<&ConvertResponsesMessagesOptions>,
) -> Vec<Value> {
    let include_system_prompt = options.and_then(|o| o.include_system_prompt).unwrap_or(true);
    let grammar_props = options.and_then(|o| o.grammar_tool_input_properties.as_ref());
    let supports_mid_convo_sys = options.and_then(|o| o.supports_mid_convo_system_messages);
    let supports_additional_tools = options.is_some_and(|o| o.supports_additional_tools);
    let supports_tool_search = options.is_some_and(|o| o.supports_tool_search);
    let tool_options = options.and_then(|o| o.tool_options.clone());

    let resolved_ctx = resolve_transcript(context, supports_mid_convo_sys);
    let transcript_tools = resolve_transcript_tools(
        &resolved_ctx.messages,
        supports_additional_tools || supports_tool_search,
    );

    let normalize_cb = |id: &str, target: &Model, source: &AssistantMessage| -> String {
        if allowed_tool_call_providers.contains(&target.provider) {
            normalize_responses_tool_call_id(id, target, source)
        } else {
            id.to_string()
        }
    };
    let initial_system = get_initial_system_message(&resolved_ctx.messages);
    let body_messages = without_initial_system_message(&resolved_ctx.messages);
    let transformed = transform_messages(&body_messages, model, Some(&normalize_cb));

    let supports_developer_role = model
        .compat
        .as_ref()
        .and_then(|c| c.as_openai_responses())
        .and_then(|rc| rc.supports_developer_role)
        .unwrap_or(true);
    let instruction_role = if model.reasoning && supports_developer_role {
        "developer"
    } else {
        "system"
    };

    let mut messages: Vec<Value> = Vec::new();
    let allow_images = model.input.contains(&ModelModality::Image);

    if include_system_prompt {
        if let Some(sys) = initial_system {
            let text = get_system_message_text(sys);
            if !text.is_empty() {
                let mut item = Map::new();
                item.insert("role".to_string(), Value::String(instruction_role.to_string()));
                item.insert("content".to_string(), Value::String(sanitize_surrogates(&text)));
                messages.push(Value::Object(item));
            }
        }
    }

    let mut msg_index: usize = 0;
    for msg in &transformed {
        match msg {
            Message::System(sys) => {
                if transcript_tools.anchors_additions {
                    if let Some(added) = sys.tools_added.as_deref().filter(|t| !t.is_empty()) {
                        if supports_additional_tools {
                            if let Ok(converted) = convert_responses_tools(added, tool_options.as_ref()) {
                                let mut add_item = Map::new();
                                add_item.insert("type".to_string(), Value::String("additional_tools".to_string()));
                                add_item.insert("role".to_string(), Value::String("developer".to_string()));
                                add_item.insert("tools".to_string(), Value::Array(converted));
                                messages.push(Value::Object(add_item));
                            }
                        } else if supports_tool_search {
                            let names: Vec<&str> = added.iter().map(|t| t.name.as_str()).collect();
                            let call_id = format!(
                                "pi_tool_load_{}",
                                short_hash(&format!("system:{msg_index}:{}", names.join(",")))
                            );
                            let mut call_args = Map::new();
                            call_args.insert("query".to_string(), Value::String(names.join(" ")));
                            call_args.insert(
                                "limit".to_string(),
                                Value::Number(serde_json::Number::from(names.len() as u64)),
                            );
                            let mut call_item = Map::new();
                            call_item.insert("type".to_string(), Value::String("tool_search_call".to_string()));
                            call_item.insert("call_id".to_string(), Value::String(call_id.clone()));
                            call_item.insert("execution".to_string(), Value::String("client".to_string()));
                            call_item.insert("status".to_string(), Value::String("completed".to_string()));
                            call_item.insert("arguments".to_string(), Value::Object(call_args));
                            messages.push(Value::Object(call_item));

                            let mut search_tool_opts = tool_options.clone().unwrap_or_default();
                            search_tool_opts.tool_search_result = true;
                            if let Ok(converted) = convert_responses_tools(added, Some(&search_tool_opts)) {
                                let mut out_item = Map::new();
                                out_item.insert("type".to_string(), Value::String("tool_search_output".to_string()));
                                out_item.insert("call_id".to_string(), Value::String(call_id));
                                out_item.insert("execution".to_string(), Value::String("client".to_string()));
                                out_item.insert("status".to_string(), Value::String("completed".to_string()));
                                out_item.insert("tools".to_string(), Value::Array(converted));
                                messages.push(Value::Object(out_item));
                            }
                        }
                    }
                }
                let text = render_system_message_update(sys);
                if !text.is_empty() {
                    let mut item = Map::new();
                    item.insert("role".to_string(), Value::String(instruction_role.to_string()));
                    item.insert("content".to_string(), Value::String(sanitize_surrogates(&text)));
                    messages.push(Value::Object(item));
                }
            }
            Message::User(user) => match &user.content {
                UserMessageContent::Text(text) => {
                    let mut text_obj = Map::new();
                    text_obj.insert("type".to_string(), Value::String("input_text".to_string()));
                    text_obj.insert("text".to_string(), Value::String(sanitize_surrogates(text)));
                    let mut item = Map::new();
                    item.insert("role".to_string(), Value::String("user".to_string()));
                    item.insert("content".to_string(), Value::Array(vec![Value::Object(text_obj)]));
                    messages.push(Value::Object(item));
                }
                UserMessageContent::Blocks(blocks) => {
                    let contents = format_user_content_blocks(blocks, allow_images);
                    if !contents.is_empty() {
                        let mut item = Map::new();
                        item.insert("role".to_string(), Value::String("user".to_string()));
                        item.insert("content".to_string(), Value::Array(contents));
                        messages.push(Value::Object(item));
                    }
                }
            },
            Message::Assistant(assistant) => {
                let is_different_model =
                    assistant.model != model.id && assistant.provider == model.provider && assistant.api == model.api;
                let is_same_source =
                    assistant.model == model.id && assistant.provider == model.provider && assistant.api == model.api;

                let mut output_items: Vec<Value> = Vec::new();
                let mut fallback_text_count: usize = 0;

                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Thinking(thinking) => {
                            if let Some(sig) = thinking.thinking_signature.as_deref() {
                                if let Ok(Value::Object(mut reasoning_obj)) = pi_js::json::parse(sig) {
                                    let is_reasoning_item =
                                        reasoning_obj.get("type").and_then(Value::as_str) == Some("reasoning");
                                    if is_reasoning_item && !is_different_model {
                                        reasoning_obj.shift_remove("status");
                                        output_items.push(Value::Object(reasoning_obj));
                                    }
                                }
                            }
                        }
                        AssistantContentBlock::Text(text_block) => {
                            let parsed_sig = parse_text_signature_v1(text_block.text_signature.as_deref());
                            let explicit_id = parsed_sig
                                .as_ref()
                                .map(|s| s.id.as_str())
                                .filter(|id| !id.is_empty() && id.len() <= 64 && is_same_source);
                            let msg_id = if let Some(id) = explicit_id {
                                id.to_string()
                            } else if fallback_text_count == 0 {
                                fallback_text_count += 1;
                                format!("msg_pi_{msg_index}")
                            } else {
                                let idx = fallback_text_count;
                                fallback_text_count += 1;
                                format!("msg_pi_{msg_index}_{idx}")
                            };

                            let mut out_text = Map::new();
                            out_text.insert("type".to_string(), Value::String("output_text".to_string()));
                            out_text.insert("text".to_string(), Value::String(sanitize_surrogates(&text_block.text)));
                            out_text.insert("annotations".to_string(), Value::Array(Vec::new()));

                            let mut msg_obj = Map::new();
                            msg_obj.insert("type".to_string(), Value::String("message".to_string()));
                            msg_obj.insert("role".to_string(), Value::String("assistant".to_string()));
                            msg_obj.insert("content".to_string(), Value::Array(vec![Value::Object(out_text)]));
                            msg_obj.insert("status".to_string(), Value::String("completed".to_string()));
                            msg_obj.insert("id".to_string(), Value::String(msg_id));
                            if is_same_source {
                                if let Some(phase) = parsed_sig.and_then(|s| s.phase) {
                                    if !phase.is_empty() {
                                        msg_obj.insert("phase".to_string(), Value::String(phase));
                                    }
                                }
                            }
                            output_items.push(Value::Object(msg_obj));
                        }
                        AssistantContentBlock::ToolCall(tc) => {
                            let (call_id, raw_item_id) = split_tool_call_id(&tc.id);
                            let custom_prop = grammar_props.and_then(|m| m.get(&tc.name)).map(String::as_str);
                            let expected_prefix = if custom_prop.is_some() { "ctc_" } else { "fc_" };
                            let include_item_id = raw_item_id.and_then(|item_id| {
                                if is_different_model || !item_id.starts_with(expected_prefix) {
                                    None
                                } else {
                                    Some(item_id)
                                }
                            });

                            let mut item = Map::new();
                            if let Some(prop) = custom_prop {
                                let input_val = tc.arguments.get(prop).and_then(Value::as_str).unwrap_or("");
                                item.insert("type".to_string(), Value::String("custom_tool_call".to_string()));
                                item.insert("call_id".to_string(), Value::String(call_id.to_string()));
                                item.insert("name".to_string(), Value::String(tc.name.clone()));
                                item.insert("input".to_string(), Value::String(input_val.to_string()));
                            } else {
                                item.insert("type".to_string(), Value::String("function_call".to_string()));
                                item.insert("call_id".to_string(), Value::String(call_id.to_string()));
                                item.insert("name".to_string(), Value::String(tc.name.clone()));
                                let arg_map: Map<String, Value> =
                                    tc.arguments.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                                item.insert(
                                    "arguments".to_string(),
                                    Value::String(pi_js::json::stringify(&Value::Object(arg_map))),
                                );
                            }
                            if let Some(item_id) = include_item_id {
                                item.insert("id".to_string(), Value::String(item_id.to_string()));
                            }
                            if is_same_source {
                                if let Some(ns) = &tc.namespace {
                                    if !ns.is_empty() {
                                        item.insert("namespace".to_string(), Value::String(ns.clone()));
                                    }
                                }
                            }
                            output_items.push(Value::Object(item));
                        }
                    }
                }
                messages.extend(output_items);
            }
            Message::ToolResult(tr) => {
                let (call_id, _) = split_tool_call_id(&tr.tool_call_id);
                let mut text_parts: Vec<String> = Vec::new();
                let mut image_blocks: Vec<Value> = Vec::new();

                for block in &tr.content {
                    match block {
                        UserContentBlock::Text(t) => {
                            if !t.text.is_empty() {
                                text_parts.push(sanitize_surrogates(&t.text));
                            }
                        }
                        UserContentBlock::Image(img) if allow_images => {
                            let mut obj = Map::new();
                            obj.insert("type".to_string(), Value::String("input_image".to_string()));
                            obj.insert("detail".to_string(), Value::String("auto".to_string()));
                            obj.insert(
                                "image_url".to_string(),
                                Value::String(format!("data:{};base64,{}", img.mime_type, img.data)),
                            );
                            image_blocks.push(Value::Object(obj));
                        }
                        UserContentBlock::Image(_) => {}
                    }
                }

                let joined_text = text_parts.join("\n");
                let has_text = !joined_text.is_empty();
                let has_images = !image_blocks.is_empty();

                let output_val = if !has_images {
                    if has_text {
                        Value::String(joined_text)
                    } else {
                        Value::String("(no tool output)".to_string())
                    }
                } else {
                    let mut items = Vec::new();
                    let label = if has_text {
                        joined_text
                    } else {
                        "(see attached image)".to_string()
                    };
                    let mut t_obj = Map::new();
                    t_obj.insert("type".to_string(), Value::String("input_text".to_string()));
                    t_obj.insert("text".to_string(), Value::String(label));
                    items.push(Value::Object(t_obj));
                    items.extend(image_blocks);
                    Value::Array(items)
                };

                let tr_type = if grammar_props.is_some_and(|m| m.contains_key(&tr.tool_name)) {
                    "custom_tool_call_output"
                } else {
                    "function_call_output"
                };
                let mut tr_item = Map::new();
                tr_item.insert("type".to_string(), Value::String(tr_type.to_string()));
                tr_item.insert("call_id".to_string(), Value::String(call_id.to_string()));
                tr_item.insert("output".to_string(), output_val);
                messages.push(Value::Object(tr_item));
            }
        }
        msg_index += 1;
    }

    messages
}

/// Convert `Tool` definitions into OpenAI Responses API `tools` JSON objects.
pub fn convert_responses_tools(tools: &[Tool], options: Option<&ConvertResponsesToolsOptions>) -> Result<Vec<Value>> {
    let supports_strict_mode = options.and_then(|o| o.strict).unwrap_or(true);
    let supports_grammar_tools = options.and_then(|o| o.supports_openai_grammar_tools).unwrap_or(false);
    let tool_search_result = options.is_some_and(|o| o.tool_search_result);
    let mut out = Vec::with_capacity(tools.len());

    for tool in tools {
        if let Some(GrammarConstrainedSampling {
            format,
            definition,
            input_property: _,
        }) = resolve_grammar_constrained_sampling(tool, supports_grammar_tools)?
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
            let mut obj = Map::new();
            obj.insert("type".to_string(), Value::String("custom".to_string()));
            obj.insert("name".to_string(), Value::String(tool.name.clone()));
            obj.insert("description".to_string(), Value::String(tool.description.clone()));
            obj.insert("format".to_string(), Value::Object(fmt_obj));
            if tool_search_result {
                obj.insert("defer_loading".to_string(), Value::Bool(true));
            }
            out.push(Value::Object(obj));
            continue;
        }

        let strict_bool = resolve_json_schema_strict_sampling(tool, supports_strict_mode, None)?;
        let params_schema = get_json_schema_tool_parameters(tool, strict_bool)?;

        let mut obj = Map::new();
        obj.insert("type".to_string(), Value::String("function".to_string()));
        obj.insert("name".to_string(), Value::String(tool.name.clone()));
        obj.insert("description".to_string(), Value::String(tool.description.clone()));
        obj.insert("parameters".to_string(), params_schema.to_value());
        if tool_search_result {
            obj.insert("defer_loading".to_string(), Value::Bool(true));
        }
        if supports_strict_mode {
            obj.insert("strict".to_string(), Value::Bool(strict_bool.unwrap_or(false)));
        }
        out.push(Value::Object(obj));
    }

    Ok(out)
}

#[derive(Debug, Clone)]
enum ActiveItemKind {
    Reasoning,
    Text,
    ToolCall {
        item_id: Option<String>,
        call_id: String,
        partial_json: String,
        is_custom: bool,
        grammar_buffer: Option<GrammarToolInputJsonBuffer>,
        done: bool,
    },
}

#[derive(Debug, Clone)]
struct ActiveBlock {
    block_idx: usize,
    kind: ActiveItemKind,
}

/// Process a stream of parsed OpenAI Responses SSE JSON events into `output` and `event_stream`.
pub async fn process_responses_stream(
    mut events: ResponsesEventJsonStream,
    output: &mut AssistantMessage,
    event_stream: &AssistantMessageEventStream,
    model: &Model,
    options: Option<&ProcessResponsesStreamOptions>,
) -> Result<()> {
    let mut blocks_by_output_index: IndexMap<usize, usize> = IndexMap::new();
    let mut active_blocks: Vec<ActiveBlock> = Vec::new();
    let mut fallback_block_idx: Option<usize> = None;
    let mut saw_terminal_event = false;

    let grammar_props = options.and_then(|o| o.grammar_tool_input_properties.as_ref());
    let requested_service_tier = options.and_then(|o| o.service_tier.as_deref());

    while let Some(event_res) = events.next().await {
        let event = event_res?;
        let Some(event_type) = event.get("type").and_then(Value::as_str) else {
            continue;
        };

        match event_type {
            "response.created" => {
                if let Some(id) = event.get("response").and_then(|r| r.get("id")).and_then(Value::as_str) {
                    output.response_id = Some(id.to_string());
                }
            }
            "response.output_item.added" => {
                let output_index = event.get("output_index").and_then(Value::as_u64).map(|u| u as usize);
                let Some(item) = event.get("item") else {
                    continue;
                };
                let Some(item_type) = item.get("type").and_then(Value::as_str) else {
                    continue;
                };

                match item_type {
                    "reasoning" => {
                        let block_idx = output.content.len();
                        output.content.push(AssistantContentBlock::Thinking(ThinkingContent {
                            thinking: String::new(),
                            thinking_signature: None,
                            redacted: None,
                        }));
                        let state_idx = active_blocks.len();
                        active_blocks.push(ActiveBlock {
                            block_idx,
                            kind: ActiveItemKind::Reasoning,
                        });
                        if let Some(oi) = output_index {
                            blocks_by_output_index.insert(oi, state_idx);
                        } else {
                            fallback_block_idx = Some(state_idx);
                        }
                        event_stream.push(AssistantMessageEvent::ThinkingStart {
                            content_index: block_idx,
                            partial: output.clone(),
                        });
                    }
                    "message" => {
                        let phase = item.get("phase").and_then(Value::as_str);
                        if phase == Some("final_answer") {
                            output.stop_reason = StopReason::Stop;
                        }
                        let text_signature = item
                            .get("id")
                            .and_then(Value::as_str)
                            .map(|id| encode_text_signature_v1(id, phase));
                        let block_idx = output.content.len();
                        output.content.push(AssistantContentBlock::Text(TextContent {
                            text: String::new(),
                            text_signature,
                        }));
                        let state_idx = active_blocks.len();
                        active_blocks.push(ActiveBlock {
                            block_idx,
                            kind: ActiveItemKind::Text,
                        });
                        if let Some(oi) = output_index {
                            blocks_by_output_index.insert(oi, state_idx);
                        } else {
                            fallback_block_idx = Some(state_idx);
                        }
                        event_stream.push(AssistantMessageEvent::TextStart {
                            content_index: block_idx,
                            partial: output.clone(),
                        });
                    }
                    "function_call" | "custom_tool_call" => {
                        let is_custom = item_type == "custom_tool_call";
                        let item_id = item.get("id").and_then(Value::as_str).map(str::to_string);
                        let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or("").to_string();
                        let name = item.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                        let namespace = item.get("namespace").and_then(Value::as_str).map(str::to_string);
                        let full_id = match &item_id {
                            Some(id) if !id.is_empty() => format!("{call_id}|{id}"),
                            _ => call_id.clone(),
                        };
                        let initial_raw = if is_custom {
                            item.get("input").and_then(Value::as_str).unwrap_or("")
                        } else {
                            item.get("arguments").and_then(Value::as_str).unwrap_or("")
                        };
                        let initial_args = if is_custom {
                            parse_tool_call_args(initial_raw, true, &name, grammar_props)
                        } else {
                            IndexMap::new()
                        };

                        let block_idx = output.content.len();
                        output.content.push(AssistantContentBlock::ToolCall(ToolCall {
                            id: full_id,
                            name,
                            arguments: initial_args,
                            thought_signature: None,
                            namespace,
                        }));
                        let state_idx = active_blocks.len();
                        active_blocks.push(ActiveBlock {
                            block_idx,
                            kind: ActiveItemKind::ToolCall {
                                item_id,
                                call_id,
                                partial_json: initial_raw.to_string(),
                                is_custom,
                                grammar_buffer: is_custom.then(GrammarToolInputJsonBuffer::default),
                                done: false,
                            },
                        });
                        if let Some(oi) = output_index {
                            blocks_by_output_index.insert(oi, state_idx);
                        } else {
                            fallback_block_idx = Some(state_idx);
                        }
                        event_stream.push(AssistantMessageEvent::ToolCallStart {
                            content_index: block_idx,
                            partial: output.clone(),
                        });
                    }
                    _ => {}
                }
            }
            "response.reasoning_summary_part.added" => {
                let output_index = event.get("output_index").and_then(Value::as_u64).map(|u| u as usize);
                if let Some(state_idx) = resolve_state_idx(
                    output_index,
                    &blocks_by_output_index,
                    fallback_block_idx,
                    &active_blocks,
                    |k| matches!(k, ActiveItemKind::Reasoning),
                ) {
                    let block_idx = active_blocks[state_idx].block_idx;
                    if let Some(AssistantContentBlock::Thinking(t)) = output.content.get_mut(block_idx) {
                        if !t.thinking.is_empty() {
                            t.thinking.push_str("\n\n");
                            event_stream.push(AssistantMessageEvent::ThinkingDelta {
                                content_index: block_idx,
                                delta: "\n\n".to_string(),
                                partial: output.clone(),
                            });
                        }
                    }
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let output_index = event.get("output_index").and_then(Value::as_u64).map(|u| u as usize);
                let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if delta.is_empty() {
                    continue;
                }
                if let Some(state_idx) = resolve_state_idx(
                    output_index,
                    &blocks_by_output_index,
                    fallback_block_idx,
                    &active_blocks,
                    |k| matches!(k, ActiveItemKind::Reasoning),
                ) {
                    let block_idx = active_blocks[state_idx].block_idx;
                    if let Some(AssistantContentBlock::Thinking(t)) = output.content.get_mut(block_idx) {
                        t.thinking.push_str(delta);
                        event_stream.push(AssistantMessageEvent::ThinkingDelta {
                            content_index: block_idx,
                            delta: delta.to_string(),
                            partial: output.clone(),
                        });
                    }
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let output_index = event.get("output_index").and_then(Value::as_u64).map(|u| u as usize);
                let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if delta.is_empty() {
                    continue;
                }
                if let Some(state_idx) = resolve_state_idx(
                    output_index,
                    &blocks_by_output_index,
                    fallback_block_idx,
                    &active_blocks,
                    |k| matches!(k, ActiveItemKind::Text),
                ) {
                    let block_idx = active_blocks[state_idx].block_idx;
                    if let Some(AssistantContentBlock::Text(t)) = output.content.get_mut(block_idx) {
                        t.text.push_str(delta);
                        event_stream.push(AssistantMessageEvent::TextDelta {
                            content_index: block_idx,
                            delta: delta.to_string(),
                            partial: output.clone(),
                        });
                    }
                }
            }
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                let output_index = event.get("output_index").and_then(Value::as_u64).map(|u| u as usize);
                let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if let Some(state_idx) = resolve_state_idx(
                    output_index,
                    &blocks_by_output_index,
                    fallback_block_idx,
                    &active_blocks,
                    |k| matches!(k, ActiveItemKind::ToolCall { .. }),
                ) {
                    let block_idx = active_blocks[state_idx].block_idx;
                    let tool_name = match output.content.get(block_idx) {
                        Some(AssistantContentBlock::ToolCall(tc)) => tc.name.clone(),
                        _ => String::new(),
                    };
                    let custom_prop = grammar_props
                        .and_then(|m| m.get(&tool_name))
                        .map(String::as_str)
                        .unwrap_or("input");
                    let mut emitted_delta: Option<String> = None;
                    let parsed_args = if let ActiveItemKind::ToolCall {
                        partial_json,
                        is_custom,
                        grammar_buffer,
                        ..
                    } = &mut active_blocks[state_idx].kind
                    {
                        partial_json.push_str(delta);
                        if *is_custom {
                            if let Some(buf) = grammar_buffer {
                                if let Ok(d) =
                                    append_grammar_tool_input_json_delta(buf, custom_prop, partial_json, false)
                                {
                                    emitted_delta = d;
                                }
                            }
                        } else {
                            emitted_delta = Some(delta.to_string());
                        }
                        Some(parse_tool_call_args(
                            partial_json,
                            *is_custom,
                            &tool_name,
                            grammar_props,
                        ))
                    } else {
                        None
                    };
                    if let (Some(args), Some(AssistantContentBlock::ToolCall(tc))) =
                        (parsed_args, output.content.get_mut(block_idx))
                    {
                        tc.arguments = args;
                    }
                    if let Some(out_delta) = emitted_delta {
                        event_stream.push(AssistantMessageEvent::ToolCallDelta {
                            content_index: block_idx,
                            delta: out_delta,
                            partial: output.clone(),
                        });
                    }
                }
            }
            "response.function_call_arguments.done" | "response.custom_tool_call_input.done" => {
                let output_index = event.get("output_index").and_then(Value::as_u64).map(|u| u as usize);
                if let Some(state_idx) = resolve_state_idx(
                    output_index,
                    &blocks_by_output_index,
                    fallback_block_idx,
                    &active_blocks,
                    |k| matches!(k, ActiveItemKind::ToolCall { .. }),
                ) {
                    let block_idx = active_blocks[state_idx].block_idx;
                    let raw = if event_type == "response.custom_tool_call_input.done" {
                        event.get("input").and_then(Value::as_str)
                    } else {
                        event.get("arguments").and_then(Value::as_str)
                    };
                    if let Some(raw_str) = raw {
                        let tool_name = match output.content.get(block_idx) {
                            Some(AssistantContentBlock::ToolCall(tc)) => tc.name.clone(),
                            _ => String::new(),
                        };
                        let custom_prop = grammar_props
                            .and_then(|m| m.get(&tool_name))
                            .map(String::as_str)
                            .unwrap_or("input");
                        let mut emitted_delta: Option<String> = None;
                        let parsed_args = if let ActiveItemKind::ToolCall {
                            partial_json,
                            is_custom,
                            grammar_buffer,
                            ..
                        } = &mut active_blocks[state_idx].kind
                        {
                            *partial_json = raw_str.to_string();
                            if *is_custom {
                                if let Some(buf) = grammar_buffer {
                                    if let Ok(d) =
                                        append_grammar_tool_input_json_delta(buf, custom_prop, partial_json, true)
                                    {
                                        emitted_delta = d;
                                    }
                                }
                            }
                            Some(parse_tool_call_args(
                                partial_json,
                                *is_custom,
                                &tool_name,
                                grammar_props,
                            ))
                        } else {
                            None
                        };
                        if let (Some(args), Some(AssistantContentBlock::ToolCall(tc))) =
                            (parsed_args, output.content.get_mut(block_idx))
                        {
                            tc.arguments = args;
                        }
                        if let Some(out_delta) = emitted_delta {
                            event_stream.push(AssistantMessageEvent::ToolCallDelta {
                                content_index: block_idx,
                                delta: out_delta,
                                partial: output.clone(),
                            });
                        }
                    }
                }
            }
            "response.output_item.done" => {
                let output_index = event.get("output_index").and_then(Value::as_u64).map(|u| u as usize);
                let Some(item) = event.get("item") else {
                    continue;
                };
                let Some(item_type) = item.get("type").and_then(Value::as_str) else {
                    continue;
                };

                match item_type {
                    "reasoning" => {
                        if let Some(state_idx) = resolve_state_idx(
                            output_index,
                            &blocks_by_output_index,
                            fallback_block_idx,
                            &active_blocks,
                            |k| matches!(k, ActiveItemKind::Reasoning),
                        ) {
                            let block_idx = active_blocks[state_idx].block_idx;
                            if let Some(AssistantContentBlock::Thinking(t)) = output.content.get_mut(block_idx) {
                                if t.thinking.is_empty() {
                                    if let Some(summary) = item.get("summary").and_then(Value::as_array) {
                                        let parts: Vec<&str> = summary
                                            .iter()
                                            .filter_map(|s| s.get("text").and_then(Value::as_str))
                                            .collect();
                                        if !parts.is_empty() {
                                            t.thinking = parts.join("\n\n");
                                        }
                                    }
                                }
                                t.thinking_signature = Some(pi_js::json::stringify(item));
                                let content_final = t.thinking.clone();
                                event_stream.push(AssistantMessageEvent::ThinkingEnd {
                                    content_index: block_idx,
                                    content: content_final,
                                    partial: output.clone(),
                                });
                            }
                        }
                    }
                    "message" => {
                        let phase = item.get("phase").and_then(Value::as_str);
                        if phase == Some("final_answer") {
                            output.stop_reason = StopReason::Stop;
                        }
                        if let Some(state_idx) = resolve_state_idx(
                            output_index,
                            &blocks_by_output_index,
                            fallback_block_idx,
                            &active_blocks,
                            |k| matches!(k, ActiveItemKind::Text),
                        ) {
                            let block_idx = active_blocks[state_idx].block_idx;
                            if let Some(AssistantContentBlock::Text(t)) = output.content.get_mut(block_idx) {
                                if t.text.is_empty() {
                                    if let Some(content_arr) = item.get("content").and_then(Value::as_array) {
                                        let mut collected = String::new();
                                        for c in content_arr {
                                            if let Some(txt) = c.get("text").and_then(Value::as_str) {
                                                collected.push_str(txt);
                                            } else if let Some(refusal) = c.get("refusal").and_then(Value::as_str) {
                                                collected.push_str(refusal);
                                            }
                                        }
                                        t.text = collected;
                                    }
                                }
                                if let Some(id) = item.get("id").and_then(Value::as_str) {
                                    t.text_signature = Some(encode_text_signature_v1(id, phase));
                                }
                                let content_final = t.text.clone();
                                event_stream.push(AssistantMessageEvent::TextEnd {
                                    content_index: block_idx,
                                    content: content_final,
                                    partial: output.clone(),
                                });
                            }
                        }
                    }
                    "function_call" | "custom_tool_call" => {
                        let done_item_id = item.get("id").and_then(Value::as_str);
                        let done_call_id = item.get("call_id").and_then(Value::as_str);
                        let state_idx = if let Some(oi) = output_index {
                            blocks_by_output_index.get(&oi).copied()
                        } else {
                            fallback_block_idx.filter(|&si| {
                                matches!(
                                    &active_blocks[si].kind,
                                    ActiveItemKind::ToolCall { item_id, call_id, .. }
                                    if done_item_id.map_or(true, |id| item_id.as_deref() == Some(id))
                                        && done_call_id.map_or(true, |cid| call_id == cid)
                                )
                            })
                        };
                        if let Some(si) = state_idx {
                            let block_idx = active_blocks[si].block_idx;
                            let namespace = item.get("namespace").and_then(Value::as_str).map(str::to_string);
                            let tool_name = match output.content.get(block_idx) {
                                Some(AssistantContentBlock::ToolCall(tc)) => tc.name.clone(),
                                _ => String::new(),
                            };
                            let final_args = if let ActiveItemKind::ToolCall {
                                partial_json,
                                is_custom,
                                done,
                                ..
                            } = &mut active_blocks[si].kind
                            {
                                *done = true;
                                let raw_str = if *is_custom {
                                    item.get("input")
                                        .and_then(Value::as_str)
                                        .unwrap_or(partial_json.as_str())
                                } else {
                                    item.get("arguments")
                                        .and_then(Value::as_str)
                                        .filter(|s| !s.is_empty())
                                        .unwrap_or(partial_json.as_str())
                                };
                                Some(parse_tool_call_args(raw_str, *is_custom, &tool_name, grammar_props))
                            } else {
                                None
                            };
                            if output_index.is_none() {
                                fallback_block_idx = None;
                            }
                            let mut finished_tc: Option<ToolCall> = None;
                            if let Some(AssistantContentBlock::ToolCall(tc)) = output.content.get_mut(block_idx) {
                                if let Some(args) = final_args {
                                    tc.arguments = args;
                                }
                                if let Some(ns) = namespace {
                                    tc.namespace = Some(ns);
                                }
                                finished_tc = Some(tc.clone());
                            }
                            if let Some(tool_call) = finished_tc {
                                event_stream.push(AssistantMessageEvent::ToolCallEnd {
                                    content_index: block_idx,
                                    tool_call,
                                    partial: output.clone(),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
            "response.completed" | "response.done" | "response.incomplete" => {
                saw_terminal_event = true;
                let Some(response) = event.get("response") else {
                    continue;
                };

                // Reject if any started tool call never received output_item.done
                for ab in &active_blocks {
                    if let ActiveItemKind::ToolCall { done: false, .. } = ab.kind {
                        if let Some(AssistantContentBlock::ToolCall(tc)) = output.content.get(ab.block_idx) {
                            return Err(Error::msg(format!(
                                "OpenAI Responses stream completed with an unfinished tool call: {} ({})",
                                tc.name, tc.id
                            )));
                        }
                    }
                }

                if let Some(id) = response.get("id").and_then(Value::as_str) {
                    output.response_id = Some(id.to_string());
                }

                // Backfill reasoning encrypted_content if omitted from output_item.done
                if let Some(resp_output) = response.get("output").and_then(Value::as_array) {
                    backfill_reasoning_encrypted_content(output, resp_output);
                }

                // Usage accounting
                if let Some(usage) = response.get("usage") {
                    let input_tokens = usage.get("input_tokens").and_then(Value::as_i64).unwrap_or(0);
                    let output_tokens = usage.get("output_tokens").and_then(Value::as_i64).unwrap_or(0);
                    let total_tokens = usage.get("total_tokens").and_then(Value::as_i64).unwrap_or(0);
                    let cached_tokens = usage
                        .get("input_tokens_details")
                        .and_then(|d| d.get("cached_tokens"))
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    let cache_write_tokens = usage
                        .get("input_tokens_details")
                        .and_then(|d| d.get("cache_write_tokens"))
                        .and_then(Value::as_i64)
                        .unwrap_or(0);

                    let net_input = input_tokens
                        .saturating_sub(cached_tokens)
                        .saturating_sub(cache_write_tokens);
                    output.usage.input = net_input;
                    output.usage.output = output_tokens;
                    output.usage.cache_read = cached_tokens;
                    output.usage.cache_write = cache_write_tokens;
                    output.usage.total_tokens = if total_tokens > 0 {
                        total_tokens
                    } else {
                        net_input + output_tokens + cached_tokens + cache_write_tokens
                    };

                    calculate_cost(&AnyModel::Chat(model.clone()), &mut output.usage);
                    let resp_tier = response.get("service_tier").and_then(Value::as_str);
                    let resolve_default = options.is_some_and(|o| o.resolve_service_tier_on_default);
                    let effective_tier = if resolve_default
                        && resp_tier == Some("default")
                        && matches!(requested_service_tier, Some("flex" | "priority"))
                    {
                        requested_service_tier
                    } else {
                        resp_tier.or(requested_service_tier)
                    };
                    apply_service_tier_pricing(&mut output.usage, effective_tier, Some(model));
                }

                if let Some(end_turn) = response.get("end_turn").and_then(Value::as_bool) {
                    output.end_turn = Some(end_turn);
                }

                let status =
                    response
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or(if event_type == "response.incomplete" {
                            "incomplete"
                        } else {
                            "completed"
                        });

                match status {
                    "completed" => {
                        output.raw_stop_reason = Some("completed".to_string());
                        let has_tool_call = output
                            .content
                            .iter()
                            .any(|b| matches!(b, AssistantContentBlock::ToolCall(_)));
                        output.stop_reason = if has_tool_call {
                            StopReason::ToolUse
                        } else {
                            StopReason::Stop
                        };
                    }
                    "incomplete" => {
                        let reason = response
                            .get("incomplete_details")
                            .and_then(|d| d.get("reason"))
                            .and_then(Value::as_str);
                        output.raw_stop_reason = Some(match reason {
                            Some(r) if !r.is_empty() => format!("incomplete.{r}"),
                            _ => "incomplete".to_string(),
                        });
                        if reason == Some("max_output_tokens") {
                            output.stop_reason = StopReason::Length;
                        } else {
                            output.stop_reason = StopReason::Error;
                            let detail = reason.unwrap_or("unknown");
                            output.error_message = Some(format!("Response incomplete: {detail}"));
                        }
                    }
                    "failed" => {
                        output.raw_stop_reason = Some("failed".to_string());
                        return Err(Error::msg(format_responses_error_object(response.get("error"))));
                    }
                    "cancelled" => {
                        output.raw_stop_reason = Some("cancelled".to_string());
                        output.stop_reason = StopReason::Aborted;
                    }
                    other => {
                        output.raw_stop_reason = Some(other.to_string());
                        output.stop_reason = if output
                            .content
                            .iter()
                            .any(|b| matches!(b, AssistantContentBlock::ToolCall(_)))
                        {
                            StopReason::ToolUse
                        } else {
                            StopReason::Stop
                        };
                    }
                }
                break;
            }
            "response.failed" => {
                output.raw_stop_reason = Some("failed".to_string());
                let err_obj = event.get("response").and_then(|r| r.get("error"));
                return Err(Error::msg(format_responses_error_object(err_obj)));
            }
            "error" => {
                let err_obj = event.get("error").or(Some(&event));
                return Err(Error::msg(format_responses_error_object(err_obj)));
            }
            _ => {}
        }
    }

    if !saw_terminal_event {
        return Err(Error::msg(
            "OpenAI Responses stream ended before a terminal response event",
        ));
    }

    Ok(())
}

fn resolve_state_idx(
    output_index: Option<usize>,
    by_output_index: &IndexMap<usize, usize>,
    fallback: Option<usize>,
    active_blocks: &[ActiveBlock],
    pred: impl Fn(&ActiveItemKind) -> bool,
) -> Option<usize> {
    if let Some(oi) = output_index {
        return by_output_index.get(&oi).copied();
    }
    if let Some(fb) = fallback {
        if pred(&active_blocks[fb].kind) {
            return Some(fb);
        }
    }
    active_blocks.iter().rposition(|ab| pred(&ab.kind))
}

fn backfill_reasoning_encrypted_content(output: &mut AssistantMessage, resp_output: &[Value]) {
    let mut encrypted_by_id: IndexMap<String, String> = IndexMap::new();
    for item in resp_output {
        if item.get("type").and_then(Value::as_str) != Some("reasoning") {
            continue;
        }
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        if let Some(enc) = item.get("encrypted_content").and_then(Value::as_str) {
            encrypted_by_id.insert(id.to_string(), enc.to_string());
        }
    }
    if encrypted_by_id.is_empty() {
        return;
    }
    for block in &mut output.content {
        if let AssistantContentBlock::Thinking(t) = block {
            let Some(sig) = t.thinking_signature.as_deref() else {
                continue;
            };
            if let Ok(Value::Object(mut obj)) = pi_js::json::parse(sig) {
                let has_encrypted = obj
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty());
                if !has_encrypted {
                    if let Some(enc) = obj
                        .get("id")
                        .and_then(Value::as_str)
                        .and_then(|id| encrypted_by_id.get(id))
                    {
                        obj.insert("encrypted_content".to_string(), Value::String(enc.clone()));
                        t.thinking_signature = Some(pi_js::json::stringify(&Value::Object(obj)));
                    }
                }
            }
        }
    }
}

fn format_responses_error_object(err_val: Option<&Value>) -> String {
    let Some(val) = err_val else {
        return "OpenAI Responses error".to_string();
    };
    let code = val.get("code").and_then(Value::as_str);
    let message = val.get("message").and_then(Value::as_str);
    let base = match (code, message) {
        (Some(c), Some(m)) if !c.is_empty() && !m.is_empty() => format!("{c}: {m}"),
        (Some(c), _) if !c.is_empty() => c.to_string(),
        (_, Some(m)) if !m.is_empty() => m.to_string(),
        _ => pi_js::json::stringify(val),
    };
    enrich_chatgpt_usage_limit_error(&base)
}

struct SseReadState {
    body_stream: BodyStream,
    model: Model,
    on_event: Option<Arc<dyn Fn(&Value, &Model) -> BoxFuture<Result<()>> + Send + Sync>>,
    buffer: String,
    pending_utf8: Vec<u8>,
    event_data_lines: Vec<String>,
    queued: VecDeque<Value>,
    finished: bool,
}

fn drain_sse_buffer(
    buffer: &mut String,
    event_data_lines: &mut Vec<String>,
    queued: &mut VecDeque<Value>,
    flush_eof: bool,
) -> Result<()> {
    while let Some(newline_pos) = buffer.find('\n') {
        let mut line = buffer[..newline_pos].to_string();
        buffer.drain(..=newline_pos);
        if line.ends_with('\r') {
            line.pop();
        }
        if line.is_empty() {
            if !event_data_lines.is_empty() {
                let data = event_data_lines.join("\n");
                event_data_lines.clear();
                if let Some(parsed) = parse_sse_json_payload(&data)? {
                    queued.push_back(parsed);
                }
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let payload = rest.strip_prefix(' ').unwrap_or(rest);
            event_data_lines.push(payload.to_string());
        }
    }
    if flush_eof {
        if !buffer.is_empty() {
            let tail = std::mem::take(buffer);
            for raw_line in tail.lines() {
                let line = raw_line.trim_end_matches('\r');
                if line.starts_with(':') {
                    continue;
                }
                if let Some(rest) = line.strip_prefix("data:") {
                    let payload = rest.strip_prefix(' ').unwrap_or(rest);
                    event_data_lines.push(payload.to_string());
                }
            }
        }
        if !event_data_lines.is_empty() {
            let data = event_data_lines.join("\n");
            event_data_lines.clear();
            if let Some(parsed) = parse_sse_json_payload(&data)? {
                queued.push_back(parsed);
            }
        }
    }
    Ok(())
}

/// Decode an HTTP `Response` SSE body into a stream of parsed JSON events, invoking
/// `on_provider_stream_event` for each event if configured.
pub fn response_to_sse_json_stream(
    response: Response,
    model: Model,
    on_event: Option<Arc<dyn Fn(&Value, &Model) -> BoxFuture<Result<()>> + Send + Sync>>,
) -> Result<ResponsesEventJsonStream> {
    let body_stream = response.body_stream()?;
    let initial = SseReadState {
        body_stream,
        model,
        on_event,
        buffer: String::new(),
        pending_utf8: Vec::new(),
        event_data_lines: Vec::new(),
        queued: VecDeque::new(),
        finished: false,
    };

    let stream = futures::stream::unfold(initial, |mut state| async move {
        loop {
            if let Some(parsed) = state.queued.pop_front() {
                if let Some(cb) = &state.on_event {
                    if let Err(err) = cb(&parsed, &state.model).await {
                        state.finished = true;
                        return Some((Err(err), state));
                    }
                }
                return Some((Ok(parsed), state));
            }
            if state.finished {
                return None;
            }
            match state.body_stream.next().await {
                Some(Ok(chunk)) => {
                    let text = decode_utf8_chunk(&mut state.pending_utf8, chunk.as_ref(), false);
                    state.buffer.push_str(&text);
                    if let Err(err) =
                        drain_sse_buffer(&mut state.buffer, &mut state.event_data_lines, &mut state.queued, false)
                    {
                        state.finished = true;
                        return Some((Err(err), state));
                    }
                }
                Some(Err(err)) => {
                    state.finished = true;
                    return Some((Err(err), state));
                }
                None => {
                    let tail = decode_utf8_chunk(&mut state.pending_utf8, &[], true);
                    state.buffer.push_str(&tail);
                    state.finished = true;
                    if let Err(err) =
                        drain_sse_buffer(&mut state.buffer, &mut state.event_data_lines, &mut state.queued, true)
                    {
                        return Some((Err(err), state));
                    }
                }
            }
        }
    });

    Ok(Box::pin(stream))
}

fn parse_sse_json_payload(data: &str) -> Result<Option<Value>> {
    let trimmed = pi_js::str16::trim(data);
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return Ok(None);
    }
    let parsed = pi_js::json::parse(trimmed)
        .map_err(|e| Error::msg(format!("Failed to parse Responses SSE event JSON: {e}")))?;
    Ok(Some(parsed))
}

fn decode_utf8_chunk(pending: &mut Vec<u8>, bytes: &[u8], flush: bool) -> String {
    pending.extend_from_slice(bytes);
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
                } else {
                    if flush && !pending.is_empty() {
                        out.push('\u{FFFD}');
                        pending.clear();
                    }
                    break;
                }
            }
        }
    }
    out
}

/// Apply `CacheRetention` resolution respecting `PI_CACHE_RETENTION` env override.
pub fn resolve_responses_cache_retention(
    cache_retention: Option<crate::types::CacheRetention>,
    env: Option<&crate::types::ProviderEnv>,
) -> crate::types::CacheRetention {
    if let Some(explicit) = cache_retention {
        return explicit;
    }
    if let Some(val) = crate::utils::provider_env::get_provider_env_value("PI_CACHE_RETENTION", env) {
        if val.eq_ignore_ascii_case("long") {
            return crate::types::CacheRetention::Long;
        }
        if val.eq_ignore_ascii_case("none") {
            return crate::types::CacheRetention::None;
        }
    }
    crate::types::CacheRetention::Short
}

/// Resolve `prompt_cache_retention` field value for OpenAI APIs (`"24h"` for Long when on official OpenAI host).
pub fn get_prompt_cache_retention(
    base_url: &str,
    cache_retention: crate::types::CacheRetention,
) -> Option<&'static str> {
    if cache_retention != crate::types::CacheRetention::Long {
        return None;
    }
    if base_url.contains("api.openai.com") {
        Some("24h")
    } else {
        None
    }
}

fn parse_tool_call_args(
    raw: &str,
    is_custom: bool,
    tool_name: &str,
    grammar_props: Option<&IndexMap<String, String>>,
) -> IndexMap<String, Value> {
    if !is_custom {
        parse_streaming_json(Some(raw))
    } else {
        let prop = grammar_props
            .and_then(|m| m.get(tool_name))
            .map(String::as_str)
            .unwrap_or("input");
        let mut map = IndexMap::new();
        map.insert(prop.to_string(), Value::String(raw.to_string()));
        map
    }
}

/// Create a pending initial `AssistantMessage` matching `model`.
pub fn create_pending_assistant_message(model: &Model) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Pending,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
    }
}
