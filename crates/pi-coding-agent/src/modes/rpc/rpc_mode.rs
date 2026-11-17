//! Port of packages/coding-agent/src/modes/rpc/rpc-mode.ts

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_agent_core::{AgentMessage, QueueMode};
use pi_ai::{ImageContent, Model};
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Unsubscribe};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::modes::interactive::theme::theme::{SetThemeResult, Theme, ThemeInfo, theme};
use crate::modes::json_event::{AgentSessionEvent, to_json_event};
use crate::modes::rpc::jsonl::{attach_jsonl_line_reader, serialize_json_line_value};

// Re-export types for consumers
use super::rpc_types::{
    AvailableModelsResponseData, AvailableThinkingLevelsResponseData, BashResult, CancelledResult, ClearQueueResult,
    CompactionResult, CycleModelResponseData, CycleThinkingLevelResult, ExportHtmlResult, ForkMessage,
    ForkMessagesResponseData, ForkResult, GetCommandsResponseData, GetEntriesResult, GetMessagesResponseData,
    GetTreeResult, LastAssistantTextResponseData, NotifyType, PromptDisposition, PromptResponseData,
    QueuedInputDisposition, QueuedInputResponseData, RpcCommandType, RpcExtensionErrorEvent, RpcSlashCommand,
    RpcSlashCommandSource, RpcStreamingBehavior, SessionEntry, SessionStats, SessionTreeNode, ThinkingLevel,
    session_entry_id, session_tree_node_to_rpc,
};
pub use super::rpc_types::{RpcCommand, RpcExtensionUIRequest, RpcExtensionUIResponse, RpcResponse, RpcSessionState};

// ============================================================================
// Runtime / Session / IO Seams
// ============================================================================

/// Dialog options passed to RPC extension UI prompts.
pub type RpcExtensionDialogOptions = crate::core::extensions::types::ExtensionUIDialogOptions;

/// Widget options passed to RPC extension UI `set_widget_lines`.
pub type RpcExtensionWidgetOptions = crate::core::extensions::types::ExtensionWidgetOptions;

/// Extension error payload forwarded onto the RPC wire.
pub type RpcExtensionError = crate::core::extensions::types::ExtensionError;

/// Fork placement for extension/RPC fork requests.
pub type RpcForkPosition = crate::core::extensions::types::ForkPosition;

/// Options for `RpcSessionRuntime::new_session`.
#[derive(Clone, Default)]
pub struct RpcNewSessionOptions {
    pub parent_session: Option<String>,
    /// Opaque runtime setup/withSession closures bridged by runtime adapters.
    pub opaque: Option<Arc<dyn Any + Send + Sync>>,
}

/// Options for `RpcSessionRuntime::switch_session`.
#[derive(Clone, Default)]
pub struct RpcSwitchSessionOptions {
    pub opaque: Option<Arc<dyn Any + Send + Sync>>,
}

/// Options for `RpcSessionRuntime::fork`.
#[derive(Clone, Default)]
pub struct RpcForkOptions {
    pub position: Option<RpcForkPosition>,
    pub opaque: Option<Arc<dyn Any + Send + Sync>>,
}

/// Result returned by `RpcSessionRuntime::fork`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RpcForkResult {
    pub cancelled: bool,
    pub selected_text: Option<String>,
}

/// Options for `RpcSession::navigate_tree`.
#[derive(Clone, Debug, Default)]
pub struct RpcNavigateTreeOptions {
    pub summarize: Option<bool>,
    pub custom_instructions: Option<String>,
    pub replace_instructions: Option<bool>,
    pub label: Option<String>,
}

/// Options for `RpcSession::prompt`.
#[derive(Clone)]
pub struct RpcPromptOptions {
    pub images: Option<Vec<ImageContent>>,
    pub streaming_behavior: Option<RpcStreamingBehavior>,
    pub source: &'static str,
    pub preflight_result: Arc<dyn Fn(PromptDisposition) + Send + Sync>,
}

/// Result of emitting `user_bash` extension event.
#[derive(Clone)]
pub enum RpcUserBashOutcome {
    Result(BashResult),
    Operations(Arc<dyn crate::core::tools::bash::BashOperations>),
}

impl From<crate::core::extensions::types::UserBashEventResult> for RpcUserBashOutcome {
    fn from(res: crate::core::extensions::types::UserBashEventResult) -> Self {
        match res {
            crate::core::extensions::types::UserBashEventResult::Result { result } => Self::Result(result.into()),
            crate::core::extensions::types::UserBashEventResult::Operations { operations } => {
                Self::Operations(operations)
            }
        }
    }
}

/// Options for `RpcSession::execute_bash`.
#[derive(Clone, Default)]
pub struct RpcExecuteBashOptions {
    pub exclude_from_context: Option<bool>,
    pub id: Option<String>,
    pub operations: Option<Arc<dyn crate::core::tools::bash::BashOperations>>,
}

/// Extension UI context interface implemented by RPC mode.
#[async_trait]
pub trait RpcExtensionUiContext: Send + Sync {
    async fn select(
        &self,
        title: &str,
        options: Vec<String>,
        opts: Option<RpcExtensionDialogOptions>,
    ) -> pi_js::Result<Option<String>>;

    async fn confirm(&self, title: &str, message: &str, opts: Option<RpcExtensionDialogOptions>)
    -> pi_js::Result<bool>;

    async fn input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        opts: Option<RpcExtensionDialogOptions>,
    ) -> pi_js::Result<Option<String>>;

    fn notify(&self, message: &str, notify_type: Option<NotifyType>);

    fn on_terminal_input(&self) -> Unsubscribe;

    fn set_status(&self, key: &str, text: Option<&str>);

    fn set_working_message(&self, message: Option<&str>);

    fn set_working_visible(&self, visible: bool);

    fn set_hidden_thinking_label(&self, label: Option<&str>);

    fn set_widget_lines(&self, key: &str, content: Option<Vec<String>>, options: Option<RpcExtensionWidgetOptions>);

    fn set_widget_factory_unsupported(&self);

    fn set_footer_unsupported(&self);

    fn set_header_unsupported(&self);

    fn set_title(&self, title: &str);

    async fn custom_unsupported(&self) -> pi_js::Result<Value>;

    fn paste_to_editor(&self, text: &str);

    fn set_editor_text(&self, text: &str);

    fn get_editor_text(&self) -> String;

    async fn editor(&self, title: &str, prefill: Option<&str>) -> pi_js::Result<Option<String>>;

    fn theme(&self) -> Theme;

    fn get_all_themes(&self) -> Vec<ThemeInfo>;

    fn get_theme(&self, name: &str) -> Option<Theme>;

    fn set_theme(&self, name: &str) -> SetThemeResult;

    fn get_tools_expanded(&self) -> bool;

    fn set_tools_expanded(&self, expanded: bool);
}

/// Actions exposed to extension commands via `ctx.*`.
#[derive(Clone)]
pub struct RpcCommandContextActions {
    pub wait_for_idle: Arc<dyn Fn() -> BoxFuture<pi_js::Result<()>> + Send + Sync>,
    pub new_session:
        Arc<dyn Fn(Option<RpcNewSessionOptions>) -> BoxFuture<pi_js::Result<CancelledResult>> + Send + Sync>,
    pub fork: Arc<dyn Fn(String, Option<RpcForkOptions>) -> BoxFuture<pi_js::Result<CancelledResult>> + Send + Sync>,
    pub navigate_tree:
        Arc<dyn Fn(String, Option<RpcNavigateTreeOptions>) -> BoxFuture<pi_js::Result<CancelledResult>> + Send + Sync>,
    pub switch_session:
        Arc<dyn Fn(String, Option<RpcSwitchSessionOptions>) -> BoxFuture<pi_js::Result<CancelledResult>> + Send + Sync>,
    pub reload: Arc<dyn Fn() -> BoxFuture<pi_js::Result<()>> + Send + Sync>,
}

/// Bindings passed to `RpcSession::bind_extensions`.
#[derive(Clone)]
pub struct RpcExtensionBindings {
    pub ui_context: Arc<dyn RpcExtensionUiContext>,
    pub mode: &'static str,
    pub command_context_actions: RpcCommandContextActions,
    pub shutdown_handler: Arc<dyn Fn() + Send + Sync>,
    pub on_error: Arc<dyn Fn(RpcExtensionError) + Send + Sync>,
}

/// Session operations consumed by RPC mode.
#[async_trait]
pub trait RpcSession: Send + Sync + 'static {
    async fn bind_extensions(&self, bindings: RpcExtensionBindings) -> pi_js::Result<()>;

    fn subscribe(&self, listener: Arc<dyn Fn(AgentSessionEvent) + Send + Sync>) -> Unsubscribe;

    fn subscribe_agent_backpressure(&self, listener: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>) -> Unsubscribe;

    async fn prompt(&self, message: &str, options: RpcPromptOptions) -> pi_js::Result<()>;

    async fn steer(&self, message: &str, images: Option<Vec<ImageContent>>) -> pi_js::Result<QueuedInputDisposition>;

    async fn follow_up(
        &self,
        message: &str,
        images: Option<Vec<ImageContent>>,
    ) -> pi_js::Result<QueuedInputDisposition>;

    async fn abort(&self);

    fn clear_queue(&self) -> ClearQueueResult;

    fn is_idle(&self) -> bool;

    async fn wait_for_idle(&self);

    fn model(&self) -> Option<Model>;

    fn thinking_level(&self) -> ThinkingLevel;

    fn is_streaming(&self) -> bool;

    fn is_compacting(&self) -> bool;

    fn steering_mode(&self) -> QueueMode;

    fn follow_up_mode(&self) -> QueueMode;

    fn session_file(&self) -> Option<String>;

    fn session_id(&self) -> String;

    fn session_name(&self) -> Option<String>;

    fn auto_compaction_enabled(&self) -> bool;

    fn messages(&self) -> Vec<AgentMessage>;

    fn pending_message_count(&self) -> usize;

    fn get_available_models(&self) -> Vec<Model>;

    async fn set_model(&self, model: Model) -> pi_js::Result<()>;

    async fn cycle_model(&self) -> pi_js::Result<Option<CycleModelResponseData>>;

    fn set_thinking_level(&self, level: ThinkingLevel);

    fn cycle_thinking_level(&self) -> Option<ThinkingLevel>;

    fn get_available_thinking_levels(&self) -> Vec<ThinkingLevel>;

    fn set_steering_mode(&self, mode: QueueMode);

    fn set_follow_up_mode(&self, mode: QueueMode);

    async fn compact(&self, custom_instructions: Option<&str>) -> pi_js::Result<CompactionResult>;

    fn set_auto_compaction_enabled(&self, enabled: bool);

    fn set_auto_retry_enabled(&self, enabled: bool);

    fn abort_retry(&self);

    fn cwd(&self) -> String;

    async fn emit_user_bash(
        &self,
        command: &str,
        exclude_from_context: bool,
        cwd: &str,
    ) -> pi_js::Result<Option<RpcUserBashOutcome>>;

    fn record_bash_result(&self, command: &str, result: &BashResult, exclude_from_context: Option<bool>);

    async fn execute_bash(&self, command: &str, options: RpcExecuteBashOptions) -> pi_js::Result<BashResult>;

    fn abort_bash(&self);

    fn get_session_stats(&self) -> SessionStats;

    async fn export_to_html(&self, output_path: Option<&str>) -> pi_js::Result<String>;

    fn leaf_id(&self) -> Option<String>;

    fn get_user_messages_for_forking(&self) -> Vec<ForkMessage>;

    fn get_entries(&self) -> Vec<SessionEntry>;

    fn get_tree(&self) -> Vec<SessionTreeNode>;

    fn get_last_assistant_text(&self) -> Option<String>;

    fn set_session_name(&self, name: &str);

    fn get_commands(&self) -> Vec<RpcSlashCommand>;

    async fn navigate_tree(
        &self,
        target_id: &str,
        options: Option<RpcNavigateTreeOptions>,
    ) -> pi_js::Result<CancelledResult>;

    async fn reload(&self) -> pi_js::Result<()>;
}

/// Runtime host abstraction consumed by RPC mode.
#[async_trait]
pub trait RpcSessionRuntime: Send + Sync + 'static {
    fn session(&self) -> Arc<dyn RpcSession>;

    fn set_rebind_session(&self, rebind: Option<Arc<dyn Fn() -> BoxFuture<pi_js::Result<()>> + Send + Sync>>);

    async fn new_session(&self, options: Option<RpcNewSessionOptions>) -> pi_js::Result<CancelledResult>;

    async fn switch_session(
        &self,
        session_path: &str,
        options: Option<RpcSwitchSessionOptions>,
    ) -> pi_js::Result<CancelledResult>;

    async fn fork(&self, entry_id: &str, options: Option<RpcForkOptions>) -> pi_js::Result<RpcForkResult>;

    async fn dispose(&self);
}

/// Process signals handled by RPC mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpcSignal {
    SigTerm,
    SigHup,
}

impl RpcSignal {
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::SigTerm => 143,
            Self::SigHup => 129,
        }
    }
}

pub type RpcLineCallback = Arc<dyn Fn(&str) + Send + Sync>;
pub type RpcEndCallback = Arc<dyn Fn() + Send + Sync>;
pub type RpcSignalCallback = Arc<dyn Fn(RpcSignal) + Send + Sync>;

/// Abstract I/O, signal, and exit boundary for RPC mode (real stdio or in-memory test harness).
#[async_trait]
pub trait RpcIo: Send + Sync + 'static {
    fn take_over_stdout(&self);
    fn write_line(&self, line: &str);
    fn wait_for_stdout_backpressure(&self) -> BoxFuture<()>;
    fn flush_stdout(&self) -> BoxFuture<pi_js::Result<()>>;
    fn kill_tracked_detached_children(&self);
    fn attach_input(&self, on_line: RpcLineCallback, on_end: RpcEndCallback) -> Unsubscribe;
    fn subscribe_signals(&self, on_signal: RpcSignalCallback) -> Unsubscribe;
    fn on_exit(&self, exit_code: i32);
}

/// Production [`RpcIo`] backed by process stdin/stdout and OS signals.
#[derive(Clone, Default)]
pub struct StdioRpcIo;

#[async_trait]
impl RpcIo for StdioRpcIo {
    fn take_over_stdout(&self) {
        crate::core::output_guard::take_over_stdout();
    }

    fn write_line(&self, line: &str) {
        crate::core::output_guard::write_raw_stdout(line);
    }

    fn wait_for_stdout_backpressure(&self) -> BoxFuture<()> {
        Box::pin(crate::core::output_guard::wait_for_raw_stdout_backpressure())
    }

    fn flush_stdout(&self) -> BoxFuture<pi_js::Result<()>> {
        Box::pin(crate::core::output_guard::flush_raw_stdout())
    }

    fn kill_tracked_detached_children(&self) {
        crate::utils::shell::kill_tracked_detached_children();
    }

    fn attach_input(&self, on_line: RpcLineCallback, on_end: RpcEndCallback) -> Unsubscribe {
        let handle = attach_jsonl_line_reader(tokio::io::stdin(), on_line, Some(on_end));
        Unsubscribe::new(move || handle.detach())
    }

    fn subscribe_signals(&self, on_signal: RpcSignalCallback) -> Unsubscribe {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};

            let listen_hup = pi_js::env::platform() != "win32";
            let jh = tokio::spawn(async move {
                let mut term_stream = signal(SignalKind::terminate()).ok();
                let mut hup_stream = if listen_hup {
                    signal(SignalKind::hangup()).ok()
                } else {
                    None
                };

                loop {
                    tokio::select! {
                        res = async {
                            match term_stream.as_mut() {
                                Some(s) => s.recv().await,
                                None => std::future::pending().await,
                            }
                        } => {
                            if res.is_none() {
                                break;
                            }
                            on_signal(RpcSignal::SigTerm);
                        }
                        res = async {
                            match hup_stream.as_mut() {
                                Some(s) => s.recv().await,
                                None => std::future::pending().await,
                            }
                        } => {
                            if res.is_none() {
                                break;
                            }
                            on_signal(RpcSignal::SigHup);
                        }
                    }
                }
            });
            let abort_handle = jh.abort_handle();
            Unsubscribe::new(move || abort_handle.abort())
        }

        #[cfg(not(unix))]
        {
            let jh = tokio::spawn(async move {
                while tokio::signal::ctrl_c().await.is_ok() {
                    on_signal(RpcSignal::SigTerm);
                }
            });
            let abort_handle = jh.abort_handle();
            Unsubscribe::new(move || abort_handle.abort())
        }
    }

    fn on_exit(&self, _exit_code: i32) {}
}

// ============================================================================
// Internal RPC Mode Controller
// ============================================================================

struct RpcMutableState {
    session: Arc<dyn RpcSession>,
    unsubscribe: Option<Unsubscribe>,
    unsubscribe_backpressure: Option<Unsubscribe>,
    detach_input: Option<Unsubscribe>,
    pending_extension_requests: IndexMap<String, oneshot::Sender<RpcExtensionUIResponse>>,
    shutdown_requested: bool,
    shutting_down: bool,
    input_ready: bool,
    active_commands: usize,
    signal_count: usize,
    exit_sent: bool,
    exit_tx: Option<oneshot::Sender<i32>>,
}

struct RpcController {
    runtime_host: Arc<dyn RpcSessionRuntime>,
    io: Arc<dyn RpcIo>,
    state: Mutex<RpcMutableState>,
}

impl RpcController {
    fn output<T: Serialize>(&self, value: &T) {
        let line = serialize_json_line_value(value).unwrap_or_else(|_| format!("{}\n", pi_js::json::stringify(value)));
        self.io.write_line(&line);
    }

    fn emit_exit(&self, exit_code: i32) {
        let tx = {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.exit_sent {
                return;
            }
            guard.exit_sent = true;
            guard.exit_tx.take()
        };
        self.io.on_exit(exit_code);
        if let Some(tx) = tx {
            let _ = tx.send(exit_code);
        }
    }

    fn trigger_shutdown(self: &Arc<Self>, exit_code: i32, signal: Option<RpcSignal>) {
        let (unsub, unsub_bp) = {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.shutting_down {
                return;
            }
            guard.shutting_down = true;
            (guard.unsubscribe.take(), guard.unsubscribe_backpressure.take())
        };
        if let Some(u) = unsub {
            u.call();
        }
        if let Some(u) = unsub_bp {
            u.call();
        }

        let ctrl = Arc::clone(self);
        tokio::spawn(async move {
            ctrl.runtime_host.dispose().await;
            let detach_input = {
                let mut guard = ctrl.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.pending_extension_requests.clear();
                guard.detach_input.take()
            };
            if let Some(u) = detach_input {
                u.call();
            }
            if signal != Some(RpcSignal::SigTerm) {
                let _ = ctrl.io.flush_stdout().await;
            }
            ctrl.emit_exit(exit_code);
        });
    }

    fn check_shutdown_requested(self: &Arc<Self>) {
        let should_shutdown = {
            let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.shutdown_requested
                && !guard.shutting_down
                && guard.input_ready
                && guard.active_commands == 0
                && guard.session.is_idle()
        };
        if should_shutdown {
            self.trigger_shutdown(0, None);
        }
    }

    async fn create_dialog_request<T, F>(
        &self,
        opts: Option<RpcExtensionDialogOptions>,
        default_value: T,
        build_request: impl FnOnce(String) -> RpcExtensionUIRequest,
        parse_response: F,
    ) -> T
    where
        F: FnOnce(RpcExtensionUIResponse) -> T,
    {
        if opts
            .as_ref()
            .and_then(|o| o.signal.as_ref())
            .is_some_and(AbortSignal::aborted)
        {
            return default_value;
        }

        let id = pi_js::crypto::random_uuid();
        let (tx, rx) = oneshot::channel::<RpcExtensionUIResponse>();
        {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.pending_extension_requests.insert(id.clone(), tx);
        }
        let req = build_request(id.clone());
        self.output(&req);

        let timeout_ms = opts.as_ref().and_then(|o| o.timeout).filter(|&ms| ms > 0);
        let signal = opts.and_then(|o| o.signal);

        let result = tokio::select! {
            res = rx => match res {
                Ok(resp) => parse_response(resp),
                Err(_) => default_value,
            },
            _ = async {
                match signal.as_ref() {
                    Some(sig) => sig.cancelled().await,
                    None => std::future::pending().await,
                }
            } => default_value,
            _ = async {
                match timeout_ms {
                    Some(ms) => {
                        let _ = pi_js::time::sleep(ms, None).await;
                    }
                    None => std::future::pending().await,
                }
            } => default_value,
        };

        {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.pending_extension_requests.shift_remove(&id);
        }
        result
    }

    async fn rebind_session(self: &Arc<Self>) -> pi_js::Result<()> {
        let session = self.runtime_host.session();
        {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.session = Arc::clone(&session);
        }

        let ui_context: Arc<dyn RpcExtensionUiContext> = Arc::new(RpcExtensionUi { ctrl: Arc::clone(self) });

        let runtime_for_new = Arc::clone(&self.runtime_host);
        let runtime_for_fork = Arc::clone(&self.runtime_host);
        let runtime_for_switch = Arc::clone(&self.runtime_host);
        let session_for_idle = Arc::clone(&session);
        let session_for_nav = Arc::clone(&session);
        let session_for_reload = Arc::clone(&session);

        let ctrl_for_shutdown = Arc::clone(self);
        let ctrl_for_error = Arc::clone(self);

        session
            .bind_extensions(RpcExtensionBindings {
                ui_context,
                mode: "rpc",
                command_context_actions: RpcCommandContextActions {
                    wait_for_idle: Arc::new(move || {
                        let s = Arc::clone(&session_for_idle);
                        Box::pin(async move {
                            s.wait_for_idle().await;
                            Ok(())
                        })
                    }),
                    new_session: Arc::new(move |options| {
                        let rt = Arc::clone(&runtime_for_new);
                        Box::pin(async move { rt.new_session(options).await })
                    }),
                    fork: Arc::new(move |entry_id, options| {
                        let rt = Arc::clone(&runtime_for_fork);
                        Box::pin(async move {
                            let result = rt.fork(&entry_id, options).await?;
                            Ok(CancelledResult {
                                cancelled: result.cancelled,
                            })
                        })
                    }),
                    navigate_tree: Arc::new(move |target_id, options| {
                        let s = Arc::clone(&session_for_nav);
                        Box::pin(async move { s.navigate_tree(&target_id, options).await })
                    }),
                    switch_session: Arc::new(move |session_path, options| {
                        let rt = Arc::clone(&runtime_for_switch);
                        Box::pin(async move { rt.switch_session(&session_path, options).await })
                    }),
                    reload: Arc::new(move || {
                        let s = Arc::clone(&session_for_reload);
                        Box::pin(async move { s.reload().await })
                    }),
                },
                shutdown_handler: Arc::new(move || {
                    {
                        let mut guard = ctrl_for_shutdown.state.lock().unwrap_or_else(|e| e.into_inner());
                        guard.shutdown_requested = true;
                    }
                    // An idle session has no command response or agent_settled left to check the flag.
                    // Check on the next tick so the requesting handler finishes first.
                    let ctrl = Arc::clone(&ctrl_for_shutdown);
                    tokio::spawn(async move {
                        tokio::task::yield_now().await;
                        ctrl.check_shutdown_requested();
                    });
                }),
                on_error: Arc::new(move |err| {
                    ctrl_for_error.output(&RpcExtensionErrorEvent::new(err.extension_path, err.event, err.error));
                }),
            })
            .await?;

        let (prev_unsub, prev_bp) = {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (guard.unsubscribe.take(), guard.unsubscribe_backpressure.take())
        };
        if let Some(u) = prev_unsub {
            u.call();
        }
        if let Some(u) = prev_bp {
            u.call();
        }

        let ctrl_for_events = Arc::clone(self);
        let unsub = session.subscribe(Arc::new(move |event| {
            let is_settled = matches!(event, AgentSessionEvent::AgentSettled);
            if let Ok(json_event) = to_json_event(&event) {
                ctrl_for_events.output(&json_event);
            }
            if is_settled {
                ctrl_for_events.check_shutdown_requested();
            }
        }));

        let io_for_bp = Arc::clone(&self.io);
        let unsub_bp = session.subscribe_agent_backpressure(Arc::new(move || {
            let io = Arc::clone(&io_for_bp);
            Box::pin(async move {
                io.wait_for_stdout_backpressure().await;
            })
        }));

        {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.unsubscribe = Some(unsub);
            guard.unsubscribe_backpressure = Some(unsub_bp);
        }
        Ok(())
    }

    async fn handle_command(self: &Arc<Self>, command: RpcCommand) -> pi_js::Result<Option<RpcResponse>> {
        let id = command.id().map(ToOwned::to_owned);
        let session = {
            let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            Arc::clone(&guard.session)
        };

        match command {
            // =================================================================
            // Prompting
            // =================================================================
            RpcCommand::Prompt {
                message,
                images,
                streaming_behavior,
                ..
            } => {
                // Start prompt handling immediately, but emit the authoritative response only after
                // prompt preflight succeeds. Queued and immediately handled prompts also count as success.
                let preflight_succeeded = Arc::new(AtomicBool::new(false));
                // Extension commands run inside prompt(); defer shutdown requests until it settles.
                {
                    let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    guard.active_commands += 1;
                }

                let preflight_flag = Arc::clone(&preflight_succeeded);
                let ctrl_preflight = Arc::clone(self);
                let id_preflight = id.clone();

                let ctrl_done = Arc::clone(self);
                let id_done = id;

                tokio::spawn(async move {
                    let result = session
                        .prompt(
                            &message,
                            RpcPromptOptions {
                                images,
                                streaming_behavior,
                                source: "rpc",
                                preflight_result: Arc::new(move |disposition| {
                                    preflight_flag.store(true, Ordering::SeqCst);
                                    if let Ok(resp) = RpcResponse::success(
                                        id_preflight.clone(),
                                        "prompt",
                                        &PromptResponseData { disposition },
                                    ) {
                                        ctrl_preflight.output(&resp);
                                    }
                                }),
                            },
                        )
                        .await;

                    if let Err(err) = result
                        && !preflight_succeeded.load(Ordering::SeqCst)
                    {
                        ctrl_done.output(&RpcResponse::error(id_done, "prompt", err.to_string()));
                    }

                    {
                        let mut guard = ctrl_done.state.lock().unwrap_or_else(|e| e.into_inner());
                        guard.active_commands = guard.active_commands.saturating_sub(1);
                    }
                    ctrl_done.check_shutdown_requested();
                });

                Ok(None)
            }

            RpcCommand::Steer { message, images, .. } => {
                let disposition = session.steer(&message, images).await?;
                Ok(Some(RpcResponse::success(
                    id,
                    "steer",
                    &QueuedInputResponseData { disposition },
                )?))
            }

            RpcCommand::FollowUp { message, images, .. } => {
                let disposition = session.follow_up(&message, images).await?;
                Ok(Some(RpcResponse::success(
                    id,
                    "follow_up",
                    &QueuedInputResponseData { disposition },
                )?))
            }

            RpcCommand::Abort { .. } => {
                session.abort().await;
                Ok(Some(RpcResponse::success_empty(id, "abort")))
            }

            RpcCommand::ClearQueue { .. } => {
                let cleared = session.clear_queue();
                Ok(Some(RpcResponse::success(id, "clear_queue", &cleared)?))
            }

            RpcCommand::NewSession { parent_session, .. } => {
                let options = parent_session.map(|p| RpcNewSessionOptions {
                    parent_session: Some(p),
                    opaque: None,
                });
                let result = self.runtime_host.new_session(options).await?;
                if !result.cancelled {
                    self.rebind_session().await?;
                }
                Ok(Some(RpcResponse::success(id, "new_session", &result)?))
            }

            // =================================================================
            // State
            // =================================================================
            RpcCommand::GetState { .. } => {
                let state = RpcSessionState {
                    model: session.model(),
                    thinking_level: session.thinking_level(),
                    is_streaming: session.is_streaming(),
                    is_compacting: session.is_compacting(),
                    steering_mode: session.steering_mode(),
                    follow_up_mode: session.follow_up_mode(),
                    session_file: session.session_file(),
                    session_id: session.session_id(),
                    session_name: session.session_name(),
                    auto_compaction_enabled: session.auto_compaction_enabled(),
                    message_count: session.messages().len() as i64,
                    pending_message_count: session.pending_message_count() as i64,
                };
                Ok(Some(RpcResponse::success(id, "get_state", &state)?))
            }

            // =================================================================
            // Model
            // =================================================================
            RpcCommand::SetModel { provider, model_id, .. } => {
                let models = session.get_available_models();
                let Some(model) = models.into_iter().find(|m| m.provider == provider && m.id == model_id) else {
                    return Ok(Some(RpcResponse::error(
                        id,
                        "set_model",
                        format!("Model not found: {provider}/{model_id}"),
                    )));
                };
                session.set_model(model.clone()).await?;
                Ok(Some(RpcResponse::success(id, "set_model", &model)?))
            }

            RpcCommand::CycleModel { .. } => {
                let result = session.cycle_model().await?;
                match result {
                    None => Ok(Some(RpcResponse::success_value(id, "cycle_model", Value::Null))),
                    Some(res) => Ok(Some(RpcResponse::success(id, "cycle_model", &res)?)),
                }
            }

            RpcCommand::GetAvailableModels { .. } => {
                let models = session.get_available_models();
                Ok(Some(RpcResponse::success(
                    id,
                    "get_available_models",
                    &AvailableModelsResponseData { models },
                )?))
            }

            // =================================================================
            // Thinking
            // =================================================================
            RpcCommand::SetThinkingLevel { level, .. } => {
                session.set_thinking_level(level);
                Ok(Some(RpcResponse::success_empty(id, "set_thinking_level")))
            }

            RpcCommand::CycleThinkingLevel { .. } => {
                let level = session.cycle_thinking_level();
                match level {
                    None => Ok(Some(RpcResponse::success_value(
                        id,
                        "cycle_thinking_level",
                        Value::Null,
                    ))),
                    Some(level) => Ok(Some(RpcResponse::success(
                        id,
                        "cycle_thinking_level",
                        &CycleThinkingLevelResult { level },
                    )?)),
                }
            }

            RpcCommand::GetAvailableThinkingLevels { .. } => {
                let levels = session.get_available_thinking_levels();
                Ok(Some(RpcResponse::success(
                    id,
                    "get_available_thinking_levels",
                    &AvailableThinkingLevelsResponseData { levels },
                )?))
            }

            // =================================================================
            // Queue Modes
            // =================================================================
            RpcCommand::SetSteeringMode { mode, .. } => {
                session.set_steering_mode(mode);
                Ok(Some(RpcResponse::success_empty(id, "set_steering_mode")))
            }

            RpcCommand::SetFollowUpMode { mode, .. } => {
                session.set_follow_up_mode(mode);
                Ok(Some(RpcResponse::success_empty(id, "set_follow_up_mode")))
            }

            // =================================================================
            // Compaction
            // =================================================================
            RpcCommand::Compact {
                custom_instructions, ..
            } => {
                let result = session.compact(custom_instructions.as_deref()).await?;
                Ok(Some(RpcResponse::success(id, "compact", &result)?))
            }

            RpcCommand::SetAutoCompaction { enabled, .. } => {
                session.set_auto_compaction_enabled(enabled);
                Ok(Some(RpcResponse::success_empty(id, "set_auto_compaction")))
            }

            // =================================================================
            // Retry
            // =================================================================
            RpcCommand::SetAutoRetry { enabled, .. } => {
                session.set_auto_retry_enabled(enabled);
                Ok(Some(RpcResponse::success_empty(id, "set_auto_retry")))
            }

            RpcCommand::AbortRetry { .. } => {
                session.abort_retry();
                Ok(Some(RpcResponse::success_empty(id, "abort_retry")))
            }

            // =================================================================
            // Bash
            // =================================================================
            RpcCommand::Bash {
                command,
                exclude_from_context,
                ..
            } => {
                let cwd = session.cwd();
                let event_result = session
                    .emit_user_bash(&command, exclude_from_context.unwrap_or(false), &cwd)
                    .await?;

                let operations = match event_result {
                    Some(RpcUserBashOutcome::Result(res)) => {
                        session.record_bash_result(&command, &res, exclude_from_context);
                        return Ok(Some(RpcResponse::success(id, "bash", &res)?));
                    }
                    Some(RpcUserBashOutcome::Operations(ops)) => Some(ops),
                    None => None,
                };

                let result = session
                    .execute_bash(
                        &command,
                        RpcExecuteBashOptions {
                            exclude_from_context,
                            id: id.clone(),
                            operations,
                        },
                    )
                    .await?;
                Ok(Some(RpcResponse::success(id, "bash", &result)?))
            }

            RpcCommand::AbortBash { .. } => {
                session.abort_bash();
                Ok(Some(RpcResponse::success_empty(id, "abort_bash")))
            }

            // =================================================================
            // Session
            // =================================================================
            RpcCommand::GetSessionStats { .. } => {
                let stats = session.get_session_stats();
                Ok(Some(RpcResponse::success(id, "get_session_stats", &stats)?))
            }

            RpcCommand::ExportHtml { output_path, .. } => {
                let path = session.export_to_html(output_path.as_deref()).await?;
                Ok(Some(RpcResponse::success(
                    id,
                    "export_html",
                    &ExportHtmlResult { path },
                )?))
            }

            RpcCommand::SwitchSession { session_path, .. } => {
                let result = self.runtime_host.switch_session(&session_path, None).await?;
                if !result.cancelled {
                    self.rebind_session().await?;
                }
                Ok(Some(RpcResponse::success(id, "switch_session", &result)?))
            }

            RpcCommand::Fork { entry_id, .. } => {
                let result = self.runtime_host.fork(&entry_id, None).await?;
                if !result.cancelled {
                    self.rebind_session().await?;
                }
                Ok(Some(RpcResponse::success(
                    id,
                    "fork",
                    &ForkResult {
                        text: result.selected_text,
                        cancelled: result.cancelled,
                    },
                )?))
            }

            RpcCommand::Clone { .. } => {
                let Some(leaf_id) = session.leaf_id() else {
                    return Ok(Some(RpcResponse::error(
                        id,
                        "clone",
                        "Cannot clone session: no current entry selected",
                    )));
                };
                let result = self
                    .runtime_host
                    .fork(
                        &leaf_id,
                        Some(RpcForkOptions {
                            position: Some(RpcForkPosition::At),
                            opaque: None,
                        }),
                    )
                    .await?;
                if !result.cancelled {
                    self.rebind_session().await?;
                }
                Ok(Some(RpcResponse::success(
                    id,
                    "clone",
                    &CancelledResult {
                        cancelled: result.cancelled,
                    },
                )?))
            }

            RpcCommand::GetForkMessages { .. } => {
                let messages = session.get_user_messages_for_forking();
                Ok(Some(RpcResponse::success(
                    id,
                    "get_fork_messages",
                    &ForkMessagesResponseData { messages },
                )?))
            }

            RpcCommand::GetEntries { since, .. } => {
                let mut entries = session.get_entries();
                if let Some(since_id) = since {
                    let Some(since_idx) = entries
                        .iter()
                        .position(|e| session_entry_id(e) == Some(since_id.as_str()))
                    else {
                        return Ok(Some(RpcResponse::error(
                            id,
                            "get_entries",
                            format!("Entry not found: {since_id}"),
                        )));
                    };
                    entries = entries.split_off(since_idx + 1);
                }
                Ok(Some(RpcResponse::success(
                    id,
                    "get_entries",
                    &GetEntriesResult {
                        entries,
                        leaf_id: session.leaf_id(),
                    },
                )?))
            }

            RpcCommand::GetTree { .. } => Ok(Some(RpcResponse::success(
                id,
                "get_tree",
                &GetTreeResult {
                    tree: session.get_tree(),
                    leaf_id: session.leaf_id(),
                },
            )?)),

            RpcCommand::GetLastAssistantText { .. } => {
                let text = session.get_last_assistant_text();
                Ok(Some(RpcResponse::success(
                    id,
                    "get_last_assistant_text",
                    &LastAssistantTextResponseData { text },
                )?))
            }

            RpcCommand::SetSessionName { name, .. } => {
                let trimmed = pi_js::str16::trim(&name);
                if trimmed.is_empty() {
                    return Ok(Some(RpcResponse::error(
                        id,
                        "set_session_name",
                        "Session name cannot be empty",
                    )));
                }
                session.set_session_name(trimmed);
                Ok(Some(RpcResponse::success_empty(id, "set_session_name")))
            }

            // =================================================================
            // Messages
            // =================================================================
            RpcCommand::GetMessages { .. } => {
                let messages = session.messages();
                Ok(Some(RpcResponse::success(
                    id,
                    "get_messages",
                    &GetMessagesResponseData { messages },
                )?))
            }

            // =================================================================
            // Commands
            // =================================================================
            RpcCommand::GetCommands { .. } => {
                let commands = session.get_commands();
                Ok(Some(RpcResponse::success(
                    id,
                    "get_commands",
                    &GetCommandsResponseData { commands },
                )?))
            }
        }
    }

    fn handle_input_line(self: &Arc<Self>, line: &str) {
        let parsed = match pi_js::json::parse(line) {
            Ok(val) => val,
            Err(parse_err) => {
                self.output(&RpcResponse::error(
                    None,
                    "parse",
                    format!("Failed to parse command: {parse_err}"),
                ));
                let io = Arc::clone(&self.io);
                tokio::spawn(async move {
                    io.wait_for_stdout_backpressure().await;
                });
                return;
            }
        };

        if let Some(obj) = parsed.as_object()
            && obj.get("type").and_then(Value::as_str) == Some("extension_ui_response")
        {
            if let Ok(response) = serde_json::from_value::<RpcExtensionUIResponse>(parsed) {
                let pending = {
                    let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    guard.pending_extension_requests.shift_remove(response.id())
                };
                if let Some(tx) = pending {
                    let _ = tx.send(response);
                }
            }
            return;
        }

        let id = parsed.get("id").and_then(Value::as_str).map(ToOwned::to_owned);
        let cmd_type = parsed.get("type").and_then(Value::as_str).map(ToOwned::to_owned);

        {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.active_commands += 1;
        }

        let ctrl = Arc::clone(self);
        tokio::spawn(async move {
            let cmd_name = cmd_type.as_deref().unwrap_or("undefined");
            let response = if cmd_type.as_deref().and_then(RpcCommandType::from_type_str).is_none() {
                Some(RpcResponse::error(
                    id.clone(),
                    cmd_name,
                    format!("Unknown command: {cmd_name}"),
                ))
            } else {
                match serde_json::from_value::<RpcCommand>(parsed) {
                    Ok(command) => match ctrl.handle_command(command).await {
                        Ok(resp) => resp,
                        Err(err) => Some(RpcResponse::error(id, cmd_name, err.to_string())),
                    },
                    Err(err) => Some(RpcResponse::error(id, cmd_name, err.to_string())),
                }
            };

            if let Some(resp) = response {
                ctrl.output(&resp);
                ctrl.io.wait_for_stdout_backpressure().await;
            }

            {
                let mut guard = ctrl.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.active_commands = guard.active_commands.saturating_sub(1);
            }
            ctrl.check_shutdown_requested();
        });
    }
}

struct RpcExtensionUi {
    ctrl: Arc<RpcController>,
}

#[async_trait]
impl RpcExtensionUiContext for RpcExtensionUi {
    async fn select(
        &self,
        title: &str,
        options: Vec<String>,
        opts: Option<RpcExtensionDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        let title_owned = title.to_string();
        let timeout = opts.as_ref().and_then(|o| o.timeout);
        Ok(self
            .ctrl
            .create_dialog_request(
                opts,
                None,
                move |id| RpcExtensionUIRequest::Select {
                    id,
                    title: title_owned,
                    options,
                    timeout,
                },
                |r| match r {
                    RpcExtensionUIResponse::Value { value, .. } => Some(value),
                    RpcExtensionUIResponse::Cancelled { .. } | RpcExtensionUIResponse::Confirmed { .. } => None,
                },
            )
            .await)
    }

    async fn confirm(
        &self,
        title: &str,
        message: &str,
        opts: Option<RpcExtensionDialogOptions>,
    ) -> pi_js::Result<bool> {
        let title_owned = title.to_string();
        let message_owned = message.to_string();
        let timeout = opts.as_ref().and_then(|o| o.timeout);
        Ok(self
            .ctrl
            .create_dialog_request(
                opts,
                false,
                move |id| RpcExtensionUIRequest::Confirm {
                    id,
                    title: title_owned,
                    message: message_owned,
                    timeout,
                },
                |r| match r {
                    RpcExtensionUIResponse::Confirmed { confirmed, .. } => confirmed,
                    RpcExtensionUIResponse::Cancelled { .. } | RpcExtensionUIResponse::Value { .. } => false,
                },
            )
            .await)
    }

    async fn input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        opts: Option<RpcExtensionDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        let title_owned = title.to_string();
        let placeholder_owned = placeholder.map(ToOwned::to_owned);
        let timeout = opts.as_ref().and_then(|o| o.timeout);
        Ok(self
            .ctrl
            .create_dialog_request(
                opts,
                None,
                move |id| RpcExtensionUIRequest::Input {
                    id,
                    title: title_owned,
                    placeholder: placeholder_owned,
                    timeout,
                },
                |r| match r {
                    RpcExtensionUIResponse::Value { value, .. } => Some(value),
                    RpcExtensionUIResponse::Cancelled { .. } | RpcExtensionUIResponse::Confirmed { .. } => None,
                },
            )
            .await)
    }

    fn notify(&self, message: &str, notify_type: Option<NotifyType>) {
        self.ctrl.output(&RpcExtensionUIRequest::Notify {
            id: pi_js::crypto::random_uuid(),
            message: message.to_string(),
            notify_type,
        });
    }

    fn on_terminal_input(&self) -> Unsubscribe {
        Unsubscribe::noop()
    }

    fn set_status(&self, key: &str, text: Option<&str>) {
        self.ctrl.output(&RpcExtensionUIRequest::SetStatus {
            id: pi_js::crypto::random_uuid(),
            status_key: key.to_string(),
            status_text: text.map(ToOwned::to_owned),
        });
    }

    fn set_working_message(&self, _message: Option<&str>) {}

    fn set_working_visible(&self, _visible: bool) {}

    fn set_hidden_thinking_label(&self, _label: Option<&str>) {}

    fn set_widget_lines(&self, key: &str, content: Option<Vec<String>>, options: Option<RpcExtensionWidgetOptions>) {
        self.ctrl.output(&RpcExtensionUIRequest::SetWidget {
            id: pi_js::crypto::random_uuid(),
            widget_key: key.to_string(),
            widget_lines: content,
            widget_placement: options.and_then(|o| o.placement),
        });
    }

    fn set_widget_factory_unsupported(&self) {}

    fn set_footer_unsupported(&self) {}

    fn set_header_unsupported(&self) {}

    fn set_title(&self, title: &str) {
        self.ctrl.output(&RpcExtensionUIRequest::SetTitle {
            id: pi_js::crypto::random_uuid(),
            title: title.to_string(),
        });
    }

    async fn custom_unsupported(&self) -> pi_js::Result<Value> {
        Ok(Value::Null)
    }

    fn paste_to_editor(&self, text: &str) {
        self.set_editor_text(text);
    }

    fn set_editor_text(&self, text: &str) {
        self.ctrl.output(&RpcExtensionUIRequest::SetEditorText {
            id: pi_js::crypto::random_uuid(),
            text: text.to_string(),
        });
    }

    fn get_editor_text(&self) -> String {
        String::new()
    }

    async fn editor(&self, title: &str, prefill: Option<&str>) -> pi_js::Result<Option<String>> {
        let title_owned = title.to_string();
        let prefill_owned = prefill.map(ToOwned::to_owned);
        Ok(self
            .ctrl
            .create_dialog_request(
                None,
                None,
                move |id| RpcExtensionUIRequest::Editor {
                    id,
                    title: title_owned,
                    prefill: prefill_owned,
                },
                |r| match r {
                    RpcExtensionUIResponse::Value { value, .. } => Some(value),
                    RpcExtensionUIResponse::Cancelled { .. } | RpcExtensionUIResponse::Confirmed { .. } => None,
                },
            )
            .await)
    }

    fn theme(&self) -> Theme {
        theme()
    }

    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        Vec::new()
    }

    fn get_theme(&self, _name: &str) -> Option<Theme> {
        None
    }

    fn set_theme(&self, _name: &str) -> SetThemeResult {
        SetThemeResult {
            success: false,
            error: Some("Theme switching not supported in RPC mode".to_string()),
        }
    }

    fn get_tools_expanded(&self) -> bool {
        false
    }

    fn set_tools_expanded(&self, _expanded: bool) {}
}

/// Run in RPC mode with a custom [`RpcIo`] transport (for in-memory testing and embeddings).
pub async fn run_rpc_mode_with_io(runtime_host: Arc<dyn RpcSessionRuntime>, io: Arc<dyn RpcIo>) -> pi_js::Result<()> {
    io.take_over_stdout();

    let (exit_tx, exit_rx) = oneshot::channel::<i32>();
    let initial_session = runtime_host.session();
    let ctrl = Arc::new(RpcController {
        runtime_host: Arc::clone(&runtime_host),
        io: Arc::clone(&io),
        state: Mutex::new(RpcMutableState {
            session: initial_session,
            unsubscribe: None,
            unsubscribe_backpressure: None,
            detach_input: None,
            pending_extension_requests: IndexMap::new(),
            shutdown_requested: false,
            shutting_down: false,
            input_ready: false,
            active_commands: 0,
            signal_count: 0,
            exit_sent: false,
            exit_tx: Some(exit_tx),
        }),
    });

    let ctrl_rebind = Arc::clone(&ctrl);
    runtime_host.set_rebind_session(Some(Arc::new(move || {
        let c = Arc::clone(&ctrl_rebind);
        Box::pin(async move { c.rebind_session().await })
    })));

    ctrl.rebind_session().await?;

    let ctrl_signal = Arc::clone(&ctrl);
    let signal_sub = io.subscribe_signals(Arc::new(move |sig| {
        ctrl_signal.io.kill_tracked_detached_children();
        let (signal_count, shutting_down) = {
            let mut guard = ctrl_signal.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.signal_count += 1;
            (guard.signal_count, guard.shutting_down)
        };
        if !shutting_down {
            ctrl_signal.trigger_shutdown(sig.exit_code(), Some(sig));
        } else if signal_count > 1 {
            // Process managers often close stdin and send SIGTERM together, so one signal
            // during shutdown is not a request to skip cleanup; a repeated one is.
            ctrl_signal.emit_exit(sig.exit_code());
        }
    }));

    let ctrl_line = Arc::clone(&ctrl);
    let ctrl_end = Arc::clone(&ctrl);
    let detach_input = io.attach_input(
        Arc::new(move |line| {
            ctrl_line.handle_input_line(line);
        }),
        Arc::new(move || {
            ctrl_end.trigger_shutdown(0, None);
        }),
    );

    {
        let mut guard = ctrl.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.detach_input = Some(detach_input);
        guard.input_ready = true;
    }

    // Handle a shutdown requested during startup (e.g. from session_start).
    ctrl.check_shutdown_requested();

    let exit_code = exit_rx.await.unwrap_or(0);
    signal_sub.call();

    let (unsub, unsub_bp, detach) = {
        let mut guard = ctrl.state.lock().unwrap_or_else(|e| e.into_inner());
        (
            guard.unsubscribe.take(),
            guard.unsubscribe_backpressure.take(),
            guard.detach_input.take(),
        )
    };
    if let Some(u) = unsub {
        u.call();
    }
    if let Some(u) = unsub_bp {
        u.call();
    }
    if let Some(u) = detach {
        u.call();
    }

    Err(pi_js::Error::Exit(exit_code))
}

/// Run in RPC mode on stdin/stdout.
///
/// Listens for JSON commands on stdin, outputs events and responses on stdout.
pub async fn run_rpc_mode(runtime_host: Arc<dyn RpcSessionRuntime>) -> pi_js::Result<()> {
    run_rpc_mode_with_io(runtime_host, Arc::new(StdioRpcIo)).await
}

struct RpcUiContextAdapter(Arc<dyn RpcExtensionUiContext>);

#[async_trait]
impl crate::core::extensions::types::ExtensionUIContext for RpcUiContextAdapter {
    async fn select(
        &self,
        title: &str,
        options: Vec<String>,
        opts: Option<crate::core::extensions::types::ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        self.0.select(title, options, opts).await
    }

    async fn confirm(
        &self,
        title: &str,
        message: &str,
        opts: Option<crate::core::extensions::types::ExtensionUIDialogOptions>,
    ) -> pi_js::Result<bool> {
        self.0.confirm(title, message, opts).await
    }

    async fn input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        opts: Option<crate::core::extensions::types::ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        self.0.input(title, placeholder, opts).await
    }

    fn notify(&self, message: &str, notify_type: Option<NotifyType>) {
        self.0.notify(message, notify_type);
    }

    fn on_terminal_input(&self, _handler: crate::core::extensions::types::TerminalInputHandler) -> Unsubscribe {
        self.0.on_terminal_input()
    }

    fn set_status(&self, key: &str, text: Option<&str>) {
        self.0.set_status(key, text);
    }

    fn set_working_message(&self, message: Option<&str>) {
        self.0.set_working_message(message);
    }

    fn set_working_visible(&self, visible: bool) {
        self.0.set_working_visible(visible);
    }

    fn set_working_indicator(&self, _options: Option<crate::core::extensions::types::WorkingIndicatorOptions>) {}

    fn set_hidden_thinking_label(&self, label: Option<&str>) {
        self.0.set_hidden_thinking_label(label);
    }

    fn set_widget_lines(&self, key: &str, content: Option<Vec<String>>, options: Option<RpcExtensionWidgetOptions>) {
        self.0.set_widget_lines(key, content, options);
    }

    fn set_widget_factory(
        &self,
        _key: &str,
        _factory: Option<crate::core::extensions::types::WidgetFactory>,
        _options: Option<RpcExtensionWidgetOptions>,
    ) {
        self.0.set_widget_factory_unsupported();
    }

    fn set_footer(&self, _factory: Option<crate::core::extensions::types::FooterFactory>) {
        self.0.set_footer_unsupported();
    }

    fn set_header(&self, _factory: Option<crate::core::extensions::types::HeaderFactory>) {
        self.0.set_header_unsupported();
    }

    fn set_title(&self, title: &str) {
        self.0.set_title(title);
    }

    async fn custom(
        &self,
        _factory: crate::core::extensions::types::CustomUiFactory,
        _options: Option<crate::core::extensions::types::ExtensionCustomOptions>,
    ) -> pi_js::Result<Value> {
        self.0.custom_unsupported().await
    }

    fn paste_to_editor(&self, text: &str) {
        self.0.paste_to_editor(text);
    }

    fn set_editor_text(&self, text: &str) {
        self.0.set_editor_text(text);
    }

    fn get_editor_text(&self) -> String {
        self.0.get_editor_text()
    }

    async fn editor(&self, title: &str, prefill: Option<&str>) -> pi_js::Result<Option<String>> {
        self.0.editor(title, prefill).await
    }

    fn add_autocomplete_provider(&self, _factory: crate::core::extensions::types::AutocompleteProviderFactory) {}

    fn set_editor_component(&self, _factory: Option<crate::core::extensions::types::EditorFactory>) {}

    fn get_editor_component(&self) -> Option<crate::core::extensions::types::EditorFactory> {
        None
    }

    fn theme(&self) -> crate::core::extensions::types::Theme {
        crate::core::extensions::types::Theme::new(Arc::new(self.0.theme()))
    }

    fn get_all_themes(&self) -> Vec<crate::core::extensions::types::ThemeInfo> {
        self.0
            .get_all_themes()
            .into_iter()
            .map(|info| crate::core::extensions::types::ThemeInfo {
                name: info.name,
                path: info.path,
            })
            .collect()
    }

    fn get_theme(&self, name: &str) -> Option<crate::core::extensions::types::Theme> {
        self.0
            .get_theme(name)
            .map(|t| crate::core::extensions::types::Theme::new(Arc::new(t)))
    }

    fn set_theme_by_name(&self, name: &str) -> crate::core::extensions::types::SetThemeResult {
        let res = self.0.set_theme(name);
        crate::core::extensions::types::SetThemeResult {
            success: res.success,
            error: res.error,
        }
    }

    fn set_theme(
        &self,
        _theme: crate::core::extensions::types::Theme,
    ) -> crate::core::extensions::types::SetThemeResult {
        crate::core::extensions::types::SetThemeResult {
            success: false,
            error: Some("Theme switching not supported in RPC mode".to_string()),
        }
    }

    fn get_tools_expanded(&self) -> bool {
        self.0.get_tools_expanded()
    }

    fn set_tools_expanded(&self, expanded: bool) {
        self.0.set_tools_expanded(expanded);
    }
}

#[async_trait]
impl RpcSession for crate::core::agent_session::AgentSession {
    async fn bind_extensions(&self, bindings: RpcExtensionBindings) -> pi_js::Result<()> {
        let ext_mode = match bindings.mode {
            "tui" => crate::core::extensions::types::ExtensionMode::Tui,
            "json" => crate::core::extensions::types::ExtensionMode::Json,
            "print" => crate::core::extensions::types::ExtensionMode::Print,
            _ => crate::core::extensions::types::ExtensionMode::Rpc,
        };
        let cmd_actions = bindings.command_context_actions;
        let actions = crate::core::extensions::types::ExtensionCommandContextActions {
            wait_for_idle: cmd_actions.wait_for_idle,
            new_session: {
                let f = cmd_actions.new_session;
                Arc::new(move |opts| {
                    let mapped = opts.map(|o| RpcNewSessionOptions {
                        parent_session: o.parent_session,
                        opaque: None,
                    });
                    let f = Arc::clone(&f);
                    Box::pin(async move {
                        let r = f(mapped).await?;
                        Ok(crate::core::extensions::types::CancelledResult { cancelled: r.cancelled })
                    })
                })
            },
            fork: {
                let f = cmd_actions.fork;
                Arc::new(move |entry_id, opts| {
                    let mapped = opts.map(|o| RpcForkOptions {
                        position: o.position,
                        opaque: None,
                    });
                    let entry_id = entry_id.to_owned();
                    let f = Arc::clone(&f);
                    Box::pin(async move {
                        let r = f(entry_id, mapped).await?;
                        Ok(crate::core::extensions::types::CancelledResult { cancelled: r.cancelled })
                    })
                })
            },
            navigate_tree: {
                let f = cmd_actions.navigate_tree;
                Arc::new(move |target_id, opts| {
                    let mapped = opts.map(|o| RpcNavigateTreeOptions {
                        summarize: o.summarize,
                        custom_instructions: o.custom_instructions,
                        replace_instructions: o.replace_instructions,
                        label: o.label,
                    });
                    let target_id = target_id.to_owned();
                    let f = Arc::clone(&f);
                    Box::pin(async move {
                        let r = f(target_id, mapped).await?;
                        Ok(crate::core::extensions::types::CancelledResult { cancelled: r.cancelled })
                    })
                })
            },
            switch_session: {
                let f = cmd_actions.switch_session;
                Arc::new(move |session_path, opts| {
                    let mapped = opts.map(|_| RpcSwitchSessionOptions::default());
                    let session_path = session_path.to_owned();
                    let f = Arc::clone(&f);
                    Box::pin(async move {
                        let r = f(session_path, mapped).await?;
                        Ok(crate::core::extensions::types::CancelledResult { cancelled: r.cancelled })
                    })
                })
            },
            reload: cmd_actions.reload,
        };
        let shutdown_handler = bindings.shutdown_handler;
        self.bind_extensions(crate::core::agent_session::ExtensionBindings {
            ui_context: Some(Arc::new(RpcUiContextAdapter(bindings.ui_context))),
            mode: Some(ext_mode),
            command_context_actions: Some(actions),
            abort_handler: None,
            shutdown_handler: Some(Arc::new(move || shutdown_handler())),
            on_error: Some(bindings.on_error),
        })
        .await
    }

    fn subscribe(&self, listener: Arc<dyn Fn(AgentSessionEvent) + Send + Sync>) -> Unsubscribe {
        self.subscribe(move |ev| listener(ev))
    }

    fn subscribe_agent_backpressure(&self, listener: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>) -> Unsubscribe {
        self.agent().subscribe_async(move |_ev, _sig| listener())
    }

    async fn prompt(&self, message: &str, options: RpcPromptOptions) -> pi_js::Result<()> {
        let source = match options.source {
            "interactive" => crate::core::extensions::types::InputSource::Interactive,
            "extension" => crate::core::extensions::types::InputSource::Extension,
            _ => crate::core::extensions::types::InputSource::Rpc,
        };
        let preflight_cb = options.preflight_result;
        self.prompt(
            message,
            Some(crate::core::agent_session::PromptOptions {
                expand_prompt_templates: None,
                streaming_behavior: options.streaming_behavior.map(Into::into),
                images: options.images,
                source: Some(source),
                preflight_result: Some(Arc::new(move |outcome| preflight_cb(outcome.into()))),
            }),
        )
        .await
    }

    async fn steer(&self, message: &str, images: Option<Vec<ImageContent>>) -> pi_js::Result<QueuedInputDisposition> {
        self.steer(
            message,
            images,
            Some(crate::core::agent_session::QueueInputOptions {
                source: Some(crate::core::extensions::types::InputSource::Rpc),
            }),
        )
        .await
        .map(Into::into)
    }

    async fn follow_up(
        &self,
        message: &str,
        images: Option<Vec<ImageContent>>,
    ) -> pi_js::Result<QueuedInputDisposition> {
        self.follow_up(
            message,
            images,
            Some(crate::core::agent_session::QueueInputOptions {
                source: Some(crate::core::extensions::types::InputSource::Rpc),
            }),
        )
        .await
        .map(Into::into)
    }

    async fn abort(&self) {
        self.abort().await;
    }

    fn clear_queue(&self) -> ClearQueueResult {
        let res = self.clear_queue();
        ClearQueueResult {
            steering: res.steering,
            follow_up: res.follow_up,
        }
    }

    fn is_idle(&self) -> bool {
        self.is_idle()
    }

    async fn wait_for_idle(&self) {
        self.wait_for_idle().await;
    }

    fn model(&self) -> Option<Model> {
        self.model()
    }

    fn thinking_level(&self) -> ThinkingLevel {
        self.thinking_level()
    }

    fn is_streaming(&self) -> bool {
        self.is_streaming()
    }

    fn is_compacting(&self) -> bool {
        self.is_compacting()
    }

    fn steering_mode(&self) -> QueueMode {
        self.steering_mode()
    }

    fn follow_up_mode(&self) -> QueueMode {
        self.follow_up_mode()
    }

    fn session_file(&self) -> Option<String> {
        self.session_file()
    }

    fn session_id(&self) -> String {
        self.session_id()
    }

    fn session_name(&self) -> Option<String> {
        self.session_name()
    }

    fn auto_compaction_enabled(&self) -> bool {
        self.auto_compaction_enabled()
    }

    fn messages(&self) -> Vec<AgentMessage> {
        self.messages()
    }

    fn pending_message_count(&self) -> usize {
        self.pending_message_count()
    }

    fn get_available_models(&self) -> Vec<Model> {
        self.model_runtime().get_available_snapshot()
    }

    async fn set_model(&self, model: Model) -> pi_js::Result<()> {
        self.set_model(model, None).await
    }

    async fn cycle_model(&self) -> pi_js::Result<Option<CycleModelResponseData>> {
        self.cycle_model(None, None).await.map(|opt| {
            opt.map(|r| CycleModelResponseData {
                model: r.model,
                thinking_level: r.thinking_level,
                is_scoped: r.is_scoped,
            })
        })
    }

    fn set_thinking_level(&self, level: ThinkingLevel) {
        self.set_thinking_level(level, None);
    }

    fn cycle_thinking_level(&self) -> Option<ThinkingLevel> {
        self.cycle_thinking_level(None)
    }

    fn get_available_thinking_levels(&self) -> Vec<ThinkingLevel> {
        self.get_available_thinking_levels()
    }

    fn set_steering_mode(&self, mode: QueueMode) {
        self.set_steering_mode(mode);
    }

    fn set_follow_up_mode(&self, mode: QueueMode) {
        self.set_follow_up_mode(mode);
    }

    async fn compact(&self, custom_instructions: Option<&str>) -> pi_js::Result<CompactionResult> {
        self.compact(custom_instructions).await
    }

    fn set_auto_compaction_enabled(&self, enabled: bool) {
        self.set_auto_compaction_enabled(enabled);
    }

    fn set_auto_retry_enabled(&self, enabled: bool) {
        self.set_auto_retry_enabled(enabled);
    }

    fn abort_retry(&self) {
        self.abort_retry();
    }

    fn cwd(&self) -> String {
        self.session_manager().get_cwd()
    }

    async fn emit_user_bash(
        &self,
        command: &str,
        exclude_from_context: bool,
        cwd: &str,
    ) -> pi_js::Result<Option<RpcUserBashOutcome>> {
        let Some(runner) = self.extension_runner() else {
            return Ok(None);
        };
        runner
            .emit_user_bash(crate::core::extensions::types::UserBashEvent {
                command: command.to_owned(),
                exclude_from_context,
                cwd: cwd.to_owned(),
            })
            .await
            .map(|opt| opt.map(Into::into))
    }

    fn record_bash_result(&self, command: &str, result: &BashResult, exclude_from_context: Option<bool>) {
        self.record_bash_result(
            command,
            &result.into(),
            Some(crate::core::agent_session::RecordBashOptions { exclude_from_context }),
        );
    }

    async fn execute_bash(&self, command: &str, options: RpcExecuteBashOptions) -> pi_js::Result<BashResult> {
        self.execute_bash(
            command,
            None,
            Some(crate::core::agent_session::ExecuteBashOptions {
                exclude_from_context: options.exclude_from_context,
                id: options.id,
                operations: options.operations,
            }),
        )
        .await
        .map(Into::into)
    }

    fn abort_bash(&self) {
        self.abort_bash();
    }

    fn get_session_stats(&self) -> SessionStats {
        self.get_session_stats()
    }

    async fn export_to_html(&self, output_path: Option<&str>) -> pi_js::Result<String> {
        self.export_to_html(output_path, None).await
    }

    fn leaf_id(&self) -> Option<String> {
        self.session_manager().get_leaf_id()
    }

    fn get_user_messages_for_forking(&self) -> Vec<ForkMessage> {
        self.get_user_messages_for_forking()
            .into_iter()
            .map(|m| ForkMessage {
                entry_id: m.entry_id,
                text: m.text,
            })
            .collect()
    }

    fn get_entries(&self) -> Vec<SessionEntry> {
        self.session_manager()
            .get_entries()
            .into_iter()
            .filter_map(|e| serde_json::to_value(e).ok())
            .collect()
    }

    fn get_tree(&self) -> Vec<SessionTreeNode> {
        self.session_manager()
            .get_tree()
            .iter()
            .filter_map(|n| session_tree_node_to_rpc(n).ok())
            .collect()
    }

    fn get_last_assistant_text(&self) -> Option<String> {
        self.get_last_assistant_text()
    }

    fn set_session_name(&self, name: &str) {
        self.set_session_name(name);
    }

    fn get_commands(&self) -> Vec<RpcSlashCommand> {
        self.get_commands()
            .into_iter()
            .map(|c| RpcSlashCommand {
                name: c.name,
                description: c.description,
                source: match c.source {
                    crate::core::extensions::types::SlashCommandSource::Extension => RpcSlashCommandSource::Extension,
                    crate::core::extensions::types::SlashCommandSource::Prompt => RpcSlashCommandSource::Prompt,
                    crate::core::extensions::types::SlashCommandSource::Skill => RpcSlashCommandSource::Skill,
                },
                source_info: c.source_info,
            })
            .collect()
    }

    async fn navigate_tree(
        &self,
        target_id: &str,
        options: Option<RpcNavigateTreeOptions>,
    ) -> pi_js::Result<CancelledResult> {
        let mapped = options.map(|o| crate::core::agent_session::NavigateTreeOptions {
            summarize: o.summarize,
            custom_instructions: o.custom_instructions,
            replace_instructions: o.replace_instructions,
            label: o.label,
        });
        let res = self.navigate_tree(target_id, mapped).await?;
        Ok(CancelledResult {
            cancelled: res.cancelled,
        })
    }

    async fn reload(&self) -> pi_js::Result<()> {
        self.reload(None).await
    }
}

#[async_trait]
impl RpcSessionRuntime for crate::core::agent_session_runtime::AgentSessionRuntime {
    fn session(&self) -> Arc<dyn RpcSession> {
        Arc::new(self.session())
    }

    fn set_rebind_session(&self, rebind: Option<Arc<dyn Fn() -> BoxFuture<pi_js::Result<()>> + Send + Sync>>) {
        self.set_rebind_session(
            rebind.map(|cb| Arc::new(move |_sess| cb()) as crate::core::agent_session_runtime::RebindSessionFn),
        );
    }

    async fn new_session(&self, options: Option<RpcNewSessionOptions>) -> pi_js::Result<CancelledResult> {
        let mapped = options.map(|o| crate::core::agent_session_runtime::RuntimeNewSessionOptions {
            parent_session: o.parent_session,
            setup: None,
            with_session: None,
        });
        let res = self.new_session(mapped).await?;
        Ok(CancelledResult {
            cancelled: res.cancelled,
        })
    }

    async fn switch_session(
        &self,
        session_path: &str,
        _options: Option<RpcSwitchSessionOptions>,
    ) -> pi_js::Result<CancelledResult> {
        let res = self.switch_session(session_path, None).await?;
        Ok(CancelledResult {
            cancelled: res.cancelled,
        })
    }

    async fn fork(&self, entry_id: &str, options: Option<RpcForkOptions>) -> pi_js::Result<RpcForkResult> {
        let mapped = options.map(|o| crate::core::agent_session_runtime::RuntimeForkOptions {
            position: o.position,
            with_session: None,
        });
        let res = self.fork(entry_id, mapped).await?;
        Ok(RpcForkResult {
            cancelled: res.cancelled,
            selected_text: res.selected_text,
        })
    }

    async fn dispose(&self) {
        self.dispose().await;
    }
}
