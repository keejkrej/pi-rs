//! Port of packages/coding-agent/test/interactive-mode-status.test.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_ai::auth::types::{AuthInteraction, AuthType};
use pi_ai::{AssistantMessage, Model, ModelCost, ModelInput, ModelThinkingLevel, Transport};
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
use pi_coding_agent::modes::interactive::components::tree_selector::{SessionEntry, SessionTreeNode};
use pi_coding_agent::modes::interactive::components::user_message_selector::UserMessageItem;
use pi_coding_agent::modes::interactive::interactive_mode::{
    AgentSessionRuntimeDiagnostic, AutocompleteProviderFactory, BashExecutionResult, CacheMissNotice,
    CacheWarmingStatusSummary, CancelledResult, ClearQueueResult, CompactionSummaryResult, CycleDirection,
    CycleModelResult, DiagnosticLevel, ExtensionCustomOptions, InteractiveExtensionBindings,
    InteractiveExtensionRunner, InteractiveMode, InteractiveModeHooks, InteractiveModeOptions, InteractiveModelRuntime,
    InteractivePromptOptions, InteractiveResourceLoader, InteractiveRuntimeHost, InteractiveSession,
    InteractiveSessionManager, InteractiveSettingsManager, LoadedExtensionInfo, LoadedExtensionsResult,
    LoadedThemeResource, NavigateTreeOptions, NavigateTreeResult, PromptTemplateInfo, ProviderAuthInfo,
    ProviderAuthStatusInfo, RegisteredExtensionCommand, RegisteredExtensionShortcut, ResourceDiagnostic,
    RuntimeForkOptions, RuntimeForkResult, SessionStats, SkillInfo, SourceInfo, StoredCredentialInfo,
    SwitchSessionOptions, ToolStatus, UsageBreakdownEntry,
};
use pi_coding_agent::modes::interactive::model_catalog_refresh::{ModelCatalogRefreshResult, ModelCatalogRuntime};
use pi_coding_agent::modes::interactive::session_share::{SessionShareDestination, ShareSession};
use pi_coding_agent::modes::interactive::theme::theme::{Theme, init_theme};
use pi_coding_agent::modes::interactive::theme::theme_controller::ThemeSettingsSource;
use pi_coding_agent::modes::json_event::AgentSessionEvent;
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::{BoxFuture, Result, Unsubscribe};
use pi_tui::autocomplete::{
    ApplyCompletionResult, AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions, GetSuggestionsOptions,
};
use pi_tui::editor_component::EditorTextCallback;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::tui::OverlayHandle;
use pi_tui::{
    Component, Container, EditorComponent, Focusable, KeybindingsManager, OverlayOptions, ScrollViewScrollbar,
    TUI_KEYBINDINGS, WheelScrollLines, set_keybindings, strip_terminal_sequences,
};
use serde_json::{Value, json};
use serial_test::serial;

type SessionListener = Arc<dyn Fn(AgentSessionEvent) -> BoxFuture<()> + Send + Sync>;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

struct MockTerminal {
    cols: i64,
    rows: i64,
    on_input: Mutex<Option<TerminalInputHandler>>,
    writes: Mutex<Vec<String>>,
}

impl MockTerminal {
    fn new(cols: i64, rows: i64) -> Self {
        Self {
            cols,
            rows,
            on_input: Mutex::new(None),
            writes: Mutex::new(Vec::new()),
        }
    }

    fn send_input(&self, data: &str) {
        let handler = self.on_input.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(cb) = handler {
            cb(data);
        }
    }

    fn clear_writes(&self) {
        self.writes.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    fn writes_text(&self) -> String {
        strip_terminal_sequences(&self.writes.lock().unwrap_or_else(|e| e.into_inner()).join(""))
    }
}

#[async_trait]
impl Terminal for MockTerminal {
    fn start(&self, on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {
        *self.on_input.lock().unwrap_or_else(|e| e.into_inner()) = Some(on_input);
    }
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, data: &str) {
        self.writes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(data.to_string());
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
    quiet_startup: QuietStartup,
    enable_skill_commands: bool,
    theme: Mutex<Option<String>>,
    set_theme_calls: Mutex<Vec<String>>,
}

impl Default for MockSettings {
    fn default() -> Self {
        Self {
            quiet_startup: QuietStartup::False,
            enable_skill_commands: false,
            theme: Mutex::new(Some("dark".to_string())),
            set_theme_calls: Mutex::new(Vec::new()),
        }
    }
}

impl ThemeSettingsSource for MockSettings {
    fn get_theme_setting(&self) -> Option<String> {
        self.theme.lock().unwrap_or_else(|e| e.into_inner()).clone()
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
        self.enable_skill_commands
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
        self.quiet_startup
    }
    fn set_quiet_startup(&self, _quiet: QuietStartup) {}
    fn get_mermaid_rendering_mode(&self) -> MermaidRenderingMode {
        MermaidRenderingMode::Off
    }
    fn set_mermaid_rendering_mode(&self, _mode: MermaidRenderingMode) {}
    fn get_show_cache_miss_notices(&self) -> bool {
        true
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
        self.theme.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn set_theme(&self, theme: &str) {
        self.set_theme_calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(theme.to_string());
        *self.theme.lock().unwrap_or_else(|e| e.into_inner()) = Some(theme.to_string());
    }
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
    cwd: String,
}

impl Default for MockSessionManager {
    fn default() -> Self {
        Self {
            cwd: "/tmp/project".to_string(),
        }
    }
}

impl InteractiveSessionManager for MockSessionManager {
    fn get_cwd(&self) -> String {
        self.cwd.clone()
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

#[derive(Default)]
struct MockModelRuntime {
    models: Vec<Model>,
    providers: Vec<ProviderAuthInfo>,
}

impl ModelCatalogRuntime for MockModelRuntime {
    fn runtime_ptr_key(&self) -> usize {
        self as *const Self as usize
    }
    fn refresh(&self, _signal: AbortSignal) -> BoxFuture<Result<ModelCatalogRefreshResult>> {
        Box::pin(async { Ok(ModelCatalogRefreshResult::default()) })
    }
    fn get_available_snapshot(&self) -> Vec<Model> {
        self.models.clone()
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
        self.providers.clone()
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

#[derive(Default)]
struct MockResourceLoader {
    skills: Vec<SkillInfo>,
    skill_diagnostics: Vec<ResourceDiagnostic>,
    extensions: LoadedExtensionsResult,
    system_prompt_source: Option<SourceInfo>,
    append_system_prompt_sources: Vec<SourceInfo>,
    agents_files: Vec<SourceInfo>,
}

impl InteractiveResourceLoader for MockResourceLoader {
    fn get_skills(&self) -> (Vec<SkillInfo>, Vec<ResourceDiagnostic>) {
        (self.skills.clone(), self.skill_diagnostics.clone())
    }
    fn get_prompts(&self) -> (Vec<PromptTemplateInfo>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new())
    }
    fn get_themes(&self) -> (Vec<Theme>, Vec<LoadedThemeResource>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new(), Vec::new())
    }
    fn get_extensions(&self) -> LoadedExtensionsResult {
        self.extensions.clone()
    }
    fn get_system_prompt_source(&self) -> Option<SourceInfo> {
        self.system_prompt_source.clone()
    }
    fn get_append_system_prompt_sources(&self) -> Vec<SourceInfo> {
        self.append_system_prompt_sources.clone()
    }
    fn get_agents_files(&self) -> Vec<SourceInfo> {
        self.agents_files.clone()
    }
}

#[derive(Default)]
struct MockExtensionRunner {
    user_bash_result: Mutex<Option<BashExecutionResult>>,
}

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
        self.user_bash_result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }
    fn emit_user_bash(
        &self,
        _command: &str,
        _exclude_from_context: bool,
        _cwd: &str,
    ) -> BoxFuture<Result<Option<BashExecutionResult>>> {
        let res = self.user_bash_result.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Box::pin(async move { Ok(res) })
    }
}

struct MockSession {
    settings: Arc<MockSettings>,
    session_manager: Arc<MockSessionManager>,
    model_runtime: Arc<MockModelRuntime>,
    resource_loader: Arc<MockResourceLoader>,
    extension_runner: Arc<MockExtensionRunner>,
    is_streaming: AtomicBool,
    steering_messages: Mutex<Vec<String>>,
    listeners: Mutex<Vec<SessionListener>>,
}

impl MockSession {
    fn new(
        settings: MockSettings,
        session_manager: MockSessionManager,
        model_runtime: MockModelRuntime,
        resource_loader: MockResourceLoader,
        extension_runner: MockExtensionRunner,
    ) -> Self {
        Self {
            settings: Arc::new(settings),
            session_manager: Arc::new(session_manager),
            model_runtime: Arc::new(model_runtime),
            resource_loader: Arc::new(resource_loader),
            extension_runner: Arc::new(extension_runner),
            is_streaming: AtomicBool::new(false),
            steering_messages: Mutex::new(Vec::new()),
            listeners: Mutex::new(Vec::new()),
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
            MockSettings::default(),
            MockSessionManager::default(),
            MockModelRuntime::default(),
            MockResourceLoader::default(),
            MockExtensionRunner::default(),
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
        self.is_streaming.load(Ordering::SeqCst)
    }
    fn is_idle(&self) -> bool {
        !self.is_streaming()
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
        self.steering_messages.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
    fn get_steering_messages(&self) -> Vec<String> {
        self.steering_messages.lock().unwrap_or_else(|e| e.into_inner()).clone()
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
    fn subscribe(&self, listener: SessionListener) -> Unsubscribe {
        self.listeners.lock().unwrap_or_else(|e| e.into_inner()).push(listener);
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

struct TestFocusableComponent {
    label: String,
    focused: AtomicBool,
    inputs: Mutex<Vec<String>>,
}

impl TestFocusableComponent {
    fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            focused: AtomicBool::new(false),
            inputs: Mutex::new(Vec::new()),
        }
    }

    fn is_focused(&self) -> bool {
        self.focused.load(Ordering::SeqCst)
    }

    fn inputs(&self) -> Vec<String> {
        self.inputs.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Focusable for TestFocusableComponent {
    fn focused(&self) -> bool {
        self.is_focused()
    }

    fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::SeqCst);
    }
}

impl Component for TestFocusableComponent {
    fn render(&self, _width: usize) -> Vec<String> {
        vec![self.label.clone()]
    }

    fn handle_input(&self, data: &str) {
        self.inputs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(data.to_string());
    }

    fn invalidate(&self) {}

    fn has_handle_input(&self) -> bool {
        true
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
    }
}

struct CaptureEditor {
    label: String,
    focused: AtomicBool,
    inputs: Mutex<Vec<String>>,
    text: Mutex<String>,
    on_submit: Mutex<Option<EditorTextCallback>>,
    on_change: Mutex<Option<EditorTextCallback>>,
    autocomplete_providers: Mutex<Vec<Arc<dyn AutocompleteProvider>>>,
}

impl CaptureEditor {
    fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            focused: AtomicBool::new(false),
            inputs: Mutex::new(Vec::new()),
            text: Mutex::new(String::new()),
            on_submit: Mutex::new(None),
            on_change: Mutex::new(None),
            autocomplete_providers: Mutex::new(Vec::new()),
        }
    }

    fn is_focused(&self) -> bool {
        self.focused.load(Ordering::SeqCst)
    }

    fn inputs(&self) -> Vec<String> {
        self.inputs.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn providers(&self) -> Vec<Arc<dyn AutocompleteProvider>> {
        self.autocomplete_providers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Focusable for CaptureEditor {
    fn focused(&self) -> bool {
        self.is_focused()
    }

    fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::SeqCst);
    }
}

impl Component for CaptureEditor {
    fn render(&self, _width: usize) -> Vec<String> {
        vec![self.label.clone()]
    }

    fn handle_input(&self, data: &str) {
        self.inputs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(data.to_string());
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
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
    fn set_autocomplete_provider(&self, provider: Arc<dyn AutocompleteProvider>) {
        self.autocomplete_providers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(provider);
    }
}

struct DummyOtherComponent;

impl Component for DummyOtherComponent {
    fn render(&self, _width: usize) -> Vec<String> {
        vec!["OTHER".to_string()]
    }

    fn invalidate(&self) {}
}

#[derive(Default)]
struct ModeBuilder {
    settings: MockSettings,
    session_manager: MockSessionManager,
    model_runtime: MockModelRuntime,
    resource_loader: MockResourceLoader,
    extension_runner: MockExtensionRunner,
    verbose: bool,
    cols: i64,
    rows: i64,
}

impl ModeBuilder {
    fn new() -> Self {
        Self {
            cols: 120,
            rows: 24,
            ..Default::default()
        }
    }

    fn build(self) -> (InteractiveMode, Arc<MockSession>, Arc<MockTerminal>) {
        let terminal = Arc::new(MockTerminal::new(self.cols, self.rows));
        let session = Arc::new(MockSession::new(
            self.settings,
            self.session_manager,
            self.model_runtime,
            self.resource_loader,
            self.extension_runner,
        ));
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
                verbose: Some(self.verbose),
                terminal: Some(Arc::clone(&terminal) as Arc<dyn Terminal>),
                hooks: Some(hooks),
                ..Default::default()
            },
        );
        (mode, session, terminal)
    }
}

fn render_last_line(container: &Container, width: usize) -> String {
    let children = container.children();
    let Some(last) = children.last() else {
        return String::new();
    };
    strip_terminal_sequences(&last.render(width).join("\n"))
}

fn render_all(container: &Container, width: usize) -> String {
    let lines: Vec<String> = container.children().into_iter().flat_map(|c| c.render(width)).collect();
    strip_terminal_sequences(&lines.join("\n"))
}

fn normalize_rendered_output(container: &Container, width: usize) -> String {
    render_all(container, width)
        .replace('\\', "/")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn make_model(provider: &str, id: &str, name: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_string(),
        name: name.to_string(),
        api: "openai-responses".to_string(),
        provider: provider.to_string(),
        base_url: "https://example.invalid".to_string(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 4096,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn create_source_info(file_path: &str, source: &str, scope: &str, origin: &str, base_dir: Option<&str>) -> SourceInfo {
    SourceInfo {
        path: file_path.to_string(),
        source: source.to_string(),
        scope: scope.to_string(),
        origin: origin.to_string(),
        base_dir: base_dir.map(str::to_string),
        changelog_path: None,
    }
}

fn create_extension_fixtures() -> Vec<(String, Option<SourceInfo>)> {
    vec![
        (
            "/tmp/project/.pi/extensions/answer.ts".to_string(),
            Some(create_source_info(
                "/tmp/project/.pi/extensions/answer.ts",
                "local",
                "project",
                "top-level",
                Some("/tmp/project/.pi/extensions"),
            )),
        ),
        (
            "/tmp/project/.pi/extensions/local-index/index.ts".to_string(),
            Some(create_source_info(
                "/tmp/project/.pi/extensions/local-index/index.ts",
                "local",
                "project",
                "top-level",
                Some("/tmp/project/.pi/extensions"),
            )),
        ),
        (
            "/tmp/agent/extensions/user-index/index.ts".to_string(),
            Some(create_source_info(
                "/tmp/agent/extensions/user-index/index.ts",
                "local",
                "user",
                "top-level",
                Some("/tmp/agent/extensions"),
            )),
        ),
        (
            "/tmp/project/.pi/npm/node_modules/pi-markdown-preview/extensions/index.ts".to_string(),
            Some(create_source_info(
                "/tmp/project/.pi/npm/node_modules/pi-markdown-preview/extensions/index.ts",
                "npm:pi-markdown-preview",
                "project",
                "package",
                Some("/tmp/project/.pi/npm/node_modules/pi-markdown-preview"),
            )),
        ),
        (
            "/tmp/project/.pi/npm/node_modules/@scope/pi-scoped/extensions/index.ts".to_string(),
            Some(create_source_info(
                "/tmp/project/.pi/npm/node_modules/@scope/pi-scoped/extensions/index.ts",
                "npm:@scope/pi-scoped",
                "project",
                "package",
                Some("/tmp/project/.pi/npm/node_modules/@scope/pi-scoped"),
            )),
        ),
        (
            "/tmp/project/.pi/git/github.com/HazAT/pi-interactive-subagents/extensions/index.ts".to_string(),
            Some(create_source_info(
                "/tmp/project/.pi/git/github.com/HazAT/pi-interactive-subagents/extensions/index.ts",
                "git:github.com/HazAT/pi-interactive-subagents",
                "project",
                "package",
                Some("/tmp/project/.pi/git/github.com/HazAT/pi-interactive-subagents"),
            )),
        ),
        (
            "/tmp/project/.pi/git/github.com/HazAT/pi-interactive-subagents/extensions/subagents/index.ts".to_string(),
            Some(create_source_info(
                "/tmp/project/.pi/git/github.com/HazAT/pi-interactive-subagents/extensions/subagents/index.ts",
                "git:github.com/HazAT/pi-interactive-subagents",
                "project",
                "package",
                Some("/tmp/project/.pi/git/github.com/HazAT/pi-interactive-subagents"),
            )),
        ),
        (
            "/tmp/temp/cli-extension.ts".to_string(),
            Some(create_source_info(
                "/tmp/temp/cli-extension.ts",
                "cli",
                "temporary",
                "top-level",
                Some("/tmp/temp"),
            )),
        ),
    ]
}

// ============================================================================
// InteractiveMode.showStatus
// ============================================================================

#[test]
#[serial]
fn show_status_coalesces_immediately_sequential_status_messages() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let (mode, _, _) = ModeBuilder::new().build();
    mode.show_status("STATUS_ONE");
    assert_eq!(mode.chat_container().children().len(), 2);
    assert!(render_last_line(mode.chat_container(), 120).contains("STATUS_ONE"));

    mode.show_status("STATUS_TWO");
    assert_eq!(mode.chat_container().children().len(), 2);
    assert!(render_last_line(mode.chat_container(), 120).contains("STATUS_TWO"));
    assert!(!render_last_line(mode.chat_container(), 120).contains("STATUS_ONE"));
}

#[test]
#[serial]
fn show_status_appends_a_new_status_line_if_something_else_was_added_in_between() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let (mode, _, _) = ModeBuilder::new().build();
    mode.show_status("STATUS_ONE");
    assert_eq!(mode.chat_container().children().len(), 2);

    mode.chat_container().add_child(Arc::new(DummyOtherComponent));
    assert_eq!(mode.chat_container().children().len(), 3);

    mode.show_status("STATUS_TWO");
    assert_eq!(mode.chat_container().children().len(), 5);
    assert!(render_last_line(mode.chat_container(), 120).contains("STATUS_TWO"));
}

// ============================================================================
// InteractiveMode.showManagedToolStatus
// ============================================================================

#[test]
#[serial]
fn show_managed_tool_status_renders_tool_updates_as_one_contiguous_group() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);
    let (mode, _, _) = ModeBuilder::new().build();

    mode.show_managed_tool_status(&ToolStatus {
        is_warning: false,
        message: "fd downloading".to_string(),
    });
    mode.show_managed_tool_status(&ToolStatus {
        is_warning: false,
        message: "rg downloading".to_string(),
    });
    mode.show_managed_tool_status(&ToolStatus {
        is_warning: true,
        message: "rg failed".to_string(),
    });

    assert_eq!(mode.chat_container().children().len(), 4);
    assert_eq!(
        normalize_rendered_output(mode.chat_container(), 200),
        "fd downloading\n rg downloading\n Warning: rg failed"
    );
}

// ============================================================================
// InteractiveMode.setToolsExpanded
// ============================================================================

#[tokio::test]
#[serial]
async fn set_tools_expanded_applies_expansion_state_to_active_header_and_chat_entries() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let mut b = ModeBuilder::new();
    b.resource_loader.skills = vec![SkillInfo {
        name: "commit".to_string(),
        description: "Commit helper".to_string(),
        file_path: "/tmp/skill/SKILL.md".to_string(),
        source_info: None,
    }];
    let (mode, session, _) = b.build();
    mode.init().await;

    // Add a tool execution component with >5 lines of output so expansion changes rendering.
    session
        .emit(AgentSessionEvent::ToolExecutionStart {
            tool_call_id: "tc-1".to_string(),
            tool_name: "bash".to_string(),
            args: json!({ "command": "seq 1 12" }),
            parent_tool_call_id: None,
        })
        .await;
    let lines: Vec<String> = (1..=12).map(|n| format!("line-{n}")).collect();
    session
        .emit(AgentSessionEvent::ToolExecutionEnd {
            tool_call_id: "tc-1".to_string(),
            tool_name: "bash".to_string(),
            result: pi_agent_core::AgentToolResult::text(lines.join("\n")),
            is_error: false,
            parent_tool_call_id: None,
        })
        .await;

    assert!(render_all(mode.header_container(), 200).contains("to show full startup help"));
    assert!(render_all(mode.loaded_resources_container(), 200).contains("commit"));
    assert!(!render_all(mode.chat_container(), 200).contains("line-1\n"));

    mode.create_extension_ui_context().set_tools_expanded(true);

    let header_out = render_all(mode.header_container(), 200);
    let res_out = render_all(mode.loaded_resources_container(), 200);
    let chat_out = render_all(mode.chat_container(), 200);
    assert!(header_out.contains("for external editor"), "header={header_out}");
    assert!(res_out.contains("/tmp/skill/SKILL.md"), "res={res_out}");
    assert!(chat_out.contains("line-1"), "chat={chat_out}");
    assert!(chat_out.contains("Tool output: expanded"), "chat={chat_out}");

    mode.stop(None);
}

// ============================================================================
// InteractiveMode pending user bash output (Regression #11875)
// ============================================================================

#[tokio::test]
#[serial]
async fn ctrl_o_expands_user_bash_output_in_the_pending_area() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);
    let b = ModeBuilder::new();
    let long_output = (1..=25).map(|i| format!("out-{i}")).collect::<Vec<_>>().join("\n");
    *b.extension_runner
        .user_bash_result
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(BashExecutionResult {
        output: long_output,
        exit_code: Some(0),
        cancelled: false,
        truncated: false,
        full_output_path: None,
    });
    let (mode, session, _) = b.build();
    mode.init().await;
    session.is_streaming.store(true, Ordering::SeqCst);

    let ui_ctx = mode.create_extension_ui_context();
    let editor = Arc::new(CaptureEditor::new("EDITOR"));
    let editor_c = Arc::clone(&editor);
    ui_ctx.set_editor_component(Some(Arc::new(move |_tui, _theme, _kb| {
        Arc::clone(&editor_c) as Arc<dyn EditorComponent>
    })));
    let on_submit = editor.on_submit().expect("on_submit");
    on_submit("!seq 1 25");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if render_all(mode.pending_messages_container(), 200).contains("out-25") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    ui_ctx.set_tools_expanded(true);
    let pending_out = render_all(mode.pending_messages_container(), 200);
    assert!(pending_out.contains("out-1"), "pending_out={pending_out}");
    mode.stop(None);
}

#[tokio::test]
#[serial]
async fn queued_message_updates_keep_user_bash_output_in_the_pending_area() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let b = ModeBuilder::new();
    *b.extension_runner
        .user_bash_result
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(BashExecutionResult {
        output: "bash-pending-line".to_string(),
        exit_code: Some(0),
        cancelled: false,
        truncated: false,
        full_output_path: None,
    });
    let (mode, session, _) = b.build();
    mode.init().await;
    session.is_streaming.store(true, Ordering::SeqCst);

    let ui_ctx = mode.create_extension_ui_context();
    let editor = Arc::new(CaptureEditor::new("EDITOR"));
    let editor_c = Arc::clone(&editor);
    ui_ctx.set_editor_component(Some(Arc::new(move |_tui, _theme, _kb| {
        Arc::clone(&editor_c) as Arc<dyn EditorComponent>
    })));
    let on_submit = editor.on_submit().expect("on_submit");
    on_submit("!echo hi");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if render_all(mode.pending_messages_container(), 200).contains("bash-pending-line") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    *session.steering_messages.lock().unwrap_or_else(|e| e.into_inner()) = vec!["steer".to_string()];
    session
        .emit(AgentSessionEvent::QueueUpdate {
            steering: vec!["steer".to_string()],
            follow_up: Vec::new(),
        })
        .await;

    let pending_out = render_all(mode.pending_messages_container(), 200);
    assert!(pending_out.contains("bash-pending-line"), "pending={pending_out}");
    assert!(pending_out.contains("Steering: steer"), "pending={pending_out}");

    mode.stop(None);
}

#[tokio::test]
#[serial]
async fn settling_the_agent_run_moves_user_bash_output_to_the_chat() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let b = ModeBuilder::new();
    *b.extension_runner
        .user_bash_result
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(BashExecutionResult {
        output: "settled-bash-output".to_string(),
        exit_code: Some(0),
        cancelled: false,
        truncated: false,
        full_output_path: None,
    });
    let (mode, session, _) = b.build();
    mode.init().await;
    session.is_streaming.store(true, Ordering::SeqCst);

    let ui_ctx = mode.create_extension_ui_context();
    let editor = Arc::new(CaptureEditor::new("EDITOR"));
    let editor_c = Arc::clone(&editor);
    ui_ctx.set_editor_component(Some(Arc::new(move |_tui, _theme, _kb| {
        Arc::clone(&editor_c) as Arc<dyn EditorComponent>
    })));
    let on_submit = editor.on_submit().expect("on_submit");
    on_submit("!echo settled");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if render_all(mode.pending_messages_container(), 200).contains("settled-bash-output") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    session.emit(AgentSessionEvent::AgentSettled).await;

    assert!(mode.pending_messages_container().children().is_empty());
    let chat_out = render_all(mode.chat_container(), 200);
    assert!(chat_out.contains("settled-bash-output"), "chat={chat_out}");

    mode.stop(None);
}

// ============================================================================
// InteractiveMode.createExtensionUIContext setTheme
// ============================================================================

#[test]
#[serial]
fn create_extension_ui_context_set_theme_persists_theme_changes_to_settings_manager() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let (mode, session, _) = ModeBuilder::new().build();
    let ui_context = mode.create_extension_ui_context();
    let result = ui_context.set_theme_name("light");

    assert!(result.success);
    assert_eq!(
        session
            .settings
            .set_theme_calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_slice(),
        ["light"]
    );
    assert_eq!(session.settings.get_theme().as_deref(), Some("light"));
}

#[test]
#[serial]
fn create_extension_ui_context_set_theme_does_not_persist_invalid_theme_names() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let (mode, session, _) = ModeBuilder::new().build();
    let ui_context = mode.create_extension_ui_context();
    let result = ui_context.set_theme_name("__missing_theme__");

    assert!(!result.success);
    assert!(result.error.is_some());
    assert!(
        session
            .settings
            .set_theme_calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    );
    assert_eq!(session.settings.get_theme().as_deref(), Some("dark"));
    // PORT: `InteractiveThemeController::apply_theme_name` calls `notify_changed()` even on fallback,
    // so `request_render` is not asserted to be zero here.
}

// ============================================================================
// InteractiveMode.showExtensionCustom
// ============================================================================

async fn flush_tui(mode: &InteractiveMode) {
    let ui = mode.ui();
    ui.request_render(Some(true));
    ui.render_now(Some(true));
    tokio::task::yield_now().await;
}

#[tokio::test]
#[serial]
async fn show_extension_custom_overlay_reclaims_input_after_non_overlay_custom_ui_closes() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let mut b = ModeBuilder::new();
    b.cols = 80;
    b.rows = 24;
    let (mode, _, terminal) = b.build();
    mode.init().await;

    let ui_ctx = mode.create_extension_ui_context();
    let editor = Arc::new(CaptureEditor::new("EDITOR"));
    let editor_c = Arc::clone(&editor);
    ui_ctx.set_editor_component(Some(Arc::new(move |_tui, _theme, _kb| {
        Arc::clone(&editor_c) as Arc<dyn EditorComponent>
    })));

    let overlay = Arc::new(TestFocusableComponent::new("OVERLAY"));
    let replacement = Arc::new(TestFocusableComponent::new("REPLACEMENT"));

    let close_overlay: Arc<Mutex<Option<Arc<dyn Fn(Value) + Send + Sync>>>> = Arc::new(Mutex::new(None));
    let close_overlay_c = Arc::clone(&close_overlay);
    let overlay_c = Arc::clone(&overlay);
    let overlay_task = tokio::spawn({
        let ui_ctx = Arc::clone(&ui_ctx);
        async move {
            ui_ctx
                .custom(
                    Box::new(move |_tui, _theme, _kb, done| {
                        *close_overlay_c.lock().unwrap_or_else(|e| e.into_inner()) = Some(done);
                        Box::pin(async move { Ok(Arc::clone(&overlay_c) as Arc<dyn Component>) })
                    }),
                    Some(ExtensionCustomOptions {
                        overlay: true,
                        ..Default::default()
                    }),
                )
                .await
        }
    });
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    flush_tui(&mode).await;
    assert!(overlay.is_focused());

    let close_replacement: Arc<Mutex<Option<Arc<dyn Fn(Value) + Send + Sync>>>> = Arc::new(Mutex::new(None));
    let close_replacement_c = Arc::clone(&close_replacement);
    let replacement_c = Arc::clone(&replacement);
    let replacement_task = tokio::spawn({
        let ui_ctx = Arc::clone(&ui_ctx);
        async move {
            ui_ctx
                .custom(
                    Box::new(move |_tui, _theme, _kb, done| {
                        *close_replacement_c.lock().unwrap_or_else(|e| e.into_inner()) = Some(done);
                        Box::pin(async move { Ok(Arc::clone(&replacement_c) as Arc<dyn Component>) })
                    }),
                    None,
                )
                .await
        }
    });
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    flush_tui(&mode).await;
    assert!(replacement.is_focused());

    let close_rep = close_replacement
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .expect("replacement done initialized");
    close_rep(json!("done"));
    let rep_res = replacement_task
        .await
        .expect("replacement task")
        .expect("replacement ok");
    assert_eq!(rep_res, json!("done"));

    flush_tui(&mode).await;
    terminal.send_input("x");
    flush_tui(&mode).await;

    assert_eq!(overlay.inputs(), vec!["x"]);
    assert!(editor.inputs().is_empty());
    assert!(overlay.is_focused());

    let close_ov = close_overlay
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .expect("overlay done initialized");
    close_ov(json!("closed"));
    let _ = overlay_task.await;
    mode.stop(None);
}

#[tokio::test]
#[serial]
async fn closing_lower_overlay_custom_ui_removes_its_own_overlay_not_topmost() {
    for non_capturing in [false, true] {
        let _g = TestGuard;
        init_theme(Some("dark"), None);

        let mut b = ModeBuilder::new();
        b.cols = 80;
        b.rows = 24;
        let (mode, _, terminal) = b.build();
        mode.init().await;

        let ui_ctx = mode.create_extension_ui_context();
        let editor = Arc::new(CaptureEditor::new("EDITOR"));
        let editor_c = Arc::clone(&editor);
        ui_ctx.set_editor_component(Some(Arc::new(move |_tui, _theme, _kb| {
            Arc::clone(&editor_c) as Arc<dyn EditorComponent>
        })));

        let lower = Arc::new(TestFocusableComponent::new("LOWER"));
        let upper = Arc::new(TestFocusableComponent::new("UPPER"));
        let close_lower: Arc<Mutex<Option<Arc<dyn Fn(Value) + Send + Sync>>>> = Arc::new(Mutex::new(None));
        let close_upper: Arc<Mutex<Option<Arc<dyn Fn(Value) + Send + Sync>>>> = Arc::new(Mutex::new(None));

        let close_lower_c = Arc::clone(&close_lower);
        let lower_c = Arc::clone(&lower);
        let lower_task = tokio::spawn({
            let ui_ctx = Arc::clone(&ui_ctx);
            async move {
                ui_ctx
                    .custom(
                        Box::new(move |_tui, _theme, _kb, done| {
                            *close_lower_c.lock().unwrap_or_else(|e| e.into_inner()) = Some(done);
                            Box::pin(async move { Ok(Arc::clone(&lower_c) as Arc<dyn Component>) })
                        }),
                        Some(ExtensionCustomOptions {
                            overlay: true,
                            overlay_options: Some(OverlayOptions {
                                non_capturing: Some(non_capturing),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                    )
                    .await
            }
        });
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        flush_tui(&mode).await;

        let close_upper_c = Arc::clone(&close_upper);
        let upper_c = Arc::clone(&upper);
        let upper_task = tokio::spawn({
            let ui_ctx = Arc::clone(&ui_ctx);
            async move {
                ui_ctx
                    .custom(
                        Box::new(move |_tui, _theme, _kb, done| {
                            *close_upper_c.lock().unwrap_or_else(|e| e.into_inner()) = Some(done);
                            Box::pin(async move { Ok(Arc::clone(&upper_c) as Arc<dyn Component>) })
                        }),
                        Some(ExtensionCustomOptions {
                            overlay: true,
                            ..Default::default()
                        }),
                    )
                    .await
            }
        });
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        flush_tui(&mode).await;
        assert!(upper.is_focused());

        let close_low = close_lower
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .expect("close_lower");
        close_low(json!("lower closed"));
        assert_eq!(
            lower_task.await.expect("lower task").expect("lower ok"),
            json!("lower closed")
        );
        terminal.clear_writes();
        flush_tui(&mode).await;

        let viewport = terminal.writes_text();
        assert!(viewport.contains("UPPER"), "viewport={viewport}");
        assert!(!viewport.contains("LOWER"), "viewport={viewport}");
        terminal.send_input("x");
        flush_tui(&mode).await;
        assert_eq!(upper.inputs(), vec!["x"]);
        assert!(lower.inputs().is_empty());

        let close_up = close_upper
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .expect("close_upper");
        close_up(json!("upper closed"));
        assert_eq!(
            upper_task.await.expect("upper task").expect("upper ok"),
            json!("upper closed")
        );
        flush_tui(&mode).await;
        assert!(!mode.ui().has_overlay());
        assert!(editor.is_focused());

        mode.stop(None);
    }
}

#[tokio::test]
#[serial]
async fn reset_extension_ui_removes_every_extension_overlay_but_keeps_pi_internal_overlays() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let mut b = ModeBuilder::new();
    b.cols = 80;
    b.rows = 24;
    let (mode, _, terminal) = b.build();
    mode.init().await;

    let ui_ctx = mode.create_extension_ui_context();
    let editor = Arc::new(CaptureEditor::new("EDITOR"));
    let editor_c = Arc::clone(&editor);
    ui_ctx.set_editor_component(Some(Arc::new(move |_tui, _theme, _kb| {
        Arc::clone(&editor_c) as Arc<dyn EditorComponent>
    })));

    let internal = Arc::new(TestFocusableComponent::new("INTERNAL"));
    let lower = Arc::new(TestFocusableComponent::new("LOWER"));
    let hidden = Arc::new(TestFocusableComponent::new("HIDDEN"));
    let upper = Arc::new(TestFocusableComponent::new("UPPER"));

    let _internal_handle = mode
        .ui()
        .show_overlay(Arc::clone(&internal) as Arc<dyn Component>, None);

    let lower_c = Arc::clone(&lower);
    let _t1 = tokio::spawn({
        let ui_ctx = Arc::clone(&ui_ctx);
        async move {
            ui_ctx
                .custom(
                    Box::new(move |_t, _th, _k, _done| Box::pin(async move { Ok(lower_c as Arc<dyn Component>) })),
                    Some(ExtensionCustomOptions {
                        overlay: true,
                        ..Default::default()
                    }),
                )
                .await
        }
    });
    let hidden_c = Arc::clone(&hidden);
    let _t2 = tokio::spawn({
        let ui_ctx = Arc::clone(&ui_ctx);
        async move {
            ui_ctx
                .custom(
                    Box::new(move |_t, _th, _k, _done| Box::pin(async move { Ok(hidden_c as Arc<dyn Component>) })),
                    Some(ExtensionCustomOptions {
                        overlay: true,
                        on_handle: Some(Arc::new(|h: OverlayHandle| h.set_hidden(true))),
                        ..Default::default()
                    }),
                )
                .await
        }
    });
    let upper_c = Arc::clone(&upper);
    let _t3 = tokio::spawn({
        let ui_ctx = Arc::clone(&ui_ctx);
        async move {
            ui_ctx
                .custom(
                    Box::new(move |_t, _th, _k, _done| Box::pin(async move { Ok(upper_c as Arc<dyn Component>) })),
                    Some(ExtensionCustomOptions {
                        overlay: true,
                        ..Default::default()
                    }),
                )
                .await
        }
    });
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    flush_tui(&mode).await;
    assert!(upper.is_focused());

    mode.reset_extension_ui();
    terminal.clear_writes();
    flush_tui(&mode).await;

    let viewport = terminal.writes_text();
    assert!(viewport.contains("INTERNAL"), "viewport={viewport}");
    assert!(!viewport.contains("LOWER"), "viewport={viewport}");
    assert!(!viewport.contains("UPPER"), "viewport={viewport}");
    assert!(mode.ui().has_overlay());
    // PORT: `pi_tui::Component` has no `.dispose()`, and TS mocked `setCustomEditorComponent: vi.fn()` on a partial
    // `InteractiveMode` `this` so `ui.setFocus(editor)` never ran; real `reset_extension_ui` calls
    // `set_custom_editor_component(None)` which calls `ui.set_focus(default_editor)` (matching TS `interactive-mode.ts:3056`).
    // Verify extension overlays were torn down and neither keeps input, while the internal overlay remains active until hidden.
    terminal.send_input("x");
    flush_tui(&mode).await;
    assert!(lower.inputs().is_empty());
    assert!(upper.inputs().is_empty());

    mode.ui().hide_overlay();
    assert!(!mode.ui().has_overlay());
    mode.stop(None);
}

// ============================================================================
// InteractiveMode.createExtensionUIContext addAutocompleteProvider & setupAutocompleteProvider
// ============================================================================

struct WrapAutocomplete {
    name: String,
    calls: Arc<Mutex<Vec<String>>>,
    inner: Arc<dyn AutocompleteProvider + Send + Sync>,
    trigger_chars: Mutex<Option<Vec<String>>>,
}

impl WrapAutocomplete {
    fn new(
        name: &str,
        calls: Arc<Mutex<Vec<String>>>,
        inner: Arc<dyn AutocompleteProvider + Send + Sync>,
        triggers: Option<Vec<String>>,
    ) -> Self {
        Self {
            name: name.to_string(),
            calls,
            inner,
            trigger_chars: Mutex::new(triggers),
        }
    }
}

#[async_trait]
impl AutocompleteProvider for WrapAutocomplete {
    fn trigger_characters(&self) -> Option<Vec<String>> {
        self.trigger_chars.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_trigger_characters(&self, trigger_characters: Option<Vec<String>>) {
        *self.trigger_chars.lock().unwrap_or_else(|e| e.into_inner()) = trigger_characters;
    }

    async fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: i64,
        cursor_col: i64,
        options: GetSuggestionsOptions,
    ) -> Option<AutocompleteSuggestions> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("getSuggestions:{}", self.name));
        self.inner
            .get_suggestions(lines, cursor_line, cursor_col, options)
            .await
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: i64,
        cursor_col: i64,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("applyCompletion:{}", self.name));
        self.inner
            .apply_completion(lines, cursor_line, cursor_col, item, prefix)
    }

    fn should_trigger_file_completion(&self, lines: &[String], cursor_line: i64, cursor_col: i64) -> bool {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("shouldTrigger:{}", self.name));
        self.inner
            .should_trigger_file_completion(lines, cursor_line, cursor_col)
    }
}

#[test]
#[serial]
fn create_extension_ui_context_add_autocomplete_provider_stores_and_rebuilds_immediately() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let (mode, _, _) = ModeBuilder::new().build();
    let ui_ctx = mode.create_extension_ui_context();
    let editor = Arc::new(CaptureEditor::new("EDITOR"));
    let editor_c = Arc::clone(&editor);
    ui_ctx.set_editor_component(Some(Arc::new(move |_t, _th, _k| {
        Arc::clone(&editor_c) as Arc<dyn EditorComponent>
    })));
    let before = editor.providers().len();

    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_c = Arc::clone(&calls);
    let wrapper: AutocompleteProviderFactory =
        Arc::new(move |current| Arc::new(WrapAutocomplete::new("wrap", Arc::clone(&calls_c), current, None)));
    ui_ctx.add_autocomplete_provider(wrapper);

    let providers = editor.providers();
    assert_eq!(providers.len(), before + 1);
    let active = providers.last().expect("provider set");
    assert!(active.should_trigger_file_completion(&["foo".to_string()], 0, 3));
    assert_eq!(
        calls.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
        ["shouldTrigger:wrap"]
    );
}

#[test]
#[serial]
fn setup_autocomplete_provider_stacks_wrapper_factories_over_a_fresh_base_provider() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let (mode, _, _) = ModeBuilder::new().build();
    let ui_ctx = mode.create_extension_ui_context();
    let editor = Arc::new(CaptureEditor::new("EDITOR"));
    let editor_c = Arc::clone(&editor);
    ui_ctx.set_editor_component(Some(Arc::new(move |_t, _th, _k| {
        Arc::clone(&editor_c) as Arc<dyn EditorComponent>
    })));

    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_1 = Arc::clone(&calls);
    let calls_2 = Arc::clone(&calls);
    ui_ctx.add_autocomplete_provider(Arc::new(move |current| {
        Arc::new(WrapAutocomplete::new("wrap1", Arc::clone(&calls_1), current, None))
    }));
    ui_ctx.add_autocomplete_provider(Arc::new(move |current| {
        Arc::new(WrapAutocomplete::new("wrap2", Arc::clone(&calls_2), current, None))
    }));

    calls.lock().unwrap_or_else(|e| e.into_inner()).clear();
    mode.setup_autocomplete_provider();

    let providers = editor.providers();
    let provider = providers.last().expect("provider set");
    assert!(provider.should_trigger_file_completion(&["foo".to_string()], 0, 3));
    assert_eq!(
        calls.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
        ["shouldTrigger:wrap2", "shouldTrigger:wrap1"]
    );
}

#[test]
#[serial]
fn setup_autocomplete_provider_merges_trigger_characters_from_wrapper_factories() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let (mode, _, _) = ModeBuilder::new().build();
    let ui_ctx = mode.create_extension_ui_context();
    let editor = Arc::new(CaptureEditor::new("EDITOR"));
    let editor_c = Arc::clone(&editor);
    ui_ctx.set_editor_component(Some(Arc::new(move |_t, _th, _k| {
        Arc::clone(&editor_c) as Arc<dyn EditorComponent>
    })));

    let dummy_calls = Arc::new(Mutex::new(Vec::new()));
    let c1 = Arc::clone(&dummy_calls);
    let c2 = Arc::clone(&dummy_calls);
    ui_ctx.add_autocomplete_provider(Arc::new(move |current| {
        Arc::new(WrapAutocomplete::new(
            "p1",
            Arc::clone(&c1),
            current,
            Some(vec!["$".to_string()]),
        ))
    }));
    ui_ctx.add_autocomplete_provider(Arc::new(move |current| {
        Arc::new(WrapAutocomplete::new(
            "p2",
            Arc::clone(&c2),
            current,
            Some(vec!["!".to_string()]),
        ))
    }));

    let providers = editor.providers();
    let provider = providers.last().expect("provider set");
    assert_eq!(
        provider.trigger_characters(),
        Some(vec!["$".to_string(), "!".to_string()])
    );
}

// ============================================================================
// InteractiveMode.createBaseAutocompleteProvider (via setup_autocomplete_provider)
// ============================================================================

#[tokio::test]
#[serial]
async fn create_base_autocomplete_provider_matches_model_command_arguments_across_provider_model_order() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let mut b = ModeBuilder::new();
    b.session_manager.cwd = "/tmp".to_string();
    b.model_runtime.models = vec![
        make_model("github-copilot", "gpt-5.2-codex", "GPT-5.2 Codex"),
        make_model("openai-codex", "gpt-5.5", "GPT-5.5"),
    ];
    let (mode, _, _) = b.build();

    let ui_ctx = mode.create_extension_ui_context();
    let editor = Arc::new(CaptureEditor::new("EDITOR"));
    let editor_c = Arc::clone(&editor);
    ui_ctx.set_editor_component(Some(Arc::new(move |_t, _th, _k| {
        Arc::clone(&editor_c) as Arc<dyn EditorComponent>
    })));

    mode.setup_autocomplete_provider();
    let providers = editor.providers();
    let provider = providers.last().expect("provider set");

    let line = "/model codexgpt".to_string();
    let suggestions = provider
        .get_suggestions(
            &[line.clone()],
            0,
            line.len() as i64,
            GetSuggestionsOptions {
                signal: AbortController::new().signal(),
                force: None,
            },
        )
        .await
        .expect("suggestions");

    let values: Vec<&str> = suggestions.items.iter().map(|i| i.value.as_str()).collect();
    assert_eq!(values, vec!["openai-codex/gpt-5.5", "github-copilot/gpt-5.2-codex"]);
}

#[tokio::test]
#[serial]
async fn create_base_autocomplete_provider_matches_login_command_arguments_by_provider_id_and_name() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let mut b = ModeBuilder::new();
    b.session_manager.cwd = "/tmp".to_string();
    b.model_runtime.providers = vec![
        ProviderAuthInfo {
            id: "anthropic".to_string(),
            name: "Anthropic".to_string(),
            has_oauth: true,
            oauth_method_name: None,
            oauth_login_label: None,
            oauth_is_subscription: true,
            has_api_key: true,
            api_key_method_name: None,
            api_key_has_login: false,
        },
        ProviderAuthInfo {
            id: "openai".to_string(),
            name: "OpenAI".to_string(),
            has_oauth: false,
            oauth_method_name: None,
            oauth_login_label: None,
            oauth_is_subscription: false,
            has_api_key: true,
            api_key_method_name: None,
            api_key_has_login: false,
        },
        ProviderAuthInfo {
            id: "radius".to_string(),
            name: "Radius".to_string(),
            has_oauth: true,
            oauth_method_name: None,
            oauth_login_label: None,
            oauth_is_subscription: false,
            has_api_key: false,
            api_key_method_name: None,
            api_key_has_login: false,
        },
    ];
    let (mode, _, _) = b.build();

    let ui_ctx = mode.create_extension_ui_context();
    let editor = Arc::new(CaptureEditor::new("EDITOR"));
    let editor_c = Arc::clone(&editor);
    ui_ctx.set_editor_component(Some(Arc::new(move |_t, _th, _k| {
        Arc::clone(&editor_c) as Arc<dyn EditorComponent>
    })));

    mode.setup_autocomplete_provider();
    let providers = editor.providers();
    let provider = providers.last().expect("provider set");

    let line = "/login subscription anthrop".to_string();
    let suggestions = provider
        .get_suggestions(
            &[line.clone()],
            0,
            line.len() as i64,
            GetSuggestionsOptions {
                signal: AbortController::new().signal(),
                force: None,
            },
        )
        .await
        .expect("suggestions");
    assert_eq!(
        suggestions.items,
        vec![AutocompleteItem {
            value: "anthropic".to_string(),
            label: "anthropic".to_string(),
            description: Some("Anthropic · subscription/API key".to_string()),
        }]
    );

    let radius_line = "/login radius".to_string();
    let radius_suggestions = provider
        .get_suggestions(
            &[radius_line.clone()],
            0,
            radius_line.len() as i64,
            GetSuggestionsOptions {
                signal: AbortController::new().signal(),
                force: None,
            },
        )
        .await
        .expect("radius suggestions");
    assert_eq!(
        radius_suggestions.items,
        vec![AutocompleteItem {
            value: "radius".to_string(),
            label: "radius".to_string(),
            description: Some("Radius · account".to_string()),
        }]
    );
}

// ============================================================================
// InteractiveMode.showLoadedResources
// ============================================================================

#[test]
#[serial]
fn show_loaded_resources_shows_compact_resource_listing_by_default() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let mut b = ModeBuilder::new();
    b.resource_loader.skills = vec![SkillInfo {
        name: "commit".to_string(),
        description: String::new(),
        file_path: "/tmp/skill/SKILL.md".to_string(),
        source_info: None,
    }];
    let (mode, _, _) = b.build();

    mode.show_loaded_resources(None, false, false);
    let output = render_all(mode.loaded_resources_container(), 120);
    assert!(output.contains("[Skills]"), "output={output}");
    assert!(output.contains("commit"), "output={output}");
    assert!(!output.contains("/tmp/skill/SKILL.md"), "output={output}");
}

#[test]
#[serial]
fn show_loaded_resources_shows_full_resource_listing_when_expanded() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let mut b = ModeBuilder::new();
    b.resource_loader.skills = vec![SkillInfo {
        name: "commit".to_string(),
        description: String::new(),
        file_path: "/tmp/skill/SKILL.md".to_string(),
        source_info: None,
    }];
    let (mode, _, _) = b.build();
    mode.create_extension_ui_context().set_tools_expanded(true);

    mode.show_loaded_resources(None, false, false);
    // PORT: Uses real `build_scope_groups` / `format_scope_groups` rather than the TS `"resource-list"` stub.
    let output = render_all(mode.loaded_resources_container(), 120);
    assert!(output.contains("[Skills]"), "output={output}");
    assert!(output.contains("/tmp/skill/SKILL.md"), "output={output}");
    assert!(!output.contains("  commit"), "output={output}");
}

#[test]
#[serial]
fn show_loaded_resources_shows_full_listing_on_verbose_startup_even_when_collapsed() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let mut b = ModeBuilder::new();
    b.settings.quiet_startup = QuietStartup::True;
    b.verbose = true;
    b.resource_loader.skills = vec![SkillInfo {
        name: "commit".to_string(),
        description: String::new(),
        file_path: "/tmp/skill/SKILL.md".to_string(),
        source_info: None,
    }];
    let (mode, _, _) = b.build();

    mode.show_loaded_resources(None, false, false);
    let output = render_all(mode.loaded_resources_container(), 120);
    assert!(output.contains("[Skills]"), "output={output}");
    assert!(output.contains("/tmp/skill/SKILL.md"), "output={output}");
    assert!(!output.contains("  commit"), "output={output}");
}

#[test]
#[serial]
fn show_loaded_resources_abbreviates_extensions_in_compact_listing() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let mut b = ModeBuilder::new();
    b.resource_loader.extensions = LoadedExtensionsResult {
        extensions: vec![
            LoadedExtensionInfo {
                path: "/tmp/extensions/answer.ts".to_string(),
                hidden: false,
                source_info: None,
            },
            LoadedExtensionInfo {
                path: "/tmp/extensions/btw.ts".to_string(),
                hidden: false,
                source_info: None,
            },
        ],
        ..Default::default()
    };
    let (mode, _, _) = b.build();

    mode.show_loaded_resources(None, false, false);
    let output = render_all(mode.loaded_resources_container(), 120);
    assert!(output.contains("[Extensions]"), "output={output}");
    assert!(output.contains("answer.ts, btw.ts"), "output={output}");
    assert!(!output.contains("extensions/answer.ts"), "output={output}");
}

#[test]
#[serial]
fn show_loaded_resources_captures_mixed_extension_layouts_in_compact_output() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let (mode, _, _) = ModeBuilder::new().build();
    mode.show_loaded_resources(Some(create_extension_fixtures()), false, false);

    assert_eq!(
        normalize_rendered_output(mode.loaded_resources_container(), 220),
        "[Extensions]\n  @scope/pi-scoped, answer.ts, cli-extension.ts, HazAT/pi-interactive-subagents, HazAT/pi-interactive-subagents:subagents, local-index, pi-markdown-preview, user-index"
    );
}

#[test]
#[serial]
fn show_loaded_resources_adds_more_parent_folders_until_local_extension_labels_are_unique() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let extensions = vec![
        (
            "/tmp/alpha/one/index.ts".to_string(),
            Some(create_source_info(
                "/tmp/alpha/one/index.ts",
                "cli",
                "temporary",
                "top-level",
                Some("/tmp/alpha"),
            )),
        ),
        (
            "/tmp/beta/one/index.ts".to_string(),
            Some(create_source_info(
                "/tmp/beta/one/index.ts",
                "cli",
                "temporary",
                "top-level",
                Some("/tmp/beta"),
            )),
        ),
        (
            "/tmp/gamma/one/index.ts".to_string(),
            Some(create_source_info(
                "/tmp/gamma/one/index.ts",
                "cli",
                "temporary",
                "top-level",
                Some("/tmp/gamma"),
            )),
        ),
    ];
    let (mode, _, _) = ModeBuilder::new().build();
    mode.show_loaded_resources(Some(extensions), false, false);

    assert_eq!(
        normalize_rendered_output(mode.loaded_resources_container(), 220),
        "[Extensions]\n  alpha/one, beta/one, gamma/one"
    );
}

#[test]
#[serial]
fn show_loaded_resources_strips_index_ts_from_local_extension_label() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let extensions = vec![(
        "/tmp/extensions/plan-mode/index.ts".to_string(),
        Some(create_source_info(
            "/tmp/extensions/plan-mode/index.ts",
            "local",
            "project",
            "top-level",
            Some("/tmp/extensions"),
        )),
    )];
    let (mode, _, _) = ModeBuilder::new().build();
    mode.show_loaded_resources(Some(extensions), false, false);

    assert_eq!(
        normalize_rendered_output(mode.loaded_resources_container(), 220),
        "[Extensions]\n  plan-mode"
    );
}

#[test]
#[serial]
fn show_loaded_resources_strips_index_js_from_local_extension_label() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let extensions = vec![(
        "/tmp/extensions/plan-mode/index.js".to_string(),
        Some(create_source_info(
            "/tmp/extensions/plan-mode/index.js",
            "local",
            "project",
            "top-level",
            Some("/tmp/extensions"),
        )),
    )];
    let (mode, _, _) = ModeBuilder::new().build();
    mode.show_loaded_resources(Some(extensions), false, false);

    assert_eq!(
        normalize_rendered_output(mode.loaded_resources_container(), 220),
        "[Extensions]\n  plan-mode"
    );
}

#[test]
#[serial]
fn show_loaded_resources_mixed_single_file_and_subdirectory_index_ts_extensions() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let extensions = vec![
        (
            "/tmp/extensions/webfetch.ts".to_string(),
            Some(create_source_info(
                "/tmp/extensions/webfetch.ts",
                "local",
                "project",
                "top-level",
                Some("/tmp/extensions"),
            )),
        ),
        (
            "/tmp/extensions/plan-mode/index.ts".to_string(),
            Some(create_source_info(
                "/tmp/extensions/plan-mode/index.ts",
                "local",
                "project",
                "top-level",
                Some("/tmp/extensions"),
            )),
        ),
    ];
    let (mode, _, _) = ModeBuilder::new().build();
    mode.show_loaded_resources(Some(extensions), false, false);

    assert_eq!(
        normalize_rendered_output(mode.loaded_resources_container(), 220),
        "[Extensions]\n  plan-mode, webfetch.ts"
    );
}

#[test]
#[serial]
fn show_loaded_resources_multiple_index_ts_with_unique_parent_dirs_need_no_disambiguation() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let extensions = vec![
        (
            "/tmp/extensions/foo/index.ts".to_string(),
            Some(create_source_info(
                "/tmp/extensions/foo/index.ts",
                "local",
                "project",
                "top-level",
                Some("/tmp/extensions"),
            )),
        ),
        (
            "/tmp/extensions/bar/index.ts".to_string(),
            Some(create_source_info(
                "/tmp/extensions/bar/index.ts",
                "local",
                "project",
                "top-level",
                Some("/tmp/extensions"),
            )),
        ),
    ];
    let (mode, _, _) = ModeBuilder::new().build();
    mode.show_loaded_resources(Some(extensions), false, false);

    assert_eq!(
        normalize_rendered_output(mode.loaded_resources_container(), 220),
        "[Extensions]\n  bar, foo"
    );
}

#[test]
#[serial]
fn show_loaded_resources_multiple_index_ts_with_same_parent_dir_disambiguated_with_grandparent() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let extensions = vec![
        (
            "/tmp/alpha/tools/index.ts".to_string(),
            Some(create_source_info(
                "/tmp/alpha/tools/index.ts",
                "cli",
                "temporary",
                "top-level",
                Some("/tmp/alpha"),
            )),
        ),
        (
            "/tmp/beta/tools/index.ts".to_string(),
            Some(create_source_info(
                "/tmp/beta/tools/index.ts",
                "cli",
                "temporary",
                "top-level",
                Some("/tmp/beta"),
            )),
        ),
    ];
    let (mode, _, _) = ModeBuilder::new().build();
    mode.show_loaded_resources(Some(extensions), false, false);

    assert_eq!(
        normalize_rendered_output(mode.loaded_resources_container(), 220),
        "[Extensions]\n  alpha/tools, beta/tools"
    );
}

#[test]
#[serial]
fn show_loaded_resources_non_index_file_in_subdirectory_stays_as_filename() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let extensions = vec![(
        "/tmp/extensions/my-ext/main.ts".to_string(),
        Some(create_source_info(
            "/tmp/extensions/my-ext/main.ts",
            "local",
            "project",
            "top-level",
            Some("/tmp/extensions"),
        )),
    )];
    let (mode, _, _) = ModeBuilder::new().build();
    mode.show_loaded_resources(Some(extensions), false, false);

    assert_eq!(
        normalize_rendered_output(mode.loaded_resources_container(), 220),
        "[Extensions]\n  main.ts"
    );
}

#[test]
#[serial]
fn show_loaded_resources_package_extensions_strip_index_ts_correctly() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let extensions = vec![(
        "/tmp/project/.pi/npm/node_modules/pi-markdown-preview/extensions/index.ts".to_string(),
        Some(create_source_info(
            "/tmp/project/.pi/npm/node_modules/pi-markdown-preview/extensions/index.ts",
            "npm:pi-markdown-preview",
            "project",
            "package",
            Some("/tmp/project/.pi/npm/node_modules/pi-markdown-preview"),
        )),
    )];
    let (mode, _, _) = ModeBuilder::new().build();
    mode.show_loaded_resources(Some(extensions), false, false);

    assert_eq!(
        normalize_rendered_output(mode.loaded_resources_container(), 220),
        "[Extensions]\n  pi-markdown-preview"
    );
}

#[test]
#[serial]
fn show_loaded_resources_labels_npm_sibling_extensions_relative_to_declaring_package() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let extensions = vec![
        (
            "/tmp/project/.pi/npm/node_modules/primary-package/index.ts".to_string(),
            Some(create_source_info(
                "/tmp/project/.pi/npm/node_modules/primary-package/index.ts",
                "npm:primary-package",
                "project",
                "package",
                Some("/tmp/project/.pi/npm/node_modules/primary-package"),
            )),
        ),
        (
            "/tmp/project/.pi/npm/node_modules/sibling-package/index.ts".to_string(),
            Some(create_source_info(
                "/tmp/project/.pi/npm/node_modules/sibling-package/index.ts",
                "npm:primary-package",
                "project",
                "package",
                Some("/tmp/project/.pi/npm/node_modules/primary-package"),
            )),
        ),
    ];
    let (mode, _, _) = ModeBuilder::new().build();
    mode.show_loaded_resources(Some(extensions), false, false);

    assert_eq!(
        normalize_rendered_output(mode.loaded_resources_container(), 220),
        "[Extensions]\n  primary-package, primary-package:../sibling-package"
    );
}

#[test]
#[serial]
fn show_loaded_resources_labels_windows_npm_sibling_extensions_relative_to_declaring_package() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let primary_path = r"C:\Users\me\.pi\agent\npm\node_modules\primary-package\index.ts";
    let sibling_path = r"C:\Users\me\.pi\agent\npm\node_modules\sibling-package\index.ts";
    let base_dir = r"C:\Users\me\.pi\agent\npm\node_modules\primary-package";
    let extensions = vec![
        (
            primary_path.to_string(),
            Some(create_source_info(
                primary_path,
                "npm:primary-package",
                "user",
                "package",
                Some(base_dir),
            )),
        ),
        (
            sibling_path.to_string(),
            Some(create_source_info(
                sibling_path,
                "npm:primary-package",
                "user",
                "package",
                Some(base_dir),
            )),
        ),
    ];
    let (mode, _, _) = ModeBuilder::new().build();
    mode.show_loaded_resources(Some(extensions), false, false);

    assert_eq!(
        normalize_rendered_output(mode.loaded_resources_container(), 220),
        "[Extensions]\n  primary-package, primary-package:../sibling-package"
    );
}

#[test]
#[serial]
fn show_loaded_resources_captures_mixed_extension_layouts_in_expanded_output() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let (mode, _, _) = ModeBuilder::new().build();
    mode.create_extension_ui_context().set_tools_expanded(true);
    mode.show_loaded_resources(Some(create_extension_fixtures()), false, false);

    assert_eq!(
        normalize_rendered_output(mode.loaded_resources_container(), 220),
        "[Extensions]\n  project\n    /tmp/project/.pi/extensions/answer.ts\n    /tmp/project/.pi/extensions/local-index\n    git:github.com/HazAT/pi-interactive-subagents\n      extensions\n      extensions/subagents\n    npm:@scope/pi-scoped\n      extensions\n    npm:pi-markdown-preview\n      extensions\n  user\n    /tmp/agent/extensions/user-index\n  path\n    /tmp/temp/cli-extension.ts"
    );
}

#[test]
#[serial]
fn show_loaded_resources_shows_context_paths_relative_to_cwd_while_preserving_external_paths() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let home = pi_js::env::home_dir();
    let cwd = pi_js::path::join(&[&home, "Development", "pi-mono"]);
    let mut b = ModeBuilder::new();
    b.session_manager.cwd = cwd.clone();
    b.resource_loader.agents_files = vec![
        SourceInfo {
            path: pi_js::path::join(&[&home, ".pi", "agent", "AGENTS.md"]),
            ..Default::default()
        },
        SourceInfo {
            path: pi_js::path::join(&[&cwd, "AGENTS.md"]),
            ..Default::default()
        },
    ];
    let (mode, _, _) = b.build();

    mode.show_loaded_resources(None, false, false);
    let output = render_all(mode.loaded_resources_container(), 220).replace('\\', "/");
    let norm_cwd = cwd.replace('\\', "/");
    assert!(output.contains("[Context]"), "output={output}");
    assert!(output.contains("~/.pi/agent/AGENTS.md, AGENTS.md"), "output={output}");
    assert!(!output.contains(&format!("{norm_cwd}/AGENTS.md")), "output={output}");
}

#[test]
#[serial]
fn show_loaded_resources_shows_system_prompt_context_paths_before_project_context_files() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let cwd = "/tmp/project";
    let mut b = ModeBuilder::new();
    b.session_manager.cwd = cwd.to_string();
    b.resource_loader.system_prompt_source = Some(SourceInfo {
        path: pi_js::path::join(&[cwd, ".pi", "SYSTEM.md"]),
        ..Default::default()
    });
    b.resource_loader.append_system_prompt_sources = vec![SourceInfo {
        path: pi_js::path::join(&[cwd, ".pi", "APPEND_SYSTEM.md"]),
        ..Default::default()
    }];
    b.resource_loader.agents_files = vec![SourceInfo {
        path: pi_js::path::join(&[cwd, "AGENTS.md"]),
        ..Default::default()
    }];
    let (mode, _, _) = b.build();

    mode.show_loaded_resources(None, false, false);
    let output = render_all(mode.loaded_resources_container(), 220).replace('\\', "/");
    assert!(output.contains("[Context]"), "output={output}");
    assert!(
        output.contains(".pi/SYSTEM.md, .pi/APPEND_SYSTEM.md, AGENTS.md"),
        "output={output}"
    );
}

#[test]
#[serial]
fn show_loaded_resources_shows_full_context_paths_when_expanded() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let home = pi_js::env::home_dir();
    let cwd = pi_js::path::join(&[&home, "Development", "pi-mono"]);
    let mut b = ModeBuilder::new();
    b.session_manager.cwd = cwd.clone();
    b.resource_loader.agents_files = vec![
        SourceInfo {
            path: pi_js::path::join(&[&home, ".pi", "agent", "AGENTS.md"]),
            ..Default::default()
        },
        SourceInfo {
            path: pi_js::path::join(&[&cwd, "AGENTS.md"]),
            ..Default::default()
        },
    ];
    let (mode, _, _) = b.build();
    mode.create_extension_ui_context().set_tools_expanded(true);

    mode.show_loaded_resources(None, false, false);
    let output = render_all(mode.loaded_resources_container(), 220).replace('\\', "/");
    assert!(output.contains("[Context]"), "output={output}");
    assert!(output.contains("~/.pi/agent/AGENTS.md"), "output={output}");
    assert!(output.contains("~/Development/pi-mono/AGENTS.md"), "output={output}");
    assert!(!output.contains("~/.pi/agent/AGENTS.md, AGENTS.md"), "output={output}");
}

#[test]
#[serial]
fn show_loaded_resources_does_not_show_verbose_listing_on_quiet_startup_during_reload() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let mut b = ModeBuilder::new();
    b.settings.quiet_startup = QuietStartup::True;
    b.resource_loader.skills = vec![SkillInfo {
        name: "commit".to_string(),
        description: String::new(),
        file_path: "/tmp/skill/SKILL.md".to_string(),
        source_info: None,
    }];
    let (mode, _, _) = b.build();

    mode.show_loaded_resources(Some(vec![("/tmp/ext/index.ts".to_string(), None)]), false, true);
    assert!(mode.loaded_resources_container().children().is_empty());
}

#[tokio::test]
#[serial]
async fn show_loaded_resources_hides_resource_listing_but_keeps_startup_header_with_header_quiet_startup() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let mut b = ModeBuilder::new();
    b.settings.quiet_startup = QuietStartup::Header;
    b.resource_loader.skills = vec![SkillInfo {
        name: "commit".to_string(),
        description: String::new(),
        file_path: "/tmp/skill/SKILL.md".to_string(),
        source_info: None,
    }];
    let (mode, _, _) = b.build();
    mode.init().await;

    assert!(mode.loaded_resources_container().children().is_empty());
    // Startup header was built (`BuiltInHeader` + spacers), while resource listing details stay hidden.
    assert_eq!(mode.header_container().children().len(), 3);
    assert!(render_all(mode.header_container(), 120).contains("to show full startup help"));

    mode.stop(None);
}

#[tokio::test]
#[serial]
async fn show_loaded_resources_hides_startup_header_with_full_quiet_startup_unless_verbose() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let mut b_quiet = ModeBuilder::new();
    b_quiet.settings.quiet_startup = QuietStartup::True;
    let (quiet, _, _) = b_quiet.build();
    quiet.init().await;
    assert!(render_all(quiet.header_container(), 120).trim().is_empty());
    quiet.stop(None);

    let mut b_verbose = ModeBuilder::new();
    b_verbose.settings.quiet_startup = QuietStartup::Header;
    b_verbose.verbose = true;
    b_verbose.resource_loader.skills = vec![SkillInfo {
        name: "commit".to_string(),
        description: String::new(),
        file_path: "/tmp/skill/SKILL.md".to_string(),
        source_info: None,
    }];
    let (verbose, _, _) = b_verbose.build();
    verbose.init().await;
    assert!(render_all(verbose.loaded_resources_container(), 120).contains("[Skills]"));
    verbose.stop(None);
}

#[test]
#[serial]
fn show_loaded_resources_still_shows_diagnostics_on_quiet_startup_when_requested() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let mut b = ModeBuilder::new();
    b.settings.quiet_startup = QuietStartup::True;
    b.resource_loader.skills = vec![SkillInfo {
        name: "commit".to_string(),
        description: String::new(),
        file_path: "/tmp/skill/SKILL.md".to_string(),
        source_info: None,
    }];
    b.resource_loader.skill_diagnostics = vec![ResourceDiagnostic {
        diagnostic_type: DiagnosticLevel::Warning,
        message: "duplicate skill name".to_string(),
        path: None,
        collision: None,
    }];
    let (mode, _, _) = b.build();

    mode.show_loaded_resources(None, false, true);
    let output = render_all(mode.loaded_resources_container(), 120);
    assert!(output.contains("[Skill warnings]"), "output={output}");
    assert!(!output.contains("[Skills]"), "output={output}");
}
