//! Port of packages/coding-agent/test/suite/regressions/4167-thinking-toggle-pending-tool-render.test.ts

// #4167

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_ai::auth::types::{AuthInteraction, AuthType};
use pi_ai::{
    AssistantContentBlock, AssistantMessage, JsonObject, Message, Model, ModelThinkingLevel, StopReason, TextContent,
    ToolCall, ToolResultMessage, Transport, Usage, UsageCost, UserContentBlock,
};
use pi_coding_agent::modes::interactive::bug_report::BugReportSession;
use pi_coding_agent::modes::interactive::components::custom_entry::EntryRenderer;
use pi_coding_agent::modes::interactive::components::custom_message::MessageRenderer;
use pi_coding_agent::modes::interactive::components::footer::{
    FooterContextUsage, FooterRoutedModel, FooterSession, FooterUsageRecord,
};
use pi_coding_agent::modes::interactive::components::markdown_transform::MarkdownTransformer;
use pi_coding_agent::modes::interactive::components::mermaid::MermaidRenderingMode;
use pi_coding_agent::modes::interactive::components::model_selector::ScopedModelItem;
use pi_coding_agent::modes::interactive::components::session_selector::SessionListProgress;
use pi_coding_agent::modes::interactive::components::session_selector_search::SessionInfo;
use pi_coding_agent::modes::interactive::components::settings_selector::{
    CacheWarmingMode, DefaultProjectTrust, DoubleEscapeAction, FullscreenExitOutput, QuietStartup, SteeringMode,
    TreeFilterMode, TuiMode, WarningSettings,
};
use pi_coding_agent::modes::interactive::components::tool_execution::ToolRenderers;
use pi_coding_agent::modes::interactive::components::tree_selector::{
    SessionEntry, SessionMessageEntry, SessionTreeNode,
};
use pi_coding_agent::modes::interactive::components::user_message_selector::UserMessageItem;
use pi_coding_agent::modes::interactive::interactive_mode::{
    AgentSessionRuntimeDiagnostic, BashExecutionResult, CacheMissNotice, CacheWarmingStatusSummary, CancelledResult,
    ClearQueueResult, CompactionSummaryResult, CycleDirection, CycleModelResult, InteractiveExtensionBindings,
    InteractiveExtensionRunner, InteractiveMode, InteractiveModeHooks, InteractiveModeOptions, InteractiveModelRuntime,
    InteractivePromptOptions, InteractiveResourceLoader, InteractiveRuntimeHost, InteractiveSession,
    InteractiveSessionManager, InteractiveSettingsManager, LoadedExtensionsResult, LoadedThemeResource,
    NavigateTreeOptions, NavigateTreeResult, PromptTemplateInfo, ProviderAuthInfo, ProviderAuthStatusInfo,
    RegisteredExtensionCommand, RegisteredExtensionShortcut, ResourceDiagnostic, RuntimeForkOptions, RuntimeForkResult,
    SessionStats, SkillInfo, SourceInfo, StoredCredentialInfo, SwitchSessionOptions, UsageBreakdownEntry,
};
use pi_coding_agent::modes::interactive::model_catalog_refresh::{ModelCatalogRefreshResult, ModelCatalogRuntime};
use pi_coding_agent::modes::interactive::session_share::{SessionShareDestination, ShareSession};
use pi_coding_agent::modes::interactive::theme::theme::{Theme, init_theme};
use pi_coding_agent::modes::interactive::theme::theme_controller::ThemeSettingsSource;
use pi_coding_agent::modes::json_event::AgentSessionEvent;
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result, Unsubscribe};
use pi_tui::terminal::{TerminalInputHandler, TerminalResizeHandler};
use pi_tui::terminal_image::TerminalCapabilityOverrides;
use pi_tui::{
    Container, KeybindingsManager, ScrollViewScrollbar, TUI_KEYBINDINGS, Terminal, WheelScrollLines, set_keybindings,
    strip_terminal_sequences,
};
use serde_json::json;
use serial_test::serial;

const TOOL_CALL_ID: &str = "tool-4167";
const TOOL_NAME: &str = "slow_tool";

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

struct VirtualTerminal;

#[async_trait]
impl Terminal for VirtualTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        120
    }
    fn rows(&self) -> i64 {
        40
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&self, _lines: i64) {}
    fn hide_cursor(&self) {}
    fn show_cursor(&self) {}
    fn clear_line(&self) {}
    fn clear_from_cursor(&self) {}
    fn clear_screen(&self) {}
    fn set_title(&self, _title: &str) {}
    fn set_progress(&self, _active: bool) {}
}

struct FakeSettingsManager;

impl ThemeSettingsSource for FakeSettingsManager {
    fn get_theme_setting(&self) -> Option<String> {
        Some("dark".to_string())
    }
}

impl InteractiveSettingsManager for FakeSettingsManager {
    fn get_terminal_capability_overrides(&self) -> TerminalCapabilityOverrides {
        TerminalCapabilityOverrides::default()
    }
    fn get_tui_mode(&self) -> TuiMode {
        TuiMode::Regular
    }
    fn set_tui_mode(&self, _mode: TuiMode) {}
    fn get_show_hardware_cursor(&self) -> bool {
        false
    }
    fn set_show_hardware_cursor(&self, _enabled: bool) {}
    fn get_clear_on_shrink(&self) -> bool {
        false
    }
    fn set_clear_on_shrink(&self, _enabled: bool) {}
    fn get_fullscreen_copy_on_select(&self) -> bool {
        false
    }
    fn set_fullscreen_copy_on_select(&self, _enabled: bool) {}
    fn get_fullscreen_wheel_scroll_lines(&self) -> WheelScrollLines {
        WheelScrollLines::AUTO
    }
    fn set_fullscreen_wheel_scroll_lines(&self, _lines: WheelScrollLines) {}
    fn get_fullscreen_exit_output(&self) -> FullscreenExitOutput {
        FullscreenExitOutput::Transcript
    }
    fn set_fullscreen_exit_output(&self, _output: FullscreenExitOutput) {}
    fn get_fullscreen_scrollbar(&self) -> ScrollViewScrollbar {
        ScrollViewScrollbar::Auto
    }
    fn set_fullscreen_scrollbar(&self, _mode: ScrollViewScrollbar) {}
    fn get_editor_padding_x(&self) -> i64 {
        0
    }
    fn set_editor_padding_x(&self, _padding: i64) {}
    fn get_autocomplete_max_visible(&self) -> i64 {
        5
    }
    fn set_autocomplete_max_visible(&self, _max_visible: i64) {}
    fn get_hide_thinking_block(&self) -> bool {
        false
    }
    fn set_hide_thinking_block(&self, _hidden: bool) {}
    fn get_output_pad(&self) -> i64 {
        1
    }
    fn set_output_pad(&self, _pad: i64) {}
    fn get_enable_skill_commands(&self) -> bool {
        false
    }
    fn set_enable_skill_commands(&self, _enabled: bool) {}
    fn get_collapse_changelog(&self) -> bool {
        false
    }
    fn set_collapse_changelog(&self, _collapsed: bool) {}
    fn get_code_block_indent(&self) -> usize {
        2
    }
    fn get_quiet_startup(&self) -> QuietStartup {
        QuietStartup::True
    }
    fn set_quiet_startup(&self, _quiet: QuietStartup) {}
    fn get_mermaid_rendering_mode(&self) -> MermaidRenderingMode {
        MermaidRenderingMode::Off
    }
    fn set_mermaid_rendering_mode(&self, _mode: MermaidRenderingMode) {}
    fn get_show_cache_miss_notices(&self) -> bool {
        false
    }
    fn set_show_cache_miss_notices(&self, _shown: bool) {}
    fn get_show_terminal_progress(&self) -> bool {
        false
    }
    fn set_show_terminal_progress(&self, _enabled: bool) {}
    fn get_show_images(&self) -> bool {
        false
    }
    fn set_show_images(&self, _enabled: bool) {}
    fn get_image_width_cells(&self) -> Option<i64> {
        Some(60)
    }
    fn set_image_width_cells(&self, _width: Option<i64>) {}
    fn get_image_auto_resize(&self) -> bool {
        false
    }
    fn set_image_auto_resize(&self, _enabled: bool) {}
    fn get_block_images(&self) -> bool {
        false
    }
    fn set_block_images(&self, _blocked: bool) {}
    fn get_theme(&self) -> Option<String> {
        Some("dark".to_string())
    }
    fn set_theme(&self, _theme: &str) {}
    fn get_last_changelog_version(&self) -> Option<String> {
        None
    }
    fn set_last_changelog_version(&self, _version: &str) {}
    fn get_extension_changelog_version(&self, _key: &str) -> Option<String> {
        None
    }
    fn set_extension_changelog_version(&self, _key: &str, _version: &str) {}
    fn get_http_idle_timeout_ms(&self) -> u64 {
        300_000
    }
    fn set_http_idle_timeout_ms(&self, _timeout_ms: u64) {}
    fn get_cache_warming_mode(&self) -> CacheWarmingMode {
        CacheWarmingMode::Off
    }
    fn set_cache_warming_mode(&self, _mode: CacheWarmingMode) {}
    fn get_transport(&self) -> Transport {
        Transport::Sse
    }
    fn set_transport(&self, _transport: Transport) {}
    fn get_session_share_destination(&self) -> SessionShareDestination {
        SessionShareDestination::Gist
    }
    fn set_session_share_destination(&self, _destination: SessionShareDestination) {}
    fn get_default_provider(&self) -> Option<String> {
        None
    }
    fn get_default_model(&self) -> Option<String> {
        None
    }
    fn get_default_thinking_level(&self) -> Option<ModelThinkingLevel> {
        None
    }
    fn get_all_model_thinking_levels(&self) -> IndexMap<String, ModelThinkingLevel> {
        IndexMap::new()
    }
    fn set_model_thinking_level(&self, _provider: &str, _model_id: &str, _level: ModelThinkingLevel) {}
    fn remove_model_thinking_level(&self, _provider: &str, _model_id: &str) {}
    fn get_enabled_models(&self) -> Option<Vec<String>> {
        None
    }
    fn set_enabled_models(&self, _patterns: Option<Vec<String>>) {}
    fn get_warnings(&self) -> WarningSettings {
        WarningSettings::default()
    }
    fn set_warnings(&self, _warnings: WarningSettings) {}
    fn get_enable_install_telemetry(&self) -> bool {
        false
    }
    fn set_enable_install_telemetry(&self, _enabled: bool) {}
    fn get_default_project_trust(&self) -> DefaultProjectTrust {
        DefaultProjectTrust::Ask
    }
    fn set_default_project_trust(&self, _trust: DefaultProjectTrust) {}
    fn get_double_escape_action(&self) -> DoubleEscapeAction {
        DoubleEscapeAction::Tree
    }
    fn set_double_escape_action(&self, _action: DoubleEscapeAction) {}
    fn get_tree_filter_mode(&self) -> TreeFilterMode {
        TreeFilterMode::Default
    }
    fn set_tree_filter_mode(&self, _mode: TreeFilterMode) {}
    fn get_branch_summary_skip_prompt(&self) -> bool {
        false
    }
    fn get_external_editor_command(&self) -> Option<String> {
        None
    }
    fn get_or_create_device_id(&self) -> String {
        "device-4167".to_string()
    }
    fn is_project_trusted(&self) -> bool {
        true
    }
}

struct FakeSessionManager {
    cwd: String,
    entries: Vec<SessionEntry>,
}

impl InteractiveSessionManager for FakeSessionManager {
    fn get_cwd(&self) -> String {
        self.cwd.clone()
    }
    fn get_session_dir(&self) -> String {
        self.cwd.clone()
    }
    fn get_session_id(&self) -> String {
        "session-4167".to_string()
    }
    fn get_session_file(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
    fn is_persisted(&self) -> bool {
        false
    }
    fn uses_default_session_dir(&self) -> bool {
        false
    }
    fn get_leaf_id(&self) -> Option<String> {
        self.entries.last().map(|e| e.id().to_string())
    }
    fn get_tree(&self) -> Vec<SessionTreeNode> {
        Vec::new()
    }
    fn get_entries(&self) -> Vec<SessionEntry> {
        self.entries.clone()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        self.entries.clone()
    }
    fn build_context_entries(&self) -> Vec<SessionEntry> {
        self.entries.clone()
    }
    fn append_label_change(&self, _entry_id: &str, _label: Option<&str>) {}
    fn list_current_sessions(
        &self,
        _on_progress: Option<SessionListProgress>,
        _signal: AbortSignal,
    ) -> BoxFuture<Result<Vec<SessionInfo>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn list_all_sessions(
        &self,
        _on_progress: Option<SessionListProgress>,
        _signal: AbortSignal,
    ) -> BoxFuture<Result<Vec<SessionInfo>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn rename_session(&self, _session_file: &str, _next_name: Option<&str>) -> BoxFuture<Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

struct FakeModelRuntime;

impl ModelCatalogRuntime for FakeModelRuntime {
    fn runtime_ptr_key(&self) -> usize {
        self as *const Self as usize
    }
    fn refresh(&self, _signal: AbortSignal) -> BoxFuture<Result<ModelCatalogRefreshResult>> {
        Box::pin(async { Ok(ModelCatalogRefreshResult::default()) })
    }
    fn get_available_snapshot(&self) -> Vec<Model> {
        Vec::new()
    }
    fn get_model(&self, _provider: &str, _id: &str) -> Option<Model> {
        None
    }
    fn get_error(&self) -> Option<String> {
        None
    }
}

#[async_trait]
impl InteractiveModelRuntime for FakeModelRuntime {
    fn get_providers(&self) -> Vec<ProviderAuthInfo> {
        Vec::new()
    }
    fn get_provider_auth_status(&self, _provider_id: &str) -> ProviderAuthStatusInfo {
        ProviderAuthStatusInfo::default()
    }
    fn is_using_oauth(&self, _provider_id: &str) -> bool {
        false
    }
    async fn check_auth_type(&self, _provider_id: &str) -> Option<AuthType> {
        None
    }
    async fn get_api_key(&self, _provider_id: &str) -> Option<String> {
        None
    }
    async fn list_credentials(&self, _signal: AbortSignal) -> Result<Vec<StoredCredentialInfo>> {
        Ok(Vec::new())
    }
    async fn logout(&self, _provider_id: &str, _signal: AbortSignal) -> Result<()> {
        Ok(())
    }
    async fn refresh_providers(
        &self,
        _providers: Vec<String>,
        _signal: AbortSignal,
    ) -> Result<ModelCatalogRefreshResult> {
        Ok(ModelCatalogRefreshResult::default())
    }
    async fn login(
        &self,
        _provider_id: &str,
        _auth_type: AuthType,
        _interaction: Arc<dyn AuthInteraction>,
        _signal: AbortSignal,
        _device_id: String,
    ) -> Result<()> {
        Ok(())
    }
}

struct FakeResourceLoader;

impl InteractiveResourceLoader for FakeResourceLoader {
    fn get_skills(&self) -> (Vec<SkillInfo>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new())
    }
    fn get_prompts(&self) -> (Vec<PromptTemplateInfo>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new())
    }
    fn get_themes(&self) -> (Vec<Theme>, Vec<LoadedThemeResource>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new(), Vec::new())
    }
    fn get_extensions(&self) -> LoadedExtensionsResult {
        LoadedExtensionsResult::default()
    }
    fn get_system_prompt_source(&self) -> Option<SourceInfo> {
        None
    }
    fn get_append_system_prompt_sources(&self) -> Vec<SourceInfo> {
        Vec::new()
    }
    fn get_agents_files(&self) -> Vec<SourceInfo> {
        Vec::new()
    }
}

struct FakeExtensionRunner;

impl InteractiveExtensionRunner for FakeExtensionRunner {
    fn get_registered_commands(&self) -> Vec<RegisteredExtensionCommand> {
        Vec::new()
    }
    fn has_command(&self, _name: &str) -> bool {
        false
    }
    fn get_command_diagnostics(&self) -> Vec<ResourceDiagnostic> {
        Vec::new()
    }
    fn get_shortcut_diagnostics(&self) -> Vec<ResourceDiagnostic> {
        Vec::new()
    }
    fn get_shortcuts(&self) -> IndexMap<String, RegisteredExtensionShortcut> {
        IndexMap::new()
    }
    fn get_markdown_transformers(&self) -> Vec<MarkdownTransformer> {
        Vec::new()
    }
    fn get_entry_renderer(&self, _custom_type: &str) -> Option<EntryRenderer> {
        None
    }
    fn get_message_renderer(&self, _custom_type: &str) -> Option<MessageRenderer> {
        None
    }
    fn has_user_bash_handlers(&self) -> bool {
        false
    }
    fn emit_user_bash(
        &self,
        _command: &str,
        _exclude_from_context: bool,
        _cwd: &str,
    ) -> BoxFuture<Result<Option<BashExecutionResult>>> {
        Box::pin(async { Ok(None) })
    }
}

type EventListener = Arc<dyn Fn(AgentSessionEvent) -> BoxFuture<()> + Send + Sync>;

struct FakeSession {
    settings: Arc<FakeSettingsManager>,
    session_manager: Arc<FakeSessionManager>,
    model_runtime: Arc<FakeModelRuntime>,
    resource_loader: Arc<FakeResourceLoader>,
    extension_runner: Arc<FakeExtensionRunner>,
    listeners: Mutex<Vec<EventListener>>,
}

impl FakeSession {
    fn new(entries: Vec<SessionEntry>) -> Arc<Self> {
        Arc::new(Self {
            settings: Arc::new(FakeSettingsManager),
            session_manager: Arc::new(FakeSessionManager {
                cwd: pi_js::env::cwd(),
                entries,
            }),
            model_runtime: Arc::new(FakeModelRuntime),
            resource_loader: Arc::new(FakeResourceLoader),
            extension_runner: Arc::new(FakeExtensionRunner),
            listeners: Mutex::new(Vec::new()),
        })
    }

    async fn emit(&self, event: AgentSessionEvent) {
        let listeners = self.listeners.lock().unwrap().clone();
        for l in listeners {
            l(event.clone()).await;
        }
    }
}

impl FooterSession for FakeSession {
    fn session_ptr_key(&self) -> usize {
        self as *const Self as usize
    }
    fn session_id(&self) -> String {
        "session-4167".to_string()
    }
    fn leaf_id(&self) -> Option<String> {
        self.session_manager.get_leaf_id()
    }
    fn entry_count(&self) -> usize {
        self.session_manager.entries.len()
    }
    fn cwd(&self) -> String {
        self.session_manager.get_cwd()
    }
    fn session_name(&self) -> Option<String> {
        None
    }
    fn model(&self) -> Option<Model> {
        None
    }
    fn routed_model(&self) -> Option<FooterRoutedModel> {
        None
    }
    fn thinking_level(&self) -> String {
        "off".to_string()
    }
    fn get_context_usage(&self) -> Option<FooterContextUsage> {
        None
    }
    fn is_using_subscription(&self, _provider: &str) -> bool {
        false
    }
    fn usage_records(&self) -> Vec<FooterUsageRecord> {
        Vec::new()
    }
}

#[async_trait]
impl InteractiveSession for FakeSession {
    fn session_manager(&self) -> Arc<dyn InteractiveSessionManager> {
        Arc::clone(&self.session_manager) as Arc<dyn InteractiveSessionManager>
    }
    fn settings_manager(&self) -> Arc<dyn InteractiveSettingsManager> {
        Arc::clone(&self.settings) as Arc<dyn InteractiveSettingsManager>
    }
    fn model_runtime(&self) -> Arc<dyn InteractiveModelRuntime> {
        Arc::clone(&self.model_runtime) as Arc<dyn InteractiveModelRuntime>
    }
    fn resource_loader(&self) -> Arc<dyn InteractiveResourceLoader> {
        Arc::clone(&self.resource_loader) as Arc<dyn InteractiveResourceLoader>
    }
    fn extension_runner(&self) -> Arc<dyn InteractiveExtensionRunner> {
        Arc::clone(&self.extension_runner) as Arc<dyn InteractiveExtensionRunner>
    }
    fn as_footer_session(&self) -> Arc<dyn FooterSession> {
        Arc::new(FakeFooterSessionRef(Arc::clone(&self.session_manager)))
    }
    fn as_share_session(&self) -> Option<Arc<dyn ShareSession>> {
        None
    }
    fn as_bug_report_session(&self) -> Option<Arc<dyn BugReportSession>> {
        None
    }
    fn model_thinking_level(&self) -> ModelThinkingLevel {
        ModelThinkingLevel::Off
    }
    fn scoped_models(&self) -> Vec<ScopedModelItem> {
        Vec::new()
    }
    fn set_scoped_models(&self, _models: Vec<ScopedModelItem>) {}
    fn auto_compaction_enabled(&self) -> bool {
        false
    }
    fn set_auto_compaction_enabled(&self, _enabled: bool) {}
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::OneAtATime
    }
    fn set_steering_mode(&self, _mode: SteeringMode) {}
    fn follow_up_mode(&self) -> SteeringMode {
        SteeringMode::OneAtATime
    }
    fn set_follow_up_mode(&self, _mode: SteeringMode) {}
    fn set_transport(&self, _transport: Transport) {}
    fn set_cache_warming_mode(&self, _mode: CacheWarmingMode) {}
    fn cache_warming_status(&self) -> Option<CacheWarmingStatusSummary> {
        None
    }
    fn is_streaming(&self) -> bool {
        false
    }
    fn is_idle(&self) -> bool {
        true
    }
    fn is_compacting(&self) -> bool {
        false
    }
    fn is_bash_running(&self) -> bool {
        false
    }
    fn retry_attempt(&self) -> u32 {
        0
    }
    fn pending_message_count(&self) -> usize {
        0
    }
    fn get_steering_messages(&self) -> Vec<String> {
        Vec::new()
    }
    fn get_follow_up_messages(&self) -> Vec<String> {
        Vec::new()
    }
    fn clear_queue(&self) -> ClearQueueResult {
        ClearQueueResult::default()
    }
    fn messages(&self) -> Vec<AgentMessage> {
        Vec::new()
    }
    fn prompt_templates(&self) -> Vec<PromptTemplateInfo> {
        Vec::new()
    }
    fn session_file(&self) -> Option<String> {
        None
    }
    fn signal(&self) -> Option<AbortSignal> {
        None
    }
    fn get_available_thinking_levels(&self) -> Vec<ModelThinkingLevel> {
        vec![ModelThinkingLevel::Off]
    }
    fn set_thinking_level(&self, _level: ModelThinkingLevel, _persist: bool) -> Result<()> {
        Ok(())
    }
    fn cycle_thinking_level(&self) -> Option<ModelThinkingLevel> {
        None
    }
    async fn set_model(&self, _model: Model, _persist: bool) -> Result<()> {
        Ok(())
    }
    async fn cycle_model(&self, _direction: CycleDirection) -> Result<Option<CycleModelResult>> {
        Ok(None)
    }
    async fn prompt(&self, _text: &str, _options: Option<InteractivePromptOptions>) -> Result<()> {
        Ok(())
    }
    async fn steer(&self, _text: &str) -> Result<()> {
        Ok(())
    }
    async fn follow_up(&self, _text: &str) -> Result<()> {
        Ok(())
    }
    async fn abort(&self) {}
    async fn wait_for_idle(&self) {}
    fn abort_compaction(&self) {}
    fn abort_branch_summary(&self) {}
    fn abort_retry(&self) {}
    fn abort_bash(&self) {}
    fn subscribe(&self, listener: EventListener) -> Unsubscribe {
        self.listeners.lock().unwrap().push(listener);
        Unsubscribe::noop()
    }
    async fn bind_extensions(&self, _bindings: InteractiveExtensionBindings) -> Result<()> {
        Ok(())
    }
    async fn reload(&self, _before_session_start: Option<Arc<dyn Fn() + Send + Sync>>) -> Result<()> {
        Ok(())
    }
    async fn compact(&self, _custom_instructions: Option<&str>) -> Result<CompactionSummaryResult> {
        Ok(CompactionSummaryResult::default())
    }
    async fn navigate_tree(&self, _target_id: &str, _options: NavigateTreeOptions) -> Result<NavigateTreeResult> {
        Ok(NavigateTreeResult::default())
    }
    fn get_user_messages_for_forking(&self) -> Vec<UserMessageItem> {
        Vec::new()
    }
    fn get_session_stats(&self) -> SessionStats {
        SessionStats::default()
    }
    fn get_last_assistant_text(&self) -> Option<String> {
        None
    }
    fn set_session_name(&self, _name: &str) {}
    fn export_to_jsonl(&self, _output_path: Option<&str>) -> Result<String> {
        Ok(String::new())
    }
    async fn export_to_html(&self, _output_path: Option<&str>, _theme_name: Option<&str>) -> Result<String> {
        Ok(String::new())
    }
    async fn execute_bash(
        &self,
        _command: &str,
        _on_chunk: Arc<dyn for<'a> Fn(&'a str) + Send + Sync>,
        _exclude_from_context: bool,
    ) -> Result<BashExecutionResult> {
        Ok(BashExecutionResult::default())
    }
    fn record_bash_result(&self, _command: &str, _result: &BashExecutionResult, _exclude_from_context: bool) {}
    fn detect_cache_miss(&self, _message: &AssistantMessage) -> Option<CacheMissNotice> {
        None
    }
    fn compute_cache_waste(&self) -> (i64, usize, f64) {
        (0, 0, 0.0)
    }
    fn get_usage_cost_breakdown(&self) -> Vec<UsageBreakdownEntry> {
        Vec::new()
    }
    fn get_tool_renderers(&self, _tool_name: &str) -> Option<ToolRenderers> {
        None
    }
}

struct FakeFooterSessionRef(Arc<FakeSessionManager>);

impl FooterSession for FakeFooterSessionRef {
    fn session_ptr_key(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
    fn session_id(&self) -> String {
        self.0.get_session_id()
    }
    fn leaf_id(&self) -> Option<String> {
        self.0.get_leaf_id()
    }
    fn entry_count(&self) -> usize {
        self.0.entries.len()
    }
    fn cwd(&self) -> String {
        self.0.get_cwd()
    }
    fn session_name(&self) -> Option<String> {
        None
    }
    fn model(&self) -> Option<Model> {
        None
    }
    fn routed_model(&self) -> Option<FooterRoutedModel> {
        None
    }
    fn thinking_level(&self) -> String {
        "off".to_string()
    }
    fn get_context_usage(&self) -> Option<FooterContextUsage> {
        None
    }
    fn is_using_subscription(&self, _provider: &str) -> bool {
        false
    }
    fn usage_records(&self) -> Vec<FooterUsageRecord> {
        Vec::new()
    }
}

struct FakeRuntimeHost {
    session: Arc<FakeSession>,
}

#[async_trait]
impl InteractiveRuntimeHost for FakeRuntimeHost {
    fn session(&self) -> Arc<dyn InteractiveSession> {
        Arc::clone(&self.session) as Arc<dyn InteractiveSession>
    }
    fn agent_dir(&self) -> String {
        pi_js::env::tmp_dir()
    }
    fn diagnostics(&self) -> Vec<AgentSessionRuntimeDiagnostic> {
        Vec::new()
    }
    fn model_fallback_message(&self) -> Option<String> {
        None
    }
    fn set_before_session_invalidate(&self, _cb: Arc<dyn Fn() + Send + Sync>) {}
    fn set_rebind_session(&self, _cb: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>) {}
    async fn new_session(&self) -> Result<CancelledResult> {
        Ok(CancelledResult { cancelled: true })
    }
    async fn fork(&self, _entry_id: &str, _options: Option<RuntimeForkOptions>) -> Result<RuntimeForkResult> {
        Ok(RuntimeForkResult {
            cancelled: true,
            selected_text: None,
        })
    }
    async fn switch_session(
        &self,
        _session_path: &str,
        _options: Option<SwitchSessionOptions>,
    ) -> Result<CancelledResult> {
        Ok(CancelledResult { cancelled: true })
    }
    async fn import_from_jsonl(&self, _input_path: &str, _cwd_override: Option<&str>) -> Result<CancelledResult> {
        Ok(CancelledResult { cancelled: true })
    }
    async fn dispose(&self) {}
}

fn create_assistant_tool_call_message() -> AgentMessage {
    let mut args = JsonObject::new();
    args.insert("delayMs".to_string(), json!(10_000));
    AgentMessage::Message(Message::Assistant(AssistantMessage {
        content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
            TOOL_CALL_ID,
            TOOL_NAME,
            args,
        ))],
        api: "test-api".to_string(),
        provider: "test-provider".into(),
        model: "test-model".to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0,
            cost: UsageCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
                total_from_provider: None,
            },
        },
        stop_reason: StopReason::ToolUse,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
        duration_ms: None,
    }))
}

fn create_tool_result_message(text: &str) -> AgentMessage {
    AgentMessage::Message(Message::ToolResult(ToolResultMessage {
        tool_call_id: TOOL_CALL_ID.to_string(),
        tool_name: TOOL_NAME.to_string(),
        content: vec![UserContentBlock::Text(TextContent::new(text))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: pi_js::time::now_ms(),
        duration_ms: None,
    }))
}

fn create_session_entries(messages: Vec<AgentMessage>) -> Vec<SessionEntry> {
    let mut parent_id: Option<String> = None;
    messages
        .into_iter()
        .enumerate()
        .map(|(index, message)| {
            let id = format!("entry-{index}");
            let entry = SessionEntry::Message(SessionMessageEntry {
                id: id.clone(),
                parent_id: parent_id.clone(),
                timestamp: "2026-01-01T00:00:00.000Z".to_string(),
                message,
            });
            parent_id = Some(id);
            entry
        })
        .collect()
}

fn render_chat(container: &Container) -> String {
    strip_terminal_sequences(&container.render(120).join("\n"))
}

fn create_mode(entries: Vec<SessionEntry>) -> (InteractiveMode, Arc<FakeSession>) {
    let session = FakeSession::new(entries);
    let host: Arc<dyn InteractiveRuntimeHost> = Arc::new(FakeRuntimeHost {
        session: Arc::clone(&session),
    });
    let options = InteractiveModeOptions {
        initial_theme_setting: Some("dark".to_string()),
        terminal: Some(Arc::new(VirtualTerminal)),
        hooks: Some(InteractiveModeHooks {
            exit_process: Arc::new(|_| {}),
            ..Default::default()
        }),
        ..Default::default()
    };
    (InteractiveMode::new(host, options), session)
}

mod interactive_mode_render_session_entries {
    use super::*;

    #[tokio::test]
    #[serial]
    async fn keeps_unresolved_rendered_tool_calls_registered_for_live_completion_events() {
        let _guard = TestGuard;
        init_theme(Some("dark"), None);

        // PORT: `render_session_entries`, `handle_event`, and `pending_tool_components` are `pub(crate)` in
        // `InteractiveMode`, so this exercises them via `init()` (`render_initial_messages`) and `session.subscribe`.
        let (mode, session) = create_mode(create_session_entries(vec![create_assistant_tool_call_message()]));
        mode.init().await;
        tokio::time::sleep(Duration::from_millis(5)).await;

        assert!(render_chat(mode.chat_container()).contains(TOOL_NAME));

        session
            .emit(AgentSessionEvent::ToolExecutionEnd {
                tool_call_id: TOOL_CALL_ID.to_string(),
                tool_name: TOOL_NAME.to_string(),
                result: pi_agent_core::AgentToolResult::text("FINAL_RESULT"),
                is_error: false,
                parent_tool_call_id: None,
            })
            .await;

        assert!(render_chat(mode.chat_container()).contains("FINAL_RESULT"));

        // Verify the entry was removed from pending_tool_components after completion.
        session
            .emit(AgentSessionEvent::ToolExecutionEnd {
                tool_call_id: TOOL_CALL_ID.to_string(),
                tool_name: TOOL_NAME.to_string(),
                result: pi_agent_core::AgentToolResult::text("STALE_SECOND_RESULT"),
                is_error: false,
                parent_tool_call_id: None,
            })
            .await;

        let chat = render_chat(mode.chat_container());
        assert!(chat.contains("FINAL_RESULT"));
        assert!(!chat.contains("STALE_SECOND_RESULT"));

        mode.stop(None);
    }

    #[tokio::test]
    #[serial]
    async fn does_not_keep_completed_historical_tool_calls_registered_as_pending() {
        let _guard = TestGuard;
        init_theme(Some("dark"), None);

        let (mode, session) = create_mode(create_session_entries(vec![
            create_assistant_tool_call_message(),
            create_tool_result_message("HISTORICAL_RESULT"),
        ]));
        mode.init().await;
        tokio::time::sleep(Duration::from_millis(5)).await;

        assert!(render_chat(mode.chat_container()).contains("HISTORICAL_RESULT"));

        // Completed historical tool calls must not stay registered in pending_tool_components.
        session
            .emit(AgentSessionEvent::ToolExecutionEnd {
                tool_call_id: TOOL_CALL_ID.to_string(),
                tool_name: TOOL_NAME.to_string(),
                result: pi_agent_core::AgentToolResult::text("UNEXPECTED_OVERWRITE"),
                is_error: false,
                parent_tool_call_id: None,
            })
            .await;

        let chat = render_chat(mode.chat_container());
        assert!(chat.contains("HISTORICAL_RESULT"));
        assert!(!chat.contains("UNEXPECTED_OVERWRITE"));

        mode.stop(None);
    }
}
