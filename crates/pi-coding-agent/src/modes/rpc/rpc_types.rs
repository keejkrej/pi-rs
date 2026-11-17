//! Port of packages/coding-agent/src/modes/rpc/rpc-types.ts

use indexmap::IndexMap;
use pi_agent_core::{AgentMessage, QueueMode};
use pi_ai::{ImageContent, Model, ModelThinkingLevel};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;

/// Thinking level on the RPC wire (supports `"max"` via [`ModelThinkingLevel`]).
pub type ThinkingLevel = ModelThinkingLevel;

// ============================================================================
// Shared Core / Wire Types
// ============================================================================

/// Outcome of a queued user input (`steer` or `follow_up`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueuedInputDisposition {
    Handled,
    Queued,
}

impl From<crate::core::agent_session::QueuedInputDisposition> for QueuedInputDisposition {
    fn from(value: crate::core::agent_session::QueuedInputDisposition) -> Self {
        match value {
            crate::core::agent_session::QueuedInputDisposition::Handled => Self::Handled,
            crate::core::agent_session::QueuedInputDisposition::Queued => Self::Queued,
        }
    }
}

/// Disposition reported in a successful `prompt` RPC response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptDisposition {
    Handled,
    Queued,
    Started,
}

impl From<crate::core::agent_session::PromptPreflightOutcome> for PromptDisposition {
    fn from(value: crate::core::agent_session::PromptPreflightOutcome) -> Self {
        match value {
            crate::core::agent_session::PromptPreflightOutcome::Handled => Self::Handled,
            crate::core::agent_session::PromptPreflightOutcome::Queued => Self::Queued,
            crate::core::agent_session::PromptPreflightOutcome::Started => Self::Started,
        }
    }
}

/// Queue behavior for `prompt` when the agent is already streaming.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RpcStreamingBehavior {
    #[serde(rename = "steer")]
    Steer,
    #[serde(rename = "followUp")]
    FollowUp,
}

impl From<RpcStreamingBehavior> for crate::core::agent_session::StreamingBehavior {
    fn from(value: RpcStreamingBehavior) -> Self {
        match value {
            RpcStreamingBehavior::Steer => Self::Steer,
            RpcStreamingBehavior::FollowUp => Self::FollowUp,
        }
    }
}

/// Scope, origin, and metadata for a loaded resource.
pub use crate::core::source_info::{SourceInfo, SourceOrigin, SourceScope};

/// Result returned by `bash` RPC command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashResult {
    pub output: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

impl From<crate::core::bash_executor::BashResult> for BashResult {
    fn from(res: crate::core::bash_executor::BashResult) -> Self {
        Self {
            output: res.output,
            exit_code: res.exit_code,
            cancelled: res.cancelled,
            truncated: res.truncated,
            full_output_path: res.full_output_path,
        }
    }
}

impl From<&BashResult> for crate::core::bash_executor::BashResult {
    fn from(res: &BashResult) -> Self {
        Self {
            output: res.output.clone(),
            exit_code: res.exit_code,
            cancelled: res.cancelled,
            truncated: res.truncated,
            full_output_path: res.full_output_path.clone(),
        }
    }
}

/// Result returned by `compact` RPC command.
pub type CompactionResult = crate::core::compaction::compaction::CompactionResult<Value>;

/// Context window utilization reported inside [`SessionStats`].
pub use crate::core::extensions::types::ContextUsage;

/// Aggregated token counts and session statistics for `/session` and `get_session_stats`.
pub use crate::core::agent_session::{SessionStats, SessionTokenStats};

/// Serialized session entry on the RPC wire.
pub type SessionEntry = Value;

/// Extract the `id` field from a serialized [`SessionEntry`].
pub fn session_entry_id(entry: &SessionEntry) -> Option<&str> {
    entry.get("id").and_then(Value::as_str)
}

/// Node in a session entry tree serialized on the RPC wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreeNode {
    pub entry: SessionEntry,
    pub children: Vec<SessionTreeNode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_timestamp: Option<String>,
}

/// Convert a core [`crate::core::session_manager::SessionTreeNode`] to its RPC wire node.
pub fn session_tree_node_to_rpc(
    node: &crate::core::session_manager::SessionTreeNode,
) -> pi_js::Result<SessionTreeNode> {
    let entry = serde_json::to_value(&node.entry).map_err(|e| pi_js::Error::msg(e.to_string()))?;
    let children = node
        .children
        .iter()
        .map(session_tree_node_to_rpc)
        .collect::<pi_js::Result<Vec<_>>>()?;
    Ok(SessionTreeNode {
        entry,
        children,
        label: node.label.clone(),
        label_timestamp: node.label_timestamp.clone(),
    })
}

/// Notification severity for `notify` extension UI requests.
/// Placement for extension widget UI requests.
pub use crate::core::extensions::types::{NotifyType, WidgetPlacement};

// ============================================================================
// RPC Commands (stdin)
// ============================================================================

/// Command type discriminator matching TS `RpcCommandType = RpcCommand["type"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcCommandType {
    Prompt,
    Steer,
    FollowUp,
    Abort,
    ClearQueue,
    NewSession,
    GetState,
    SetModel,
    CycleModel,
    GetAvailableModels,
    SetThinkingLevel,
    CycleThinkingLevel,
    GetAvailableThinkingLevels,
    SetSteeringMode,
    SetFollowUpMode,
    Compact,
    SetAutoCompaction,
    SetAutoRetry,
    AbortRetry,
    Bash,
    AbortBash,
    GetSessionStats,
    ExportHtml,
    SwitchSession,
    Fork,
    Clone,
    GetForkMessages,
    GetEntries,
    GetTree,
    GetLastAssistantText,
    SetSessionName,
    GetMessages,
    GetCommands,
}

impl RpcCommandType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Steer => "steer",
            Self::FollowUp => "follow_up",
            Self::Abort => "abort",
            Self::ClearQueue => "clear_queue",
            Self::NewSession => "new_session",
            Self::GetState => "get_state",
            Self::SetModel => "set_model",
            Self::CycleModel => "cycle_model",
            Self::GetAvailableModels => "get_available_models",
            Self::SetThinkingLevel => "set_thinking_level",
            Self::CycleThinkingLevel => "cycle_thinking_level",
            Self::GetAvailableThinkingLevels => "get_available_thinking_levels",
            Self::SetSteeringMode => "set_steering_mode",
            Self::SetFollowUpMode => "set_follow_up_mode",
            Self::Compact => "compact",
            Self::SetAutoCompaction => "set_auto_compaction",
            Self::SetAutoRetry => "set_auto_retry",
            Self::AbortRetry => "abort_retry",
            Self::Bash => "bash",
            Self::AbortBash => "abort_bash",
            Self::GetSessionStats => "get_session_stats",
            Self::ExportHtml => "export_html",
            Self::SwitchSession => "switch_session",
            Self::Fork => "fork",
            Self::Clone => "clone",
            Self::GetForkMessages => "get_fork_messages",
            Self::GetEntries => "get_entries",
            Self::GetTree => "get_tree",
            Self::GetLastAssistantText => "get_last_assistant_text",
            Self::SetSessionName => "set_session_name",
            Self::GetMessages => "get_messages",
            Self::GetCommands => "get_commands",
        }
    }

    pub fn from_type_str(s: &str) -> Option<Self> {
        match s {
            "prompt" => Some(Self::Prompt),
            "steer" => Some(Self::Steer),
            "follow_up" => Some(Self::FollowUp),
            "abort" => Some(Self::Abort),
            "clear_queue" => Some(Self::ClearQueue),
            "new_session" => Some(Self::NewSession),
            "get_state" => Some(Self::GetState),
            "set_model" => Some(Self::SetModel),
            "cycle_model" => Some(Self::CycleModel),
            "get_available_models" => Some(Self::GetAvailableModels),
            "set_thinking_level" => Some(Self::SetThinkingLevel),
            "cycle_thinking_level" => Some(Self::CycleThinkingLevel),
            "get_available_thinking_levels" => Some(Self::GetAvailableThinkingLevels),
            "set_steering_mode" => Some(Self::SetSteeringMode),
            "set_follow_up_mode" => Some(Self::SetFollowUpMode),
            "compact" => Some(Self::Compact),
            "set_auto_compaction" => Some(Self::SetAutoCompaction),
            "set_auto_retry" => Some(Self::SetAutoRetry),
            "abort_retry" => Some(Self::AbortRetry),
            "bash" => Some(Self::Bash),
            "abort_bash" => Some(Self::AbortBash),
            "get_session_stats" => Some(Self::GetSessionStats),
            "export_html" => Some(Self::ExportHtml),
            "switch_session" => Some(Self::SwitchSession),
            "fork" => Some(Self::Fork),
            "clone" => Some(Self::Clone),
            "get_fork_messages" => Some(Self::GetForkMessages),
            "get_entries" => Some(Self::GetEntries),
            "get_tree" => Some(Self::GetTree),
            "get_last_assistant_text" => Some(Self::GetLastAssistantText),
            "set_session_name" => Some(Self::SetSessionName),
            "get_messages" => Some(Self::GetMessages),
            "get_commands" => Some(Self::GetCommands),
            _ => None,
        }
    }
}

/// RPC command sent on stdin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum RpcCommand {
    // Prompting
    #[serde(rename = "prompt")]
    Prompt {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        streaming_behavior: Option<RpcStreamingBehavior>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "steer")]
    Steer {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "follow_up")]
    FollowUp {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "abort")]
    Abort {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "clear_queue")]
    ClearQueue {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "new_session")]
    NewSession {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // State
    #[serde(rename = "get_state")]
    GetState {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // Model
    #[serde(rename = "set_model")]
    SetModel {
        provider: String,
        model_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "cycle_model")]
    CycleModel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "get_available_models")]
    GetAvailableModels {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // Thinking
    #[serde(rename = "set_thinking_level")]
    SetThinkingLevel {
        level: ThinkingLevel,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "cycle_thinking_level")]
    CycleThinkingLevel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "get_available_thinking_levels")]
    GetAvailableThinkingLevels {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // Queue modes
    #[serde(rename = "set_steering_mode")]
    SetSteeringMode {
        mode: QueueMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "set_follow_up_mode")]
    SetFollowUpMode {
        mode: QueueMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // Compaction
    #[serde(rename = "compact")]
    Compact {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "set_auto_compaction")]
    SetAutoCompaction {
        enabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // Retry
    #[serde(rename = "set_auto_retry")]
    SetAutoRetry {
        enabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "abort_retry")]
    AbortRetry {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // Bash
    #[serde(rename = "bash")]
    Bash {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exclude_from_context: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "abort_bash")]
    AbortBash {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // Session
    #[serde(rename = "get_session_stats")]
    GetSessionStats {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "export_html")]
    ExportHtml {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "switch_session")]
    SwitchSession {
        session_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "fork")]
    Fork {
        entry_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "clone")]
    Clone {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "get_fork_messages")]
    GetForkMessages {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "get_entries")]
    GetEntries {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "get_tree")]
    GetTree {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "get_last_assistant_text")]
    GetLastAssistantText {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename = "set_session_name")]
    SetSessionName {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // Messages
    #[serde(rename = "get_messages")]
    GetMessages {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // Commands (available for invocation via prompt)
    #[serde(rename = "get_commands")]
    GetCommands {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
}

impl RpcCommand {
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Prompt { id, .. }
            | Self::Steer { id, .. }
            | Self::FollowUp { id, .. }
            | Self::Abort { id }
            | Self::ClearQueue { id }
            | Self::NewSession { id, .. }
            | Self::GetState { id }
            | Self::SetModel { id, .. }
            | Self::CycleModel { id }
            | Self::GetAvailableModels { id }
            | Self::SetThinkingLevel { id, .. }
            | Self::CycleThinkingLevel { id }
            | Self::GetAvailableThinkingLevels { id }
            | Self::SetSteeringMode { id, .. }
            | Self::SetFollowUpMode { id, .. }
            | Self::Compact { id, .. }
            | Self::SetAutoCompaction { id, .. }
            | Self::SetAutoRetry { id, .. }
            | Self::AbortRetry { id }
            | Self::Bash { id, .. }
            | Self::AbortBash { id }
            | Self::GetSessionStats { id }
            | Self::ExportHtml { id, .. }
            | Self::SwitchSession { id, .. }
            | Self::Fork { id, .. }
            | Self::Clone { id }
            | Self::GetForkMessages { id }
            | Self::GetEntries { id, .. }
            | Self::GetTree { id }
            | Self::GetLastAssistantText { id }
            | Self::SetSessionName { id, .. }
            | Self::GetMessages { id }
            | Self::GetCommands { id } => id.as_deref(),
        }
    }

    pub fn set_id(&mut self, new_id: Option<String>) {
        match self {
            Self::Prompt { id, .. }
            | Self::Steer { id, .. }
            | Self::FollowUp { id, .. }
            | Self::Abort { id }
            | Self::ClearQueue { id }
            | Self::NewSession { id, .. }
            | Self::GetState { id }
            | Self::SetModel { id, .. }
            | Self::CycleModel { id }
            | Self::GetAvailableModels { id }
            | Self::SetThinkingLevel { id, .. }
            | Self::CycleThinkingLevel { id }
            | Self::GetAvailableThinkingLevels { id }
            | Self::SetSteeringMode { id, .. }
            | Self::SetFollowUpMode { id, .. }
            | Self::Compact { id, .. }
            | Self::SetAutoCompaction { id, .. }
            | Self::SetAutoRetry { id, .. }
            | Self::AbortRetry { id }
            | Self::Bash { id, .. }
            | Self::AbortBash { id }
            | Self::GetSessionStats { id }
            | Self::ExportHtml { id, .. }
            | Self::SwitchSession { id, .. }
            | Self::Fork { id, .. }
            | Self::Clone { id }
            | Self::GetForkMessages { id }
            | Self::GetEntries { id, .. }
            | Self::GetTree { id }
            | Self::GetLastAssistantText { id }
            | Self::SetSessionName { id, .. }
            | Self::GetMessages { id }
            | Self::GetCommands { id } => *id = new_id,
        }
    }

    pub const fn command_type(&self) -> RpcCommandType {
        match self {
            Self::Prompt { .. } => RpcCommandType::Prompt,
            Self::Steer { .. } => RpcCommandType::Steer,
            Self::FollowUp { .. } => RpcCommandType::FollowUp,
            Self::Abort { .. } => RpcCommandType::Abort,
            Self::ClearQueue { .. } => RpcCommandType::ClearQueue,
            Self::NewSession { .. } => RpcCommandType::NewSession,
            Self::GetState { .. } => RpcCommandType::GetState,
            Self::SetModel { .. } => RpcCommandType::SetModel,
            Self::CycleModel { .. } => RpcCommandType::CycleModel,
            Self::GetAvailableModels { .. } => RpcCommandType::GetAvailableModels,
            Self::SetThinkingLevel { .. } => RpcCommandType::SetThinkingLevel,
            Self::CycleThinkingLevel { .. } => RpcCommandType::CycleThinkingLevel,
            Self::GetAvailableThinkingLevels { .. } => RpcCommandType::GetAvailableThinkingLevels,
            Self::SetSteeringMode { .. } => RpcCommandType::SetSteeringMode,
            Self::SetFollowUpMode { .. } => RpcCommandType::SetFollowUpMode,
            Self::Compact { .. } => RpcCommandType::Compact,
            Self::SetAutoCompaction { .. } => RpcCommandType::SetAutoCompaction,
            Self::SetAutoRetry { .. } => RpcCommandType::SetAutoRetry,
            Self::AbortRetry { .. } => RpcCommandType::AbortRetry,
            Self::Bash { .. } => RpcCommandType::Bash,
            Self::AbortBash { .. } => RpcCommandType::AbortBash,
            Self::GetSessionStats { .. } => RpcCommandType::GetSessionStats,
            Self::ExportHtml { .. } => RpcCommandType::ExportHtml,
            Self::SwitchSession { .. } => RpcCommandType::SwitchSession,
            Self::Fork { .. } => RpcCommandType::Fork,
            Self::Clone { .. } => RpcCommandType::Clone,
            Self::GetForkMessages { .. } => RpcCommandType::GetForkMessages,
            Self::GetEntries { .. } => RpcCommandType::GetEntries,
            Self::GetTree { .. } => RpcCommandType::GetTree,
            Self::GetLastAssistantText { .. } => RpcCommandType::GetLastAssistantText,
            Self::SetSessionName { .. } => RpcCommandType::SetSessionName,
            Self::GetMessages { .. } => RpcCommandType::GetMessages,
            Self::GetCommands { .. } => RpcCommandType::GetCommands,
        }
    }
}

// ============================================================================
// RPC Slash Command (for get_commands response)
// ============================================================================

/// Kind of slash command returned by `get_commands`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RpcSlashCommandSource {
    Extension,
    Prompt,
    Skill,
}

/// A command available for invocation via prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSlashCommand {
    /// Command name (without leading slash)
    pub name: String,
    /// Human-readable description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// What kind of command this is
    pub source: RpcSlashCommandSource,
    /// Source metadata for the owning resource
    pub source_info: SourceInfo,
}

// ============================================================================
// RPC State
// ============================================================================

/// Session state returned by `get_state`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSessionState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Model>,
    pub thinking_level: ThinkingLevel,
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub steering_mode: QueueMode,
    pub follow_up_mode: QueueMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    pub auto_compaction_enabled: bool,
    pub message_count: i64,
    pub pending_message_count: i64,
}

// ============================================================================
// RPC Responses (stdout)
// ============================================================================

mod optional_nullable_value {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::Value;

    pub fn serialize<S: Serializer>(value: &Option<Value>, serializer: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(v) => v.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
        Value::deserialize(deserializer).map(Some)
    }
}

fn default_response_type() -> String {
    "response".to_string()
}

/// Typed payload structs for RPC command responses and client results.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponseData {
    pub disposition: PromptDisposition,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedInputResponseData {
    pub disposition: QueuedInputDisposition,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearQueueResult {
    pub steering: Vec<String>,
    pub follow_up: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelledResult {
    pub cancelled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleModelResponseData {
    pub model: Model,
    pub thinking_level: ThinkingLevel,
    pub is_scoped: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableModelsResponseData {
    pub models: Vec<Model>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleThinkingLevelResult {
    pub level: ThinkingLevel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableThinkingLevelsResponseData {
    pub levels: Vec<ThinkingLevel>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportHtmlResult {
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub cancelled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkMessage {
    pub entry_id: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkMessagesResponseData {
    pub messages: Vec<ForkMessage>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetEntriesResult {
    pub entries: Vec<SessionEntry>,
    pub leaf_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTreeResult {
    pub tree: Vec<SessionTreeNode>,
    pub leaf_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastAssistantTextResponseData {
    pub text: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetMessagesResponseData {
    pub messages: Vec<AgentMessage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetCommandsResponseData {
    pub commands: Vec<RpcSlashCommand>,
}

/// RPC response emitted on stdout.
///
/// Key order matches TS `rpc-mode.ts`: `id?`, `type`, `command`, `success`, `data?`/`error?`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type", default = "default_response_type")]
    pub r#type: String,
    pub command: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional_nullable_value")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RpcResponse {
    pub fn success_empty(id: Option<String>, command: impl Into<String>) -> Self {
        Self {
            id,
            r#type: "response".to_string(),
            command: command.into(),
            success: true,
            data: None,
            error: None,
        }
    }

    pub fn success_value(id: Option<String>, command: impl Into<String>, data: Value) -> Self {
        Self {
            id,
            r#type: "response".to_string(),
            command: command.into(),
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn success<T: Serialize>(id: Option<String>, command: impl Into<String>, data: &T) -> pi_js::Result<Self> {
        let value = serde_json::to_value(data).map_err(|e| pi_js::Error::msg(e.to_string()))?;
        Ok(Self::success_value(id, command, value))
    }

    pub fn error(id: Option<String>, command: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id,
            r#type: "response".to_string(),
            command: command.into(),
            success: false,
            data: None,
            error: Some(message.into()),
        }
    }
}

/// Extension error event emitted on stdout by RPC mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcExtensionErrorEvent {
    #[serde(rename = "type")]
    pub r#type: String,
    pub extension_path: String,
    pub event: String,
    pub error: String,
}

impl RpcExtensionErrorEvent {
    pub fn new(extension_path: impl Into<String>, event: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            r#type: "extension_error".to_string(),
            extension_path: extension_path.into(),
            event: event.into(),
            error: error.into(),
        }
    }
}

// ============================================================================
// Extension UI Events (stdout)
// ============================================================================

/// Emitted when an extension needs user input or updates host UI state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RpcExtensionUIRequest {
    Select {
        id: String,
        title: String,
        options: Vec<String>,
        timeout: Option<u64>,
    },
    Confirm {
        id: String,
        title: String,
        message: String,
        timeout: Option<u64>,
    },
    Input {
        id: String,
        title: String,
        placeholder: Option<String>,
        timeout: Option<u64>,
    },
    Editor {
        id: String,
        title: String,
        prefill: Option<String>,
    },
    Notify {
        id: String,
        message: String,
        notify_type: Option<NotifyType>,
    },
    SetStatus {
        id: String,
        status_key: String,
        status_text: Option<String>,
    },
    SetWidget {
        id: String,
        widget_key: String,
        widget_lines: Option<Vec<String>>,
        widget_placement: Option<WidgetPlacement>,
    },
    SetTitle {
        id: String,
        title: String,
    },
    SetEditorText {
        id: String,
        text: String,
    },
}

impl RpcExtensionUIRequest {
    pub fn id(&self) -> &str {
        match self {
            Self::Select { id, .. }
            | Self::Confirm { id, .. }
            | Self::Input { id, .. }
            | Self::Editor { id, .. }
            | Self::Notify { id, .. }
            | Self::SetStatus { id, .. }
            | Self::SetWidget { id, .. }
            | Self::SetTitle { id, .. }
            | Self::SetEditorText { id, .. } => id,
        }
    }
}

impl Serialize for RpcExtensionUIRequest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Key order matches TS rpc-mode.ts: type, id, method, then method-specific fields.
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("type", "extension_ui_request")?;
        match self {
            Self::Select {
                id,
                title,
                options,
                timeout,
            } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("method", "select")?;
                map.serialize_entry("title", title)?;
                map.serialize_entry("options", options)?;
                if let Some(t) = timeout {
                    map.serialize_entry("timeout", t)?;
                }
            }
            Self::Confirm {
                id,
                title,
                message,
                timeout,
            } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("method", "confirm")?;
                map.serialize_entry("title", title)?;
                map.serialize_entry("message", message)?;
                if let Some(t) = timeout {
                    map.serialize_entry("timeout", t)?;
                }
            }
            Self::Input {
                id,
                title,
                placeholder,
                timeout,
            } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("method", "input")?;
                map.serialize_entry("title", title)?;
                if let Some(p) = placeholder {
                    map.serialize_entry("placeholder", p)?;
                }
                if let Some(t) = timeout {
                    map.serialize_entry("timeout", t)?;
                }
            }
            Self::Editor { id, title, prefill } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("method", "editor")?;
                map.serialize_entry("title", title)?;
                if let Some(p) = prefill {
                    map.serialize_entry("prefill", p)?;
                }
            }
            Self::Notify {
                id,
                message,
                notify_type,
            } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("method", "notify")?;
                map.serialize_entry("message", message)?;
                if let Some(nt) = notify_type {
                    map.serialize_entry("notifyType", nt)?;
                }
            }
            Self::SetStatus {
                id,
                status_key,
                status_text,
            } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("method", "setStatus")?;
                map.serialize_entry("statusKey", status_key)?;
                if let Some(st) = status_text {
                    map.serialize_entry("statusText", st)?;
                }
            }
            Self::SetWidget {
                id,
                widget_key,
                widget_lines,
                widget_placement,
            } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("method", "setWidget")?;
                map.serialize_entry("widgetKey", widget_key)?;
                if let Some(lines) = widget_lines {
                    map.serialize_entry("widgetLines", lines)?;
                }
                if let Some(placement) = widget_placement {
                    map.serialize_entry("widgetPlacement", placement)?;
                }
            }
            Self::SetTitle { id, title } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("method", "setTitle")?;
                map.serialize_entry("title", title)?;
            }
            Self::SetEditorText { id, text } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("method", "set_editor_text")?;
                map.serialize_entry("text", text)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for RpcExtensionUIRequest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "method", rename_all_fields = "camelCase")]
        enum RawMethod {
            #[serde(rename = "select")]
            Select {
                id: String,
                title: String,
                options: Vec<String>,
                #[serde(default)]
                timeout: Option<u64>,
            },
            #[serde(rename = "confirm")]
            Confirm {
                id: String,
                title: String,
                message: String,
                #[serde(default)]
                timeout: Option<u64>,
            },
            #[serde(rename = "input")]
            Input {
                id: String,
                title: String,
                #[serde(default)]
                placeholder: Option<String>,
                #[serde(default)]
                timeout: Option<u64>,
            },
            #[serde(rename = "editor")]
            Editor {
                id: String,
                title: String,
                #[serde(default)]
                prefill: Option<String>,
            },
            #[serde(rename = "notify")]
            Notify {
                id: String,
                message: String,
                #[serde(default)]
                notify_type: Option<NotifyType>,
            },
            #[serde(rename = "setStatus")]
            SetStatus {
                id: String,
                status_key: String,
                #[serde(default)]
                status_text: Option<String>,
            },
            #[serde(rename = "setWidget")]
            SetWidget {
                id: String,
                widget_key: String,
                #[serde(default)]
                widget_lines: Option<Vec<String>>,
                #[serde(default)]
                widget_placement: Option<WidgetPlacement>,
            },
            #[serde(rename = "setTitle")]
            SetTitle { id: String, title: String },
            #[serde(rename = "set_editor_text")]
            SetEditorText { id: String, text: String },
        }

        let mut map = IndexMap::<String, Value>::deserialize(deserializer)?;
        if let Some(kind) = map.get("type").and_then(Value::as_str)
            && kind != "extension_ui_request"
        {
            return Err(de::Error::custom(format!(
                "expected type \"extension_ui_request\", got \"{kind}\""
            )));
        }
        map.shift_remove("type");
        let obj = Value::Object(map.into_iter().collect());
        let raw = RawMethod::deserialize(obj).map_err(de::Error::custom)?;
        Ok(match raw {
            RawMethod::Select {
                id,
                title,
                options,
                timeout,
            } => Self::Select {
                id,
                title,
                options,
                timeout,
            },
            RawMethod::Confirm {
                id,
                title,
                message,
                timeout,
            } => Self::Confirm {
                id,
                title,
                message,
                timeout,
            },
            RawMethod::Input {
                id,
                title,
                placeholder,
                timeout,
            } => Self::Input {
                id,
                title,
                placeholder,
                timeout,
            },
            RawMethod::Editor { id, title, prefill } => Self::Editor { id, title, prefill },
            RawMethod::Notify {
                id,
                message,
                notify_type,
            } => Self::Notify {
                id,
                message,
                notify_type,
            },
            RawMethod::SetStatus {
                id,
                status_key,
                status_text,
            } => Self::SetStatus {
                id,
                status_key,
                status_text,
            },
            RawMethod::SetWidget {
                id,
                widget_key,
                widget_lines,
                widget_placement,
            } => Self::SetWidget {
                id,
                widget_key,
                widget_lines,
                widget_placement,
            },
            RawMethod::SetTitle { id, title } => Self::SetTitle { id, title },
            RawMethod::SetEditorText { id, text } => Self::SetEditorText { id, text },
        })
    }
}

// ============================================================================
// Extension UI Commands (stdin)
// ============================================================================

/// Response to an extension UI request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RpcExtensionUIResponse {
    Value { id: String, value: String },
    Confirmed { id: String, confirmed: bool },
    Cancelled { id: String },
}

impl RpcExtensionUIResponse {
    pub fn id(&self) -> &str {
        match self {
            Self::Value { id, .. } | Self::Confirmed { id, .. } | Self::Cancelled { id } => id,
        }
    }
}

impl Serialize for RpcExtensionUIResponse {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("type", "extension_ui_response")?;
        match self {
            Self::Value { id, value } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("value", value)?;
            }
            Self::Confirmed { id, confirmed } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("confirmed", confirmed)?;
            }
            Self::Cancelled { id } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("cancelled", &true)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for RpcExtensionUIResponse {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(rename = "type")]
            kind: Option<String>,
            id: String,
            #[serde(default)]
            value: Option<String>,
            #[serde(default)]
            confirmed: Option<bool>,
            #[serde(default)]
            cancelled: Option<bool>,
        }

        let raw = Raw::deserialize(deserializer)?;
        if let Some(kind) = raw.kind.as_deref()
            && kind != "extension_ui_response"
        {
            return Err(de::Error::custom(format!(
                "expected type \"extension_ui_response\", got \"{kind}\""
            )));
        }
        if raw.cancelled == Some(true) {
            return Ok(Self::Cancelled { id: raw.id });
        }
        if let Some(value) = raw.value {
            return Ok(Self::Value { id: raw.id, value });
        }
        if let Some(confirmed) = raw.confirmed {
            return Ok(Self::Confirmed { id: raw.id, confirmed });
        }
        Err(de::Error::custom(
            "extension_ui_response requires \"value\", \"confirmed\", or \"cancelled\": true",
        ))
    }
}
