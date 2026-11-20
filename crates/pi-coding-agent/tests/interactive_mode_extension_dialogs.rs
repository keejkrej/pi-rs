//! Port of packages/coding-agent/test/interactive-mode-extension-dialogs.test.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_ai::auth::types::{AuthInteraction, AuthType};
use pi_ai::{AssistantMessage, Model, ModelThinkingLevel};
use pi_coding_agent::modes::interactive::bug_report::BugReportSession;
use pi_coding_agent::modes::interactive::components::custom_entry::EntryRenderer;
use pi_coding_agent::modes::interactive::components::custom_message::MessageRenderer;
use pi_coding_agent::modes::interactive::components::footer::{FooterRoutedModel, FooterSession, FooterUsageRecord};
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
use pi_coding_agent::modes::interactive::components::tree_selector::{SessionEntry, SessionTreeNode};
use pi_coding_agent::modes::interactive::components::user_message_selector::UserMessageItem;
use pi_coding_agent::modes::interactive::interactive_mode::{
    AgentSessionRuntimeDiagnostic, BashExecutionResult, CacheMissNotice, CacheWarmingStatusSummary, CancelledResult,
    ClearQueueResult, CompactionSummaryResult, CycleDirection, CycleModelResult, EditorFactory, ExtensionCustomOptions,
    ExtensionUIDialogOptions, InteractiveExtensionBindings, InteractiveExtensionRunner, InteractiveMode,
    InteractiveModeOptions, InteractiveModelRuntime, InteractivePromptOptions, InteractiveResourceLoader,
    InteractiveRuntimeHost, InteractiveSession, InteractiveSessionManager, InteractiveSettingsManager,
    LoadedExtensionsResult, LoadedThemeResource, NavigateTreeOptions, NavigateTreeResult, PromptTemplateInfo,
    ProviderAuthInfo, ProviderAuthStatusInfo, RegisteredExtensionCommand, RegisteredExtensionShortcut,
    ResourceDiagnostic, RuntimeForkOptions, RuntimeForkResult, SessionStats, SkillInfo, SourceInfo,
    StoredCredentialInfo, SwitchSessionOptions, UsageBreakdownEntry,
};
use pi_coding_agent::modes::interactive::model_catalog_refresh::{ModelCatalogRefreshResult, ModelCatalogRuntime};
use pi_coding_agent::modes::interactive::program_status_reporter::{
    BlockedKind, BlockedStatus, ProgramStatus, ProgramStatusState,
};
use pi_coding_agent::modes::interactive::session_share::{SessionShareDestination, ShareSession};
use pi_coding_agent::modes::interactive::theme::theme::{Theme, init_theme};
use pi_coding_agent::modes::interactive::theme::theme_controller::ThemeSettingsSource;
use pi_coding_agent::modes::json_event::AgentSessionEvent;
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::{BoxFuture, Result, Unsubscribe};
use pi_tui::autocomplete::AutocompleteProvider;
use pi_tui::editor_component::{EditorBorderColor, EditorTextCallback};
use pi_tui::terminal::{TerminalInputHandler, TerminalResizeHandler};
use pi_tui::tui::OverlayHandle;
use pi_tui::{
    Component, EditorComponent, KeybindingsManager, ScrollViewScrollbar, TUI_KEYBINDINGS, Terminal, Text,
    WheelScrollLines, set_keybindings, strip_terminal_sequences,
};
use serial_test::serial;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

struct RecordingTerminal {
    program_statuses: Mutex<Vec<ProgramStatus>>,
}

impl RecordingTerminal {
    fn new() -> Self {
        Self {
            program_statuses: Mutex::new(Vec::new()),
        }
    }

    fn statuses(&self) -> Vec<ProgramStatus> {
        self.program_statuses.lock().unwrap().clone()
    }
}

#[async_trait]
impl Terminal for RecordingTerminal {
    fn start(&self, _: TerminalInputHandler, _: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _: Option<i64>, _: Option<i64>) {}
    fn write(&self, _: &str) {}
    fn columns(&self) -> i64 {
        80
    }
    fn rows(&self) -> i64 {
        24
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&self, _: i64) {}
    fn hide_cursor(&self) {}
    fn show_cursor(&self) {}
    fn clear_line(&self) {}
    fn clear_from_cursor(&self) {}
    fn clear_screen(&self) {}
    fn set_title(&self, _: &str) {}
    fn set_progress(&self, _: bool) {}
    fn set_program_status(&self, status: ProgramStatus) {
        self.program_statuses.lock().unwrap().push(status);
    }
}

struct MockSettings;
impl ThemeSettingsSource for MockSettings {
    fn get_theme_setting(&self) -> Option<String> {
        Some("dark".to_string())
    }
}
impl InteractiveSettingsManager for MockSettings {
    fn get_terminal_capability_overrides(&self) -> pi_tui::terminal_image::TerminalCapabilityOverrides {
        Default::default()
    }
    fn get_tui_mode(&self) -> TuiMode {
        TuiMode::Regular
    }
    fn set_tui_mode(&self, _: TuiMode) {}
    fn get_show_hardware_cursor(&self) -> bool {
        false
    }
    fn set_show_hardware_cursor(&self, _: bool) {}
    fn get_clear_on_shrink(&self) -> bool {
        false
    }
    fn set_clear_on_shrink(&self, _: bool) {}
    fn get_fullscreen_copy_on_select(&self) -> bool {
        true
    }
    fn set_fullscreen_copy_on_select(&self, _: bool) {}
    fn get_fullscreen_wheel_scroll_lines(&self) -> WheelScrollLines {
        WheelScrollLines::AUTO
    }
    fn set_fullscreen_wheel_scroll_lines(&self, _: WheelScrollLines) {}
    fn get_fullscreen_exit_output(&self) -> FullscreenExitOutput {
        FullscreenExitOutput::Transcript
    }
    fn set_fullscreen_exit_output(&self, _: FullscreenExitOutput) {}
    fn get_fullscreen_scrollbar(&self) -> ScrollViewScrollbar {
        ScrollViewScrollbar::Auto
    }
    fn set_fullscreen_scrollbar(&self, _: ScrollViewScrollbar) {}
    fn get_editor_padding_x(&self) -> i64 {
        0
    }
    fn set_editor_padding_x(&self, _: i64) {}
    fn get_autocomplete_max_visible(&self) -> i64 {
        5
    }
    fn set_autocomplete_max_visible(&self, _: i64) {}
    fn get_hide_thinking_block(&self) -> bool {
        false
    }
    fn set_hide_thinking_block(&self, _: bool) {}
    fn get_output_pad(&self) -> i64 {
        0
    }
    fn set_output_pad(&self, _: i64) {}
    fn get_enable_skill_commands(&self) -> bool {
        true
    }
    fn set_enable_skill_commands(&self, _: bool) {}
    fn get_collapse_changelog(&self) -> bool {
        false
    }
    fn set_collapse_changelog(&self, _: bool) {}
    fn get_code_block_indent(&self) -> usize {
        2
    }
    fn get_quiet_startup(&self) -> QuietStartup {
        QuietStartup::False
    }
    fn set_quiet_startup(&self, _: QuietStartup) {}
    fn get_mermaid_rendering_mode(&self) -> MermaidRenderingMode {
        MermaidRenderingMode::Off
    }
    fn set_mermaid_rendering_mode(&self, _: MermaidRenderingMode) {}
    fn get_show_cache_miss_notices(&self) -> bool {
        false
    }
    fn set_show_cache_miss_notices(&self, _: bool) {}
    fn get_show_terminal_progress(&self) -> bool {
        false
    }
    fn set_show_terminal_progress(&self, _: bool) {}
    fn get_show_images(&self) -> bool {
        false
    }
    fn set_show_images(&self, _: bool) {}
    fn get_image_width_cells(&self) -> Option<i64> {
        None
    }
    fn set_image_width_cells(&self, _: Option<i64>) {}
    fn get_image_auto_resize(&self) -> bool {
        false
    }
    fn set_image_auto_resize(&self, _: bool) {}
    fn get_block_images(&self) -> bool {
        false
    }
    fn set_block_images(&self, _: bool) {}
    fn get_theme(&self) -> Option<String> {
        Some("dark".to_string())
    }
    fn set_theme(&self, _: &str) {}
    fn get_last_changelog_version(&self) -> Option<String> {
        None
    }
    fn set_last_changelog_version(&self, _: &str) {}
    fn get_extension_changelog_version(&self, _: &str) -> Option<String> {
        None
    }
    fn set_extension_changelog_version(&self, _: &str, _: &str) {}
    fn get_http_idle_timeout_ms(&self) -> u64 {
        0
    }
    fn set_http_idle_timeout_ms(&self, _: u64) {}
    fn get_cache_warming_mode(&self) -> CacheWarmingMode {
        CacheWarmingMode::Off
    }
    fn set_cache_warming_mode(&self, _: CacheWarmingMode) {}
    fn get_transport(&self) -> pi_ai::Transport {
        pi_ai::Transport::Sse
    }
    fn set_transport(&self, _: pi_ai::Transport) {}
    fn get_session_share_destination(&self) -> SessionShareDestination {
        SessionShareDestination::Gist
    }
    fn set_session_share_destination(&self, _: SessionShareDestination) {}
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
    fn set_model_thinking_level(&self, _: &str, _: &str, _: ModelThinkingLevel) {}
    fn remove_model_thinking_level(&self, _: &str, _: &str) {}
    fn get_enabled_models(&self) -> Option<Vec<String>> {
        None
    }
    fn set_enabled_models(&self, _: Option<Vec<String>>) {}
    fn get_warnings(&self) -> WarningSettings {
        WarningSettings::default()
    }
    fn set_warnings(&self, _: WarningSettings) {}
    fn get_enable_install_telemetry(&self) -> bool {
        false
    }
    fn set_enable_install_telemetry(&self, _: bool) {}
    fn get_default_project_trust(&self) -> DefaultProjectTrust {
        DefaultProjectTrust::Ask
    }
    fn set_default_project_trust(&self, _: DefaultProjectTrust) {}
    fn get_double_escape_action(&self) -> DoubleEscapeAction {
        DoubleEscapeAction::Tree
    }
    fn set_double_escape_action(&self, _: DoubleEscapeAction) {}
    fn get_tree_filter_mode(&self) -> TreeFilterMode {
        TreeFilterMode::Default
    }
    fn set_tree_filter_mode(&self, _: TreeFilterMode) {}
    fn get_branch_summary_skip_prompt(&self) -> bool {
        false
    }
    fn get_external_editor_command(&self) -> Option<String> {
        None
    }
    fn get_or_create_device_id(&self) -> String {
        "device-1".to_string()
    }
    fn is_project_trusted(&self) -> bool {
        true
    }
}

struct MockSessionManager;
impl InteractiveSessionManager for MockSessionManager {
    fn get_cwd(&self) -> String {
        "/tmp".to_string()
    }
    fn get_session_dir(&self) -> String {
        "/tmp".to_string()
    }
    fn get_session_id(&self) -> String {
        "sess-1".to_string()
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
        true
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_tree(&self) -> Vec<SessionTreeNode> {
        Vec::new()
    }
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn build_context_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn append_label_change(&self, _: &str, _: Option<&str>) {}
    fn list_current_sessions(
        &self,
        _: Option<SessionListProgress>,
        _: AbortSignal,
    ) -> BoxFuture<Result<Vec<SessionInfo>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn list_all_sessions(&self, _: Option<SessionListProgress>, _: AbortSignal) -> BoxFuture<Result<Vec<SessionInfo>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn rename_session(&self, _: &str, _: Option<&str>) -> BoxFuture<Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

struct MockModelRuntime;
impl ModelCatalogRuntime for MockModelRuntime {
    fn runtime_ptr_key(&self) -> usize {
        self as *const Self as usize
    }
    fn refresh(&self, _: AbortSignal) -> BoxFuture<Result<ModelCatalogRefreshResult>> {
        Box::pin(async { Ok(ModelCatalogRefreshResult::default()) })
    }
    fn get_available_snapshot(&self) -> Vec<Model> {
        Vec::new()
    }
    fn get_model(&self, _: &str, _: &str) -> Option<Model> {
        None
    }
    fn get_error(&self) -> Option<String> {
        None
    }
}
#[async_trait]
impl InteractiveModelRuntime for MockModelRuntime {
    fn get_providers(&self) -> Vec<ProviderAuthInfo> {
        Vec::new()
    }
    fn get_provider_auth_status(&self, _: &str) -> ProviderAuthStatusInfo {
        ProviderAuthStatusInfo::default()
    }
    fn is_using_oauth(&self, _: &str) -> bool {
        false
    }
    async fn check_auth_type(&self, _: &str) -> Option<AuthType> {
        None
    }
    async fn get_api_key(&self, _: &str) -> Option<String> {
        None
    }
    async fn list_credentials(&self, _: AbortSignal) -> Result<Vec<StoredCredentialInfo>> {
        Ok(Vec::new())
    }
    async fn logout(&self, _: &str, _: AbortSignal) -> Result<()> {
        Ok(())
    }
    async fn refresh_providers(&self, _: Vec<String>, _: AbortSignal) -> Result<ModelCatalogRefreshResult> {
        Ok(ModelCatalogRefreshResult::default())
    }
    async fn login(&self, _: &str, _: AuthType, _: Arc<dyn AuthInteraction>, _: AbortSignal, _: String) -> Result<()> {
        Ok(())
    }
}

struct MockResourceLoader;
impl InteractiveResourceLoader for MockResourceLoader {
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

struct MockExtensionRunner;
impl InteractiveExtensionRunner for MockExtensionRunner {
    fn get_registered_commands(&self) -> Vec<RegisteredExtensionCommand> {
        Vec::new()
    }
    fn has_command(&self, _: &str) -> bool {
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
    fn get_entry_renderer(&self, _: &str) -> Option<EntryRenderer> {
        None
    }
    fn get_message_renderer(&self, _: &str) -> Option<MessageRenderer> {
        None
    }
    fn has_user_bash_handlers(&self) -> bool {
        false
    }
    fn emit_user_bash(&self, _: &str, _: bool, _: &str) -> BoxFuture<Result<Option<BashExecutionResult>>> {
        Box::pin(async { Ok(None) })
    }
}

struct MockSession;
impl FooterSession for MockSession {
    fn session_ptr_key(&self) -> usize {
        self as *const Self as usize
    }
    fn session_id(&self) -> String {
        "sess-1".to_string()
    }
    fn leaf_id(&self) -> Option<String> {
        None
    }
    fn entry_count(&self) -> usize {
        0
    }
    fn cwd(&self) -> String {
        "/tmp".to_string()
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
    fn get_context_usage(&self) -> Option<pi_coding_agent::modes::interactive::components::footer::FooterContextUsage> {
        None
    }
    fn is_using_subscription(&self, _: &str) -> bool {
        false
    }
    fn usage_records(&self) -> Vec<FooterUsageRecord> {
        Vec::new()
    }
}

#[async_trait]
impl InteractiveSession for MockSession {
    fn session_manager(&self) -> Arc<dyn InteractiveSessionManager> {
        Arc::new(MockSessionManager)
    }
    fn settings_manager(&self) -> Arc<dyn InteractiveSettingsManager> {
        Arc::new(MockSettings)
    }
    fn model_runtime(&self) -> Arc<dyn InteractiveModelRuntime> {
        Arc::new(MockModelRuntime)
    }
    fn resource_loader(&self) -> Arc<dyn InteractiveResourceLoader> {
        Arc::new(MockResourceLoader)
    }
    fn extension_runner(&self) -> Arc<dyn InteractiveExtensionRunner> {
        Arc::new(MockExtensionRunner)
    }
    fn as_footer_session(&self) -> Arc<dyn FooterSession> {
        Arc::new(MockSession)
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
    fn set_scoped_models(&self, _: Vec<ScopedModelItem>) {}
    fn auto_compaction_enabled(&self) -> bool {
        false
    }
    fn set_auto_compaction_enabled(&self, _: bool) {}
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::OneAtATime
    }
    fn set_steering_mode(&self, _: SteeringMode) {}
    fn follow_up_mode(&self) -> SteeringMode {
        SteeringMode::OneAtATime
    }
    fn set_follow_up_mode(&self, _: SteeringMode) {}
    fn set_transport(&self, _: pi_ai::Transport) {}
    fn set_cache_warming_mode(&self, _: CacheWarmingMode) {}
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
    fn set_thinking_level(&self, _: ModelThinkingLevel, _: bool) -> Result<()> {
        Ok(())
    }
    fn cycle_thinking_level(&self) -> Option<ModelThinkingLevel> {
        None
    }
    async fn set_model(&self, _: Model, _: bool) -> Result<()> {
        Ok(())
    }
    async fn cycle_model(&self, _: CycleDirection) -> Result<Option<CycleModelResult>> {
        Ok(None)
    }
    async fn prompt(&self, _: &str, _: Option<InteractivePromptOptions>) -> Result<()> {
        Ok(())
    }
    async fn steer(&self, _: &str) -> Result<()> {
        Ok(())
    }
    async fn follow_up(&self, _: &str) -> Result<()> {
        Ok(())
    }
    async fn abort(&self) {}
    async fn wait_for_idle(&self) {}
    fn abort_compaction(&self) {}
    fn abort_branch_summary(&self) {}
    fn abort_retry(&self) {}
    fn abort_bash(&self) {}
    fn subscribe(&self, _: Arc<dyn Fn(AgentSessionEvent) -> BoxFuture<()> + Send + Sync>) -> Unsubscribe {
        Unsubscribe::noop()
    }
    async fn bind_extensions(&self, _: InteractiveExtensionBindings) -> Result<()> {
        Ok(())
    }
    async fn reload(&self, _: Option<Arc<dyn Fn() + Send + Sync>>) -> Result<()> {
        Ok(())
    }
    async fn compact(&self, _: Option<&str>) -> Result<CompactionSummaryResult> {
        Ok(CompactionSummaryResult {
            summary: String::new(),
            tokens_before: 0,
            usage: None,
        })
    }
    async fn navigate_tree(&self, _: &str, _: NavigateTreeOptions) -> Result<NavigateTreeResult> {
        Ok(NavigateTreeResult {
            aborted: false,
            cancelled: false,
            editor_text: None,
        })
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
    fn set_session_name(&self, _: &str) {}
    fn export_to_jsonl(&self, _: Option<&str>) -> Result<String> {
        Ok(String::new())
    }
    async fn export_to_html(&self, _: Option<&str>, _: Option<&str>) -> Result<String> {
        Ok(String::new())
    }
    async fn execute_bash(
        &self,
        _: &str,
        _: Arc<dyn for<'a> Fn(&'a str) + Send + Sync>,
        _: bool,
    ) -> Result<BashExecutionResult> {
        Ok(BashExecutionResult::default())
    }
    fn record_bash_result(&self, _: &str, _: &BashExecutionResult, _: bool) {}
    fn detect_cache_miss(&self, _: &AssistantMessage) -> Option<CacheMissNotice> {
        None
    }
    fn compute_cache_waste(&self) -> (i64, usize, f64) {
        (0, 0, 0.0)
    }
    fn get_usage_cost_breakdown(&self) -> Vec<UsageBreakdownEntry> {
        Vec::new()
    }
    fn get_tool_renderers(&self, _: &str) -> Option<ToolRenderers> {
        None
    }
}

struct MockRuntimeHost;
#[async_trait]
impl InteractiveRuntimeHost for MockRuntimeHost {
    fn session(&self) -> Arc<dyn InteractiveSession> {
        Arc::new(MockSession)
    }
    fn agent_dir(&self) -> String {
        "/tmp/pi-agent".to_string()
    }
    fn diagnostics(&self) -> Vec<AgentSessionRuntimeDiagnostic> {
        Vec::new()
    }
    fn model_fallback_message(&self) -> Option<String> {
        None
    }
    fn set_before_session_invalidate(&self, _: Arc<dyn Fn() + Send + Sync>) {}
    fn set_rebind_session(&self, _: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>) {}
    async fn new_session(&self) -> Result<CancelledResult> {
        Ok(CancelledResult { cancelled: false })
    }
    async fn fork(&self, _: &str, _: Option<RuntimeForkOptions>) -> Result<RuntimeForkResult> {
        Ok(RuntimeForkResult {
            cancelled: false,
            selected_text: None,
        })
    }
    async fn switch_session(&self, _: &str, _: Option<SwitchSessionOptions>) -> Result<CancelledResult> {
        Ok(CancelledResult { cancelled: false })
    }
    async fn import_from_jsonl(&self, _: &str, _: Option<&str>) -> Result<CancelledResult> {
        Ok(CancelledResult { cancelled: false })
    }
    async fn dispose(&self) {}
}

struct CustomEditorStub(Mutex<String>);

impl Component for CustomEditorStub {
    fn render(&self, _: usize) -> Vec<String> {
        vec!["custom editor".to_string()]
    }
    fn invalidate(&self) {}
}

impl EditorComponent for CustomEditorStub {
    fn get_text(&self) -> String {
        self.0.lock().unwrap().clone()
    }
    fn set_text(&self, text: &str) {
        *self.0.lock().unwrap() = text.to_string();
    }
    fn add_to_history(&self, _: &str) {}
    fn insert_text_at_cursor(&self, _: &str) {}
    fn border_color(&self) -> Option<EditorBorderColor> {
        None
    }
    fn set_border_color(&self, _: Option<EditorBorderColor>) {}
    fn set_padding_x(&self, _: f64) {}
    fn set_autocomplete_max_visible(&self, _: f64) {}
    fn set_autocomplete_provider(&self, _: Arc<dyn AutocompleteProvider>) {}
    fn on_submit(&self) -> Option<EditorTextCallback> {
        None
    }
    fn set_on_submit(&self, _: Option<EditorTextCallback>) {}
    fn on_change(&self) -> Option<EditorTextCallback> {
        None
    }
    fn set_on_change(&self, _: Option<EditorTextCallback>) {}
}

struct FormWithInput(Arc<dyn Fn(&str) + Send + Sync>);

impl Component for FormWithInput {
    fn render(&self, _: usize) -> Vec<String> {
        vec!["custom form".to_string()]
    }
    fn handle_input(&self, data: &str) {
        (self.0)(data);
    }
    fn invalidate(&self) {}
}

fn setup() -> (TestGuard, InteractiveMode, Arc<RecordingTerminal>) {
    init_theme(Some("dark"), None);
    let term = Arc::new(RecordingTerminal::new());
    let mode = InteractiveMode::new(
        Arc::new(MockRuntimeHost),
        InteractiveModeOptions {
            terminal: Some(term.clone()),
            ..Default::default()
        },
    );
    (TestGuard, mode, term)
}

fn visible(mode: &InteractiveMode) -> String {
    let children = mode.editor_container().children();
    assert_eq!(children.len(), 1);
    let focused = mode.renderer().get_focused_component().expect("focused component");
    assert!(Arc::ptr_eq(&children[0], &focused));
    strip_terminal_sequences(&focused.render(80).join("\n"))
}

// #11166
#[tokio::test]
#[serial]
async fn shows_earlier_confirm_again_after_later_one_is_answered() {
    let (_guard, mode, term) = setup();
    let mode1 = mode.clone();
    let first = tokio::spawn(async move { mode1.show_extension_confirm("First", "first dialog", None).await });
    tokio::task::yield_now().await;
    let mode2 = mode.clone();
    let second = tokio::spawn(async move { mode2.show_extension_confirm("Second", "second dialog", None).await });
    tokio::task::yield_now().await;

    assert!(visible(&mode).contains("Second"));
    assert_eq!(
        term.statuses().last(),
        Some(&ProgramStatus {
            state: ProgramStatusState::Blocked,
            app: Some("pi".to_string()),
            kind: Some(BlockedKind::Permission),
            message: Some("Second".to_string()),
        })
    );

    mode.renderer().get_focused_component().unwrap().handle_input("\x1b");
    assert!(!second.await.unwrap());

    assert!(visible(&mode).contains("First"));
    assert_eq!(
        term.statuses().last(),
        Some(&ProgramStatus {
            state: ProgramStatusState::Blocked,
            app: Some("pi".to_string()),
            kind: Some(BlockedKind::Permission),
            message: Some("First".to_string()),
        })
    );

    mode.renderer().get_focused_component().unwrap().handle_input("\r");
    assert!(first.await.unwrap());
    assert_ne!(
        term.statuses().last().map(|s| s.state),
        Some(ProgramStatusState::Blocked)
    );
    let focused = mode.renderer().get_focused_component().unwrap();
    assert_eq!(mode.editor_container().children().len(), 1);
    assert!(Arc::ptr_eq(&focused, &mode.editor_container().children()[0]));
    assert!(mode.active_editor().is_default(mode.default_editor()));
}

#[tokio::test]
#[serial]
async fn removes_hidden_dialog_when_signal_aborts() {
    let (_guard, mode, _term) = setup();
    let controller = AbortController::new();
    let opts = ExtensionUIDialogOptions {
        signal: Some(controller.signal()),
        timeout_ms: None,
    };
    let mode1 = mode.clone();
    let first = tokio::spawn(async move { mode1.show_extension_confirm("First", "first dialog", Some(opts)).await });
    tokio::task::yield_now().await;
    let mode2 = mode.clone();
    let second = tokio::spawn(async move { mode2.show_extension_confirm("Second", "second dialog", None).await });
    tokio::task::yield_now().await;

    controller.abort(None);
    assert!(!first.await.unwrap());
    assert!(visible(&mode).contains("Second"));

    mode.renderer().get_focused_component().unwrap().handle_input("\r");
    assert!(second.await.unwrap());
    let focused = mode.renderer().get_focused_component().unwrap();
    assert!(Arc::ptr_eq(&focused, &mode.editor_container().children()[0]));
    assert!(mode.active_editor().is_default(mode.default_editor()));
}

// #9909
#[tokio::test]
#[serial]
async fn stacks_concurrent_inline_custom_components() {
    let (_guard, mode, _term) = setup();
    let dones = Arc::new(Mutex::new(Vec::new()));

    let mut handles = Vec::new();
    for label in ["custom one", "custom two"] {
        let dones_c = Arc::clone(&dones);
        let mode_c = mode.clone();
        handles.push(tokio::spawn(async move {
            mode_c
                .show_extension_custom(
                    Box::new(move |_, _, _, done| {
                        let label = label.to_string();
                        let dones_c = Arc::clone(&dones_c);
                        Box::pin(async move {
                            dones_c.lock().unwrap().push(done);
                            Ok(Arc::new(Text::new(Some(&label), Some(0), Some(0), None)) as Arc<dyn Component>)
                        })
                    }),
                    None,
                )
                .await
                .unwrap()
        }));
        tokio::task::yield_now().await;
    }

    assert!(visible(&mode).contains("custom two"));
    dones.lock().unwrap()[1](serde_json::json!("two"));
    assert_eq!(handles.pop().unwrap().await.unwrap(), serde_json::json!("two"));
    assert!(visible(&mode).contains("custom one"));

    dones.lock().unwrap()[0](serde_json::json!("one"));
    assert_eq!(handles.pop().unwrap().await.unwrap(), serde_json::json!("one"));
    let focused = mode.renderer().get_focused_component().unwrap();
    assert!(Arc::ptr_eq(&focused, &mode.editor_container().children()[0]));
    assert!(mode.active_editor().is_default(mode.default_editor()));
}

#[tokio::test]
#[serial]
async fn shows_confirm_opened_from_inline_custom_component_and_restores_component() {
    let (_guard, mode, _term) = setup();
    let done_slot = Arc::new(Mutex::new(None));
    let done_c = Arc::clone(&done_slot);
    let confirm_handle: Arc<Mutex<Option<tokio::task::JoinHandle<bool>>>> = Arc::new(Mutex::new(None));
    let confirm_c = Arc::clone(&confirm_handle);
    let mode_c = mode.clone();
    let mode_in = mode.clone();

    let custom = tokio::spawn(async move {
        mode_c
            .show_extension_custom(
                Box::new(move |_, _, _, done| {
                    let done_c = Arc::clone(&done_c);
                    let confirm_c = Arc::clone(&confirm_c);
                    let mode_in = mode_in.clone();
                    Box::pin(async move {
                        *done_c.lock().unwrap() = Some(done);
                        Ok(Arc::new(FormWithInput(Arc::new(move |_| {
                            let mode_confirm = mode_in.clone();
                            *confirm_c.lock().unwrap() = Some(tokio::spawn(async move {
                                mode_confirm
                                    .show_extension_confirm("Discard?", "discard changes", None)
                                    .await
                            }));
                        }))) as Arc<dyn Component>)
                    })
                }),
                None,
            )
            .await
            .unwrap()
    });
    tokio::task::yield_now().await;
    assert!(visible(&mode).contains("custom form"));

    mode.renderer().get_focused_component().unwrap().handle_input("x");
    tokio::task::yield_now().await;
    assert!(visible(&mode).contains("Discard?"));
    mode.renderer().get_focused_component().unwrap().handle_input("\x1b");
    let confirmed = confirm_handle.lock().unwrap().take().unwrap().await.unwrap();
    assert!(!confirmed);

    assert!(visible(&mode).contains("custom form"));
    done_slot.lock().unwrap().take().unwrap()(serde_json::json!("kept"));
    assert_eq!(custom.await.unwrap(), serde_json::json!("kept"));
    let focused = mode.renderer().get_focused_component().unwrap();
    assert!(Arc::ptr_eq(&focused, &mode.editor_container().children()[0]));
    assert!(mode.active_editor().is_default(mode.default_editor()));
}

// #11428
#[tokio::test]
#[serial]
async fn reports_blocked_status_for_opted_in_inline_custom_component() {
    let (_guard, mode, term) = setup();
    let dones = Arc::new(Mutex::new(Vec::new()));

    let dones_1 = Arc::clone(&dones);
    let mode_1 = mode.clone();
    let first = tokio::spawn(async move {
        mode_1
            .show_extension_custom(
                Box::new(move |_, _, _, done| {
                    let dones_1 = Arc::clone(&dones_1);
                    Box::pin(async move {
                        dones_1.lock().unwrap().push(done);
                        Ok(Arc::new(Text::new(Some("approval"), Some(0), Some(0), None)) as Arc<dyn Component>)
                    })
                }),
                Some(ExtensionCustomOptions {
                    blocked: Some(BlockedStatus {
                        kind: BlockedKind::Permission,
                        message: "Approve?".to_string(),
                    }),
                    ..Default::default()
                }),
            )
            .await
            .unwrap()
    });
    tokio::task::yield_now().await;
    assert!(visible(&mode).contains("approval"));
    assert_eq!(
        term.statuses().last().and_then(|s| s.message.clone()).as_deref(),
        Some("Approve?")
    );

    let dones_2 = Arc::clone(&dones);
    let mode_2 = mode.clone();
    let loader = tokio::spawn(async move {
        mode_2
            .show_extension_custom(
                Box::new(move |_, _, _, done| {
                    let dones_2 = Arc::clone(&dones_2);
                    Box::pin(async move {
                        dones_2.lock().unwrap().push(done);
                        Ok(Arc::new(Text::new(Some("loading"), Some(0), Some(0), None)) as Arc<dyn Component>)
                    })
                }),
                None,
            )
            .await
            .unwrap()
    });
    tokio::task::yield_now().await;
    assert!(visible(&mode).contains("loading"));
    assert_ne!(
        term.statuses().last().map(|s| s.state),
        Some(ProgramStatusState::Blocked)
    );

    dones.lock().unwrap()[1](serde_json::json!("loaded"));
    assert_eq!(loader.await.unwrap(), serde_json::json!("loaded"));
    assert_eq!(
        term.statuses().last().and_then(|s| s.message.clone()).as_deref(),
        Some("Approve?")
    );

    dones.lock().unwrap()[0](serde_json::json!("yes"));
    assert_eq!(first.await.unwrap(), serde_json::json!("yes"));
    assert_ne!(
        term.statuses().last().map(|s| s.state),
        Some(ProgramStatusState::Blocked)
    );
}

// #11816
#[tokio::test]
#[serial]
async fn reports_blocked_status_for_opted_in_overlays_even_while_hidden() {
    let (_guard, mode, term) = setup();
    let dones = Arc::new(Mutex::new(Vec::new()));
    let handles: Arc<Mutex<Vec<OverlayHandle>>> = Arc::new(Mutex::new(Vec::new()));

    let mut tasks = Vec::new();
    for (text, blocked) in [
        ("first", Some(("First?", BlockedKind::Question))),
        ("second", Some(("Second?", BlockedKind::Question))),
        ("loading", None),
    ] {
        let dones_c = Arc::clone(&dones);
        let handles_c = Arc::clone(&handles);
        let mode_c = mode.clone();
        let text = text.to_string();
        let blocked_status = blocked.map(|(msg, kind)| BlockedStatus {
            kind,
            message: msg.to_string(),
        });
        tasks.push(tokio::spawn(async move {
            mode_c
                .show_extension_custom(
                    Box::new(move |_, _, _, done| {
                        let dones_c = Arc::clone(&dones_c);
                        Box::pin(async move {
                            dones_c.lock().unwrap().push(done);
                            Ok(Arc::new(Text::new(Some(&text), Some(0), Some(0), None)) as Arc<dyn Component>)
                        })
                    }),
                    Some(ExtensionCustomOptions {
                        overlay: true,
                        on_handle: Some(Arc::new(move |h| handles_c.lock().unwrap().push(h))),
                        blocked: blocked_status,
                        ..Default::default()
                    }),
                )
                .await
                .unwrap()
        }));
        tokio::task::yield_now().await;
    }

    assert_eq!(mode.extension_overlay_count(), 3);
    assert_eq!(
        term.statuses()
            .iter()
            .filter(|s| s.state == ProgramStatusState::Blocked)
            .count(),
        2
    );

    dones.lock().unwrap()[2](serde_json::json!("loaded"));
    assert_eq!(tasks.pop().unwrap().await.unwrap(), serde_json::json!("loaded"));

    handles.lock().unwrap()[0].set_hidden(true);
    dones.lock().unwrap()[1](serde_json::json!("two"));
    assert_eq!(tasks.pop().unwrap().await.unwrap(), serde_json::json!("two"));
    assert_eq!(
        term.statuses().last().and_then(|s| s.message.clone()).as_deref(),
        Some("First?")
    );

    dones.lock().unwrap()[0](serde_json::json!("one"));
    assert_eq!(tasks.pop().unwrap().await.unwrap(), serde_json::json!("one"));
    assert_ne!(
        term.statuses().last().map(|s| s.state),
        Some(ProgramStatusState::Blocked)
    );
}

// #11816
#[tokio::test]
#[serial]
async fn clears_overlay_blocked_status_on_reset_extension_ui() {
    let (_guard, mode, term) = setup();
    let mode_c = mode.clone();
    let _task = tokio::spawn(async move {
        let _ = mode_c
            .show_extension_custom(
                Box::new(|_, _, _, _| {
                    Box::pin(async {
                        Ok(Arc::new(Text::new(Some("question"), Some(0), Some(0), None)) as Arc<dyn Component>)
                    })
                }),
                Some(ExtensionCustomOptions {
                    overlay: true,
                    blocked: Some(BlockedStatus {
                        kind: BlockedKind::Question,
                        message: "Question?".to_string(),
                    }),
                    ..Default::default()
                }),
            )
            .await;
    });
    tokio::task::yield_now().await;
    assert_eq!(mode.extension_overlay_count(), 1);
    assert_eq!(
        term.statuses().last().and_then(|s| s.message.clone()).as_deref(),
        Some("Question?")
    );

    mode.reset_extension_ui();
    assert_eq!(mode.extension_overlay_count(), 0);
    assert_ne!(
        term.statuses().last().map(|s| s.state),
        Some(ProgramStatusState::Blocked)
    );
}

// #11431
#[tokio::test]
#[serial]
async fn keeps_open_dialog_visible_when_editor_component_is_replaced() {
    let (_guard, mode, _term) = setup();
    let mode_c = mode.clone();
    let confirmed = tokio::spawn(async move { mode_c.show_extension_confirm("Title", "Continue?", None).await });
    tokio::task::yield_now().await;

    let custom_editor = Arc::new(CustomEditorStub(Mutex::new(String::new())));
    let custom_ret: Arc<dyn EditorComponent> = custom_editor.clone();
    let factory: EditorFactory = Arc::new(move |_, _, _| Arc::clone(&custom_ret));
    mode.set_custom_editor_component(Some(factory));

    assert!(visible(&mode).contains("Title"));
    mode.renderer().get_focused_component().unwrap().handle_input("\r");
    assert!(confirmed.await.unwrap());
    let focused = mode.renderer().get_focused_component().unwrap();
    assert!(Arc::ptr_eq(&focused, &mode.editor_container().children()[0]));
    assert!(strip_terminal_sequences(&focused.render(80).join("\n")).contains("custom editor"));
}
