//! Port of packages/coding-agent/test/interactive-mode-import-command.test.ts

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
use pi_js::Error;
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result, Unsubscribe};
use pi_tui::terminal::{TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{ScrollViewScrollbar, Terminal, WheelScrollLines, strip_terminal_sequences};
use serial_test::serial;

struct NoopTerminal;
#[async_trait]
impl Terminal for NoopTerminal {
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
        true
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
        0
    }
    fn set_output_pad(&self, _pad: i64) {}
    fn get_enable_skill_commands(&self) -> bool {
        true
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
        QuietStartup::False
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
        None
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
        0
    }
    fn set_http_idle_timeout_ms(&self, _timeout_ms: u64) {}
    fn get_cache_warming_mode(&self) -> CacheWarmingMode {
        CacheWarmingMode::Off
    }
    fn set_cache_warming_mode(&self, _mode: CacheWarmingMode) {}
    fn get_transport(&self) -> pi_ai::Transport {
        pi_ai::Transport::Sse
    }
    fn set_transport(&self, _transport: pi_ai::Transport) {}
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
        "device-1".to_string()
    }
    fn is_project_trusted(&self) -> bool {
        true
    }
}

struct MockSessionManager;
impl InteractiveSessionManager for MockSessionManager {
    fn get_cwd(&self) -> String {
        "/tmp/project".to_string()
    }
    fn get_session_dir(&self) -> String {
        "/tmp/sessions".to_string()
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

struct MockModelRuntime;
impl ModelCatalogRuntime for MockModelRuntime {
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
impl InteractiveModelRuntime for MockModelRuntime {
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
        "/tmp/project".to_string()
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
    fn is_using_subscription(&self, _provider: &str) -> bool {
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
    fn set_transport(&self, _transport: pi_ai::Transport) {}
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
    fn subscribe(&self, _listener: Arc<dyn Fn(AgentSessionEvent) -> BoxFuture<()> + Send + Sync>) -> Unsubscribe {
        Unsubscribe::noop()
    }
    async fn bind_extensions(&self, _bindings: InteractiveExtensionBindings) -> Result<()> {
        Ok(())
    }
    async fn reload(&self, _before_session_start: Option<Arc<dyn Fn() + Send + Sync>>) -> Result<()> {
        Ok(())
    }
    async fn compact(&self, _custom_instructions: Option<&str>) -> Result<CompactionSummaryResult> {
        Ok(CompactionSummaryResult {
            summary: String::new(),
            tokens_before: 0,
            usage: None,
        })
    }
    async fn navigate_tree(&self, _target_id: &str, _options: NavigateTreeOptions) -> Result<NavigateTreeResult> {
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

struct MockRuntimeHost {
    import_calls: Mutex<Vec<String>>,
    import_error: Option<String>,
    exit_calls: Mutex<Vec<i32>>,
}

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
    fn set_before_session_invalidate(&self, _cb: Arc<dyn Fn() + Send + Sync>) {}
    fn set_rebind_session(&self, _cb: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>) {}
    async fn new_session(&self) -> Result<CancelledResult> {
        Ok(CancelledResult { cancelled: false })
    }
    async fn fork(&self, _entry_id: &str, _options: Option<RuntimeForkOptions>) -> Result<RuntimeForkResult> {
        Ok(RuntimeForkResult::default())
    }
    async fn switch_session(
        &self,
        _session_path: &str,
        _options: Option<SwitchSessionOptions>,
    ) -> Result<CancelledResult> {
        Ok(CancelledResult { cancelled: false })
    }
    async fn import_from_jsonl(&self, input_path: &str, _cwd_override: Option<&str>) -> Result<CancelledResult> {
        self.import_calls.lock().unwrap().push(input_path.to_string());
        if let Some(ref err) = self.import_error {
            return Err(Error::msg(err.clone()));
        }
        Ok(CancelledResult { cancelled: false })
    }
    async fn dispose(&self) {}
}

fn build_mode(import_error: Option<&str>) -> (InteractiveMode, Arc<MockRuntimeHost>) {
    let host = Arc::new(MockRuntimeHost {
        import_calls: Mutex::new(Vec::new()),
        import_error: import_error.map(str::to_string),
        exit_calls: Mutex::new(Vec::new()),
    });
    let h = Arc::clone(&host);
    let hooks = InteractiveModeHooks {
        exit_process: Arc::new(move |code| h.exit_calls.lock().unwrap().push(code)),
        ..Default::default()
    };
    let mode = InteractiveMode::new(
        host.clone(),
        InteractiveModeOptions {
            terminal: Some(Arc::new(NoopTerminal)),
            hooks: Some(hooks),
            ..Default::default()
        },
    );
    (mode, host)
}

async fn run_import_and_confirm(mode: &InteractiveMode, command_text: &str) {
    let m = mode.clone();
    let cmd = command_text.to_string();
    let handle = tokio::spawn(async move {
        m.handle_import_command(&cmd).await;
    });
    for _ in 0..200 {
        if let Some(focused) = mode.renderer().get_focused_component() {
            let rendered = strip_terminal_sequences(&focused.render(120).join("\n"));
            if rendered.contains("Import session") {
                focused.handle_input("\r");
                break;
            }
        }
        tokio::task::yield_now().await;
    }
    handle.await.expect("handle_import_command task completes");
}

#[test]
#[serial]
fn strips_quotes_from_import_path_arguments() {
    init_theme(Some("dark"), None);
    let (mode, _) = build_mode(None);
    assert_eq!(
        mode.get_path_command_argument("/import \"path/to/session.jsonl\"", "/import")
            .as_deref(),
        Some("path/to/session.jsonl")
    );
    assert_eq!(
        mode.get_path_command_argument("/import \"path with spaces/session.jsonl\"", "/import")
            .as_deref(),
        Some("path with spaces/session.jsonl")
    );
}

#[test]
#[serial]
fn preserves_apostrophes_in_unquoted_import_path_arguments() {
    init_theme(Some("dark"), None);
    let (mode, _) = build_mode(None);
    assert_eq!(
        mode.get_path_command_argument("/import john's/session.jsonl", "/import")
            .as_deref(),
        Some("john's/session.jsonl")
    );
}

#[test]
#[serial]
fn enforces_command_token_boundaries() {
    init_theme(Some("dark"), None);
    let (mode, _) = build_mode(None);
    assert_eq!(
        mode.get_path_command_argument("/important /tmp/session.jsonl", "/import"),
        None
    );
    assert_eq!(mode.get_path_command_argument("/exporter out.html", "/export"), None);
    assert_eq!(
        mode.get_path_command_argument("/import /tmp/session.jsonl", "/import")
            .as_deref(),
        Some("/tmp/session.jsonl")
    );
}

#[tokio::test]
#[serial]
async fn passes_unquoted_path_to_runtime_host_import_from_jsonl() {
    init_theme(Some("dark"), None);
    let (mode, host) = build_mode(None);

    run_import_and_confirm(&mode, "/import \"path/to/session.jsonl\"").await;

    assert_eq!(
        *host.import_calls.lock().unwrap(),
        vec!["path/to/session.jsonl".to_string()]
    );
    let output = strip_terminal_sequences(&mode.chat_container().render(120).join("\n"));
    assert!(
        output.contains("Session imported from: path/to/session.jsonl"),
        "got: {output:?}"
    );
}

#[tokio::test]
#[serial]
async fn passes_unquoted_apostrophe_path_to_runtime_host_import_from_jsonl_unchanged() {
    init_theme(Some("dark"), None);
    let (mode, host) = build_mode(None);

    run_import_and_confirm(&mode, "/import john's/session.jsonl").await;

    assert_eq!(
        *host.import_calls.lock().unwrap(),
        vec!["john's/session.jsonl".to_string()]
    );
    let output = strip_terminal_sequences(&mode.chat_container().render(120).join("\n"));
    assert!(
        output.contains("Session imported from: john's/session.jsonl"),
        "got: {output:?}"
    );
}

#[tokio::test]
#[serial]
async fn shows_a_non_fatal_error_when_import_path_does_not_exist() {
    init_theme(Some("dark"), None);
    let (mode, host) = build_mode(Some("File not found: /tmp/missing-session.jsonl"));

    run_import_and_confirm(&mode, "/import /tmp/missing-session.jsonl").await;

    assert!(host.exit_calls.lock().unwrap().is_empty());
    let output = strip_terminal_sequences(&mode.chat_container().render(120).join("\n"));
    assert!(
        output.contains("Failed to import session: File not found: /tmp/missing-session.jsonl"),
        "got: {output:?}"
    );
}
