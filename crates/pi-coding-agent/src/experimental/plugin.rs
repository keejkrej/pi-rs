//! Port of packages/coding-agent/src/experimental/plugin.ts

pub use crate::experimental::services::agent_controller::{
    AGENT_CONTROLLER_SERVICE, AgentCompactionRequest, AgentController, AgentOperationError, AgentOperationResponse,
    AgentPromptImage, AgentPromptRequest, AgentPromptResult, AgentQueueResponse,
};
pub use crate::experimental::services::presentation_ui::{
    PRESENTATION_UI_SERVICE, PresentationSelectItem, PresentationUI, PresentationUiApi,
};
pub use crate::experimental::services::slash_commands::{
    SLASH_COMMANDS_SERVICE, SlashCommandCompletion, SlashCommandContribution, SlashCommandRunResult, SlashCommands,
    SlashCommandsApi,
};
