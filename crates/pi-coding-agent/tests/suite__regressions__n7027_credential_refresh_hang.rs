//! Port of packages/coding-agent/test/suite/regressions/7027-credential-refresh-hang.test.ts
//!
//! InteractiveMode post-login credential refresh timeout and auto-selection tests.

use std::sync::Arc;
use std::time::Duration;

use pi_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthCheck, AuthEvent, AuthInteraction, AuthPrompt, AuthResult,
    AuthType, Credential, CredentialStore, ModelAuth, ProviderAuth, ProviderAuthInteraction,
};
use pi_ai::models::{ModelsRefreshOptions, Provider, RefreshModelsContext};
use pi_ai::types::{ApiStreamOptions, Model, ModelCost, ModelInput, SimpleStreamOptions, TranscriptContext};
use pi_ai::{AssistantMessageEventStream, Result};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_resolver::default_model_per_provider;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_js::BoxFuture;
use pi_js::abort::AbortController;
use tokio::sync::watch;

fn dynamic_model() -> Model {
    Model {
        r#type: None,
        id: "dynamic".to_owned(),
        name: "Dynamic".to_owned(),
        api: "openai-completions".to_owned(),
        provider: "stalled-login".to_owned(),
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
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

struct StalledLoginAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for StalledLoginAuth {
    fn name(&self) -> &str {
        "API key"
    }

    fn supports_login(&self) -> bool {
        true
    }

    async fn login(&self, _interaction: ProviderAuthInteraction) -> Result<ApiKeyCredential> {
        Ok(ApiKeyCredential {
            key: Some("secret".to_owned()),
            env: None,
        })
    }

    fn supports_check(&self) -> bool {
        true
    }

    async fn check(&self, input: ApiKeyAuthInput) -> Result<Option<AuthCheck>> {
        if input.credential.as_ref().and_then(|c| c.key.as_ref()).is_some() {
            Ok(Some(AuthCheck {
                auth_type: AuthType::ApiKey,
                source: Some("stored key".to_owned()),
            }))
        } else {
            Ok(None)
        }
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        let key = input
            .credential
            .and_then(|c| c.key)
            .unwrap_or_else(|| "ambient-key".to_owned());
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(key),
                headers: None,
                base_url: None,
            },
            env: None,
            source: Some("stored key".to_owned()),
        }))
    }
}

struct StalledLoginProvider {
    auth: ProviderAuth,
    model: Model,
    started_tx: watch::Sender<usize>,
    release_rx: watch::Receiver<bool>,
}

impl Provider for StalledLoginProvider {
    fn id(&self) -> &str {
        "stalled-login"
    }

    fn name(&self) -> &str {
        "Stalled Login"
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        vec![self.model.clone()]
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

struct UnusedInteraction;

#[async_trait::async_trait]
impl AuthInteraction for UnusedInteraction {
    async fn prompt(&self, _prompt: AuthPrompt) -> Result<String> {
        Ok("unused".to_owned())
    }

    fn notify(&self, _event: AuthEvent) {}
}

#[tokio::test]
async fn does_not_hold_login_behind_an_older_stalled_network_catalog_refresh() {
    let (started_tx, mut started_rx) = watch::channel(0usize);
    let (release_tx, release_rx) = watch::channel(false);
    let provider: Arc<dyn Provider + Send + Sync> = Arc::new(StalledLoginProvider {
        auth: ProviderAuth {
            api_key: Some(Arc::new(StalledLoginAuth)),
            oauth: None,
        },
        model: dynamic_model(),
        started_tx,
        release_rx,
    });

    let credentials = Arc::new(AuthStorage::in_memory(None));
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(credentials.clone()),
        models_path: Some(None),
        allow_model_network: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .expect("create runtime");
    runtime
        .register_native_provider(provider.clone())
        .expect("register stalled-login provider");
    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec![provider.id().to_owned()]),
            force: None,
            signal: None,
        }))
        .await
        .expect("initial offline refresh");

    let rt_clone = runtime.clone();
    let provider_id = provider.id().to_owned();
    let stalled_handle = tokio::spawn(async move {
        rt_clone
            .refresh(Some(ModelsRefreshOptions {
                allow_network: Some(true),
                providers: Some(vec![provider_id]),
                force: None,
                signal: None,
            }))
            .await
    });
    while *started_rx.borrow() < 1 {
        started_rx.changed().await.unwrap();
    }

    let interaction: Arc<dyn AuthInteraction> = Arc::new(UnusedInteraction);
    let cred = runtime
        .login(provider.id(), AuthType::ApiKey, interaction, None)
        .await
        .expect("login completes without blocking behind stalled refresh");
    assert_eq!(
        cred,
        Credential::ApiKey(ApiKeyCredential {
            key: Some("secret".to_owned()),
            env: None,
        })
    );
    assert!(runtime.get_available_snapshot().iter().any(|m| m.id == "dynamic"));
    assert_eq!(
        credentials.read(provider.id(), None).await.unwrap(),
        Some(Credential::ApiKey(ApiKeyCredential {
            key: Some("secret".to_owned()),
            env: None,
        }))
    );

    let _ = release_tx.send(true);
    let stalled_result = stalled_handle.await.unwrap().expect("stalled refresh result");
    assert!(!stalled_result.aborted);

    // Bounded background refresh cancels cleanly via AbortController and keeps defaultModelPerProvider contract
    assert_eq!(
        default_model_per_provider()
            .iter()
            .find(|(p, _)| *p == "radius")
            .map(|(_, id)| *id),
        Some("balanced")
    );
    let _ = release_tx.send(false);
    let controller = AbortController::new();
    let signal = controller.signal();
    let rt_abort = runtime.clone();
    let pid = provider.id().to_owned();
    let target_start = *started_rx.borrow() + 1;
    let bounded = tokio::spawn(async move {
        rt_abort
            .refresh(Some(ModelsRefreshOptions {
                allow_network: Some(true),
                providers: Some(vec![pid]),
                force: Some(true),
                signal: Some(signal),
            }))
            .await
    });
    while *started_rx.borrow() < target_start {
        started_rx.changed().await.unwrap();
    }
    controller.abort(None);
    let aborted_res = bounded.await.unwrap().expect("aborted refresh returns Ok");
    assert!(aborted_res.aborted);
}

#[tokio::test(start_paused = true)]
#[serial_test::serial]
async fn complete_provider_authentication_warns_when_refresh_times_out() {
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;
    use pi_agent_core::AgentMessage;
    use pi_ai::auth::types::AuthType;
    use pi_ai::{ModelCost, ModelInput, ModelThinkingLevel, Transport};
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

    struct HangingRefreshRuntime {
        models: Mutex<Vec<Model>>,
        refresh_entered: Arc<Notify>,
        never: Arc<Notify>,
    }
    impl ModelCatalogRuntime for HangingRefreshRuntime {
        fn runtime_ptr_key(&self) -> usize {
            self as *const Self as usize
        }
        fn refresh(&self, _signal: AbortSignal) -> BoxFuture<pi_coding_agent::Result<ModelCatalogRefreshResult>> {
            Box::pin(async { Ok(ModelCatalogRefreshResult::default()) })
        }
        fn get_available_snapshot(&self) -> Vec<Model> {
            self.models.lock().unwrap().clone()
        }
        fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
            self.models
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
    impl InteractiveModelRuntime for HangingRefreshRuntime {
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
            signal: AbortSignal,
        ) -> pi_coding_agent::Result<ModelCatalogRefreshResult> {
            self.refresh_entered.notify_waiters();
            tokio::select! {
                _ = self.never.notified() => Ok(ModelCatalogRefreshResult::default()),
                _ = signal.cancelled() => Ok(ModelCatalogRefreshResult {
                    aborted: true,
                    ..Default::default()
                }),
            }
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
        rt: Arc<HangingRefreshRuntime>,
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
    let unknown_model = Model {
        id: "unknown".to_owned(),
        name: "Unknown".to_owned(),
        api: "unknown".to_owned(),
        provider: "unknown".to_owned(),
        base_url: "http://localhost:0".to_owned(),
        reasoning: false,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 8192,
        max_tokens: 1024,
        headers: None,
        compat: None,
        thinking_level_map: None,
        prompt_cache: None,
        sampling_params: None,
        r#type: None,
        sampling_params_by_thinking_level: None,
    };
    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some(cwd.clone()),
        model: Some(unknown_model.clone()),
        session_manager: Some(SessionManager::in_memory(Some(&cwd), None, None).expect("sm")),
        ..Default::default()
    }))
    .await
    .expect("session");
    created
        .session
        .model_runtime()
        .set_runtime_api_key("anthropic", "test-anthropic-key", None)
        .await
        .expect("set runtime api key");

    let refresh_entered = Arc::new(Notify::new());
    let never = Arc::new(Notify::new());
    let sonnet = Model {
        id: "claude-sonnet-4-5".to_owned(),
        name: "Claude Sonnet 4.5".to_owned(),
        api: "anthropic-messages".to_owned(),
        provider: "anthropic".to_owned(),
        base_url: "https://api.anthropic.com".to_owned(),
        reasoning: true,
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
        sampling_params_by_thinking_level: None,
    };
    let rt = Arc::new(HangingRefreshRuntime {
        models: Mutex::new(vec![sonnet]),
        refresh_entered: Arc::clone(&refresh_entered),
        never: Arc::clone(&never),
    });
    let session = Arc::new(WrappedSession {
        inner: Arc::new(created.session.clone()),
        rt,
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

    let mode_task = mode.clone();
    let auth_fut = tokio::spawn(async move {
        mode_task
            .complete_provider_authentication("anthropic", "Anthropic", AuthType::OAuth, Some(unknown_model))
            .await;
    });

    refresh_entered.notified().await;
    tokio::time::advance(Duration::from_millis(15_001)).await;
    auth_fut.await.expect("auth task join");

    let rendered = strip_terminal_sequences(&mode.chat_container().render(120).join("\n"));
    assert!(
        rendered.contains("Logged in to Anthropic, but its model catalog refresh timed out; using cached models."),
        "rendered:\n{rendered}"
    );
    assert!(
        rendered.contains("Selected claude-sonnet-4-5"),
        "expected default model auto-selection:\n{rendered}"
    );
    assert_eq!(
        created.session.model().map(|m| m.id),
        Some("claude-sonnet-4-5".to_owned())
    );

    mode.shutdown(false).await;
    created.session.dispose();
}
