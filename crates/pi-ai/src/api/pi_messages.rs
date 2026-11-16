//! Port of packages/ai/src/api/pi-messages.ts

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use futures::StreamExt;
use indexmap::IndexMap;
use pi_js::fetch::{Body, Headers, Request, Response};
use pi_js::{Error, Result};
use serde::de::{self, Deserializer};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};
use url::Url;

use crate::types::{
    ApiStreamOptions, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, CacheRetention, JsonObject,
    JsonValue, Model, ProviderEnv, ProviderResponse, ProviderStreams, SimpleStreamOptions, StopReason, StreamOptions,
    TextContent, ThinkingContent, ThinkingLevel, ToolCall, ToolChoice, TranscriptContext, Usage,
};
use crate::utils::diagnostics::{
    AssistantMessageDiagnostic, append_assistant_message_diagnostic, create_assistant_message_diagnostic,
};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::headers::{headers_to_record, provider_headers_to_record};
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::provider_env::get_provider_env_value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PiMessagesToolChoice {
    Auto,
    None,
    Required,
    Function { name: String },
}

impl From<ToolChoice> for PiMessagesToolChoice {
    fn from(value: ToolChoice) -> Self {
        match value {
            ToolChoice::Auto => Self::Auto,
            ToolChoice::None => Self::None,
        }
    }
}

impl Serialize for PiMessagesToolChoice {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Auto => serializer.serialize_str("auto"),
            Self::None => serializer.serialize_str("none"),
            Self::Required => serializer.serialize_str("required"),
            Self::Function { name } => {
                let mut fn_obj = Map::new();
                fn_obj.insert("name".to_string(), Value::String(name.clone()));
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "function")?;
                map.serialize_entry("function", &Value::Object(fn_obj))?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for PiMessagesToolChoice {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let val = Value::deserialize(deserializer)?;
        match &val {
            Value::String(s) => match s.as_str() {
                "auto" => Ok(Self::Auto),
                "none" => Ok(Self::None),
                "required" => Ok(Self::Required),
                other => Err(de::Error::custom(format!("unknown pi-messages toolChoice: {other}"))),
            },
            Value::Object(obj) => {
                if obj.get("type").and_then(Value::as_str) != Some("function") {
                    return Err(de::Error::custom(
                        "pi-messages toolChoice object must have type \"function\"",
                    ));
                }
                let name = obj
                    .get("function")
                    .and_then(Value::as_object)
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| de::Error::missing_field("function.name"))?;
                Ok(Self::Function { name: name.to_string() })
            }
            _ => Err(de::Error::custom(
                "pi-messages toolChoice must be a string or function object",
            )),
        }
    }
}

#[derive(Clone, Default, Debug)]
pub struct PiMessagesOptions {
    pub base: StreamOptions,
    pub reasoning: Option<ThinkingLevel>,
    pub tool_choice: Option<PiMessagesToolChoice>,
    pub debug: Option<bool>,
}

impl Deref for PiMessagesOptions {
    type Target = StreamOptions;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for PiMessagesOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl From<StreamOptions> for PiMessagesOptions {
    fn from(base: StreamOptions) -> Self {
        Self {
            base,
            ..Default::default()
        }
    }
}

impl From<PiMessagesOptions> for ApiStreamOptions {
    fn from(options: PiMessagesOptions) -> Self {
        let mut extra = JsonObject::new();
        if let Some(reasoning) = options.reasoning {
            extra.insert("reasoning".to_string(), Value::String(reasoning.as_str().to_string()));
        }
        if let Some(tool_choice) = &options.tool_choice {
            extra.insert("toolChoice".to_string(), to_json_value(tool_choice));
        }
        if let Some(debug) = options.debug {
            extra.insert("debug".to_string(), Value::Bool(debug));
        }
        Self {
            base: options.base,
            extra,
            extension: None,
        }
    }
}

impl PiMessagesOptions {
    fn from_api_options(options: Option<ApiStreamOptions>) -> Self {
        let Some(opts) = options else {
            return Self::default();
        };
        let reasoning = opts
            .extra
            .get("reasoning")
            .cloned()
            .and_then(|v| serde_json::from_value::<ThinkingLevel>(v).ok());
        let tool_choice = opts
            .extra
            .get("toolChoice")
            .cloned()
            .and_then(|v| serde_json::from_value::<PiMessagesToolChoice>(v).ok());
        let debug = opts.extra.get("debug").and_then(Value::as_bool);
        Self {
            base: opts.base,
            reasoning,
            tool_choice,
            debug,
        }
    }
}

/// Impact summary of a server-side message rewrite (e.g. a gateway policy).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiMessagesRewriteImpact {
    pub policy_id: String,
    pub policy_version: i64,
    pub changed: bool,
    pub token_count_change: i64,
    pub message_count_change: i64,
    pub system_prompt_changed: bool,
}

/// Serialized assistant-message event as sent by a pi-messages backend.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PiMessagesEvent {
    #[serde(rename = "start")]
    Start,
    #[serde(rename = "text_start")]
    TextStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
    },
    #[serde(rename = "text_delta")]
    TextDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
    },
    #[serde(rename = "text_end")]
    TextEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        content: String,
        #[serde(rename = "contentSignature", default, skip_serializing_if = "Option::is_none")]
        content_signature: Option<String>,
    },
    #[serde(rename = "thinking_start")]
    ThinkingStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
    },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
    },
    #[serde(rename = "thinking_end")]
    ThinkingEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        content: String,
        #[serde(rename = "contentSignature", default, skip_serializing_if = "Option::is_none")]
        content_signature: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redacted: Option<bool>,
    },
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
    },
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
    },
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        #[serde(rename = "toolCall")]
        tool_call: ToolCall,
    },
    #[serde(rename = "done")]
    Done {
        reason: StopReason,
        usage: Usage,
        #[serde(rename = "responseId", default, skip_serializing_if = "Option::is_none")]
        response_id: Option<String>,
        #[serde(rename = "providerThinkingLevel", default, skip_serializing_if = "Option::is_none")]
        provider_thinking_level: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rewrite: Option<PiMessagesRewriteImpact>,
    },
    #[serde(rename = "error")]
    Error {
        reason: StopReason,
        usage: Usage,
        #[serde(rename = "errorMessage", default, skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        #[serde(rename = "responseId", default, skip_serializing_if = "Option::is_none")]
        response_id: Option<String>,
        #[serde(rename = "providerThinkingLevel", default, skip_serializing_if = "Option::is_none")]
        provider_thinking_level: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rewrite: Option<PiMessagesRewriteImpact>,
    },
}

#[derive(Clone, Debug, Deserialize)]
struct PiMessagesErrorPayload {
    #[serde(default)]
    message: Option<JsonValue>,
    #[serde(default)]
    code: Option<JsonValue>,
}

#[derive(Debug)]
pub struct PiMessagesResponseError {
    pub message: String,
    pub code: Option<String>,
    pub diagnostic_details: JsonObject,
}

impl fmt::Display for PiMessagesResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PiMessagesResponseError {}

fn to_json_value<T: Serialize + ?Sized>(value: &T) -> JsonValue {
    let serialized = pi_js::json::stringify(value);
    pi_js::json::parse(&serialized).unwrap_or(JsonValue::Null)
}

fn to_json_object<T: Serialize + ?Sized>(value: &T) -> JsonObject {
    match to_json_value(value) {
        Value::Object(map) => map.into_iter().collect(),
        _ => JsonObject::new(),
    }
}

fn parse_pi_messages_error_body(body: &str) -> Option<(JsonObject, PiMessagesErrorPayload)> {
    let parsed: JsonValue = pi_js::json::parse(body).ok()?;
    let error_val = parsed.get("error")?.as_object()?;
    let mut obj = JsonObject::new();
    for (k, v) in error_val {
        obj.insert(k.clone(), v.clone());
    }
    let payload: PiMessagesErrorPayload = serde_json::from_value(Value::Object(error_val.clone())).ok()?;
    Some((obj, payload))
}

fn truncate_diagnostic_string(value: &str) -> String {
    const MAX_LENGTH: usize = 8192;
    if pi_js::str16::len(value) > MAX_LENGTH {
        format!("{}…", pi_js::str16::slice(value, 0, Some(MAX_LENGTH as i64)))
    } else {
        value.to_string()
    }
}

fn format_pi_messages_response_error(
    response: &Response,
    body: &str,
    error_payload: Option<&PiMessagesErrorPayload>,
) -> String {
    let message = error_payload.and_then(|p| p.message.as_ref()).and_then(Value::as_str);
    let code = error_payload.and_then(|p| p.code.as_ref()).and_then(Value::as_str);
    let suffix = message.unwrap_or(body);
    let code_suffix = code.map(|c| format!(" ({c})")).unwrap_or_default();
    format!(
        "{} {}: {}{}",
        response.status(),
        response.status_text(),
        suffix,
        code_suffix
    )
}

fn create_pi_messages_response_error(model: &Model, url: &Url, response: &Response, body: &str) -> Error {
    let parsed = parse_pi_messages_error_body(body);
    let message = format_pi_messages_response_error(response, body, parsed.as_ref().map(|(_, payload)| payload));
    let code = parsed
        .as_ref()
        .and_then(|(_, p)| p.code.as_ref())
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut details = JsonObject::new();
    details.insert("version".to_string(), Value::from(1));
    details.insert("provider".to_string(), Value::String(model.provider.clone()));
    details.insert("model".to_string(), Value::String(model.id.clone()));
    details.insert("url".to_string(), Value::String(url.to_string()));
    details.insert("status".to_string(), Value::from(response.status()));
    details.insert(
        "statusText".to_string(),
        Value::String(response.status_text().to_string()),
    );
    if let Some((err_obj, _)) = parsed {
        details.insert("error".to_string(), Value::Object(err_obj.into_iter().collect()));
    } else {
        details.insert("body".to_string(), Value::String(truncate_diagnostic_string(body)));
    }
    details.insert("timestampMs".to_string(), Value::from(pi_js::time::now_ms()));

    Error::typed(
        "PiMessagesResponseError",
        PiMessagesResponseError {
            message,
            code,
            diagnostic_details: details,
        },
    )
}

fn append_rewrite_diagnostic(message: &mut AssistantMessage, rewrite: Option<&PiMessagesRewriteImpact>) {
    let Some(rewrite) = rewrite else {
        return;
    };
    append_assistant_message_diagnostic(
        message,
        AssistantMessageDiagnostic {
            r#type: "pi_messages_rewrite".to_string(),
            timestamp: pi_js::time::now_ms(),
            error: None,
            details: Some(to_json_object(rewrite)),
        },
    );
}

struct EventConverter {
    partial: AssistantMessage,
    tool_json: IndexMap<usize, String>,
}

impl EventConverter {
    fn new(model: &Model) -> Self {
        Self {
            partial: AssistantMessage {
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
            },
            tool_json: IndexMap::new(),
        }
    }

    fn set_slot(&mut self, index: usize, block: AssistantContentBlock) {
        if index < self.partial.content.len() {
            self.partial.content[index] = block;
        } else {
            while self.partial.content.len() < index {
                self.partial
                    .content
                    .push(AssistantContentBlock::Text(TextContent::new("")));
            }
            self.partial.content.push(block);
        }
    }

    fn convert(&mut self, event: PiMessagesEvent) -> Result<AssistantMessageEvent> {
        match event {
            PiMessagesEvent::Done {
                reason,
                usage,
                response_id,
                provider_thinking_level,
                rewrite,
            } => {
                self.partial.stop_reason = reason;
                self.partial.usage = usage;
                self.partial.response_id = response_id;
                if let Some(level) = provider_thinking_level {
                    self.partial.provider_thinking_level = Some(level);
                }
                append_rewrite_diagnostic(&mut self.partial, rewrite.as_ref());
                Ok(AssistantMessageEvent::Done {
                    reason,
                    message: self.partial.clone(),
                })
            }
            PiMessagesEvent::Error {
                reason,
                usage,
                error_message,
                response_id,
                provider_thinking_level,
                rewrite,
            } => {
                self.partial.stop_reason = reason;
                self.partial.usage = usage;
                self.partial.error_message = error_message;
                self.partial.response_id = response_id;
                if let Some(level) = provider_thinking_level {
                    self.partial.provider_thinking_level = Some(level);
                }
                append_rewrite_diagnostic(&mut self.partial, rewrite.as_ref());
                Ok(AssistantMessageEvent::Error {
                    reason,
                    error: self.partial.clone(),
                })
            }
            PiMessagesEvent::Start => Ok(AssistantMessageEvent::Start {
                partial: self.partial.clone(),
            }),
            PiMessagesEvent::TextStart { content_index } => {
                self.set_slot(content_index, AssistantContentBlock::Text(TextContent::new("")));
                Ok(AssistantMessageEvent::TextStart {
                    content_index,
                    partial: self.partial.clone(),
                })
            }
            PiMessagesEvent::TextDelta { content_index, delta } => {
                let Some(AssistantContentBlock::Text(t)) = self.partial.content.get_mut(content_index) else {
                    return Err(Error::msg(format!(
                        "Unexpected text_delta at contentIndex {content_index}"
                    )));
                };
                t.text.push_str(&delta);
                Ok(AssistantMessageEvent::TextDelta {
                    content_index,
                    delta,
                    partial: self.partial.clone(),
                })
            }
            PiMessagesEvent::TextEnd {
                content_index,
                content,
                content_signature,
            } => {
                let Some(AssistantContentBlock::Text(t)) = self.partial.content.get_mut(content_index) else {
                    return Err(Error::msg(format!(
                        "Unexpected text_end at contentIndex {content_index}"
                    )));
                };
                t.text = content.clone();
                t.text_signature = content_signature;
                Ok(AssistantMessageEvent::TextEnd {
                    content_index,
                    content,
                    partial: self.partial.clone(),
                })
            }
            PiMessagesEvent::ThinkingStart { content_index } => {
                self.set_slot(content_index, AssistantContentBlock::Thinking(ThinkingContent::new("")));
                Ok(AssistantMessageEvent::ThinkingStart {
                    content_index,
                    partial: self.partial.clone(),
                })
            }
            PiMessagesEvent::ThinkingDelta { content_index, delta } => {
                let Some(AssistantContentBlock::Thinking(t)) = self.partial.content.get_mut(content_index) else {
                    return Err(Error::msg(format!(
                        "Unexpected thinking_delta at contentIndex {content_index}"
                    )));
                };
                t.thinking.push_str(&delta);
                Ok(AssistantMessageEvent::ThinkingDelta {
                    content_index,
                    delta,
                    partial: self.partial.clone(),
                })
            }
            PiMessagesEvent::ThinkingEnd {
                content_index,
                content,
                content_signature,
                redacted,
            } => {
                let Some(AssistantContentBlock::Thinking(t)) = self.partial.content.get_mut(content_index) else {
                    return Err(Error::msg(format!(
                        "Unexpected thinking_end at contentIndex {content_index}"
                    )));
                };
                t.thinking = content.clone();
                t.thinking_signature = content_signature;
                t.redacted = redacted;
                Ok(AssistantMessageEvent::ThinkingEnd {
                    content_index,
                    content,
                    partial: self.partial.clone(),
                })
            }
            PiMessagesEvent::ToolCallStart {
                content_index,
                id,
                tool_name,
            } => {
                self.set_slot(
                    content_index,
                    AssistantContentBlock::ToolCall(ToolCall::new(id, tool_name, JsonObject::new())),
                );
                self.tool_json.insert(content_index, String::new());
                Ok(AssistantMessageEvent::ToolCallStart {
                    content_index,
                    partial: self.partial.clone(),
                })
            }
            PiMessagesEvent::ToolCallDelta { content_index, delta } => {
                let json_buf = self.tool_json.entry(content_index).or_default();
                json_buf.push_str(&delta);
                let parsed = parse_streaming_json(Some(json_buf));
                let Some(AssistantContentBlock::ToolCall(tc)) = self.partial.content.get_mut(content_index) else {
                    return Err(Error::msg(format!(
                        "Unexpected toolcall_delta at contentIndex {content_index}"
                    )));
                };
                tc.arguments = parsed;
                Ok(AssistantMessageEvent::ToolCallDelta {
                    content_index,
                    delta,
                    partial: self.partial.clone(),
                })
            }
            PiMessagesEvent::ToolCallEnd {
                content_index,
                tool_call,
            } => {
                let Some(slot) = self.partial.content.get_mut(content_index) else {
                    return Err(Error::msg(format!(
                        "Unexpected toolcall_end at contentIndex {content_index}"
                    )));
                };
                *slot = AssistantContentBlock::ToolCall(tool_call.clone());
                self.tool_json.shift_remove(&content_index);
                Ok(AssistantMessageEvent::ToolCallEnd {
                    content_index,
                    tool_call,
                    partial: self.partial.clone(),
                })
            }
        }
    }
}

fn create_error_event(model: &Model, error: &Error, aborted: bool) -> AssistantMessageEvent {
    let reason = if aborted {
        StopReason::Aborted
    } else {
        StopReason::Error
    };
    let mut assistant_message = AssistantMessage {
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
        stop_reason: reason,
        deferred: None,
        error_message: Some(error.to_string()),
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
    };

    if !aborted {
        if let Some(resp_err) = error.downcast_ref::<PiMessagesResponseError>() {
            append_assistant_message_diagnostic(
                &mut assistant_message,
                create_assistant_message_diagnostic(
                    "pi_messages_response_failure",
                    error,
                    Some(resp_err.diagnostic_details.clone()),
                ),
            );
        }
    }

    AssistantMessageEvent::Error {
        reason,
        error: assistant_message,
    }
}

fn resolve_cache_retention(
    cache_retention: Option<CacheRetention>,
    env: Option<&ProviderEnv>,
) -> Option<CacheRetention> {
    if cache_retention.is_some() {
        return cache_retention;
    }
    if get_provider_env_value("PI_CACHE_RETENTION", env).as_deref() == Some("long") {
        Some(CacheRetention::Long)
    } else {
        None
    }
}

fn build_request_payload(model: &Model, context: &TranscriptContext, options: &PiMessagesOptions) -> JsonValue {
    let mut opts_map = Map::new();
    if let Some(temp) = options.base.temperature {
        opts_map.insert("temperature".to_string(), pi_js::json::number_value(temp));
    }
    if let Some(max_tokens) = options.base.max_tokens {
        opts_map.insert("maxTokens".to_string(), Value::from(max_tokens));
    }
    if let Some(reasoning) = options.reasoning {
        opts_map.insert("reasoning".to_string(), Value::String(reasoning.as_str().to_string()));
    }
    if let Some(cache_retention) = resolve_cache_retention(options.base.cache_retention, options.base.env.as_ref()) {
        opts_map.insert("cacheRetention".to_string(), to_json_value(&cache_retention));
    }
    if let Some(session_id) = &options.base.session_id {
        opts_map.insert("sessionId".to_string(), Value::String(session_id.clone()));
    }
    if let Some(tool_choice) = &options.tool_choice {
        opts_map.insert("toolChoice".to_string(), to_json_value(tool_choice));
    }

    let mut root = Map::new();
    root.insert("model".to_string(), Value::String(model.id.clone()));
    root.insert("context".to_string(), to_json_value(context));
    root.insert("options".to_string(), Value::Object(opts_map));
    Value::Object(root)
}

async fn run_pi_messages_stream(
    model: Model,
    context: TranscriptContext,
    options: PiMessagesOptions,
    event_stream: AssistantMessageEventStream,
) -> Result<()> {
    let api_key = options
        .base
        .api_key
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::msg(format!("No API key provided for provider \"{}\"", model.provider)))?;

    let base_trimmed = model.base_url.trim_end_matches('/');
    let mut url = Url::parse(&format!("{base_trimmed}/messages"))
        .map_err(|e| Error::msg(format!("Invalid pi-messages URL: {e}")))?;
    if options.debug == Some(true) {
        url.query_pairs_mut().append_pair("debug", "1");
    }

    let mut payload = build_request_payload(&model, &context, &options);
    if let Some(on_payload) = &options.base.on_payload {
        if let Some(next_payload) = on_payload(payload.clone(), &model).await? {
            payload = next_payload;
        }
    }

    let mut headers = Headers::new();
    headers.set("authorization", &format!("Bearer {api_key}"));
    headers.set("accept", "text/event-stream");
    headers.set("content-type", "application/json");
    if let Some(extra_headers) = provider_headers_to_record([options.base.headers.as_ref()]) {
        for (name, value) in extra_headers {
            headers.set(&name, &value);
        }
    }

    let mut request = Request::post(url.to_string())
        .with_headers(headers)
        .body(Body::Text(pi_js::json::stringify(&payload)));
    if let Some(signal) = &options.base.signal {
        request = request.signal(signal.clone());
    }

    let response = if let Some(custom_fetch) = &options.base.fetch {
        custom_fetch(request).await?
    } else {
        pi_js::fetch::fetch(request).await?
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
        let body = response.text().await?;
        return Err(create_pi_messages_response_error(&model, &url, &response, &body));
    }

    if !response.has_body() {
        return Err(Error::msg(format!("{} response has no body", model.provider)));
    }

    let mut sse_stream = response.sse()?;
    let mut converter = EventConverter::new(&model);

    while let Some(sse_result) = sse_stream.next().await {
        let sse = sse_result?;
        let data = pi_js::str16::trim(&sse.data);
        if data.is_empty() || data == "[DONE]" {
            continue;
        }

        let raw_json: JsonValue = pi_js::json::parse(data)?;
        if let Some(on_event) = &options.base.on_provider_stream_event {
            on_event(&raw_json, &model).await?;
        }
        let pi_event: PiMessagesEvent =
            serde_json::from_value(raw_json).map_err(|e| Error::msg(format!("Invalid pi-messages SSE event: {e}")))?;
        let event = converter.convert(pi_event)?;
        let is_terminal = matches!(
            event,
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
        );
        event_stream.push(event);
        if is_terminal {
            return Ok(());
        }
    }

    Err(Error::msg(format!(
        "{} stream ended without a terminal event",
        model.provider
    )))
}

pub fn stream_with_options(
    model: &Model,
    context: &TranscriptContext,
    options: Option<PiMessagesOptions>,
) -> AssistantMessageEventStream {
    let event_stream = AssistantMessageEventStream::new();
    let stream_clone = event_stream.clone();
    let model_owned = model.clone();
    let context_owned = context.clone();
    let options_owned = options.unwrap_or_default();

    tokio::spawn(async move {
        let signal = options_owned.base.signal.clone();
        if let Err(err) =
            run_pi_messages_stream(model_owned.clone(), context_owned, options_owned, stream_clone.clone()).await
        {
            let aborted = signal.as_ref().is_some_and(|s| s.aborted());
            stream_clone.push(create_error_event(&model_owned, &err, aborted));
        }
    });

    event_stream
}

pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: Option<ApiStreamOptions>,
) -> AssistantMessageEventStream {
    stream_with_options(model, context, Some(PiMessagesOptions::from_api_options(options)))
}

pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let opts = options.map(|o| PiMessagesOptions {
        base: o.base,
        reasoning: o.reasoning,
        tool_choice: o.tool_choice.map(Into::into),
        debug: None,
    });
    stream_with_options(model, context, opts)
}

pub fn provider_streams() -> ProviderStreams {
    ProviderStreams {
        stream: Arc::new(|model, context, options| stream(model, context, options)),
        stream_simple: Arc::new(|model, context, options| stream_simple(model, context, options)),
        fetch_deferred: None,
        cancel_deferred: None,
    }
}
