//! Port of packages/coding-agent/test/rpc-prompt-response-semantics.test.ts

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_agent_core::{AgentMessage, QueueMode};
use pi_ai::{ImageContent, Model};
use pi_coding_agent::modes::json_event::AgentSessionEvent;
use pi_coding_agent::modes::rpc::rpc_mode::{
    RpcEndCallback, RpcExecuteBashOptions, RpcExtensionBindings, RpcForkOptions, RpcForkResult, RpcIo, RpcLineCallback,
    RpcNavigateTreeOptions, RpcNewSessionOptions, RpcPromptOptions, RpcSession, RpcSessionRuntime, RpcSignal,
    RpcSignalCallback, RpcSwitchSessionOptions, RpcUserBashOutcome, run_rpc_mode_with_io,
};
use pi_coding_agent::modes::rpc::rpc_types::{
    BashResult, CancelledResult, ClearQueueResult, CompactionResult, CycleModelResponseData, ForkMessage,
    PromptDisposition, QueuedInputDisposition, RpcSlashCommand, RpcStreamingBehavior, SessionEntry, SessionStats,
    SessionTokenStats, SessionTreeNode, ThinkingLevel,
};
use pi_js::{BoxFuture, Error, Unsubscribe};
use pretty_assertions::assert_eq as pretty_assert_eq;
use serde_json::{Value, json};
use tokio::sync::Notify;

// ============================================================================
// In-Memory RpcIo & Test Session / Runtime Harness
// ============================================================================

#[derive(Default)]
struct InMemoryRpcIo {
    output_lines: Mutex<Vec<String>>,
    line_handler: Mutex<Option<RpcLineCallback>>,
    end_handler: Mutex<Option<RpcEndCallback>>,
    signal_handler: Mutex<Option<RpcSignalCallback>>,
    exit_codes: Mutex<Vec<i32>>,
    on_exit_hook: Mutex<Option<Arc<dyn Fn(i32) + Send + Sync>>>,
}

impl InMemoryRpcIo {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn set_on_exit_hook(&self, hook: impl Fn(i32) + Send + Sync + 'static) {
        *self.on_exit_hook.lock().unwrap() = Some(Arc::new(hook));
    }

    fn send_line(&self, line: &str) {
        let cb = self
            .line_handler
            .lock()
            .unwrap()
            .clone()
            .expect("line_handler attached");
        cb(line);
    }

    fn emit_end(&self) {
        let cb = self.end_handler.lock().unwrap().clone().expect("end_handler attached");
        cb();
    }

    fn emit_signal(&self, sig: RpcSignal) {
        let cb = self
            .signal_handler
            .lock()
            .unwrap()
            .clone()
            .expect("signal_handler attached");
        cb(sig);
    }

    fn has_line_handler(&self) -> bool {
        self.line_handler.lock().unwrap().is_some()
    }

    fn has_signal_handler(&self) -> bool {
        self.signal_handler.lock().unwrap().is_some()
    }

    fn exit_codes(&self) -> Vec<i32> {
        self.exit_codes.lock().unwrap().clone()
    }

    fn clear_output(&self) {
        self.output_lines.lock().unwrap().clear();
    }

    fn push_raw_output(&self, line: &str) {
        self.output_lines.lock().unwrap().push(line.to_string());
    }

    fn parsed_output_lines(&self) -> Vec<Value> {
        let lines = self.output_lines.lock().unwrap().clone();
        lines
            .iter()
            .flat_map(|chunk| chunk.split('\n'))
            .filter(|line| !line.trim().is_empty())
            .map(|line| pi_js::json::parse(line).expect("valid output json line"))
            .collect()
    }

    fn prompt_responses(&self, id: &str) -> Vec<Value> {
        self.parsed_output_lines()
            .into_iter()
            .filter(|record| {
                record.get("id").and_then(Value::as_str) == Some(id)
                    && record.get("type").and_then(Value::as_str) == Some("response")
                    && record.get("command").and_then(Value::as_str) == Some("prompt")
            })
            .collect()
    }
}

#[async_trait]
impl RpcIo for InMemoryRpcIo {
    fn take_over_stdout(&self) {}

    fn write_line(&self, line: &str) {
        self.output_lines.lock().unwrap().push(line.to_string());
    }

    fn wait_for_stdout_backpressure(&self) -> BoxFuture<()> {
        Box::pin(async {})
    }

    fn flush_stdout(&self) -> BoxFuture<pi_js::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn kill_tracked_detached_children(&self) {}

    fn attach_input(&self, on_line: RpcLineCallback, on_end: RpcEndCallback) -> Unsubscribe {
        *self.line_handler.lock().unwrap() = Some(on_line);
        *self.end_handler.lock().unwrap() = Some(on_end);
        Unsubscribe::noop()
    }

    fn subscribe_signals(&self, on_signal: RpcSignalCallback) -> Unsubscribe {
        *self.signal_handler.lock().unwrap() = Some(on_signal);
        Unsubscribe::noop()
    }

    fn on_exit(&self, exit_code: i32) {
        self.exit_codes.lock().unwrap().push(exit_code);
        let hook = self.on_exit_hook.lock().unwrap().clone();
        if let Some(h) = hook {
            h(exit_code);
        }
    }
}

type InputInterceptor = Arc<
    dyn Fn(&str, &str, Option<RpcStreamingBehavior>, &TestRpcSession) -> BoxFuture<Option<InputInterceptResult>>
        + Send
        + Sync,
>;
type ExtensionCommandHandler = Arc<dyn Fn(&TestRpcSession) -> BoxFuture<()> + Send + Sync>;
type SessionStartHook = Arc<dyn Fn(&TestRpcSession, Arc<dyn Fn() + Send + Sync>) + Send + Sync>;

#[derive(Clone)]
enum InputInterceptResult {
    Handled,
    Transform(String),
}

struct TestSessionState {
    listeners: Vec<(u64, Arc<dyn Fn(AgentSessionEvent) + Send + Sync>)>,
    next_listener_id: u64,
    is_streaming: bool,
    is_idle: bool,
    steering: Vec<String>,
    follow_up: Vec<String>,
    shutdown_handler: Option<Arc<dyn Fn() + Send + Sync>>,
    leaf_id: Option<String>,
    entries: Vec<SessionEntry>,
    commands: Vec<(String, ExtensionCommandHandler)>,
    input_interceptor: Option<InputInterceptor>,
    on_session_start: Option<SessionStartHook>,
}

struct TestRpcSession {
    with_auth: bool,
    provider_name: String,
    response_delay_ms: u64,
    state: Mutex<TestSessionState>,
}

impl TestRpcSession {
    fn new(with_auth: bool, provider_name: &str, response_delay_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            with_auth,
            provider_name: provider_name.to_string(),
            response_delay_ms,
            state: Mutex::new(TestSessionState {
                listeners: Vec::new(),
                next_listener_id: 0,
                is_streaming: false,
                is_idle: true,
                steering: Vec::new(),
                follow_up: Vec::new(),
                shutdown_handler: None,
                leaf_id: Some("entry-1".to_string()),
                entries: Vec::new(),
                commands: Vec::new(),
                input_interceptor: None,
                on_session_start: None,
            }),
        })
    }

    fn emit(&self, event: AgentSessionEvent) {
        let listeners: Vec<_> = {
            let guard = self.state.lock().unwrap();
            guard.listeners.iter().map(|(_, l)| Arc::clone(l)).collect()
        };
        for l in listeners {
            l(event.clone());
        }
    }

    fn emit_queue_update(&self) {
        let (steering, follow_up) = {
            let guard = self.state.lock().unwrap();
            (guard.steering.clone(), guard.follow_up.clone())
        };
        self.emit(AgentSessionEvent::QueueUpdate { steering, follow_up });
    }

    fn queue_user_message(&self, text: &str, behavior: RpcStreamingBehavior) {
        {
            let mut guard = self.state.lock().unwrap();
            match behavior {
                RpcStreamingBehavior::Steer => guard.steering.push(text.to_string()),
                RpcStreamingBehavior::FollowUp => guard.follow_up.push(text.to_string()),
            }
        }
        self.emit_queue_update();
    }

    fn register_command(&self, name: &str, handler: ExtensionCommandHandler) {
        self.state.lock().unwrap().commands.push((name.to_string(), handler));
    }

    fn set_input_interceptor(&self, interceptor: InputInterceptor) {
        self.state.lock().unwrap().input_interceptor = Some(interceptor);
    }

    fn set_on_session_start(&self, hook: SessionStartHook) {
        self.state.lock().unwrap().on_session_start = Some(hook);
    }

    fn register_entries(&self, entries: Vec<SessionEntry>, leaf_id: Option<String>) {
        let mut guard = self.state.lock().unwrap();
        guard.entries = entries;
        guard.leaf_id = leaf_id;
    }
}

#[async_trait]
impl RpcSession for TestRpcSession {
    async fn bind_extensions(&self, bindings: RpcExtensionBindings) -> pi_js::Result<()> {
        let (hook, shutdown) = {
            let mut guard = self.state.lock().unwrap();
            guard.shutdown_handler = Some(Arc::clone(&bindings.shutdown_handler));
            (guard.on_session_start.clone(), bindings.shutdown_handler)
        };
        if let Some(h) = hook {
            h(self, shutdown);
        }
        Ok(())
    }

    fn subscribe(&self, listener: Arc<dyn Fn(AgentSessionEvent) + Send + Sync>) -> Unsubscribe {
        let id = {
            let mut guard = self.state.lock().unwrap();
            guard.next_listener_id += 1;
            let id = guard.next_listener_id;
            guard.listeners.push((id, listener));
            id
        };
        Unsubscribe::new(move || {
            let _ = id;
        })
    }

    fn subscribe_agent_backpressure(&self, _listener: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>) -> Unsubscribe {
        Unsubscribe::noop()
    }

    async fn prompt(&self, message: &str, options: RpcPromptOptions) -> pi_js::Result<()> {
        if let Some(stripped) = message.strip_prefix('/') {
            let cmd_name = stripped.split_whitespace().next().unwrap_or("");
            let cmd_handler = {
                let guard = self.state.lock().unwrap();
                guard
                    .commands
                    .iter()
                    .find(|(n, _)| n == cmd_name)
                    .map(|(_, h)| Arc::clone(h))
            };
            if let Some(handler) = cmd_handler {
                handler(self).await;
                (options.preflight_result)(PromptDisposition::Handled);
                return Ok(());
            }
        }

        let interceptor = self.state.lock().unwrap().input_interceptor.clone();
        let current_text = if let Some(intercept) = interceptor {
            match intercept(message, options.source, options.streaming_behavior, self).await {
                Some(InputInterceptResult::Handled) => {
                    (options.preflight_result)(PromptDisposition::Handled);
                    return Ok(());
                }
                Some(InputInterceptResult::Transform(t)) => t,
                None => message.to_string(),
            }
        } else {
            message.to_string()
        };

        let is_streaming = self.state.lock().unwrap().is_streaming;
        if is_streaming {
            let Some(behavior) = options.streaming_behavior else {
                return Err(Error::msg(
                    "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.",
                ));
            };
            self.queue_user_message(&current_text, behavior);
            (options.preflight_result)(PromptDisposition::Queued);
            return Ok(());
        }

        if !self.with_auth {
            return Err(Error::msg(format!(
                "No API key found for {}.\n\nUse /login to log into a provider via OAuth or API key. See: https://docs.example.invalid/auth",
                self.provider_name
            )));
        }

        {
            let mut guard = self.state.lock().unwrap();
            guard.is_streaming = true;
            guard.is_idle = false;
        }
        (options.preflight_result)(PromptDisposition::Started);
        self.emit(AgentSessionEvent::AgentStart);

        if self.response_delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.response_delay_ms)).await;
        }

        self.emit(AgentSessionEvent::AgentEnd {
            messages: Vec::new(),
            will_retry: false,
        });
        {
            let mut guard = self.state.lock().unwrap();
            guard.is_streaming = false;
            guard.is_idle = true;
        }
        self.emit(AgentSessionEvent::AgentSettled { aborted: false });
        Ok(())
    }

    async fn steer(&self, message: &str, _images: Option<Vec<ImageContent>>) -> pi_js::Result<QueuedInputDisposition> {
        let interceptor = self.state.lock().unwrap().input_interceptor.clone();
        let text = if let Some(intercept) = interceptor {
            match intercept(message, "rpc", Some(RpcStreamingBehavior::Steer), self).await {
                Some(InputInterceptResult::Handled) => return Ok(QueuedInputDisposition::Handled),
                Some(InputInterceptResult::Transform(t)) => t,
                None => message.to_string(),
            }
        } else {
            message.to_string()
        };
        self.queue_user_message(&text, RpcStreamingBehavior::Steer);
        Ok(QueuedInputDisposition::Queued)
    }

    async fn follow_up(
        &self,
        message: &str,
        _images: Option<Vec<ImageContent>>,
    ) -> pi_js::Result<QueuedInputDisposition> {
        let interceptor = self.state.lock().unwrap().input_interceptor.clone();
        let text = if let Some(intercept) = interceptor {
            match intercept(message, "rpc", Some(RpcStreamingBehavior::FollowUp), self).await {
                Some(InputInterceptResult::Handled) => return Ok(QueuedInputDisposition::Handled),
                Some(InputInterceptResult::Transform(t)) => t,
                None => message.to_string(),
            }
        } else {
            message.to_string()
        };
        self.queue_user_message(&text, RpcStreamingBehavior::FollowUp);
        Ok(QueuedInputDisposition::Queued)
    }

    async fn abort(&self) {
        let mut guard = self.state.lock().unwrap();
        guard.is_streaming = false;
        guard.is_idle = true;
    }

    fn clear_queue(&self) -> ClearQueueResult {
        let mut guard = self.state.lock().unwrap();
        let steering = std::mem::take(&mut guard.steering);
        let follow_up = std::mem::take(&mut guard.follow_up);
        ClearQueueResult { steering, follow_up }
    }

    fn is_idle(&self) -> bool {
        self.state.lock().unwrap().is_idle
    }

    async fn wait_for_idle(&self) {}

    fn model(&self) -> Option<Model> {
        None
    }

    fn thinking_level(&self) -> ThinkingLevel {
        ThinkingLevel::Medium
    }

    fn is_streaming(&self) -> bool {
        self.state.lock().unwrap().is_streaming
    }

    fn is_compacting(&self) -> bool {
        false
    }

    fn steering_mode(&self) -> QueueMode {
        QueueMode::OneAtATime
    }

    fn follow_up_mode(&self) -> QueueMode {
        QueueMode::OneAtATime
    }

    fn session_file(&self) -> Option<String> {
        None
    }

    fn session_id(&self) -> String {
        "test-session-id".to_string()
    }

    fn session_name(&self) -> Option<String> {
        None
    }

    fn auto_compaction_enabled(&self) -> bool {
        true
    }

    fn messages(&self) -> Vec<AgentMessage> {
        Vec::new()
    }

    fn pending_message_count(&self) -> usize {
        let guard = self.state.lock().unwrap();
        guard.steering.len() + guard.follow_up.len()
    }

    fn get_available_models(&self) -> Vec<Model> {
        Vec::new()
    }

    async fn set_model(&self, _model: Model) -> pi_js::Result<()> {
        Ok(())
    }

    async fn cycle_model(&self) -> pi_js::Result<Option<CycleModelResponseData>> {
        Ok(None)
    }

    fn set_thinking_level(&self, _level: ThinkingLevel) {}

    fn cycle_thinking_level(&self) -> Option<ThinkingLevel> {
        None
    }

    fn get_available_thinking_levels(&self) -> Vec<ThinkingLevel> {
        vec![ThinkingLevel::Off, ThinkingLevel::Medium]
    }

    fn set_steering_mode(&self, _mode: QueueMode) {}

    fn set_follow_up_mode(&self, _mode: QueueMode) {}

    async fn compact(&self, _custom_instructions: Option<&str>) -> pi_js::Result<CompactionResult> {
        Err(Error::msg("not implemented in test"))
    }

    fn set_auto_compaction_enabled(&self, _enabled: bool) {}

    fn set_auto_retry_enabled(&self, _enabled: bool) {}

    fn abort_retry(&self) {}

    fn cwd(&self) -> String {
        ".".to_string()
    }

    async fn emit_user_bash(
        &self,
        _command: &str,
        _exclude_from_context: bool,
        _cwd: &str,
    ) -> pi_js::Result<Option<RpcUserBashOutcome>> {
        Ok(None)
    }

    fn record_bash_result(&self, _command: &str, _result: &BashResult, _exclude_from_context: Option<bool>) {}

    async fn execute_bash(&self, _command: &str, _options: RpcExecuteBashOptions) -> pi_js::Result<BashResult> {
        Err(Error::msg("not implemented in test"))
    }

    fn abort_bash(&self) {}

    fn get_session_stats(&self) -> SessionStats {
        SessionStats {
            session_file: None,
            session_id: "test-session-id".to_string(),
            user_messages: 0,
            assistant_messages: 0,
            tool_calls: 0,
            tool_results: 0,
            total_messages: 0,
            tokens: SessionTokenStats {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                total: 0,
            },
            cost: 0.0,
            context_usage: None,
        }
    }

    async fn export_to_html(&self, _output_path: Option<&str>) -> pi_js::Result<String> {
        Err(Error::msg("not implemented in test"))
    }

    fn leaf_id(&self) -> Option<String> {
        self.state.lock().unwrap().leaf_id.clone()
    }

    fn get_user_messages_for_forking(&self) -> Vec<ForkMessage> {
        Vec::new()
    }

    fn get_entries(&self) -> Vec<SessionEntry> {
        self.state.lock().unwrap().entries.clone()
    }

    fn get_tree(&self) -> Vec<SessionTreeNode> {
        Vec::new()
    }

    fn get_last_assistant_text(&self) -> Option<String> {
        None
    }

    fn set_session_name(&self, _name: &str) {}

    fn get_commands(&self) -> Vec<RpcSlashCommand> {
        Vec::new()
    }

    async fn navigate_tree(
        &self,
        _target_id: &str,
        _options: Option<RpcNavigateTreeOptions>,
    ) -> pi_js::Result<CancelledResult> {
        Ok(CancelledResult { cancelled: true })
    }

    async fn reload(&self) -> pi_js::Result<()> {
        Ok(())
    }
}

struct TestRpcRuntime {
    session: Arc<TestRpcSession>,
    dispose_calls: AtomicUsize,
    slow_dispose: AtomicBool,
    dispose_release: Notify,
    fork_calls: Mutex<Vec<(String, Option<RpcForkOptions>)>>,
}

impl TestRpcRuntime {
    fn new(session: Arc<TestRpcSession>) -> Arc<Self> {
        Arc::new(Self {
            session,
            dispose_calls: AtomicUsize::new(0),
            slow_dispose: AtomicBool::new(false),
            dispose_release: Notify::new(),
            fork_calls: Mutex::new(Vec::new()),
        })
    }

    fn enable_slow_dispose(&self) {
        self.slow_dispose.store(true, Ordering::SeqCst);
    }

    fn finish_dispose(&self) {
        self.slow_dispose.store(false, Ordering::SeqCst);
        self.dispose_release.notify_waiters();
    }

    fn dispose_count(&self) -> usize {
        self.dispose_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl RpcSessionRuntime for TestRpcRuntime {
    fn session(&self) -> Arc<dyn RpcSession> {
        Arc::clone(&self.session) as Arc<dyn RpcSession>
    }

    fn set_rebind_session(&self, _rebind: Option<Arc<dyn Fn() -> BoxFuture<pi_js::Result<()>> + Send + Sync>>) {}

    async fn new_session(&self, _options: Option<RpcNewSessionOptions>) -> pi_js::Result<CancelledResult> {
        Ok(CancelledResult { cancelled: true })
    }

    async fn switch_session(
        &self,
        _session_path: &str,
        _options: Option<RpcSwitchSessionOptions>,
    ) -> pi_js::Result<CancelledResult> {
        Ok(CancelledResult { cancelled: true })
    }

    async fn fork(&self, entry_id: &str, options: Option<RpcForkOptions>) -> pi_js::Result<RpcForkResult> {
        self.fork_calls.lock().unwrap().push((entry_id.to_string(), options));
        Ok(RpcForkResult {
            cancelled: false,
            selected_text: Some(String::new()),
        })
    }

    async fn dispose(&self) {
        self.dispose_calls.fetch_add(1, Ordering::SeqCst);
        if self.slow_dispose.load(Ordering::SeqCst) {
            self.dispose_release.notified().await;
        }
    }
}

async fn wait_for(mut pred: impl FnMut() -> bool, msg: &str) {
    for _ in 0..2500 {
        if pred() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("timed out waiting for: {msg}");
}

async fn start_rpc_mode(
    session: Arc<TestRpcSession>,
) -> (Arc<InMemoryRpcIo>, Arc<TestRpcRuntime>, tokio::task::JoinHandle<()>) {
    let io = InMemoryRpcIo::new();
    let runtime = TestRpcRuntime::new(session);
    let rt_run = Arc::clone(&runtime) as Arc<dyn RpcSessionRuntime>;
    let io_run = Arc::clone(&io) as Arc<dyn RpcIo>;
    let handle = tokio::spawn(async move {
        let _ = run_rpc_mode_with_io(rt_run, io_run).await;
    });
    wait_for(|| io.has_line_handler() && io.has_signal_handler(), "rpc mode ready").await;
    (io, runtime, handle)
}

// ============================================================================
// Tests
// ============================================================================

mod rpc_prompt_response_semantics {
    use super::*;

    #[tokio::test]
    async fn emits_one_failure_response_when_prompt_preflight_rejects() {
        let session = TestRpcSession::new(false, "fake-provider", 0);
        let (io, _rt, handle) = start_rpc_mode(session).await;

        io.send_line(&pi_js::json::stringify(&json!({
            "id": "b1",
            "type": "prompt",
            "message": "Hello"
        })));

        wait_for(|| io.prompt_responses("b1").len() == 1, "b1 failure response").await;
        let responses = io.prompt_responses("b1");
        pretty_assert_eq!(responses.len(), 1);
        pi_js::testing::assert_json_matches(
            &responses[0],
            &json!({
                "id": "b1",
                "type": "response",
                "command": "prompt",
                "success": false
            }),
        );
        let err = responses[0]["error"].as_str().expect("error string");
        assert!(
            err.contains(
                "No API key found for fake-provider.\n\nUse /login to log into a provider via OAuth or API key. See:"
            ),
            "unexpected error: {err}"
        );

        handle.abort();
    }

    // #9098: a successful prompt may start an agent run or be consumed by an extension.
    #[tokio::test]
    async fn emits_one_started_response_when_prompt_preflight_succeeds() {
        let session = TestRpcSession::new(true, "anthropic", 0);
        let (io, _rt, handle) = start_rpc_mode(session).await;

        io.send_line(&pi_js::json::stringify(&json!({
            "id": "b2",
            "type": "prompt",
            "message": "Hello"
        })));

        wait_for(|| io.prompt_responses("b2").len() == 1, "b2 started response").await;
        let responses = io.prompt_responses("b2");
        pretty_assert_eq!(responses.len(), 1);
        pi_js::testing::assert_json_matches(
            &responses[0],
            &json!({
                "id": "b2",
                "type": "response",
                "command": "prompt",
                "success": true,
                "data": { "disposition": "started" }
            }),
        );

        handle.abort();
    }

    #[tokio::test]
    async fn reports_extension_commands_and_intercepted_input_as_handled_without_starting_a_run() {
        let session = TestRpcSession::new(false, "anthropic", 0);
        session.register_command("handled", Arc::new(|_| Box::pin(async {})));
        session.set_input_interceptor(Arc::new(|text, _source, _behavior, _session| {
            let text = text.to_string();
            Box::pin(async move {
                if text == "handled input" {
                    Some(InputInterceptResult::Handled)
                } else {
                    None
                }
            })
        }));

        let (io, _rt, handle) = start_rpc_mode(session).await;

        for (id, message) in [("command", "/handled"), ("input", "handled input")] {
            io.send_line(&pi_js::json::stringify(&json!({
                "id": id,
                "type": "prompt",
                "message": message
            })));
            wait_for(|| !io.prompt_responses(id).is_empty(), "handled prompt response").await;
            pretty_assert_eq!(
                io.prompt_responses(id),
                vec![json!({
                    "id": id,
                    "type": "response",
                    "command": "prompt",
                    "success": true,
                    "data": { "disposition": "handled" }
                })]
            );
        }

        let agent_starts: Vec<_> = io
            .parsed_output_lines()
            .into_iter()
            .filter(|line| line.get("type").and_then(Value::as_str) == Some("agent_start"))
            .collect();
        pretty_assert_eq!(agent_starts.len(), 0);

        handle.abort();
    }

    #[tokio::test]
    async fn emits_one_success_response_when_prompt_is_queued_during_streaming() {
        let session = TestRpcSession::new(true, "anthropic", 100);
        let (io, _rt, handle) = start_rpc_mode(session).await;

        io.send_line(&pi_js::json::stringify(&json!({
            "id": "b3-start",
            "type": "prompt",
            "message": "Start"
        })));
        wait_for(|| io.prompt_responses("b3-start").len() == 1, "b3-start response").await;

        io.clear_output();
        io.send_line(&pi_js::json::stringify(&json!({
            "id": "b3",
            "type": "prompt",
            "message": "Queue this",
            "streamingBehavior": "followUp"
        })));

        wait_for(|| io.prompt_responses("b3").len() == 1, "b3 queued response").await;
        let responses = io.prompt_responses("b3");
        pretty_assert_eq!(responses.len(), 1);
        pi_js::testing::assert_json_matches(
            &responses[0],
            &json!({
                "id": "b3",
                "type": "response",
                "command": "prompt",
                "success": true,
                "data": { "disposition": "queued" }
            }),
        );

        tokio::time::sleep(Duration::from_millis(150)).await;
        handle.abort();
    }

    // #9803: a handler can consume A while independently queueing B; report A's outcome.
    #[tokio::test]
    async fn reports_steer_and_follow_up_as_handled_even_when_an_extension_queues_another_message() {
        for cmd_type in ["steer", "follow_up"] {
            let session = TestRpcSession::new(true, "anthropic", 250);
            let deliver_as = if cmd_type == "steer" {
                RpcStreamingBehavior::Steer
            } else {
                RpcStreamingBehavior::FollowUp
            };
            session.set_input_interceptor(Arc::new(move |text, source, _behavior, sess| {
                let text = text.to_string();
                let source = source.to_string();
                if text == "A" && source == "rpc" {
                    sess.queue_user_message("B", deliver_as);
                    Box::pin(async { Some(InputInterceptResult::Handled) })
                } else {
                    Box::pin(async { None })
                }
            }));

            let (io, _rt, handle) = start_rpc_mode(session).await;

            io.send_line(&pi_js::json::stringify(&json!({
                "id": "start",
                "type": "prompt",
                "message": "Start"
            })));
            wait_for(|| io.prompt_responses("start").len() == 1, "start response").await;

            io.send_line(&pi_js::json::stringify(&json!({
                "id": "A",
                "type": cmd_type,
                "message": "A"
            })));

            let expected_resp = json!({
                "id": "A",
                "type": "response",
                "command": cmd_type,
                "success": true,
                "data": { "disposition": "handled" }
            });
            let expected_queue = json!({
                "type": "queue_update",
                "steering": if cmd_type == "steer" { vec!["B"] } else { Vec::<&str>::new() },
                "followUp": if cmd_type == "follow_up" { vec!["B"] } else { Vec::<&str>::new() },
            });

            wait_for(
                || {
                    let lines = io.parsed_output_lines();
                    lines.contains(&expected_resp) && lines.contains(&expected_queue)
                },
                &format!("{cmd_type} handled response and queue_update"),
            )
            .await;

            let a_responses: Vec<_> = io
                .parsed_output_lines()
                .into_iter()
                .filter(|line| {
                    line.get("type").and_then(Value::as_str) == Some("response")
                        && line.get("id").and_then(Value::as_str) == Some("A")
                })
                .collect();
            pretty_assert_eq!(a_responses.len(), 1, "case {cmd_type}");

            handle.abort();
        }
    }

    #[tokio::test]
    async fn reports_steer_and_follow_up_as_queued_after_input_transformation() {
        for cmd_type in ["steer", "follow_up"] {
            let session = TestRpcSession::new(false, "anthropic", 0);
            session.set_input_interceptor(Arc::new(|text, _source, _behavior, _sess| {
                let text = text.to_string();
                Box::pin(async move {
                    if text == "A" {
                        Some(InputInterceptResult::Transform("B".to_string()))
                    } else {
                        None
                    }
                })
            }));

            let (io, _rt, handle) = start_rpc_mode(session).await;

            io.send_line(&pi_js::json::stringify(&json!({
                "id": "A",
                "type": cmd_type,
                "message": "A"
            })));

            let expected_resp = json!({
                "id": "A",
                "type": "response",
                "command": cmd_type,
                "success": true,
                "data": { "disposition": "queued" }
            });
            let expected_queue = json!({
                "type": "queue_update",
                "steering": if cmd_type == "steer" { vec!["B"] } else { Vec::<&str>::new() },
                "followUp": if cmd_type == "follow_up" { vec!["B"] } else { Vec::<&str>::new() },
            });

            wait_for(
                || {
                    let lines = io.parsed_output_lines();
                    lines.contains(&expected_resp) && lines.contains(&expected_queue)
                },
                &format!("{cmd_type} queued response and queue_update"),
            )
            .await;

            handle.abort();
        }
    }

    #[tokio::test]
    async fn returns_and_clears_queued_steering_and_follow_up_messages() {
        let session = TestRpcSession::new(true, "anthropic", 150);
        let (io, _rt, handle) = start_rpc_mode(session).await;

        io.send_line(&pi_js::json::stringify(&json!({
            "id": "clear-start",
            "type": "prompt",
            "message": "Start"
        })));
        wait_for(|| io.prompt_responses("clear-start").len() == 1, "clear-start").await;

        io.send_line(&pi_js::json::stringify(&json!({
            "id": "clear-steering",
            "type": "prompt",
            "message": "Change direction",
            "streamingBehavior": "steer"
        })));
        wait_for(|| io.prompt_responses("clear-steering").len() == 1, "clear-steering").await;
        pi_js::testing::assert_json_matches(
            &io.prompt_responses("clear-steering")[0],
            &json!({ "data": { "disposition": "queued" } }),
        );

        io.send_line(&pi_js::json::stringify(&json!({
            "id": "clear-follow-up",
            "type": "prompt",
            "message": "Summarize when finished",
            "streamingBehavior": "followUp"
        })));
        wait_for(|| io.prompt_responses("clear-follow-up").len() == 1, "clear-follow-up").await;

        io.send_line(&pi_js::json::stringify(&json!({
            "id": "clear",
            "type": "clear_queue"
        })));
        let expected = json!({
            "id": "clear",
            "type": "response",
            "command": "clear_queue",
            "success": true,
            "data": {
                "steering": ["Change direction"],
                "followUp": ["Summarize when finished"]
            }
        });
        wait_for(|| io.parsed_output_lines().contains(&expected), "clear_queue response").await;

        tokio::time::sleep(Duration::from_millis(200)).await;
        let agent_starts: Vec<_> = io
            .parsed_output_lines()
            .into_iter()
            .filter(|line| line.get("type").and_then(Value::as_str) == Some("agent_start"))
            .collect();
        pretty_assert_eq!(agent_starts.len(), 1);

        handle.abort();
    }

    // #5868 + clone + get_entries extra verification
    #[tokio::test]
    async fn preserves_request_id_on_unknown_command_and_handles_clone_and_get_entries() {
        let session = TestRpcSession::new(false, "anthropic", 0);
        session.register_entries(
            vec![
                json!({ "type": "message", "id": "e1", "parentId": null, "timestamp": "2026-01-01T00:00:00Z" }),
                json!({ "type": "message", "id": "e2", "parentId": "e1", "timestamp": "2026-01-01T00:00:01Z" }),
            ],
            Some("e2".to_string()),
        );
        let (io, rt, handle) = start_rpc_mode(session).await;

        io.send_line(&pi_js::json::stringify(&json!({ "id": "u1", "type": "foobar" })));
        let expected_unknown = json!({
            "id": "u1",
            "type": "response",
            "command": "foobar",
            "success": false,
            "error": "Unknown command: foobar"
        });
        wait_for(
            || io.parsed_output_lines().contains(&expected_unknown),
            "unknown command error",
        )
        .await;

        io.send_line(&pi_js::json::stringify(
            &json!({ "id": "ge1", "type": "get_entries", "since": "e1" }),
        ));
        let expected_entries = json!({
            "id": "ge1",
            "type": "response",
            "command": "get_entries",
            "success": true,
            "data": {
                "entries": [{ "type": "message", "id": "e2", "parentId": "e1", "timestamp": "2026-01-01T00:00:01Z" }],
                "leafId": "e2"
            }
        });
        wait_for(
            || io.parsed_output_lines().contains(&expected_entries),
            "get_entries since",
        )
        .await;

        io.send_line(&pi_js::json::stringify(&json!({ "id": "c1", "type": "clone" })));
        let expected_clone = json!({
            "id": "c1",
            "type": "response",
            "command": "clone",
            "success": true,
            "data": { "cancelled": false }
        });
        wait_for(|| io.parsed_output_lines().contains(&expected_clone), "clone response").await;
        pretty_assert_eq!(rt.fork_calls.lock().unwrap().len(), 1);
        pretty_assert_eq!(rt.fork_calls.lock().unwrap()[0].0, "e2");

        handle.abort();
    }
}

mod rpc_extension_shutdown_requests {
    use super::*;

    // #11599: an async ctx.shutdown() while idle must not wait for another stdin command.
    #[tokio::test]
    async fn shuts_down_an_idle_session_when_an_extension_requests_shutdown_asynchronously() {
        let session = TestRpcSession::new(false, "anthropic", 0);
        let shutdown_slot: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>> = Arc::new(Mutex::new(None));
        let slot_c = Arc::clone(&shutdown_slot);
        session.set_on_session_start(Arc::new(move |_sess, shutdown| {
            *slot_c.lock().unwrap() = Some(shutdown);
        }));

        let (io, _rt, _handle) = start_rpc_mode(session).await;
        wait_for(
            || shutdown_slot.lock().unwrap().is_some(),
            "session_start captured shutdown",
        )
        .await;
        assert!(io.exit_codes().is_empty());

        let request_shutdown = shutdown_slot.lock().unwrap().clone().unwrap();
        request_shutdown();

        wait_for(|| io.exit_codes() == vec![0], "idle shutdown exits 0").await;
        pretty_assert_eq!(io.exit_codes().len(), 1);
    }

    #[tokio::test]
    async fn writes_the_command_response_before_shutting_down_for_a_shutdown_requested_by_an_extension_command() {
        let session = TestRpcSession::new(false, "anthropic", 0);
        session.register_command(
            "quit-now",
            Arc::new(|sess| {
                let shutdown = sess.state.lock().unwrap().shutdown_handler.clone().unwrap();
                Box::pin(async move {
                    shutdown();
                    tokio::time::sleep(Duration::from_millis(10)).await;
                })
            }),
        );

        let (io, _rt, _handle) = start_rpc_mode(session).await;
        let io_for_hook = Arc::clone(&io);
        io.set_on_exit_hook(move |_| {
            io_for_hook.push_raw_output(&format!("{}\n", pi_js::json::stringify(&json!({ "type": "exit" }))));
        });

        io.send_line(&pi_js::json::stringify(&json!({
            "id": "q",
            "type": "prompt",
            "message": "/quit-now"
        })));

        wait_for(|| io.exit_codes().len() == 1, "exit called once").await;
        let types: Vec<String> = io
            .parsed_output_lines()
            .into_iter()
            .map(|line| {
                if line.get("id").and_then(Value::as_str) == Some("q") {
                    "response".to_string()
                } else {
                    line.get("type").and_then(Value::as_str).unwrap_or("").to_string()
                }
            })
            .collect();
        let resp_idx = types.iter().position(|t| t == "response");
        let exit_idx = types.iter().position(|t| t == "exit");
        assert!(resp_idx.is_some());
        assert!(resp_idx < exit_idx, "expected response before exit: {types:?}");
    }

    #[tokio::test]
    async fn waits_for_a_command_that_arrives_before_an_idle_shutdown_request_is_processed() {
        let session = TestRpcSession::new(false, "anthropic", 0);
        let shutdown_slot: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>> = Arc::new(Mutex::new(None));
        let slot_c = Arc::clone(&shutdown_slot);
        session.set_on_session_start(Arc::new(move |_sess, shutdown| {
            *slot_c.lock().unwrap() = Some(shutdown);
        }));
        session.register_command(
            "slow",
            Arc::new(|_| {
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                })
            }),
        );

        let (io, _rt, _handle) = start_rpc_mode(session).await;
        let io_for_hook = Arc::clone(&io);
        io.set_on_exit_hook(move |_| {
            io_for_hook.push_raw_output(&format!("{}\n", pi_js::json::stringify(&json!({ "type": "exit" }))));
        });

        wait_for(
            || shutdown_slot.lock().unwrap().is_some(),
            "session_start captured shutdown",
        )
        .await;

        let request_shutdown = shutdown_slot.lock().unwrap().clone().unwrap();
        // The idle check is scheduled for the next tick; the command starts before it runs.
        request_shutdown();
        io.send_line(&pi_js::json::stringify(&json!({
            "id": "s",
            "type": "prompt",
            "message": "/slow"
        })));

        wait_for(|| io.exit_codes().len() == 1, "exit called once").await;
        let types: Vec<String> = io
            .parsed_output_lines()
            .into_iter()
            .filter_map(|line| {
                if line.get("id").and_then(Value::as_str) == Some("s") {
                    Some("response".to_string())
                } else {
                    let t = line.get("type").and_then(Value::as_str).unwrap_or("");
                    if t == "exit" { Some("exit".to_string()) } else { None }
                }
            })
            .collect();
        pretty_assert_eq!(types, vec!["response".to_string(), "exit".to_string()]);
    }
}

mod rpc_shutdown_triggers {
    use super::*;

    async fn start_with_slow_dispose() -> (Arc<InMemoryRpcIo>, Arc<TestRpcRuntime>, tokio::task::JoinHandle<()>) {
        let session = TestRpcSession::new(false, "anthropic", 0);
        let (io, runtime, handle) = start_rpc_mode(session).await;
        runtime.enable_slow_dispose();
        (io, runtime, handle)
    }

    // fcatuhe's report on #11599: Paseo closes stdin and sends SIGTERM together.
    #[tokio::test]
    async fn lets_a_shutdown_started_by_stdin_eof_finish_when_one_sigterm_follows() {
        let (io, runtime, _handle) = start_with_slow_dispose().await;

        io.emit_end();
        wait_for(|| runtime.dispose_count() == 1, "dispose called once").await;

        assert!(io.has_signal_handler());
        io.emit_signal(RpcSignal::SigTerm);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(io.exit_codes().is_empty());

        runtime.finish_dispose();
        wait_for(|| io.exit_codes() == vec![0], "exit called with 0").await;
        pretty_assert_eq!(io.exit_codes().len(), 1);
    }

    #[tokio::test]
    async fn lets_a_shutdown_started_by_sigterm_finish_when_stdin_eof_follows() {
        let (io, runtime, _handle) = start_with_slow_dispose().await;

        io.emit_signal(RpcSignal::SigTerm);
        wait_for(|| runtime.dispose_count() == 1, "dispose called once").await;

        io.emit_end();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(io.exit_codes().is_empty());

        runtime.finish_dispose();
        wait_for(|| io.exit_codes() == vec![143], "exit called with 143").await;
        pretty_assert_eq!(io.exit_codes().len(), 1);
        pretty_assert_eq!(runtime.dispose_count(), 1);
    }

    #[tokio::test]
    async fn exits_at_once_on_a_repeated_signal_during_shutdown() {
        let (io, runtime, _handle) = start_with_slow_dispose().await;

        io.emit_end();
        wait_for(|| runtime.dispose_count() == 1, "dispose called once").await;

        io.emit_signal(RpcSignal::SigTerm);
        io.emit_signal(RpcSignal::SigTerm);
        wait_for(|| io.exit_codes() == vec![143], "forced exit 143").await;
        pretty_assert_eq!(io.exit_codes().len(), 1);
    }
}

mod rpc_wire_and_client_round_trip {
    use super::*;
    use pi_coding_agent::modes::rpc::jsonl::{attach_jsonl_line_reader, serialize_json_line_value};
    use pi_coding_agent::modes::rpc::rpc_client::{RpcClient, RpcClientOptions};
    use pi_coding_agent::modes::rpc::rpc_types::{RpcExtensionUIRequest, RpcExtensionUIResponse, RpcResponse};
    use tokio::io::AsyncWriteExt;

    #[test]
    fn serializes_rpc_response_and_ui_wire_objects_in_ts_key_order() {
        let resp = RpcResponse::success(Some("req_1".to_string()), "cycle_model", &Value::Null).unwrap();
        let line = pi_js::json::stringify(&resp);
        pretty_assert_eq!(
            line,
            r#"{"id":"req_1","type":"response","command":"cycle_model","success":true,"data":null}"#
        );

        let void_resp = RpcResponse::success_empty(None, "abort");
        pretty_assert_eq!(
            pi_js::json::stringify(&void_resp),
            r#"{"type":"response","command":"abort","success":true}"#
        );

        let ui_req = RpcExtensionUIRequest::Select {
            id: "ui_1".to_string(),
            title: "Pick".to_string(),
            options: vec!["a".to_string(), "b".to_string()],
            timeout: Some(500),
        };
        pretty_assert_eq!(
            pi_js::json::stringify(&ui_req),
            r#"{"type":"extension_ui_request","id":"ui_1","method":"select","title":"Pick","options":["a","b"],"timeout":500}"#
        );

        let ui_resp = RpcExtensionUIResponse::Cancelled { id: "ui_1".to_string() };
        pretty_assert_eq!(
            pi_js::json::stringify(&ui_resp),
            r#"{"type":"extension_ui_response","id":"ui_1","cancelled":true}"#
        );
    }

    #[tokio::test]
    async fn round_trips_commands_and_events_over_duplex_streams_and_handles_listener_unsubscribe_during_dispatch() {
        struct DuplexRpcIo {
            reader: Mutex<Option<tokio::io::DuplexStream>>,
            writer_tx: tokio::sync::mpsc::UnboundedSender<String>,
        }

        #[async_trait]
        impl RpcIo for DuplexRpcIo {
            fn take_over_stdout(&self) {}
            fn write_line(&self, line: &str) {
                let _ = self.writer_tx.send(line.to_string());
            }
            fn wait_for_stdout_backpressure(&self) -> BoxFuture<()> {
                Box::pin(async {})
            }
            fn flush_stdout(&self) -> BoxFuture<pi_js::Result<()>> {
                Box::pin(async { Ok(()) })
            }
            fn kill_tracked_detached_children(&self) {}
            fn attach_input(&self, on_line: RpcLineCallback, on_end: RpcEndCallback) -> Unsubscribe {
                let r = self.reader.lock().unwrap().take().expect("reader taken once");
                let handle = attach_jsonl_line_reader(r, on_line, Some(on_end));
                Unsubscribe::new(move || handle.detach())
            }
            fn subscribe_signals(&self, _on_signal: RpcSignalCallback) -> Unsubscribe {
                Unsubscribe::noop()
            }
            fn on_exit(&self, _exit_code: i32) {}
        }

        let (client_to_server_tx, server_rx) = tokio::io::duplex(8192);
        let (server_to_client_tx, client_rx) = tokio::io::duplex(8192);

        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let mut w = server_to_client_tx;
            while let Some(line) = out_rx.recv().await {
                if w.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                let _ = w.flush().await;
            }
        });

        let session = TestRpcSession::new(true, "anthropic", 10);
        let runtime = TestRpcRuntime::new(session);
        let io = Arc::new(DuplexRpcIo {
            reader: Mutex::new(Some(server_rx)),
            writer_tx: out_tx,
        });
        let server_task = tokio::spawn(async move {
            let _ = run_rpc_mode_with_io(runtime, io).await;
        });

        let client = RpcClient::new(RpcClientOptions::default());
        client
            .start_with_io(client_rx, client_to_server_tx)
            .await
            .expect("start_with_io");

        // #9990: unsubscribing first listener during dispatch must not skip second listener.
        let second_hits = Arc::new(AtomicUsize::new(0));
        let second_hits_c = Arc::clone(&second_hits);
        let unsub_slot: Arc<Mutex<Option<Unsubscribe>>> = Arc::new(Mutex::new(None));
        let unsub_slot_c = Arc::clone(&unsub_slot);
        let u1 = client.on_event(Arc::new(move |_| {
            if let Some(u) = unsub_slot_c.lock().unwrap().take() {
                u.call();
            }
        }));
        *unsub_slot.lock().unwrap() = Some(u1);
        let u2 = client.on_event(Arc::new(move |_| {
            second_hits_c.fetch_add(1, Ordering::SeqCst);
        }));

        let state = client.get_state().await.expect("get_state");
        pretty_assert_eq!(state.session_id, "test-session-id");
        pretty_assert_eq!(client.cycle_model().await.expect("cycle_model"), None);

        let events = client
            .prompt_and_wait("hello", None, Some(2000))
            .await
            .expect("prompt_and_wait");
        assert!(!events.is_empty());
        assert!(second_hits.load(Ordering::SeqCst) > 0);

        u2.call();
        client.stop().await;
        server_task.abort();
        let _ = serialize_json_line_value(&json!({}));
    }
}
