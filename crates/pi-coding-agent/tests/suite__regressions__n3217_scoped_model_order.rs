//! Port of packages/coding-agent/test/suite/regressions/3217-scoped-model-order.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::AgentMessage;
use pi_ai::auth::types::{AuthInteraction, AuthType};
use pi_ai::{Model, ModelCost, ModelInput, ModelThinkingLevel, Transport};
use pi_coding_agent::core::agent_session::AgentSession;
use pi_coding_agent::core::sdk::ScopedModelEntry;
use pi_coding_agent::modes::interactive::bug_report::BugReportSession;
use pi_coding_agent::modes::interactive::components::footer::{
    FooterContextUsage, FooterRoutedModel, FooterSession, FooterUsageRecord,
};
use pi_coding_agent::modes::interactive::components::model_selector::{ModelSelectorComponent, ScopedModelItem};
use pi_coding_agent::modes::interactive::components::settings_selector::{CacheWarmingMode, SteeringMode, TuiMode};
use pi_coding_agent::modes::interactive::components::tool_execution::ToolRenderers;
use pi_coding_agent::modes::interactive::components::user_message_selector::UserMessageItem;
use pi_coding_agent::modes::interactive::interactive_mode::{
    AgentSessionRuntimeDiagnostic, BashExecutionResult, CacheMissNotice, CacheWarmingStatusSummary, CancelledResult,
    ClearQueueResult, CompactionSummaryResult, CycleDirection, CycleModelResult, InteractiveExtensionBindings,
    InteractiveExtensionRunner, InteractiveMode, InteractiveModeHooks, InteractiveModeOptions, InteractiveModelRuntime,
    InteractivePromptOptions, InteractiveResourceLoader, InteractiveRuntimeHost, InteractiveSession,
    InteractiveSessionManager, InteractiveSettingsManager, NavigateTreeOptions, NavigateTreeResult, PromptTemplateInfo,
    ProviderAuthInfo, ProviderAuthStatusInfo, RuntimeForkOptions, RuntimeForkResult, SessionStats,
    StoredCredentialInfo, SwitchSessionOptions, UsageBreakdownEntry,
};
use pi_coding_agent::modes::interactive::model_catalog_refresh::{ModelCatalogRefreshResult, ModelCatalogRuntime};
use pi_coding_agent::modes::interactive::session_share::ShareSession;
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_coding_agent::modes::json_event::AgentSessionEvent;
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result, Unsubscribe};
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{KeybindingsManager, TUI, TUI_KEYBINDINGS, TuiMainScreen, set_keybindings, strip_terminal_sequences};
use serial_test::serial;
use support::suite::harness::{HarnessOptions, create_harness};

struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

fn create_test_model(id: &str, name: &str) -> Model {
    Model {
        id: id.to_string(),
        name: name.to_string(),
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        base_url: "https://api.anthropic.com".to_string(),
        reasoning: false,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 200_000,
        max_tokens: 8_192,
        headers: None,
        compat: None,
        thinking_level_map: None,
        prompt_cache: None,
        sampling_params: None,
        r#type: None,
    }
}

struct FakeTerminal;

#[async_trait]
impl Terminal for FakeTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        120
    }
    fn rows(&self) -> i64 {
        30
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

struct StubModelRuntime {
    models: Vec<Model>,
}

impl ModelCatalogRuntime for StubModelRuntime {
    fn runtime_ptr_key(&self) -> usize {
        self as *const Self as usize
    }
    fn refresh(&self, _signal: AbortSignal) -> BoxFuture<Result<ModelCatalogRefreshResult>> {
        Box::pin(async { Ok(ModelCatalogRefreshResult::default()) })
    }
    fn get_available_snapshot(&self) -> Vec<Model> {
        self.models.clone()
    }
    fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
        self.models
            .iter()
            .find(|m| m.provider == provider && m.id == id)
            .cloned()
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

struct OrderSession {
    inner: Arc<AgentSession>,
    model_runtime: Arc<StubModelRuntime>,
}

impl FooterSession for OrderSession {
    fn session_ptr_key(&self) -> usize {
        FooterSession::session_ptr_key(self.inner.as_ref())
    }
    fn session_id(&self) -> String {
        FooterSession::session_id(self.inner.as_ref())
    }
    fn leaf_id(&self) -> Option<String> {
        FooterSession::leaf_id(self.inner.as_ref())
    }
    fn entry_count(&self) -> usize {
        FooterSession::entry_count(self.inner.as_ref())
    }
    fn cwd(&self) -> String {
        FooterSession::cwd(self.inner.as_ref())
    }
    fn session_name(&self) -> Option<String> {
        FooterSession::session_name(self.inner.as_ref())
    }
    fn model(&self) -> Option<Model> {
        FooterSession::model(self.inner.as_ref())
    }
    fn routed_model(&self) -> Option<FooterRoutedModel> {
        FooterSession::routed_model(self.inner.as_ref())
    }
    fn thinking_level(&self) -> String {
        FooterSession::thinking_level(self.inner.as_ref())
    }
    fn get_context_usage(&self) -> Option<FooterContextUsage> {
        FooterSession::get_context_usage(self.inner.as_ref())
    }
    fn is_using_subscription(&self, provider: &str) -> bool {
        FooterSession::is_using_subscription(self.inner.as_ref(), provider)
    }
    fn usage_records(&self) -> Vec<FooterUsageRecord> {
        FooterSession::usage_records(self.inner.as_ref())
    }
}

#[async_trait]
impl InteractiveSession for OrderSession {
    fn session_manager(&self) -> Arc<dyn InteractiveSessionManager> {
        InteractiveSession::session_manager(self.inner.as_ref())
    }
    fn settings_manager(&self) -> Arc<dyn InteractiveSettingsManager> {
        InteractiveSession::settings_manager(self.inner.as_ref())
    }
    fn model_runtime(&self) -> Arc<dyn InteractiveModelRuntime> {
        Arc::clone(&self.model_runtime) as Arc<dyn InteractiveModelRuntime>
    }
    fn resource_loader(&self) -> Arc<dyn InteractiveResourceLoader> {
        InteractiveSession::resource_loader(self.inner.as_ref())
    }
    fn extension_runner(&self) -> Arc<dyn InteractiveExtensionRunner> {
        InteractiveSession::extension_runner(self.inner.as_ref())
    }
    fn as_footer_session(&self) -> Arc<dyn FooterSession> {
        Arc::clone(&self.inner) as Arc<dyn FooterSession>
    }
    fn as_share_session(&self) -> Option<Arc<dyn ShareSession>> {
        InteractiveSession::as_share_session(self.inner.as_ref())
    }
    fn as_bug_report_session(&self) -> Option<Arc<dyn BugReportSession>> {
        InteractiveSession::as_bug_report_session(self.inner.as_ref())
    }
    fn model_thinking_level(&self) -> ModelThinkingLevel {
        InteractiveSession::model_thinking_level(self.inner.as_ref())
    }
    fn scoped_models(&self) -> Vec<ScopedModelItem> {
        InteractiveSession::scoped_models(self.inner.as_ref())
    }
    fn set_scoped_models(&self, models: Vec<ScopedModelItem>) {
        InteractiveSession::set_scoped_models(self.inner.as_ref(), models);
    }
    fn auto_compaction_enabled(&self) -> bool {
        InteractiveSession::auto_compaction_enabled(self.inner.as_ref())
    }
    fn set_auto_compaction_enabled(&self, enabled: bool) {
        InteractiveSession::set_auto_compaction_enabled(self.inner.as_ref(), enabled);
    }
    fn steering_mode(&self) -> SteeringMode {
        InteractiveSession::steering_mode(self.inner.as_ref())
    }
    fn set_steering_mode(&self, mode: SteeringMode) {
        InteractiveSession::set_steering_mode(self.inner.as_ref(), mode);
    }
    fn follow_up_mode(&self) -> SteeringMode {
        InteractiveSession::follow_up_mode(self.inner.as_ref())
    }
    fn set_follow_up_mode(&self, mode: SteeringMode) {
        InteractiveSession::set_follow_up_mode(self.inner.as_ref(), mode);
    }
    fn set_transport(&self, transport: Transport) {
        InteractiveSession::set_transport(self.inner.as_ref(), transport);
    }
    fn set_cache_warming_mode(&self, mode: CacheWarmingMode) {
        InteractiveSession::set_cache_warming_mode(self.inner.as_ref(), mode);
    }
    fn cache_warming_status(&self) -> Option<CacheWarmingStatusSummary> {
        InteractiveSession::cache_warming_status(self.inner.as_ref())
    }
    fn is_streaming(&self) -> bool {
        InteractiveSession::is_streaming(self.inner.as_ref())
    }
    fn is_idle(&self) -> bool {
        InteractiveSession::is_idle(self.inner.as_ref())
    }
    fn is_compacting(&self) -> bool {
        InteractiveSession::is_compacting(self.inner.as_ref())
    }
    fn is_bash_running(&self) -> bool {
        InteractiveSession::is_bash_running(self.inner.as_ref())
    }
    fn retry_attempt(&self) -> u32 {
        InteractiveSession::retry_attempt(self.inner.as_ref())
    }
    fn pending_message_count(&self) -> usize {
        InteractiveSession::pending_message_count(self.inner.as_ref())
    }
    fn get_steering_messages(&self) -> Vec<String> {
        InteractiveSession::get_steering_messages(self.inner.as_ref())
    }
    fn get_follow_up_messages(&self) -> Vec<String> {
        InteractiveSession::get_follow_up_messages(self.inner.as_ref())
    }
    fn clear_queue(&self) -> ClearQueueResult {
        InteractiveSession::clear_queue(self.inner.as_ref())
    }
    fn messages(&self) -> Vec<AgentMessage> {
        InteractiveSession::messages(self.inner.as_ref())
    }
    fn prompt_templates(&self) -> Vec<PromptTemplateInfo> {
        InteractiveSession::prompt_templates(self.inner.as_ref())
    }
    fn session_file(&self) -> Option<String> {
        InteractiveSession::session_file(self.inner.as_ref())
    }
    fn signal(&self) -> Option<AbortSignal> {
        InteractiveSession::signal(self.inner.as_ref())
    }
    fn get_available_thinking_levels(&self) -> Vec<ModelThinkingLevel> {
        InteractiveSession::get_available_thinking_levels(self.inner.as_ref())
    }
    fn set_thinking_level(&self, level: ModelThinkingLevel, persist: bool) -> Result<()> {
        InteractiveSession::set_thinking_level(self.inner.as_ref(), level, persist)
    }
    fn cycle_thinking_level(&self) -> Option<ModelThinkingLevel> {
        InteractiveSession::cycle_thinking_level(self.inner.as_ref())
    }
    async fn set_model(&self, model: Model, persist: bool) -> Result<()> {
        InteractiveSession::set_model(self.inner.as_ref(), model, persist).await
    }
    async fn cycle_model(&self, direction: CycleDirection) -> Result<Option<CycleModelResult>> {
        InteractiveSession::cycle_model(self.inner.as_ref(), direction).await
    }
    async fn prompt(&self, text: &str, options: Option<InteractivePromptOptions>) -> Result<()> {
        InteractiveSession::prompt(self.inner.as_ref(), text, options).await
    }
    async fn steer(&self, text: &str) -> Result<()> {
        InteractiveSession::steer(self.inner.as_ref(), text).await
    }
    async fn follow_up(&self, text: &str) -> Result<()> {
        InteractiveSession::follow_up(self.inner.as_ref(), text).await
    }
    async fn abort(&self) {
        InteractiveSession::abort(self.inner.as_ref()).await
    }
    async fn wait_for_idle(&self) {
        InteractiveSession::wait_for_idle(self.inner.as_ref()).await
    }
    fn abort_compaction(&self) {
        InteractiveSession::abort_compaction(self.inner.as_ref())
    }
    fn abort_branch_summary(&self) {
        InteractiveSession::abort_branch_summary(self.inner.as_ref())
    }
    fn abort_retry(&self) {
        InteractiveSession::abort_retry(self.inner.as_ref())
    }
    fn abort_bash(&self) {
        InteractiveSession::abort_bash(self.inner.as_ref())
    }
    fn subscribe(&self, listener: Arc<dyn Fn(AgentSessionEvent) -> BoxFuture<()> + Send + Sync>) -> Unsubscribe {
        InteractiveSession::subscribe(self.inner.as_ref(), listener)
    }
    async fn bind_extensions(&self, bindings: InteractiveExtensionBindings) -> Result<()> {
        InteractiveSession::bind_extensions(self.inner.as_ref(), bindings).await
    }
    async fn reload(&self, before_session_start: Option<Arc<dyn Fn() + Send + Sync>>) -> Result<()> {
        InteractiveSession::reload(self.inner.as_ref(), before_session_start).await
    }
    async fn compact(&self, custom_instructions: Option<&str>) -> Result<CompactionSummaryResult> {
        InteractiveSession::compact(self.inner.as_ref(), custom_instructions).await
    }
    async fn navigate_tree(&self, target_id: &str, options: NavigateTreeOptions) -> Result<NavigateTreeResult> {
        InteractiveSession::navigate_tree(self.inner.as_ref(), target_id, options).await
    }
    fn get_user_messages_for_forking(&self) -> Vec<UserMessageItem> {
        InteractiveSession::get_user_messages_for_forking(self.inner.as_ref())
    }
    fn get_session_stats(&self) -> SessionStats {
        InteractiveSession::get_session_stats(self.inner.as_ref())
    }
    fn get_last_assistant_text(&self) -> Option<String> {
        InteractiveSession::get_last_assistant_text(self.inner.as_ref())
    }
    fn set_session_name(&self, name: &str) {
        InteractiveSession::set_session_name(self.inner.as_ref(), name)
    }
    fn export_to_jsonl(&self, output_path: Option<&str>) -> Result<String> {
        InteractiveSession::export_to_jsonl(self.inner.as_ref(), output_path)
    }
    async fn export_to_html(&self, output_path: Option<&str>, theme_name: Option<&str>) -> Result<String> {
        InteractiveSession::export_to_html(self.inner.as_ref(), output_path, theme_name).await
    }
    async fn execute_bash(
        &self,
        command: &str,
        on_chunk: Arc<dyn for<'a> Fn(&'a str) + Send + Sync>,
        exclude_from_context: bool,
    ) -> Result<BashExecutionResult> {
        InteractiveSession::execute_bash(self.inner.as_ref(), command, on_chunk, exclude_from_context).await
    }
    fn record_bash_result(&self, command: &str, result: &BashExecutionResult, exclude_from_context: bool) {
        InteractiveSession::record_bash_result(self.inner.as_ref(), command, result, exclude_from_context)
    }
    fn detect_cache_miss(&self, message: &pi_ai::AssistantMessage) -> Option<CacheMissNotice> {
        InteractiveSession::detect_cache_miss(self.inner.as_ref(), message)
    }
    fn compute_cache_waste(&self) -> (i64, usize, f64) {
        InteractiveSession::compute_cache_waste(self.inner.as_ref())
    }
    fn get_usage_cost_breakdown(&self) -> Vec<UsageBreakdownEntry> {
        InteractiveSession::get_usage_cost_breakdown(self.inner.as_ref())
    }
    fn get_tool_renderers(&self, tool_name: &str) -> Option<ToolRenderers> {
        InteractiveSession::get_tool_renderers(self.inner.as_ref(), tool_name)
    }
}

struct StubHost {
    session: Arc<OrderSession>,
    agent_dir: String,
}

#[async_trait]
impl InteractiveRuntimeHost for StubHost {
    fn session(&self) -> Arc<dyn InteractiveSession> {
        Arc::clone(&self.session) as Arc<dyn InteractiveSession>
    }
    fn agent_dir(&self) -> String {
        self.agent_dir.clone()
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
            cancelled: true,
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

#[tokio::test]
#[serial]
async fn propagates_reordered_scoped_models_back_to_the_session_state() {
    init_theme(Some("dark"), Some(false));
    let _guard = Guard;

    let model_a = create_test_model("alpha-model", "Alpha Model");
    let model_b = create_test_model("beta-model", "Beta Model");
    let model_c = create_test_model("gamma-model", "Gamma Model");

    let harness = create_harness(HarnessOptions::default()).await;
    harness.session.set_scoped_models(vec![
        ScopedModelEntry {
            model: model_a.clone(),
            thinking_level: None,
        },
        ScopedModelEntry {
            model: model_b.clone(),
            thinking_level: None,
        },
    ]);
    let session = Arc::new(OrderSession {
        inner: Arc::new(harness.session.clone()),
        model_runtime: Arc::new(StubModelRuntime {
            models: vec![model_a, model_b, model_c],
        }),
    });
    let host = Arc::new(StubHost {
        session,
        agent_dir: harness.temp_dir.clone(),
    });
    let mode = InteractiveMode::new(
        host,
        InteractiveModeOptions {
            tui_mode: Some(TuiMode::Regular),
            terminal: Some(Arc::new(FakeTerminal)),
            hooks: Some(InteractiveModeHooks {
                exit_process: Arc::new(|_| {}),
                ..Default::default()
            }),
            ..Default::default()
        },
    );

    mode.show_models_selector();
    let focused = mode.renderer().get_focused_component().expect("focused selector");

    // Item 0 is alpha-model; Alt+Down moves it below beta-model.
    focused.handle_input("\x1bn");
    let reordered: Vec<String> = harness
        .session
        .scoped_models()
        .iter()
        .map(|s| s.model.id.clone())
        .collect();
    assert_eq!(reordered, vec!["beta-model".to_string(), "alpha-model".to_string()]);

    // Alt+Up moves alpha-model back above beta-model.
    focused.handle_input("\x1bp");
    let restored: Vec<String> = harness
        .session
        .scoped_models()
        .iter()
        .map(|s| s.model.id.clone())
        .collect();
    assert_eq!(restored, vec!["alpha-model".to_string(), "beta-model".to_string()]);

    mode.shutdown(false).await;
    harness.cleanup();
}

#[tokio::test]
#[serial]
async fn preserves_scoped_model_order_in_the_model_scoped_tab() {
    init_theme(Some("dark"), Some(false));
    let _guard = Guard;

    let model_a = create_test_model("alpha-model", "Alpha Model");
    let model_b = create_test_model("beta-model", "Beta Model");
    let runtime = Arc::new(StubModelRuntime {
        models: vec![model_a.clone(), model_b.clone()],
    });
    let tui: Arc<dyn TUI + Send + Sync> = Arc::new(TuiMainScreen::new(Arc::new(FakeTerminal), Some(false), None));

    let selector = ModelSelectorComponent::new(
        tui,
        Some(model_b.clone()),
        runtime,
        vec![
            ScopedModelItem {
                model: model_b,
                thinking_level: None,
            },
            ScopedModelItem {
                model: model_a,
                thinking_level: None,
            },
        ],
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );

    let rendered = strip_terminal_sequences(&selector.render(120).join("\n"));
    let beta_pos = rendered.find("beta-model").expect("beta-model rendered");
    let alpha_pos = rendered.find("alpha-model").expect("alpha-model rendered");
    assert!(
        beta_pos < alpha_pos,
        "expected beta-model before alpha-model in scoped tab, got:\n{rendered}"
    );
}
