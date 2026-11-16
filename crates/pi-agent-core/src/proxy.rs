//! Port of packages/agent/src/proxy.ts

use futures::StreamExt;
use indexmap::IndexMap;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, Context,
    FauxAssistantMessageContent, FauxToolCallOptions, JsonObject, Model, OnPayloadFn, SimpleStreamOptions, StopReason,
    ThinkingBudgets, Transport, Usage, create_assistant_message_event_stream, faux_assistant_message, faux_text,
    faux_thinking, faux_tool_call, parse_streaming_json,
};
use pi_js::abort::AbortSignal;
use pi_js::fetch::{Headers, Request, fetch};
use pi_js::text::Utf8StreamDecoder;
use pi_js::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::ThinkingLevel;

/// Event format emitted by the pi proxy server over SSE.
///
/// Mirrors `AssistantMessageEvent` without carrying full `partial` snapshots on every chunk.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProxyAssistantMessageEvent {
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
        #[serde(default, rename = "contentSignature", skip_serializing_if = "Option::is_none")]
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
        #[serde(default, rename = "contentSignature", skip_serializing_if = "Option::is_none")]
        content_signature: Option<String>,
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
        #[serde(default, rename = "toolCall", skip_serializing_if = "Option::is_none")]
        tool_call: Option<ToolCall>,
    },
    #[serde(rename = "done")]
    Done {
        reason: StopReason,
        usage: Usage,
        #[serde(default, rename = "providerThinkingLevel", skip_serializing_if = "Option::is_none")]
        provider_thinking_level: Option<String>,
    },
    #[serde(rename = "error")]
    Error {
        reason: StopReason,
        #[serde(default, rename = "errorMessage", skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        usage: Usage,
        #[serde(default, rename = "providerThinkingLevel", skip_serializing_if = "Option::is_none")]
        provider_thinking_level: Option<String>,
    },
}

/// JSON-serializable subset of [`ProxyStreamOptions`] forwarded in the POST request body.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProxyRequestOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning: Option<pi_ai::ThinkingLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_retention: Option<pi_ai::CacheRetention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    headers: Option<IndexMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata: Option<IndexMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transport: Option<Transport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking_budgets: Option<ThinkingBudgets>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_retry_delay_ms: Option<i64>,
}

/// Request body posted to `<proxy_url>/api/stream`.
#[derive(Serialize)]
struct ProxyRequestBody {
    model: Model,
    context: Context,
    options: ProxyRequestOptions,
}

/// Options for [`stream_proxy`].
#[derive(Clone)]
pub struct ProxyStreamOptions {
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    pub signal: Option<AbortSignal>,
    pub cache_retention: Option<pi_ai::CacheRetention>,
    pub reasoning: Option<pi_ai::ThinkingLevel>,
    pub session_id: Option<String>,
    pub transport: Option<Transport>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub max_retry_delay_ms: Option<i64>,
    pub metadata: Option<IndexMap<String, Value>>,
    pub headers: Option<IndexMap<String, String>>,
    pub on_payload: Option<OnPayloadFn>,
    pub auth_token: String,
    pub proxy_url: String,
}

impl ProxyStreamOptions {
    /// Creates proxy stream options with required authentication token and proxy base URL.
    pub fn new(auth_token: impl Into<String>, proxy_url: impl Into<String>) -> Self {
        Self {
            temperature: None,
            max_tokens: None,
            signal: None,
            cache_retention: None,
            reasoning: None,
            session_id: None,
            transport: None,
            thinking_budgets: None,
            max_retry_delay_ms: None,
            metadata: None,
            headers: None,
            on_payload: None,
            auth_token: auth_token.into(),
            proxy_url: proxy_url.into(),
        }
    }

    /// Builds [`ProxyStreamOptions`] from existing [`SimpleStreamOptions`] plus proxy credentials.
    pub fn from_simple(
        simple: SimpleStreamOptions,
        auth_token: impl Into<String>,
        proxy_url: impl Into<String>,
    ) -> Self {
        Self {
            temperature: simple.temperature,
            max_tokens: simple.max_tokens,
            signal: simple.signal,
            cache_retention: simple.cache_retention,
            reasoning: simple.reasoning,
            session_id: simple.session_id,
            transport: simple.transport,
            thinking_budgets: simple.thinking_budgets,
            max_retry_delay_ms: simple.max_retry_delay_ms,
            metadata: simple.metadata,
            headers: simple.headers,
            on_payload: simple.on_payload,
            auth_token: auth_token.into(),
            proxy_url: proxy_url.into(),
        }
    }

    /// Converts an agent [`ThinkingLevel`] into `SimpleStreamOptions`-compatible reasoning.
    pub fn with_agent_reasoning(mut self, level: ThinkingLevel) -> Self {
        self.reasoning = level.to_ai_thinking_level();
        self
    }
}

/// Streams assistant responses through a backend pi proxy server (`/api/stream`).
///
/// The proxy handles provider credentials and strips `partial` payloads from SSE chunks;
/// this client reconstructs `partial` and streams standard [`AssistantMessageEvent`]s.
pub fn stream_proxy(model: Model, context: Context, options: ProxyStreamOptions) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let stream_task = stream.clone();

    tokio::spawn(async move {
        let mut partial = init_assistant_message(&model);
        let mut tool_call_json_buffers = IndexMap::<usize, String>::new();

        let result = run_proxy_stream(
            &model,
            context,
            &options,
            &stream_task,
            &mut partial,
            &mut tool_call_json_buffers,
        )
        .await;

        if let Err(err) = result {
            let error_msg = err.message();
            let stop_reason = if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
                StopReason::Aborted
            } else {
                StopReason::Error
            };
            partial.stop_reason = stop_reason;
            partial.error_message = Some(error_msg);
            stream_task.push(AssistantMessageEvent::Error {
                reason: stop_reason,
                error: partial.clone(),
            });
            stream_task.end(Some(partial));
        } else {
            stream_task.end(None);
        }
    });

    stream
}

async fn run_proxy_stream(
    model: &Model,
    context: Context,
    options: &ProxyStreamOptions,
    stream: &AssistantMessageEventStream,
    partial: &mut AssistantMessage,
    tool_call_json_buffers: &mut IndexMap<usize, String>,
) -> Result<()> {
    let endpoint = format!("{}/api/stream", options.proxy_url.trim_end_matches('/'));
    let payload = ProxyRequestBody {
        model: model.clone(),
        context,
        options: ProxyRequestOptions {
            temperature: options.temperature,
            max_tokens: options.max_tokens,
            reasoning: options.reasoning,
            cache_retention: options.cache_retention,
            session_id: options.session_id.clone(),
            headers: options.headers.clone(),
            metadata: options.metadata.clone(),
            transport: options.transport,
            thinking_budgets: options.thinking_budgets.clone(),
            max_retry_delay_ms: options.max_retry_delay_ms,
        },
    };
    let body = pi_js::json::try_stringify(&payload)?;

    let mut headers = Headers::new();
    headers.set("Authorization", &format!("Bearer {}", options.auth_token));
    headers.set("Content-Type", "application/json");

    let mut request = Request::post(endpoint).with_headers(headers).body(body);
    if let Some(signal) = &options.signal {
        request = request.signal(signal.clone());
    }

    let response = fetch(request).await?;
    if !response.ok() {
        let mut message = format!("Proxy error: {} {}", response.status(), response.status_text());
        if let Ok(text) = response.text().await
            && let Ok(parsed) = pi_js::json::parse(&text)
            && let Some(err_str) = parsed.get("error").and_then(Value::as_str)
        {
            message = format!("Proxy error: {err_str}");
        }
        return Err(Error::msg(message));
    }

    if !response.has_body() {
        return Err(Error::msg("No response body"));
    }

    let mut body_stream = response.body_stream()?;
    let mut decoder = Utf8StreamDecoder::new();
    let mut line_buffer = String::new();
    let mut saw_terminal_event = false;

    while let Some(chunk_res) = body_stream.next().await {
        if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Err(Error::msg("Request aborted by the user"));
        }
        let chunk = chunk_res?;
        line_buffer.push_str(&decoder.decode(&chunk, true));

        let mut start = 0;
        while let Some(rel_nl) = line_buffer[start..].find('\n') {
            let line_end = start + rel_nl;
            if process_sse_line(&line_buffer[start..line_end], partial, tool_call_json_buffers, stream)? {
                saw_terminal_event = true;
            }
            start = line_end + 1;
        }
        if start > 0 {
            line_buffer.drain(..start);
        }
    }

    if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Err(Error::msg("Request aborted by the user"));
    }

    let tail = decoder.decode(&[], false);
    if !tail.is_empty() {
        line_buffer.push_str(&tail);
    }
    if !line_buffer.is_empty() {
        for raw_line in line_buffer.split('\n') {
            if process_sse_line(raw_line, partial, tool_call_json_buffers, stream)? {
                saw_terminal_event = true;
            }
        }
    }

    if !saw_terminal_event {
        partial.stop_reason = StopReason::Error;
        partial.error_message = Some("Connection closed by proxy server before the response completed".to_string());
        stream.push(AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: partial.clone(),
        });
    }

    Ok(())
}

fn process_sse_line(
    raw_line: &str,
    partial: &mut AssistantMessage,
    tool_call_json_buffers: &mut IndexMap<usize, String>,
    stream: &AssistantMessageEventStream,
) -> Result<bool> {
    let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
    let Some(data) = line.strip_prefix("data: ") else {
        return Ok(false);
    };
    let trimmed = data.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return Ok(false);
    }
    let proxy_event: ProxyAssistantMessageEvent = pi_js::json::parse_as(trimmed)?;
    let Some(event) = process_proxy_event(proxy_event, partial, tool_call_json_buffers)? else {
        return Ok(false);
    };
    let is_terminal = matches!(
        event,
        AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
    );
    stream.push(event);
    Ok(is_terminal)
}

/// Applies one decoded [`ProxyAssistantMessageEvent`] to `partial`, tracking streamed tool-call JSON
/// buffers separately from `ToolCall` arguments.
pub fn process_proxy_event(
    proxy_event: ProxyAssistantMessageEvent,
    partial: &mut AssistantMessage,
    tool_call_json_buffers: &mut IndexMap<usize, String>,
) -> Result<Option<AssistantMessageEvent>> {
    match proxy_event {
        ProxyAssistantMessageEvent::Start => Ok(Some(AssistantMessageEvent::Start {
            partial: partial.clone(),
        })),
        ProxyAssistantMessageEvent::TextStart { content_index } => {
            set_content_slot(partial, content_index, AssistantContentBlock::Text(faux_text("")))?;
            Ok(Some(AssistantMessageEvent::TextStart {
                content_index,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::TextDelta { content_index, delta } => {
            let Some(AssistantContentBlock::Text(block)) = partial.content.get_mut(content_index) else {
                return Err(Error::msg("Received text_delta for non-text content"));
            };
            block.text.push_str(&delta);
            Ok(Some(AssistantMessageEvent::TextDelta {
                content_index,
                delta,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::TextEnd {
            content_index,
            content_signature,
        } => {
            let Some(AssistantContentBlock::Text(block)) = partial.content.get_mut(content_index) else {
                return Err(Error::msg("Received text_end for non-text content"));
            };
            block.text_signature = content_signature;
            let content = block.text.clone();
            Ok(Some(AssistantMessageEvent::TextEnd {
                content_index,
                content,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::ThinkingStart { content_index } => {
            set_content_slot(
                partial,
                content_index,
                AssistantContentBlock::Thinking(faux_thinking("")),
            )?;
            Ok(Some(AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::ThinkingDelta { content_index, delta } => {
            let Some(AssistantContentBlock::Thinking(block)) = partial.content.get_mut(content_index) else {
                return Err(Error::msg("Received thinking_delta for non-thinking content"));
            };
            block.thinking.push_str(&delta);
            Ok(Some(AssistantMessageEvent::ThinkingDelta {
                content_index,
                delta,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::ThinkingEnd {
            content_index,
            content_signature,
        } => {
            let Some(AssistantContentBlock::Thinking(block)) = partial.content.get_mut(content_index) else {
                return Err(Error::msg("Received thinking_end for non-thinking content"));
            };
            block.thinking_signature = content_signature;
            let content = block.thinking.clone();
            Ok(Some(AssistantMessageEvent::ThinkingEnd {
                content_index,
                content,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::ToolCallStart {
            content_index,
            id,
            tool_name,
        } => {
            let call = faux_tool_call(
                &tool_name,
                JsonObject::new(),
                Some(FauxToolCallOptions { id: Some(id) }),
            );
            set_content_slot(partial, content_index, AssistantContentBlock::ToolCall(call))?;
            tool_call_json_buffers.insert(content_index, String::new());
            Ok(Some(AssistantMessageEvent::ToolCallStart {
                content_index,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::ToolCallDelta { content_index, delta } => {
            let Some(AssistantContentBlock::ToolCall(call)) = partial.content.get_mut(content_index) else {
                return Err(Error::msg("Received toolcall_delta for non-toolCall content"));
            };
            let buf = tool_call_json_buffers.entry(content_index).or_default();
            buf.push_str(&delta);
            let parsed = parse_streaming_json(Some(buf.as_str()));
            call.arguments = value_to_json_object(parsed);
            Ok(Some(AssistantMessageEvent::ToolCallDelta {
                content_index,
                delta,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::ToolCallEnd {
            content_index,
            tool_call,
        } => {
            tool_call_json_buffers.shift_remove(&content_index);
            let Some(AssistantContentBlock::ToolCall(call)) = partial.content.get_mut(content_index) else {
                return Ok(None);
            };
            if let Some(updated) = tool_call {
                *call = updated;
            }
            let finalized = call.clone();
            Ok(Some(AssistantMessageEvent::ToolCallEnd {
                content_index,
                tool_call: finalized,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::Done {
            reason,
            usage,
            provider_thinking_level,
        } => {
            partial.stop_reason = reason;
            partial.usage = usage;
            if let Some(level) = provider_thinking_level {
                partial.provider_thinking_level = Some(level);
            }
            Ok(Some(AssistantMessageEvent::Done {
                reason,
                message: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::Error {
            reason,
            error_message,
            usage,
            provider_thinking_level,
        } => {
            partial.stop_reason = reason;
            partial.error_message = error_message;
            partial.usage = usage;
            if let Some(level) = provider_thinking_level {
                partial.provider_thinking_level = Some(level);
            }
            Ok(Some(AssistantMessageEvent::Error {
                reason,
                error: partial.clone(),
            }))
        }
    }
}

fn init_assistant_message(model: &Model) -> AssistantMessage {
    let mut msg = faux_assistant_message(FauxAssistantMessageContent::Blocks(Vec::new()), None);
    msg.api = model.api.clone();
    msg.provider = model.provider.clone();
    msg.model = model.id.clone();
    msg.stop_reason = StopReason::Stop;
    msg.error_message = None;
    msg.timestamp = pi_js::time::now_ms();
    msg
}

fn set_content_slot(partial: &mut AssistantMessage, index: usize, block: AssistantContentBlock) -> Result<()> {
    if index < partial.content.len() {
        partial.content[index] = block;
        Ok(())
    } else if index == partial.content.len() {
        partial.content.push(block);
        Ok(())
    } else {
        Err(Error::msg(format!(
            "Invalid contentIndex {index} for partial content of length {}",
            partial.content.len()
        )))
    }
}

fn value_to_json_object(value: Value) -> JsonObject {
    match value {
        Value::Object(map) => map.into_iter().collect(),
        _ => JsonObject::new(),
    }
}
