//! Port of packages/coding-agent/test/interactive-mode-compaction.test.ts

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_ai::auth::types::{AuthInteraction, AuthType};
use pi_ai::{AssistantMessage, Model, ModelThinkingLevel, Transport, Usage, UsageCost};
use pi_coding_agent::modes::interactive::bug_report::BugReportSession;
use pi_coding_agent::modes::interactive::components::footer::{
    FooterContextUsage, FooterRoutedModel, FooterSession, FooterUsageRecord,
};
use pi_coding_agent::modes::interactive::components::mermaid::MermaidRenderingMode;
use pi_coding_agent::modes::interactive::components::model_selector::ScopedModelItem;
use pi_coding_agent::modes::interactive::components::settings_selector::{
    CacheWarmingMode, DefaultProjectTrust, DoubleEscapeAction, FullscreenExitOutput, QuietStartup, SteeringMode,
    TreeFilterMode, TuiMode, WarningSettings,
};
use pi_coding_agent::modes::interactive::components::tool_execution::ToolRenderers;
use pi_coding_agent::modes::interactive::components::tree_selector::{
    BranchSummaryEntry, CompactionEntry, SessionEntry, SessionTreeNode,
};
use pi_coding_agent::modes::interactive::components::user_message_selector::UserMessageItem;
use pi_coding_agent::modes::interactive::interactive_mode::{
    AgentSessionRuntimeDiagnostic, BashExecutionResult, CacheMissNotice, CacheWarmingStatusSummary, CancelledResult,
    ClearQueueResult, CompactionQueueMode, CompactionSummaryResult, CycleDirection, CycleModelResult,
    InteractiveExtensionBindings, InteractiveExtensionRunner, InteractiveMode, InteractiveModeHooks,
    InteractiveModeOptions, InteractiveModelRuntime, InteractivePromptOptions, InteractiveResourceLoader,
    InteractiveRuntimeHost, InteractiveSession, InteractiveSessionManager, InteractiveSettingsManager,
    LoadedExtensionsResult, LoadedThemeResource, NavigateTreeOptions, NavigateTreeResult, PromptTemplateInfo,
    ProviderAuthInfo, ProviderAuthStatusInfo, RegisteredExtensionCommand, RegisteredExtensionShortcut,
    ResourceDiagnostic, RuntimeForkOptions, RuntimeForkResult, SessionStats, SkillInfo, SourceInfo,
    StoredCredentialInfo, SwitchSessionOptions, UsageBreakdownEntry,
};
use pi_coding_agent::modes::interactive::model_catalog_refresh::{ModelCatalogRefreshResult, ModelCatalogRuntime};
use pi_coding_agent::modes::interactive::session_share::{SessionShareDestination, ShareSession};
use pi_coding_agent::modes::interactive::theme::theme::{Theme, init_theme};
use pi_coding_agent::modes::interactive::theme::theme_controller::ThemeSettingsSource;
use pi_coding_agent::modes::json_event::{AgentSessionEvent, CompactionReason};
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result, Unsubscribe};
use pi_tui::editor_component::EditorTextCallback;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{
    Component, EditorComponent, KeybindingsManager, ScrollViewScrollbar, TUI_KEYBINDINGS, WheelScrollLines,
    set_keybindings, strip_terminal_sequences,
};
use serde_json::json;
use serial_test::serial;

type SessionListener = Arc<dyn Fn(AgentSessionEvent) -> BoxFuture<()> + Send + Sync>;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

struct RecordingTerminal {
    progress_calls: Mutex<Vec<bool>>,
}

impl RecordingTerminal {
    fn new() -> Self {
        Self {
            progress_calls: Mutex::new(Vec::new()),
        }
    }

    fn progress_calls(&self) -> Vec<bool> {
        self.progress_calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait]
impl Terminal for RecordingTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        120
    }
    fn rows(&self) -> i64 {
        24
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
    fn set_progress(&self, active: bool) {
        self.progress_calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(active);
    }
}

struct StubSettings {
    show_cache_miss_notices: bool,
    show_terminal_progress: bool,
}

impl Default for StubSettings {
    fn default() -> Self {
        Self {
            show_cache_miss_notices: true,
            show_terminal_progress: false,
        }
    }
}

impl ThemeSettingsSource for StubSettings {
    fn get_theme_setting(&self) -> Option<String> {
        Some("dark".to_string())
    }
}

impl InteractiveSettingsManager for StubSettings {
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
        false
    }
    fn set_fullscreen_copy_on_select(&self, _enabled: bool) {}
    fn get_fullscreen_wheel_scroll_lines(&self) -> WheelScrollLines {
        WheelScrollLines::default()
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
        self.show_cache_miss_notices
    }
    fn set_show_cache_miss_notices(&self, _shown: bool) {}
    fn get_show_terminal_progress(&self) -> bool {
        self.show_terminal_progress
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
        "device-1".to_string()
    }
    fn is_project_trusted(&self) -> bool {
        true
    }
}

struct MockSessionManager {
    context_entries: Mutex<Vec<SessionEntry>>,
}

impl MockSessionManager {
    fn new(context_entries: Vec<SessionEntry>) -> Self {
        Self {
            context_entries: Mutex::new(context_entries),
        }
    }
}

impl InteractiveSessionManager for MockSessionManager {
    fn get_cwd(&self) -> String {
        "/tmp".to_string()
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
        self.context_entries.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn append_label_change(&self, _entry_id: &str, _label: Option<&str>) {}
    fn list_current_sessions(
        &self,
        _on_progress: Option<pi_coding_agent::modes::interactive::components::session_selector::SessionListProgress>,
        _signal: AbortSignal,
    ) -> BoxFuture<Result<Vec<pi_coding_agent::modes::interactive::components::session_selector_search::SessionInfo>>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn list_all_sessions(
        &self,
        _on_progress: Option<pi_coding_agent::modes::interactive::components::session_selector::SessionListProgress>,
        _signal: AbortSignal,
    ) -> BoxFuture<Result<Vec<pi_coding_agent::modes::interactive::components::session_selector_search::SessionInfo>>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn rename_session(&self, _session_file: &str, _next_name: Option<&str>) -> BoxFuture<Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

struct StubModelRuntime;

impl ModelCatalogRuntime for StubModelRuntime {
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
impl InteractiveModelRuntime for StubModelRuntime {
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

struct StubResourceLoader;

impl InteractiveResourceLoader for StubResourceLoader {
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

struct StubExtensionRunner;

impl InteractiveExtensionRunner for StubExtensionRunner {
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
    fn get_markdown_transformers(
        &self,
    ) -> Vec<pi_coding_agent::modes::interactive::components::markdown_transform::MarkdownTransformer> {
        Vec::new()
    }
    fn get_entry_renderer(
        &self,
        _custom_type: &str,
    ) -> Option<pi_coding_agent::modes::interactive::components::custom_entry::EntryRenderer> {
        None
    }
    fn get_message_renderer(
        &self,
        _custom_type: &str,
    ) -> Option<pi_coding_agent::modes::interactive::components::custom_message::MessageRenderer> {
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

#[derive(Clone, Debug)]
struct PromptCall {
    text: String,
    streaming_behavior: Option<CompactionQueueMode>,
}

struct MockSession {
    settings: Arc<StubSettings>,
    session_manager: Arc<MockSessionManager>,
    model_runtime: Arc<StubModelRuntime>,
    resource_loader: Arc<StubResourceLoader>,
    extension_runner: Arc<StubExtensionRunner>,
    is_compacting: AtomicBool,
    abort_calls: AtomicUsize,
    prompt_calls: Mutex<Vec<PromptCall>>,
    listeners: Mutex<Vec<SessionListener>>,
    bindings: Mutex<Option<InteractiveExtensionBindings>>,
}

impl MockSession {
    fn new(settings: StubSettings, session_manager: MockSessionManager) -> Self {
        Self {
            settings: Arc::new(settings),
            session_manager: Arc::new(session_manager),
            model_runtime: Arc::new(StubModelRuntime),
            resource_loader: Arc::new(StubResourceLoader),
            extension_runner: Arc::new(StubExtensionRunner),
            is_compacting: AtomicBool::new(false),
            abort_calls: AtomicUsize::new(0),
            prompt_calls: Mutex::new(Vec::new()),
            listeners: Mutex::new(Vec::new()),
            bindings: Mutex::new(None),
        }
    }

    async fn emit(&self, event: AgentSessionEvent) {
        let listeners = self.listeners.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for listener in listeners {
            listener(event.clone()).await;
        }
    }
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
impl InteractiveSession for MockSession {
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
        Arc::new(MockSession::new(
            StubSettings::default(),
            MockSessionManager::new(Vec::new()),
        ))
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
        true
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
        self.is_compacting.load(Ordering::SeqCst)
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
    async fn prompt(&self, text: &str, options: Option<InteractivePromptOptions>) -> Result<()> {
        self.prompt_calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(PromptCall {
                text: text.to_string(),
                streaming_behavior: options.and_then(|o| o.streaming_behavior),
            });
        Ok(())
    }
    async fn steer(&self, _text: &str) -> Result<()> {
        Ok(())
    }
    async fn follow_up(&self, _text: &str) -> Result<()> {
        Ok(())
    }
    async fn abort(&self) {
        self.abort_calls.fetch_add(1, Ordering::SeqCst);
    }
    async fn wait_for_idle(&self) {}
    fn abort_compaction(&self) {}
    fn abort_branch_summary(&self) {}
    fn abort_retry(&self) {}
    fn abort_bash(&self) {}
    fn subscribe(&self, listener: SessionListener) -> Unsubscribe {
        self.listeners.lock().unwrap_or_else(|e| e.into_inner()).push(listener);
        Unsubscribe::noop()
    }
    async fn bind_extensions(&self, bindings: InteractiveExtensionBindings) -> Result<()> {
        *self.bindings.lock().unwrap_or_else(|e| e.into_inner()) = Some(bindings);
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
        _on_chunk: Arc<dyn Fn(&str) + Send + Sync>,
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

struct StubRuntimeHost {
    session: Arc<MockSession>,
}

#[async_trait]
impl InteractiveRuntimeHost for StubRuntimeHost {
    fn session(&self) -> Arc<dyn InteractiveSession> {
        Arc::clone(&self.session) as Arc<dyn InteractiveSession>
    }
    fn agent_dir(&self) -> String {
        "/tmp/agent".to_string()
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
        Ok(RuntimeForkResult {
            cancelled: false,
            selected_text: None,
        })
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

struct CaptureEditor {
    text: Mutex<String>,
    on_submit: Mutex<Option<EditorTextCallback>>,
    on_change: Mutex<Option<EditorTextCallback>>,
}

impl CaptureEditor {
    fn new() -> Self {
        Self {
            text: Mutex::new(String::new()),
            on_submit: Mutex::new(None),
            on_change: Mutex::new(None),
        }
    }
}

impl Component for CaptureEditor {
    fn render(&self, _width: usize) -> Vec<String> {
        vec![self.text.lock().unwrap_or_else(|e| e.into_inner()).clone()]
    }

    fn invalidate(&self) {}
}

impl EditorComponent for CaptureEditor {
    fn get_text(&self) -> String {
        self.text.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn set_text(&self, text: &str) {
        *self.text.lock().unwrap_or_else(|e| e.into_inner()) = text.to_string();
    }
    fn on_submit(&self) -> Option<EditorTextCallback> {
        self.on_submit.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn set_on_submit(&self, on_submit: Option<EditorTextCallback>) {
        *self.on_submit.lock().unwrap_or_else(|e| e.into_inner()) = on_submit;
    }
    fn on_change(&self) -> Option<EditorTextCallback> {
        self.on_change.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn set_on_change(&self, on_change: Option<EditorTextCallback>) {
        *self.on_change.lock().unwrap_or_else(|e| e.into_inner()) = on_change;
    }
}

fn build_mode(
    settings: StubSettings,
    entries: Vec<SessionEntry>,
) -> (InteractiveMode, Arc<MockSession>, Arc<RecordingTerminal>) {
    let terminal = Arc::new(RecordingTerminal::new());
    let session = Arc::new(MockSession::new(settings, MockSessionManager::new(entries)));
    let host = Arc::new(StubRuntimeHost {
        session: Arc::clone(&session),
    });
    let hooks = InteractiveModeHooks {
        exit_process: Arc::new(|_| {}),
        ..Default::default()
    };
    let mode = InteractiveMode::new(
        host,
        InteractiveModeOptions {
            terminal: Some(Arc::clone(&terminal) as Arc<dyn Terminal>),
            hooks: Some(hooks),
            ..Default::default()
        },
    );
    (mode, session, terminal)
}

fn chat_text(mode: &InteractiveMode) -> String {
    let lines: Vec<String> = mode
        .chat_container()
        .children()
        .into_iter()
        .flat_map(|c| c.render(200))
        .collect();
    strip_terminal_sequences(&lines.join("\n"))
}

fn pending_text(mode: &InteractiveMode) -> String {
    let lines: Vec<String> = mode
        .pending_messages_container()
        .children()
        .into_iter()
        .flat_map(|c| c.render(200))
        .collect();
    strip_terminal_sequences(&lines.join("\n"))
}

#[test]
#[serial]
fn uses_the_cache_miss_notice_setting_for_compaction_and_branch_summary_costs() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let usage = Usage {
        input: 10,
        output: 20,
        cache_read: 30,
        cache_write: 40,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 100,
        cost: UsageCost {
            input: 0.01,
            output: 0.02,
            cache_read: 0.03,
            cache_write: 0.065,
            total: 0.125,
        },
    };
    let entries = vec![
        SessionEntry::Compaction(CompactionEntry {
            id: "c1".to_string(),
            parent_id: None,
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            summary: "compaction summary".to_string(),
            first_kept_entry_id: "kept".to_string(),
            tokens_before: 100,
            details: None,
            usage: Some(usage),
            system_message: None,
        }),
        SessionEntry::BranchSummary(BranchSummaryEntry {
            id: "b1".to_string(),
            parent_id: Some("c1".to_string()),
            timestamp: "2025-01-01T00:00:01Z".to_string(),
            from_id: "c1".to_string(),
            summary: "branch summary".to_string(),
            details: None,
            usage: Some(usage),
        }),
    ];

    let (enabled, _, _) = build_mode(
        StubSettings {
            show_cache_miss_notices: true,
            ..Default::default()
        },
        entries.clone(),
    );
    enabled.render_initial_messages();
    let output = chat_text(&enabled);
    assert!(
        output.contains("Compaction: 100 tokens billed (~$0.13)"),
        "output={output}"
    );
    assert!(
        output.contains("Branch summary: 100 tokens billed (~$0.13)"),
        "output={output}"
    );

    let (disabled, _, _) = build_mode(
        StubSettings {
            show_cache_miss_notices: false,
            ..Default::default()
        },
        entries,
    );
    disabled.render_initial_messages();
    let disabled_output = chat_text(&disabled);
    assert!(!disabled_output.contains("tokens billed"), "output={disabled_output}");
}

#[test]
#[serial]
fn renders_each_compaction_cost_after_its_summary() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let current_usage = Usage {
        input: 10,
        output: 20,
        cache_read: 30,
        cache_write: 40,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 100,
        cost: UsageCost {
            input: 0.01,
            output: 0.02,
            cache_read: 0.03,
            cache_write: 0.04,
            total: 0.1,
        },
    };
    let previous_usage = Usage {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 10,
        cost: UsageCost {
            input: 0.001,
            output: 0.002,
            cache_read: 0.003,
            cache_write: 0.004,
            total: 0.01,
        },
    };
    let entries = vec![
        SessionEntry::Compaction(CompactionEntry {
            id: "current".to_string(),
            parent_id: Some("previous".to_string()),
            timestamp: "2025-01-02T00:00:00Z".to_string(),
            summary: "current summary".to_string(),
            first_kept_entry_id: "kept".to_string(),
            tokens_before: 200,
            details: None,
            usage: Some(current_usage),
            system_message: None,
        }),
        SessionEntry::Compaction(CompactionEntry {
            id: "previous".to_string(),
            parent_id: None,
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            summary: "previous summary".to_string(),
            first_kept_entry_id: "kept".to_string(),
            tokens_before: 100,
            details: None,
            usage: Some(previous_usage),
            system_message: None,
        }),
    ];

    let (mode, _, _) = build_mode(StubSettings::default(), entries);
    mode.create_extension_ui_context().set_tools_expanded(true);
    mode.render_initial_messages();

    let rendered = chat_text(&mode);
    let idx_cur_sum = rendered.find("current summary").expect("current summary rendered");
    let idx_cur_cost = rendered
        .find("Compaction: 100 tokens billed (~$0.10)")
        .expect("current cost rendered");
    let idx_prev_sum = rendered.find("previous summary").expect("previous summary rendered");
    let idx_prev_cost = rendered
        .find("Compaction: 10 tokens billed (~$0.01)")
        .expect("previous cost rendered");

    assert!(idx_cur_sum < idx_cur_cost, "rendered={rendered}");
    assert!(idx_cur_cost < idx_prev_sum, "rendered={rendered}");
    assert!(idx_prev_sum < idx_prev_cost, "rendered={rendered}");
}

#[tokio::test]
#[serial]
async fn renders_retained_entries_and_appends_the_latest_summary_cost_at_the_bottom() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let usage = Usage {
        input: 10,
        output: 20,
        cache_read: 30,
        cache_write: 40,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 100,
        cost: UsageCost {
            input: 0.01,
            output: 0.02,
            cache_read: 0.03,
            cache_write: 0.065,
            total: 0.125,
        },
    };
    let latest = SessionEntry::Compaction(CompactionEntry {
        id: "latest".to_string(),
        parent_id: Some("previous".to_string()),
        timestamp: "2025-01-02T00:00:00Z".to_string(),
        summary: "summary".to_string(),
        first_kept_entry_id: "kept".to_string(),
        tokens_before: 123,
        details: None,
        usage: Some(usage),
        system_message: None,
    });
    let previous = SessionEntry::Compaction(CompactionEntry {
        id: "previous".to_string(),
        parent_id: None,
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        summary: "previous summary".to_string(),
        first_kept_entry_id: "kept".to_string(),
        tokens_before: 100,
        details: None,
        usage: Some(usage),
        system_message: None,
    });

    let (mode, session, _) = build_mode(StubSettings::default(), vec![latest, previous]);
    mode.init().await;
    mode.create_extension_ui_context().set_tools_expanded(true);

    let usage_json = pi_js::json::parse(&pi_js::json::stringify(&usage)).expect("usage json roundtrip");
    session
        .emit(AgentSessionEvent::CompactionEnd {
            reason: CompactionReason::Manual,
            result: Some(json!({
                "tokensBefore": 123,
                "summary": "summary",
                "usage": usage_json,
            })),
            aborted: false,
            will_retry: false,
            error_message: None,
        })
        .await;

    let rendered = chat_text(&mode);
    let prev_idx = rendered.find("previous summary").expect("previous summary rendered");
    let latest_idx = rendered
        .find("Compacted from 123 tokens")
        .expect("latest summary rendered");
    let latest_cost_idx = rendered
        .rfind("Compaction: 100 tokens billed (~$0.13)")
        .expect("latest compaction cost rendered");
    assert!(prev_idx < latest_idx, "rendered={rendered}");
    assert!(latest_idx < latest_cost_idx, "rendered={rendered}");

    mode.stop(None);
}

#[tokio::test]
#[serial]
async fn updates_the_working_state_when_the_same_agent_run_resumes_after_compaction() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let (mode, session, terminal) = build_mode(
        StubSettings {
            show_cache_miss_notices: true,
            show_terminal_progress: true,
        },
        Vec::new(),
    );
    mode.init().await;

    // Install a non-embedding custom editor so working status indicator renders into `status_container`.
    let ui_ctx = mode.create_extension_ui_context();
    let editor = Arc::new(CaptureEditor::new());
    ui_ctx.set_editor_component(Some(Arc::new(move |_tui, _theme, _kb| {
        Arc::clone(&editor) as Arc<dyn EditorComponent>
    })));

    session.emit(AgentSessionEvent::TurnStart).await;
    assert_eq!(terminal.progress_calls(), vec![true]);
    assert_eq!(mode.status_container().children().len(), 1);

    ui_ctx.set_working_visible(false);
    session.emit(AgentSessionEvent::TurnStart).await;
    assert_eq!(terminal.progress_calls(), vec![true, true]);
    assert!(mode.status_container().children().is_empty());

    mode.stop(None);
}

#[tokio::test]
#[serial]
async fn routes_interactive_response_aborts_through_agent_session() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let (mode, session, _) = build_mode(StubSettings::default(), Vec::new());
    mode.init().await;

    let abort_handler = session
        .bindings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .expect("bind_extensions called during init")
        .abort_handler
        .clone();
    abort_handler();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if session.abort_calls.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(session.abort_calls.load(Ordering::SeqCst), 1);

    mode.stop(None);
}

#[tokio::test]
#[serial]
async fn preserves_steering_behavior_when_flushing_into_an_active_agent_run() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let (mode, session, _) = build_mode(StubSettings::default(), Vec::new());
    mode.init().await;

    let ui_ctx = mode.create_extension_ui_context();
    let editor = Arc::new(CaptureEditor::new());
    let editor_c = Arc::clone(&editor);
    ui_ctx.set_editor_component(Some(Arc::new(move |_tui, _theme, _kb| {
        Arc::clone(&editor_c) as Arc<dyn EditorComponent>
    })));
    let on_submit = editor.on_submit().expect("init() wires on_submit");

    session.is_compacting.store(true, Ordering::SeqCst);
    on_submit("change direction");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if pending_text(&mode).contains("Steering: change direction") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(pending_text(&mode).contains("Steering: change direction"));

    session.is_compacting.store(false, Ordering::SeqCst);
    session
        .emit(AgentSessionEvent::CompactionEnd {
            reason: CompactionReason::Manual,
            result: None,
            aborted: false,
            will_retry: false,
            error_message: None,
        })
        .await;

    let prompt_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < prompt_deadline {
        if !session
            .prompt_calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let calls = session.prompt_calls.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].text, "change direction");
    assert_eq!(calls[0].streaming_behavior, Some(CompactionQueueMode::Steer));
    assert!(!pending_text(&mode).contains("Steering: change direction"));
    assert!(!chat_text(&mode).contains("Error:"));

    mode.stop(None);
}
