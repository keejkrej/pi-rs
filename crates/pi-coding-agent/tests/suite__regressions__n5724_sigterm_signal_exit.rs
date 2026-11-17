//! Port of packages/coding-agent/test/suite/regressions/5724-sigterm-signal-exit.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_coding_agent::modes::interactive::components::settings_selector::TuiMode;
use pi_coding_agent::modes::interactive::interactive_mode::{
    AgentSessionRuntimeDiagnostic, CancelledResult, InteractiveMode, InteractiveModeHooks, InteractiveModeOptions,
    InteractiveRuntimeHost, InteractiveSession, RuntimeForkOptions, RuntimeForkResult, SwitchSessionOptions,
};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_js::{BoxFuture, Result};
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

struct RecordingSignalTerminal {
    events: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Terminal for RecordingSignalTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}

    fn stop(&self) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push("terminal:stop".to_string());
    }

    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push("drain_input".to_string());
    }

    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        80
    }
    fn rows(&self) -> i64 {
        24
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

struct PendingDisposeHost {
    session: Arc<dyn InteractiveSession>,
    agent_dir: String,
    events: Arc<Mutex<Vec<String>>>,
    dispose_entered: Arc<Notify>,
    release_dispose: Arc<Notify>,
}

#[async_trait]
impl InteractiveRuntimeHost for PendingDisposeHost {
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

    async fn dispose(&self) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push("dispose:start".to_string());
        self.dispose_entered.notify_waiters();
        self.release_dispose.notified().await;
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push("dispose:end".to_string());
    }
}

#[tokio::test]
#[serial]
async fn keeps_signal_handlers_registered_while_signal_triggered_cleanup_is_pending() {
    init_theme(Some("dark"), Some(false));
    let _guard = ThemeResetGuard;

    let harness = create_harness(HarnessOptions::default()).await;
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let dispose_entered = Arc::new(Notify::new());
    let release_dispose = Arc::new(Notify::new());

    let host = Arc::new(PendingDisposeHost {
        session: Arc::new(harness.session.clone()),
        agent_dir: harness.temp_dir.clone(),
        events: Arc::clone(&events),
        dispose_entered: Arc::clone(&dispose_entered),
        release_dispose: Arc::clone(&release_dispose),
    });

    let events_exit = Arc::clone(&events);
    let mode = InteractiveMode::new(
        host,
        InteractiveModeOptions {
            tui_mode: Some(TuiMode::Regular),
            terminal: Some(Arc::new(RecordingSignalTerminal {
                events: Arc::clone(&events),
            })),
            hooks: Some(InteractiveModeHooks {
                exit_process: Arc::new(move |code| {
                    events_exit
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(format!("exit:{code}"));
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
    );

    mode.register_signal_handlers();
    let registered_count = mode.signal_handler_count();
    assert!(
        registered_count > 0,
        "expected registered signal handlers before signal shutdown"
    );

    let mode_shutdown = mode.clone();
    let shutdown_task = tokio::spawn(async move {
        mode_shutdown.shutdown(true).await;
    });

    dispose_entered.notified().await;

    // While runtime_host.dispose() is still awaiting completion, signal handlers
    // must remain registered and process exit must not have run yet.
    assert_eq!(mode.signal_handler_count(), registered_count);
    assert_eq!(
        events.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
        &["dispose:start".to_string()]
    );

    release_dispose.notify_waiters();
    shutdown_task.await.expect("shutdown join");

    assert_eq!(mode.signal_handler_count(), 0);
    assert_eq!(
        events.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
        &[
            "dispose:start".to_string(),
            "dispose:end".to_string(),
            "drain_input".to_string(),
            "exit:0".to_string(),
        ]
    );
}
