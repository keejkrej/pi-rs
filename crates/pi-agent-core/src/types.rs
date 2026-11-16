//! Port of packages/agent/src/types.ts

use std::fmt;
use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};
use pi_ai::{
    AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, ConstrainedSamplingConfig, Context,
    JsonObject, Message, Model, ModelThinkingLevel, Schema, SimpleStreamOptions, StopReason, SystemMessage, Tool,
    ToolCall, ToolResultMessage, Transport, Usage, UserContentBlock, UserMessage,
};
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Custom agent messages map placeholder for Rust hosts (TS module augmentation equivalent).
///
/// Hosts that define domain-specific message shapes serialize/deserialize them through
/// [`AgentMessage::Custom`].
pub type CustomAgentMessages = IndexMap<String, Value>;

/// Stream function used by [`Agent`] and [`agent_loop`](crate::agent_loop::agent_loop).
pub type StreamFn = Arc<
    dyn Fn(Model, Context, Option<SimpleStreamOptions>) -> BoxFuture<Result<AssistantMessageEventStream>> + Send + Sync,
>;

/// Requested reasoning level for an agent run (`"off"` disables thinking).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    #[default]
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

impl ThinkingLevel {
    /// Converts to `Option<pi_ai::ThinkingLevel>`, mapping [`ThinkingLevel::Off`] to `None`.
    pub fn to_ai_thinking_level(self) -> Option<pi_ai::ThinkingLevel> {
        match self {
            Self::Off => None,
            Self::Minimal => Some(pi_ai::ThinkingLevel::Minimal),
            Self::Low => Some(pi_ai::ThinkingLevel::Low),
            Self::Medium => Some(pi_ai::ThinkingLevel::Medium),
            Self::High => Some(pi_ai::ThinkingLevel::High),
            Self::Xhigh => Some(pi_ai::ThinkingLevel::Xhigh),
        }
    }

    /// Creates an agent thinking level from an optional `pi_ai::ThinkingLevel`.
    pub fn from_ai_thinking_level(level: Option<pi_ai::ThinkingLevel>) -> Self {
        match level {
            None => Self::Off,
            Some(pi_ai::ThinkingLevel::Minimal) => Self::Minimal,
            Some(pi_ai::ThinkingLevel::Low) => Self::Low,
            Some(pi_ai::ThinkingLevel::Medium) => Self::Medium,
            Some(pi_ai::ThinkingLevel::High) => Self::High,
            Some(pi_ai::ThinkingLevel::Xhigh) => Self::Xhigh,
        }
    }

    /// Converts directly to [`pi_ai::ModelThinkingLevel`] for stamping finalized assistant messages.
    pub fn to_model_thinking_level(self) -> ModelThinkingLevel {
        match self {
            Self::Off => ModelThinkingLevel::Off,
            Self::Minimal => ModelThinkingLevel::Minimal,
            Self::Low => ModelThinkingLevel::Low,
            Self::Medium => ModelThinkingLevel::Medium,
            Self::High => ModelThinkingLevel::High,
            Self::Xhigh => ModelThinkingLevel::Xhigh,
        }
    }
}

/// Queue draining strategy for steering and follow-up messages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueMode {
    #[serde(rename = "all")]
    All,
    #[default]
    #[serde(rename = "one-at-a-time")]
    OneAtATime,
}

/// How multiple tool calls in a single assistant message are executed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExecutionMode {
    Sequential,
    #[default]
    Parallel,
}

/// Extensible union of LLM messages and custom host messages.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentMessage {
    Message(Message),
    Custom(Value),
}

impl AgentMessage {
    /// Creates a custom host message from any JSON value (typically an object with `"role"`).
    pub fn custom(value: Value) -> Self {
        Self::Custom(value)
    }

    /// Serializes a typed custom message into [`AgentMessage::Custom`].
    pub fn try_custom_from<T: Serialize>(value: &T) -> Result<Self> {
        let json = pi_js::json::try_stringify(value)?;
        let parsed = pi_js::json::parse(&json)?;
        Ok(Self::Custom(parsed))
    }

    /// Returns the `"role"` discriminator if this message is an LLM message or a custom JSON object with `"role"`.
    pub fn role(&self) -> Option<&str> {
        match self {
            Self::Message(msg) => match msg {
                Message::System(_) => Some("system"),
                Message::User(_) => Some("user"),
                Message::Assistant(_) => Some("assistant"),
                Message::ToolResult(_) => Some("toolResult"),
            },
            Self::Custom(val) => val.get("role").and_then(Value::as_str),
        }
    }

    /// Returns the inner [`Message`] if this is a standard LLM message.
    pub fn as_message(&self) -> Option<&Message> {
        match self {
            Self::Message(msg) => Some(msg),
            Self::Custom(_) => None,
        }
    }

    /// Returns a mutable reference to the inner [`Message`] if this is a standard LLM message.
    pub fn as_message_mut(&mut self) -> Option<&mut Message> {
        match self {
            Self::Message(msg) => Some(msg),
            Self::Custom(_) => None,
        }
    }

    /// Consumes self and returns the inner [`Message`] if this is a standard LLM message.
    pub fn into_message(self) -> Option<Message> {
        match self {
            Self::Message(msg) => Some(msg),
            Self::Custom(_) => None,
        }
    }

    pub fn as_system(&self) -> Option<&SystemMessage> {
        match self {
            Self::Message(Message::System(m)) => Some(m),
            _ => None,
        }
    }

    pub fn as_user(&self) -> Option<&UserMessage> {
        match self {
            Self::Message(Message::User(m)) => Some(m),
            _ => None,
        }
    }

    pub fn as_assistant(&self) -> Option<&AssistantMessage> {
        match self {
            Self::Message(Message::Assistant(m)) => Some(m),
            _ => None,
        }
    }

    pub fn as_tool_result(&self) -> Option<&ToolResultMessage> {
        match self {
            Self::Message(Message::ToolResult(m)) => Some(m),
            _ => None,
        }
    }

    pub fn as_custom(&self) -> Option<&Value> {
        match self {
            Self::Custom(v) => Some(v),
            _ => None,
        }
    }

    pub fn stop_reason(&self) -> Option<StopReason> {
        self.as_assistant().map(|m| m.stop_reason)
    }
}

impl From<Message> for AgentMessage {
    fn from(m: Message) -> Self {
        Self::Message(m)
    }
}

impl From<SystemMessage> for AgentMessage {
    fn from(m: SystemMessage) -> Self {
        Self::Message(Message::System(m))
    }
}

impl From<UserMessage> for AgentMessage {
    fn from(m: UserMessage) -> Self {
        Self::Message(Message::User(m))
    }
}

impl From<AssistantMessage> for AgentMessage {
    fn from(m: AssistantMessage) -> Self {
        Self::Message(Message::Assistant(m))
    }
}

impl From<ToolResultMessage> for AgentMessage {
    fn from(m: ToolResultMessage) -> Self {
        Self::Message(Message::ToolResult(m))
    }
}

/// Optional flow-control directive returned by a tool execution.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolControl {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

/// Result produced by executing an [`AgentTool`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolResult {
    pub content: Vec<UserContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ToolControl>,
}

impl AgentToolResult {
    /// Convenience constructor for a plain-text tool result without extra details or control flags.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![UserContentBlock::Text(pi_ai::faux_text(&text.into()))],
            details: None,
            structured_content: None,
            usage: None,
            is_error: None,
            terminate: None,
            control: None,
        }
    }
}

/// Minimal tool-call invocation descriptor passed to nested/standalone tool helpers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentToolCall {
    pub id: String,
    pub name: String,
    pub arguments: JsonObject,
}

impl AgentToolCall {
    /// Converts into a [`pi_ai::ToolCall`].
    pub fn to_tool_call(&self) -> ToolCall {
        pi_ai::faux_tool_call(
            &self.name,
            self.arguments.clone(),
            Some(pi_ai::FauxToolCallOptions {
                id: Some(self.id.clone()),
            }),
        )
    }
}

impl From<&ToolCall> for AgentToolCall {
    fn from(tc: &ToolCall) -> Self {
        Self {
            id: tc.id.clone(),
            name: tc.name.clone(),
            arguments: tc.arguments.clone(),
        }
    }
}

/// Outcome returned by [`run_tool_call`](crate::agent_loop::run_tool_call).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolCallOutcome {
    pub result: AgentToolResult,
    pub is_error: bool,
}

/// Callback for streaming partial tool execution progress updates.
pub type AgentToolUpdateCallback = Arc<dyn Fn(AgentToolResult) + Send + Sync>;

/// Optional synchronous argument pre-processor executed prior to schema validation.
pub type AgentToolPrepareArgumentsFn = Arc<dyn Fn(Value) -> Value + Send + Sync>;

/// Asynchronous tool execution callback `(tool_call_id, params, signal, on_update)`.
pub type AgentToolExecuteFn = Arc<
    dyn Fn(String, Value, Option<AbortSignal>, Option<AgentToolUpdateCallback>) -> BoxFuture<Result<AgentToolResult>>
        + Send
        + Sync,
>;

/// Optional replay hook invoked when re-applying historical tool calls.
pub type AgentToolReplayFn = Arc<dyn Fn(String, Value, AgentToolResult, bool) -> BoxFuture<Result<()>> + Send + Sync>;

/// Executable tool definition extending [`pi_ai::Tool`].
#[derive(Clone)]
pub struct AgentTool {
    pub name: String,
    pub label: String,
    pub description: String,
    pub parameters: Schema,
    pub constrained_sampling: Option<ConstrainedSamplingConfig>,
    pub output_schema: Option<Value>,
    pub execution_mode: Option<ToolExecutionMode>,
    pub prepare_arguments: Option<AgentToolPrepareArgumentsFn>,
    pub execute: AgentToolExecuteFn,
    pub replay: Option<AgentToolReplayFn>,
}

impl AgentTool {
    /// Extracts the underlying LLM-facing [`pi_ai::Tool`] descriptor.
    pub fn to_tool(&self) -> Tool {
        Tool {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
            constrained_sampling: self.constrained_sampling.clone(),
        }
    }
}

impl fmt::Debug for AgentTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentTool")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("description", &self.description)
            .field("execution_mode", &self.execution_mode)
            .finish_non_exhaustive()
    }
}

/// Prompt context managed across turns in an agent run.
#[derive(Clone, Debug, Default)]
pub struct AgentContext {
    pub messages: Vec<AgentMessage>,
    pub tools: Option<Vec<AgentTool>>,
}

/// Observable runtime state snapshot of an [`Agent`](crate::agent::Agent).
#[derive(Clone, Debug)]
pub struct AgentState {
    pub system_prompt: String,
    pub model: Model,
    pub thinking_level: ThinkingLevel,
    pub tools: Vec<AgentTool>,
    pub messages: Vec<AgentMessage>,
    pub is_streaming: bool,
    pub stream_message: Option<AgentMessage>,
    pub pending_tool_calls: IndexSet<String>,
    pub error: Option<String>,
}

/// Context passed to [`BeforeToolCallFn`].
#[derive(Clone, Debug)]
pub struct BeforeToolCallContext {
    pub assistant_message: AssistantMessage,
    pub tool_call: ToolCall,
    pub args: Value,
    pub context: AgentContext,
}

/// Decision returned from [`BeforeToolCallFn`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeToolCallResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
    /// Optional replacement arguments executed without revalidation (Rust analog of in-place arg mutation in JS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}

/// Context passed to [`AfterToolCallFn`].
#[derive(Clone, Debug)]
pub struct AfterToolCallContext {
    pub assistant_message: AssistantMessage,
    pub tool_call: ToolCall,
    pub args: Value,
    pub result: AgentToolResult,
    pub is_error: bool,
    pub context: AgentContext,
}

/// Overrides returned from [`AfterToolCallFn`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AfterToolCallResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<UserContentBlock>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

/// Action returned from a [`FinishTurnFn`] hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FinishTurnAction {
    Continue,
    End,
}

/// Decision object returned from [`FinishTurnFn`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishTurnDecision {
    pub action: FinishTurnAction,
}

/// Context passed to [`FinishTurnFn`].
#[derive(Clone, Debug)]
pub struct FinishTurnContext {
    pub message: AssistantMessage,
    pub tool_results: Vec<ToolResultMessage>,
    pub context: AgentContext,
}

/// Updates applied right before starting the next assistant turn.
#[derive(Clone, Debug, Default)]
pub struct AgentLoopTurnUpdate {
    pub context: Option<AgentContext>,
    pub messages: Option<Vec<AgentMessage>>,
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
    pub reasoning: Option<ThinkingLevel>,
}

/// Context passed to [`PrepareNextTurnWithContextFn`].
#[derive(Clone, Debug)]
pub struct PrepareNextTurnContext {
    pub context: AgentContext,
}

/// Request snapshot passed to [`PrepareRequestFn`].
#[derive(Clone, Debug)]
pub struct PrepareRequestContext {
    pub context: AgentContext,
    pub model: Model,
    pub thinking_level: ThinkingLevel,
}

/// Request overrides returned by [`PrepareRequestFn`].
#[derive(Clone, Debug, Default)]
pub struct PrepareRequestResult {
    pub context: Option<AgentContext>,
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
}

/// Hook transforming agent messages before `convert_to_llm`.
pub type TransformContextFn =
    Arc<dyn Fn(Vec<AgentMessage>, Option<AbortSignal>) -> BoxFuture<Result<Vec<AgentMessage>>> + Send + Sync>;

/// Hook converting agent messages to provider LLM messages.
pub type ConvertToLlmFn = Arc<dyn Fn(Vec<AgentMessage>) -> BoxFuture<Result<Vec<Message>>> + Send + Sync>;

/// Dynamic API key lookup callback keyed by provider name.
pub type GetApiKeyFn = Arc<dyn Fn(String) -> BoxFuture<Result<Option<String>>> + Send + Sync>;

/// Queue polling callback for steering or follow-up messages.
pub type GetQueuedMessagesFn = Arc<dyn Fn() -> BoxFuture<Result<Vec<AgentMessage>>> + Send + Sync>;

/// Hook invoked before executing each validated tool call.
pub type BeforeToolCallFn = Arc<
    dyn Fn(BeforeToolCallContext, Option<AbortSignal>) -> BoxFuture<Result<Option<BeforeToolCallResult>>> + Send + Sync,
>;

/// Hook invoked after executing each tool call.
pub type AfterToolCallFn = Arc<
    dyn Fn(AfterToolCallContext, Option<AbortSignal>) -> BoxFuture<Result<Option<AfterToolCallResult>>> + Send + Sync,
>;

/// Hook invoked after finishing a turn (assistant message + any tool results).
pub type FinishTurnFn =
    Arc<dyn Fn(FinishTurnContext, Option<AbortSignal>) -> BoxFuture<Result<Option<FinishTurnDecision>>> + Send + Sync>;

/// Hook invoked before the next assistant turn to refresh state/context.
pub type PrepareNextTurnFn =
    Arc<dyn Fn(Option<AbortSignal>) -> BoxFuture<Result<Option<AgentLoopTurnUpdate>>> + Send + Sync>;

/// Context-aware hook invoked before the next assistant turn to refresh state/context.
pub type PrepareNextTurnWithContextFn = Arc<
    dyn Fn(PrepareNextTurnContext, Option<AbortSignal>) -> BoxFuture<Result<Option<AgentLoopTurnUpdate>>> + Send + Sync,
>;

/// Hook invoked immediately before preparing/calling the LLM request for a turn.
pub type PrepareRequestFn = Arc<
    dyn Fn(PrepareRequestContext, Option<AbortSignal>) -> BoxFuture<Result<Option<PrepareRequestResult>>> + Send + Sync,
>;

/// Optional stream-level payload inspector matching `SimpleStreamOptions::on_payload`.
pub type OnPayloadFn = pi_ai::OnPayloadFn;

/// Configuration for [`agent_loop`](crate::agent_loop::agent_loop) and [`run_agent_loop`](crate::agent_loop::run_agent_loop).
#[derive(Clone)]
pub struct AgentLoopConfig {
    pub model: Model,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    pub api_key: Option<String>,
    pub cache_retention: Option<pi_ai::CacheRetention>,
    pub reasoning: Option<ThinkingLevel>,
    pub session_id: Option<String>,
    pub transport: Option<Transport>,
    pub thinking_budgets: Option<pi_ai::ThinkingBudgets>,
    pub max_retry_delay_ms: Option<i64>,
    pub metadata: Option<IndexMap<String, Value>>,
    pub headers: Option<IndexMap<String, String>>,
    pub on_payload: Option<OnPayloadFn>,
    pub tool_execution: Option<ToolExecutionMode>,
    pub convert_to_llm: ConvertToLlmFn,
    pub transform_context: Option<TransformContextFn>,
    pub get_api_key: Option<GetApiKeyFn>,
    pub get_steering_messages: Option<GetQueuedMessagesFn>,
    pub get_follow_up_messages: Option<GetQueuedMessagesFn>,
    pub before_tool_call: Option<BeforeToolCallFn>,
    pub after_tool_call: Option<AfterToolCallFn>,
    pub finish_turn: Option<FinishTurnFn>,
    pub prepare_next_turn: Option<PrepareNextTurnFn>,
    pub prepare_next_turn_with_context: Option<PrepareNextTurnWithContextFn>,
    pub prepare_request: Option<PrepareRequestFn>,
    pub stream_fn: Option<StreamFn>,
}

impl AgentLoopConfig {
    /// Creates a config with required `model` and `convert_to_llm` and all optional hooks unset.
    pub fn new(model: Model, convert_to_llm: ConvertToLlmFn) -> Self {
        Self {
            model,
            temperature: None,
            max_tokens: None,
            api_key: None,
            cache_retention: None,
            reasoning: None,
            session_id: None,
            transport: None,
            thinking_budgets: None,
            max_retry_delay_ms: None,
            metadata: None,
            headers: None,
            on_payload: None,
            tool_execution: None,
            convert_to_llm,
            transform_context: None,
            get_api_key: None,
            get_steering_messages: None,
            get_follow_up_messages: None,
            before_tool_call: None,
            after_tool_call: None,
            finish_turn: None,
            prepare_next_turn: None,
            prepare_next_turn_with_context: None,
            prepare_request: None,
            stream_fn: None,
        }
    }
}

impl fmt::Debug for AgentLoopConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentLoopConfig")
            .field("model", &self.model.id)
            .field("reasoning", &self.reasoning)
            .field("session_id", &self.session_id)
            .field("tool_execution", &self.tool_execution)
            .finish_non_exhaustive()
    }
}

/// Lifecycle events emitted by [`agent_loop`](crate::agent_loop::agent_loop) and [`Agent`](crate::agent::Agent).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    #[serde(rename = "agent_start")]
    AgentStart,
    #[serde(rename = "agent_end")]
    AgentEnd { messages: Vec<AgentMessage> },
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
        message: AgentMessage,
        #[serde(rename = "assistantMessageEvent")]
        assistant_message_event: AssistantMessageEvent,
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
    },
    #[serde(rename = "tool_execution_update")]
    ToolExecutionUpdate {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        args: Value,
        #[serde(rename = "partialResult")]
        partial_result: AgentToolResult,
    },
    #[serde(rename = "tool_execution_end")]
    ToolExecutionEnd {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        result: AgentToolResult,
        #[serde(rename = "isError")]
        is_error: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_mode_and_thinking_level_serde() {
        assert_eq!(pi_js::json::stringify(&QueueMode::OneAtATime), "\"one-at-a-time\"");
        assert_eq!(pi_js::json::stringify(&QueueMode::All), "\"all\"");
        assert_eq!(pi_js::json::stringify(&ThinkingLevel::Off), "\"off\"");
        assert_eq!(pi_js::json::stringify(&ThinkingLevel::Xhigh), "\"xhigh\"");
    }
}
