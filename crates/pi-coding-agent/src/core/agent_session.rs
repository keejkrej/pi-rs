//! Port of packages/coding-agent/src/core/agent-session.ts#L1-L1385

// @generated-mods begin (scaffold-owned, do not edit)
mod part_2;
pub use part_2::*;
mod part_3;
// @generated-mods end

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use pi_agent_core::agent::Agent;
use pi_agent_core::agent_loop::run_tool_call;
use pi_agent_core::types::{
    AfterToolCallContext, AfterToolCallResult, AgentContext, AgentEvent, AgentLoopTurnUpdate, AgentMessage, AgentTool,
    AgentToolCall, AgentToolCallOutcome, AgentToolResult, BeforeToolCallContext, BeforeToolCallResult,
    FinishTurnAction, FinishTurnContext, FinishTurnDecision, PrepareNextTurnContext, PrepareRequestContext,
    PrepareRequestResult,
};
use pi_ai::AnyModel;
use pi_ai::types::{
    AssistantMessage, AssistantMessageEvent, ImageContent, Model, ModelThinkingLevel, ProviderEnv, ProviderHeaders,
    ToolResultMessage, UserContentBlock,
};
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::{BoxFuture, Unsubscribe};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::core::auth_guidance::format_no_api_key_found_message;
use crate::core::cache_warmer::CacheWarmer;
use crate::core::compaction::{CompactionResult, estimate_projected_context_tokens, should_compact};
use crate::core::defaults::{from_agent_thinking_level, to_agent_thinking_level};
use crate::core::extensions::runner::{
    BoundaryBaseEvent, ExtensionErrorListener, ExtensionRunner, RunnerEmitEvent, ShutdownHandler,
};
use crate::core::extensions::types::{
    AgentActivityOutcome, AgentEndEvent, AgentSettledEvent, AgentStartEvent, BashToolResultEvent,
    BoundaryContextPreview, CompactionReason as ExtCompactionReason, ContextUsage, CustomToolResultEvent,
    EditToolResultEvent, ExecuteToolOptions, ExtensionCommandContextActions, ExtensionError, ExtensionMode,
    ExtensionUIContext, FindToolResultEvent, GrepToolResultEvent, LsToolResultEvent, MessageEndEvent,
    MessageStartEvent, MessageUpdateEvent, PowerShellToolResultEvent, ReadToolResultEvent, SessionBoundaryDraft,
    SessionCompactFailedEvent, SessionStartEvent, SessionStartReason, ToolCallEvent, ToolDefinition,
    ToolExecutionEndEvent, ToolExecutionStartEvent, ToolExecutionUpdateEvent, ToolResultContentBlock, ToolResultEvent,
    TurnStartEvent, WriteToolResultEvent,
};
use crate::core::messages::{BashExecutionMessage, convert_to_llm};
use crate::core::model_runtime::{ModelRuntime, ModelRuntimeAuthOverrides, ResolveModelOptions};
use crate::core::nested_tool_calls::{
    NestedCallSummary, NestedToolCallHost, NestedToolCallOptions, NestedToolCallRunner, NestedToolExecutionEvent,
    NestedToolUpdateAsyncCallback,
};
use crate::core::resource_loader::ResourceLoader;
use crate::core::sdk::ScopedModelEntry;
use crate::core::session_manager::{BranchSummaryEntry, FileEntry, SessionEntry, SessionManager, SessionProjection};
use crate::core::settings_manager::{ModelRef, SettingsManager};
use crate::core::source_info::SourceInfo;
use crate::core::tools::{
    BashToolDetails, EditToolDetails, FindToolDetails, GrepToolDetails, LsToolDetails, PowerShellToolDetails,
    ReadToolDetails,
};

pub type ExtensionRunnerRef = Arc<Mutex<Option<ExtensionRunner>>>;
use crate::core::system_prompt::{
    BuildSystemPromptOptions, NormalizedBuildSystemPromptOptions, normalize_build_system_prompt_options,
};

pub(crate) fn normalized_to_build_options(opts: &NormalizedBuildSystemPromptOptions) -> BuildSystemPromptOptions {
    BuildSystemPromptOptions {
        custom_prompt: opts.custom_prompt.clone(),
        force_system_prompt: opts.force_system_prompt.clone(),
        selected_tools: Some(opts.selected_tools.clone()),
        hidden_tools: Some(opts.hidden_tools.clone()),
        tool_snippets: Some(opts.tool_snippets.clone()),
        tool_guidelines: Some(opts.tool_guidelines.clone()),
        prompt_guidelines: Some(opts.prompt_guidelines.clone()),
        append_system_prompt: Some(opts.append_system_prompt.clone()),
        sections: Some(opts.sections.clone()),
        cwd: opts.cwd.clone(),
        context_files: Some(opts.context_files.clone()),
        skills: Some(opts.skills.clone()),
    }
}

pub(crate) fn resolved_compaction_settings(
    settings_manager: &SettingsManager,
    model: Option<&Model>,
) -> crate::core::compaction::compaction::CompactionSettings {
    let model_ref = model.map(|m| ModelRef {
        provider: m.provider.clone(),
        id: m.id.clone(),
    });
    match settings_manager.get_compaction_settings(model_ref.as_ref()) {
        Ok(s) => crate::core::compaction::compaction::CompactionSettings {
            enabled: s.enabled,
            reserve_tokens: s.reserve_tokens,
            keep_recent_tokens: s.keep_recent_tokens,
        },
        Err(_) => crate::core::compaction::compaction::DEFAULT_COMPACTION_SETTINGS,
    }
}

pub(crate) fn to_ext_compaction_reason(reason: CompactionReason) -> ExtCompactionReason {
    match reason {
        CompactionReason::Manual => ExtCompactionReason::Manual,
        CompactionReason::Threshold => ExtCompactionReason::Threshold,
        CompactionReason::Overflow => ExtCompactionReason::Overflow,
    }
}
use crate::core::usage_totals::combine_usage;
use crate::core::virtual_models::{
    ModelRouteReason, VIRTUAL_MODEL_STATE_ENTRY, VirtualModelStateData, get_branch_selection, get_virtual_model_state,
    is_virtual_model,
};
use crate::utils::tool_result_images::{NormalizeToolResultImagesOptions, normalize_tool_result_images};

// ============================================================================
// Skill Block Parsing
// ============================================================================

/// Parsed skill block from a user message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSkillBlock {
    pub name: String,
    pub location: String,
    pub content: String,
    pub user_message: Option<String>,
}

/// Parse a skill block from message text.
/// Returns `None` if the text doesn't contain a skill block.
pub fn parse_skill_block(text: &str) -> Option<ParsedSkillBlock> {
    static SKILL_BLOCK_RE: std::sync::LazyLock<Option<Regex>> = std::sync::LazyLock::new(|| {
        Regex::new(r#"(?s)^<skill name="([^"]+)" location="([^"]+)">\n(.*?)\n</skill>(?:\n\n(.+))?$"#).ok()
    });
    let caps = SKILL_BLOCK_RE.as_ref()?.captures(text)?;
    let name = caps.get(1)?.as_str().to_string();
    let location = caps.get(2)?.as_str().to_string();
    let content = caps.get(3)?.as_str().to_string();
    let user_message = caps
        .get(4)
        .map(|m| pi_js::str16::trim(m.as_str()).to_string())
        .filter(|s| !s.is_empty());
    Some(ParsedSkillBlock {
        name,
        location,
        content,
        user_message,
    })
}

// ============================================================================
// Session Events
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactionReason {
    Manual,
    Threshold,
    Overflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SummarizationRetrySource {
    BranchSummary,
    Compaction,
}

/// Session-specific events that extend the core `AgentEvent`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentSessionEvent {
    #[serde(rename = "agent_start")]
    AgentStart,
    #[serde(rename = "agent_end")]
    AgentEnd {
        messages: Vec<AgentMessage>,
        #[serde(rename = "willRetry")]
        will_retry: bool,
    },
    #[serde(rename = "agent_settled")]
    AgentSettled,
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
        partial_result: AgentToolResult,
        #[serde(rename = "parentToolCallId", default, skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
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
    EntryAppended { entry: SessionEntry },
    #[serde(rename = "session_info_changed")]
    SessionInfoChanged { name: Option<String> },
    #[serde(rename = "thinking_level_changed")]
    ThinkingLevelChanged { level: ModelThinkingLevel },
    #[serde(rename = "compaction_end")]
    CompactionEnd {
        reason: CompactionReason,
        result: Option<CompactionResult>,
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
}

/// Listener function for agent session events.
pub type AgentSessionEventListener = Arc<dyn Fn(AgentSessionEvent) + Send + Sync>;

// ============================================================================
// Types
// ============================================================================

pub(crate) fn without_deleted_headers(headers: Option<&ProviderHeaders>) -> Option<IndexMap<String, String>> {
    headers.map(|h| {
        h.iter()
            .filter_map(|(k, v)| v.as_ref().map(|val| (k.clone(), val.clone())))
            .collect()
    })
}

#[derive(Clone)]
pub struct AgentSessionConfig {
    pub agent: Agent,
    pub session_manager: SessionManager,
    pub settings_manager: SettingsManager,
    pub cwd: String,
    pub scoped_models: Option<Vec<ScopedModelEntry>>,
    pub resource_loader: Arc<dyn ResourceLoader>,
    pub custom_tools: Option<Vec<ToolDefinition>>,
    pub model_runtime: ModelRuntime,
    pub cache_warmer: Option<Arc<CacheWarmer>>,
    pub initial_active_tool_names: Option<Vec<String>>,
    pub uses_default_tools: Option<bool>,
    pub allowed_tool_names: Option<Vec<String>>,
    pub excluded_tool_names: Option<Vec<String>>,
    pub base_tools_override: Option<IndexMap<String, AgentTool>>,
    pub extension_runner_ref: Option<ExtensionRunnerRef>,
    pub session_start_event: Option<SessionStartEvent>,
    /// Tracks whether a concrete model was selected (since `Agent` substitutes a fallback model when None).
    pub initial_model: Option<Model>,
    /// Preserves `ModelThinkingLevel::Max` separately from `pi_agent_core::ThinkingLevel`.
    pub initial_thinking_level: Option<ModelThinkingLevel>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamingBehavior {
    Steer,
    FollowUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptPreflightOutcome {
    Started,
    Queued,
    Handled,
}

pub type PromptPreflightCallback = Arc<dyn Fn(PromptPreflightOutcome) + Send + Sync>;

#[derive(Clone, Default)]
pub struct PromptOptions {
    pub expand_prompt_templates: Option<bool>,
    pub streaming_behavior: Option<StreamingBehavior>,
    pub images: Option<Vec<ImageContent>>,
    pub source: Option<crate::core::extensions::types::InputSource>,
    pub preflight_result: Option<PromptPreflightCallback>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuedInputDisposition {
    Queued,
    Handled,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct QueueInputOptions {
    pub source: Option<crate::core::extensions::types::InputSource>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CycleDirection {
    Forward,
    Backward,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ModelMutationOptions {
    pub persist: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelCycleResult {
    pub model: Model,
    pub thinking_level: ModelThinkingLevel,
    pub is_scoped: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoutedModel {
    pub model: Model,
    pub thinking_level: Option<ModelThinkingLevel>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClearQueueResult {
    pub steering: Vec<String>,
    pub follow_up: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTokenStats {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub total: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    pub session_id: String,
    pub user_messages: i64,
    pub assistant_messages: i64,
    pub tool_calls: i64,
    pub tool_results: i64,
    pub total_messages: i64,
    pub tokens: SessionTokenStats,
    pub cost: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<ContextUsage>,
}

#[derive(Clone, Debug, Default)]
pub struct NavigateTreeOptions {
    pub summarize: Option<bool>,
    pub custom_instructions: Option<String>,
    pub replace_instructions: Option<bool>,
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NavigateTreeResult {
    pub editor_text: Option<String>,
    pub cancelled: bool,
    pub aborted: Option<bool>,
    pub summary_entry: Option<BranchSummaryEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkableUserMessage {
    pub entry_id: String,
    pub text: String,
}

#[derive(Clone, Default)]
pub struct ExecuteBashOptions {
    pub exclude_from_context: Option<bool>,
    pub id: Option<String>,
    pub operations: Option<Arc<dyn crate::core::tools::bash::BashOperations>>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RecordBashOptions {
    pub exclude_from_context: Option<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct ExportHtmlOptions {
    pub theme_name: Option<String>,
}

#[derive(Clone)]
pub struct SummarizeBugReportOptions {
    pub hint: Option<String>,
    pub signal: AbortSignal,
}

#[derive(Default)]
pub struct ReloadOptions {
    pub before_session_start: Option<Box<dyn FnOnce() -> BoxFuture<pi_js::Result<()>> + Send>>,
}

#[derive(Clone, Default)]
pub struct ExtensionBindings {
    pub ui_context: Option<Arc<dyn ExtensionUIContext>>,
    pub mode: Option<ExtensionMode>,
    pub command_context_actions: Option<ExtensionCommandContextActions>,
    pub abort_handler: Option<Arc<dyn Fn() + Send + Sync>>,
    pub shutdown_handler: Option<ShutdownHandler>,
    pub on_error: Option<ExtensionErrorListener>,
}

#[derive(Clone)]
pub(crate) struct ToolDefinitionEntry {
    pub definition: ToolDefinition,
    pub source_info: SourceInfo,
}

pub(crate) type DeferredSettledAction = Box<dyn FnOnce() -> BoxFuture<()> + Send>;

pub(crate) struct SummarizationRequestAuth {
    pub model: Model,
    pub api_key: Option<String>,
    pub headers: Option<IndexMap<String, String>>,
    pub env: Option<ProviderEnv>,
    pub thinking_level: ModelThinkingLevel,
}

pub(crate) struct RequestAuth {
    pub model: Model,
    pub api_key: Option<String>,
    pub headers: Option<IndexMap<String, String>>,
    pub env: Option<ProviderEnv>,
}

pub(crate) struct AgentSessionState {
    pub event_listeners: Vec<(u64, AgentSessionEventListener)>,
    pub next_listener_id: u64,
    pub unsubscribe_agent: Option<Unsubscribe>,
    pub has_model: bool,
    pub thinking_level: ModelThinkingLevel,
    pub compaction_abort_controller: Option<Arc<AbortController>>,
    pub auto_compaction_abort_controller: Option<Arc<AbortController>>,
    pub branch_summary_abort_controller: Option<Arc<AbortController>>,
    pub retry_abort_controller: Option<Arc<AbortController>>,
    pub retry_attempt: u32,
    pub retry_waiters: Vec<oneshot::Sender<()>>,
    pub steering_messages: Vec<String>,
    pub follow_up_messages: Vec<String>,
    pub pending_next_turn_messages: Vec<AgentMessage>,
    pub bash_abort_controllers: Vec<(u64, Arc<AbortController>)>,
    pub next_bash_id: u64,
    pub pending_bash_messages: Vec<BashExecutionMessage>,
    pub pending_custom_messages: Vec<AgentMessage>,
    pub scoped_models: Vec<ScopedModelEntry>,
    pub is_agent_run_active: bool,
    pub agent_run_abort_requested: bool,
    pub overflow_recovery_attempted: bool,
    pub idle_waiters: Vec<oneshot::Sender<()>>,
    pub turn_index: usize,
    pub last_assistant_message: Option<AssistantMessage>,
    pub last_assistant_tool_results: Vec<ToolResultMessage>,
    pub failed_response: Option<AssistantMessage>,
    pub entry_ids_by_message: Vec<(AgentMessage, String)>,
    pub message_end_replacements: Vec<(AgentMessage, AgentMessage)>,
    pub boundary_dispatched_messages: Vec<AssistantMessage>,
    pub pending_extension_send_tasks: Vec<tokio::task::JoinHandle<()>>,
    pub last_activity_outcome: AgentActivityOutcome,
    pub is_before_settle: bool,
    pub abort_during_before_settle: bool,
    pub is_emitting_agent_settled: bool,
    pub deferred_settled_actions: Vec<DeferredSettledAction>,
    pub custom_tools: Vec<ToolDefinition>,
    pub base_tool_definitions: IndexMap<String, ToolDefinition>,
    pub cwd: String,
    pub extension_runner_ref: Option<ExtensionRunnerRef>,
    pub extension_runner: Option<ExtensionRunner>,
    pub initial_active_tool_names: Option<Vec<String>>,
    pub pending_tool_names: IndexSet<String>,
    pub uses_default_tools: bool,
    pub allowed_tool_names: Option<IndexSet<String>>,
    pub excluded_tool_names: Option<IndexSet<String>>,
    pub base_tools_override: Option<IndexMap<String, AgentTool>>,
    pub session_start_event: SessionStartEvent,
    pub extension_ui_context: Option<Arc<dyn ExtensionUIContext>>,
    pub extension_mode: ExtensionMode,
    pub extension_command_context_actions: Option<ExtensionCommandContextActions>,
    pub extension_abort_handler: Option<Arc<dyn Fn() + Send + Sync>>,
    pub extension_shutdown_handler: Option<ShutdownHandler>,
    pub extension_error_listener: Option<ExtensionErrorListener>,
    pub extension_error_unsubscriber: Option<Unsubscribe>,
    pub tool_registry: IndexMap<String, AgentTool>,
    pub nested_tool_calls: Option<Arc<NestedToolCallRunner>>,
    pub completed_nested_call_summaries: IndexMap<String, NestedCallSummary>,
    pub hidden_declarations: IndexSet<String>,
    pub tool_definitions: IndexMap<String, ToolDefinitionEntry>,
    pub tool_prompt_snippets: IndexMap<String, String>,
    pub tool_prompt_guidelines: IndexMap<String, Vec<String>>,
    pub base_system_prompt_options: NormalizedBuildSystemPromptOptions,
    pub run_system_prompt_options: Option<NormalizedBuildSystemPromptOptions>,
}

pub(crate) struct AgentSessionInner {
    pub agent: Agent,
    pub session_manager: SessionManager,
    pub settings_manager: SettingsManager,
    pub resource_loader: Arc<dyn ResourceLoader>,
    pub model_runtime: ModelRuntime,
    pub cache_warmer: Option<Arc<CacheWarmer>>,
    pub state: Mutex<AgentSessionState>,
}

/// Core abstraction for agent lifecycle and session management.
#[derive(Clone)]
pub struct AgentSession {
    pub(crate) inner: Arc<AgentSessionInner>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoundaryKind {
    TurnEnd,
    AgentBeforeSettle,
}

impl AgentSession {
    pub fn new(config: AgentSessionConfig) -> Self {
        let has_model = config.initial_model.is_some();
        let thinking_level = config
            .initial_thinking_level
            .unwrap_or_else(|| from_agent_thinking_level(config.agent.state().thinking_level));
        let base_prompt_opts = normalize_build_system_prompt_options(&BuildSystemPromptOptions {
            cwd: config.cwd.clone(),
            ..Default::default()
        });

        let state = AgentSessionState {
            event_listeners: Vec::new(),
            next_listener_id: 1,
            unsubscribe_agent: None,
            has_model,
            thinking_level,
            compaction_abort_controller: None,
            auto_compaction_abort_controller: None,
            branch_summary_abort_controller: None,
            retry_abort_controller: None,
            retry_attempt: 0,
            retry_waiters: Vec::new(),
            steering_messages: Vec::new(),
            follow_up_messages: Vec::new(),
            pending_next_turn_messages: Vec::new(),
            bash_abort_controllers: Vec::new(),
            next_bash_id: 1,
            pending_bash_messages: Vec::new(),
            pending_custom_messages: Vec::new(),
            scoped_models: config.scoped_models.unwrap_or_default(),
            is_agent_run_active: false,
            agent_run_abort_requested: false,
            overflow_recovery_attempted: false,
            idle_waiters: Vec::new(),
            turn_index: 0,
            last_assistant_message: None,
            last_assistant_tool_results: Vec::new(),
            failed_response: None,
            entry_ids_by_message: Vec::new(),
            message_end_replacements: Vec::new(),
            boundary_dispatched_messages: Vec::new(),
            pending_extension_send_tasks: Vec::new(),
            last_activity_outcome: AgentActivityOutcome::Completed,
            is_before_settle: false,
            abort_during_before_settle: false,
            is_emitting_agent_settled: false,
            deferred_settled_actions: Vec::new(),
            custom_tools: config.custom_tools.unwrap_or_default(),
            base_tool_definitions: IndexMap::new(),
            cwd: config.cwd,
            extension_runner_ref: config.extension_runner_ref,
            extension_runner: None,
            initial_active_tool_names: config.initial_active_tool_names.clone(),
            pending_tool_names: IndexSet::new(),
            uses_default_tools: config.uses_default_tools.unwrap_or(false),
            allowed_tool_names: config.allowed_tool_names.map(|v| v.into_iter().collect()),
            excluded_tool_names: config.excluded_tool_names.map(|v| v.into_iter().collect()),
            base_tools_override: config.base_tools_override,
            session_start_event: config.session_start_event.unwrap_or(SessionStartEvent {
                reason: SessionStartReason::Startup,
                previous_session_file: None,
            }),
            extension_ui_context: None,
            extension_mode: ExtensionMode::Print,
            extension_command_context_actions: None,
            extension_abort_handler: None,
            extension_shutdown_handler: None,
            extension_error_listener: None,
            extension_error_unsubscriber: None,
            tool_registry: IndexMap::new(),
            nested_tool_calls: None,
            completed_nested_call_summaries: IndexMap::new(),
            hidden_declarations: IndexSet::new(),
            tool_definitions: IndexMap::new(),
            tool_prompt_snippets: IndexMap::new(),
            tool_prompt_guidelines: IndexMap::new(),
            base_system_prompt_options: base_prompt_opts,
            run_system_prompt_options: None,
        };

        let session = Self {
            inner: Arc::new(AgentSessionInner {
                agent: config.agent,
                session_manager: config.session_manager,
                settings_manager: config.settings_manager,
                resource_loader: config.resource_loader,
                model_runtime: config.model_runtime,
                cache_warmer: config.cache_warmer,
                state: Mutex::new(state),
            }),
        };

        if let Some(ref warmer) = session.inner.cache_warmer {
            let weak = Arc::downgrade(&session.inner);
            warmer.set_on_warmed(Some(Arc::new(move |entry| {
                if let Some(inner) = weak.upgrade() {
                    AgentSession { inner }._emit(AgentSessionEvent::EntryAppended {
                        entry: SessionEntry::Usage(entry.clone()),
                    });
                }
            })));
        }

        let weak_ev = Arc::downgrade(&session.inner);
        let unsub = session.inner.agent.subscribe_async(move |event, _signal| {
            let weak = weak_ev.clone();
            Box::pin(async move {
                if let Some(inner) = weak.upgrade() {
                    AgentSession { inner }._handle_agent_event(event).await;
                }
            })
        });
        session
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .unsubscribe_agent = Some(unsub);

        session._install_agent_tool_hooks();
        session._install_agent_next_turn_refresh();
        session._install_agent_request_projection();
        session._install_agent_boundary_hooks();
        session._install_hidden_declarations_projection();
        session._install_agent_forced_prompt_projection();

        let initial_active = config.initial_active_tool_names;
        session._build_runtime(initial_active.as_deref(), None, true);
        if initial_active.is_none() {
            session._restore_tools_from_transcript();
        }

        session
    }

    pub fn model_runtime(&self) -> &ModelRuntime {
        &self.inner.model_runtime
    }

    pub fn ptr_key(&self) -> usize {
        Arc::as_ptr(&self.inner) as usize
    }

    pub(crate) async fn _get_required_request_auth(
        &self,
        model: &Model,
        signal: Option<AbortSignal>,
    ) -> pi_js::Result<RequestAuth> {
        let result = match self
            .inner
            .model_runtime
            .get_auth_model(
                &AnyModel::Chat(model.clone()),
                Some(ModelRuntimeAuthOverrides {
                    signal,
                    ..Default::default()
                }),
            )
            .await
        {
            Ok(res) => res,
            Err(err) => {
                if err.to_string().contains("authHeader requires a resolved API key") {
                    return Err(pi_js::Error::msg(format_no_api_key_found_message(&model.provider)));
                }
                return Err(err);
            }
        };

        if let Some(res) = result
            && (res.auth.api_key.is_some() || res.auth.headers.is_some())
        {
            let mut request_model = model.clone();
            if let Some(base_url) = res.auth.base_url {
                request_model.base_url = base_url;
            }
            return Ok(RequestAuth {
                model: request_model,
                api_key: res.auth.api_key,
                headers: without_deleted_headers(res.auth.headers.as_ref()),
                env: res.env,
            });
        }

        if self.inner.model_runtime.is_using_oauth(&model.provider) {
            return Err(pi_js::Error::msg(format!(
                "Authentication failed for \"{}\". Credentials may have expired or network is unavailable. Run '/login {}' to re-authenticate.",
                model.provider, model.provider
            )));
        }
        Err(pi_js::Error::msg(format_no_api_key_found_message(&model.provider)))
    }

    pub(crate) async fn _get_summarization_request_auth(
        &self,
        selected_model: &Model,
        signal: Option<AbortSignal>,
    ) -> pi_js::Result<SummarizationRequestAuth> {
        let current_thinking = self.thinking_level();
        let (model, thinking_level) = if is_virtual_model(selected_model) {
            let messages = convert_to_llm(&self.messages());
            let route = self
                .inner
                .model_runtime
                .resolve_model(
                    selected_model,
                    &messages,
                    ResolveModelOptions {
                        reason: ModelRouteReason::Direct,
                        thinking_level: current_thinking,
                        signal: signal.clone(),
                        ..Default::default()
                    },
                )
                .await?;
            (route.model, route.thinking_level)
        } else {
            (selected_model.clone(), current_thinking)
        };

        if self.inner.agent.stream_fn().is_none() {
            let auth = self._get_required_request_auth(&model, signal).await?;
            return Ok(SummarizationRequestAuth {
                model: auth.model,
                api_key: auth.api_key,
                headers: auth.headers,
                env: auth.env,
                thinking_level,
            });
        }

        match self
            .inner
            .model_runtime
            .get_auth_model(
                &AnyModel::Chat(model.clone()),
                Some(ModelRuntimeAuthOverrides {
                    signal: signal.clone(),
                    ..Default::default()
                }),
            )
            .await
        {
            Ok(Some(res)) => {
                let mut request_model = model;
                if let Some(base_url) = res.auth.base_url {
                    request_model.base_url = base_url;
                }
                Ok(SummarizationRequestAuth {
                    model: request_model,
                    api_key: res.auth.api_key,
                    headers: without_deleted_headers(res.auth.headers.as_ref()),
                    env: res.env,
                    thinking_level,
                })
            }
            Ok(None) => Ok(SummarizationRequestAuth {
                model,
                api_key: None,
                headers: None,
                env: None,
                thinking_level,
            }),
            Err(err) => {
                if signal.as_ref().is_some_and(AbortSignal::aborted) {
                    return Err(err);
                }
                Ok(SummarizationRequestAuth {
                    model,
                    api_key: None,
                    headers: None,
                    env: None,
                    thinking_level,
                })
            }
        }
    }

    pub(crate) fn _model_for_message(&self, message: &AssistantMessage) -> Option<Model> {
        let model = self.model()?;
        if is_virtual_model(&model) {
            return self
                .inner
                .model_runtime
                .get_physical_model(&message.provider, &message.model);
        }
        if model.provider == message.provider && model.id == message.model {
            Some(model)
        } else {
            None
        }
    }

    pub(crate) fn _record_selection(&self) {
        let Some(model) = self.model() else {
            return;
        };
        let branch = self.inner.session_manager.get_branch(None);
        let recorded = get_branch_selection(&branch, |provider, model_id| {
            self.inner.model_runtime.get_model(provider, model_id)
        });
        let Some((recorded_provider, recorded_model_id)) = recorded else {
            return;
        };
        if recorded_provider == model.provider && recorded_model_id == model.id {
            return;
        }
        let recorded_model = self
            .inner
            .model_runtime
            .get_model(&recorded_provider, &recorded_model_id);
        if !is_virtual_model(&model) && !recorded_model.as_ref().is_some_and(is_virtual_model) {
            return;
        }
        let _ = self
            .inner
            .session_manager
            .append_model_change(&model.provider, &model.id);
    }

    pub(crate) fn _limits_model(&self) -> Option<Model> {
        self.routed_model().map(|r| r.model).or_else(|| self.model())
    }

    fn _install_agent_tool_hooks(&self) {
        let weak_before = Arc::downgrade(&self.inner);
        self.inner
            .agent
            .set_before_tool_call(Some(Arc::new(move |ctx, _signal| {
                let weak = weak_before.clone();
                Box::pin(async move {
                    let Some(inner) = weak.upgrade() else {
                        return Ok(None);
                    };
                    AgentSession { inner }._before_tool_call(ctx, None).await
                })
            })));

        let weak_after = Arc::downgrade(&self.inner);
        self.inner.agent.set_after_tool_call(Some(Arc::new(move |ctx, _signal| {
            let weak = weak_after.clone();
            Box::pin(async move {
                let Some(inner) = weak.upgrade() else {
                    return Ok(None);
                };
                AgentSession { inner }._after_tool_call(ctx, None).await
            })
        })));
    }

    pub(crate) async fn _before_tool_call(
        &self,
        context: BeforeToolCallContext,
        parent_tool_call_id: Option<&str>,
    ) -> pi_js::Result<Option<BeforeToolCallResult>> {
        let runner = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.extension_runner.clone()
        };
        let Some(runner) = runner else {
            return Ok(None);
        };
        if !runner.has_handlers("tool_call") {
            return Ok(None);
        }

        let mut event = ToolCallEvent::from_tool_name_and_input(
            context.tool_call.id,
            parent_tool_call_id.map(str::to_string),
            &context.tool_call.name,
            context.args,
        );
        let original_input = event.input_value();
        let res = runner.emit_tool_call(&mut event).await?;
        let next_input = event.input_value();
        let updated_args = (next_input != original_input).then_some(next_input);
        Ok(res
            .map(|r| BeforeToolCallResult {
                block: r.block,
                reason: r.reason,
                terminate: r.terminate,
                args: updated_args.clone(),
            })
            .or_else(|| {
                updated_args.map(|args| BeforeToolCallResult {
                    block: None,
                    reason: None,
                    terminate: None,
                    args: Some(args),
                })
            }))
    }

    pub(crate) async fn _after_tool_call(
        &self,
        context: AfterToolCallContext,
        parent_tool_call_id: Option<&str>,
    ) -> pi_js::Result<Option<AfterToolCallResult>> {
        let runner = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.extension_runner.clone()
        };
        let hook_result = match runner {
            Some(runner) if runner.has_handlers("tool_result") => {
                let input_map: IndexMap<String, Value> = context
                    .args
                    .as_object()
                    .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default();
                let ev_content: Vec<ToolResultContentBlock> = context
                    .result
                    .content
                    .iter()
                    .map(|b| match b {
                        UserContentBlock::Text(t) => ToolResultContentBlock::Text(t.clone()),
                        UserContentBlock::Image(i) => ToolResultContentBlock::Image(i.clone()),
                    })
                    .collect();
                let parent_id = parent_tool_call_id.map(str::to_string);
                let tool_call_id = context.tool_call.id.clone();
                let structured_content = context.result.structured_content.clone();
                let is_error = context.is_error;
                let usage = context.result.usage.clone();
                let event = match context.tool_call.name.as_str() {
                    "bash" => ToolResultEvent::Bash(BashToolResultEvent {
                        tool_call_id,
                        parent_tool_call_id: parent_id,
                        input: input_map,
                        content: ev_content,
                        structured_content,
                        is_error,
                        usage,
                        details: context
                            .result
                            .details
                            .as_ref()
                            .and_then(|d| serde_json::from_value::<BashToolDetails>(d.clone()).ok()),
                    }),
                    "powershell" => ToolResultEvent::PowerShell(PowerShellToolResultEvent {
                        tool_call_id,
                        parent_tool_call_id: parent_id,
                        input: input_map,
                        content: ev_content,
                        structured_content,
                        is_error,
                        usage,
                        details: context
                            .result
                            .details
                            .as_ref()
                            .and_then(|d| serde_json::from_value::<PowerShellToolDetails>(d.clone()).ok()),
                    }),
                    "read" => ToolResultEvent::Read(ReadToolResultEvent {
                        tool_call_id,
                        parent_tool_call_id: parent_id,
                        input: input_map,
                        content: ev_content,
                        structured_content,
                        is_error,
                        usage,
                        details: context
                            .result
                            .details
                            .as_ref()
                            .and_then(|d| serde_json::from_value::<ReadToolDetails>(d.clone()).ok()),
                    }),
                    "edit" => ToolResultEvent::Edit(EditToolResultEvent {
                        tool_call_id,
                        parent_tool_call_id: parent_id,
                        input: input_map,
                        content: ev_content,
                        structured_content,
                        is_error,
                        usage,
                        details: context
                            .result
                            .details
                            .as_ref()
                            .and_then(|d| serde_json::from_value::<EditToolDetails>(d.clone()).ok()),
                    }),
                    "write" => ToolResultEvent::Write(WriteToolResultEvent {
                        tool_call_id,
                        parent_tool_call_id: parent_id,
                        input: input_map,
                        content: ev_content,
                        structured_content,
                        is_error,
                        usage,
                    }),
                    "grep" => ToolResultEvent::Grep(GrepToolResultEvent {
                        tool_call_id,
                        parent_tool_call_id: parent_id,
                        input: input_map,
                        content: ev_content,
                        structured_content,
                        is_error,
                        usage,
                        details: context
                            .result
                            .details
                            .as_ref()
                            .and_then(|d| serde_json::from_value::<GrepToolDetails>(d.clone()).ok()),
                    }),
                    "find" => ToolResultEvent::Find(FindToolResultEvent {
                        tool_call_id,
                        parent_tool_call_id: parent_id,
                        input: input_map,
                        content: ev_content,
                        structured_content,
                        is_error,
                        usage,
                        details: context
                            .result
                            .details
                            .as_ref()
                            .and_then(|d| serde_json::from_value::<FindToolDetails>(d.clone()).ok()),
                    }),
                    "ls" => ToolResultEvent::Ls(LsToolResultEvent {
                        tool_call_id,
                        parent_tool_call_id: parent_id,
                        input: input_map,
                        content: ev_content,
                        structured_content,
                        is_error,
                        usage,
                        details: context
                            .result
                            .details
                            .as_ref()
                            .and_then(|d| serde_json::from_value::<LsToolDetails>(d.clone()).ok()),
                    }),
                    tool_name => ToolResultEvent::Custom(CustomToolResultEvent {
                        tool_call_id,
                        parent_tool_call_id: parent_id,
                        tool_name: tool_name.to_string(),
                        input: input_map,
                        content: ev_content,
                        structured_content,
                        is_error,
                        usage,
                        details: context.result.details.clone(),
                    }),
                };
                runner.emit_tool_result(event).await
            }
            _ => None,
        };

        let content: Vec<UserContentBlock> = hook_result
            .as_ref()
            .and_then(|r| r.content.as_ref())
            .map(|blocks| {
                blocks
                    .iter()
                    .map(|b| match b {
                        ToolResultContentBlock::Text(t) => UserContentBlock::Text(t.clone()),
                        ToolResultContentBlock::Image(i) => UserContentBlock::Image(i.clone()),
                    })
                    .collect()
            })
            .unwrap_or_else(|| context.result.content.clone());

        let resize_options = self
            ._limits_model()
            .and_then(|m| m.input_limits)
            .and_then(|il| il.images)
            .and_then(|imgs| imgs.resize);
        let normalized_content = normalize_tool_result_images(
            content.clone(),
            Some(NormalizeToolResultImagesOptions {
                auto_resize_images: Some(self.inner.settings_manager.get_image_auto_resize()),
                resize_options,
            }),
        )
        .await;

        if hook_result.is_none() && normalized_content == content {
            return Ok(None);
        }

        Ok(Some(AfterToolCallResult {
            content: Some(normalized_content),
            details: hook_result.as_ref().and_then(|r| r.details.clone()),
            structured_content: match &hook_result {
                Some(hr) => hr.structured_content.clone(),
                None => context.result.structured_content.clone(),
            },
            is_error: Some(
                hook_result
                    .as_ref()
                    .and_then(|r| r.is_error)
                    .unwrap_or(context.is_error),
            ),
            usage: hook_result.as_ref().and_then(|r| r.usage.clone()),
            terminate: None,
        }))
    }

    pub(crate) async fn _execute_nested_tool_call(
        &self,
        parent_tool_call_id: &str,
        name: &str,
        args: Value,
        options: ExecuteToolOptions,
    ) -> pi_js::Result<AgentToolCallOutcome> {
        let runner = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = guard.nested_tool_calls.clone() {
                existing
            } else {
                struct SessionNestedToolHost {
                    weak: std::sync::Weak<AgentSessionInner>,
                }

                #[async_trait]
                impl NestedToolCallHost for SessionNestedToolHost {
                    fn get_tools(&self) -> Vec<AgentTool> {
                        self.weak
                            .upgrade()
                            .map(|inner| AgentSession { inner }._get_callable_tools(None))
                            .unwrap_or_default()
                    }

                    fn is_sequential(&self) -> bool {
                        self.weak.upgrade().is_some_and(|inner| {
                            inner.agent.tool_execution() == pi_agent_core::ToolExecutionMode::Sequential
                        })
                    }

                    async fn run_tool_call(
                        &self,
                        tool_call: AgentToolCall,
                        parent_tool_call_id: &str,
                        signal: Option<AbortSignal>,
                        on_update: NestedToolUpdateAsyncCallback,
                    ) -> AgentToolCallOutcome {
                        let Some(inner) = self.weak.upgrade() else {
                            return AgentToolCallOutcome {
                                tool_call,
                                result: AgentToolResult::text("No assistant message issued this call"),
                                is_error: true,
                            };
                        };
                        let session = AgentSession { inner };
                        let Some(assistant_message) = session._find_last_assistant_message() else {
                            return AgentToolCallOutcome {
                                tool_call,
                                result: AgentToolResult::text("No assistant message issued this call"),
                                is_error: true,
                            };
                        };
                        let tools = session._get_callable_tools(None);
                        let st = session.inner.agent.state();
                        let weak_b = Arc::downgrade(&session.inner);
                        let weak_a = Arc::downgrade(&session.inner);
                        let parent_b = parent_tool_call_id.to_string();
                        let parent_a = parent_tool_call_id.to_string();
                        run_tool_call(
                            tool_call,
                            pi_agent_core::agent_loop::RunToolCallOptions {
                                tools,
                                assistant_message,
                                context: AgentContext {
                                    messages: st.messages,
                                    tools: Some(st.tools),
                                },
                                before_tool_call: Some(Arc::new(move |ctx, _sig| {
                                    let weak = weak_b.clone();
                                    let pid = parent_b.clone();
                                    Box::pin(async move {
                                        let Some(inner) = weak.upgrade() else {
                                            return Ok(None);
                                        };
                                        AgentSession { inner }._before_tool_call(ctx, Some(&pid)).await
                                    })
                                })),
                                after_tool_call: Some(Arc::new(move |ctx, _sig| {
                                    let weak = weak_a.clone();
                                    let pid = parent_a.clone();
                                    Box::pin(async move {
                                        let Some(inner) = weak.upgrade() else {
                                            return Ok(None);
                                        };
                                        AgentSession { inner }._after_tool_call(ctx, Some(&pid)).await
                                    })
                                })),
                                signal,
                                on_update: Some(Arc::new(move |partial| {
                                    let cb = on_update.clone();
                                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                                        handle.spawn(async move {
                                            cb(partial).await;
                                        });
                                    }
                                })),
                            },
                        )
                        .await
                    }

                    async fn emit(&self, event: NestedToolExecutionEvent) {
                        if let Some(inner) = self.weak.upgrade() {
                            let session = AgentSession { inner };
                            let (runner_event, session_event) = match event {
                                NestedToolExecutionEvent::ToolExecutionStart {
                                    tool_call_id,
                                    tool_name,
                                    args,
                                    parent_tool_call_id,
                                } => (
                                    RunnerEmitEvent::ToolExecutionStart(ToolExecutionStartEvent {
                                        tool_call_id: tool_call_id.clone(),
                                        tool_name: tool_name.clone(),
                                        args: args.clone(),
                                        parent_tool_call_id: Some(parent_tool_call_id.clone()),
                                    }),
                                    AgentSessionEvent::ToolExecutionStart {
                                        tool_call_id,
                                        tool_name,
                                        args,
                                        parent_tool_call_id: Some(parent_tool_call_id),
                                    },
                                ),
                                NestedToolExecutionEvent::ToolExecutionUpdate {
                                    tool_call_id,
                                    tool_name,
                                    args,
                                    partial_result,
                                    parent_tool_call_id,
                                } => (
                                    RunnerEmitEvent::ToolExecutionUpdate(ToolExecutionUpdateEvent {
                                        tool_call_id: tool_call_id.clone(),
                                        tool_name: tool_name.clone(),
                                        args: args.clone(),
                                        partial_result: serde_json::to_value(&partial_result).unwrap_or(Value::Null),
                                        parent_tool_call_id: Some(parent_tool_call_id.clone()),
                                    }),
                                    AgentSessionEvent::ToolExecutionUpdate {
                                        tool_call_id,
                                        tool_name,
                                        args,
                                        partial_result,
                                        parent_tool_call_id: Some(parent_tool_call_id),
                                    },
                                ),
                                NestedToolExecutionEvent::ToolExecutionEnd {
                                    tool_call_id,
                                    tool_name,
                                    result,
                                    is_error,
                                    parent_tool_call_id,
                                } => (
                                    RunnerEmitEvent::ToolExecutionEnd(ToolExecutionEndEvent {
                                        tool_call_id: tool_call_id.clone(),
                                        tool_name: tool_name.clone(),
                                        result: serde_json::to_value(&result).unwrap_or(Value::Null),
                                        is_error,
                                        parent_tool_call_id: Some(parent_tool_call_id.clone()),
                                    }),
                                    AgentSessionEvent::ToolExecutionEnd {
                                        tool_call_id,
                                        tool_name,
                                        result,
                                        is_error,
                                        parent_tool_call_id: Some(parent_tool_call_id),
                                    },
                                ),
                            };
                            let runner = session
                                .inner
                                .state
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .extension_runner
                                .clone();
                            if let Some(runner) = runner {
                                let _ = runner.emit(runner_event).await;
                            }
                            session._emit(session_event);
                        }
                    }
                }

                let created = Arc::new(NestedToolCallRunner::new(Arc::new(SessionNestedToolHost {
                    weak: Arc::downgrade(&self.inner),
                })));
                guard.nested_tool_calls = Some(Arc::clone(&created));
                created
            }
        };

        Ok(runner
            .execute(
                parent_tool_call_id,
                name,
                args,
                Some(NestedToolCallOptions {
                    signal: options.signal,
                    on_update: options.on_update,
                }),
            )
            .await)
    }

    pub(crate) fn _exceeds_compaction_threshold(&self, model: &Model, projection: &SessionProjection) -> bool {
        if model.context_window <= 0 {
            return false;
        }
        let branch = self.inner.session_manager.get_branch(None);
        let tokens = estimate_projected_context_tokens(projection, &branch).tokens;
        let settings = resolved_compaction_settings(&self.inner.settings_manager, Some(model));
        should_compact(tokens, model.context_window, &settings)
    }

    pub(crate) async fn _compact_before_next_assistant_response(
        &self,
        mut context: AgentContext,
    ) -> pi_js::Result<AgentContext> {
        let projection = self.inner.session_manager.build_session_projection();
        let Some(model) = self.model() else {
            context.messages = projection.messages;
            return Ok(context);
        };
        if is_virtual_model(&model) || !self._exceeds_compaction_threshold(&model, &projection) {
            context.messages = projection.messages;
            return Ok(context);
        }
        self._run_auto_compaction(CompactionReason::Threshold, false).await;
        context.messages = self.inner.session_manager.build_session_projection().messages;
        Ok(context)
    }

    fn _install_agent_request_projection(&self) {
        let previous = self.inner.agent.prepare_request();
        let weak = Arc::downgrade(&self.inner);
        self.inner
            .agent
            .set_prepare_request(Some(Arc::new(move |request, signal| {
                let prev = previous.clone();
                let weak = weak.clone();
                Box::pin(async move {
                    let Some(inner) = weak.upgrade() else {
                        return Ok(None);
                    };
                    let session = AgentSession { inner };
                    let failed = {
                        let mut guard = session.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                        guard.failed_response.take()
                    };

                    let prepare = |session: &AgentSession,
                                   req: &PrepareRequestContext,
                                   prev_fn: &Option<pi_agent_core::PrepareRequestFn>,
                                   sig: &Option<AbortSignal>| {
                        let session = session.clone();
                        let req = req.clone();
                        let prev_fn = prev_fn.clone();
                        let sig = sig.clone();
                        async move {
                            let projection = session.inner.session_manager.build_session_projection();
                            let st = session.inner.agent.state();
                            let canonical_context = AgentContext {
                                messages: projection.messages.clone(),
                                tools: Some(st.tools.clone()),
                            };
                            let prev_res = match prev_fn {
                                Some(f) => {
                                    f(
                                        PrepareRequestContext {
                                            context: canonical_context.clone(),
                                            model: st.model.clone(),
                                            thinking_level: st.thinking_level,
                                        },
                                        sig,
                                    )
                                    .await?
                                }
                                None => None,
                            };
                            let next_ctx = prev_res
                                .as_ref()
                                .and_then(|r| r.context.clone())
                                .unwrap_or(canonical_context);
                            Ok::<_, pi_js::Error>((prev_res, next_ctx, projection, req))
                        }
                    };

                    let (mut prev_res, mut context, projection, _) =
                        prepare(&session, &request, &prev, &signal).await?;
                    let st = session.inner.agent.state();
                    let model = prev_res.as_ref().and_then(|r| r.model.clone()).unwrap_or(st.model);
                    let thinking_level = prev_res
                        .as_ref()
                        .and_then(|r| r.thinking_level)
                        .map(from_agent_thinking_level)
                        .unwrap_or_else(|| session.thinking_level());

                    if !is_virtual_model(&model) {
                        return Ok(Some(PrepareRequestResult {
                            context: Some(context),
                            model: Some(model),
                            thinking_level: Some(to_agent_thinking_level(thinking_level)),
                        }));
                    }

                    let last_response = context.messages.iter().rposition(|m| m.role() == Some("assistant"));
                    let tail = match last_response {
                        Some(idx) => &context.messages[idx + 1..],
                        None => &context.messages[..],
                    };
                    let user_turn = tail.iter().any(|m| m.role() == Some("user"));
                    let branch = session.inner.session_manager.get_branch(None);
                    let state = get_virtual_model_state(&branch, &model.provider, &model.id);
                    let llm_msgs = convert_to_llm(&context.messages);
                    let route = session
                        .inner
                        .model_runtime
                        .resolve_model(
                            &model,
                            &llm_msgs,
                            ResolveModelOptions {
                                reason: if failed.is_some() {
                                    ModelRouteReason::Retry
                                } else if user_turn {
                                    ModelRouteReason::User
                                } else {
                                    ModelRouteReason::Continuation
                                },
                                thinking_level,
                                signal: signal.clone(),
                                failed,
                                state: state.clone(),
                            },
                        )
                        .await?;

                    if route.state.is_some() && route.state != state {
                        let data = VirtualModelStateData {
                            provider: model.provider.clone(),
                            model_id: model.id.clone(),
                            state: route.state.clone().unwrap_or(Value::Null),
                        };
                        let val = pi_js::json::parse(&pi_js::json::stringify(&data))?;
                        if let Ok(entry_id) = session
                            .inner
                            .session_manager
                            .append_custom_entry(VIRTUAL_MODEL_STATE_ENTRY, Some(val))
                            && let Some(entry) = session.inner.session_manager.get_entry(&entry_id)
                        {
                            session._emit(AgentSessionEvent::EntryAppended { entry });
                        }
                    }

                    if session._exceeds_compaction_threshold(&route.model, &projection) {
                        session._run_auto_compaction(CompactionReason::Threshold, false).await;
                        let res = prepare(&session, &request, &prev, &signal).await?;
                        prev_res = res.0;
                        context = res.1;
                    }
                    let _ = prev_res;

                    Ok(Some(PrepareRequestResult {
                        context: Some(context),
                        model: Some(route.model),
                        thinking_level: Some(to_agent_thinking_level(route.thinking_level)),
                    }))
                })
            })));
    }

    pub(crate) async fn _dispatch_turn_end_boundary(
        &self,
        message: &AssistantMessage,
        tool_results: &[ToolResultMessage],
    ) -> bool {
        let outcome = match message.stop_reason {
            pi_ai::StopReason::Aborted => AgentActivityOutcome::Aborted,
            pi_ai::StopReason::Error => AgentActivityOutcome::Error,
            _ => AgentActivityOutcome::Completed,
        };
        let (runner, turn_index) = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.last_activity_outcome = outcome;
            (guard.extension_runner.clone(), guard.turn_index)
        };

        let Some(runner) = runner else {
            return false;
        };
        if !runner.has_handlers("turn_end") {
            return false;
        }
        let assistant_agent_msg = AgentMessage::from(message.clone());
        let Some(message_entry_id) = self._find_persisted_message_entry_id(&assistant_agent_msg) else {
            runner.emit_error(ExtensionError {
                extension_path: "<boundary>".to_string(),
                event: "turn_end".to_string(),
                error: "turn_end could not resolve the persisted assistant entry ID".to_string(),
                stack: None,
            });
            return false;
        };
        let tool_result_entry_ids: Vec<String> = tool_results
            .iter()
            .filter_map(|tr| self._find_persisted_message_entry_id(&AgentMessage::from(tr.clone())))
            .collect();

        let weak_preview = Arc::downgrade(&self.inner);
        let boundary = runner
            .emit_boundary(
                BoundaryBaseEvent::TurnEnd {
                    turn_index: turn_index as i64,
                    message: AgentMessage::from(message.clone()),
                    tool_results: tool_results.to_vec(),
                    message_entry_id,
                    tool_result_entry_ids,
                    outcome,
                },
                move |entries: &[SessionBoundaryDraft]| {
                    let entries = entries.to_vec();
                    let weak_preview = weak_preview.clone();
                    Box::pin(async move {
                        Ok(weak_preview
                            .upgrade()
                            .map(|inner| {
                                AgentSession { inner }._build_boundary_context(&entries, BoundaryKind::TurnEnd)
                            })
                            .unwrap_or_default())
                    })
                },
            )
            .await;

        self._await_pending_extension_sends().await;
        self._commit_boundary_drafts(&boundary.entries);
        if boundary.r#continue && !self._build_boundary_context(&[], BoundaryKind::TurnEnd).can_continue {
            self._report_invalid_boundary_continuation("turn_end");
            return false;
        }
        boundary.r#continue
    }

    fn _install_agent_boundary_hooks(&self) {
        let previous = self.inner.agent.finish_turn();
        let weak = Arc::downgrade(&self.inner);
        self.inner
            .agent
            .set_finish_turn(Some(Arc::new(move |mut turn: FinishTurnContext, signal| {
                let prev = previous.clone();
                let weak = weak.clone();
                Box::pin(async move {
                    let Some(inner) = weak.upgrade() else {
                        return Ok(None);
                    };
                    let session = AgentSession { inner };
                    let resolved_msg = session._resolve_message_replacement(AgentMessage::from(turn.message.clone()));
                    if let Some(resolved_assistant) = resolved_msg.as_assistant() {
                        turn.message = resolved_assistant.clone();
                    }
                    {
                        let mut guard = session.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                        guard.boundary_dispatched_messages.push(turn.message.clone());
                    }
                    let ext_continue = session
                        ._dispatch_turn_end_boundary(&turn.message, &turn.tool_results)
                        .await;
                    let prev_decision = match prev {
                        Some(f) => f(turn, signal).await?,
                        None => None,
                    };
                    if let Some(d) = prev_decision
                        && d.action == FinishTurnAction::End
                    {
                        return Ok(Some(d));
                    }
                    if ext_continue || prev_decision.is_some_and(|d| d.action == FinishTurnAction::Continue) {
                        return Ok(Some(FinishTurnDecision {
                            action: FinishTurnAction::Continue,
                        }));
                    }
                    Ok(None)
                })
            })));
    }

    fn _install_agent_next_turn_refresh(&self) {
        let prev_with_ctx = self.inner.agent.prepare_next_turn_with_context();
        let prev_no_ctx = self.inner.agent.prepare_next_turn();
        let weak = Arc::downgrade(&self.inner);
        self.inner.agent.set_prepare_next_turn_with_context(Some(Arc::new(
            move |turn: PrepareNextTurnContext, signal| {
                let prev_with = prev_with_ctx.clone();
                let prev_no = prev_no_ctx.clone();
                let weak = weak.clone();
                Box::pin(async move {
                    let Some(inner) = weak.upgrade() else {
                        return Ok(None);
                    };
                    let session = AgentSession { inner };
                    let context = session
                        ._compact_before_next_assistant_response(turn.context.clone())
                        .await?;
                    let prev_snapshot = if let Some(f) = prev_with {
                        f(
                            PrepareNextTurnContext {
                                context: context.clone(),
                            },
                            signal,
                        )
                        .await?
                    } else if let Some(f) = prev_no {
                        f(signal).await?
                    } else {
                        None
                    };

                    let next_context = prev_snapshot
                        .as_ref()
                        .and_then(|s| s.context.clone())
                        .unwrap_or(context);
                    let (mut run_options, base_options) = {
                        let guard = session.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                        (
                            guard
                                .run_system_prompt_options
                                .clone()
                                .unwrap_or_else(|| guard.base_system_prompt_options.clone()),
                            guard.base_system_prompt_options.clone(),
                        )
                    };
                    run_options.selected_tools = session.get_active_tool_names();
                    let mut merged_snippets = base_options.tool_snippets;
                    for (k, v) in run_options.tool_snippets {
                        merged_snippets.insert(k, v);
                    }
                    run_options.tool_snippets = merged_snippets;
                    let mut merged_guidelines = base_options.tool_guidelines;
                    for (k, v) in run_options.tool_guidelines {
                        merged_guidelines.insert(k, v);
                    }
                    run_options.tool_guidelines = merged_guidelines;

                    let update_message =
                        session._prepare_prompt_and_tool_loadout(&mut run_options, Some(&next_context.messages));
                    {
                        let mut guard = session.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                        guard.run_system_prompt_options = Some(run_options);
                    }

                    let mut messages_out = prev_snapshot.as_ref().and_then(|s| s.messages.clone());
                    if let Some(upd) = update_message {
                        messages_out.get_or_insert_with(Vec::new).push(AgentMessage::from(upd));
                    }

                    let st = session.inner.agent.state();
                    Ok(Some(AgentLoopTurnUpdate {
                        context: Some(AgentContext {
                            messages: next_context.messages,
                            tools: Some(st.tools),
                        }),
                        messages: messages_out,
                        model: Some(st.model),
                        thinking_level: Some(st.thinking_level),
                        reasoning: None,
                    }))
                })
            },
        )));
    }

    // =========================================================================
    // Event Subscription
    // =========================================================================

    pub(crate) fn _refresh_finalized_context(&self) {
        let projection = self.inner.session_manager.build_session_projection();
        {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            for entry in &projection.entries {
                for message in &entry.messages {
                    guard
                        .entry_ids_by_message
                        .push((message.clone(), entry.source_entry.id().to_string()));
                }
            }
        }
        self.inner.agent.replace_messages(projection.messages);
    }

    pub(crate) fn _apply_boundary_drafts(
        &self,
        manager: &SessionManager,
        drafts: &[SessionBoundaryDraft],
    ) -> Vec<SessionEntry> {
        let mut appended = Vec::new();
        for draft in drafts {
            let entry_id = match draft {
                SessionBoundaryDraft::Custom { custom_type, data } => {
                    manager.append_custom_entry(custom_type, data.clone()).ok()
                }
                SessionBoundaryDraft::CustomMessage {
                    custom_type,
                    content,
                    display,
                    details,
                } => manager
                    .append_custom_message_entry(custom_type, content.clone(), *display, details.clone())
                    .ok(),
                SessionBoundaryDraft::ContextEdit { target_id, replacement } => {
                    manager.append_context_edit(target_id, replacement.clone()).ok()
                }
                SessionBoundaryDraft::Compaction {
                    summary,
                    first_kept_entry_id,
                    details,
                    usage,
                } => {
                    let projection = manager.build_session_projection();
                    let branch = manager.get_branch(None);
                    let tokens_before = estimate_projected_context_tokens(&projection, &branch).tokens;
                    manager
                        .append_compaction(
                            summary,
                            first_kept_entry_id.as_deref(),
                            tokens_before,
                            details.clone(),
                            Some(true),
                            usage.clone(),
                        )
                        .ok()
                }
            };
            if let Some(id) = entry_id
                && let Some(entry) = manager.get_entry(&id)
            {
                appended.push(entry);
            }
        }
        appended
    }

    fn _create_boundary_preview_manager(&self, drafts: &[SessionBoundaryDraft]) -> pi_js::Result<SessionManager> {
        let header = self
            .inner
            .session_manager
            .get_header()
            .ok_or_else(|| pi_js::Error::msg("Session header is missing"))?;
        let mut initial = vec![FileEntry::Session(header)];
        initial.extend(
            self.inner
                .session_manager
                .get_branch(None)
                .into_iter()
                .map(FileEntry::from_session_entry),
        );
        let manager = SessionManager::in_memory(Some(&self.inner.session_manager.get_cwd()), None, Some(initial))?;
        self._apply_boundary_drafts(&manager, drafts);
        Ok(manager)
    }

    fn _get_pending_boundary_messages(&self) -> Vec<AgentMessage> {
        let mut pending = self.inner.agent.peek_queued_messages();
        let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        pending.extend(guard.pending_custom_messages.iter().cloned());
        pending
    }

    pub(crate) fn _build_boundary_context(
        &self,
        drafts: &[SessionBoundaryDraft],
        boundary: BoundaryKind,
    ) -> BoundaryContextPreview {
        let Ok(preview_mgr) = self._create_boundary_preview_manager(drafts) else {
            return BoundaryContextPreview::default();
        };
        let projection = preview_mgr.build_session_projection();
        let pending_messages = self._get_pending_boundary_messages();
        let llm_messages = convert_to_llm(&projection.messages);
        let final_role = llm_messages.last().map(pi_ai::types::Message::role);
        let has_non_system_context = llm_messages.iter().any(|m| m.role() != "system");
        let context_can_continue = has_non_system_context && final_role != Some("assistant");
        let pending_custom_context = !self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending_custom_messages
            .is_empty();
        let queued_can_continue = match boundary {
            BoundaryKind::TurnEnd => self.inner.agent.has_queued_messages(),
            BoundaryKind::AgentBeforeSettle => {
                final_role == Some("assistant") && self.inner.agent.has_queued_messages()
            }
        };
        let can_continue = context_can_continue || pending_custom_context || queued_can_continue;

        BoundaryContextPreview {
            context_entries: projection.entries,
            context_messages: projection.messages,
            llm_messages,
            pending_messages,
            can_continue,
        }
    }

    pub(crate) fn _commit_boundary_drafts(&self, drafts: &[SessionBoundaryDraft]) {
        if drafts.is_empty() {
            return;
        }
        let appended = self._apply_boundary_drafts(&self.inner.session_manager, drafts);
        self._refresh_finalized_context();
        for entry in appended {
            self._emit(AgentSessionEvent::EntryAppended { entry });
        }
    }

    pub(crate) fn _report_invalid_boundary_continuation(&self, event: &str) {
        let runner = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extension_runner
            .clone();
        if let Some(runner) = runner {
            runner.emit_error(ExtensionError {
                extension_path: "<boundary>".to_string(),
                event: event.to_string(),
                error: format!("{event} requested continuation without runnable model context"),
                stack: None,
            });
        }
    }

    /// Emit an event to all listeners.
    pub(crate) fn _emit(&self, event: AgentSessionEvent) {
        let listeners: Vec<AgentSessionEventListener> = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.event_listeners.iter().map(|(_, l)| Arc::clone(l)).collect()
        };
        for listener in listeners {
            listener(event.clone());
        }
    }

    pub(crate) fn _emit_queue_update(&self) {
        let (steering, follow_up) = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (guard.steering_messages.clone(), guard.follow_up_messages.clone())
        };
        self._emit(AgentSessionEvent::QueueUpdate { steering, follow_up });
    }

    pub(crate) async fn _emit_session_compact_failed(
        &self,
        reason: CompactionReason,
        error_message: Option<String>,
        aborted: bool,
        will_retry: bool,
        from_extension: bool,
    ) {
        let runner = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.extension_runner.clone()
        };
        if let Some(runner) = runner
            && runner.has_handlers("session_compact_failed")
        {
            let _ = runner
                .emit(SessionCompactFailedEvent {
                    reason: to_ext_compaction_reason(reason),
                    error_message,
                    aborted,
                    will_retry,
                    from_extension,
                })
                .await;
        }
    }

    pub(crate) fn _resolve_idle_wait_if_idle(&self) {
        if !self.is_idle() {
            return;
        }
        let waiters = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_agent_run_active
                || guard.compaction_abort_controller.is_some()
                || guard.auto_compaction_abort_controller.is_some()
                || guard.branch_summary_abort_controller.is_some()
            {
                return;
            }
            std::mem::take(&mut guard.idle_waiters)
        };
        for waiter in waiters {
            let _ = waiter.send(());
        }
    }

    pub(crate) async fn _emit_agent_settled(&self) {
        if let Some(ref warmer) = self.inner.cache_warmer {
            warmer.on_agent_settled();
        }
        let runner = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.is_agent_run_active = false;
            guard.is_emitting_agent_settled = true;
            guard.extension_runner.clone()
        };

        if let Some(runner) = runner {
            let _ = runner.emit(AgentSettledEvent {}).await;
        }
        self._emit(AgentSessionEvent::AgentSettled);

        let deferred = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.is_emitting_agent_settled = false;
            std::mem::take(&mut guard.deferred_settled_actions)
        };

        for action in deferred {
            action().await;
        }
        self._resolve_idle_wait_if_idle();
    }

    pub(crate) async fn _await_pending_extension_sends(&self) {
        loop {
            let handles = {
                let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                if guard.pending_extension_send_tasks.is_empty() {
                    return;
                }
                std::mem::take(&mut guard.pending_extension_send_tasks)
            };
            for handle in handles {
                let _ = handle.await;
            }
        }
    }

    pub(crate) fn _resolve_message_replacement(&self, mut message: AgentMessage) -> AgentMessage {
        let replacements = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .message_end_replacements
            .clone();
        if replacements.is_empty() {
            return message;
        }
        for (old, new) in &replacements {
            if &message == old {
                message = new.clone();
            }
        }
        message
    }

    pub(crate) async fn _handle_agent_event(&self, event: AgentEvent) {
        let mut event = event;
        match &mut event {
            AgentEvent::AgentStart => {
                self.inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .message_end_replacements
                    .clear();
            }
            AgentEvent::TurnEnd { message, .. } => {
                *message = self._resolve_message_replacement(message.clone());
            }
            AgentEvent::AgentEnd { messages } => {
                for msg in messages.iter_mut() {
                    *msg = self._resolve_message_replacement(msg.clone());
                }
            }
            _ => {}
        }

        let nested = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.nested_tool_calls.clone()
        };
        if let Some(nested) = nested {
            let apply_nested_summary = |tr: &mut ToolResultMessage, summary: &NestedCallSummary| {
                if let Some(ref calls) = summary.calls {
                    tr.nested_calls = Some(calls.clone());
                }
                if let Some(ref usage) = summary.usage {
                    tr.usage = Some(match tr.usage {
                        Some(ref existing) => combine_usage(existing, usage),
                        None => usage.clone(),
                    });
                }
            };

            match &mut event {
                AgentEvent::MessageStart { message } => {
                    if let Some(mut tr) = message.as_tool_result().cloned()
                        && let Some(summary) = nested.take_record(&tr.tool_call_id)
                    {
                        apply_nested_summary(&mut tr, &summary);
                        self.inner
                            .state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .completed_nested_call_summaries
                            .insert(tr.tool_call_id.clone(), summary);
                        *message = AgentMessage::from(tr);
                    }
                }
                AgentEvent::MessageEnd { message } => {
                    if let Some(mut tr) = message.as_tool_result().cloned() {
                        let summary = {
                            self.inner
                                .state
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .completed_nested_call_summaries
                                .get(&tr.tool_call_id)
                                .cloned()
                        }
                        .or_else(|| nested.take_record(&tr.tool_call_id));
                        if let Some(summary) = summary {
                            apply_nested_summary(&mut tr, &summary);
                            let updated_msg = AgentMessage::from(tr.clone());
                            *message = updated_msg.clone();
                            let mut agent_msgs = self.inner.agent.state().messages;
                            if let Some(slot) = agent_msgs.iter_mut().rev().find(|m| {
                                m.as_tool_result()
                                    .is_some_and(|existing| existing.tool_call_id == tr.tool_call_id)
                            }) {
                                *slot = updated_msg;
                                self.inner.agent.replace_messages(agent_msgs);
                            }
                        }
                    }
                }
                AgentEvent::TurnEnd { tool_results, .. } => {
                    let summaries = self
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .completed_nested_call_summaries
                        .clone();
                    if !summaries.is_empty() {
                        for tr in tool_results.iter_mut() {
                            if let Some(summary) = summaries.get(&tr.tool_call_id) {
                                apply_nested_summary(tr, summary);
                            }
                        }
                    }
                }
                AgentEvent::AgentEnd { messages } => {
                    let summaries = {
                        let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                        std::mem::take(&mut guard.completed_nested_call_summaries)
                    };
                    if !summaries.is_empty() {
                        for msg in messages.iter_mut() {
                            if let Some(mut tr) = msg.as_tool_result().cloned()
                                && let Some(summary) = summaries.get(&tr.tool_call_id)
                            {
                                apply_nested_summary(&mut tr, summary);
                                *msg = AgentMessage::from(tr);
                            }
                        }
                    }
                    nested.clear();
                }
                _ => {}
            }
        }

        if let AgentEvent::MessageStart { ref message } = event
            && let Some(user_msg) = message.as_user()
        {
            let message_text = match &user_msg.content {
                pi_ai::UserMessageContent::Text(s) => s.clone(),
                pi_ai::UserMessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| match b {
                        pi_ai::UserContentBlock::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            };
            let mut queue_changed = false;
            {
                let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.overflow_recovery_attempted = false;
                if !message_text.is_empty() {
                    if let Some(idx) = guard.steering_messages.iter().position(|s| s == &message_text) {
                        guard.steering_messages.remove(idx);
                        queue_changed = true;
                    } else if let Some(idx) = guard.follow_up_messages.iter().position(|s| s == &message_text) {
                        guard.follow_up_messages.remove(idx);
                        queue_changed = true;
                    }
                }
            }
            if queue_changed {
                self._emit_queue_update();
            }
        }

        self._emit_extension_event(&mut event).await;

        let session_event = match &event {
            AgentEvent::AgentStart => AgentSessionEvent::AgentStart,
            AgentEvent::AgentEnd { messages } => AgentSessionEvent::AgentEnd {
                messages: messages.clone(),
                will_retry: self._will_retry_after_agent_end(messages),
            },
            AgentEvent::TurnStart => AgentSessionEvent::TurnStart,
            AgentEvent::TurnEnd { message, tool_results } => AgentSessionEvent::TurnEnd {
                message: message.clone(),
                tool_results: tool_results.clone(),
            },
            AgentEvent::MessageStart { message } => AgentSessionEvent::MessageStart {
                message: message.clone(),
            },
            AgentEvent::MessageUpdate {
                message,
                assistant_message_event,
            } => AgentSessionEvent::MessageUpdate {
                message: message.clone(),
                assistant_message_event: assistant_message_event.clone(),
            },
            AgentEvent::MessageEnd { message } => AgentSessionEvent::MessageEnd {
                message: message.clone(),
            },
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => AgentSessionEvent::ToolExecutionStart {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                args: args.clone(),
                parent_tool_call_id: None,
            },
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                args,
                partial_result,
            } => AgentSessionEvent::ToolExecutionUpdate {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                args: args.clone(),
                partial_result: partial_result.clone(),
                parent_tool_call_id: None,
            },
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
            } => AgentSessionEvent::ToolExecutionEnd {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                result: result.clone(),
                is_error: *is_error,
                parent_tool_call_id: None,
            },
        };
        self._emit(session_event);

        if let AgentEvent::MessageEnd { ref message } = event {
            let entry_id = if message.role() == Some("custom")
                && let Some(val) = message.as_custom()
                && let Ok(cm) =
                    pi_js::json::parse_as::<crate::core::messages::CustomMessage>(&pi_js::json::stringify(val))
            {
                self.inner
                    .session_manager
                    .append_custom_message_entry(&cm.custom_type, cm.content, cm.display, cm.details)
                    .ok()
            } else if matches!(message.role(), Some("system" | "user" | "assistant" | "toolResult")) {
                self.inner.session_manager.append_message(message.clone()).ok()
            } else {
                None
            };

            if let Some(id) = entry_id {
                self.inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .entry_ids_by_message
                    .push((message.clone(), id));
            }

            if let Some(assistant_msg) = message.as_assistant().cloned() {
                let mut emit_retry_success: Option<u32> = None;
                {
                    let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    guard.last_assistant_message = Some(assistant_msg.clone());
                    if assistant_msg.stop_reason != pi_ai::StopReason::Error
                        && assistant_msg.stop_reason != pi_ai::StopReason::Length
                    {
                        guard.overflow_recovery_attempted = false;
                    }
                    if assistant_msg.stop_reason != pi_ai::StopReason::Error && guard.retry_attempt > 0 {
                        emit_retry_success = Some(guard.retry_attempt);
                        guard.retry_attempt = 0;
                    }
                }
                if let Some(attempt) = emit_retry_success {
                    self._emit(AgentSessionEvent::AutoRetryEnd {
                        success: true,
                        attempt,
                        final_error: None,
                    });
                }
            }
        }

        if let AgentEvent::TurnEnd { tool_results, .. } = event {
            {
                let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.last_assistant_tool_results = tool_results;
            }
            self._flush_pending_custom_messages();
        }
    }

    pub(crate) fn _will_retry_after_agent_end(&self, messages: &[AgentMessage]) -> bool {
        let (abort_requested, retry_attempt) = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (guard.agent_run_abort_requested, guard.retry_attempt)
        };
        if abort_requested {
            return false;
        }
        let settings = self.inner.settings_manager.get_retry_settings();
        if !settings.enabled || i64::from(retry_attempt) >= settings.max_retries {
            return false;
        }
        for message in messages.iter().rev() {
            if let Some(assistant) = message.as_assistant() {
                return self._is_retryable_error(assistant);
            }
        }
        false
    }

    pub(crate) fn _find_persisted_message_entry_id(&self, message: &AgentMessage) -> Option<String> {
        {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, id)) = guard.entry_ids_by_message.iter().rev().find(|(m, _)| m == message) {
                return Some(id.clone());
            }
        }

        let branch = self.inner.session_manager.get_branch(None);
        for entry in branch.iter().rev() {
            if let SessionEntry::Message(msg_entry) = entry
                && &msg_entry.message == message
            {
                return Some(msg_entry.id.clone());
            }
        }

        let agent_messages = self.inner.agent.state().messages;
        let message_index = agent_messages.iter().position(|m| m == message)?;
        let projection = self.inner.session_manager.build_session_projection();
        let mut projected_index = 0usize;
        for entry in &projection.entries {
            for _ in &entry.messages {
                if projected_index == message_index {
                    let id = entry.source_entry.id().to_string();
                    self.inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .entry_ids_by_message
                        .push((message.clone(), id.clone()));
                    return Some(id);
                }
                projected_index += 1;
            }
        }
        None
    }

    pub(crate) fn _omit_recovery_attempt(
        &self,
        message: &AssistantMessage,
        tool_results: &[AgentMessage],
    ) -> pi_js::Result<()> {
        let mut targets: Vec<AgentMessage> = Vec::with_capacity(1 + tool_results.len());
        targets.push(AgentMessage::from(message.clone()));
        targets.extend(tool_results.iter().cloned());

        let target_ids: Vec<Option<String>> = targets
            .iter()
            .map(|target| self._find_persisted_message_entry_id(target))
            .collect();
        let agent_messages = self.inner.agent.state().messages;
        let unresolved_projected_target = targets
            .iter()
            .enumerate()
            .any(|(idx, target)| target_ids[idx].is_none() && agent_messages.iter().any(|m| m == target));
        if unresolved_projected_target {
            return Err(pi_js::Error::msg(
                "Cannot persist recovery omission because a projected message has no source entry",
            ));
        }
        for target_id in target_ids.into_iter().flatten() {
            let edit_id = self.inner.session_manager.append_context_edit(&target_id, None)?;
            if let Some(entry) = self.inner.session_manager.get_entry(&edit_id) {
                self._emit(AgentSessionEvent::EntryAppended { entry });
            }
        }
        self._refresh_finalized_context();
        Ok(())
    }

    pub(crate) fn _find_last_assistant_message(&self) -> Option<AssistantMessage> {
        let messages = self.inner.agent.state().messages;
        messages.iter().rev().find_map(|m| m.as_assistant().cloned())
    }

    fn _replace_message_in_place(&self, target: &mut AgentMessage, replacement: AgentMessage) {
        let mut current_msgs = self.inner.agent.state().messages;
        if let Some(idx) = current_msgs.iter().rposition(|m| m == target) {
            current_msgs[idx] = replacement.clone();
            self.inner.agent.replace_messages(current_msgs);
        }
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .message_end_replacements
            .push((target.clone(), replacement.clone()));
        *target = replacement;
    }

    async fn _emit_extension_event(&self, event: &mut AgentEvent) {
        let runner = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.extension_runner.clone()
        };
        let Some(runner) = runner else {
            return;
        };

        match event {
            AgentEvent::AgentStart => {
                self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).turn_index = 0;
                let _ = runner.emit(AgentStartEvent {}).await;
            }
            AgentEvent::AgentEnd { messages } => {
                let _ = runner
                    .emit(AgentEndEvent {
                        messages: messages.clone(),
                    })
                    .await;
                self._await_pending_extension_sends().await;
            }
            AgentEvent::TurnStart => {
                let turn_index = self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).turn_index;
                let _ = runner
                    .emit(TurnStartEvent {
                        turn_index: turn_index as i64,
                        timestamp: pi_js::time::now_ms(),
                    })
                    .await;
            }
            AgentEvent::TurnEnd { message, tool_results } => {
                let already_dispatched = if let Some(assistant) = message.as_assistant() {
                    let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(pos) = guard.boundary_dispatched_messages.iter().rposition(|m| m == assistant) {
                        guard.boundary_dispatched_messages.remove(pos);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if !already_dispatched && let Some(assistant) = message.as_assistant() {
                    self._dispatch_turn_end_boundary(assistant, tool_results).await;
                }
                self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).turn_index += 1;
            }
            AgentEvent::MessageStart { message } => {
                let _ = runner
                    .emit(MessageStartEvent {
                        message: message.clone(),
                    })
                    .await;
            }
            AgentEvent::MessageUpdate {
                message,
                assistant_message_event,
            } => {
                let _ = runner
                    .emit(MessageUpdateEvent {
                        message: message.clone(),
                        assistant_message_event: assistant_message_event.clone(),
                    })
                    .await;
            }
            AgentEvent::MessageEnd { message } => {
                if let Some(replacement) = runner
                    .emit_message_end(MessageEndEvent {
                        message: message.clone(),
                    })
                    .await
                {
                    let normalized = crate::core::session_manager::repair_null_content_agent_message(&replacement);
                    self._replace_message_in_place(message, normalized);
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                let _ = runner
                    .emit(ToolExecutionStartEvent {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        args: args.clone(),
                        parent_tool_call_id: None,
                    })
                    .await;
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                args,
                partial_result,
            } => {
                let _ = runner
                    .emit(ToolExecutionUpdateEvent {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        args: args.clone(),
                        partial_result: serde_json::to_value(partial_result).unwrap_or(Value::Null),
                        parent_tool_call_id: None,
                    })
                    .await;
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
            } => {
                let _ = runner
                    .emit(ToolExecutionEndEvent {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        result: serde_json::to_value(result).unwrap_or(Value::Null),
                        is_error: *is_error,
                        parent_tool_call_id: None,
                    })
                    .await;
            }
        }
    }

    /// Subscribe to agent session events.
    pub fn subscribe(&self, listener: impl Fn(AgentSessionEvent) + Send + Sync + 'static) -> Unsubscribe {
        let id = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let id = guard.next_listener_id;
            guard.next_listener_id += 1;
            guard.event_listeners.push((id, Arc::new(listener)));
            id
        };
        let weak = Arc::downgrade(&self.inner);
        Unsubscribe::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .event_listeners
                    .retain(|(entry_id, _)| *entry_id != id);
            }
        })
    }

    fn _disconnect_from_agent(&self) {
        let unsub = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .unsubscribe_agent
            .take();
        if let Some(u) = unsub {
            u.call();
        }
    }

    /// Remove all listeners and disconnect from agent.
    pub fn dispose(&self) {
        self.abort_retry();
        self.abort_compaction();
        self.abort_branch_summary();
        self.abort_bash();
        self.inner.agent.abort();

        let runner = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.event_listeners.clear();
            guard.extension_runner.clone()
        };
        if let Some(runner) = runner {
            runner.invalidate(Some(
                "This extension ctx is stale after session replacement or reload. Do not use a captured pi or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload().",
            ));
        }
        self._disconnect_from_agent();
        if let Some(ref warmer) = self.inner.cache_warmer {
            warmer.set_on_warmed(None);
            warmer.cancel();
        }
        pi_ai::cleanup_session_resources(Some(&self.session_id()));
    }
}
