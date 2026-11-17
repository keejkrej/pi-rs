//! Port of packages/coding-agent/test/interactive-tui.test.ts

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
use pi_coding_agent::modes::interactive::components::status_indicator::{
    CompactionStatusReason, StatusIndicator, StatusIndicatorKind,
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
use pi_coding_agent::modes::interactive::tui_renderer::{
    InteractiveTuiMode, InteractiveTuiOptions, create_interactive_tui,
};
use pi_coding_agent::modes::json_event::AgentSessionEvent;
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result, Unsubscribe};
use pi_tui::autocomplete::AutocompleteProvider;
use pi_tui::components::scroll_view::ScrollViewFollow;
use pi_tui::editor_component::{EditorBorderColor, EditorComponent, EditorTextCallback};
use pi_tui::keybindings::KeyIdOrList;
use pi_tui::terminal::{TerminalInputHandler, TerminalResizeHandler};
use pi_tui::tui::{TuiMode as PiTuiMode, TuiMouseEvent, TuiMouseEventResult, is_viewport_tui};
use pi_tui::{
    Component, Focusable, KeybindingsConfig, KeybindingsManager, ScrollView, ScrollViewOptions, ScrollViewScrollbar,
    TUI_KEYBINDINGS, Terminal, Text, WheelScrollLines, set_keybindings, strip_terminal_sequences,
};
use serial_test::serial;
use tokio::sync::Notify;

struct TestGuard;
impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

struct RecordingTerminal {
    cols: i64,
    rows: i64,
    writes: Mutex<Vec<String>>,
    start_count: AtomicUsize,
    stop_count: AtomicUsize,
    on_input: Mutex<Option<TerminalInputHandler>>,
    render_notify: Notify,
}

impl RecordingTerminal {
    fn new(cols: i64, rows: i64) -> Arc<Self> {
        Arc::new(Self {
            cols,
            rows,
            writes: Mutex::new(Vec::new()),
            start_count: AtomicUsize::new(0),
            stop_count: AtomicUsize::new(0),
            on_input: Mutex::new(None),
            render_notify: Notify::new(),
        })
    }

    fn send_input(&self, data: &str) {
        let cb = self.on_input.lock().unwrap().clone();
        if let Some(f) = cb {
            f(data);
        }
    }

    async fn wait_for_render(&self) {
        tokio::select! {
            _ = self.render_notify.notified() => {}
            _ = tokio::time::sleep(Duration::from_millis(80)) => {}
        }
    }

    fn writes(&self) -> Vec<String> {
        self.writes.lock().unwrap().clone()
    }
}

#[async_trait]
impl Terminal for RecordingTerminal {
    fn start(&self, on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {
        self.start_count.fetch_add(1, Ordering::SeqCst);
        *self.on_input.lock().unwrap() = Some(on_input);
    }
    fn stop(&self) {
        self.stop_count.fetch_add(1, Ordering::SeqCst);
        *self.on_input.lock().unwrap() = None;
    }
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, data: &str) {
        self.writes.lock().unwrap().push(data.to_string());
        self.render_notify.notify_waiters();
    }
    fn columns(&self) -> i64 {
        self.cols
    }
    fn rows(&self) -> i64 {
        self.rows
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

struct MockSettings {
    tui_mode: Mutex<TuiMode>,
    copy_on_select: AtomicBool,
    clear_on_shrink: AtomicBool,
}
impl MockSettings {
    fn new(mode: TuiMode, copy_on_select: bool) -> Arc<Self> {
        Arc::new(Self {
            tui_mode: Mutex::new(mode),
            copy_on_select: AtomicBool::new(copy_on_select),
            clear_on_shrink: AtomicBool::new(false),
        })
    }
}
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
        *self.tui_mode.lock().unwrap()
    }
    fn set_tui_mode(&self, mode: TuiMode) {
        *self.tui_mode.lock().unwrap() = mode;
    }
    fn get_show_hardware_cursor(&self) -> bool {
        false
    }
    fn set_show_hardware_cursor(&self, _enabled: bool) {}
    fn get_clear_on_shrink(&self) -> bool {
        self.clear_on_shrink.load(Ordering::SeqCst)
    }
    fn set_clear_on_shrink(&self, enabled: bool) {
        self.clear_on_shrink.store(enabled, Ordering::SeqCst);
    }
    fn get_fullscreen_copy_on_select(&self) -> bool {
        self.copy_on_select.load(Ordering::SeqCst)
    }
    fn set_fullscreen_copy_on_select(&self, enabled: bool) {
        self.copy_on_select.store(enabled, Ordering::SeqCst);
    }
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

struct MockSession {
    settings: Arc<MockSettings>,
    last_assistant_text: Option<String>,
    last_assistant_calls: Arc<AtomicUsize>,
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
        self.settings.clone()
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
            settings: self.settings.clone(),
            last_assistant_text: self.last_assistant_text.clone(),
            last_assistant_calls: self.last_assistant_calls.clone(),
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
        self.last_assistant_calls.fetch_add(1, Ordering::SeqCst);
        self.last_assistant_text.clone()
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
    session: Arc<MockSession>,
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
    async fn import_from_jsonl(&self, _input_path: &str, _cwd_override: Option<&str>) -> Result<CancelledResult> {
        Ok(CancelledResult { cancelled: false })
    }
    async fn dispose(&self) {}
}

fn build_mode_with(
    tui_mode: TuiMode,
    copy_on_select: bool,
    terminal: Arc<RecordingTerminal>,
    hooks: InteractiveModeHooks,
) -> (InteractiveMode, Arc<AtomicUsize>) {
    let assistant_calls = Arc::new(AtomicUsize::new(0));
    let settings = MockSettings::new(tui_mode, copy_on_select);
    let session = Arc::new(MockSession {
        settings,
        last_assistant_text: Some("assistant response".to_string()),
        last_assistant_calls: Arc::clone(&assistant_calls),
    });
    let mode = InteractiveMode::new(
        Arc::new(MockRuntimeHost { session }),
        InteractiveModeOptions {
            tui_mode: Some(tui_mode),
            terminal: Some(terminal),
            hooks: Some(hooks),
            ..Default::default()
        },
    );
    (mode, assistant_calls)
}

struct RecordingComponent {
    focused: AtomicBool,
    inputs: Mutex<Vec<String>>,
    invalidated_modes: Mutex<Vec<PiTuiMode>>,
    ui: Option<Arc<dyn pi_tui::TUI + Send + Sync>>,
}

impl RecordingComponent {
    fn new(ui: Option<Arc<dyn pi_tui::TUI + Send + Sync>>) -> Arc<Self> {
        Arc::new(Self {
            focused: AtomicBool::new(false),
            inputs: Mutex::new(Vec::new()),
            invalidated_modes: Mutex::new(Vec::new()),
            ui,
        })
    }
}

impl Focusable for RecordingComponent {
    fn focused(&self) -> bool {
        self.focused.load(Ordering::SeqCst)
    }
    fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::SeqCst);
    }
}

impl Component for RecordingComponent {
    fn render(&self, _width: usize) -> Vec<String> {
        vec!["content".to_string()]
    }
    fn handle_input(&self, data: &str) {
        self.inputs.lock().unwrap().push(data.to_string());
    }
    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }
    fn wants_key_release(&self) -> bool {
        false
    }
    fn invalidate(&self) {
        if let Some(ref ui) = self.ui {
            self.invalidated_modes.lock().unwrap().push(ui.mode());
        }
    }
    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
    }
}

struct StubCustomEditor;
impl Component for StubCustomEditor {
    fn render(&self, _width: usize) -> Vec<String> {
        vec!["custom".to_string()]
    }
    fn handle_input(&self, _data: &str) {}
    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }
    fn wants_key_release(&self) -> bool {
        false
    }
    fn invalidate(&self) {}
}
impl EditorComponent for StubCustomEditor {
    fn get_text(&self) -> String {
        String::new()
    }
    fn set_text(&self, _text: &str) {}
    fn add_to_history(&self, _text: &str) {}
    fn insert_text_at_cursor(&self, _text: &str) {}
    fn get_expanded_text(&self) -> String {
        String::new()
    }
    fn on_submit(&self) -> Option<EditorTextCallback> {
        None
    }
    fn set_on_submit(&self, _handler: Option<EditorTextCallback>) {}
    fn on_change(&self) -> Option<EditorTextCallback> {
        None
    }
    fn set_on_change(&self, _handler: Option<EditorTextCallback>) {}
    fn border_color(&self) -> Option<EditorBorderColor> {
        None
    }
    fn set_border_color(&self, _color: Option<EditorBorderColor>) {}
    fn set_autocomplete_provider(&self, _provider: Arc<dyn AutocompleteProvider>) {}
    fn set_padding_x(&self, _padding_x: f64) {}
    fn set_autocomplete_max_visible(&self, _max_visible: f64) {}
}

#[tokio::test]
#[serial]
async fn selects_the_alternate_screen_renderer_only_when_requested() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let main_term = RecordingTerminal::new(80, 24);
    let main_tui = create_interactive_tui(InteractiveTuiOptions {
        tui_mode: Some(InteractiveTuiMode::Regular),
        show_hardware_cursor: false,
        log_directory: "/tmp".to_string(),
        terminal: Some(main_term.clone()),
        ..Default::default()
    });
    assert_eq!(main_tui.as_tui_arc().mode(), PiTuiMode::Regular);
    assert!(!is_viewport_tui(&*main_tui.as_tui_arc()));
    main_tui.as_tui_arc().start();
    main_term.wait_for_render().await;
    assert!(!main_term.writes().iter().any(|w| w.contains("\x1b[?1049h")));
    main_tui.as_tui_arc().stop(None);

    let alt_term = RecordingTerminal::new(80, 24);
    let alt_tui = create_interactive_tui(InteractiveTuiOptions {
        tui_mode: Some(InteractiveTuiMode::Fullscreen),
        show_hardware_cursor: false,
        log_directory: "/tmp".to_string(),
        terminal: Some(alt_term.clone()),
        ..Default::default()
    });
    assert_eq!(alt_tui.as_tui_arc().mode(), PiTuiMode::Fullscreen);
    assert!(is_viewport_tui(&*alt_tui.as_tui_arc()));
    alt_tui.as_tui_arc().start();
    alt_term.wait_for_render().await;
    assert!(alt_term.writes().iter().any(|w| w.contains("\x1b[?1049h")));
    alt_tui.as_tui_arc().stop(None);
}

#[tokio::test]
#[serial]
async fn shows_the_configured_jump_to_bottom_shortcut_while_scrolled_up() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);
    let mut cfg = KeybindingsConfig::new();
    cfg.insert(
        "tui.altScreen.bottom".to_string(),
        Some(KeyIdOrList::One("ctrl+j".to_string())),
    );
    set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), Some(cfg)));

    let terminal = RecordingTerminal::new(50, 4);
    let ui = create_interactive_tui(InteractiveTuiOptions {
        tui_mode: Some(InteractiveTuiMode::Fullscreen),
        show_hardware_cursor: false,
        log_directory: "/tmp".to_string(),
        terminal: Some(terminal.clone()),
        ..Default::default()
    });
    let lines: Vec<String> = (1..=8).map(|i| format!("line {i}")).collect();
    let scroll = Arc::new(ScrollView::new(
        Arc::new(Text::new(Some(&lines.join("\n")), Some(0), Some(0), None)),
        Some(ScrollViewOptions {
            follow: Some(ScrollViewFollow::End),
            primary: Some(true),
            ..Default::default()
        }),
    ));
    let alt = ui.as_alt_screen().expect("alt screen").clone();
    alt.set_layout_root(Some(scroll));
    alt.start();
    terminal.wait_for_render().await;
    terminal.send_input("\x1b[<64;1;1M");
    terminal.wait_for_render().await;
    alt.render_now(None);

    let row3 = strip_terminal_sequences(alt.get_screen_lines().get(3).map(String::as_str).unwrap_or(""));
    alt.stop(None);
    assert!(row3.contains("↓ Jump to latest message · Ctrl+J"), "row3 was: {row3:?}");
}

#[tokio::test]
#[serial]
async fn replaces_the_renderer_and_restores_the_previous_screen_for_resume_hint_exits() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);
    let terminal = RecordingTerminal::new(40, 8);
    let (mode, _) = build_mode_with(
        TuiMode::Regular,
        true,
        terminal.clone(),
        InteractiveModeHooks::default(),
    );

    let comp = RecordingComponent::new(Some(mode.ui()));
    mode.renderer().as_tui_arc().add_child(comp.clone());
    mode.renderer().as_tui_arc().set_focus(Some(comp.clone()));

    mode.ui().start();
    terminal.wait_for_render().await;
    assert!(mode.switch_tui_mode(TuiMode::Fullscreen, false, true));
    terminal.wait_for_render().await;

    assert_eq!(mode.ui().mode(), PiTuiMode::Fullscreen);
    let children = mode.renderer().as_tui_arc().children();
    assert_eq!(children.len(), 1);
    assert!(Arc::ptr_eq(&children[0], &(comp.clone() as Arc<dyn Component>)));
    let focused = mode.renderer().get_focused_component().expect("focused component");
    assert!(Arc::ptr_eq(&focused, &(comp.clone() as Arc<dyn Component>)));
    assert!(comp.focused());
    assert_eq!(*comp.invalidated_modes.lock().unwrap(), vec![PiTuiMode::Fullscreen]);
    assert_eq!(terminal.start_count.load(Ordering::SeqCst), 2);
    assert_eq!(terminal.stop_count.load(Ordering::SeqCst), 1);

    mode.stop_interactive_tui(FullscreenExitOutput::ResumeHint);
    assert_eq!(mode.ui().mode(), PiTuiMode::Fullscreen);
    assert_eq!(terminal.start_count.load(Ordering::SeqCst), 2);
    assert_eq!(terminal.stop_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
#[serial]
async fn feeds_clipboard_text_to_the_focused_component_as_a_bracketed_paste() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);
    let terminal = RecordingTerminal::new(80, 24);
    let (mode, _) = build_mode_with(
        TuiMode::Regular,
        true,
        terminal,
        InteractiveModeHooks {
            clipboard_read_text: Arc::new(|| Box::pin(async { Ok(Some("clipboard text".to_string())) })),
            ..Default::default()
        },
    );
    let comp = RecordingComponent::new(None);
    mode.renderer().as_tui_arc().set_focus(Some(comp.clone()));

    mode.handle_right_click_paste().await;

    assert_eq!(
        *comp.inputs.lock().unwrap(),
        vec!["\x1b[200~clipboard text\x1b[201~".to_string()]
    );
}

#[tokio::test]
#[serial]
async fn copies_an_active_fullscreen_selection_when_copy_on_select_is_disabled() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);
    let terminal = RecordingTerminal::new(40, 4);
    let copied: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let copied_ref = Arc::clone(&copied);
    let (mode, assistant_calls) = build_mode_with(
        TuiMode::Fullscreen,
        false,
        terminal.clone(),
        InteractiveModeHooks {
            clipboard_copy: Arc::new(move |text: &str| {
                let text = text.to_string();
                let copied_c = Arc::clone(&copied_ref);
                Box::pin(async move {
                    copied_c.lock().unwrap().push(text);
                    Ok(())
                })
            }),
            ..Default::default()
        },
    );

    let alt = mode.renderer().as_alt_screen().expect("alt").clone();
    alt.add_child(Arc::new(Text::new(
        Some("alpha\nbeta\ngamma\ndelta"),
        Some(0),
        Some(0),
        None,
    )));
    alt.start();
    terminal.wait_for_render().await;
    alt.render_now(Some(true));

    terminal.send_input("\x1b[<0;1;1M");
    terminal.send_input("\x1b[<32;4;2M");
    terminal.send_input("\x1b[<0;4;2m");
    terminal.wait_for_render().await;
    alt.render_now(None);
    copied.lock().unwrap().clear();

    mode.handle_copy_command(true, true).await;
    alt.render_now(None);

    let viewport = alt.get_screen_lines();
    alt.stop(None);

    assert_eq!(*copied.lock().unwrap(), vec!["alpha\nbeta".to_string()]);
    assert_eq!(assistant_calls.load(Ordering::SeqCst), 0);
    assert!(
        viewport.iter().any(|l| strip_terminal_sequences(l).contains("Copied!")),
        "viewport missing Copied!: {viewport:?}"
    );
}

#[tokio::test]
#[serial]
async fn copies_the_last_assistant_message_with_an_active_fullscreen_selection_when_copy_on_select_is_enabled() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);
    let terminal = RecordingTerminal::new(40, 4);
    let copied: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let copied_ref = Arc::clone(&copied);
    let (mode, assistant_calls) = build_mode_with(
        TuiMode::Fullscreen,
        true,
        terminal.clone(),
        InteractiveModeHooks {
            clipboard_copy: Arc::new(move |text: &str| {
                let text = text.to_string();
                let copied_c = Arc::clone(&copied_ref);
                Box::pin(async move {
                    copied_c.lock().unwrap().push(text);
                    Ok(())
                })
            }),
            ..Default::default()
        },
    );

    let alt = mode.renderer().as_alt_screen().expect("alt").clone();
    alt.add_child(Arc::new(Text::new(
        Some("alpha\nbeta\ngamma\ndelta"),
        Some(0),
        Some(0),
        None,
    )));
    alt.start();
    terminal.wait_for_render().await;
    alt.render_now(Some(true));

    terminal.send_input("\x1b[<0;1;1M");
    terminal.send_input("\x1b[<32;4;2M");
    terminal.send_input("\x1b[<0;4;2m");
    terminal.wait_for_render().await;
    alt.render_now(None);
    copied.lock().unwrap().clear();

    mode.handle_copy_command(true, true).await;
    alt.render_now(None);

    let viewport = alt.get_screen_lines();
    alt.stop(None);

    assert_eq!(*copied.lock().unwrap(), vec!["assistant response".to_string()]);
    assert_eq!(assistant_calls.load(Ordering::SeqCst), 1);
    assert!(
        viewport.iter().any(|l| strip_terminal_sequences(l).contains("Copied!")),
        "viewport missing Copied!: {viewport:?}"
    );
}

#[tokio::test]
#[serial]
async fn flashes_copied_for_the_copy_shortcut_in_fullscreen_mode() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);
    let terminal = RecordingTerminal::new(40, 4);
    let copied: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let copied_ref = Arc::clone(&copied);
    let (mode, _) = build_mode_with(
        TuiMode::Fullscreen,
        true,
        terminal.clone(),
        InteractiveModeHooks {
            clipboard_copy: Arc::new(move |text: &str| {
                let text = text.to_string();
                let copied_c = Arc::clone(&copied_ref);
                Box::pin(async move {
                    copied_c.lock().unwrap().push(text);
                    Ok(())
                })
            }),
            ..Default::default()
        },
    );

    let alt = mode.renderer().as_alt_screen().expect("alt").clone();
    alt.start();
    terminal.wait_for_render().await;

    mode.handle_copy_command(true, true).await;
    alt.render_now(None);

    let viewport = alt.get_screen_lines();
    alt.stop(None);

    assert_eq!(*copied.lock().unwrap(), vec!["assistant response".to_string()]);
    assert!(
        viewport.iter().any(|l| strip_terminal_sequences(l).contains("Copied!")),
        "viewport missing Copied!: {viewport:?}"
    );
}

#[tokio::test]
#[serial]
async fn keeps_the_status_line_confirmation_for_the_copy_shortcut_in_regular_mode() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);
    let terminal = RecordingTerminal::new(80, 24);
    let (mode, _) = build_mode_with(
        TuiMode::Regular,
        true,
        terminal,
        InteractiveModeHooks {
            clipboard_copy: Arc::new(|_| Box::pin(async { Ok(()) })),
            ..Default::default()
        },
    );

    mode.handle_copy_command(true, true).await;

    let output = strip_terminal_sequences(&mode.chat_container().render(120).join("\n"));
    assert!(
        output.contains("Copied last agent message to clipboard"),
        "unexpected output: {output:?}"
    );
}

#[tokio::test]
#[serial]
async fn routes_every_status_through_the_editor_opt_in() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    for embed in [true, false] {
        let terminal = RecordingTerminal::new(80, 24);
        let (mode, _) = build_mode_with(TuiMode::Regular, true, terminal, InteractiveModeHooks::default());
        if !embed {
            mode.set_custom_editor_component(Some(Arc::new(|_, _, _| Arc::new(StubCustomEditor))));
        }

        let ui = mode.ui();
        let indicators = vec![
            StatusIndicator::working(ui.clone(), "Working", None, None),
            StatusIndicator::compaction(ui.clone(), CompactionStatusReason::Manual, None),
            StatusIndicator::compaction(ui.clone(), CompactionStatusReason::Threshold, None),
            StatusIndicator::compaction(ui.clone(), CompactionStatusReason::Overflow, None),
            StatusIndicator::branch_summary(ui.clone()),
            StatusIndicator::retry(ui, 1, 3, 1000),
        ];

        for ind in &indicators {
            let expected_kind = ind.kind();
            mode.show_status_indicator(ind.clone());
            assert_eq!(mode.active_status_indicator_kind(), Some(expected_kind));
            assert_eq!(mode.active_working_indicator_embedded(), embed);
            if embed {
                assert!(mode.status_container().children().is_empty());
            } else {
                assert_eq!(mode.status_container().children().len(), 1);
            }
        }
        for ind in indicators {
            ind.dispose();
        }
    }
}

#[test]
#[serial]
fn does_not_reserve_separate_status_height_for_an_embedded_indicator() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);
    let terminal = RecordingTerminal::new(80, 24);
    let (mode, _) = build_mode_with(TuiMode::Regular, true, terminal, InteractiveModeHooks::default());

    for kind in [
        StatusIndicatorKind::Working,
        StatusIndicatorKind::Compaction,
        StatusIndicatorKind::BranchSummary,
        StatusIndicatorKind::Retry,
    ] {
        let ind = match kind {
            StatusIndicatorKind::Working => StatusIndicator::working(mode.ui(), "Working", None, None),
            StatusIndicatorKind::Compaction => {
                StatusIndicator::compaction(mode.ui(), CompactionStatusReason::Manual, None)
            }
            StatusIndicatorKind::BranchSummary => StatusIndicator::branch_summary(mode.ui()),
            StatusIndicatorKind::Retry => StatusIndicator::retry(mode.ui(), 1, 3, 1000),
        };
        mode.show_status_indicator(ind.clone());
        assert!(mode.active_working_indicator_embedded());
        assert!(mode.status_container().children().is_empty());

        mode.clear_status_indicator(Some(kind));
        assert!(mode.active_status_indicator_kind().is_none());
        assert!(mode.status_container().children().is_empty());
        ind.dispose();
    }
}

#[test]
#[serial]
fn uses_the_standalone_row_for_a_custom_editor_that_has_not_opted_in() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    for (tui_mode, expected_children) in [(TuiMode::Regular, 1usize), (TuiMode::Fullscreen, 0usize)] {
        let terminal = RecordingTerminal::new(80, 24);
        let (mode, _) = build_mode_with(tui_mode, true, terminal, InteractiveModeHooks::default());
        mode.ui().set_clear_on_shrink(true);
        mode.set_custom_editor_component(Some(Arc::new(|_, _, _| Arc::new(StubCustomEditor))));
        let ind = StatusIndicator::working(mode.ui(), "Working", None, None);
        mode.show_status_indicator(ind.clone());
        assert!(!mode.active_working_indicator_embedded());

        mode.clear_status_indicator(None);
        assert_eq!(mode.status_container().children().len(), expected_children);
        ind.dispose();
    }
}

#[tokio::test]
#[serial]
async fn drops_the_fullscreen_selection_when_session_entries_are_re_rendered() {
    // Regression test for #9311: selection coordinates survived session switches.
    let _g = TestGuard;
    init_theme(Some("dark"), None);
    let terminal = RecordingTerminal::new(40, 4);
    let (mode, _) = build_mode_with(
        TuiMode::Fullscreen,
        false,
        terminal.clone(),
        InteractiveModeHooks::default(),
    );

    let alt = mode.renderer().as_alt_screen().expect("alt").clone();
    alt.add_child(Arc::new(Text::new(
        Some("alpha\nbeta\ngamma\ndelta"),
        Some(0),
        Some(0),
        None,
    )));
    alt.start();
    terminal.wait_for_render().await;
    alt.render_now(Some(true));

    terminal.send_input("\x1b[<0;1;1M");
    terminal.send_input("\x1b[<32;4;2M");
    terminal.send_input("\x1b[<0;4;2m");
    terminal.wait_for_render().await;
    assert!(alt.has_active_selection());

    mode.render_session_entries(&[], false, false);

    assert!(!alt.has_active_selection());
    alt.stop(None);
}
