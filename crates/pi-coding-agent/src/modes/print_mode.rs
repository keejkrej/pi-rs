//! Port of packages/coding-agent/src/modes/print-mode.ts

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_agent_core::AgentMessage;
use pi_ai::{AssistantContentBlock, ImageContent, Message, StopReason};
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result, Unsubscribe};
use serde_json::Value;
use tokio::sync::{Notify, OnceCell};

use crate::modes::json_event::{AgentSessionEvent, to_json_event};

// WIRE(@tui-w3): Replace PrintSession / PrintSessionRuntime trait adapters with concrete
// crate::core::agent_session::AgentSession / AgentSessionRuntime, crate::core::output_guard,
// and crate::utils::shell::kill_tracked_detached_children once @mcp & @ca-cli land on main.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrintModeKind {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Debug, Default)]
pub struct PrintModeOptions {
    pub mode: PrintModeKind,
    pub messages: Vec<String>,
    pub initial_message: Option<String>,
    pub initial_images: Option<Vec<ImageContent>>,
}

#[derive(Clone, Debug, Default)]
pub struct PrintPromptOptions {
    pub images: Option<Vec<ImageContent>>,
}

#[derive(Clone, Default)]
pub struct PrintBindCommandActions {
    pub wait_for_idle: Option<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>,
}

pub trait PrintSession: Send + Sync {
    fn session_header_json(&self) -> Option<Value>;
    fn bind_extensions(&self, mode: &str, on_error: Arc<dyn Fn(&str, &str) + Send + Sync>) -> BoxFuture<Result<()>>;
    fn subscribe(&self, listener: Arc<dyn Fn(AgentSessionEvent) + Send + Sync>) -> Unsubscribe;
    fn subscribe_backpressure(&self, listener: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>) -> Unsubscribe;
    fn prompt(&self, text: &str, options: Option<PrintPromptOptions>) -> BoxFuture<Result<()>>;
    fn messages(&self) -> Vec<AgentMessage>;
    fn wait_for_idle(&self) -> BoxFuture<()>;
    fn reload(&self) -> BoxFuture<Result<()>>;
    fn navigate_tree(&self, target_id: &str) -> BoxFuture<Result<bool>>;
}

pub trait PrintSessionRuntime: Send + Sync {
    fn session(&self) -> Arc<dyn PrintSession>;
    fn set_rebind_session(&self, cb: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>);
    fn new_session(&self, parent_session: Option<String>) -> BoxFuture<Result<bool>>;
    fn fork(&self, entry_id: &str) -> BoxFuture<Result<bool>>;
    fn switch_session(&self, session_path: &str) -> BoxFuture<Result<bool>>;
    fn dispose(&self) -> BoxFuture<()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintSignalKind {
    SigTerm,
    SigHup,
}

impl PrintSignalKind {
    pub fn exit_code(self) -> i32 {
        match self {
            Self::SigHup => 129,
            Self::SigTerm => 143,
        }
    }
}

pub type PrintSignalTrigger = Arc<dyn Fn(PrintSignalKind) + Send + Sync>;

#[derive(Clone)]
pub struct PrintModeHooks {
    pub write_stdout: Arc<dyn Fn(&str) + Send + Sync>,
    pub write_stderr: Arc<dyn Fn(&str) + Send + Sync>,
    pub wait_backpressure: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>,
    pub flush_stdout: Arc<dyn Fn() -> BoxFuture<Result<()>> + Send + Sync>,
    pub kill_detached_children: Arc<dyn Fn() + Send + Sync>,
    pub exit_process: Arc<dyn Fn(i32) + Send + Sync>,
    pub register_signal_handler: Arc<dyn Fn(PrintSignalTrigger) -> Box<dyn FnOnce() + Send> + Send + Sync>,
}

impl Default for PrintModeHooks {
    fn default() -> Self {
        Self {
            write_stdout: Arc::new(|text| crate::core::output_guard::write_raw_stdout(text)),
            write_stderr: Arc::new(|msg| pi_js::console::error(msg)),
            wait_backpressure: Arc::new(|| Box::pin(crate::core::output_guard::wait_for_raw_stdout_backpressure())),
            flush_stdout: Arc::new(|| Box::pin(crate::core::output_guard::flush_raw_stdout())),
            kill_detached_children: Arc::new(|| {
                crate::utils::shell::kill_tracked_detached_children();
            }),
            exit_process: Arc::new(|code| std::process::exit(code)),
            register_signal_handler: Arc::new(|trigger| register_os_signals(trigger)),
        }
    }
}

fn register_os_signals(trigger: PrintSignalTrigger) -> Box<dyn FnOnce() + Send> {
    let cancelled = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    {
        let cancel_flag = Arc::clone(&cancelled);
        let trig = Arc::clone(&trigger);
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(async move {
                use tokio::signal::unix::{SignalKind, signal};
                let mut sigterm = match signal(SignalKind::terminate()) {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let mut sighup = signal(SignalKind::hangup()).ok();
                loop {
                    if cancel_flag.load(Ordering::SeqCst) {
                        break;
                    }
                    tokio::select! {
                        _ = sigterm.recv() => {
                            if cancel_flag.load(Ordering::SeqCst) { break; }
                            trig(PrintSignalKind::SigTerm);
                        }
                        _ = async {
                            match sighup.as_mut() {
                                Some(s) => { s.recv().await; }
                                None => std::future::pending::<()>().await,
                            }
                        } => {
                            if cancel_flag.load(Ordering::SeqCst) { break; }
                            trig(PrintSignalKind::SigHup);
                        }
                    }
                }
            });
        }
    }
    #[cfg(not(unix))]
    {
        let _ = trigger;
    }
    Box::new(move || {
        cancelled.store(true, Ordering::SeqCst);
    })
}

/// Run in print (single-shot) mode using default OS stdout/stderr/signal hooks.
pub async fn run_print_mode(runtime_host: Arc<dyn PrintSessionRuntime>, options: PrintModeOptions) -> i32 {
    run_print_mode_with_hooks(runtime_host, options, PrintModeHooks::default()).await
}

/// Run in print (single-shot) mode with injectable IO/signal hooks for deterministic testing.
pub async fn run_print_mode_with_hooks(
    runtime_host: Arc<dyn PrintSessionRuntime>,
    options: PrintModeOptions,
    hooks: PrintModeHooks,
) -> i32 {
    let mode = options.mode;
    let initial_session = runtime_host.session();
    let session_holder: Arc<Mutex<Arc<dyn PrintSession>>> = Arc::new(Mutex::new(initial_session));
    let unsub_holder: Arc<Mutex<Option<Unsubscribe>>> = Arc::new(Mutex::new(None));
    let bp_unsub_holder: Arc<Mutex<Option<Unsubscribe>>> = Arc::new(Mutex::new(None));

    // Shared single-flight disposal cell so concurrent SIGTERM/normal exit waits on the exact same future.
    let dispose_started = Arc::new(OnceCell::<()>::new());
    let dispose_done = Arc::new(Notify::new());
    let dispose_finished = Arc::new(AtomicBool::new(false));
    let signal_count = Arc::new(AtomicUsize::new(0));

    let dispose_runtime = {
        let runtime_host = Arc::clone(&runtime_host);
        let unsub_holder = Arc::clone(&unsub_holder);
        let bp_unsub_holder = Arc::clone(&bp_unsub_holder);
        let dispose_started = Arc::clone(&dispose_started);
        let dispose_done = Arc::clone(&dispose_done);
        let dispose_finished = Arc::clone(&dispose_finished);
        move || {
            let runtime_host = Arc::clone(&runtime_host);
            let unsub_holder = Arc::clone(&unsub_holder);
            let bp_unsub_holder = Arc::clone(&bp_unsub_holder);
            let dispose_started = Arc::clone(&dispose_started);
            let dispose_done = Arc::clone(&dispose_done);
            let dispose_finished = Arc::clone(&dispose_finished);
            async move {
                let is_first = dispose_started.set(()).is_ok();
                if is_first {
                    if let Some(unsub) = unsub_holder.lock().unwrap_or_else(|e| e.into_inner()).take() {
                        unsub.call();
                    }
                    if let Some(unsub) = bp_unsub_holder.lock().unwrap_or_else(|e| e.into_inner()).take() {
                        unsub.call();
                    }
                    runtime_host.dispose().await;
                    dispose_finished.store(true, Ordering::SeqCst);
                    dispose_done.notify_waiters();
                } else if !dispose_finished.load(Ordering::SeqCst) {
                    dispose_done.notified().await;
                }
            }
        }
    };

    let trigger_signal: PrintSignalTrigger = {
        let signal_count = Arc::clone(&signal_count);
        let kill_children = Arc::clone(&hooks.kill_detached_children);
        let exit_process = Arc::clone(&hooks.exit_process);
        let dispose_runtime = dispose_runtime.clone();
        Arc::new(move |sig: PrintSignalKind| {
            let count = signal_count.fetch_add(1, Ordering::SeqCst) + 1;
            kill_children();
            let code = sig.exit_code();
            if count > 1 {
                exit_process(code);
                return;
            }
            let dispose_fut = dispose_runtime();
            let exit_cb = Arc::clone(&exit_process);
            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::spawn(async move {
                    dispose_fut.await;
                    exit_cb(code);
                });
            }
        })
    };

    let cleanup_signal = (hooks.register_signal_handler)(trigger_signal);

    let rebind_session = {
        let runtime_host = Arc::clone(&runtime_host);
        let session_holder = Arc::clone(&session_holder);
        let unsub_holder = Arc::clone(&unsub_holder);
        let bp_unsub_holder = Arc::clone(&bp_unsub_holder);
        let write_stdout = Arc::clone(&hooks.write_stdout);
        let write_stderr = Arc::clone(&hooks.write_stderr);
        let wait_bp = Arc::clone(&hooks.wait_backpressure);
        Arc::new(move || -> BoxFuture<Result<()>> {
            let runtime_host = Arc::clone(&runtime_host);
            let session_holder = Arc::clone(&session_holder);
            let unsub_holder = Arc::clone(&unsub_holder);
            let bp_unsub_holder = Arc::clone(&bp_unsub_holder);
            let write_stdout = Arc::clone(&write_stdout);
            let write_stderr = Arc::clone(&write_stderr);
            let wait_bp = Arc::clone(&wait_bp);
            Box::pin(async move {
                let session = runtime_host.session();
                *session_holder.lock().unwrap_or_else(|e| e.into_inner()) = Arc::clone(&session);

                let mode_str = if mode == PrintModeKind::Json { "json" } else { "print" };
                let err_cb_stderr = Arc::clone(&write_stderr);
                session
                    .bind_extensions(
                        mode_str,
                        Arc::new(move |ext_path, err| {
                            err_cb_stderr(&format!("Extension error ({ext_path}): {err}"));
                        }),
                    )
                    .await?;

                if let Some(prev) = unsub_holder.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    prev.call();
                }
                if let Some(prev) = bp_unsub_holder.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    prev.call();
                }

                let stdout_cb = Arc::clone(&write_stdout);
                let unsub = session.subscribe(Arc::new(move |event| {
                    if mode == PrintModeKind::Json
                        && let Ok(json_ev) = to_json_event(&event)
                    {
                        stdout_cb(&format!("{}\n", pi_js::json::stringify(&json_ev)));
                    }
                }));
                *unsub_holder.lock().unwrap_or_else(|e| e.into_inner()) = Some(unsub);

                if mode == PrintModeKind::Json {
                    let bp_cb = Arc::clone(&wait_bp);
                    let bp_unsub = session.subscribe_backpressure(bp_cb);
                    *bp_unsub_holder.lock().unwrap_or_else(|e| e.into_inner()) = Some(bp_unsub);
                }

                Ok(())
            })
        })
    };

    {
        let rebind_cb = Arc::clone(&rebind_session);
        runtime_host.set_rebind_session(Arc::new(move || {
            let rebind_cb = Arc::clone(&rebind_cb);
            Box::pin(async move {
                let _ = rebind_cb().await;
            })
        }));
    }

    let result_code = async {
        if mode == PrintModeKind::Json {
            let header = {
                let guard = session_holder.lock().unwrap_or_else(|e| e.into_inner());
                guard.session_header_json()
            };
            if let Some(hdr) = header {
                (hooks.write_stdout)(&format!("{}\n", pi_js::json::stringify(&hdr)));
            }
        }

        if let Err(err) = rebind_session().await {
            (hooks.write_stderr)(&err.to_string());
            return 1;
        }

        if let Some(ref initial) = options.initial_message {
            let session = Arc::clone(&session_holder.lock().unwrap_or_else(|e| e.into_inner()));
            if let Err(err) = session
                .prompt(
                    initial,
                    Some(PrintPromptOptions {
                        images: options.initial_images.clone(),
                    }),
                )
                .await
            {
                (hooks.write_stderr)(&err.to_string());
                return 1;
            }
        }

        for msg in &options.messages {
            let session = Arc::clone(&session_holder.lock().unwrap_or_else(|e| e.into_inner()));
            if let Err(err) = session.prompt(msg, None).await {
                (hooks.write_stderr)(&err.to_string());
                return 1;
            }
        }

        let mut exit_code = 0;
        if mode == PrintModeKind::Text {
            let session = Arc::clone(&session_holder.lock().unwrap_or_else(|e| e.into_inner()));
            let messages = session.messages();
            if let Some(AgentMessage::Message(Message::Assistant(asst))) = messages.last() {
                if asst.stop_reason == StopReason::Error || asst.stop_reason == StopReason::Aborted {
                    let fallback = format!(
                        "Request {}",
                        match asst.stop_reason {
                            StopReason::Error => "error",
                            StopReason::Aborted => "aborted",
                            _ => "",
                        }
                    );
                    let err_msg = asst
                        .error_message
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&fallback);
                    (hooks.write_stderr)(err_msg);
                    exit_code = 1;
                } else {
                    for block in &asst.content {
                        if let AssistantContentBlock::Text(text) = block {
                            (hooks.write_stdout)(&format!("{}\n", text.text));
                        }
                    }
                    if asst.stop_reason == StopReason::Length {
                        (hooks.write_stderr)("Response was truncated: the model reached its output token limit.");
                        exit_code = 1;
                    }
                }
            }
        }

        exit_code
    }
    .await;

    cleanup_signal();
    dispose_runtime().await;
    let _ = (hooks.flush_stdout)().await;
    let _ = &None::<AbortSignal>;
    result_code
}
