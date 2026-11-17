//! Port of packages/coding-agent/test/suite/regressions/9068-user-bash-fail-closed.test.ts (unit coding-agent-modes-print-json-rpc)

// #9068

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_agent_core::{AgentMessage, QueueMode};
use pi_ai::{ImageContent, Model};
use pi_coding_agent::modes::json_event::AgentSessionEvent;
use pi_coding_agent::modes::rpc::rpc_mode::{
    RpcEndCallback, RpcExecuteBashOptions, RpcExtensionBindings, RpcExtensionError, RpcForkOptions, RpcForkResult,
    RpcIo, RpcLineCallback, RpcNavigateTreeOptions, RpcNewSessionOptions, RpcPromptOptions, RpcSession,
    RpcSessionRuntime, RpcSignalCallback, RpcSwitchSessionOptions, RpcUserBashOutcome, run_rpc_mode_with_io,
};
use pi_coding_agent::modes::rpc::rpc_types::{
    BashResult, CancelledResult, ClearQueueResult, CompactionResult, CycleModelResponseData, ForkMessage,
    QueuedInputDisposition, RpcSlashCommand, SessionEntry, SessionStats, SessionTokenStats, SessionTreeNode,
    ThinkingLevel,
};
use pi_js::{BoxFuture, Error, Unsubscribe};
use serde_json::{Value, json};

#[derive(Default)]
struct InMemoryRpcIo {
    output_lines: Mutex<Vec<String>>,
    line_handler: Mutex<Option<RpcLineCallback>>,
    end_handler: Mutex<Option<RpcEndCallback>>,
    signal_handler: Mutex<Option<RpcSignalCallback>>,
}

impl InMemoryRpcIo {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
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

    fn has_line_handler(&self) -> bool {
        self.line_handler.lock().unwrap().is_some()
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

    fn on_exit(&self, _exit_code: i32) {}
}

fn local_result() -> BashResult {
    BashResult {
        output: "local output".to_string(),
        exit_code: Some(0),
        cancelled: false,
        truncated: false,
        full_output_path: None,
    }
}

enum UserBashBehavior {
    Throw(&'static str),
    InvalidResult(&'static str),
    Passthrough,
}

struct TestRpcSession {
    behavior: UserBashBehavior,
    execute_count: AtomicUsize,
    on_error: Mutex<Option<Arc<dyn Fn(RpcExtensionError) + Send + Sync>>>,
}

impl TestRpcSession {
    fn new(behavior: UserBashBehavior) -> Arc<Self> {
        Arc::new(Self {
            behavior,
            execute_count: AtomicUsize::new(0),
            on_error: Mutex::new(None),
        })
    }
}

#[async_trait]
impl RpcSession for TestRpcSession {
    async fn bind_extensions(&self, bindings: RpcExtensionBindings) -> pi_js::Result<()> {
        *self.on_error.lock().unwrap() = Some(bindings.on_error);
        Ok(())
    }

    fn subscribe(&self, _listener: Arc<dyn Fn(AgentSessionEvent) + Send + Sync>) -> Unsubscribe {
        Unsubscribe::noop()
    }

    fn subscribe_agent_backpressure(&self, _listener: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>) -> Unsubscribe {
        Unsubscribe::noop()
    }

    async fn prompt(&self, _message: &str, _options: RpcPromptOptions) -> pi_js::Result<()> {
        Ok(())
    }

    async fn steer(&self, _message: &str, _images: Option<Vec<ImageContent>>) -> pi_js::Result<QueuedInputDisposition> {
        Ok(QueuedInputDisposition::Handled)
    }

    async fn follow_up(
        &self,
        _message: &str,
        _images: Option<Vec<ImageContent>>,
    ) -> pi_js::Result<QueuedInputDisposition> {
        Ok(QueuedInputDisposition::Handled)
    }

    async fn abort(&self) {}

    fn clear_queue(&self) -> ClearQueueResult {
        ClearQueueResult::default()
    }

    fn is_idle(&self) -> bool {
        true
    }

    async fn wait_for_idle(&self) {}

    fn model(&self) -> Option<Model> {
        None
    }

    fn thinking_level(&self) -> ThinkingLevel {
        ThinkingLevel::Off
    }

    fn is_streaming(&self) -> bool {
        false
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
        "test-session-9068".to_string()
    }

    fn session_name(&self) -> Option<String> {
        None
    }

    fn auto_compaction_enabled(&self) -> bool {
        false
    }

    fn messages(&self) -> Vec<AgentMessage> {
        Vec::new()
    }

    fn pending_message_count(&self) -> usize {
        0
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
        vec![ThinkingLevel::Off]
    }

    fn set_steering_mode(&self, _mode: QueueMode) {}

    fn set_follow_up_mode(&self, _mode: QueueMode) {}

    async fn compact(&self, _custom_instructions: Option<&str>) -> pi_js::Result<CompactionResult> {
        Err(Error::msg("not implemented"))
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
        match self.behavior {
            UserBashBehavior::Throw(msg) | UserBashBehavior::InvalidResult(msg) => {
                if let Some(on_error) = self.on_error.lock().unwrap().clone() {
                    on_error(RpcExtensionError {
                        extension_path: "inline-ext".to_string(),
                        event: "user_bash".to_string(),
                        error: msg.to_string(),
                        stack: None,
                    });
                }
                Err(Error::msg(msg))
            }
            UserBashBehavior::Passthrough => Ok(None),
        }
    }

    fn record_bash_result(&self, _command: &str, _result: &BashResult, _exclude_from_context: Option<bool>) {}

    async fn execute_bash(&self, _command: &str, _options: RpcExecuteBashOptions) -> pi_js::Result<BashResult> {
        self.execute_count.fetch_add(1, Ordering::SeqCst);
        Ok(local_result())
    }

    fn abort_bash(&self) {}

    fn get_session_stats(&self) -> SessionStats {
        SessionStats {
            session_file: None,
            session_id: "test-session-9068".to_string(),
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
        Err(Error::msg("not implemented"))
    }

    fn leaf_id(&self) -> Option<String> {
        None
    }

    fn get_user_messages_for_forking(&self) -> Vec<ForkMessage> {
        Vec::new()
    }

    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
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

    async fn fork(&self, _entry_id: &str, _options: Option<RpcForkOptions>) -> pi_js::Result<RpcForkResult> {
        Ok(RpcForkResult {
            cancelled: true,
            selected_text: Some(String::new()),
        })
    }

    async fn dispose(&self) {}
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

mod rpc_user_bash_failure_handling_9068 {
    use super::*;
    use pretty_assertions::assert_eq;

    async fn run_rpc_bash_case(
        case_name: &str,
        behavior: UserBashBehavior,
        expected_error: Option<&str>,
        expected_execute_count: usize,
    ) {
        let io = InMemoryRpcIo::new();
        let session = TestRpcSession::new(behavior);
        let runtime: Arc<dyn RpcSessionRuntime> = Arc::new(TestRpcRuntime {
            session: Arc::clone(&session),
        });
        let io_run = Arc::clone(&io) as Arc<dyn RpcIo>;
        let handle = tokio::spawn(async move {
            let _ = run_rpc_mode_with_io(runtime, io_run).await;
        });

        wait_for(|| io.has_line_handler(), "line handler attached").await;

        io.send_line(&pi_js::json::stringify(&json!({
            "id": "bash-request",
            "type": "bash",
            "command": "pwd"
        })));

        wait_for(
            || {
                io.parsed_output_lines().iter().any(|o| {
                    o.get("type").and_then(Value::as_str) == Some("response")
                        && o.get("id") == Some(&json!("bash-request"))
                })
            },
            &format!("{case_name}: bash-request response"),
        )
        .await;

        let outputs = io.parsed_output_lines();
        let response = outputs
            .iter()
            .find(|o| {
                o.get("type").and_then(Value::as_str) == Some("response") && o.get("id") == Some(&json!("bash-request"))
            })
            .expect("bash response");

        assert_eq!(response.get("id"), Some(&json!("bash-request")), "{case_name}");
        assert_eq!(response.get("type"), Some(&json!("response")), "{case_name}");
        assert_eq!(response.get("command"), Some(&json!("bash")), "{case_name}");
        assert_eq!(
            response.get("success"),
            Some(&json!(expected_error.is_none())),
            "{case_name}"
        );

        if let Some(err_substr) = expected_error {
            let err_text = response.get("error").and_then(Value::as_str).unwrap_or("");
            assert!(
                err_text.contains(err_substr),
                "{case_name}: expected error containing {err_substr:?}, got {err_text:?}"
            );
            assert!(
                outputs.iter().any(|o| {
                    o.get("type").and_then(Value::as_str) == Some("extension_error")
                        && o.get("event").and_then(Value::as_str) == Some("user_bash")
                        && o.get("error")
                            .and_then(Value::as_str)
                            .is_some_and(|e| e.contains(err_substr))
                }),
                "{case_name}: expected extension_error event in {outputs:?}"
            );
        } else {
            let expected_data = pi_js::json::parse(&pi_js::json::stringify(&local_result())).unwrap();
            assert_eq!(response.get("data"), Some(&expected_data), "{case_name}");
        }

        assert_eq!(
            session.execute_count.load(Ordering::SeqCst),
            expected_execute_count,
            "{case_name}"
        );

        io.emit_end();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn rpc_user_bash_cases() {
        run_rpc_bash_case(
            "fails the request without executing bash when a handler throws",
            UserBashBehavior::Throw("Routing failed"),
            Some("Routing failed"),
            0,
        )
        .await;

        run_rpc_bash_case(
            "fails the request without executing bash when a handler returns an empty result",
            UserBashBehavior::InvalidResult("Invalid user_bash handler result"),
            Some("Invalid user_bash handler result"),
            0,
        )
        .await;

        run_rpc_bash_case(
            "executes bash normally when a handler returns undefined",
            UserBashBehavior::Passthrough,
            None,
            1,
        )
        .await;
    }
}
