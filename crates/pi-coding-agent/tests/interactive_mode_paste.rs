//! Port of packages/coding-agent/test/interactive-mode-paste.test.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_ai::auth::types::{AuthInteraction, AuthType};
use pi_ai::{AssistantMessage, Model, ModelThinkingLevel};
use pi_coding_agent::core::keybindings::KeybindingsManager;
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
use pi_coding_agent::modes::interactive::interactive_mode::EditorFactory;
use pi_coding_agent::modes::interactive::interactive_mode::{
    ActiveEditor, AgentSessionRuntimeDiagnostic, BashExecutionResult, CacheMissNotice, CacheWarmingStatusSummary,
    CancelledResult, ClearQueueResult, CompactionSummaryResult, CycleDirection, CycleModelResult, DoneFn,
    InteractiveExtensionBindings, InteractiveExtensionRunner, InteractiveMode, InteractiveModeOptions,
    InteractiveModelRuntime, InteractivePromptOptions, InteractiveResourceLoader, InteractiveRuntimeHost,
    InteractiveSession, InteractiveSessionManager, InteractiveSettingsManager, LoadedExtensionsResult,
    LoadedThemeResource, NavigateTreeOptions, NavigateTreeResult, PromptTemplateInfo, ProviderAuthInfo,
    ProviderAuthStatusInfo, RegisteredExtensionCommand, RegisteredExtensionShortcut, ResourceDiagnostic,
    RuntimeForkOptions, RuntimeForkResult, SessionStats, SkillInfo, SourceInfo, StoredCredentialInfo,
    SwitchSessionOptions, UsageBreakdownEntry, sync_tui_keybindings,
};
use pi_coding_agent::modes::interactive::model_catalog_refresh::{ModelCatalogRefreshResult, ModelCatalogRuntime};
use pi_coding_agent::modes::interactive::session_share::{SessionShareDestination, ShareSession};
use pi_coding_agent::modes::interactive::theme::theme::{Theme, init_theme};
use pi_coding_agent::modes::interactive::theme::theme_controller::ThemeSettingsSource;
use pi_coding_agent::modes::json_event::AgentSessionEvent;
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Error, Result, Unsubscribe};
use pi_tui::autocomplete::AutocompleteProvider;
use pi_tui::editor_component::{EditorBorderColor, EditorTextCallback};
use pi_tui::terminal::{TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{
    Component, Editor, EditorComponent, KeybindingsManager as TuiKeybindingsManager, ScrollViewScrollbar,
    TUI_KEYBINDINGS, Terminal, Text, WheelScrollLines, set_keybindings,
};
use serial_test::serial;

const COLLAPSED_PASTE_LINE_COUNT: usize = 12;

fn pasted_text(label: &str) -> String {
    (1..=COLLAPSED_PASTE_LINE_COUNT)
        .map(|i| format!("{label} {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn paste(editor: &ActiveEditor, text: &str) {
    editor.handle_input(&format!("\x1b[200~{text}\x1b[201~"));
    assert_ne!(editor.get_text(), text);
}

fn submit(editor: &ActiveEditor) -> Option<String> {
    let submitted = Arc::new(Mutex::new(None::<String>));
    let sub_c = Arc::clone(&submitted);
    editor.set_on_submit(Some(Arc::new(move |txt| {
        *sub_c.lock().unwrap() = Some(txt.to_string());
    })));
    editor.handle_input("\r");
    let out = submitted.lock().unwrap().clone();
    out
}

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(TuiKeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

struct NoopTerminal;
#[async_trait]
impl Terminal for NoopTerminal {
    fn start(&self, _: TerminalInputHandler, _: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _: Option<i64>, _: Option<i64>) {}
    fn write(&self, _: &str) {}
    fn columns(&self) -> i64 {
        120
    }
    fn rows(&self) -> i64 {
        40
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

struct MockSession {
    follow_up: Mutex<Vec<String>>,
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
        Arc::new(MockSession {
            follow_up: Mutex::new(Vec::new()),
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
        self.follow_up.lock().unwrap().clone()
    }
    fn clear_queue(&self) -> ClearQueueResult {
        ClearQueueResult {
            steering: Vec::new(),
            follow_up: std::mem::take(&mut *self.follow_up.lock().unwrap()),
        }
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

struct MockRuntimeHost(Arc<MockSession>);
#[async_trait]
impl InteractiveRuntimeHost for MockRuntimeHost {
    fn session(&self) -> Arc<dyn InteractiveSession> {
        self.0.clone()
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

fn setup(follow_up: Vec<String>) -> (TestGuard, InteractiveMode) {
    init_theme(Some("dark"), None);
    let kb = KeybindingsManager::new(None, None);
    sync_tui_keybindings(&kb);
    let session = Arc::new(MockSession {
        follow_up: Mutex::new(follow_up),
    });
    let mode = InteractiveMode::new(
        Arc::new(MockRuntimeHost(session)),
        InteractiveModeOptions {
            terminal: Some(Arc::new(NoopTerminal)),
            ..Default::default()
        },
    );
    (TestGuard, mode)
}

struct ReplacementEditor(Editor);

impl Component for ReplacementEditor {
    fn render(&self, width: usize) -> Vec<String> {
        self.0.render(width)
    }
    fn handle_input(&self, data: &str) {
        self.0.handle_input(data);
    }
    fn invalidate(&self) {
        self.0.invalidate();
    }
}

impl EditorComponent for ReplacementEditor {
    fn get_text(&self) -> String {
        self.0.get_text()
    }
    fn set_text(&self, text: &str) {
        self.0.set_text(text);
    }
    fn get_expanded_text(&self) -> String {
        self.0.get_expanded_text()
    }
    fn add_to_history(&self, text: &str) {
        self.0.add_to_history(text);
    }
    fn insert_text_at_cursor(&self, text: &str) {
        self.0.insert_text_at_cursor(text);
    }
    fn set_autocomplete_provider(&self, provider: Arc<dyn AutocompleteProvider>) {
        self.0.set_autocomplete_provider(provider);
    }
    fn border_color(&self) -> Option<EditorBorderColor> {
        Some(self.0.border_color())
    }
    fn set_border_color(&self, border_color: Option<EditorBorderColor>) {
        if let Some(bc) = border_color {
            self.0.set_border_color(bc);
        }
    }
    fn set_padding_x(&self, padding: f64) {
        self.0.set_padding_x(padding);
    }
    fn set_autocomplete_max_visible(&self, max_visible: f64) {
        self.0.set_autocomplete_max_visible(max_visible);
    }
    fn on_submit(&self) -> Option<EditorTextCallback> {
        self.0.on_submit()
    }
    fn set_on_submit(&self, on_submit: Option<EditorTextCallback>) {
        self.0.set_on_submit(on_submit);
    }
    fn on_change(&self) -> Option<EditorTextCallback> {
        self.0.on_change()
    }
    fn set_on_change(&self, on_change: Option<EditorTextCallback>) {
        self.0.set_on_change(on_change);
    }
}

// #9809
#[test]
#[serial]
fn preserves_collapsed_paste_content_between_default_and_replacement_editor() {
    let (_guard, mode) = setup(Vec::new());
    let into_replacement = pasted_text("into replacement");
    paste(&mode.active_editor(), &into_replacement);

    let factory: EditorFactory = Arc::new(|tui, theme, _kb| Arc::new(ReplacementEditor(Editor::new(tui, theme, None))));
    mode.set_custom_editor_component(Some(factory));
    assert_eq!(
        submit(&mode.active_editor()).as_deref(),
        Some(into_replacement.as_str())
    );

    let back_to_default = pasted_text("back to default");
    paste(&mode.active_editor(), &back_to_default);
    mode.set_custom_editor_component(None);
    assert_eq!(submit(&mode.active_editor()).as_deref(), Some(back_to_default.as_str()));
}

#[tokio::test]
#[serial]
async fn preserves_collapsed_paste_content_around_non_overlay_custom_ui_close_and_failure() {
    let (_guard, mode) = setup(Vec::new());
    let draft = pasted_text("draft");
    paste(&mode.active_editor(), &draft);

    let comp: Arc<dyn Component> = Arc::new(Text::new(Some("panel"), Some(0), Some(0), None));
    let comp_c = Arc::clone(&comp);
    let close_slot: Arc<Mutex<Option<DoneFn>>> = Arc::new(Mutex::new(None));
    let close_c = Arc::clone(&close_slot);
    let mode_c = mode.clone();
    let shown = tokio::spawn(async move {
        mode_c
            .show_extension_custom(
                Box::new(move |_, _, _, done| {
                    let comp_c = Arc::clone(&comp_c);
                    let close_c = Arc::clone(&close_c);
                    Box::pin(async move {
                        *close_c.lock().unwrap() = Some(done);
                        Ok(comp_c)
                    })
                }),
                None,
            )
            .await
            .unwrap()
    });
    tokio::task::yield_now().await;
    assert!(Arc::ptr_eq(&mode.editor_container().children()[0], &comp));

    close_slot.lock().unwrap().take().unwrap()(serde_json::json!("closed"));
    assert_eq!(shown.await.unwrap(), serde_json::json!("closed"));
    assert_eq!(submit(&mode.active_editor()).as_deref(), Some(draft.as_str()));

    let draft2 = pasted_text("draft2");
    paste(&mode.active_editor(), &draft2);
    let err = mode
        .show_extension_custom(
            Box::new(|_, _, _, _| Box::pin(async { Err(Error::msg("factory failed")) })),
            None,
        )
        .await
        .expect_err("expected failure");
    assert!(err.to_string().contains("factory failed"));
    assert_eq!(submit(&mode.active_editor()).as_deref(), Some(draft2.as_str()));
}

#[test]
#[serial]
fn preserves_collapsed_paste_content_when_queued_messages_are_restored_above_it() {
    let (_guard, mode) = setup(vec!["queued follow-up".to_string()]);
    let draft = pasted_text("draft");
    paste(&mode.active_editor(), &draft);

    assert_eq!(mode.restore_queued_messages_to_editor(false), 1);
    assert_eq!(
        submit(&mode.active_editor()).as_deref(),
        Some(format!("queued follow-up\n\n{draft}").as_str())
    );
}
