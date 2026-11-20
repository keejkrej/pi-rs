//! Port of packages/coding-agent/test/suite/regressions/5943-session-start-notify.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use pi_coding_agent::core::agent_session::{ExtensionBindings, ReloadOptions};
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, ExtensionMode, ExtensionUIContext, ExtensionUIDialogOptions, InlineExtension, NotifyType,
    SessionStartEvent,
};
use pretty_assertions::assert_eq;
use support::suite::harness::{HarnessOptions, create_harness};

struct NotifyRecorderUi {
    events: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl ExtensionUIContext for NotifyRecorderUi {
    async fn select(
        &self,
        _title: &str,
        _options: Vec<String>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }
    async fn confirm(
        &self,
        _title: &str,
        _message: &str,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<bool> {
        Ok(false)
    }
    async fn input(
        &self,
        _title: &str,
        _placeholder: Option<&str>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }
    fn notify(&self, message: &str, _notify_type: Option<NotifyType>) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(message.to_string());
    }
    fn on_terminal_input(
        &self,
        _handler: pi_coding_agent::core::extensions::types::TerminalInputHandler,
    ) -> pi_js::Unsubscribe {
        pi_js::Unsubscribe::noop()
    }
    fn set_status(&self, _key: &str, _text: Option<&str>) {}
    fn set_working_message(&self, _message: Option<&str>) {}
    fn set_working_visible(&self, _visible: bool) {}
    fn set_working_indicator(
        &self,
        _options: Option<pi_coding_agent::core::extensions::types::WorkingIndicatorOptions>,
    ) {
    }
    fn set_hidden_thinking_label(&self, _label: Option<&str>) {}
    fn set_widget_lines(
        &self,
        _key: &str,
        _content: Option<Vec<String>>,
        _options: Option<pi_coding_agent::core::extensions::types::ExtensionWidgetOptions>,
    ) {
    }
    fn set_widget_factory(
        &self,
        _key: &str,
        _factory: Option<pi_coding_agent::core::extensions::types::WidgetFactory>,
        _options: Option<pi_coding_agent::core::extensions::types::ExtensionWidgetOptions>,
    ) {
    }
    fn set_footer(&self, _factory: Option<pi_coding_agent::core::extensions::types::FooterFactory>) {}
    fn set_header(&self, _factory: Option<pi_coding_agent::core::extensions::types::HeaderFactory>) {}
    fn set_title(&self, _title: &str) {}
    async fn custom(
        &self,
        _factory: pi_coding_agent::core::extensions::types::CustomUiFactory,
        _options: Option<pi_coding_agent::core::extensions::types::ExtensionCustomOptions>,
    ) -> pi_js::Result<serde_json::Value> {
        Ok(serde_json::Value::Null)
    }
    fn paste_to_editor(&self, _text: &str) {}
    fn set_editor_text(&self, _text: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    async fn editor(&self, _title: &str, _prefill: Option<&str>) -> pi_js::Result<Option<String>> {
        Ok(None)
    }
    fn add_autocomplete_provider(
        &self,
        _factory: pi_coding_agent::core::extensions::types::AutocompleteProviderFactory,
    ) {
    }
    fn set_editor_component(&self, _factory: Option<pi_coding_agent::core::extensions::types::EditorFactory>) {}
    fn get_editor_component(&self) -> Option<pi_coding_agent::core::extensions::types::EditorFactory> {
        None
    }
    fn theme(&self) -> pi_coding_agent::core::extensions::types::Theme {
        pi_coding_agent::core::extensions::types::Theme::default()
    }
    fn get_all_themes(&self) -> Vec<pi_coding_agent::core::extensions::types::ThemeInfo> {
        Vec::new()
    }
    fn get_theme(&self, _name: &str) -> Option<pi_coding_agent::core::extensions::types::Theme> {
        None
    }
    fn set_theme_by_name(&self, _name: &str) -> pi_coding_agent::core::extensions::types::SetThemeResult {
        pi_coding_agent::core::extensions::types::SetThemeResult {
            success: false,
            error: None,
        }
    }
    fn set_theme(
        &self,
        _theme: pi_coding_agent::core::extensions::types::Theme,
    ) -> pi_coding_agent::core::extensions::types::SetThemeResult {
        pi_coding_agent::core::extensions::types::SetThemeResult {
            success: false,
            error: None,
        }
    }
    fn get_tools_expanded(&self) -> bool {
        false
    }
    fn set_tools_expanded(&self, _expanded: bool) {}
}

mod interactive_session_start_tests {
    use std::fs;
    use std::sync::{Arc, Mutex};

    use super::support::model_runtime_test_utils::{create_in_memory_model_registry, get_model_runtime};
    use super::support::suite::harness::{HarnessOptions, create_harness};
    use async_trait::async_trait;
    use pi_agent_core::AgentMessage;
    use pi_ai::compat::register_faux_provider;
    use pi_ai::{
        FauxAssistantMessageContent, FauxResponseStep, Model, ModelThinkingLevel, RegisterFauxProviderOptions,
        Transport, faux_assistant_message,
    };
    use pi_coding_agent::core::agent_session::AgentSession;
    use pi_coding_agent::core::agent_session_runtime::{
        CreateAgentSessionRuntimeFactory, CreateAgentSessionRuntimeFactoryOptions, CreateAgentSessionRuntimeOptions,
        CreateAgentSessionRuntimeResult, RuntimeNewSessionOptions, create_agent_session_runtime,
    };
    use pi_coding_agent::core::agent_session_services::{
        CreateAgentSessionFromServicesOptions, CreateAgentSessionServicesOptions, create_agent_session_from_services,
        create_agent_session_services,
    };
    use pi_coding_agent::core::auth_storage::AuthStorage;
    use pi_coding_agent::core::extensions::types::{
        CustomMessageContent, CustomMessageDraft, ExtensionApi, InlineExtension, NotifyType, SessionStartEvent,
        SessionStartReason,
    };
    use pi_coding_agent::core::extensions::types::{ProviderChatModelConfig, ProviderConfig, ProviderModelConfig};
    use pi_coding_agent::core::model_runtime::ModelRuntime;
    use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions};
    use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
    use pi_coding_agent::core::session_manager::SessionManager;
    use pi_coding_agent::core::settings_manager::{QuietStartup, Settings, SettingsManager};
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
        NavigateTreeOptions, NavigateTreeResult, PromptTemplateInfo, RuntimeForkOptions, RuntimeForkResult,
        SessionStats, SwitchSessionOptions, UsageBreakdownEntry,
    };
    use pi_coding_agent::modes::interactive::session_share::ShareSession;
    use pi_coding_agent::modes::interactive::theme::theme::init_theme;
    use pi_coding_agent::modes::json_event::AgentSessionEvent;
    use pi_js::abort::AbortSignal;
    use pi_js::{BoxFuture, Result, Unsubscribe};
    use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
    use pi_tui::{Text, strip_terminal_sequences};
    use serial_test::serial;
    use tokio::sync::Notify;

    struct ThemeResetGuard;

    impl Drop for ThemeResetGuard {
        fn drop(&mut self) {
            init_theme(Some("dark"), None);
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
            100
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

    struct SingleSessionHost {
        session: Arc<dyn InteractiveSession>,
        agent_dir: String,
    }

    #[async_trait]
    impl InteractiveRuntimeHost for SingleSessionHost {
        fn session(&self) -> Arc<dyn InteractiveSession> {
            Arc::clone(&self.session)
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

    struct SlowReloadSession {
        inner: Arc<AgentSession>,
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }

    impl FooterSession for SlowReloadSession {
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
    impl InteractiveSession for SlowReloadSession {
        fn session_manager(&self) -> Arc<dyn InteractiveSessionManager> {
            InteractiveSession::session_manager(self.inner.as_ref())
        }
        fn settings_manager(&self) -> Arc<dyn InteractiveSettingsManager> {
            InteractiveSession::settings_manager(self.inner.as_ref())
        }
        fn model_runtime(&self) -> Arc<dyn InteractiveModelRuntime> {
            InteractiveSession::model_runtime(self.inner.as_ref())
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
            self.entered.notify_waiters();
            self.release.notified().await;
            if let Some(cb) = before_session_start {
                cb();
            }
            Ok(())
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

    fn build_runtime_factory(
        ext_factory: Arc<dyn Fn(ExtensionApi) -> BoxFuture<Result<()>> + Send + Sync>,
        faux_model: Option<Model>,
        model_runtime: Option<ModelRuntime>,
    ) -> CreateAgentSessionRuntimeFactory {
        Arc::new(move |opts: CreateAgentSessionRuntimeFactoryOptions| {
            let ext_factory = Arc::clone(&ext_factory);
            let faux_model = faux_model.clone();
            let model_runtime = model_runtime.clone();
            Box::pin(async move {
                let settings_manager = SettingsManager::in_memory(
                    Some(Settings {
                        quiet_startup: Some(QuietStartup::Bool(false)),
                        ..Default::default()
                    }),
                    None,
                );
                let inline = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
                    let f = Arc::clone(&ext_factory);
                    Box::pin(async move { f(pi).await })
                }));
                let services = create_agent_session_services(CreateAgentSessionServicesOptions {
                    cwd: opts.cwd,
                    agent_dir: Some(opts.agent_dir),
                    settings_manager: Some(settings_manager),
                    model_runtime,
                    resource_loader_options: Some(DefaultResourceLoaderOptions {
                        extension_factories: vec![inline.into()],
                        no_extensions: true,
                        no_skills: true,
                        no_prompt_templates: true,
                        no_themes: true,
                        no_context_files: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .await?;
                let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
                    services: services.clone(),
                    session_manager: opts.session_manager,
                    session_start_event: opts.session_start_event,
                    model: faux_model,
                    thinking_level: None,
                    scoped_models: None,
                    tools: None,
                    exclude_tools: None,
                    no_tools: None,
                    custom_tools: None,
                })
                .await?;
                Ok(CreateAgentSessionRuntimeResult::from_session_result(
                    created,
                    services,
                    Vec::new(),
                ))
            })
        })
    }

    #[tokio::test]
    #[serial]
    async fn renders_loaded_resources_before_restored_messages_without_stale_entries() {
        init_theme(Some("dark"), Some(false));
        let _guard = ThemeResetGuard;

        let dir = tempfile::tempdir().expect("tempdir");
        let project_dir = dir.path().join("project");
        let agent_dir = dir.path().join("agent");
        fs::create_dir_all(&project_dir).expect("create project_dir");
        fs::create_dir_all(&agent_dir).expect("create agent_dir");
        fs::write(project_dir.join("AGENTS.md"), "# Test Context\n").expect("write AGENTS.md");

        let cwd = project_dir.to_string_lossy().into_owned();
        let agent_dir_str = agent_dir.to_string_lossy().into_owned();
        let settings_manager = SettingsManager::in_memory(
            Some(Settings {
                quiet_startup: Some(QuietStartup::Bool(false)),
                ..Default::default()
            }),
            None,
        );
        let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir_str.clone(),
            settings_manager: Some(settings_manager.clone()),
            no_extensions: true,
            no_skills: true,
            no_prompt_templates: true,
            no_themes: true,
            ..Default::default()
        }));
        resource_loader.reload(None).await.expect("reload resources");

        let created = create_agent_session(Some(CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(agent_dir_str.clone()),
            settings_manager: Some(settings_manager),
            session_manager: Some(SessionManager::in_memory(Some(&cwd), None, None).expect("session manager")),
            resource_loader: Some(resource_loader),
            ..Default::default()
        }))
        .await
        .expect("create session");

        let host = Arc::new(SingleSessionHost {
            session: Arc::new(created.session.clone()),
            agent_dir: agent_dir_str,
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

        mode.loaded_resources_container().add_child(Arc::new(Text::new(
            Some("stale resources"),
            Some(0),
            Some(0),
            None,
        )));
        mode.chat_container()
            .add_child(Arc::new(Text::new(Some("restored message"), Some(0), Some(0), None)));

        mode.show_loaded_resources(None, false, false);

        let rendered = strip_terminal_sequences(&mode.document_container().render(100).join("\n"));
        assert!(!rendered.contains("stale resources"));
        assert!(rendered.contains("[Context]"));
        assert!(rendered.contains("AGENTS.md"));
        let ctx_pos = rendered.find("[Context]").expect("Context heading");
        let msg_pos = rendered.find("restored message").expect("restored message");
        assert!(
            ctx_pos < msg_pos,
            "expected [Context] before restored message, got:\n{rendered}"
        );

        mode.shutdown(false).await;
        created.session.dispose();
    }

    #[tokio::test]
    #[serial]
    async fn renders_replacement_session_state_before_session_start_handlers_can_notify() {
        init_theme(Some("dark"), Some(false));
        let _guard = ThemeResetGuard;

        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path().join("project");
        let agent_dir = dir.path().join("agent");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&agent_dir).expect("agent");

        let snapshot_before_notify = Arc::new(Mutex::new(Option::<String>::None));
        let mode_slot: Arc<Mutex<Option<InteractiveMode>>> = Arc::new(Mutex::new(None));
        let mode_for_ext = Arc::clone(&mode_slot);
        let snap_for_ext = Arc::clone(&snapshot_before_notify);

        let factory = build_runtime_factory(
            Arc::new(move |pi: ExtensionApi| {
                let mode_ref = Arc::clone(&mode_for_ext);
                let snap_ref = Arc::clone(&snap_for_ext);
                Box::pin(async move {
                    pi.on::<SessionStartEvent>(move |ev, ctx| {
                        let mode_ref = Arc::clone(&mode_ref);
                        let snap_ref = Arc::clone(&snap_ref);
                        Box::pin(async move {
                            if ev.reason == SessionStartReason::New {
                                if let Some(mode) = mode_ref.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                                    let rendered =
                                        strip_terminal_sequences(&mode.chat_container().render(100).join("\n"));
                                    *snap_ref.lock().unwrap_or_else(|e| e.into_inner()) = Some(rendered);
                                }
                                if let Ok(ui) = ctx.ui() {
                                    ui.notify("REPLACEMENT_STARTED_NOTICE", Some(NotifyType::Info));
                                }
                            }
                            Ok(None)
                        })
                    });
                    Ok(())
                })
            }),
            None,
            None,
        );

        let cwd_str = cwd.to_string_lossy().into_owned();
        let agent_str = agent_dir.to_string_lossy().into_owned();
        let sm = SessionManager::in_memory(Some(&cwd_str), None, None).expect("sm");
        let runtime = Arc::new(
            create_agent_session_runtime(
                factory,
                CreateAgentSessionRuntimeOptions {
                    cwd: cwd_str.clone(),
                    agent_dir: agent_str,
                    session_manager: sm,
                    session_start_event: None,
                },
            )
            .await
            .expect("create runtime"),
        );

        let mode = InteractiveMode::new(
            runtime.clone(),
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
        *mode_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(mode.clone());
        mode.init().await;

        runtime
            .new_session(Some(RuntimeNewSessionOptions {
                setup: Some(Box::new(|new_sm| {
                    Box::pin(async move {
                        new_sm.append_custom_message_entry(
                            "restored-custom",
                            pi_coding_agent::core::messages::CustomMessageContent::Text(
                                "RESTORED_REPLACEMENT_MSG".to_string(),
                            ),
                            true,
                            None,
                        )?;
                        Ok(())
                    })
                })),
                ..Default::default()
            }))
            .await
            .expect("new_session");

        let before = snapshot_before_notify
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .expect("session_start snapshot");
        assert!(before.contains("RESTORED_REPLACEMENT_MSG"));
        assert!(!before.contains("REPLACEMENT_STARTED_NOTICE"));

        let after = strip_terminal_sequences(&mode.chat_container().render(100).join("\n"));
        let msg_pos = after.find("RESTORED_REPLACEMENT_MSG").expect("restored msg");
        let notify_pos = after.find("REPLACEMENT_STARTED_NOTICE").expect("notify msg");
        assert!(msg_pos < notify_pos);

        mode.shutdown(false).await;
        runtime.dispose().await;
    }

    #[tokio::test]
    #[serial]
    async fn subscribes_before_replacement_session_start_handlers_send_messages() {
        init_theme(Some("dark"), Some(false));
        let _guard = ThemeResetGuard;

        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path().join("project");
        let agent_dir = dir.path().join("agent");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&agent_dir).expect("agent");

        let factory = build_runtime_factory(
            Arc::new(move |pi: ExtensionApi| {
                let pi_for_handler = pi.clone();
                Box::pin(async move {
                    pi.on::<SessionStartEvent>(move |ev, _ctx| {
                        let pi_send = pi_for_handler.clone();
                        Box::pin(async move {
                            if ev.reason == SessionStartReason::New {
                                pi_send.send_message(
                                    CustomMessageDraft {
                                        custom_type: "startup-notice".to_string(),
                                        content: CustomMessageContent::Text("START_CUSTOM_MSG".to_string()),
                                        display: true,
                                        details: None,
                                    },
                                    None,
                                )?;
                            }
                            Ok(None)
                        })
                    });
                    Ok(())
                })
            }),
            None,
            None,
        );

        let cwd_str = cwd.to_string_lossy().into_owned();
        let agent_str = agent_dir.to_string_lossy().into_owned();
        let sm = SessionManager::in_memory(Some(&cwd_str), None, None).expect("sm");
        let runtime = Arc::new(
            create_agent_session_runtime(
                factory,
                CreateAgentSessionRuntimeOptions {
                    cwd: cwd_str,
                    agent_dir: agent_str,
                    session_manager: sm,
                    session_start_event: None,
                },
            )
            .await
            .expect("create runtime"),
        );

        let mode = InteractiveMode::new(
            runtime.clone(),
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
        mode.init().await;
        runtime.new_session(None).await.expect("new_session");

        let mut rendered = String::new();
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            rendered = strip_terminal_sequences(&mode.chat_container().render(100).join("\n"));
            if rendered.contains("START_CUSTOM_MSG") {
                break;
            }
        }
        assert!(rendered.contains("START_CUSTOM_MSG"), "got:\n{rendered}");

        mode.shutdown(false).await;
        runtime.dispose().await;
    }

    #[tokio::test]
    #[serial]
    async fn subscribes_before_replacement_session_start_handlers_send_user_messages() {
        init_theme(Some("dark"), Some(false));
        let _guard = ThemeResetGuard;

        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path().join("project");
        let agent_dir = dir.path().join("agent");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&agent_dir).expect("agent");

        let faux = register_faux_provider(Some(RegisterFauxProviderOptions::default()));
        faux.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("START_USER_REPLY".to_string()),
            None,
        ))]);
        let model = faux.get_model();
        let auth_storage = Arc::new(AuthStorage::in_memory(None));
        let model_registry = create_in_memory_model_registry(auth_storage)
            .await
            .expect("create_in_memory_model_registry");
        let reg_models = faux
            .models()
            .iter()
            .map(|m| {
                ProviderModelConfig::Chat(ProviderChatModelConfig {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    api: Some(m.api.clone()),
                    base_url: Some(m.base_url.clone()),
                    input: m.input.clone(),
                    input_limits: m.input_limits.clone(),
                    cost: m.cost.clone(),
                    headers: None,
                    reasoning: m.reasoning,
                    thinking_level_map: m.thinking_level_map.clone(),
                    prompt_cache: m.prompt_cache.clone(),
                    context_window: m.context_window,
                    max_tokens: m.max_tokens,
                    sampling_params: m.sampling_params.clone(),
                    compat: m.compat.clone(),
                    sampling_params_by_thinking_level: None,
                })
            })
            .collect();
        model_registry
            .register_provider(
                &model.provider,
                ProviderConfig {
                    base_url: Some(model.base_url.clone()),
                    api_key: Some("faux-key".to_string()),
                    api: Some(faux.api().to_string()),
                    models: Some(reg_models),
                    ..Default::default()
                }
                .into(),
            )
            .expect("register faux provider");

        let factory = build_runtime_factory(
            Arc::new(move |pi: ExtensionApi| {
                let pi_for_handler = pi.clone();
                Box::pin(async move {
                    pi.on::<SessionStartEvent>(move |ev, _ctx| {
                        let pi_send = pi_for_handler.clone();
                        Box::pin(async move {
                            if ev.reason == SessionStartReason::New {
                                pi_send.send_user_message(
                                    CustomMessageContent::Text("START_USER_MSG".to_string()),
                                    None,
                                )?;
                            }
                            Ok(None)
                        })
                    });
                    Ok(())
                })
            }),
            Some(model),
            Some(get_model_runtime(&model_registry)),
        );

        let cwd_str = cwd.to_string_lossy().into_owned();
        let agent_str = agent_dir.to_string_lossy().into_owned();
        let sm = SessionManager::in_memory(Some(&cwd_str), None, None).expect("sm");
        let runtime = Arc::new(
            create_agent_session_runtime(
                factory,
                CreateAgentSessionRuntimeOptions {
                    cwd: cwd_str,
                    agent_dir: agent_str,
                    session_manager: sm,
                    session_start_event: None,
                },
            )
            .await
            .expect("create runtime"),
        );

        let mode = InteractiveMode::new(
            runtime.clone(),
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
        mode.init().await;
        runtime.new_session(None).await.expect("new_session");
        runtime.session().wait_for_idle().await;

        let mut rendered = String::new();
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            rendered = strip_terminal_sequences(&mode.chat_container().render(100).join("\n"));
            if rendered.contains("START_USER_MSG") && rendered.contains("START_USER_REPLY") {
                break;
            }
        }
        assert!(rendered.contains("START_USER_MSG"), "got:\n{rendered}");
        assert!(rendered.contains("START_USER_REPLY"), "got:\n{rendered}");

        mode.shutdown(false).await;
        runtime.dispose().await;
        faux.unregister();
    }

    #[tokio::test]
    #[serial]
    async fn refreshes_hide_thinking_block_before_rebuilding_chat_during_reload() {
        init_theme(Some("dark"), Some(false));
        let _guard = ThemeResetGuard;

        let dir = tempfile::tempdir().expect("tempdir");
        let project_dir = dir.path().join("project");
        let agent_dir = dir.path().join("agent");
        fs::create_dir_all(&project_dir).expect("project");
        fs::create_dir_all(&agent_dir).expect("agent");
        let settings_path = agent_dir.join("settings.json");
        fs::write(&settings_path, r#"{"hideThinkingBlock":false}"#).expect("write settings");

        let cwd = project_dir.to_string_lossy().into_owned();
        let agent_str = agent_dir.to_string_lossy().into_owned();
        let settings_manager = SettingsManager::create(&cwd, Some(&agent_str), None);

        let observed_during_session_start: Arc<Mutex<Option<(bool, String)>>> = Arc::new(Mutex::new(None));
        let mode_slot: Arc<Mutex<Option<InteractiveMode>>> = Arc::new(Mutex::new(None));
        let obs_clone = Arc::clone(&observed_during_session_start);
        let mode_clone = Arc::clone(&mode_slot);

        let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
            let obs = Arc::clone(&obs_clone);
            let mode_ref = Arc::clone(&mode_clone);
            Box::pin(async move {
                pi.on::<SessionStartEvent>(move |ev, _ctx| {
                    let obs = Arc::clone(&obs);
                    let mode_ref = Arc::clone(&mode_ref);
                    Box::pin(async move {
                        if ev.reason == SessionStartReason::Reload
                            && let Some(mode) = mode_ref.lock().unwrap_or_else(|e| e.into_inner()).clone()
                        {
                            let chat = strip_terminal_sequences(&mode.chat_container().render(100).join("\n"));
                            *obs.lock().unwrap_or_else(|e| e.into_inner()) = Some((mode.hide_thinking_block(), chat));
                        }
                        Ok(None)
                    })
                });
                Ok(())
            })
        }));

        let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: cwd.clone(),
            agent_dir: agent_str.clone(),
            settings_manager: Some(settings_manager.clone()),
            extension_factories: vec![ext.into()],
            no_extensions: true,
            no_skills: true,
            no_prompt_templates: true,
            no_themes: true,
            no_context_files: true,
            ..Default::default()
        }));
        resource_loader.reload(None).await.expect("reload");

        let created = create_agent_session(Some(CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(agent_str.clone()),
            settings_manager: Some(settings_manager),
            session_manager: Some(SessionManager::in_memory(Some(&cwd), None, None).expect("sm")),
            resource_loader: Some(resource_loader),
            ..Default::default()
        }))
        .await
        .expect("create session");

        let sm = created.session.session_manager();
        sm.append_message(AgentMessage::Message(pi_ai::Message::User(pi_ai::UserMessage {
            content: pi_ai::UserMessageContent::Text("question".to_string()),
            timestamp: 1000,
        })))
        .expect("user msg");
        sm.append_message(AgentMessage::Message(pi_ai::Message::Assistant(
            pi_ai::faux_assistant_message(
                pi_ai::FauxAssistantMessageContent::Blocks(vec![
                    pi_ai::FauxContentBlock::Thinking(pi_ai::faux_thinking("SECRET_THOUGHT_789")),
                    pi_ai::FauxContentBlock::Text(pi_ai::faux_text("VISIBLE_ANSWER_123")),
                ]),
                None,
            ),
        )))
        .expect("assistant msg");
        let ctx_msgs = sm.build_session_context().messages;
        created.session.agent().replace_messages(ctx_msgs);

        let host = Arc::new(SingleSessionHost {
            session: Arc::new(created.session.clone()),
            agent_dir: agent_str,
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
        *mode_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(mode.clone());
        mode.rebind_current_session(false).await;
        mode.render_initial_messages();

        assert!(!mode.hide_thinking_block());
        let initial_chat = strip_terminal_sequences(&mode.chat_container().render(100).join("\n"));
        assert!(initial_chat.contains("SECRET_THOUGHT_789"));

        fs::write(&settings_path, r#"{"hideThinkingBlock":true}"#).expect("update settings");
        mode.handle_reload_command().await;

        let (hide_before_start, chat_before_start) = observed_during_session_start
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .expect("SessionStart(Reload) ran");
        assert!(hide_before_start);
        assert!(chat_before_start.contains("VISIBLE_ANSWER_123"));
        assert!(!chat_before_start.contains("SECRET_THOUGHT_789"));

        mode.shutdown(false).await;
        created.session.dispose();
    }

    #[tokio::test]
    #[serial]
    async fn keeps_the_reload_blocker_focused_until_async_reload_completes() {
        init_theme(Some("dark"), Some(false));
        let _guard = ThemeResetGuard;

        let harness = create_harness(HarnessOptions::default()).await;
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let slow_sess = Arc::new(SlowReloadSession {
            inner: Arc::new(harness.session.clone()),
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        });
        let host = Arc::new(SingleSessionHost {
            session: slow_sess,
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
        mode.ui().set_focus(Some(Arc::new(mode.default_editor().clone())));
        assert!(mode.default_editor().focused());

        let mode_task = mode.clone();
        let reload_task = tokio::spawn(async move {
            mode_task.handle_reload_command().await;
        });

        entered.notified().await;
        assert!(!mode.default_editor().focused());
        let blocker_rendered = strip_terminal_sequences(&mode.editor_container().render(100).join("\n"));
        assert!(blocker_rendered.contains("Reloading keybindings, extensions"));

        release.notify_waiters();
        reload_task.await.expect("reload join");

        assert!(mode.default_editor().focused());

        mode.shutdown(false).await;
        harness.cleanup();
    }
}

#[tokio::test]
async fn runs_the_reload_render_hook_before_reload_session_start_handlers_can_notify() {
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let ev_ext = Arc::clone(&events);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let ev = Arc::clone(&ev_ext);
        async move {
            pi.on::<SessionStartEvent>(move |event, ctx| {
                let ev = Arc::clone(&ev);
                Box::pin(async move {
                    let reason = match event.reason {
                        pi_coding_agent::core::extensions::types::SessionStartReason::Startup => "startup",
                        pi_coding_agent::core::extensions::types::SessionStartReason::Reload => "reload",
                        pi_coding_agent::core::extensions::types::SessionStartReason::New => "new",
                        pi_coding_agent::core::extensions::types::SessionStartReason::Resume => "resume",
                        pi_coding_agent::core::extensions::types::SessionStartReason::Fork => "fork",
                    };
                    ev.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(format!("start:{reason}"));
                    ctx.ui()?.notify(&format!("notify:{reason}"), Some(NotifyType::Error));
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let ui = Arc::new(NotifyRecorderUi {
        events: Arc::clone(&events),
    });
    harness
        .session
        .bind_extensions(ExtensionBindings {
            ui_context: Some(ui),
            mode: Some(ExtensionMode::Tui),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["start:startup", "notify:startup"]
    );

    events.lock().unwrap_or_else(PoisonError::into_inner).clear();
    let ev_hook = Arc::clone(&events);
    let hook_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hc_c = Arc::clone(&hook_calls);
    harness
        .session
        .reload(Some(ReloadOptions {
            before_session_start: Some(Box::new(move || {
                let ev = Arc::clone(&ev_hook);
                let hc = Arc::clone(&hc_c);
                Box::pin(async move {
                    hc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    ev.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push("render".to_string());
                    Ok(())
                })
            })),
        }))
        .await
        .unwrap();

    assert_eq!(hook_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["render", "start:reload", "notify:reload"]
    );
    harness.cleanup();
}
