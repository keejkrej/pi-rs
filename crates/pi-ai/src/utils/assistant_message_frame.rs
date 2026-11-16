//! Port of packages/ai/src/utils/assistant-message-frame.ts

use std::borrow::Borrow;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, JsonObject, JsonValue, StopReason, TextContent,
    ThinkingContent, ToolCall,
};
use crate::utils::json_parse::parse_streaming_json;

/// Compact, replayable assistant-message progress. Terminal settlement is
/// intentionally excluded and must be persisted separately.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AssistantMessageFrame {
    #[serde(rename = "start")]
    Start { partial: AssistantMessage },
    #[serde(rename = "text_start")]
    TextStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        content: TextContent,
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
        #[serde(rename = "textSignature", default, skip_serializing_if = "Option::is_none")]
        text_signature: Option<String>,
    },
    #[serde(rename = "thinking_start")]
    ThinkingStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        content: ThinkingContent,
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
        #[serde(rename = "thinkingSignature", default, skip_serializing_if = "Option::is_none")]
        thinking_signature: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redacted: Option<bool>,
    },
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        #[serde(rename = "toolCall")]
        tool_call: ToolCall,
    },
    #[serde(rename = "toolcall_checkpoint")]
    ToolCallCheckpoint {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        json: String,
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
        id: String,
        name: String,
        arguments: JsonObject,
        #[serde(rename = "thoughtSignature", default, skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
}

impl AssistantMessageFrame {
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::Start { .. } => "start",
            Self::TextStart { .. } => "text_start",
            Self::TextDelta { .. } => "text_delta",
            Self::TextEnd { .. } => "text_end",
            Self::ThinkingStart { .. } => "thinking_start",
            Self::ThinkingDelta { .. } => "thinking_delta",
            Self::ThinkingEnd { .. } => "thinking_end",
            Self::ToolCallStart { .. } => "toolcall_start",
            Self::ToolCallCheckpoint { .. } => "toolcall_checkpoint",
            Self::ToolCallDelta { .. } => "toolcall_delta",
            Self::ToolCallEnd { .. } => "toolcall_end",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Thinking,
    ToolCall,
}

impl BlockKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Thinking => "thinking",
            Self::ToolCall => "toolCall",
        }
    }
}

enum EncoderBlockState {
    Text {
        covered_chars: usize,
        delta_chars: usize,
    },
    Thinking {
        covered_chars: usize,
        delta_chars: usize,
    },
    ToolCall {
        caught_up: bool,
        catchup_json: String,
        snapshot_arguments: String,
    },
}

impl EncoderBlockState {
    const fn kind(&self) -> BlockKind {
        match self {
            Self::Text { .. } => BlockKind::Text,
            Self::Thinking { .. } => BlockKind::Thinking,
            Self::ToolCall { .. } => BlockKind::ToolCall,
        }
    }
}

enum ReducerBlockState {
    Text { ended: bool },
    Thinking { ended: bool },
    ToolCall { ended: bool, json: String },
}

impl ReducerBlockState {
    const fn kind(&self) -> BlockKind {
        match self {
            Self::Text { .. } => BlockKind::Text,
            Self::Thinking { .. } => BlockKind::Thinking,
            Self::ToolCall { .. } => BlockKind::ToolCall,
        }
    }

    const fn ended(&self) -> bool {
        match self {
            Self::Text { ended } | Self::Thinking { ended } | Self::ToolCall { ended, .. } => *ended,
        }
    }
}

fn clone_start_message(message: &AssistantMessage) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: message.api.clone(),
        provider: message.provider.clone(),
        model: message.model.clone(),
        response_model: message.response_model.clone(),
        response_id: message.response_id.clone(),
        provider_thinking_level: message.provider_thinking_level.clone(),
        thinking_level: None,
        diagnostics: message.diagnostics.clone(),
        usage: message.usage.clone(),
        stop_reason: StopReason::Pending,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: message.timestamp,
    }
}

fn event_block<'a>(
    event_type: &str,
    content_index: usize,
    partial: &'a AssistantMessage,
) -> pi_js::Result<&'a AssistantContentBlock> {
    partial.content.get(content_index).ok_or_else(|| {
        pi_js::Error::msg(format!(
            "{event_type} event has no content block at index {content_index}"
        ))
    })
}

fn block_kind_name(block: &AssistantContentBlock) -> &'static str {
    match block {
        AssistantContentBlock::Text(_) => "text",
        AssistantContentBlock::Thinking(_) => "thinking",
        AssistantContentBlock::ToolCall(_) => "toolCall",
    }
}

fn empty_parsed_tool_arguments() -> String {
    let args = parse_streaming_json(Some(""));
    pi_js::json::stringify(&args)
}

fn is_json_prefix(snapshot: &JsonValue, current: &JsonValue) -> bool {
    match snapshot {
        JsonValue::String(s) => matches!(current, JsonValue::String(c) if c.starts_with(s)),
        JsonValue::Array(snap) => matches!(
            current,
            JsonValue::Array(curr)
                if snap.len() <= curr.len()
                    && snap.iter().zip(curr).all(|(a, b)| is_json_prefix(a, b))
        ),
        JsonValue::Object(snap) => matches!(
            current,
            JsonValue::Object(curr)
                if snap.iter().all(|(k, v)| {
                    curr.get(k).is_some_and(|cv| is_json_prefix(v, cv))
                })
        ),
        _ => snapshot == current,
    }
}

fn object_to_json_value(obj: JsonObject) -> JsonValue {
    let mut map = serde_json::Map::with_capacity(obj.len());
    for (k, v) in obj {
        map.insert(k, v);
    }
    JsonValue::Object(map)
}

/// Encodes one assistant stream into compact frames, skipping deltas already covered
/// by earlier snapshot content.
#[derive(Default)]
pub struct AssistantMessageFrameEncoder {
    started: bool,
    terminal: bool,
    blocks: IndexMap<usize, EncoderBlockState>,
}

impl AssistantMessageFrameEncoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn encode(&mut self, event: &AssistantMessageEvent) -> pi_js::Result<Option<AssistantMessageFrame>> {
        let event_type = match event {
            AssistantMessageEvent::Start { .. } => "start",
            AssistantMessageEvent::TextStart { .. } => "text_start",
            AssistantMessageEvent::TextDelta { .. } => "text_delta",
            AssistantMessageEvent::TextEnd { .. } => "text_end",
            AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
            AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
            AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
            AssistantMessageEvent::ToolCallStart { .. } => "toolcall_start",
            AssistantMessageEvent::ToolCallDelta { .. } => "toolcall_delta",
            AssistantMessageEvent::ToolCallEnd { .. } => "toolcall_end",
            AssistantMessageEvent::Done { .. } => "done",
            AssistantMessageEvent::Error { .. } => "error",
        };

        if self.terminal {
            return Err(pi_js::Error::msg(format!(
                "Assistant message event {event_type} follows a terminal event"
            )));
        }

        match event {
            AssistantMessageEvent::Start { partial } => {
                if self.started {
                    return Err(pi_js::Error::msg(
                        "Assistant message stream contains more than one start event",
                    ));
                }
                self.started = true;
                return Ok(Some(AssistantMessageFrame::Start {
                    partial: clone_start_message(partial),
                }));
            }
            AssistantMessageEvent::Done { .. } => {
                if !self.started {
                    return Err(pi_js::Error::msg("Assistant message done event appears before start"));
                }
                self.terminal = true;
                return Ok(None);
            }
            AssistantMessageEvent::Error { .. } => {
                self.terminal = true;
                return Ok(None);
            }
            _ => {}
        }

        if !self.started {
            return Err(pi_js::Error::msg(format!(
                "Assistant message {event_type} event appears before start"
            )));
        }

        match event {
            AssistantMessageEvent::TextStart { content_index, partial } => {
                let block = event_block("text_start", *content_index, partial)?;
                let AssistantContentBlock::Text(content) = block else {
                    return Err(pi_js::Error::msg(format!(
                        "text_start event points to {} block at index {content_index}",
                        block_kind_name(block)
                    )));
                };
                self.start_block(
                    *content_index,
                    EncoderBlockState::Text {
                        covered_chars: pi_js::str16::len(&content.text),
                        delta_chars: 0,
                    },
                )?;
                Ok(Some(AssistantMessageFrame::TextStart {
                    content_index: *content_index,
                    content: content.clone(),
                }))
            }
            AssistantMessageEvent::TextDelta {
                content_index, delta, ..
            } => self.encode_text_delta(*content_index, delta, BlockKind::Text),
            AssistantMessageEvent::TextEnd {
                content_index,
                content,
                partial,
            } => {
                let block = event_block("text_end", *content_index, partial)?;
                let AssistantContentBlock::Text(text_block) = block else {
                    return Err(pi_js::Error::msg(format!(
                        "text_end event points to {} block at index {content_index}",
                        block_kind_name(block)
                    )));
                };
                self.end_block(*content_index, BlockKind::Text)?;
                Ok(Some(AssistantMessageFrame::TextEnd {
                    content_index: *content_index,
                    content: content.clone(),
                    text_signature: text_block.text_signature.clone(),
                }))
            }
            AssistantMessageEvent::ThinkingStart { content_index, partial } => {
                let block = event_block("thinking_start", *content_index, partial)?;
                let AssistantContentBlock::Thinking(content) = block else {
                    return Err(pi_js::Error::msg(format!(
                        "thinking_start event points to {} block at index {content_index}",
                        block_kind_name(block)
                    )));
                };
                self.start_block(
                    *content_index,
                    EncoderBlockState::Thinking {
                        covered_chars: pi_js::str16::len(&content.thinking),
                        delta_chars: 0,
                    },
                )?;
                Ok(Some(AssistantMessageFrame::ThinkingStart {
                    content_index: *content_index,
                    content: content.clone(),
                }))
            }
            AssistantMessageEvent::ThinkingDelta {
                content_index, delta, ..
            } => self.encode_text_delta(*content_index, delta, BlockKind::Thinking),
            AssistantMessageEvent::ThinkingEnd {
                content_index,
                content,
                partial,
            } => {
                let block = event_block("thinking_end", *content_index, partial)?;
                let AssistantContentBlock::Thinking(thinking_block) = block else {
                    return Err(pi_js::Error::msg(format!(
                        "thinking_end event points to {} block at index {content_index}",
                        block_kind_name(block)
                    )));
                };
                self.end_block(*content_index, BlockKind::Thinking)?;
                Ok(Some(AssistantMessageFrame::ThinkingEnd {
                    content_index: *content_index,
                    content: content.clone(),
                    thinking_signature: thinking_block.thinking_signature.clone(),
                    redacted: thinking_block.redacted,
                }))
            }
            AssistantMessageEvent::ToolCallStart { content_index, partial } => {
                let block = event_block("toolcall_start", *content_index, partial)?;
                let AssistantContentBlock::ToolCall(tool_call) = block else {
                    return Err(pi_js::Error::msg(format!(
                        "toolcall_start event points to {} block at index {content_index}",
                        block_kind_name(block)
                    )));
                };
                let snapshot_arguments = pi_js::json::stringify(&tool_call.arguments);
                let caught_up = snapshot_arguments == empty_parsed_tool_arguments();
                self.start_block(
                    *content_index,
                    EncoderBlockState::ToolCall {
                        caught_up,
                        catchup_json: String::new(),
                        snapshot_arguments: if caught_up { String::new() } else { snapshot_arguments },
                    },
                )?;
                Ok(Some(AssistantMessageFrame::ToolCallStart {
                    content_index: *content_index,
                    tool_call: tool_call.clone(),
                }))
            }
            AssistantMessageEvent::ToolCallDelta {
                content_index, delta, ..
            } => {
                let state = self.block_mut(*content_index, BlockKind::ToolCall)?;
                let EncoderBlockState::ToolCall {
                    caught_up,
                    catchup_json,
                    snapshot_arguments,
                } = state
                else {
                    return Err(pi_js::Error::msg("Unreachable tool-call encoder state"));
                };
                if *caught_up {
                    return Ok(if delta.is_empty() {
                        None
                    } else {
                        Some(AssistantMessageFrame::ToolCallDelta {
                            content_index: *content_index,
                            delta: delta.clone(),
                        })
                    });
                }

                catchup_json.push_str(delta);
                let arguments_value = parse_streaming_json(Some(catchup_json));
                if pi_js::json::stringify(&arguments_value) != *snapshot_arguments {
                    let snapshot_obj = parse_streaming_json(Some(snapshot_arguments));
                    if !is_json_prefix(
                        &object_to_json_value(snapshot_obj),
                        &object_to_json_value(arguments_value),
                    ) {
                        return Ok(None);
                    }
                }

                *caught_up = true;
                snapshot_arguments.clear();
                let json = std::mem::take(catchup_json);
                Ok(if json.is_empty() {
                    None
                } else {
                    Some(AssistantMessageFrame::ToolCallCheckpoint {
                        content_index: *content_index,
                        json,
                    })
                })
            }
            AssistantMessageEvent::ToolCallEnd {
                content_index,
                tool_call,
                partial,
            } => {
                let block = event_block("toolcall_end", *content_index, partial)?;
                let AssistantContentBlock::ToolCall(_) = block else {
                    return Err(pi_js::Error::msg(format!(
                        "toolcall_end event points to {} block at index {content_index}",
                        block_kind_name(block)
                    )));
                };
                self.end_block(*content_index, BlockKind::ToolCall)?;
                Ok(Some(AssistantMessageFrame::ToolCallEnd {
                    content_index: *content_index,
                    id: tool_call.id.clone(),
                    name: tool_call.name.clone(),
                    arguments: tool_call.arguments.clone(),
                    thought_signature: tool_call.thought_signature.clone(),
                    namespace: tool_call.namespace.clone(),
                }))
            }
            AssistantMessageEvent::Start { .. }
            | AssistantMessageEvent::Done { .. }
            | AssistantMessageEvent::Error { .. } => Ok(None),
        }
    }

    fn start_block(&mut self, content_index: usize, state: EncoderBlockState) -> pi_js::Result<()> {
        if self.blocks.contains_key(&content_index) {
            return Err(pi_js::Error::msg(format!(
                "Assistant message block {content_index} starts more than once"
            )));
        }
        self.blocks.insert(content_index, state);
        Ok(())
    }

    fn block_mut(&mut self, content_index: usize, kind: BlockKind) -> pi_js::Result<&mut EncoderBlockState> {
        let Some(state) = self.blocks.get_mut(&content_index) else {
            return Err(pi_js::Error::msg(format!(
                "Assistant message {} block {content_index} has not started",
                kind.as_str()
            )));
        };
        if state.kind() != kind {
            return Err(pi_js::Error::msg(format!(
                "Assistant message block {content_index} is {}, not {}",
                state.kind().as_str(),
                kind.as_str()
            )));
        }
        Ok(state)
    }

    fn end_block(&mut self, content_index: usize, kind: BlockKind) -> pi_js::Result<()> {
        let _ = self.block_mut(content_index, kind)?;
        self.blocks.shift_remove(&content_index);
        Ok(())
    }

    fn encode_text_delta(
        &mut self,
        content_index: usize,
        delta: &str,
        kind: BlockKind,
    ) -> pi_js::Result<Option<AssistantMessageFrame>> {
        let state = self.block_mut(content_index, kind)?;
        let (covered_chars, delta_chars) = match state {
            EncoderBlockState::Text {
                covered_chars,
                delta_chars,
            }
            | EncoderBlockState::Thinking {
                covered_chars,
                delta_chars,
            } => (covered_chars, delta_chars),
            EncoderBlockState::ToolCall { .. } => {
                return Err(pi_js::Error::msg("Unreachable text encoder state"));
            }
        };

        let delta_len = pi_js::str16::len(delta);
        let delta_start = *delta_chars;
        *delta_chars += delta_len;
        let covered = covered_chars.saturating_sub(delta_start);
        if covered >= delta_len {
            return Ok(None);
        }
        let uncovered = if covered == 0 {
            delta.to_string()
        } else {
            pi_js::str16::slice(delta, covered as i64, None)
        };
        Ok(Some(match kind {
            BlockKind::Text => AssistantMessageFrame::TextDelta {
                content_index,
                delta: uncovered,
            },
            BlockKind::Thinking => AssistantMessageFrame::ThinkingDelta {
                content_index,
                delta: uncovered,
            },
            BlockKind::ToolCall => unreachable!(),
        }))
    }
}

fn append_block(
    message: &mut AssistantMessage,
    states: &mut IndexMap<usize, ReducerBlockState>,
    content_index: usize,
    block: AssistantContentBlock,
    state: ReducerBlockState,
) -> pi_js::Result<()> {
    if content_index != message.content.len() {
        let reason = if content_index < message.content.len() {
            "already exists"
        } else {
            "would leave a gap"
        };
        return Err(pi_js::Error::msg(format!(
            "Cannot start assistant message block at index {content_index}: {reason}"
        )));
    }
    message.content.push(block);
    states.insert(content_index, state);
    Ok(())
}

fn active_block<'a>(
    message: &'a mut AssistantMessage,
    states: &'a mut IndexMap<usize, ReducerBlockState>,
    content_index: usize,
    expected_kind: BlockKind,
    frame_type: &str,
) -> pi_js::Result<(&'a mut AssistantContentBlock, &'a mut ReducerBlockState)> {
    let state = states.get_mut(&content_index);
    let block = message.content.get_mut(content_index);
    let (Some(state), Some(block)) = (state, block) else {
        return Err(pi_js::Error::msg(format!(
            "{frame_type} frame has no started block at index {content_index}"
        )));
    };
    if state.kind() != expected_kind || block_kind_name(block) != expected_kind.as_str() {
        return Err(pi_js::Error::msg(format!(
            "{frame_type} frame expected {} block at index {content_index}, found {}",
            expected_kind.as_str(),
            block_kind_name(block)
        )));
    }
    if state.ended() {
        return Err(pi_js::Error::msg(format!(
            "{frame_type} frame follows the end of block at index {content_index}"
        )));
    }
    Ok((block, state))
}

/// Replay compact frames without mutating them. Returns `None` when the iterable
/// contains no start frame.
pub fn reduce_assistant_message_frames<I>(frames: I) -> pi_js::Result<Option<AssistantMessage>>
where
    I: IntoIterator,
    I::Item: Borrow<AssistantMessageFrame>,
{
    let mut message: Option<AssistantMessage> = None;
    let mut frame_before_start: Option<&'static str> = None;
    let mut states: IndexMap<usize, ReducerBlockState> = IndexMap::new();

    for item in frames {
        let frame = item.borrow();
        if let AssistantMessageFrame::Start { partial } = frame {
            if message.is_some() {
                return Err(pi_js::Error::msg(
                    "Assistant message frame sequence contains more than one start frame",
                ));
            }
            if let Some(before) = frame_before_start {
                return Err(pi_js::Error::msg(format!(
                    "{before} frame appears before the start frame"
                )));
            }
            message = Some(partial.clone());
            continue;
        }

        let Some(msg) = message.as_mut() else {
            if frame_before_start.is_none() {
                frame_before_start = Some(frame.type_name());
            }
            continue;
        };

        match frame {
            AssistantMessageFrame::TextStart { content_index, content } => {
                append_block(
                    msg,
                    &mut states,
                    *content_index,
                    AssistantContentBlock::Text(content.clone()),
                    ReducerBlockState::Text { ended: false },
                )?;
            }
            AssistantMessageFrame::TextDelta { content_index, delta } => {
                let (block, _) = active_block(msg, &mut states, *content_index, BlockKind::Text, "text_delta")?;
                let AssistantContentBlock::Text(text_block) = block else {
                    return Err(pi_js::Error::msg("Unreachable text frame state"));
                };
                text_block.text.push_str(delta);
            }
            AssistantMessageFrame::TextEnd {
                content_index,
                content,
                text_signature,
            } => {
                let (block, state) = active_block(msg, &mut states, *content_index, BlockKind::Text, "text_end")?;
                let AssistantContentBlock::Text(text_block) = block else {
                    return Err(pi_js::Error::msg("Unreachable text frame state"));
                };
                let ReducerBlockState::Text { ended } = state else {
                    return Err(pi_js::Error::msg("Unreachable text frame state"));
                };
                text_block.text = content.clone();
                text_block.text_signature = text_signature.clone();
                *ended = true;
            }
            AssistantMessageFrame::ThinkingStart { content_index, content } => {
                append_block(
                    msg,
                    &mut states,
                    *content_index,
                    AssistantContentBlock::Thinking(content.clone()),
                    ReducerBlockState::Thinking { ended: false },
                )?;
            }
            AssistantMessageFrame::ThinkingDelta { content_index, delta } => {
                let (block, _) = active_block(msg, &mut states, *content_index, BlockKind::Thinking, "thinking_delta")?;
                let AssistantContentBlock::Thinking(thinking_block) = block else {
                    return Err(pi_js::Error::msg("Unreachable thinking frame state"));
                };
                thinking_block.thinking.push_str(delta);
            }
            AssistantMessageFrame::ThinkingEnd {
                content_index,
                content,
                thinking_signature,
                redacted,
            } => {
                let (block, state) =
                    active_block(msg, &mut states, *content_index, BlockKind::Thinking, "thinking_end")?;
                let AssistantContentBlock::Thinking(thinking_block) = block else {
                    return Err(pi_js::Error::msg("Unreachable thinking frame state"));
                };
                let ReducerBlockState::Thinking { ended } = state else {
                    return Err(pi_js::Error::msg("Unreachable thinking frame state"));
                };
                thinking_block.thinking = content.clone();
                thinking_block.thinking_signature = thinking_signature.clone();
                thinking_block.redacted = *redacted;
                *ended = true;
            }
            AssistantMessageFrame::ToolCallStart {
                content_index,
                tool_call,
            } => {
                append_block(
                    msg,
                    &mut states,
                    *content_index,
                    AssistantContentBlock::ToolCall(tool_call.clone()),
                    ReducerBlockState::ToolCall {
                        ended: false,
                        json: String::new(),
                    },
                )?;
            }
            AssistantMessageFrame::ToolCallCheckpoint { content_index, json } => {
                let (block, state) = active_block(
                    msg,
                    &mut states,
                    *content_index,
                    BlockKind::ToolCall,
                    "toolcall_checkpoint",
                )?;
                let (AssistantContentBlock::ToolCall(tc), ReducerBlockState::ToolCall { json: state_json, .. }) =
                    (block, state)
                else {
                    return Err(pi_js::Error::msg("Unreachable tool-call checkpoint state"));
                };
                *state_json = json.clone();
                tc.arguments = parse_streaming_json(Some(json));
            }
            AssistantMessageFrame::ToolCallDelta { content_index, delta } => {
                let (block, state) =
                    active_block(msg, &mut states, *content_index, BlockKind::ToolCall, "toolcall_delta")?;
                let (AssistantContentBlock::ToolCall(_), ReducerBlockState::ToolCall { json: state_json, .. }) =
                    (block, state)
                else {
                    return Err(pi_js::Error::msg("Unreachable tool-call frame state"));
                };
                state_json.push_str(delta);
            }
            AssistantMessageFrame::ToolCallEnd {
                content_index,
                id,
                name,
                arguments,
                thought_signature,
                namespace,
            } => {
                let (block, state) =
                    active_block(msg, &mut states, *content_index, BlockKind::ToolCall, "toolcall_end")?;
                let (AssistantContentBlock::ToolCall(tc), ReducerBlockState::ToolCall { ended, .. }) = (block, state)
                else {
                    return Err(pi_js::Error::msg("Unreachable tool-call frame state"));
                };
                tc.id = id.clone();
                tc.name = name.clone();
                tc.arguments = arguments.clone();
                tc.thought_signature = thought_signature.clone();
                tc.namespace = namespace.clone();
                *ended = true;
            }
            AssistantMessageFrame::Start { .. } => unreachable!(),
        }
    }

    let Some(mut msg) = message else {
        return Ok(None);
    };

    for (content_index, state) in states {
        let ReducerBlockState::ToolCall { ended, json } = state else {
            continue;
        };
        if ended || json.is_empty() {
            continue;
        }
        let Some(AssistantContentBlock::ToolCall(tc)) = msg.content.get_mut(content_index) else {
            return Err(pi_js::Error::msg("Unreachable tool-call frame state"));
        };
        tc.arguments = parse_streaming_json(Some(&json));
    }

    Ok(Some(msg))
}
