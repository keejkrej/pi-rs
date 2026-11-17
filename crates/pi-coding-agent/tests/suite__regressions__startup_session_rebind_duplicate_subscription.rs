//! Port of packages/coding-agent/test/suite/regressions/startup-session-rebind-duplicate-subscription.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent_core::AgentMessage;
use pi_ai::{FauxAssistantMessageContent, FauxResponseStep, Model, ModelThinkingLevel, faux_assistant_message};
use pi_coding_agent::core::agent_session::AgentSession;
use pi_coding_agent::modes::interactive::bug_report::BugReportSession;
use pi_coding_agent::modes::interactive::components::footer::{
    FooterContextUsage, FooterRoutedModel, FooterSession, FooterUsageRecord,
};
use pi_coding_agent::modes::interactive::components::model_selector::ScopedModelItem;
use pi_coding_agent::modes::interactive::components::settings_selector::{CacheWarmingMode, SteeringMode, TuiMode};
use pi_coding_agent::modes::interactive::components::tool_execution::ToolRenderers;
use pi_coding_agent::modes::interactive::components::user_message_selector::UserMessageItem;
use pi_coding_agent::modes::interactive::interactive_mode::{
    AgentSessionRuntimeDiagnostic, BashExecutionResult, CacheMissNotice, CacheWarmingStatusSummary, CancelledResult,
    ClearQueueResult, CompactionSummaryResult, CycleDirection, CycleModelResult, InteractiveExtensionBindings,
    InteractiveExtensionRunner, InteractiveMode, InteractiveModeHooks, InteractiveModeOptions, InteractiveModelRuntime,
    InteractivePromptOptions, InteractiveResourceLoader, InteractiveRuntimeHost, InteractiveSession,
    InteractiveSessionManager, InteractiveSettingsManager, NavigateTreeOptions, NavigateTreeResult, PromptTemplateInfo,
    RuntimeForkOptions, RuntimeForkResult, SessionStats, SwitchSessionOptions, UsageBreakdownEntry,
};
use pi_coding_agent::modes::interactive::session_share::ShareSession;
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_coding_agent::modes::json_event::AgentSessionEvent;
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result, Unsubscribe};
use pi_tui::strip_terminal_sequences;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use serial_test::serial;
use support::suite::harness::{HarnessOptions, create_harness};
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

struct TrackedSession {
    inner: Arc<AgentSession>,
    subscribe_calls: AtomicUsize,
    active_listeners: Arc<AtomicUsize>,
    pause_bind: Option<(Arc<Notify>, Arc<Notify>)>,
}

impl TrackedSession {
    fn new(inner: Arc<AgentSession>, pause_bind: Option<(Arc<Notify>, Arc<Notify>)>) -> Self {
        Self {
            inner,
            subscribe_calls: AtomicUsize::new(0),
            active_listeners: Arc::new(AtomicUsize::new(0)),
            pause_bind,
        }
    }
}

impl FooterSession for TrackedSession {
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
impl InteractiveSession for TrackedSession {
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
    fn set_transport(&self, transport: pi_ai::Transport) {
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
        self.subscribe_calls.fetch_add(1, Ordering::SeqCst);
        self.active_listeners.fetch_add(1, Ordering::SeqCst);
        let unsub = InteractiveSession::subscribe(self.inner.as_ref(), listener);
        let active = Arc::clone(&self.active_listeners);
        let released = Arc::new(AtomicBool::new(false));
        Unsubscribe::new(move || {
            if !released.swap(true, Ordering::SeqCst) {
                active.fetch_sub(1, Ordering::SeqCst);
                unsub.call();
            }
        })
    }
    async fn bind_extensions(&self, bindings: InteractiveExtensionBindings) -> Result<()> {
        if let Some((ref entered, ref release)) = self.pause_bind {
            entered.notify_waiters();
            release.notified().await;
        }
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

struct SwappableRuntimeHost {
    current: Mutex<Arc<TrackedSession>>,
    agent_dir: String,
    rebind_cb: Mutex<Option<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>>,
}

#[async_trait]
impl InteractiveRuntimeHost for SwappableRuntimeHost {
    fn session(&self) -> Arc<dyn InteractiveSession> {
        Arc::clone(&*self.current.lock().unwrap_or_else(|e| e.into_inner())) as Arc<dyn InteractiveSession>
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
    fn set_rebind_session(&self, cb: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>) {
        *self.rebind_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(cb);
    }
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
async fn does_not_duplicate_session_subscriptions_across_startup_session_rebind_and_new_session() {
    init_theme(Some("dark"), Some(false));
    let _guard = ThemeResetGuard;

    let h1 = create_harness(HarnessOptions::default()).await;
    let h2 = create_harness(HarnessOptions::default()).await;
    let h3 = create_harness(HarnessOptions::default()).await;

    let s1_bind_entered = Arc::new(Notify::new());
    let s1_bind_release = Arc::new(Notify::new());
    let s1 = Arc::new(TrackedSession::new(
        Arc::new(h1.session.clone()),
        Some((Arc::clone(&s1_bind_entered), Arc::clone(&s1_bind_release))),
    ));
    let s2 = Arc::new(TrackedSession::new(Arc::new(h2.session.clone()), None));
    let s3 = Arc::new(TrackedSession::new(Arc::new(h3.session.clone()), None));

    let host = Arc::new(SwappableRuntimeHost {
        current: Mutex::new(Arc::clone(&s1)),
        agent_dir: h1.temp_dir.clone(),
        rebind_cb: Mutex::new(None),
    });

    let mode = InteractiveMode::new(
        host.clone(),
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

    // 1. Start initial startup init() -> rebind_current_session(false), paused inside s1.bind_extensions.
    let mode_startup = mode.clone();
    let startup_bind = tokio::spawn(async move {
        mode_startup.init().await;
    });
    s1_bind_entered.notified().await;
    assert_eq!(s1.subscribe_calls.load(Ordering::SeqCst), 0);

    // 2. Replace current session with s2 and run replacement rebind BEFORE s1.bind_extensions finishes.
    *host.current.lock().unwrap_or_else(|e| e.into_inner()) = Arc::clone(&s2);
    mode.rebind_current_session(true).await;

    assert_eq!(s2.subscribe_calls.load(Ordering::SeqCst), 1);
    assert_eq!(s2.active_listeners.load(Ordering::SeqCst), 1);

    // 3. Release stale s1 bind_extensions and await startup task; it must NOT subscribe to s2 a second time.
    s1_bind_release.notify_waiters();
    startup_bind.await.expect("startup bind join");

    assert_eq!(s1.subscribe_calls.load(Ordering::SeqCst), 0);
    assert_eq!(s1.active_listeners.load(Ordering::SeqCst), 0);
    assert_eq!(s2.subscribe_calls.load(Ordering::SeqCst), 1);
    assert_eq!(s2.active_listeners.load(Ordering::SeqCst), 1);

    // Run a real turn on s2 and confirm the assistant reply is rendered once cleanly (not duplicated).
    h2.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("REPLY_S2_ONCE".to_string()),
        None,
    ))]);
    h2.session.prompt("prompt on s2", None).await.expect("prompt on s2");
    let mut rendered_s2 = String::new();
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        rendered_s2 = strip_terminal_sequences(&mode.chat_container().render(80).join("\n"));
        if rendered_s2.contains("REPLY_S2_ONCE") {
            break;
        }
    }
    assert_eq!(rendered_s2.matches("REPLY_S2_ONCE").count(), 1);

    // 4. Simulate /new -> switch to s3 and verify s2's subscription is cleaned up cleanly.
    *host.current.lock().unwrap_or_else(|e| e.into_inner()) = Arc::clone(&s3);
    mode.rebind_current_session(true).await;

    assert_eq!(s2.active_listeners.load(Ordering::SeqCst), 0);
    assert_eq!(s3.subscribe_calls.load(Ordering::SeqCst), 1);
    assert_eq!(s3.active_listeners.load(Ordering::SeqCst), 1);

    mode.shutdown(false).await;
    h1.cleanup();
    h2.cleanup();
    h3.cleanup();
}
