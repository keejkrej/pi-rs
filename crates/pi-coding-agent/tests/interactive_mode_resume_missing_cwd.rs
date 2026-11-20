//! Port of packages/coding-agent/test/interactive-mode-resume-missing-cwd.test.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_ai::auth::types::{AuthInteraction, AuthType};
use pi_ai::{AssistantMessage, Model, ModelThinkingLevel};
use pi_coding_agent::core::session_cwd::{MissingSessionCwdError, SessionCwdIssue};
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
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result, Unsubscribe};
use pi_tui::terminal::{TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{
    KeybindingsManager, ScrollViewScrollbar, TUI_KEYBINDINGS, Terminal, WheelScrollLines, set_keybindings,
    strip_terminal_sequences,
};
use serde_json::Value;
use serial_test::serial;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

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

struct MockSessionManager {
    cwd: String,
    session_dir: String,
}

impl InteractiveSessionManager for MockSessionManager {
    fn get_cwd(&self) -> String {
        self.cwd.clone()
    }
    fn get_session_dir(&self) -> String {
        self.session_dir.clone()
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
        true
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

struct MockSession {
    session_manager: Arc<MockSessionManager>,
}

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
        self.session_manager.cwd.clone()
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
        self.session_manager.clone()
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
        Arc::new(MockSession {
            session_manager: self.session_manager.clone(),
        })
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

struct MockRuntimeHost {
    session: Arc<MockSession>,
    session_path: String,
    missing_cwd: String,
    current_cwd: String,
    switch_cancelled: bool,
    switch_calls: Mutex<Vec<(String, Option<String>)>>,
}

#[async_trait]
impl InteractiveRuntimeHost for MockRuntimeHost {
    fn session(&self) -> Arc<dyn InteractiveSession> {
        self.session.clone()
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
    async fn switch_session(
        &self,
        session_path: &str,
        options: Option<SwitchSessionOptions>,
    ) -> Result<CancelledResult> {
        let cwd_override = options.and_then(|o| o.cwd_override);
        self.switch_calls
            .lock()
            .unwrap()
            .push((session_path.to_string(), cwd_override.clone()));
        if session_path == self.session_path && cwd_override.is_none() {
            return Err(pi_js::Error::typed(
                "MissingSessionCwdError",
                MissingSessionCwdError::new(SessionCwdIssue {
                    session_file: Some(self.session_path.clone()),
                    session_cwd: self.missing_cwd.clone(),
                    fallback_cwd: self.current_cwd.clone(),
                }),
            ));
        }
        Ok(CancelledResult {
            cancelled: self.switch_cancelled,
        })
    }
    async fn import_from_jsonl(&self, _: &str, _: Option<&str>) -> Result<CancelledResult> {
        Ok(CancelledResult { cancelled: false })
    }
    async fn dispose(&self) {}
}

struct ResumeFixture {
    _guard: TestGuard,
    _temp: tempfile::TempDir,
    mode: InteractiveMode,
    host: Arc<MockRuntimeHost>,
    current_cwd: String,
    current_session_dir: String,
    session_path: String,
    original: String,
}

fn setup(switch_cancelled: bool) -> ResumeFixture {
    init_theme(Some("dark"), None);
    let temp = tempfile::tempdir().expect("tempdir");
    let current_cwd = temp.path().join("current");
    let old_session_dir = temp.path().join("old-sessions");
    let current_session_dir = temp.path().join("current-sessions");
    std::fs::create_dir_all(&current_cwd).expect("mkdir current");
    std::fs::create_dir_all(&old_session_dir).expect("mkdir old");
    std::fs::create_dir_all(&current_session_dir).expect("mkdir current sessions");

    let missing_cwd = current_cwd.join("renamed-away").to_string_lossy().into_owned();
    let current_cwd_str = current_cwd.to_string_lossy().into_owned();
    let current_session_dir_str = current_session_dir.to_string_lossy().into_owned();
    let session_path = old_session_dir.join("orphan.jsonl");
    let session_path_str = session_path.to_string_lossy().into_owned();
    let original = format!(
        "{}\n{}\n",
        pi_js::json::stringify(&serde_json::json!({
            "type": "session",
            "version": 3,
            "id": "orphan",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "cwd": missing_cwd
        })),
        pi_js::json::stringify(&serde_json::json!({
            "type": "message",
            "id": "m1",
            "parentId": null,
            "timestamp": "2026-01-01T00:00:00.000Z",
            "message": { "role": "user", "content": "hello", "timestamp": 1 }
        })),
    );
    std::fs::write(&session_path, &original).expect("write session.jsonl");

    let session = Arc::new(MockSession {
        session_manager: Arc::new(MockSessionManager {
            cwd: current_cwd_str.clone(),
            session_dir: current_session_dir_str.clone(),
        }),
    });
    let host = Arc::new(MockRuntimeHost {
        session,
        session_path: session_path_str.clone(),
        missing_cwd,
        current_cwd: current_cwd_str.clone(),
        switch_cancelled,
        switch_calls: Mutex::new(Vec::new()),
    });
    let mode = InteractiveMode::new(
        host.clone(),
        InteractiveModeOptions {
            terminal: Some(Arc::new(NoopTerminal)),
            hooks: Some(InteractiveModeHooks {
                exit_process: Arc::new(|_| {}),
                ..Default::default()
            }),
            ..Default::default()
        },
    );

    ResumeFixture {
        _guard: TestGuard,
        _temp: temp,
        mode,
        host,
        current_cwd: current_cwd_str,
        current_session_dir: current_session_dir_str,
        session_path: session_path_str,
        original,
    }
}

async fn choose_selector_option(mode: &InteractiveMode, down_steps: usize, cancel: bool) {
    for _ in 0..32 {
        let children = mode.editor_container().children();
        if let Some(comp) = children.first() {
            let txt = strip_terminal_sequences(&comp.render(120).join("\n"));
            if txt.contains("Continue in") && txt.contains("Fork into") {
                if cancel {
                    comp.handle_input("\x1b");
                } else {
                    for _ in 0..down_steps {
                        comp.handle_input("\x1b[B");
                    }
                    comp.handle_input("\r");
                }
                return;
            }
        }
        tokio::task::yield_now().await;
    }
    panic!("missing session cwd selector did not appear");
}

// #11060
#[tokio::test]
#[serial]
async fn forks_session_into_current_cwd_and_leaves_original_unchanged() {
    let fx = setup(false);
    let mode_c = fx.mode.clone();
    let path_c = fx.session_path.clone();
    let resume_task = tokio::spawn(async move { mode_c.handle_resume_session(&path_c).await });
    choose_selector_option(&fx.mode, 1, false).await;
    let res = resume_task.await.expect("join resume");
    assert!(!res.cancelled);

    let calls = fx.host.switch_calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    let fork_path = calls[1].0.clone();
    assert!(fork_path.starts_with(&fx.current_session_dir));
    let content = std::fs::read_to_string(&fork_path).expect("read fork");
    let lines: Vec<&str> = content.trim().lines().collect();
    let header: Value = pi_js::json::parse(lines[0]).expect("header");
    assert_eq!(header["type"], "session");
    assert_eq!(header["cwd"], fx.current_cwd);
    assert_eq!(header["parentSession"], fx.session_path);
    assert_ne!(header["id"], "orphan");
    let msg: Value = pi_js::json::parse(lines[1]).expect("entry");
    assert_eq!(msg["id"], "m1");
    assert_eq!(std::fs::read_to_string(&fx.session_path).unwrap(), fx.original);

    let chat = strip_terminal_sequences(&fx.mode.chat_container().render(400).join("\n"));
    let id = header["id"].as_str().expect("id");
    assert!(
        chat.contains(&format!("Forked into new session {id} in {}", fx.current_cwd)),
        "chat: {chat}"
    );
}

#[tokio::test]
#[serial]
async fn continues_original_session_in_current_cwd_without_writing_file() {
    let fx = setup(false);
    let mode_c = fx.mode.clone();
    let path_c = fx.session_path.clone();
    let resume_task = tokio::spawn(async move { mode_c.handle_resume_session(&path_c).await });
    choose_selector_option(&fx.mode, 0, false).await;
    let res = resume_task.await.expect("join resume");
    assert!(!res.cancelled);

    let calls = fx.host.switch_calls.lock().unwrap().clone();
    assert_eq!(
        calls.last(),
        Some(&(fx.session_path.clone(), Some(fx.current_cwd.clone())))
    );
    assert_eq!(std::fs::read_to_string(&fx.session_path).unwrap(), fx.original);
    assert_eq!(std::fs::read_dir(&fx.current_session_dir).unwrap().count(), 0);

    let chat = strip_terminal_sequences(&fx.mode.chat_container().render(400).join("\n"));
    assert!(
        chat.contains(&format!("Resumed session in {}", fx.current_cwd)),
        "chat: {chat}"
    );
}

#[tokio::test]
#[serial]
async fn removes_fork_when_extension_cancels_switch() {
    let fx = setup(true);
    let mode_c = fx.mode.clone();
    let path_c = fx.session_path.clone();
    let resume_task = tokio::spawn(async move { mode_c.handle_resume_session(&path_c).await });
    choose_selector_option(&fx.mode, 1, false).await;
    let res = resume_task.await.expect("join resume");
    assert!(res.cancelled);

    let calls = fx.host.switch_calls.lock().unwrap().clone();
    assert!(!std::path::Path::new(&calls[1].0).exists());
    assert_eq!(std::fs::read_dir(&fx.current_session_dir).unwrap().count(), 0);
}

#[tokio::test]
#[serial]
async fn creates_no_fork_when_user_cancels() {
    let fx = setup(false);
    let mode_c = fx.mode.clone();
    let path_c = fx.session_path.clone();
    let resume_task = tokio::spawn(async move { mode_c.handle_resume_session(&path_c).await });
    choose_selector_option(&fx.mode, 0, true).await;
    let res = resume_task.await.expect("join resume");
    assert!(res.cancelled);

    assert_eq!(fx.host.switch_calls.lock().unwrap().len(), 1);
    assert_eq!(std::fs::read_dir(&fx.current_session_dir).unwrap().count(), 0);
    let chat = strip_terminal_sequences(&fx.mode.chat_container().render(120).join("\n"));
    assert!(chat.contains("Resume cancelled"));
}
