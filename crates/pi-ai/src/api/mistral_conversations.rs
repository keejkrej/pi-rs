//! Port of packages/ai/src/api/mistral-conversations.ts

use std::cell::RefCell;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use futures::StreamExt;
use indexmap::IndexMap;
use pi_js::abort::AbortSignal;
use pi_js::fetch::{Body, Headers, Request};
use pi_js::{Error, Result};
use serde::de::{self, Deserializer};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};
use url::Url;

use crate::api::constrained_sampling::{get_json_schema_tool_parameters, resolve_json_schema_strict_sampling};
use crate::api::lazy::lazy_stream;
use crate::api::simple_options::build_base_options;
use crate::api::transform_messages::transform_messages;
use crate::models::{calculate_cost, clamp_thinking_level};
use crate::types::{
    ApiStreamOptions, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, CacheRetention, JsonObject,
    JsonValue, Message, Model, ModelInput, ModelThinkingLevel, ProviderHeaders, ProviderResponse, ProviderStreams,
    SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingContent, Tool, ToolCall, ToolChoice,
    TranscriptContext, Usage, UsageCost, UserContentBlock, UserMessageContent,
};
use crate::utils::error_body::{MAX_PROVIDER_ERROR_BODY_CHARS, truncate_error_text};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::hash::short_hash;
use crate::utils::headers::headers_to_record;
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::pi_user_agent::get_pi_user_agent;
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::text::{get_system_message_text, render_system_message_update};
use crate::utils::transcript::{get_current_tools, resolve_transcript};

const MISTRAL_TOOL_CALL_ID_LENGTH: usize = 9;
const DEFAULT_MISTRAL_TIMEOUT_MS: u64 = 60_000;

fn to_json_value<T: Serialize + ?Sized>(value: &T) -> JsonValue {
    let serialized = pi_js::json::stringify(value);
    pi_js::json::parse(&serialized).unwrap_or(JsonValue::Null)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MistralReasoningEffort {
    None,
    Low,
    Medium,
    High,
    Max,
}

impl MistralReasoningEffort {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    pub fn from_str_lossy(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "max" => Some(Self::Max),
            _ => None,
        }
    }
}

impl fmt::Display for MistralReasoningEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MistralPromptMode {
    Reasoning,
}

impl MistralPromptMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reasoning => "reasoning",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MistralToolChoice {
    Auto,
    None,
    Any,
    Required,
    Function { name: String },
}

impl From<ToolChoice> for MistralToolChoice {
    fn from(value: ToolChoice) -> Self {
        match value {
            ToolChoice::Auto => Self::Auto,
            ToolChoice::None => Self::None,
        }
    }
}

impl Serialize for MistralToolChoice {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Auto => serializer.serialize_str("auto"),
            Self::None => serializer.serialize_str("none"),
            Self::Any => serializer.serialize_str("any"),
            Self::Required => serializer.serialize_str("required"),
            Self::Function { name } => {
                let mut fn_obj = Map::new();
                fn_obj.insert("name".to_owned(), Value::String(name.clone()));
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "function")?;
                map.serialize_entry("function", &Value::Object(fn_obj))?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for MistralToolChoice {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let val = Value::deserialize(deserializer)?;
        match &val {
            Value::String(s) => match s.as_str() {
                "auto" => Ok(Self::Auto),
                "none" => Ok(Self::None),
                "any" => Ok(Self::Any),
                "required" => Ok(Self::Required),
                other => Err(de::Error::custom(format!("unknown mistral toolChoice: {other}"))),
            },
            Value::Object(obj) => {
                if obj.get("type").and_then(Value::as_str) != Some("function") {
                    return Err(de::Error::custom(
                        "mistral toolChoice object must have type \"function\"",
                    ));
                }
                let name = obj
                    .get("function")
                    .and_then(Value::as_object)
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| de::Error::missing_field("function.name"))?;
                Ok(Self::Function { name: name.to_owned() })
            }
            _ => Err(de::Error::custom(
                "mistral toolChoice must be a string or function object",
            )),
        }
    }
}

/// Provider-specific options for the Mistral API.
#[derive(Clone, Default, Debug)]
pub struct MistralOptions {
    pub base: StreamOptions,
    pub tool_choice: Option<MistralToolChoice>,
    pub prompt_mode: Option<MistralPromptMode>,
    pub reasoning_effort: Option<MistralReasoningEffort>,
}

impl Deref for MistralOptions {
    type Target = StreamOptions;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for MistralOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl From<StreamOptions> for MistralOptions {
    fn from(base: StreamOptions) -> Self {
        Self {
            base,
            ..Default::default()
        }
    }
}

impl From<MistralOptions> for ApiStreamOptions {
    fn from(options: MistralOptions) -> Self {
        let mut extra = JsonObject::new();
        if let Some(tool_choice) = &options.tool_choice {
            extra.insert("toolChoice".to_owned(), to_json_value(tool_choice));
        }
        if let Some(prompt_mode) = options.prompt_mode {
            extra.insert("promptMode".to_owned(), Value::String(prompt_mode.as_str().to_owned()));
        }
        if let Some(effort) = options.reasoning_effort {
            extra.insert("reasoningEffort".to_owned(), Value::String(effort.as_str().to_owned()));
        }
        Self {
            base: options.base,
            extra,
            extension: None,
        }
    }
}

impl MistralOptions {
    pub fn from_api_options(options: Option<ApiStreamOptions>) -> Self {
        let Some(opts) = options else {
            return Self::default();
        };
        let tool_choice = opts
            .extra
            .get("toolChoice")
            .cloned()
            .and_then(|v| serde_json::from_value::<MistralToolChoice>(v).ok());
        let prompt_mode = opts
            .extra
            .get("promptMode")
            .cloned()
            .and_then(|v| serde_json::from_value::<MistralPromptMode>(v).ok());
        let reasoning_effort = opts
            .extra
            .get("reasoningEffort")
            .cloned()
            .and_then(|v| serde_json::from_value::<MistralReasoningEffort>(v).ok());
        Self {
            base: opts.base,
            tool_choice,
            prompt_mode,
            reasoning_effort,
        }
    }
}

#[derive(Debug)]
struct MistralHttpError {
    status_code: u16,
    body: String,
    status_text: String,
}

impl MistralHttpError {
    fn new(status_code: u16, body: String, status_text: String) -> Self {
        Self {
            status_code,
            body,
            status_text,
        }
    }

    fn into_js_error(self) -> Error {
        Error::typed("MistralHttpError", self)
    }
}

impl fmt::Display for MistralHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.status_text.is_empty() {
            write!(f, "Request failed with status {}", self.status_code)
        } else {
            f.write_str(&self.status_text)
        }
    }
}

impl std::error::Error for MistralHttpError {}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ToolBlockKey {
    Index(i64),
    Id(String),
}

struct MistralToolCallIdNormalizer {
    id_map: IndexMap<String, String>,
    reverse_map: IndexMap<String, String>,
}

impl MistralToolCallIdNormalizer {
    fn new() -> Self {
        Self {
            id_map: IndexMap::new(),
            reverse_map: IndexMap::new(),
        }
    }

    fn normalize(&mut self, id: &str) -> String {
        if let Some(existing) = self.id_map.get(id) {
            return existing.clone();
        }

        let mut attempt = 0_u64;
        loop {
            let candidate = derive_mistral_tool_call_id(id, attempt);
            match self.reverse_map.get(&candidate) {
                Some(owner) if owner != id => {
                    attempt += 1;
                }
                _ => {
                    self.id_map.insert(id.to_owned(), candidate.clone());
                    self.reverse_map.insert(candidate.clone(), id.to_owned());
                    return candidate;
                }
            }
        }
    }
}

fn derive_mistral_tool_call_id(id: &str, attempt: u64) -> String {
    let normalized: String = id.chars().filter(char::is_ascii_alphanumeric).collect();
    if attempt == 0 && normalized.len() == MISTRAL_TOOL_CALL_ID_LENGTH {
        return normalized;
    }
    let seed_base = if normalized.is_empty() { id } else { &normalized };
    let seed = if attempt == 0 {
        seed_base.to_owned()
    } else {
        format!("{seed_base}:{attempt}")
    };
    short_hash(&seed)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(MISTRAL_TOOL_CALL_ID_LENGTH)
        .collect()
}

fn format_mistral_error(error: &Error) -> String {
    if let Some(http) = error.downcast_ref::<MistralHttpError>() {
        let trimmed = pi_js::str16::trim(&http.body);
        if !trimmed.is_empty() {
            return format!(
                "Mistral API error ({}): {}",
                http.status_code,
                truncate_error_text(trimmed, MAX_PROVIDER_ERROR_BODY_CHARS)
            );
        }
        return format!("Mistral API error ({}): {}", http.status_code, http);
    }
    error.to_string()
}

fn should_use_prompt_caching(options: &MistralOptions) -> bool {
    options.base.cache_retention != Some(CacheRetention::None)
        && options.base.session_id.as_deref().is_some_and(|s| !s.is_empty())
}

fn has_mistral_header_override(overrides: Option<&ProviderHeaders>, target_lower: &str) -> bool {
    overrides.is_some_and(|hdrs| hdrs.keys().any(|name| name.to_ascii_lowercase() == target_lower))
}

fn has_model_header_override(overrides: Option<&IndexMap<String, String>>, target_lower: &str) -> bool {
    overrides.is_some_and(|hdrs| hdrs.keys().any(|name| name.to_ascii_lowercase() == target_lower))
}

fn apply_mistral_model_headers(headers: &mut Headers, overrides: Option<&IndexMap<String, String>>) {
    let Some(hdrs) = overrides else {
        return;
    };
    for (name, value) in hdrs {
        headers.set(name, value);
    }
}

fn apply_mistral_option_headers(headers: &mut Headers, overrides: Option<&ProviderHeaders>) {
    let Some(hdrs) = overrides else {
        return;
    };
    for (name, value) in hdrs {
        match value {
            None => headers.delete(name),
            Some(v) => headers.set(name, v),
        }
    }
}

fn build_mistral_headers(model: &Model, api_key: &str, options: &MistralOptions) -> Headers {
    let mut headers = Headers::new();
    headers.set("User-Agent", &get_pi_user_agent());
    headers.set("accept", "text/event-stream");
    headers.set("authorization", &format!("Bearer {api_key}"));
    headers.set("content-type", "application/json");

    apply_mistral_model_headers(&mut headers, model.headers.as_ref());
    apply_mistral_option_headers(&mut headers, options.base.headers.as_ref());

    let has_explicit_affinity = has_model_header_override(model.headers.as_ref(), "x-affinity")
        || has_mistral_header_override(options.base.headers.as_ref(), "x-affinity");
    if should_use_prompt_caching(options)
        && !has_explicit_affinity
        && let Some(session_id) = &options.base.session_id
    {
        headers.set("x-affinity", session_id);
    }

    headers
}

// PORT: Rust `TSchema::to_value()` produces clean `serde_json::Value` maps with no JS symbol
// keys, so cloning/normalizing via JSON value already guarantees plain object keys.
fn strip_symbol_keys(value: &JsonValue) -> JsonValue {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(strip_symbol_keys).collect()),
        Value::Object(obj) => {
            let mut out = Map::new();
            for (k, v) in obj {
                out.insert(k.clone(), strip_symbol_keys(v));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn to_function_tools(tools: &[Tool]) -> Result<Vec<JsonValue>> {
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        let strict = resolve_json_schema_strict_sampling(tool, true, None)?;
        let parameters = get_json_schema_tool_parameters(tool, strict)?;
        let clean_params = strip_symbol_keys(&parameters.to_value());

        let mut fn_obj = Map::new();
        fn_obj.insert("name".to_owned(), Value::String(tool.name.clone()));
        fn_obj.insert("description".to_owned(), Value::String(tool.description.clone()));
        fn_obj.insert("parameters".to_owned(), clean_params);
        fn_obj.insert("strict".to_owned(), Value::Bool(strict.unwrap_or(false)));

        let mut wrapper = Map::new();
        wrapper.insert("type".to_owned(), Value::String("function".to_owned()));
        wrapper.insert("function".to_owned(), Value::Object(fn_obj));
        out.push(Value::Object(wrapper));
    }
    Ok(out)
}

fn build_tool_result_text(text: &str, has_images: bool, supports_images: bool, is_error: bool) -> String {
    let trimmed = pi_js::str16::trim(text);
    let error_prefix = if is_error { "[tool error] " } else { "" };

    if !trimmed.is_empty() {
        let image_suffix = if has_images && !supports_images {
            "\n[tool image omitted: model does not support images]"
        } else {
            ""
        };
        return format!("{error_prefix}{trimmed}{image_suffix}");
    }

    if has_images {
        if supports_images {
            return if is_error {
                "[tool error] (see attached image)".to_owned()
            } else {
                "(see attached image)".to_owned()
            };
        }
        return if is_error {
            "[tool error] (image omitted: model does not support images)".to_owned()
        } else {
            "(image omitted: model does not support images)".to_owned()
        };
    }

    if is_error {
        "[tool error] (no tool output)".to_owned()
    } else {
        "(no tool output)".to_owned()
    }
}

fn make_text_chunk(text: String) -> JsonValue {
    let mut obj = Map::new();
    obj.insert("type".to_owned(), Value::String("text".to_owned()));
    obj.insert("text".to_owned(), Value::String(text));
    Value::Object(obj)
}

fn make_image_chunk(mime_type: &str, data: &str) -> JsonValue {
    let mut obj = Map::new();
    obj.insert("type".to_owned(), Value::String("image_url".to_owned()));
    obj.insert(
        "imageUrl".to_owned(),
        Value::String(format!("data:{mime_type};base64,{data}")),
    );
    Value::Object(obj)
}

fn make_thinking_chunk(text: String) -> JsonValue {
    let mut obj = Map::new();
    obj.insert("type".to_owned(), Value::String("thinking".to_owned()));
    obj.insert("thinking".to_owned(), Value::Array(vec![make_text_chunk(text)]));
    Value::Object(obj)
}

fn to_chat_messages(messages: &[Message], supports_images: bool) -> Vec<JsonValue> {
    let mut result = Vec::new();

    for (index, msg) in messages.iter().enumerate() {
        match msg {
            Message::System(sys) => {
                let text = if index == 0 {
                    get_system_message_text(sys)
                } else {
                    render_system_message_update(sys)
                };
                if !text.is_empty() {
                    let mut m = Map::new();
                    m.insert("role".to_owned(), Value::String("system".to_owned()));
                    m.insert("content".to_owned(), Value::String(sanitize_surrogates(&text)));
                    result.push(Value::Object(m));
                }
            }
            Message::User(user) => match &user.content {
                UserMessageContent::Text(text) => {
                    let mut m = Map::new();
                    m.insert("role".to_owned(), Value::String("user".to_owned()));
                    m.insert("content".to_owned(), Value::String(sanitize_surrogates(text)));
                    result.push(Value::Object(m));
                }
                UserMessageContent::Blocks(items) => {
                    let had_images = items.iter().any(|item| matches!(item, UserContentBlock::Image(_)));
                    let mut chunks = Vec::new();
                    for item in items {
                        match item {
                            UserContentBlock::Text(t) => {
                                chunks.push(make_text_chunk(sanitize_surrogates(&t.text)));
                            }
                            UserContentBlock::Image(img) if supports_images => {
                                chunks.push(make_image_chunk(&img.mime_type, &img.data));
                            }
                            UserContentBlock::Image(_) => {}
                        }
                    }
                    if !chunks.is_empty() {
                        let mut m = Map::new();
                        m.insert("role".to_owned(), Value::String("user".to_owned()));
                        m.insert("content".to_owned(), Value::Array(chunks));
                        result.push(Value::Object(m));
                    } else if had_images && !supports_images {
                        let mut m = Map::new();
                        m.insert("role".to_owned(), Value::String("user".to_owned()));
                        m.insert(
                            "content".to_owned(),
                            Value::String("(image omitted: model does not support images)".to_owned()),
                        );
                        result.push(Value::Object(m));
                    }
                }
            },
            Message::Assistant(assistant) => {
                let mut content_parts = Vec::new();
                let mut tool_calls = Vec::new();

                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Text(t) => {
                            if !pi_js::str16::trim(&t.text).is_empty() {
                                content_parts.push(make_text_chunk(sanitize_surrogates(&t.text)));
                            }
                        }
                        AssistantContentBlock::Thinking(th) => {
                            if !pi_js::str16::trim(&th.thinking).is_empty() {
                                content_parts.push(make_thinking_chunk(sanitize_surrogates(&th.thinking)));
                            }
                        }
                        AssistantContentBlock::ToolCall(tc) => {
                            let mut fn_obj = Map::new();
                            fn_obj.insert("name".to_owned(), Value::String(tc.name.clone()));
                            fn_obj.insert(
                                "arguments".to_owned(),
                                Value::String(pi_js::json::stringify(&tc.arguments)),
                            );
                            let mut tc_obj = Map::new();
                            tc_obj.insert("id".to_owned(), Value::String(tc.id.clone()));
                            tc_obj.insert("type".to_owned(), Value::String("function".to_owned()));
                            tc_obj.insert("function".to_owned(), Value::Object(fn_obj));
                            tc_obj.insert("index".to_owned(), Value::from(0));
                            tool_calls.push(Value::Object(tc_obj));
                        }
                    }
                }

                if !content_parts.is_empty() || !tool_calls.is_empty() {
                    let mut m = Map::new();
                    m.insert("role".to_owned(), Value::String("assistant".to_owned()));
                    m.insert("prefix".to_owned(), Value::Bool(false));
                    if !content_parts.is_empty() {
                        m.insert("content".to_owned(), Value::Array(content_parts));
                    }
                    if !tool_calls.is_empty() {
                        m.insert("toolCalls".to_owned(), Value::Array(tool_calls));
                    }
                    result.push(Value::Object(m));
                }
            }
            Message::ToolResult(tr) => {
                let text_result = tr
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        UserContentBlock::Text(t) => Some(sanitize_surrogates(&t.text)),
                        UserContentBlock::Image(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let has_images = tr.content.iter().any(|part| matches!(part, UserContentBlock::Image(_)));
                let tool_text = build_tool_result_text(&text_result, has_images, supports_images, tr.is_error);

                let mut tool_content = vec![make_text_chunk(tool_text)];
                if supports_images {
                    for part in &tr.content {
                        if let UserContentBlock::Image(img) = part {
                            tool_content.push(make_image_chunk(&img.mime_type, &img.data));
                        }
                    }
                }

                let mut m = Map::new();
                m.insert("role".to_owned(), Value::String("tool".to_owned()));
                m.insert("toolCallId".to_owned(), Value::String(tr.tool_call_id.clone()));
                m.insert("name".to_owned(), Value::String(tr.tool_name.clone()));
                m.insert("content".to_owned(), Value::Array(tool_content));
                result.push(Value::Object(m));
            }
        }
    }

    result
}

fn build_chat_payload(
    model: &Model,
    context: &TranscriptContext,
    transformed_messages: &[Message],
    options: &MistralOptions,
) -> Result<JsonValue> {
    let supports_images = model.input.contains(&ModelInput::Image);
    let mut payload = Map::new();
    payload.insert("model".to_owned(), Value::String(model.id.clone()));
    payload.insert("stream".to_owned(), Value::Bool(true));
    payload.insert(
        "messages".to_owned(),
        Value::Array(to_chat_messages(transformed_messages, supports_images)),
    );

    let current_tools = get_current_tools(&context.messages);
    if !current_tools.is_empty() {
        payload.insert("tools".to_owned(), Value::Array(to_function_tools(&current_tools)?));
    }
    if let Some(temp) = options.base.temperature {
        payload.insert("temperature".to_owned(), pi_js::json::number_value(temp));
    }
    if let Some(max_tokens) = options.base.max_tokens {
        payload.insert("maxTokens".to_owned(), Value::from(max_tokens));
    }
    if let Some(tool_choice) = &options.tool_choice {
        payload.insert("toolChoice".to_owned(), to_json_value(tool_choice));
    }
    if let Some(prompt_mode) = options.prompt_mode {
        payload.insert("promptMode".to_owned(), Value::String(prompt_mode.as_str().to_owned()));
    }
    if let Some(effort) = options.reasoning_effort {
        payload.insert("reasoningEffort".to_owned(), Value::String(effort.as_str().to_owned()));
    }
    if should_use_prompt_caching(options)
        && let Some(session_id) = &options.base.session_id
    {
        payload.insert("promptCacheKey".to_owned(), Value::String(session_id.clone()));
    }

    Ok(Value::Object(payload))
}

fn remap_mistral_property(record: &mut Map<String, Value>, source: &str, target: &str) {
    if let Some(val) = record.shift_remove(source) {
        record.insert(target.to_owned(), val);
    }
}

fn to_mistral_wire_content_chunk(chunk: Value) -> Value {
    let Value::Object(mut wire) = chunk else {
        return chunk;
    };
    for (source, target) in [
        ("imageUrl", "image_url"),
        ("documentUrl", "document_url"),
        ("documentName", "document_name"),
        ("fileId", "file_id"),
        ("referenceIds", "reference_ids"),
        ("inputAudio", "input_audio"),
    ] {
        remap_mistral_property(&mut wire, source, target);
    }
    Value::Object(wire)
}

fn to_mistral_wire_message(message: Value) -> Value {
    let Value::Object(mut wire) = message else {
        return message;
    };
    remap_mistral_property(&mut wire, "toolCalls", "tool_calls");
    remap_mistral_property(&mut wire, "toolCallId", "tool_call_id");
    if let Some(Value::Array(chunks)) = wire.get("content").cloned() {
        wire.insert(
            "content".to_owned(),
            Value::Array(chunks.into_iter().map(to_mistral_wire_content_chunk).collect()),
        );
    }
    Value::Object(wire)
}

fn to_mistral_wire_payload(payload: JsonValue) -> JsonValue {
    let Value::Object(mut wire) = payload else {
        return payload;
    };
    for (source, target) in [
        ("topP", "top_p"),
        ("maxTokens", "max_tokens"),
        ("randomSeed", "random_seed"),
        ("responseFormat", "response_format"),
        ("toolChoice", "tool_choice"),
        ("presencePenalty", "presence_penalty"),
        ("frequencyPenalty", "frequency_penalty"),
        ("parallelToolCalls", "parallel_tool_calls"),
        ("reasoningEffort", "reasoning_effort"),
        ("promptMode", "prompt_mode"),
        ("promptCacheKey", "prompt_cache_key"),
        ("safePrompt", "safe_prompt"),
    ] {
        remap_mistral_property(&mut wire, source, target);
    }

    if let Some(Value::Array(messages)) = wire.get("messages").cloned() {
        wire.insert(
            "messages".to_owned(),
            Value::Array(messages.into_iter().map(to_mistral_wire_message).collect()),
        );
    }

    if let Some(Value::Object(mut response_format)) = wire.get("response_format").cloned() {
        remap_mistral_property(&mut response_format, "jsonSchema", "json_schema");
        if let Some(Value::Object(mut json_schema)) = response_format.get("json_schema").cloned() {
            remap_mistral_property(&mut json_schema, "schemaDefinition", "schema");
            response_format.insert("json_schema".to_owned(), Value::Object(json_schema));
        }
        wire.insert("response_format".to_owned(), Value::Object(response_format));
    }

    Value::Object(wire)
}

fn build_mistral_completions_url(base_url_str: &str) -> Result<String> {
    let mut base_url = Url::parse(base_url_str).map_err(|e| Error::msg(format!("Invalid Mistral baseUrl: {e}")))?;
    let trimmed = base_url.path().trim_end_matches('/');
    base_url.set_path(&format!("{trimmed}/"));
    let url = base_url
        .join("v1/chat/completions")
        .map_err(|e| Error::msg(format!("Invalid Mistral endpoint URL: {e}")))?;
    Ok(url.to_string())
}

fn get_mistral_cached_prompt_tokens(usage: &Map<String, Value>, prompt_tokens: i64) -> i64 {
    let raw = usage
        .get("promptTokensDetails")
        .and_then(Value::as_object)
        .and_then(|o| o.get("cachedTokens"))
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(Value::as_object)
                .and_then(|o| o.get("cached_tokens"))
        })
        .or_else(|| {
            usage
                .get("promptTokenDetails")
                .and_then(Value::as_object)
                .and_then(|o| o.get("cachedTokens"))
        })
        .or_else(|| {
            usage
                .get("prompt_token_details")
                .and_then(Value::as_object)
                .and_then(|o| o.get("cached_tokens"))
        })
        .or_else(|| usage.get("numCachedTokens"))
        .or_else(|| usage.get("num_cached_tokens"));

    let cached_f64 = raw.and_then(Value::as_f64).unwrap_or(0.0);
    if !cached_f64.is_finite() {
        return 0;
    }
    let clamped = cached_f64.max(0.0).min(prompt_tokens.max(0) as f64);
    clamped as i64
}

fn map_chat_stop_reason(reason: Option<&str>) -> (StopReason, Option<String>) {
    let Some(r) = reason else {
        return (StopReason::Stop, None);
    };
    match r {
        "stop" => (StopReason::Stop, None),
        "length" | "model_length" => (StopReason::Length, None),
        "tool_calls" => (StopReason::ToolUse, None),
        "error" => (StopReason::Error, Some("Provider stopped with: error".to_owned())),
        other => (StopReason::Error, Some(format!("Provider stopped with: {other}"))),
    }
}

enum ActiveMistralBlock {
    Text(usize),
    Thinking(usize),
}

fn flush_active_block(
    active: &mut Option<ActiveMistralBlock>,
    output: &AssistantMessage,
    stream: &AssistantMessageEventStream,
) {
    let Some(block_ref) = active.take() else {
        return;
    };
    match block_ref {
        ActiveMistralBlock::Text(idx) => {
            if let Some(AssistantContentBlock::Text(t)) = output.content.get(idx) {
                stream.push(AssistantMessageEvent::TextEnd {
                    content_index: idx,
                    content: t.text.clone(),
                    partial: output.clone(),
                });
            }
        }
        ActiveMistralBlock::Thinking(idx) => {
            if let Some(AssistantContentBlock::Thinking(th)) = output.content.get(idx) {
                stream.push(AssistantMessageEvent::ThinkingEnd {
                    content_index: idx,
                    content: th.thinking.clone(),
                    partial: output.clone(),
                });
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MistralDeltaContent {
    Text(String),
    Chunks(Vec<MistralStreamContentChunk>),
}

#[derive(Debug, Default, Deserialize)]
struct MistralStreamThinkingPart {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct MistralStreamContentChunk {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thinking: Option<Vec<MistralStreamThinkingPart>>,
}

#[derive(Debug, Default, Deserialize)]
struct MistralStreamToolCallFunction {
    #[serde(default)]
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
struct MistralStreamToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    index: Option<i64>,
    function: MistralStreamToolCallFunction,
}

#[derive(Debug, Default, Deserialize)]
struct MistralStreamDelta {
    #[serde(default)]
    content: Option<MistralDeltaContent>,
    #[serde(default)]
    tool_calls: Option<Vec<MistralStreamToolCall>>,
}

#[derive(Debug, Default, Deserialize)]
struct MistralStreamChoice {
    #[serde(default)]
    finish_reason: Option<String>,
    #[serde(default)]
    delta: MistralStreamDelta,
}

#[derive(Debug, Deserialize)]
struct MistralCompletionChunk {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    usage: Option<Map<String, Value>>,
    choices: Vec<MistralStreamChoice>,
}

fn push_text_delta(
    text_delta: String,
    active_block: &mut Option<ActiveMistralBlock>,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) {
    if text_delta.is_empty() {
        return;
    }
    if !matches!(active_block, Some(ActiveMistralBlock::Text(_))) {
        flush_active_block(active_block, output, stream);
        let idx = output.content.len();
        output.content.push(AssistantContentBlock::Text(TextContent::new("")));
        *active_block = Some(ActiveMistralBlock::Text(idx));
        stream.push(AssistantMessageEvent::TextStart {
            content_index: idx,
            partial: output.clone(),
        });
    }
    if let Some(ActiveMistralBlock::Text(idx)) = *active_block {
        if let Some(AssistantContentBlock::Text(t)) = output.content.get_mut(idx) {
            t.text.push_str(&text_delta);
        }
        stream.push(AssistantMessageEvent::TextDelta {
            content_index: idx,
            delta: text_delta,
            partial: output.clone(),
        });
    }
}

fn push_thinking_delta(
    thinking_delta: String,
    active_block: &mut Option<ActiveMistralBlock>,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) {
    if thinking_delta.is_empty() {
        return;
    }
    if !matches!(active_block, Some(ActiveMistralBlock::Thinking(_))) {
        flush_active_block(active_block, output, stream);
        let idx = output.content.len();
        output
            .content
            .push(AssistantContentBlock::Thinking(ThinkingContent::new("")));
        *active_block = Some(ActiveMistralBlock::Thinking(idx));
        stream.push(AssistantMessageEvent::ThinkingStart {
            content_index: idx,
            partial: output.clone(),
        });
    }
    if let Some(ActiveMistralBlock::Thinking(idx)) = *active_block {
        if let Some(AssistantContentBlock::Thinking(th)) = output.content.get_mut(idx) {
            th.thinking.push_str(&thinking_delta);
        }
        stream.push(AssistantMessageEvent::ThinkingDelta {
            content_index: idx,
            delta: thinking_delta,
            partial: output.clone(),
        });
    }
}

fn signal_abort_error(signal: &AbortSignal) -> Error {
    signal
        .throw_if_aborted()
        .err()
        .unwrap_or_else(|| Error::msg("Request aborted"))
}

async fn run_mistral_stream(
    model: Model,
    normalized_context: TranscriptContext,
    options: MistralOptions,
    stream: AssistantMessageEventStream,
) -> Result<()> {
    let api_key = options
        .base
        .api_key
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::msg(format!("No API key for provider: {}", model.provider)))?
        .to_owned();

    let transformed_messages = {
        let normalizer = RefCell::new(MistralToolCallIdNormalizer::new());
        let normalize_fn =
            |id: &str, _m: &Model, _assistant: &AssistantMessage| -> String { normalizer.borrow_mut().normalize(id) };
        transform_messages(&normalized_context.messages, &model, Some(&normalize_fn))
    };

    let mut payload = build_chat_payload(&model, &normalized_context, &transformed_messages, &options)?;
    if let Some(on_payload) = &options.base.on_payload
        && let Some(next_payload) = on_payload(payload.clone(), &model).await?
    {
        payload = next_payload;
    }

    let url = build_mistral_completions_url(&model.base_url)?;
    let headers = build_mistral_headers(&model, &api_key, &options);
    let timeout_signal = AbortSignal::timeout(options.base.timeout_ms.unwrap_or(DEFAULT_MISTRAL_TIMEOUT_MS));
    let combined_signal = match &options.base.signal {
        Some(caller_sig) => AbortSignal::any(&[caller_sig.clone(), timeout_signal]),
        None => timeout_signal,
    };

    if combined_signal.aborted() {
        return Err(signal_abort_error(&combined_signal));
    }

    let wire_body = pi_js::json::stringify(&to_mistral_wire_payload(payload));
    let request = Request::post(url)
        .with_headers(headers)
        .body(Body::Text(wire_body))
        .signal(combined_signal.clone());

    let response = tokio::select! {
        biased;
        _ = combined_signal.cancelled() => {
            return Err(signal_abort_error(&combined_signal));
        }
        res = async {
            if let Some(custom_fetch) = &options.base.fetch {
                custom_fetch(request).await
            } else {
                pi_js::fetch::fetch(request).await
            }
        } => res?,
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

    if !response.ok() {
        let status = response.status();
        let status_text = response.status_text().to_owned();
        let body = tokio::select! {
            biased;
            _ = combined_signal.cancelled() => {
                return Err(signal_abort_error(&combined_signal));
            }
            text_res = response.text() => text_res.unwrap_or_default(),
        };
        return Err(MistralHttpError::new(status, body, status_text).into_js_error());
    }

    if !response.has_body() {
        return Err(Error::msg("Mistral response has no body"));
    }

    let mut sse_stream = response.sse()?;
    let mut output = create_output(&model);
    stream.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });

    let mut active_block: Option<ActiveMistralBlock> = None;
    let mut tool_blocks_by_key: IndexMap<ToolBlockKey, usize> = IndexMap::new();
    let mut tool_partial_args: IndexMap<usize, String> = IndexMap::new();

    loop {
        let next_event = tokio::select! {
            biased;
            _ = combined_signal.cancelled() => {
                return Err(signal_abort_error(&combined_signal));
            }
            item = sse_stream.next() => item,
        };

        let Some(sse_result) = next_event else {
            break;
        };

        let sse = sse_result?;
        let data = pi_js::str16::trim(&sse.data);
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            break;
        }

        let raw_chunk: JsonValue = pi_js::json::parse(data)?;
        if !raw_chunk
            .as_object()
            .and_then(|o| o.get("choices"))
            .is_some_and(Value::is_array)
        {
            return Err(Error::msg("Invalid Mistral streaming event"));
        }

        if let Some(on_event) = &options.base.on_provider_stream_event {
            on_event(&raw_chunk, &model).await?;
        }

        let chunk: MistralCompletionChunk = serde_json::from_value(raw_chunk)
            .map_err(|e| Error::msg(format!("Invalid Mistral streaming event: {e}")))?;

        if output.response_id.as_deref().is_none_or(str::is_empty)
            && let Some(id) = chunk.id
            && !id.is_empty()
        {
            output.response_id = Some(id);
        }

        if let Some(usage_obj) = chunk.usage {
            let prompt_tokens = usage_obj.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0);
            let completion_tokens = usage_obj.get("completion_tokens").and_then(Value::as_i64).unwrap_or(0);
            let cached_prompt_tokens = get_mistral_cached_prompt_tokens(&usage_obj, prompt_tokens);

            output.usage.input = (prompt_tokens - cached_prompt_tokens).max(0);
            output.usage.output = completion_tokens;
            output.usage.cache_read = cached_prompt_tokens;
            output.usage.cache_write = 0;
            output.usage.total_tokens = usage_obj
                .get("total_tokens")
                .and_then(Value::as_i64)
                .filter(|&t| t != 0)
                .unwrap_or(
                    output.usage.input + output.usage.output + output.usage.cache_read + output.usage.cache_write,
                );
            calculate_cost(&AnyModel::Chat(model.clone()), &mut output.usage);
        }

        let Some(choice) = chunk.choices.into_iter().next() else {
            continue;
        };

        if let Some(finish_reason) = choice.finish_reason
            && !finish_reason.is_empty()
        {
            output.raw_stop_reason = Some(finish_reason.clone());
            let (stop_reason, err_msg) = map_chat_stop_reason(Some(finish_reason.as_str()));
            output.stop_reason = stop_reason;
            if let Some(msg) = err_msg {
                output.error_message = Some(msg);
            }
        }

        if let Some(content) = choice.delta.content {
            match content {
                MistralDeltaContent::Text(s) => {
                    push_text_delta(sanitize_surrogates(&s), &mut active_block, &mut output, &stream);
                }
                MistralDeltaContent::Chunks(items) => {
                    for item in items {
                        match item.kind.as_str() {
                            "thinking" => {
                                let raw_text = item
                                    .thinking
                                    .unwrap_or_default()
                                    .into_iter()
                                    .filter_map(|p| p.text)
                                    .filter(|t| !t.is_empty())
                                    .collect::<Vec<_>>()
                                    .join("");
                                push_thinking_delta(
                                    sanitize_surrogates(&raw_text),
                                    &mut active_block,
                                    &mut output,
                                    &stream,
                                );
                            }
                            "text" => {
                                let raw_text = item.text.unwrap_or_default();
                                push_text_delta(
                                    sanitize_surrogates(&raw_text),
                                    &mut active_block,
                                    &mut output,
                                    &stream,
                                );
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        for tool_call in choice.delta.tool_calls.unwrap_or_default() {
            flush_active_block(&mut active_block, &mut output, &stream);
            let call_id = match tool_call.id.as_deref() {
                Some(id) if !id.is_empty() && id != "null" => id.to_owned(),
                _ => derive_mistral_tool_call_id(&format!("toolcall:{}", tool_call.index.unwrap_or(0)), 0),
            };
            let key = match tool_call.index {
                Some(idx) => ToolBlockKey::Index(idx),
                None => ToolBlockKey::Id(call_id.clone()),
            };

            let block_idx = if let Some(&existing_idx) = tool_blocks_by_key.get(&key) {
                existing_idx
            } else {
                let idx = output.content.len();
                output.content.push(AssistantContentBlock::ToolCall(ToolCall::new(
                    call_id,
                    tool_call.function.name,
                    JsonObject::new(),
                )));
                tool_blocks_by_key.insert(key, idx);
                tool_partial_args.insert(idx, String::new());
                stream.push(AssistantMessageEvent::ToolCallStart {
                    content_index: idx,
                    partial: output.clone(),
                });
                idx
            };

            let args_delta = match tool_call.function.arguments {
                Some(Value::String(s)) => s,
                Some(Value::Object(obj)) => pi_js::json::stringify(&Value::Object(obj)),
                Some(other) if !other.is_null() => pi_js::json::stringify(&other),
                _ => "{}".to_owned(),
            };

            let buf = tool_partial_args.entry(block_idx).or_default();
            buf.push_str(&args_delta);
            let parsed = parse_streaming_json(Some(buf));
            if let Some(AssistantContentBlock::ToolCall(tc)) = output.content.get_mut(block_idx) {
                tc.arguments = parsed;
            }
            stream.push(AssistantMessageEvent::ToolCallDelta {
                content_index: block_idx,
                delta: args_delta,
                partial: output.clone(),
            });
        }
    }

    flush_active_block(&mut active_block, &mut output, &stream);
    for (_, idx) in tool_blocks_by_key {
        let partial_str = tool_partial_args.shift_remove(&idx);
        let parsed = parse_streaming_json(partial_str.as_deref());
        let final_tc = if let Some(AssistantContentBlock::ToolCall(tc)) = output.content.get_mut(idx) {
            tc.arguments = parsed;
            Some(tc.clone())
        } else {
            None
        };
        if let Some(tc) = final_tc {
            stream.push(AssistantMessageEvent::ToolCallEnd {
                content_index: idx,
                tool_call: tc,
                partial: output.clone(),
            });
        }
    }

    if options.base.signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Err(Error::msg("Request was aborted"));
    }

    match output.stop_reason {
        StopReason::Pending => return Err(Error::msg("Mistral stream ended without a finish reason")),
        StopReason::Aborted | StopReason::Error => {
            return Err(Error::msg(
                output
                    .error_message
                    .clone()
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| "An unknown error occurred".to_owned()),
            ));
        }
        _ => {}
    }

    stream.push(AssistantMessageEvent::Done {
        reason: output.stop_reason,
        message: output.clone(),
    });
    stream.end(Some(output));
    Ok(())
}

fn create_output(model: &Model) -> AssistantMessage {
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
    }
}

/// Stream responses from the native Mistral Chat Completions endpoint.
pub fn stream_with_options(
    model: &Model,
    context: &TranscriptContext,
    options: Option<MistralOptions>,
) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let stream_clone = stream.clone();
    let supports_mid = model
        .compat
        .as_ref()
        .and_then(|c| c.as_mistral_conversations())
        .and_then(|mc| mc.supports_mid_convo_system_messages);
    let normalized_context = resolve_transcript(context, supports_mid);
    let model_owned = model.clone();
    let opts = options.unwrap_or_default();

    tokio::spawn(async move {
        let signal = opts.base.signal.clone();
        if let Err(err) = run_mistral_stream(model_owned.clone(), normalized_context, opts, stream_clone.clone()).await
        {
            let mut err_output = create_output(&model_owned);
            err_output.stop_reason = if signal.as_ref().is_some_and(AbortSignal::aborted) {
                StopReason::Aborted
            } else {
                StopReason::Error
            };
            err_output.error_message = Some(format_mistral_error(&err));
            stream_clone.push(AssistantMessageEvent::Error {
                reason: err_output.stop_reason,
                error: err_output.clone(),
            });
            stream_clone.end(Some(err_output));
        }
    });

    stream
}

pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: Option<ApiStreamOptions>,
) -> AssistantMessageEventStream {
    stream_with_options(model, context, Some(MistralOptions::from_api_options(options)))
}

/// Maps provider-agnostic `SimpleStreamOptions` to Mistral options.
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let Some(api_key) = options
        .as_ref()
        .and_then(|o| o.base.api_key.as_deref())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
    else {
        let provider = model.provider.clone();
        return lazy_stream(model, async move {
            Err(Error::msg(format!("No API key for provider: {provider}")))
        });
    };

    let base = build_base_options(model, context, options.as_ref(), Some(&api_key));
    let clamped_reasoning = options
        .as_ref()
        .and_then(|o| o.reasoning)
        .map(|r| clamp_thinking_level(model, ModelThinkingLevel::from(r)));
    let reasoning = clamped_reasoning.filter(|&l| l != ModelThinkingLevel::Off);

    // Models with a thinking level map use `reasoning_effort`; other reasoning models use `prompt_mode`.
    let effort_map = if model.reasoning {
        model.thinking_level_map.as_ref()
    } else {
        None
    };

    let reasoning_effort = effort_map.and_then(|map| match reasoning {
        Some(level) => match map.get(&level).and_then(|v| v.as_deref()) {
            Some(s) => MistralReasoningEffort::from_str_lossy(s),
            None => Some(MistralReasoningEffort::High),
        },
        None => map
            .get(&ModelThinkingLevel::Off)
            .and_then(|v| v.as_deref())
            .and_then(MistralReasoningEffort::from_str_lossy),
    });

    let prompt_mode = if model.reasoning && effort_map.is_none() && reasoning.is_some() {
        Some(MistralPromptMode::Reasoning)
    } else {
        None
    };

    stream_with_options(
        model,
        context,
        Some(MistralOptions {
            base,
            tool_choice: options.as_ref().and_then(|o| o.tool_choice.map(Into::into)),
            prompt_mode,
            reasoning_effort,
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
