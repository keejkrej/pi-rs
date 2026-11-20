//! Port of packages/coding-agent/src/modes/json-event.ts

use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Message, StopReason, ToolCall, ToolResultMessage,
    Usage,
};
use pi_js::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::core::agent_session::{AgentSessionEvent, CompactionReason, SummarizationRetrySource};
pub use crate::core::compaction::CompactionResult;
pub use crate::core::session_manager::SessionEntry;
pub use pi_agent_core::AgentToolResult;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum JsonAssistantMessageEvent {
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
        message: AssistantMessage,
    },
    #[serde(rename = "error")]
    Error {
        reason: StopReason,
        error: AssistantMessage,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum JsonAgentSessionEvent {
    #[serde(rename = "agent_start")]
    AgentStart,
    #[serde(rename = "agent_end")]
    AgentEnd {
        messages: Vec<AgentMessage>,
        #[serde(rename = "willRetry")]
        will_retry: bool,
    },
    #[serde(rename = "agent_settled")]
    AgentSettled {
        #[serde(default)]
        aborted: bool,
    },
    #[serde(rename = "turn_start")]
    TurnStart,
    #[serde(rename = "turn_end")]
    TurnEnd {
        message: AgentMessage,
        #[serde(rename = "toolResults")]
        tool_results: Vec<ToolResultMessage>,
    },
    #[serde(rename = "message_start")]
    MessageStart { message: AgentMessage },
    #[serde(rename = "message_update")]
    MessageUpdate {
        usage: Usage,
        #[serde(rename = "assistantMessageEvent")]
        assistant_message_event: JsonAssistantMessageEvent,
    },
    #[serde(rename = "message_end")]
    MessageEnd { message: AgentMessage },
    #[serde(rename = "tool_execution_start")]
    ToolExecutionStart {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        args: Value,
        #[serde(rename = "parentToolCallId", default, skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
    },
    #[serde(rename = "tool_execution_update")]
    ToolExecutionUpdate {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        args: Value,
        #[serde(rename = "partialResult")]
        partial_result: Value,
        #[serde(rename = "parentToolCallId", default, skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
    },
    #[serde(rename = "tool_execution_end")]
    ToolExecutionEnd {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        result: Value,
        #[serde(rename = "isError")]
        is_error: bool,
        #[serde(rename = "parentToolCallId", default, skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
    },
    #[serde(rename = "queue_update")]
    QueueUpdate {
        steering: Vec<String>,
        #[serde(rename = "followUp")]
        follow_up: Vec<String>,
    },
    #[serde(rename = "compaction_start")]
    CompactionStart { reason: CompactionReason },
    #[serde(rename = "entry_appended")]
    EntryAppended { entry: Value },
    #[serde(rename = "session_info_changed")]
    SessionInfoChanged { name: Option<String> },
    #[serde(rename = "thinking_level_changed")]
    ThinkingLevelChanged { level: pi_ai::ModelThinkingLevel },
    #[serde(rename = "compaction_end")]
    CompactionEnd {
        reason: CompactionReason,
        result: Option<Value>,
        aborted: bool,
        #[serde(rename = "willRetry")]
        will_retry: bool,
        #[serde(rename = "errorMessage", default, skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
    },
    #[serde(rename = "auto_retry_start")]
    AutoRetryStart {
        attempt: u32,
        #[serde(rename = "maxAttempts")]
        max_attempts: u32,
        #[serde(rename = "delayMs")]
        delay_ms: u64,
        #[serde(rename = "errorMessage")]
        error_message: String,
    },
    #[serde(rename = "auto_retry_end")]
    AutoRetryEnd {
        success: bool,
        attempt: u32,
        #[serde(rename = "finalError", default, skip_serializing_if = "Option::is_none")]
        final_error: Option<String>,
    },
    #[serde(rename = "summarization_retry_scheduled")]
    SummarizationRetryScheduled {
        attempt: u32,
        #[serde(rename = "maxAttempts")]
        max_attempts: u32,
        #[serde(rename = "delayMs")]
        delay_ms: u64,
        #[serde(rename = "errorMessage")]
        error_message: String,
    },
    #[serde(rename = "summarization_retry_attempt_start")]
    SummarizationRetryAttemptStart {
        source: SummarizationRetrySource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<CompactionReason>,
    },
    #[serde(rename = "summarization_retry_finished")]
    SummarizationRetryFinished,
    #[serde(rename = "bash_execution_update")]
    BashExecutionUpdate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        delta: String,
    },
    #[serde(untagged)]
    Extra(IndexMap<String, Value>),
}

fn to_json_assistant_message_event(event: &AssistantMessageEvent) -> Result<JsonAssistantMessageEvent> {
    match event {
        AssistantMessageEvent::Start { .. } => Ok(JsonAssistantMessageEvent::Start),
        AssistantMessageEvent::TextStart { content_index, .. } => Ok(JsonAssistantMessageEvent::TextStart {
            content_index: *content_index,
        }),
        AssistantMessageEvent::TextDelta {
            content_index, delta, ..
        } => Ok(JsonAssistantMessageEvent::TextDelta {
            content_index: *content_index,
            delta: delta.clone(),
        }),
        AssistantMessageEvent::TextEnd {
            content_index, content, ..
        } => Ok(JsonAssistantMessageEvent::TextEnd {
            content_index: *content_index,
            content: content.clone(),
        }),
        AssistantMessageEvent::ThinkingStart { content_index, .. } => Ok(JsonAssistantMessageEvent::ThinkingStart {
            content_index: *content_index,
        }),
        AssistantMessageEvent::ThinkingDelta {
            content_index, delta, ..
        } => Ok(JsonAssistantMessageEvent::ThinkingDelta {
            content_index: *content_index,
            delta: delta.clone(),
        }),
        AssistantMessageEvent::ThinkingEnd {
            content_index, content, ..
        } => Ok(JsonAssistantMessageEvent::ThinkingEnd {
            content_index: *content_index,
            content: content.clone(),
        }),
        AssistantMessageEvent::ToolCallStart { content_index, partial } => {
            let block = partial.content.get(*content_index).ok_or_else(|| {
                Error::msg(format!(
                    "toolcall_start content at index {content_index} is not a tool call"
                ))
            })?;
            let AssistantContentBlock::ToolCall(tc) = block else {
                return Err(Error::msg(format!(
                    "toolcall_start content at index {content_index} is not a tool call"
                )));
            };
            Ok(JsonAssistantMessageEvent::ToolCallStart {
                content_index: *content_index,
                id: tc.id.clone(),
                tool_name: tc.name.clone(),
            })
        }
        AssistantMessageEvent::ToolCallDelta {
            content_index, delta, ..
        } => Ok(JsonAssistantMessageEvent::ToolCallDelta {
            content_index: *content_index,
            delta: delta.clone(),
        }),
        AssistantMessageEvent::ToolCallEnd {
            content_index,
            tool_call,
            ..
        } => Ok(JsonAssistantMessageEvent::ToolCallEnd {
            content_index: *content_index,
            tool_call: tool_call.clone(),
        }),
        AssistantMessageEvent::Done { reason, message } => Ok(JsonAssistantMessageEvent::Done {
            reason: *reason,
            message: message.clone(),
        }),
        AssistantMessageEvent::Error { reason, error } => Ok(JsonAssistantMessageEvent::Error {
            reason: *reason,
            error: error.clone(),
        }),
    }
}

/// Remove cumulative assistant snapshots from streaming wire events.
pub fn to_json_event(event: &AgentSessionEvent) -> Result<JsonAgentSessionEvent> {
    match event {
        AgentSessionEvent::AgentStart => Ok(JsonAgentSessionEvent::AgentStart),
        AgentSessionEvent::AgentEnd { messages, will_retry } => Ok(JsonAgentSessionEvent::AgentEnd {
            messages: messages.clone(),
            will_retry: *will_retry,
        }),
        AgentSessionEvent::AgentSettled { aborted } => Ok(JsonAgentSessionEvent::AgentSettled { aborted: *aborted }),
        AgentSessionEvent::TurnStart => Ok(JsonAgentSessionEvent::TurnStart),
        AgentSessionEvent::TurnEnd { message, tool_results } => Ok(JsonAgentSessionEvent::TurnEnd {
            message: message.clone(),
            tool_results: tool_results.clone(),
        }),
        AgentSessionEvent::MessageStart { message } => Ok(JsonAgentSessionEvent::MessageStart {
            message: message.clone(),
        }),
        AgentSessionEvent::MessageUpdate {
            message,
            assistant_message_event,
        } => {
            let usage = match message {
                AgentMessage::Message(Message::Assistant(a)) => a.usage.clone(),
                _ => return Err(Error::msg("message_update message is not an assistant message")),
            };
            let assistant_message_event = to_json_assistant_message_event(assistant_message_event)?;
            Ok(JsonAgentSessionEvent::MessageUpdate {
                usage,
                assistant_message_event,
            })
        }
        AgentSessionEvent::MessageEnd { message } => Ok(JsonAgentSessionEvent::MessageEnd {
            message: message.clone(),
        }),
        AgentSessionEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
            parent_tool_call_id,
        } => Ok(JsonAgentSessionEvent::ToolExecutionStart {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            args: args.clone(),
            parent_tool_call_id: parent_tool_call_id.clone(),
        }),
        AgentSessionEvent::ToolExecutionUpdate {
            tool_call_id,
            tool_name,
            args,
            partial_result,
            parent_tool_call_id,
        } => Ok(JsonAgentSessionEvent::ToolExecutionUpdate {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            args: args.clone(),
            partial_result: serde_json::to_value(partial_result).map_err(|e| Error::msg(e.to_string()))?,
            parent_tool_call_id: parent_tool_call_id.clone(),
        }),
        AgentSessionEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
            is_error,
            parent_tool_call_id,
        } => Ok(JsonAgentSessionEvent::ToolExecutionEnd {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            result: serde_json::to_value(result).map_err(|e| Error::msg(e.to_string()))?,
            is_error: *is_error,
            parent_tool_call_id: parent_tool_call_id.clone(),
        }),
        AgentSessionEvent::QueueUpdate { steering, follow_up } => Ok(JsonAgentSessionEvent::QueueUpdate {
            steering: steering.clone(),
            follow_up: follow_up.clone(),
        }),
        AgentSessionEvent::CompactionStart { reason } => Ok(JsonAgentSessionEvent::CompactionStart { reason: *reason }),
        AgentSessionEvent::EntryAppended { entry } => Ok(JsonAgentSessionEvent::EntryAppended {
            entry: serde_json::to_value(entry).map_err(|e| Error::msg(e.to_string()))?,
        }),
        AgentSessionEvent::SessionInfoChanged { name } => {
            Ok(JsonAgentSessionEvent::SessionInfoChanged { name: name.clone() })
        }
        AgentSessionEvent::ThinkingLevelChanged { level } => {
            Ok(JsonAgentSessionEvent::ThinkingLevelChanged { level: *level })
        }
        AgentSessionEvent::CompactionEnd {
            reason,
            result,
            aborted,
            will_retry,
            error_message,
        } => Ok(JsonAgentSessionEvent::CompactionEnd {
            reason: *reason,
            result: result
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .map_err(|e| Error::msg(e.to_string()))?,
            aborted: *aborted,
            will_retry: *will_retry,
            error_message: error_message.clone(),
        }),
        AgentSessionEvent::AutoRetryStart {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => Ok(JsonAgentSessionEvent::AutoRetryStart {
            attempt: *attempt,
            max_attempts: *max_attempts,
            delay_ms: *delay_ms,
            error_message: error_message.clone(),
        }),
        AgentSessionEvent::AutoRetryEnd {
            success,
            attempt,
            final_error,
        } => Ok(JsonAgentSessionEvent::AutoRetryEnd {
            success: *success,
            attempt: *attempt,
            final_error: final_error.clone(),
        }),
        AgentSessionEvent::SummarizationRetryScheduled {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => Ok(JsonAgentSessionEvent::SummarizationRetryScheduled {
            attempt: *attempt,
            max_attempts: *max_attempts,
            delay_ms: *delay_ms,
            error_message: error_message.clone(),
        }),
        AgentSessionEvent::SummarizationRetryAttemptStart { source, reason } => {
            Ok(JsonAgentSessionEvent::SummarizationRetryAttemptStart {
                source: *source,
                reason: *reason,
            })
        }
        AgentSessionEvent::SummarizationRetryFinished => Ok(JsonAgentSessionEvent::SummarizationRetryFinished),
        AgentSessionEvent::BashExecutionUpdate { id, delta } => Ok(JsonAgentSessionEvent::BashExecutionUpdate {
            id: id.clone(),
            delta: delta.clone(),
        }),
    }
}
