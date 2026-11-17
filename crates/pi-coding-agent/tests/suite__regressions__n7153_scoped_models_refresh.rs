//! Port of packages/coding-agent/test/suite/regressions/7153-scoped-models-refresh.test.ts
//!
//! InteractiveMode `/scoped-models` refresh & abort-on-close regression tests.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pi_ai::auth::credential_store::InMemoryCredentialStore;
use pi_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthCheck, AuthResult, AuthType, ModelAuth, ProviderAuth,
    ProviderAuthInteraction,
};
use pi_ai::models::{ModelsRefreshOptions, Provider, RefreshModelsContext};
use pi_ai::types::{ApiStreamOptions, Model, ModelCost, ModelInput, SimpleStreamOptions, TranscriptContext};
use pi_ai::{AssistantMessageEventStream, Result};
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_js::BoxFuture;
use pi_js::abort::AbortController;
use tokio::sync::watch;

fn make_model(id: &str, name: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: name.to_owned(),
        api: "openai-completions".to_owned(),
        provider: "scoped-refresh".to_owned(),
        base_url: "https://example.test/v1".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: 8192,
        max_tokens: 1024,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

struct StubApiKeyAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for StubApiKeyAuth {
    fn name(&self) -> &str {
        "API key"
    }

    async fn check(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthCheck>> {
        Ok(Some(AuthCheck {
            auth_type: AuthType::ApiKey,
            source: Some("env".to_owned()),
        }))
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some("test-key".to_owned()),
                headers: None,
                base_url: None,
            },
            env: None,
            source: Some("env".to_owned()),
        }))
    }

    async fn login(&self, _interaction: ProviderAuthInteraction) -> Result<ApiKeyCredential> {
        Ok(ApiKeyCredential {
            key: Some("test-key".to_owned()),
            env: None,
        })
    }
}

struct ScopedRefreshProvider {
    auth: ProviderAuth,
    models: Mutex<Vec<Model>>,
    started_tx: watch::Sender<usize>,
    release_rx: watch::Receiver<bool>,
}

impl Provider for ScopedRefreshProvider {
    fn id(&self) -> &str {
        "scoped-refresh"
    }

    fn name(&self) -> &str {
        "Scoped Refresh"
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        self.models.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn refresh_models(&self, context: RefreshModelsContext) -> Option<BoxFuture<Result<()>>> {
        if !context.allow_network {
            return Some(Box::pin(async { Ok(()) }));
        }
        let started_tx = self.started_tx.clone();
        let mut release_rx = self.release_rx.clone();
        let signal = context.signal;
        Some(Box::pin(async move {
            started_tx.send_modify(|n| *n += 1);
            loop {
                if *release_rx.borrow() || signal.aborted() {
                    return Ok(());
                }
                tokio::select! {
                    changed = release_rx.changed() => {
                        if changed.is_err() || *release_rx.borrow() {
                            return Ok(());
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(5)) => {}
                }
            }
        }))
    }

    fn stream(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        panic!("unused");
    }

    fn stream_simple(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        panic!("unused");
    }
}

#[tokio::test]
async fn serves_cached_snapshot_immediately_updates_after_refresh_and_cancels_on_abort() {
    let (started_tx, mut started_rx) = watch::channel(0usize);
    let (release_tx, release_rx) = watch::channel(false);
    let provider = Arc::new(ScopedRefreshProvider {
        auth: ProviderAuth {
            api_key: Some(Arc::new(StubApiKeyAuth)),
            oauth: None,
        },
        models: Mutex::new(vec![make_model("cached", "Cached")]),
        started_tx,
        release_rx,
    });

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(InMemoryCredentialStore::new())),
        models_path: Some(None),
        allow_model_network: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .expect("create runtime");
    runtime
        .register_native_provider(provider.clone())
        .expect("register native provider");
    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["scoped-refresh".to_owned()]),
            force: None,
            signal: None,
        }))
        .await
        .expect("initial offline load");

    // 1. Cached snapshot immediately shows `cached` while background network refresh is pending.
    let initial_ids: Vec<String> = runtime
        .get_available_snapshot()
        .into_iter()
        .filter(|m| m.provider == "scoped-refresh")
        .map(|m| m.id)
        .collect();
    assert_eq!(initial_ids, vec!["cached"]);

    let rt_bg = runtime.clone();
    let controller = AbortController::new();
    let bg_signal = controller.signal();
    let bg_task = tokio::spawn(async move {
        rt_bg
            .refresh(Some(ModelsRefreshOptions {
                allow_network: Some(true),
                providers: Some(vec!["scoped-refresh".to_owned()]),
                force: Some(true),
                signal: Some(bg_signal),
            }))
            .await
    });
    while *started_rx.borrow() < 1 {
        started_rx.changed().await.unwrap();
    }

    // While in-flight, cached snapshot stays intact without blocking.
    assert!(runtime.get_available_snapshot().iter().any(|m| m.id == "cached"));
    assert!(!runtime.get_available_snapshot().iter().any(|m| m.id == "refreshed"));

    // Completing refresh updates available snapshot.
    *provider.models.lock().unwrap() = vec![make_model("cached", "Cached"), make_model("refreshed", "Refreshed")];
    let _ = release_tx.send(true);
    let res = bg_task.await.unwrap().expect("refresh complete");
    assert!(!res.aborted);

    let updated_ids: Vec<String> = runtime
        .get_available_snapshot()
        .into_iter()
        .filter(|m| m.provider == "scoped-refresh")
        .map(|m| m.id)
        .collect();
    assert!(updated_ids.contains(&"cached".to_owned()));
    assert!(updated_ids.contains(&"refreshed".to_owned()));

    // 2. Cancelling background refresh when selector closes aborts signal cleanly.
    let _ = release_tx.send(false);
    let close_controller = AbortController::new();
    let close_signal = close_controller.signal();
    let rt_cancel = runtime.clone();
    let target_start = *started_rx.borrow() + 1;
    let cancel_task = tokio::spawn(async move {
        rt_cancel
            .refresh(Some(ModelsRefreshOptions {
                allow_network: Some(true),
                providers: Some(vec!["scoped-refresh".to_owned()]),
                force: Some(true),
                signal: Some(close_signal),
            }))
            .await
    });
    while *started_rx.borrow() < target_start {
        started_rx.changed().await.unwrap();
    }
    close_controller.abort(None);
    let cancel_res = cancel_task.await.unwrap().expect("aborted refresh returns Ok");
    assert!(cancel_res.aborted);
}

#[tokio::test]
#[serial_test::serial]
async fn interactive_show_models_selector_refreshes_and_cancels_in_flight_on_close() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use pi_agent_core::AgentMessage;
    use pi_ai::auth::types::{AuthInteraction, AuthType};
    use pi_ai::{ModelThinkingLevel, Transport};
    use pi_coding_agent::core::agent_session::AgentSession;
    use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
    use pi_coding_agent::core::session_manager::SessionManager;
    use pi_coding_agent::modes::interactive::bug_report::BugReportSession;
    use pi_coding_agent::modes::interactive::components::footer::{
        FooterContextUsage, FooterRoutedModel, FooterSession, FooterUsageRecord,
    };
    use pi_coding_agent::modes::interactive::components::model_selector::ScopedModelItem;
    use pi_coding_agent::modes::interactive::components::settings_selector::{CacheWarmingMode, SteeringMode, TuiMode};
    use pi_coding_agent::modes::interactive::components::tool_execution::ToolRenderers;
    use pi_coding_agent::modes::interactive::components::user_message_selector::UserMessageItem;
    use pi_coding_agent::modes::interactive::interactive_mode::{
        AgentSessionRuntimeDiagnostic, BashExecutionResult, CacheMissNotice, CacheWarmingStatusSummary,
        CancelledResult, ClearQueueResult, CompactionSummaryResult, CycleDirection, CycleModelResult,
        InteractiveExtensionBindings, InteractiveExtensionRunner, InteractiveMode, InteractiveModeHooks,
        InteractiveModeOptions, InteractiveModelRuntime, InteractivePromptOptions, InteractiveResourceLoader,
        InteractiveRuntimeHost, InteractiveSession, InteractiveSessionManager, InteractiveSettingsManager,
        NavigateTreeOptions, NavigateTreeResult, PromptTemplateInfo, ProviderAuthInfo, ProviderAuthStatusInfo,
        RuntimeForkOptions, RuntimeForkResult, SessionStats, StoredCredentialInfo, SwitchSessionOptions,
        UsageBreakdownEntry,
    };
    use pi_coding_agent::modes::interactive::model_catalog_refresh::{ModelCatalogRefreshResult, ModelCatalogRuntime};
    use pi_coding_agent::modes::interactive::session_share::ShareSession;
    use pi_coding_agent::modes::interactive::theme::theme::init_theme;
    use pi_coding_agent::modes::json_event::AgentSessionEvent;
    use pi_js::BoxFuture;
    use pi_js::abort::AbortSignal;
    use pi_tui::strip_terminal_sequences;
    use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
    use tempfile::TempDir;
    use tokio::sync::Notify;

    struct ThemeGuard;
    impl Drop for ThemeGuard {
        fn drop(&mut self) {
            init_theme(Some("dark"), None);
        }
    }
    struct DummyTerminal;
    #[async_trait]
    impl Terminal for DummyTerminal {
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

    struct SharedRefreshRuntime {
        inner: Arc<SharedRefreshRuntimeInner>,
    }
    struct SharedRefreshRuntimeInner {
        models: Mutex<Vec<Model>>,
        next_models: Mutex<Option<Vec<Model>>>,
        entered: Arc<Notify>,
        release: Arc<Notify>,
        aborted: AtomicBool,
    }
    impl ModelCatalogRuntime for SharedRefreshRuntime {
        fn runtime_ptr_key(&self) -> usize {
            Arc::as_ptr(&self.inner) as usize
        }
        fn refresh(&self, signal: AbortSignal) -> BoxFuture<pi_coding_agent::Result<ModelCatalogRefreshResult>> {
            let inner = Arc::clone(&self.inner);
            Box::pin(async move {
                inner.entered.notify_waiters();
                tokio::select! {
                    _ = inner.release.notified() => {
                        if let Some(next) = inner.next_models.lock().unwrap().take() {
                            *inner.models.lock().unwrap() = next;
                        }
                        Ok(ModelCatalogRefreshResult::default())
                    }
                    _ = signal.cancelled() => {
                        inner.aborted.store(true, Ordering::SeqCst);
                        Ok(ModelCatalogRefreshResult { aborted: true, ..Default::default() })
                    }
                }
            })
        }
        fn get_available_snapshot(&self) -> Vec<Model> {
            self.inner.models.lock().unwrap().clone()
        }
        fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
            self.inner
                .models
                .lock()
                .unwrap()
                .iter()
                .find(|m| m.provider == provider && m.id == id)
                .cloned()
        }
        fn get_error(&self) -> Option<String> {
            None
        }
    }
    #[async_trait]
    impl InteractiveModelRuntime for SharedRefreshRuntime {
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
        async fn list_credentials(&self, _signal: AbortSignal) -> pi_coding_agent::Result<Vec<StoredCredentialInfo>> {
            Ok(Vec::new())
        }
        async fn logout(&self, _provider_id: &str, _signal: AbortSignal) -> pi_coding_agent::Result<()> {
            Ok(())
        }
        async fn refresh_providers(
            &self,
            _providers: Vec<String>,
            _signal: AbortSignal,
        ) -> pi_coding_agent::Result<ModelCatalogRefreshResult> {
            Ok(ModelCatalogRefreshResult::default())
        }
        async fn login(
            &self,
            _provider_id: &str,
            _auth_type: AuthType,
            _interaction: Arc<dyn AuthInteraction>,
            _signal: AbortSignal,
            _device_id: String,
        ) -> pi_coding_agent::Result<()> {
            Ok(())
        }
    }

    struct WrappedSession {
        inner: Arc<AgentSession>,
        rt: Arc<SharedRefreshRuntime>,
    }
    impl FooterSession for WrappedSession {
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
    impl InteractiveSession for WrappedSession {
        fn session_manager(&self) -> Arc<dyn InteractiveSessionManager> {
            InteractiveSession::session_manager(self.inner.as_ref())
        }
        fn settings_manager(&self) -> Arc<dyn InteractiveSettingsManager> {
            InteractiveSession::settings_manager(self.inner.as_ref())
        }
        fn model_runtime(&self) -> Arc<dyn InteractiveModelRuntime> {
            Arc::clone(&self.rt) as Arc<dyn InteractiveModelRuntime>
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
        fn set_thinking_level(&self, level: ModelThinkingLevel, persist: bool) -> pi_coding_agent::Result<()> {
            InteractiveSession::set_thinking_level(self.inner.as_ref(), level, persist)
        }
        fn cycle_thinking_level(&self) -> Option<ModelThinkingLevel> {
            InteractiveSession::cycle_thinking_level(self.inner.as_ref())
        }
        async fn set_model(&self, model: Model, persist: bool) -> pi_coding_agent::Result<()> {
            InteractiveSession::set_model(self.inner.as_ref(), model, persist).await
        }
        async fn cycle_model(&self, direction: CycleDirection) -> pi_coding_agent::Result<Option<CycleModelResult>> {
            InteractiveSession::cycle_model(self.inner.as_ref(), direction).await
        }
        async fn prompt(&self, text: &str, options: Option<InteractivePromptOptions>) -> pi_coding_agent::Result<()> {
            InteractiveSession::prompt(self.inner.as_ref(), text, options).await
        }
        async fn steer(&self, text: &str) -> pi_coding_agent::Result<()> {
            InteractiveSession::steer(self.inner.as_ref(), text).await
        }
        async fn follow_up(&self, text: &str) -> pi_coding_agent::Result<()> {
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
        fn subscribe(
            &self,
            listener: Arc<dyn Fn(AgentSessionEvent) -> BoxFuture<()> + Send + Sync>,
        ) -> pi_js::Unsubscribe {
            InteractiveSession::subscribe(self.inner.as_ref(), listener)
        }
        async fn bind_extensions(&self, bindings: InteractiveExtensionBindings) -> pi_coding_agent::Result<()> {
            InteractiveSession::bind_extensions(self.inner.as_ref(), bindings).await
        }
        async fn reload(
            &self,
            before_session_start: Option<Arc<dyn Fn() + Send + Sync>>,
        ) -> pi_coding_agent::Result<()> {
            InteractiveSession::reload(self.inner.as_ref(), before_session_start).await
        }
        async fn compact(&self, custom_instructions: Option<&str>) -> pi_coding_agent::Result<CompactionSummaryResult> {
            InteractiveSession::compact(self.inner.as_ref(), custom_instructions).await
        }
        async fn navigate_tree(
            &self,
            target_id: &str,
            options: NavigateTreeOptions,
        ) -> pi_coding_agent::Result<NavigateTreeResult> {
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
        fn export_to_jsonl(&self, output_path: Option<&str>) -> pi_coding_agent::Result<String> {
            InteractiveSession::export_to_jsonl(self.inner.as_ref(), output_path)
        }
        async fn export_to_html(
            &self,
            output_path: Option<&str>,
            theme_name: Option<&str>,
        ) -> pi_coding_agent::Result<String> {
            InteractiveSession::export_to_html(self.inner.as_ref(), output_path, theme_name).await
        }
        async fn execute_bash(
            &self,
            command: &str,
            on_chunk: Arc<dyn for<'a> Fn(&'a str) + Send + Sync>,
            exclude_from_context: bool,
        ) -> pi_coding_agent::Result<BashExecutionResult> {
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
        session: Arc<WrappedSession>,
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
        async fn new_session(&self) -> pi_coding_agent::Result<CancelledResult> {
            Ok(CancelledResult { cancelled: false })
        }
        async fn fork(
            &self,
            _entry_id: &str,
            _options: Option<RuntimeForkOptions>,
        ) -> pi_coding_agent::Result<RuntimeForkResult> {
            Ok(RuntimeForkResult {
                cancelled: true,
                selected_text: None,
            })
        }
        async fn switch_session(
            &self,
            _session_path: &str,
            _options: Option<SwitchSessionOptions>,
        ) -> pi_coding_agent::Result<CancelledResult> {
            Ok(CancelledResult { cancelled: false })
        }
        async fn import_from_jsonl(
            &self,
            _input_path: &str,
            _cwd_override: Option<&str>,
        ) -> pi_coding_agent::Result<CancelledResult> {
            Ok(CancelledResult { cancelled: false })
        }
        async fn dispose(&self) {}
    }

    init_theme(Some("dark"), Some(false));
    let _guard = ThemeGuard;

    let dir = TempDir::new().expect("tempdir");
    let cwd = dir.path().to_string_lossy().into_owned();
    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some(cwd.clone()),
        session_manager: Some(SessionManager::in_memory(Some(&cwd), None, None).expect("sm")),
        ..Default::default()
    }))
    .await
    .expect("session");

    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let cached = make_model("cached-model", "Cached Model");
    let fresh = make_model("fresh-model", "Fresh Model");
    let rt_inner = Arc::new(SharedRefreshRuntimeInner {
        models: Mutex::new(vec![cached.clone()]),
        next_models: Mutex::new(Some(vec![cached.clone(), fresh])),
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
        aborted: AtomicBool::new(false),
    });
    let rt = Arc::new(SharedRefreshRuntime {
        inner: Arc::clone(&rt_inner),
    });
    let session = Arc::new(WrappedSession {
        inner: Arc::new(created.session.clone()),
        rt: Arc::clone(&rt),
    });
    let host = Arc::new(StubHost {
        session,
        agent_dir: cwd,
    });
    let mode = InteractiveMode::new(
        host,
        InteractiveModeOptions {
            tui_mode: Some(TuiMode::Regular),
            terminal: Some(Arc::new(DummyTerminal)),
            hooks: Some(InteractiveModeHooks {
                exit_process: Arc::new(|_| {}),
                ..Default::default()
            }),
            ..Default::default()
        },
    );

    mode.show_models_selector();
    let immediate = strip_terminal_sequences(&mode.editor_container().render(120).join("\n"));
    assert!(immediate.contains("cached-model"), "immediate:\n{immediate}");
    assert!(
        immediate.contains("Refreshing model catalogs"),
        "immediate:\n{immediate}"
    );

    entered.notified().await;
    release.notify_waiters();

    let mut refreshed = String::new();
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        refreshed = strip_terminal_sequences(&mode.editor_container().render(120).join("\n"));
        if refreshed.contains("fresh-model") && refreshed.contains("Model catalogs refreshed.") {
            break;
        }
    }
    assert!(refreshed.contains("fresh-model"), "refreshed:\n{refreshed}");
    assert!(
        refreshed.contains("Model catalogs refreshed."),
        "refreshed:\n{refreshed}"
    );

    // Close selector while second refresh is pending -> cancels background refresh abort signal.
    let focused = mode.renderer().get_focused_component().expect("focused selector");
    focused.handle_input("\x1b");
    mode.show_models_selector();
    entered.notified().await;
    let focused2 = mode.renderer().get_focused_component().expect("focused selector 2");
    focused2.handle_input("\x1b");
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        if rt_inner.aborted.load(Ordering::SeqCst) {
            break;
        }
    }
    assert!(rt_inner.aborted.load(Ordering::SeqCst));

    mode.shutdown(false).await;
    created.session.dispose();
}
