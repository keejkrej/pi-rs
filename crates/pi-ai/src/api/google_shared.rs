//! Port of packages/ai/src/api/google-shared.ts

use std::fmt;
use std::future::Future;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::StreamExt;
use indexmap::IndexMap;
use pi_js::abort::AbortSignal;
use pi_js::fetch::{Body, Headers, Request, Response};
use pi_js::{BoxFuture, Error, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::api::constrained_sampling::{get_json_schema_tool_parameters, resolve_json_schema_strict_sampling};
use crate::api::transform_messages::transform_messages;
use crate::models::{calculate_cost, clamp_thinking_level};
use crate::types::{
    AnyModel, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, ImageContent, JsonObject, JsonValue,
    Message, Model, ModelInput, ModelThinkingLevel, OnPayloadFn, OnProviderStreamEventFn, ProviderHeaders, StopReason,
    TextContent, ThinkingBudgets, ThinkingContent, ThinkingLevel, Tool, ToolCall, ToolChoice, TranscriptContext, Usage,
    UsageCost, UserContentBlock, UserMessageContent,
};
use crate::utils::error_body::{SdkErrorInfo, format_provider_error, normalize_provider_error};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::headers::provider_headers_to_record;
use crate::utils::pi_user_agent::get_pi_user_agent;
use crate::utils::provider_retry::{ProviderHttpError, ProviderRetryOptions, retry_provider_request};
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::text::get_system_message_text;
use crate::utils::transcript::{
    collapse_system_messages, get_current_tools, get_initial_system_message, without_initial_system_message,
};

static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

const JSON_SCHEMA_META_DECLARATIONS: &[&str] = &[
    "$schema",
    "$id",
    "$anchor",
    "$dynamicAnchor",
    "$vocabulary",
    "$comment",
    "$defs",
    "definitions",
];

/// Thinking level for Gemini 3 models.
/// Mirrors Google's `ThinkingLevel` enum values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GoogleApiThinkingLevel {
    #[serde(rename = "THINKING_LEVEL_UNSPECIFIED")]
    ThinkingLevelUnspecified,
    #[serde(rename = "MINIMAL")]
    Minimal,
    #[serde(rename = "LOW")]
    Low,
    #[serde(rename = "MEDIUM")]
    Medium,
    #[serde(rename = "HIGH")]
    High,
}

impl GoogleApiThinkingLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ThinkingLevelUnspecified => "THINKING_LEVEL_UNSPECIFIED",
            Self::Minimal => "MINIMAL",
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
        }
    }
}

impl fmt::Display for GoogleApiThinkingLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Logical Google thinking level (`Exclude<ThinkingLevel, "xhigh" | "max">`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResolvedGoogleThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
}

impl ResolvedGoogleThinkingLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

impl fmt::Display for ResolvedGoogleThinkingLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<ResolvedGoogleThinkingLevel> for ThinkingLevel {
    fn from(value: ResolvedGoogleThinkingLevel) -> Self {
        match value {
            ResolvedGoogleThinkingLevel::Minimal => Self::Minimal,
            ResolvedGoogleThinkingLevel::Low => Self::Low,
            ResolvedGoogleThinkingLevel::Medium => Self::Medium,
            ResolvedGoogleThinkingLevel::High => Self::High,
        }
    }
}

impl TryFrom<ThinkingLevel> for ResolvedGoogleThinkingLevel {
    type Error = Error;

    fn try_from(value: ThinkingLevel) -> Result<Self> {
        match value {
            ThinkingLevel::Minimal => Ok(Self::Minimal),
            ThinkingLevel::Low => Ok(Self::Low),
            ThinkingLevel::Medium => Ok(Self::Medium),
            ThinkingLevel::High => Ok(Self::High),
            ThinkingLevel::XHigh | ThinkingLevel::Max => Err(Error::msg(format!(
                "ThinkingLevel '{}' cannot map directly to ResolvedGoogleThinkingLevel",
                value.as_str()
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoogleToolChoice {
    Auto,
    None,
    Any,
}

impl GoogleToolChoice {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::Any => "any",
        }
    }
}

impl From<ToolChoice> for GoogleToolChoice {
    fn from(value: ToolChoice) -> Self {
        match value {
            ToolChoice::Auto => Self::Auto,
            ToolChoice::None => Self::None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GoogleFunctionCallingMode {
    ModeUnspecified,
    Auto,
    Any,
    None,
    Validated,
}

impl GoogleFunctionCallingMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModeUnspecified => "MODE_UNSPECIFIED",
            Self::Auto => "AUTO",
            Self::Any => "ANY",
            Self::None => "NONE",
            Self::Validated => "VALIDATED",
        }
    }
}

impl fmt::Display for GoogleFunctionCallingMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GoogleFinishReason {
    Stop,
    MaxTokens,
    Blocklist,
    ProhibitedContent,
    Spii,
    Safety,
    ImageSafety,
    ImageProhibitedContent,
    ImageRecitation,
    ImageOther,
    Recitation,
    FinishReasonUnspecified,
    Other,
    Language,
    MalformedFunctionCall,
    UnexpectedToolCall,
    TooManyToolCalls,
    NoImage,
}

impl GoogleFinishReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "STOP",
            Self::MaxTokens => "MAX_TOKENS",
            Self::Blocklist => "BLOCKLIST",
            Self::ProhibitedContent => "PROHIBITED_CONTENT",
            Self::Spii => "SPII",
            Self::Safety => "SAFETY",
            Self::ImageSafety => "IMAGE_SAFETY",
            Self::ImageProhibitedContent => "IMAGE_PROHIBITED_CONTENT",
            Self::ImageRecitation => "IMAGE_RECITATION",
            Self::ImageOther => "IMAGE_OTHER",
            Self::Recitation => "RECITATION",
            Self::FinishReasonUnspecified => "FINISH_REASON_UNSPECIFIED",
            Self::Other => "OTHER",
            Self::Language => "LANGUAGE",
            Self::MalformedFunctionCall => "MALFORMED_FUNCTION_CALL",
            Self::UnexpectedToolCall => "UNEXPECTED_TOOL_CALL",
            Self::TooManyToolCalls => "TOO_MANY_TOOL_CALLS",
            Self::NoImage => "NO_IMAGE",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleThinkingOptions {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<GoogleApiThinkingLevel>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleThinkingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_thoughts: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<GoogleApiThinkingLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleBlob {
    pub mime_type: String,
    pub data: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleFunctionCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub args: JsonObject,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleFunctionResponsePart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<GoogleBlob>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleFunctionResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub response: JsonObject,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<GoogleFunctionResponsePart>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GooglePart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<GoogleBlob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call: Option<GoogleFunctionCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_response: Option<GoogleFunctionResponse>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleContent {
    pub role: String,
    pub parts: Vec<GooglePart>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleFunctionDeclaration {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<JsonValue>,
    #[serde(rename = "parametersJsonSchema", default, skip_serializing_if = "Option::is_none")]
    pub parameters_json_schema: Option<JsonValue>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleTool {
    pub function_declarations: Vec<GoogleFunctionDeclaration>,
}

const fn model_level_to_thinking_level(level: ModelThinkingLevel) -> Option<ThinkingLevel> {
    match level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(ThinkingLevel::High),
        ModelThinkingLevel::XHigh => Some(ThinkingLevel::XHigh),
        ModelThinkingLevel::Max => Some(ThinkingLevel::Max),
    }
}

/// Resolve a supported pi level or model-specific Google mapping to a standard Google level.
pub fn resolve_google_thinking_level(model: &Model, level: ThinkingLevel) -> Result<ResolvedGoogleThinkingLevel> {
    let model_level = ModelThinkingLevel::from(level);
    let entry = model.thinking_level_map.as_ref().and_then(|map| map.get(&model_level));

    let (mapped_display, resolved) = match entry {
        Some(Some(mapped)) => (mapped.as_str(), mapped.to_ascii_lowercase()),
        Some(None) => ("null", level.as_str().to_owned()),
        None => ("undefined", level.as_str().to_owned()),
    };

    match resolved.as_str() {
        "minimal" => Ok(ResolvedGoogleThinkingLevel::Minimal),
        "low" => Ok(ResolvedGoogleThinkingLevel::Low),
        "medium" => Ok(ResolvedGoogleThinkingLevel::Medium),
        "high" => Ok(ResolvedGoogleThinkingLevel::High),
        _ => Err(Error::msg(format!(
            "Unsupported Google thinking level mapping for {}/{}: {} -> {}",
            model.provider,
            model.id,
            level.as_str(),
            mapped_display
        ))),
    }
}

fn gemini3_thinking_regex() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"gemini-3(?:\.\d+)?-(?:pro|flash)").ok())
        .as_ref()
}

fn gemma4_thinking_regex() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"gemma-?4").ok()).as_ref()
}

fn gemini_major_regex() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^gemini(?:-live)?-(\d+)").ok()).as_ref()
}

/// Whether this model uses Gemini's discrete `thinkingLevel` control instead of
/// the token-based `thinkingBudget` control. Supported levels come from the
/// model's `thinkingLevelMap`; this only selects the Google wire format.
pub fn uses_google_thinking_level(model: &Model) -> bool {
    let id = model.id.to_ascii_lowercase();
    gemini3_thinking_regex().is_some_and(|re| re.is_match(&id))
        || id == "gemini-flash-latest"
        || id == "gemini-flash-lite-latest"
        || gemma4_thinking_regex().is_some_and(|re| re.is_match(&id))
}

pub const fn to_google_thinking_level(level: ResolvedGoogleThinkingLevel) -> GoogleApiThinkingLevel {
    match level {
        ResolvedGoogleThinkingLevel::Minimal => GoogleApiThinkingLevel::Minimal,
        ResolvedGoogleThinkingLevel::Low => GoogleApiThinkingLevel::Low,
        ResolvedGoogleThinkingLevel::Medium => GoogleApiThinkingLevel::Medium,
        ResolvedGoogleThinkingLevel::High => GoogleApiThinkingLevel::High,
    }
}

// PORT: Rust shares `GoogleApiThinkingLevel` directly with REST wire payloads without a separate
// `@google/genai` SDK enum, so this conversion is an identity helper.
pub const fn to_google_sdk_thinking_level(level: GoogleApiThinkingLevel) -> GoogleApiThinkingLevel {
    level
}

pub fn get_disabled_google_thinking_config(model: &Model) -> Result<GoogleThinkingConfig> {
    if !uses_google_thinking_level(model) {
        return Ok(GoogleThinkingConfig {
            thinking_budget: Some(0),
            ..Default::default()
        });
    }

    let fallback = clamp_thinking_level(model, ModelThinkingLevel::Off);
    let Some(thinking_fallback) = model_level_to_thinking_level(fallback) else {
        return Ok(GoogleThinkingConfig {
            thinking_budget: Some(0),
            ..Default::default()
        });
    };

    let resolved_level = resolve_google_thinking_level(model, thinking_fallback)?;
    let api_level = to_google_thinking_level(resolved_level);
    Ok(GoogleThinkingConfig {
        thinking_level: Some(to_google_sdk_thinking_level(api_level)),
        ..Default::default()
    })
}

/// Determines whether a streamed Gemini `Part` should be treated as "thinking".
///
/// `thought: Some(true)` is the definitive marker for thinking content.
/// `thoughtSignature` can appear on any part type and does not indicate thinking.
pub fn is_thinking_part(part: &GooglePart) -> bool {
    part.thought == Some(true)
}

/// Retain thought signatures during streaming.
///
/// Preserves the last non-empty signature when subsequent deltas omit `thoughtSignature`.
pub fn retain_thought_signature(existing: Option<&str>, incoming: Option<&str>) -> Option<String> {
    if let Some(sig) = incoming
        && !sig.is_empty()
    {
        return Some(sig.to_owned());
    }
    existing.map(str::to_owned)
}

fn is_valid_thought_signature(signature: Option<&str>) -> bool {
    let Some(sig) = signature else {
        return false;
    };
    let bytes = sig.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return false;
    }
    let pad_count = bytes.iter().rev().take_while(|&&b| b == b'=').count();
    if pad_count > 2 || pad_count == bytes.len() {
        return false;
    }
    bytes[..bytes.len() - pad_count]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/')
}

fn resolve_thought_signature(is_same_provider_and_model: bool, signature: Option<&str>) -> Option<String> {
    if is_same_provider_and_model && is_valid_thought_signature(signature) {
        signature.map(str::to_owned)
    } else {
        None
    }
}

fn get_gemini_major_version(model_id: &str) -> Option<u32> {
    let lower = model_id.to_ascii_lowercase();
    let captures = gemini_major_regex()?.captures(&lower)?;
    captures.get(1)?.as_str().parse::<u32>().ok()
}

/// Models via Google APIs that require explicit tool call IDs in function calls/responses.
pub fn requires_tool_call_id(model_id: &str) -> bool {
    model_id.starts_with("claude-")
        || model_id.starts_with("gpt-oss-")
        || get_gemini_major_version(model_id).is_some_and(|v| v >= 3)
}

fn supports_multimodal_function_response(model_id: &str) -> bool {
    match get_gemini_major_version(model_id) {
        Some(v) => v >= 3,
        None => true,
    }
}

fn convert_user_content(content: &UserMessageContent) -> Option<GoogleContent> {
    match content {
        UserMessageContent::Text(text) => Some(GoogleContent {
            role: "user".to_owned(),
            parts: vec![GooglePart {
                text: Some(sanitize_surrogates(text)),
                ..Default::default()
            }],
        }),
        UserMessageContent::Blocks(items) => {
            if items.is_empty() {
                return None;
            }
            let parts: Vec<GooglePart> = items
                .iter()
                .map(|item| match item {
                    UserContentBlock::Text(text_block) => GooglePart {
                        text: Some(sanitize_surrogates(&text_block.text)),
                        ..Default::default()
                    },
                    UserContentBlock::Image(image_block) => GooglePart {
                        inline_data: Some(GoogleBlob {
                            mime_type: image_block.mime_type.clone(),
                            data: image_block.data.clone(),
                        }),
                        ..Default::default()
                    },
                })
                .collect();
            Some(GoogleContent {
                role: "user".to_owned(),
                parts,
            })
        }
    }
}

fn convert_assistant_content(msg: &AssistantMessage, model: &Model) -> Option<GoogleContent> {
    let is_same_provider_and_model = msg.provider == model.provider && msg.model == model.id;
    let include_id = requires_tool_call_id(&model.id);
    let mut parts = Vec::new();

    for block in &msg.content {
        match block {
            AssistantContentBlock::Text(text_block) => {
                let thought_signature =
                    resolve_thought_signature(is_same_provider_and_model, text_block.text_signature.as_deref());
                if pi_js::str16::trim(&text_block.text).is_empty() && thought_signature.is_none() {
                    continue;
                }
                parts.push(GooglePart {
                    text: Some(sanitize_surrogates(&text_block.text)),
                    thought_signature,
                    ..Default::default()
                });
            }
            AssistantContentBlock::Thinking(thinking_block) => {
                if is_same_provider_and_model {
                    let thought_signature =
                        resolve_thought_signature(true, thinking_block.thinking_signature.as_deref());
                    if pi_js::str16::trim(&thinking_block.thinking).is_empty() && thought_signature.is_none() {
                        continue;
                    }
                    parts.push(GooglePart {
                        thought: Some(true),
                        text: Some(sanitize_surrogates(&thinking_block.thinking)),
                        thought_signature,
                        ..Default::default()
                    });
                } else {
                    if pi_js::str16::trim(&thinking_block.thinking).is_empty() {
                        continue;
                    }
                    parts.push(GooglePart {
                        text: Some(sanitize_surrogates(&thinking_block.thinking)),
                        ..Default::default()
                    });
                }
            }
            AssistantContentBlock::ToolCall(tc) => {
                let thought_signature =
                    resolve_thought_signature(is_same_provider_and_model, tc.thought_signature.as_deref());
                parts.push(GooglePart {
                    function_call: Some(GoogleFunctionCall {
                        id: if include_id { Some(tc.id.clone()) } else { None },
                        name: tc.name.clone(),
                        args: tc.arguments.clone(),
                    }),
                    thought_signature,
                    ..Default::default()
                });
            }
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(GoogleContent {
            role: "model".to_owned(),
            parts,
        })
    }
}

/// Convert internal messages to Gemini `GoogleContent[]` format.
pub fn convert_messages(model: &Model, context: &TranscriptContext) -> Vec<GoogleContent> {
    let collapsed = collapse_system_messages(context);
    let conversation = without_initial_system_message(&collapsed.messages);
    let mut contents: Vec<GoogleContent> = Vec::new();

    let normalize_tool_call_id = |id: &str, m: &Model, _assistant: &AssistantMessage| -> String {
        if !requires_tool_call_id(&m.id) {
            return id.to_owned();
        }
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
    };

    let transformed = transform_messages(&conversation, model, Some(&normalize_tool_call_id));
    let supports_image_input = model.input.contains(&ModelInput::Image);
    let include_id = requires_tool_call_id(&model.id);
    let multimodal_function_response = supports_multimodal_function_response(&model.id);

    for msg in &transformed {
        match msg {
            Message::User(user_msg) => {
                if let Some(content) = convert_user_content(&user_msg.content) {
                    contents.push(content);
                }
            }
            Message::Assistant(assistant_msg) => {
                if let Some(content) = convert_assistant_content(assistant_msg, model) {
                    contents.push(content);
                }
            }
            Message::ToolResult(tool_msg) => {
                let text_result: String = tool_msg
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        UserContentBlock::Text(t) => Some(t.text.as_str()),
                        UserContentBlock::Image(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                let image_content: Vec<&ImageContent> = if supports_image_input {
                    tool_msg
                        .content
                        .iter()
                        .filter_map(|c| match c {
                            UserContentBlock::Image(img) => Some(img),
                            UserContentBlock::Text(_) => None,
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                let has_text = !text_result.is_empty();
                let has_images = !image_content.is_empty();
                let response_value = if has_text {
                    sanitize_surrogates(&text_result)
                } else if has_images {
                    "(see attached image)".to_owned()
                } else {
                    String::new()
                };

                let image_blobs: Vec<GoogleBlob> = image_content
                    .iter()
                    .map(|img| GoogleBlob {
                        mime_type: img.mime_type.clone(),
                        data: img.data.clone(),
                    })
                    .collect();

                let mut response_map = JsonObject::new();
                let response_key = if tool_msg.is_error { "error" } else { "output" };
                response_map.insert(response_key.to_owned(), JsonValue::String(response_value));

                let nested_parts = if has_images && multimodal_function_response {
                    Some(
                        image_blobs
                            .iter()
                            .cloned()
                            .map(|blob| GoogleFunctionResponsePart {
                                inline_data: Some(blob),
                            })
                            .collect(),
                    )
                } else {
                    None
                };

                let function_response_part = GooglePart {
                    function_response: Some(GoogleFunctionResponse {
                        id: if include_id {
                            Some(tool_msg.tool_call_id.clone())
                        } else {
                            None
                        },
                        name: tool_msg.tool_name.clone(),
                        response: response_map,
                        parts: nested_parts,
                    }),
                    ..Default::default()
                };

                if let Some(last) = contents.last_mut()
                    && last.role == "user"
                    && last.parts.iter().any(|p| p.function_response.is_some())
                {
                    last.parts.push(function_response_part);
                } else {
                    contents.push(GoogleContent {
                        role: "user".to_owned(),
                        parts: vec![function_response_part],
                    });
                }

                if has_images && !multimodal_function_response {
                    let mut image_parts = Vec::with_capacity(1 + image_blobs.len());
                    image_parts.push(GooglePart {
                        text: Some("Tool result image:".to_owned()),
                        ..Default::default()
                    });
                    for blob in image_blobs {
                        image_parts.push(GooglePart {
                            inline_data: Some(blob),
                            ..Default::default()
                        });
                    }
                    contents.push(GoogleContent {
                        role: "user".to_owned(),
                        parts: image_parts,
                    });
                }
            }
            Message::System(_) => {}
        }
    }

    contents
}

fn sanitize_for_open_api(schema: &JsonValue) -> JsonValue {
    match schema {
        JsonValue::Object(map) => {
            let mut result = Map::new();
            for (key, value) in map {
                if JSON_SCHEMA_META_DECLARATIONS.contains(&key.as_str()) {
                    continue;
                }
                result.insert(key.clone(), sanitize_for_open_api(value));
            }
            JsonValue::Object(result)
        }
        other => other.clone(),
    }
}

/// Convert tools to Gemini function declarations format.
///
/// By default uses `parametersJsonSchema` which supports full JSON Schema. Set
/// `use_parameters` to `true` to use the legacy OpenAPI 3.03 `parameters` field.
pub fn convert_tools(
    tools: &[Tool],
    use_parameters: bool,
    supports_strict_mode: bool,
) -> Result<Option<Vec<GoogleTool>>> {
    if tools.is_empty() {
        return Ok(None);
    }

    let mut declarations = Vec::with_capacity(tools.len());
    for tool in tools {
        let strict = resolve_json_schema_strict_sampling(tool, supports_strict_mode, None)?;
        let parameters = get_json_schema_tool_parameters(tool, strict)?;
        let param_value = parameters.to_value();
        if use_parameters {
            declarations.push(GoogleFunctionDeclaration {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: Some(sanitize_for_open_api(&param_value)),
                parameters_json_schema: None,
            });
        } else {
            declarations.push(GoogleFunctionDeclaration {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: None,
                parameters_json_schema: Some(param_value),
            });
        }
    }

    Ok(Some(vec![GoogleTool {
        function_declarations: declarations,
    }]))
}

/// Gemini 3+ enforces required function parameters in validated tool-calling modes.
pub fn supports_google_strict_tool_sampling(model_id: &str) -> bool {
    get_gemini_major_version(model_id).is_some_and(|v| v >= 3)
}

/// Map tool choice string to Gemini `GoogleFunctionCallingMode`.
pub fn map_tool_choice(choice: &str) -> GoogleFunctionCallingMode {
    match choice {
        "none" => GoogleFunctionCallingMode::None,
        "any" => GoogleFunctionCallingMode::Any,
        _ => GoogleFunctionCallingMode::Auto,
    }
}

pub fn resolve_google_function_calling_mode(
    tools: &[Tool],
    tool_choice: Option<&str>,
    supports_strict_mode: bool,
) -> Result<Option<GoogleFunctionCallingMode>> {
    let mut use_strict_mode = false;
    for tool in tools {
        if resolve_json_schema_strict_sampling(tool, supports_strict_mode, None)? == Some(true) {
            use_strict_mode = true;
            break;
        }
    }

    if let Some(choice) = tool_choice
        && (choice == "none" || choice == "any")
    {
        return Ok(Some(map_tool_choice(choice)));
    }
    if use_strict_mode {
        return Ok(Some(GoogleFunctionCallingMode::Validated));
    }
    Ok(tool_choice.map(map_tool_choice))
}

/// Map typed Gemini `FinishReason` to our `StopReason`.
pub fn map_stop_reason(reason: GoogleFinishReason) -> StopReason {
    match reason {
        GoogleFinishReason::Stop => StopReason::Stop,
        GoogleFinishReason::MaxTokens => StopReason::Length,
        GoogleFinishReason::Blocklist
        | GoogleFinishReason::ProhibitedContent
        | GoogleFinishReason::Spii
        | GoogleFinishReason::Safety
        | GoogleFinishReason::ImageSafety
        | GoogleFinishReason::ImageProhibitedContent
        | GoogleFinishReason::ImageRecitation
        | GoogleFinishReason::ImageOther
        | GoogleFinishReason::Recitation
        | GoogleFinishReason::FinishReasonUnspecified
        | GoogleFinishReason::Other
        | GoogleFinishReason::Language
        | GoogleFinishReason::MalformedFunctionCall
        | GoogleFinishReason::UnexpectedToolCall
        | GoogleFinishReason::TooManyToolCalls
        | GoogleFinishReason::NoImage => StopReason::Error,
    }
}

/// Map raw string finish reason to our `StopReason`.
pub fn map_stop_reason_string(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::Stop,
        "MAX_TOKENS" => StopReason::Length,
        _ => StopReason::Error,
    }
}

fn normalize_google_retry_error(error: Error) -> Error {
    if error.downcast_ref::<ProviderHttpError>().is_some() {
        return error;
    }
    if let Some(sdk) = error.downcast_ref::<SdkErrorInfo>() {
        let status = sdk
            .status_code
            .or(sdk.status)
            .or(sdk.metadata_http_status_code)
            .or(sdk.response_status_code);
        if status.is_some() {
            return ProviderHttpError::new(status, None, error.to_string()).into_js_error();
        }
    }
    error
}

/// Run a Google GenAI request with the shared provider retry policy
/// (408/409/429/5xx with backoff, honoring retry-after), normalizing SDK errors
/// that carry a status without response headers.
pub async fn retry_google_request<T, F, Fut>(mut request: F, options: ProviderRetryOptions) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    retry_provider_request(
        move || {
            let fut = request();
            async move { fut.await.map_err(normalize_google_retry_error) }
        },
        options,
    )
    .await
}

// ============================================================================
// Shared HTTP / SSE transport & parameter projection for Google AI and Vertex
// ============================================================================

pub(crate) fn to_json_value<T: Serialize + ?Sized>(value: &T) -> JsonValue {
    let serialized = pi_js::json::stringify(value);
    pi_js::json::parse(&serialized).unwrap_or(JsonValue::Null)
}

pub(crate) fn build_google_headers(
    model_headers: Option<&IndexMap<String, String>>,
    options_headers: Option<&ProviderHeaders>,
) -> Option<IndexMap<String, String>> {
    let mut ua = ProviderHeaders::new();
    ua.insert("User-Agent".to_owned(), Some(get_pi_user_agent()));

    let model_ph: Option<ProviderHeaders> =
        model_headers.map(|hdrs| hdrs.iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect());

    provider_headers_to_record([Some(&ua), model_ph.as_ref(), options_headers])
}

pub(crate) struct GoogleParamsInput<'a> {
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    pub tool_choice: Option<GoogleToolChoice>,
    pub thinking: Option<&'a GoogleThinkingOptions>,
    pub signal: Option<&'a AbortSignal>,
}

pub(crate) fn build_google_generate_params(
    model: &Model,
    context: &TranscriptContext,
    input: GoogleParamsInput<'_>,
) -> Result<JsonValue> {
    let contents = convert_messages(model, context);
    let initial_system = get_initial_system_message(&context.messages);
    let current_tools = get_current_tools(&context.messages);

    let mut config = Map::new();
    if let Some(temp) = input.temperature {
        config.insert("temperature".to_owned(), pi_js::json::number_value(temp));
    }
    if let Some(max_tokens) = input.max_tokens {
        config.insert("maxOutputTokens".to_owned(), Value::from(max_tokens));
    }

    if let Some(sys) = initial_system {
        let sys_text = get_system_message_text(sys);
        if !sys_text.is_empty() {
            config.insert(
                "systemInstruction".to_owned(),
                Value::String(sanitize_surrogates(&sys_text)),
            );
        }
    }

    let supports_strict = supports_google_strict_tool_sampling(&model.id);
    if !current_tools.is_empty() {
        if let Some(converted_tools) = convert_tools(&current_tools, false, supports_strict)? {
            config.insert("tools".to_owned(), to_json_value(&converted_tools));
        }
        if let Some(mode) = resolve_google_function_calling_mode(
            &current_tools,
            input.tool_choice.map(GoogleToolChoice::as_str),
            supports_strict,
        )? {
            let mut fcc = Map::new();
            fcc.insert("mode".to_owned(), Value::String(mode.as_str().to_owned()));
            let mut tc = Map::new();
            tc.insert("functionCallingConfig".to_owned(), Value::Object(fcc));
            config.insert("toolConfig".to_owned(), Value::Object(tc));
        }
    }

    if let Some(thinking) = input.thinking
        && model.reasoning
    {
        if thinking.enabled {
            let mut tc = GoogleThinkingConfig {
                include_thoughts: Some(true),
                ..Default::default()
            };
            if let Some(level) = thinking.level {
                tc.thinking_level = Some(to_google_sdk_thinking_level(level));
            } else if let Some(budget) = thinking.budget_tokens {
                tc.thinking_budget = Some(budget);
            }
            config.insert("thinkingConfig".to_owned(), to_json_value(&tc));
        } else {
            let disabled = get_disabled_google_thinking_config(model)?;
            config.insert("thinkingConfig".to_owned(), to_json_value(&disabled));
        }
    }

    if input.signal.is_some_and(AbortSignal::aborted) {
        return Err(Error::msg("Request aborted"));
    }

    let mut root = Map::new();
    root.insert("model".to_owned(), Value::String(model.id.clone()));
    root.insert("contents".to_owned(), to_json_value(&contents));
    root.insert("config".to_owned(), Value::Object(config));
    Ok(Value::Object(root))
}

fn system_instruction_to_wire_content(val: &JsonValue) -> Option<JsonValue> {
    match val {
        Value::String(s) => {
            let mut part = Map::new();
            part.insert("text".to_owned(), Value::String(s.clone()));
            let mut content = Map::new();
            content.insert("role".to_owned(), Value::String("user".to_owned()));
            content.insert("parts".to_owned(), Value::Array(vec![Value::Object(part)]));
            Some(Value::Object(content))
        }
        Value::Object(obj) => {
            if obj.contains_key("parts") {
                Some(Value::Object(obj.clone()))
            } else {
                let mut content = Map::new();
                content.insert("role".to_owned(), Value::String("user".to_owned()));
                content.insert("parts".to_owned(), Value::Array(vec![Value::Object(obj.clone())]));
                Some(Value::Object(content))
            }
        }
        Value::Array(parts) => {
            let mut content = Map::new();
            content.insert("role".to_owned(), Value::String("user".to_owned()));
            content.insert("parts".to_owned(), Value::Array(parts.clone()));
            Some(Value::Object(content))
        }
        Value::Null => None,
        other => Some(other.clone()),
    }
}

/// Project `GenerateContentParameters`-shaped payload (post `on_payload`) to the
/// Google Generative AI / Vertex `:streamGenerateContent?alt=sse` JSON body.
pub(crate) fn project_google_params_to_wire_body(params: &JsonValue, is_vertex: bool) -> Result<(String, JsonValue)> {
    let Some(root) = params.as_object() else {
        return Err(Error::msg("Google request payload must be a JSON object"));
    };

    let model_str = root
        .get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::msg("model is required and must be a string"))?
        .to_owned();

    let mut wire = Map::new();
    if let Some(contents) = root.get("contents") {
        wire.insert("contents".to_owned(), contents.clone());
    }

    if let Some(config) = root.get("config").and_then(Value::as_object) {
        if is_vertex && config.contains_key("store") {
            return Err(Error::msg(
                "store parameter is only supported in Gemini Developer API mode, not in Gemini Enterprise Agent Platform mode.",
            ));
        }

        let mut gen_config = Map::new();
        for (key, value) in config {
            if value.is_null() {
                continue;
            }
            match key.as_str() {
                "abortSignal" | "httpOptions" => {}
                "systemInstruction" => {
                    if let Some(si) = system_instruction_to_wire_content(value) {
                        wire.insert("systemInstruction".to_owned(), si);
                    }
                }
                "tools" => {
                    wire.insert("tools".to_owned(), value.clone());
                }
                "toolConfig" => {
                    wire.insert("toolConfig".to_owned(), value.clone());
                }
                "cachedContent" | "safetySettings" | "labels" | "serviceTier" | "modelArmorConfig"
                | "continuationToken" => {
                    wire.insert(key.clone(), value.clone());
                }
                "generationOutputConfig" => {
                    wire.insert("outputConfig".to_owned(), value.clone());
                }
                _ => {
                    gen_config.insert(key.clone(), value.clone());
                }
            }
        }
        if !gen_config.is_empty() {
            wire.insert("generationConfig".to_owned(), Value::Object(gen_config));
        }
    }

    Ok((model_str, Value::Object(wire)))
}

pub(crate) fn format_google_model_path(model_id: &str, is_vertex: bool) -> Result<String> {
    if model_id.is_empty() {
        return Err(Error::msg("model is required and must be a string"));
    }
    if model_id.contains("..") || model_id.contains('?') || model_id.contains('&') {
        return Err(Error::msg("invalid model parameter"));
    }

    if is_vertex {
        if model_id.starts_with("publishers/") || model_id.starts_with("projects/") || model_id.starts_with("models/") {
            Ok(model_id.to_owned())
        } else if let Some((publisher, model)) = model_id.split_once('/') {
            Ok(format!("publishers/{publisher}/models/{model}"))
        } else {
            Ok(format!("publishers/google/models/{model_id}"))
        }
    } else if model_id.starts_with("models/") || model_id.starts_with("tunedModels/") {
        Ok(model_id.to_owned())
    } else {
        Ok(format!("models/{model_id}"))
    }
}

fn create_google_http_error(
    status: u16,
    status_text: &str,
    content_type: Option<&str>,
    body_text: &str,
    headers: Headers,
) -> Error {
    let error_json = if content_type.is_some_and(|ct| ct.contains("application/json")) {
        if let Ok(parsed) = pi_js::json::parse::<JsonValue>(body_text) {
            pi_js::json::stringify(&parsed)
        } else {
            build_fallback_error_json(status, status_text, body_text)
        }
    } else {
        build_fallback_error_json(status, status_text, body_text)
    };

    if (400..600).contains(&status) {
        ProviderHttpError::new(Some(status), Some(headers), error_json).into_js_error()
    } else {
        Error::msg(error_json)
    }
}

fn build_fallback_error_json(status: u16, status_text: &str, body_text: &str) -> String {
    let mut inner = Map::new();
    inner.insert("message".to_owned(), Value::String(body_text.to_owned()));
    inner.insert("code".to_owned(), Value::from(status));
    inner.insert("status".to_owned(), Value::String(status_text.to_owned()));
    let mut wrapper = Map::new();
    wrapper.insert("error".to_owned(), Value::Object(inner));
    pi_js::json::stringify(&Value::Object(wrapper))
}

fn parse_stream_error_chunk(raw_chunk: &JsonValue) -> Option<Error> {
    let err_obj = raw_chunk.get("error")?.as_object()?;
    let status = err_obj.get("status").and_then(Value::as_str).unwrap_or("");
    let code = err_obj.get("code").and_then(Value::as_u64);
    let message = format!("got status: {}. {}", status, pi_js::json::stringify(raw_chunk));
    if let Some(code_num) = code
        && (400..600).contains(&code_num)
    {
        return Some(ProviderHttpError::new(Some(code_num as u16), None, message).into_js_error());
    }
    None
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleUsageMetadataWire {
    #[serde(default)]
    prompt_token_count: Option<i64>,
    #[serde(default)]
    candidates_token_count: Option<i64>,
    #[serde(default)]
    thoughts_token_count: Option<i64>,
    #[serde(default)]
    cached_content_token_count: Option<i64>,
    #[serde(default)]
    total_token_count: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleCandidateContentWire {
    #[serde(default)]
    parts: Vec<GooglePart>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleCandidateWire {
    #[serde(default)]
    content: Option<GoogleCandidateContentWire>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleStreamResponseWire {
    #[serde(default)]
    response_id: Option<String>,
    #[serde(default)]
    candidates: Vec<GoogleCandidateWire>,
    #[serde(default)]
    usage_metadata: Option<GoogleUsageMetadataWire>,
}

enum ActiveGoogleBlock {
    Text(usize),
    Thinking(usize),
}

fn flush_active_block(
    active: &mut Option<ActiveGoogleBlock>,
    output: &AssistantMessage,
    stream: &AssistantMessageEventStream,
) {
    let Some(block_ref) = active.take() else {
        return;
    };
    match block_ref {
        ActiveGoogleBlock::Text(idx) => {
            if let Some(AssistantContentBlock::Text(t)) = output.content.get(idx) {
                stream.push(AssistantMessageEvent::TextEnd {
                    content_index: idx,
                    content: t.text.clone(),
                    partial: output.clone(),
                });
            }
        }
        ActiveGoogleBlock::Thinking(idx) => {
            if let Some(AssistantContentBlock::Thinking(t)) = output.content.get(idx) {
                stream.push(AssistantMessageEvent::ThinkingEnd {
                    content_index: idx,
                    content: t.thinking.clone(),
                    partial: output.clone(),
                });
            }
        }
    }
}

pub(crate) async fn post_google_stream(
    url: String,
    headers: Headers,
    wire_body: JsonValue,
    signal: Option<AbortSignal>,
    retry_options: ProviderRetryOptions,
) -> Result<Response> {
    let body_text = pi_js::json::stringify(&wire_body);

    retry_google_request(
        move || {
            let req_url = url.clone();
            let req_headers = headers.clone();
            let req_body = body_text.clone();
            let req_signal = signal.clone();
            async move {
                let mut request = Request::post(req_url)
                    .with_headers(req_headers)
                    .body(Body::Text(req_body));
                if let Some(sig) = req_signal {
                    request = request.signal(sig);
                }
                let response = pi_js::fetch::fetch(request).await?;
                if !response.ok() {
                    let status = response.status();
                    let status_text = response.status_text().to_owned();
                    let resp_headers = response.headers().clone();
                    let content_type = resp_headers.get("content-type");
                    let err_text = response.text().await.unwrap_or_default();
                    return Err(create_google_http_error(
                        status,
                        &status_text,
                        content_type.as_deref(),
                        &err_text,
                        resp_headers,
                    ));
                }
                Ok(response)
            }
        },
        retry_options,
    )
    .await
}

pub(crate) struct GoogleStreamRequest {
    pub url: String,
    pub headers: Headers,
    pub wire_body: JsonValue,
    pub signal: Option<AbortSignal>,
    pub retry_options: ProviderRetryOptions,
    pub on_provider_stream_event: Option<OnProviderStreamEventFn>,
    pub missing_finish_reason_message: &'static str,
}

async fn run_google_sse_inner(
    model: &Model,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    req: GoogleStreamRequest,
) -> Result<()> {
    let response = post_google_stream(
        req.url,
        req.headers,
        req.wire_body,
        req.signal.clone(),
        req.retry_options,
    )
    .await?;

    if !response.has_body() {
        return Err(Error::msg(format!("{} response has no body", model.provider)));
    }

    let mut sse_stream = response.sse()?;
    stream.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });

    let mut active_block: Option<ActiveGoogleBlock> = None;

    loop {
        let next_event = if let Some(sig) = &req.signal {
            tokio::select! {
                biased;
                _ = sig.cancelled() => {
                    return Err(Error::msg("Request was aborted"));
                }
                item = sse_stream.next() => item,
            }
        } else {
            sse_stream.next().await
        };
        let Some(sse_result) = next_event else {
            break;
        };
        let sse = sse_result?;
        let data = pi_js::str16::trim(&sse.data);
        if data.is_empty() || data == "[DONE]" {
            continue;
        }

        let raw_chunk: JsonValue = pi_js::json::parse(data)?;
        if let Some(err) = parse_stream_error_chunk(&raw_chunk) {
            return Err(err);
        }

        if let Some(on_event) = &req.on_provider_stream_event {
            on_event(&raw_chunk, model).await?;
        }

        let chunk: GoogleStreamResponseWire =
            serde_json::from_value(raw_chunk).map_err(|e| Error::msg(format!("Invalid Google stream chunk: {e}")))?;

        if output.response_id.as_deref().is_none_or(str::is_empty)
            && let Some(resp_id) = chunk.response_id
            && !resp_id.is_empty()
        {
            output.response_id = Some(resp_id);
        }

        if let Some(candidate) = chunk.candidates.into_iter().next() {
            if let Some(content) = candidate.content {
                for part in content.parts {
                    if let Some(text) = &part.text {
                        let is_thinking = is_thinking_part(&part);
                        let needs_switch = match active_block {
                            None => true,
                            Some(ActiveGoogleBlock::Thinking(_)) => !is_thinking,
                            Some(ActiveGoogleBlock::Text(_)) => is_thinking,
                        };
                        if needs_switch {
                            flush_active_block(&mut active_block, output, stream);
                            let idx = output.content.len();
                            if is_thinking {
                                output
                                    .content
                                    .push(AssistantContentBlock::Thinking(ThinkingContent::new("")));
                                active_block = Some(ActiveGoogleBlock::Thinking(idx));
                                stream.push(AssistantMessageEvent::ThinkingStart {
                                    content_index: idx,
                                    partial: output.clone(),
                                });
                            } else {
                                output.content.push(AssistantContentBlock::Text(TextContent::new("")));
                                active_block = Some(ActiveGoogleBlock::Text(idx));
                                stream.push(AssistantMessageEvent::TextStart {
                                    content_index: idx,
                                    partial: output.clone(),
                                });
                            }
                        }

                        match active_block {
                            Some(ActiveGoogleBlock::Thinking(idx)) => {
                                if let Some(AssistantContentBlock::Thinking(th)) = output.content.get_mut(idx) {
                                    th.thinking.push_str(text);
                                    th.thinking_signature = retain_thought_signature(
                                        th.thinking_signature.as_deref(),
                                        part.thought_signature.as_deref(),
                                    );
                                }
                                stream.push(AssistantMessageEvent::ThinkingDelta {
                                    content_index: idx,
                                    delta: text.clone(),
                                    partial: output.clone(),
                                });
                            }
                            Some(ActiveGoogleBlock::Text(idx)) => {
                                if let Some(AssistantContentBlock::Text(t)) = output.content.get_mut(idx) {
                                    t.text.push_str(text);
                                    t.text_signature = retain_thought_signature(
                                        t.text_signature.as_deref(),
                                        part.thought_signature.as_deref(),
                                    );
                                }
                                stream.push(AssistantMessageEvent::TextDelta {
                                    content_index: idx,
                                    delta: text.clone(),
                                    partial: output.clone(),
                                });
                            }
                            None => {}
                        }
                    }

                    if let Some(fc) = part.function_call {
                        flush_active_block(&mut active_block, output, stream);
                        let provided_id = fc.id.filter(|id| !id.is_empty());
                        let is_duplicate = provided_id.as_ref().is_some_and(|pid| {
                            output
                                .content
                                .iter()
                                .any(|b| matches!(b, AssistantContentBlock::ToolCall(tc) if tc.id == *pid))
                        });
                        let tool_call_id = match provided_id {
                            Some(pid) if !is_duplicate => pid,
                            _ => {
                                let seq = TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
                                format!("{}_{}_{}", fc.name, pi_js::time::now_ms(), seq)
                            }
                        };

                        let mut tool_call = ToolCall::new(tool_call_id, fc.name, fc.args);
                        if let Some(sig) = part.thought_signature
                            && !sig.is_empty()
                        {
                            tool_call.thought_signature = Some(sig);
                        }

                        let idx = output.content.len();
                        let args_delta = pi_js::json::stringify(&tool_call.arguments);
                        output.content.push(AssistantContentBlock::ToolCall(tool_call.clone()));
                        stream.push(AssistantMessageEvent::ToolCallStart {
                            content_index: idx,
                            partial: output.clone(),
                        });
                        stream.push(AssistantMessageEvent::ToolCallDelta {
                            content_index: idx,
                            delta: args_delta,
                            partial: output.clone(),
                        });
                        stream.push(AssistantMessageEvent::ToolCallEnd {
                            content_index: idx,
                            tool_call,
                            partial: output.clone(),
                        });
                    }
                }
            }

            if let Some(finish_reason) = candidate.finish_reason {
                output.raw_stop_reason = Some(finish_reason.clone());
                output.stop_reason = map_stop_reason_string(&finish_reason);
                if output.stop_reason == StopReason::Stop
                    && output
                        .content
                        .iter()
                        .any(|b| matches!(b, AssistantContentBlock::ToolCall(_)))
                {
                    output.stop_reason = StopReason::ToolUse;
                }
            }
        }

        if let Some(usage_meta) = chunk.usage_metadata {
            let prompt = usage_meta.prompt_token_count.unwrap_or(0);
            let cached = usage_meta.cached_content_token_count.unwrap_or(0);
            let candidates = usage_meta.candidates_token_count.unwrap_or(0);
            let thoughts = usage_meta.thoughts_token_count.unwrap_or(0);
            let total = usage_meta.total_token_count.unwrap_or(0);
            output.usage = Usage {
                input: prompt - cached,
                output: candidates + thoughts,
                cache_read: cached,
                cache_write: 0,
                cache_write_1h: None,
                reasoning: Some(thoughts),
                total_tokens: total,
                cost: UsageCost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    total: 0.0,
                },
            };
            calculate_cost(&AnyModel::Chat(model.clone()), &mut output.usage);
        }
    }

    flush_active_block(&mut active_block, output, stream);

    if req.signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Err(Error::msg("Request was aborted"));
    }

    match output.stop_reason {
        StopReason::Pending => return Err(Error::msg(req.missing_finish_reason_message)),
        StopReason::Aborted | StopReason::Error => {
            let msg = match &output.raw_stop_reason {
                Some(raw) => format!("Provider stopped with: {raw}"),
                None => "An unknown error occurred".to_owned(),
            };
            return Err(Error::msg(msg));
        }
        _ => {}
    }

    stream.push(AssistantMessageEvent::Done {
        reason: output.stop_reason,
        message: output.clone(),
    });
    stream.end(Some(output.clone()));
    Ok(())
}

pub(crate) fn spawn_google_stream<PrepareFn>(
    model: Model,
    signal: Option<AbortSignal>,
    prepare: PrepareFn,
) -> AssistantMessageEventStream
where
    PrepareFn: FnOnce() -> BoxFuture<Result<GoogleStreamRequest>> + Send + 'static,
{
    let stream = AssistantMessageEventStream::new();
    let stream_clone = stream.clone();

    tokio::spawn(async move {
        let mut output = AssistantMessage {
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
        };

        let result: Result<()> = async {
            let req = prepare().await?;
            run_google_sse_inner(&model, &mut output, &stream_clone, req).await
        }
        .await;

        if let Err(err) = result {
            let aborted = signal.as_ref().is_some_and(AbortSignal::aborted);
            output.stop_reason = if aborted {
                StopReason::Aborted
            } else {
                StopReason::Error
            };
            let norm = normalize_provider_error(&err);
            output.error_message = Some(format_provider_error(&norm, None));
            stream_clone.push(AssistantMessageEvent::Error {
                reason: output.stop_reason,
                error: output.clone(),
            });
            stream_clone.end(Some(output));
        }
    });

    stream
}

pub(crate) async fn apply_on_payload(
    mut params: JsonValue,
    on_payload: Option<&OnPayloadFn<Model>>,
    model: &Model,
) -> Result<JsonValue> {
    if let Some(cb) = on_payload
        && let Some(next) = cb(params.clone(), model).await?
    {
        params = next;
    }
    Ok(params)
}

pub(crate) fn compute_google_budget(
    model: &Model,
    level: ResolvedGoogleThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
    flash_lite_minimal: Option<i64>,
) -> i64 {
    if let Some(budgets) = custom_budgets {
        let custom = match level {
            ResolvedGoogleThinkingLevel::Minimal => budgets.minimal,
            ResolvedGoogleThinkingLevel::Low => budgets.low,
            ResolvedGoogleThinkingLevel::Medium => budgets.medium,
            ResolvedGoogleThinkingLevel::High => budgets.high,
        };
        if let Some(tokens) = custom {
            return tokens;
        }
    }

    if model.id.contains("2.5-pro") {
        return match level {
            ResolvedGoogleThinkingLevel::Minimal => 128,
            ResolvedGoogleThinkingLevel::Low => 2048,
            ResolvedGoogleThinkingLevel::Medium => 8192,
            ResolvedGoogleThinkingLevel::High => 32768,
        };
    }

    if let Some(minimal_tokens) = flash_lite_minimal
        && model.id.contains("2.5-flash-lite")
    {
        return match level {
            ResolvedGoogleThinkingLevel::Minimal => minimal_tokens,
            ResolvedGoogleThinkingLevel::Low => 2048,
            ResolvedGoogleThinkingLevel::Medium => 8192,
            ResolvedGoogleThinkingLevel::High => 24576,
        };
    }

    if model.id.contains("2.5-flash") {
        return match level {
            ResolvedGoogleThinkingLevel::Minimal => 128,
            ResolvedGoogleThinkingLevel::Low => 2048,
            ResolvedGoogleThinkingLevel::Medium => 8192,
            ResolvedGoogleThinkingLevel::High => 24576,
        };
    }

    -1
}

pub(crate) fn resolve_simple_google_thinking(
    model: &Model,
    reasoning: Option<ThinkingLevel>,
    custom_budgets: Option<&ThinkingBudgets>,
    flash_lite_minimal: Option<i64>,
) -> Result<GoogleThinkingOptions> {
    let Some(requested) = reasoning else {
        return Ok(GoogleThinkingOptions {
            enabled: false,
            ..Default::default()
        });
    };

    let clamped = clamp_thinking_level(model, ModelThinkingLevel::from(requested));
    let Some(clamped_thinking) = model_level_to_thinking_level(clamped) else {
        return Ok(GoogleThinkingOptions {
            enabled: false,
            ..Default::default()
        });
    };

    let resolved_level = resolve_google_thinking_level(model, clamped_thinking)?;
    if uses_google_thinking_level(model) {
        return Ok(GoogleThinkingOptions {
            enabled: true,
            budget_tokens: None,
            level: Some(to_google_thinking_level(resolved_level)),
        });
    }

    Ok(GoogleThinkingOptions {
        enabled: true,
        budget_tokens: Some(compute_google_budget(
            model,
            resolved_level,
            custom_budgets,
            flash_lite_minimal,
        )),
        level: None,
    })
}
