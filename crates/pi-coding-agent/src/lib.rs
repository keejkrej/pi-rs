//! Port of packages/coding-agent/src/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod cli;
pub mod client;
pub mod config;
pub mod core;
pub mod experimental;
pub mod extensions;
pub mod main_;
pub mod migrations;
pub mod modes;
pub mod package_manager_cli;
pub mod rpc_entry;
pub mod utils;
pub mod vendor;
// @generated-mods end

pub use pi_js::{Error, Result};

// Core session management
pub use crate::cli::args::{Args, parse_args};
pub use crate::config::{
    CONFIG_DIR_NAME, VERSION, get_agent_dir, get_docs_path, get_examples_path, get_package_dir, get_readme_path,
};
pub use crate::core::agent_session::{
    AgentSession, AgentSessionConfig, AgentSessionEvent, AgentSessionEventListener, ModelCycleResult, ParsedSkillBlock,
    PromptOptions, SessionStats, parse_skill_block,
};
pub use crate::core::auth_storage::{AuthStorage, ReadOnlyAuthStorage, read_stored_credential};
pub use crate::core::cache_warmer::{CacheWarmingDecision, CacheWarmingStatus};
pub use crate::core::compaction::branch_summarization::{
    BranchPreparation, BranchSummaryResult, CollectEntriesResult, GenerateBranchSummaryOptions,
    collect_entries_for_branch_summary, generate_branch_summary, prepare_branch_entries,
};
pub use crate::core::compaction::compaction::{
    CompactionResult, DEFAULT_COMPACTION_SETTINGS, calculate_context_tokens, compact, estimate_tokens, find_cut_point,
    find_turn_start_index, generate_summary, generate_summary_with_usage, get_last_assistant_usage, should_compact,
};
pub use crate::core::compaction::utils::{FileOperations, serialize_conversation};
pub use crate::core::event_bus::{EventBus, EventBusController, create_event_bus};
pub use crate::core::extensions::types::{
    AfterProviderResponseEvent, AgentActivityOutcome, AgentBeforeSettleEvent, AgentBeforeSettleEventResult,
    AgentEndEvent, AgentSettledEvent, AgentStartEvent, AgentToolResult, AgentToolUpdateCallback, AppKeybinding,
    AutocompleteProviderFactory, BeforeAgentStartEvent, BeforeAgentStartEventResult, BeforeProviderHeadersEvent,
    BeforeProviderRequestEvent, BeforeProviderRequestEventResult, BoundaryContextPreview, BoundaryResult,
    BoundaryState, BuildSystemPromptOptions, CacheWarmingDecisionEvent, CacheWarmingDecisionEventResult,
    CompactOptions, ContextEvent, ContextEventResult, ContextUsage, ContextWithSystemEvent, EntryRenderOptions,
    EntryRenderer, ExecOptions, ExecResult, ExecuteToolOptions, Extension, ExtensionAPI, ExtensionActions,
    ExtensionCommandContext, ExtensionCommandContextActions, ExtensionContext, ExtensionContextActions, ExtensionError,
    ExtensionEvent, ExtensionFactory, ExtensionFlag, ExtensionRuntime, ExtensionShortcut, ExtensionToolContext,
    ExtensionUIContext, ExtensionUIDialogOptions, ExtensionVirtualModel, ExtensionWidgetOptions, InlineExtension,
    InputEvent, InputEventResult, InputSource, KeybindingsManager, LoadExtensionsResult, MarkdownTransformContext,
    MarkdownTransformer, McpServersChangeEvent, MessageEndEvent, MessageEndEventResult, MessageRenderOptions,
    MessageRenderer, MessageStartEvent, MessageUpdateEvent, ModelSelectEvent, ModelSelectSource,
    NormalizedBuildSystemPromptOptions, ProjectTrustContext, ProjectTrustEvent, ProjectTrustEventDecision,
    ProjectTrustEventResult, ProjectTrustHandler, ProviderConfig, ProviderModelConfig, ProviderStreamEvent,
    RegisteredCommand, RegisteredTool, ResolvedCommand, ResourcesDiscoverEvent, ResourcesDiscoverResult,
    SessionBeforeCompactEvent, SessionBeforeCompactResult, SessionBeforeForkEvent, SessionBeforeForkResult,
    SessionBeforeSwitchEvent, SessionBeforeSwitchResult, SessionBeforeTreeEvent, SessionBeforeTreeResult,
    SessionBoundaryDraft, SessionCompactEvent, SessionCompactFailedEvent, SessionInfoChangedEvent,
    SessionShutdownEvent, SessionStartEvent, SessionTreeEvent, SlashCommandInfo, SlashCommandSource, SourceInfo,
    TerminalInputHandler, ThinkingLevelSelectEvent, ToolAnnotations, ToolCallEvent, ToolCallEventResult,
    ToolDefinition, ToolExecutionEndEvent, ToolExecutionMode, ToolExecutionStartEvent, ToolExecutionUpdateEvent,
    ToolExposure, ToolInfo, ToolLoadoutChanges, ToolNamespace, ToolRenderResultOptions, ToolResultEvent,
    ToolResultEventResult, TurnEndEvent, TurnEndEventResult, TurnStartEvent, UserBashEvent, UserBashEventResult,
    UserBashOperations,
};
pub use crate::core::model_registry::ModelRegistry;
pub use crate::core::model_runtime::{
    CreateModelRuntimeOptions, ModelRuntime, ModelRuntimeAuthOverrides, ModelRuntimeCreateOptions,
};
pub use crate::core::resource_loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceDiagnostic, ResourceLoader,
};
pub use crate::core::sdk::{
    AgentSessionRuntime, AgentSessionRuntimeDiagnostic, AgentSessionRuntimeResult, AgentSessionServices,
    CreateAgentSessionFromServicesOptions, CreateAgentSessionOptions, CreateAgentSessionResult,
    CreateAgentSessionRuntimeFactory, CreateAgentSessionRuntimeOptions, CreateAgentSessionServicesOptions, NoToolsMode,
    PromptTemplate, ScopedModelEntry, Skill, create_agent_session, create_agent_session_from_services,
    create_agent_session_runtime, create_agent_session_services, ensure_default_stream_fn,
};
pub use crate::core::session_manager::{
    BranchSummaryEntry, CompactionEntry, CustomEntry, CustomMessageEntry, ModelChangeEntry, SessionContext,
    SessionEntry, SessionHeader, SessionInfo, SessionInfoEntry, SessionManager, SessionMessageEntry,
    ThinkingLevelChangeEntry, parse_session_entries,
};
pub use crate::core::settings_manager::{CompactionSettings, RetrySettings, Settings, SettingsManager};
pub use crate::core::system_prompt::{build_system_prompt, diff_system_prompt_sections};
pub use crate::core::tools::{
    BashOperations, BashToolDetails, BashToolInput, BashToolOptions, EditToolDetails, EditToolInput, EditToolOptions,
    FindToolDetails, FindToolInput, FindToolOptions, GrepToolDetails, GrepToolInput, GrepToolOptions, LsToolDetails,
    LsToolInput, LsToolOptions, PowerShellOperations, PowerShellToolDetails, PowerShellToolInput,
    PowerShellToolOptions, ReadToolDetails, ReadToolInput, ReadToolOptions, Tool, TruncationResult, WriteToolInput,
    WriteToolOptions, create_bash_tool, create_coding_tools, create_edit_tool, create_find_tool, create_grep_tool,
    create_ls_tool, create_powershell_tool, create_read_only_tools, create_read_tool, create_write_tool,
    with_file_mutation_queue,
};
pub use crate::core::trust_manager::{
    ProjectTrustDecision, ProjectTrustStore, ProjectTrustStoreEntry, has_trust_requiring_project_resources,
};

// WIRE(@tui-w3): re-export InteractiveMode, InteractiveModeOptions, JsonAgentSessionEvent, ModelInfo, PrintModeOptions, RpcClient, RpcClientOptions, RpcCommand, RpcEventListener, RpcExtensionUIRequest, RpcExtensionUIResponse, RpcResponse, RpcSessionState, run_print_mode, run_rpc_mode, and theme helpers from modes::* once @tui merges.
