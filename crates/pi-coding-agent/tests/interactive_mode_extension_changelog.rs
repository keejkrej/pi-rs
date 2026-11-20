//! Port of packages/coding-agent/test/interactive-mode-extension-changelog.test.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_ai::auth::types::{AuthInteraction, AuthType};
use pi_ai::{AssistantMessage, Message, Model, ModelThinkingLevel};
use pi_coding_agent::core::source_info::{SourcePackageIdentity, SourceScope, get_source_package_key};
use pi_coding_agent::modes::interactive::bug_report::BugReportSession;
use pi_coding_agent::modes::interactive::components::custom_entry::EntryRenderer;
use pi_coding_agent::modes::interactive::components::custom_message::MessageRenderer;
use pi_coding_agent::modes::interactive::components::footer::{FooterRoutedModel, FooterSession, FooterUsageRecord};
use pi_coding_agent::modes::interactive::components::markdown_transform::{
    MarkdownMessageType, MarkdownTransformContext, MarkdownTransformer,
};
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
    ClearQueueResult, CompactionSummaryResult, CycleDirection, CycleModelResult, ExtensionChangelogUpdate,
    InteractiveExtensionBindings, InteractiveExtensionRunner, InteractiveMode, InteractiveModeOptions,
    InteractiveModelRuntime, InteractivePromptOptions, InteractiveResourceLoader, InteractiveRuntimeHost,
    InteractiveSession, InteractiveSessionManager, InteractiveSettingsManager, LoadedExtensionInfo,
    LoadedExtensionsResult, LoadedThemeResource, NavigateTreeOptions, NavigateTreeResult, PromptTemplateInfo,
    ProviderAuthInfo, ProviderAuthStatusInfo, RegisteredExtensionCommand, RegisteredExtensionShortcut,
    ResourceDiagnostic, RuntimeForkOptions, RuntimeForkResult, SessionStats, SkillInfo, SourceInfo,
    StoredCredentialInfo, SwitchSessionOptions, UsageBreakdownEntry,
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
use serial_test::serial;

const CHANGELOG: &str = "\
# Changelog

## [Unreleased]

- Not released yet

## [1.2.0]

- Added feature B

## [1.1.0]

- Added feature A

## [1.0.0]

- Initial release
";

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

struct MockSettings {
    ext_changelog_versions: Mutex<IndexMap<String, String>>,
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
    fn get_extension_changelog_version(&self, key: &str) -> Option<String> {
        self.ext_changelog_versions.lock().unwrap().get(key).cloned()
    }
    fn set_extension_changelog_version(&self, key: &str, version: &str) {
        self.ext_changelog_versions
            .lock()
            .unwrap()
            .insert(key.to_string(), version.to_string());
    }
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

struct MockResourceLoader {
    extensions: Mutex<Vec<LoadedExtensionInfo>>,
}
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
        LoadedExtensionsResult {
            extensions: self.extensions.lock().unwrap().clone(),
            ..Default::default()
        }
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

struct MockExtensionRunner {
    transformers: Mutex<Vec<MarkdownTransformer>>,
}
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
        self.transformers.lock().unwrap().clone()
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
    settings: Arc<MockSettings>,
    loader: Arc<MockResourceLoader>,
    runner: Arc<MockExtensionRunner>,
    messages: Mutex<Vec<AgentMessage>>,
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
        self.settings.clone()
    }
    fn model_runtime(&self) -> Arc<dyn InteractiveModelRuntime> {
        Arc::new(MockModelRuntime)
    }
    fn resource_loader(&self) -> Arc<dyn InteractiveResourceLoader> {
        self.loader.clone()
    }
    fn extension_runner(&self) -> Arc<dyn InteractiveExtensionRunner> {
        self.runner.clone()
    }
    fn as_footer_session(&self) -> Arc<dyn FooterSession> {
        Arc::new(MockSession {
            settings: self.settings.clone(),
            loader: self.loader.clone(),
            runner: self.runner.clone(),
            messages: Mutex::new(Vec::new()),
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
        self.messages.lock().unwrap().clone()
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

struct ChangelogFixture {
    _guard: TestGuard,
    _temp: tempfile::TempDir,
    pkg_dir: std::path::PathBuf,
    source_info: SourceInfo,
    settings: Arc<MockSettings>,
    loader: Arc<MockResourceLoader>,
    runner: Arc<MockExtensionRunner>,
    session: Arc<MockSession>,
    mode: InteractiveMode,
}

fn setup() -> ChangelogFixture {
    init_theme(Some("dark"), None);
    let temp = tempfile::tempdir().expect("tempdir");
    let pkg_dir = temp.path().join("pkg");
    std::fs::create_dir_all(&pkg_dir).expect("mkdir pkg");
    let changelog_path = pkg_dir.join("CHANGELOG.md");
    std::fs::write(&changelog_path, CHANGELOG).expect("write CHANGELOG.md");

    let source_info = SourceInfo {
        path: pkg_dir.join("a.ts").to_string_lossy().into_owned(),
        source: "npm:pi-demo-extension".to_string(),
        scope: "user".to_string(),
        origin: "package".to_string(),
        base_dir: Some(pkg_dir.to_string_lossy().into_owned()),
        changelog_path: Some(changelog_path.to_string_lossy().into_owned()),
    };
    let mut second_info = source_info.clone();
    second_info.path = pkg_dir.join("b.ts").to_string_lossy().into_owned();

    let settings = Arc::new(MockSettings {
        ext_changelog_versions: Mutex::new(IndexMap::new()),
    });
    let loader = Arc::new(MockResourceLoader {
        extensions: Mutex::new(vec![
            LoadedExtensionInfo {
                path: source_info.path.clone(),
                hidden: false,
                source_info: Some(source_info.clone()),
            },
            LoadedExtensionInfo {
                path: second_info.path.clone(),
                hidden: false,
                source_info: Some(second_info),
            },
        ]),
    });
    let runner = Arc::new(MockExtensionRunner {
        transformers: Mutex::new(Vec::new()),
    });
    let session = Arc::new(MockSession {
        settings: settings.clone(),
        loader: loader.clone(),
        runner: runner.clone(),
        messages: Mutex::new(Vec::new()),
    });
    let mode = InteractiveMode::new(
        Arc::new(MockRuntimeHost(session.clone())),
        InteractiveModeOptions {
            terminal: Some(Arc::new(NoopTerminal)),
            ..Default::default()
        },
    );

    ChangelogFixture {
        _guard: TestGuard,
        _temp: temp,
        pkg_dir,
        source_info,
        settings,
        loader,
        runner,
        session,
        mode,
    }
}

fn pkg_key(info: &SourceInfo) -> String {
    let scope = match info.scope.as_str() {
        "project" => SourceScope::Project,
        "temporary" => SourceScope::Temporary,
        _ => SourceScope::User,
    };
    get_source_package_key(&SourcePackageIdentity {
        source: info.source.clone(),
        scope,
    })
}

fn write_pkg(pkg_dir: &std::path::Path, version: &str) {
    std::fs::write(
        pkg_dir.join("package.json"),
        pi_js::json::stringify(&serde_json::json!({ "name": "pi-demo-extension", "version": version })),
    )
    .expect("write package.json");
}

#[test]
#[serial]
fn records_version_on_first_launch_without_showing_changelog() {
    let fx = setup();
    write_pkg(&fx.pkg_dir, "1.0.0");

    assert!(fx.mode.get_extension_changelog_updates().is_empty());
    assert_eq!(
        fx.settings
            .get_extension_changelog_version(&pkg_key(&fx.source_info))
            .as_deref(),
        Some("1.0.0")
    );
}

#[test]
#[serial]
fn shows_entries_newer_than_last_seen_version_headed_by_package_name() {
    let fx = setup();
    write_pkg(&fx.pkg_dir, "1.0.0");
    assert!(fx.mode.get_extension_changelog_updates().is_empty());

    write_pkg(&fx.pkg_dir, "1.2.0");
    let updates = fx.mode.get_extension_changelog_updates();
    assert_eq!(
        updates,
        vec![ExtensionChangelogUpdate {
            name: "pi-demo-extension".to_string(),
            version: "1.2.0".to_string(),
            markdown:
                "## pi-demo-extension [1.2.0]\n\n- Added feature B\n\n## pi-demo-extension [1.1.0]\n\n- Added feature A"
                    .to_string(),
        }]
    );
    assert_eq!(
        fx.settings
            .get_extension_changelog_version(&pkg_key(&fx.source_info))
            .as_deref(),
        Some("1.2.0")
    );
    assert!(fx.mode.get_extension_changelog_updates().is_empty());
}

// #11703
#[test]
#[serial]
fn does_not_repeat_entries_for_prerelease_version() {
    let fx = setup();
    write_pkg(&fx.pkg_dir, "1.0.0");
    fx.mode.get_extension_changelog_updates();

    write_pkg(&fx.pkg_dir, "1.2.0-20261020.7");
    assert_eq!(fx.mode.get_extension_changelog_updates().len(), 1);
    assert!(fx.mode.get_extension_changelog_updates().is_empty());
}

#[test]
#[serial]
fn shows_nothing_for_unreadable_last_seen_version_and_records_current() {
    let fx = setup();
    let key = pkg_key(&fx.source_info);
    fx.settings.set_extension_changelog_version(&key, "latest");
    write_pkg(&fx.pkg_dir, "1.0.0");

    assert!(fx.mode.get_extension_changelog_updates().is_empty());
    assert_eq!(
        fx.settings.get_extension_changelog_version(&key).as_deref(),
        Some("1.0.0")
    );

    write_pkg(&fx.pkg_dir, "1.2.0");
    assert_eq!(fx.mode.get_extension_changelog_updates().len(), 1);
}

#[test]
#[serial]
fn skips_resumed_sessions() {
    let fx = setup();
    write_pkg(&fx.pkg_dir, "1.0.0");
    fx.mode.get_extension_changelog_updates();

    write_pkg(&fx.pkg_dir, "1.2.0");
    let user_msg: Message =
        serde_json::from_value(serde_json::json!({ "role": "user", "content": "hi", "timestamp": 1 }))
            .expect("user msg");
    fx.session
        .messages
        .lock()
        .unwrap()
        .push(AgentMessage::Message(user_msg));

    assert!(fx.mode.get_extension_changelog_updates().is_empty());
    assert_eq!(
        fx.settings
            .get_extension_changelog_version(&pkg_key(&fx.source_info))
            .as_deref(),
        Some("1.0.0")
    );
}

#[test]
#[serial]
fn ignores_non_package_extensions_and_packages_without_changelog() {
    let fx = setup();
    write_pkg(&fx.pkg_dir, "1.0.0");
    let key = pkg_key(&fx.source_info);

    let mut top = fx.source_info.clone();
    top.origin = "top-level".to_string();
    *fx.loader.extensions.lock().unwrap() = vec![LoadedExtensionInfo {
        path: top.path.clone(),
        hidden: false,
        source_info: Some(top),
    }];
    assert!(fx.mode.get_extension_changelog_updates().is_empty());

    let mut no_cl = fx.source_info.clone();
    no_cl.changelog_path = None;
    *fx.loader.extensions.lock().unwrap() = vec![LoadedExtensionInfo {
        path: no_cl.path.clone(),
        hidden: false,
        source_info: Some(no_cl),
    }];
    assert!(fx.mode.get_extension_changelog_updates().is_empty());
    assert!(fx.settings.get_extension_changelog_version(&key).is_none());
}

fn shortcode_transformer(allowed_types: &'static [MarkdownMessageType]) -> MarkdownTransformer {
    Arc::new(move |markdown: &str, ctx: MarkdownTransformContext| {
        if allowed_types.contains(&ctx.message_type) {
            markdown.replace(":warning:", "⚠")
        } else {
            markdown.to_string()
        }
    })
}

#[test]
#[serial]
fn startup_notices_run_changelog_transformers_and_skip_non_changelog_ones() {
    let fx = setup();
    *fx.runner.transformers.lock().unwrap() = vec![shortcode_transformer(&[MarkdownMessageType::Changelog])];
    fx.mode.set_startup_notices_for_test(
        Some("## [2.0.0]\n\n- :warning: Pi change".to_string()),
        vec![ExtensionChangelogUpdate {
            name: "pi-demo".to_string(),
            version: "1.2.0".to_string(),
            markdown: "## pi-demo [1.2.0]\n\n- :warning: Demo change".to_string(),
        }],
    );
    fx.mode.show_startup_notices_if_needed();
    let rendered = strip_terminal_sequences(&fx.mode.chat_container().render(120).join("\n"));
    assert!(rendered.contains("⚠ Pi change"), "rendered: {rendered}");
    assert!(rendered.contains("⚠ Demo change"), "rendered: {rendered}");
    assert!(!rendered.contains(":warning:"), "rendered: {rendered}");

    let fx2 = setup();
    *fx2.runner.transformers.lock().unwrap() = vec![shortcode_transformer(&[
        MarkdownMessageType::User,
        MarkdownMessageType::Assistant,
        MarkdownMessageType::AssistantThinking,
    ])];
    fx2.mode.set_startup_notices_for_test(
        Some("## [2.0.0]\n\n- :warning: Pi change".to_string()),
        vec![ExtensionChangelogUpdate {
            name: "pi-demo".to_string(),
            version: "1.2.0".to_string(),
            markdown: "## pi-demo [1.2.0]\n\n- :warning: Demo change".to_string(),
        }],
    );
    fx2.mode.show_startup_notices_if_needed();
    let rendered2 = strip_terminal_sequences(&fx2.mode.chat_container().render(120).join("\n"));
    assert!(rendered2.contains(":warning: Pi change"), "rendered2: {rendered2}");
    assert!(rendered2.contains(":warning: Demo change"), "rendered2: {rendered2}");
}
