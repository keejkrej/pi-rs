//! Port of packages/coding-agent/test/print-mode.test.ts

use std::sync::{Arc, Mutex};

use pi_agent_core::AgentMessage;
use pi_ai::{AssistantMessage, ImageContent, Message, StopReason};
use pi_coding_agent::modes::json_event::AgentSessionEvent;
use pi_coding_agent::modes::print_mode::{
    PrintModeHooks, PrintModeKind, PrintModeOptions, PrintPromptOptions, PrintSession, PrintSessionRuntime,
    PrintSignalKind, PrintSignalTrigger, run_print_mode_with_hooks,
};
use pi_js::{BoxFuture, Result, Unsubscribe};
use serde_json::{Value, json};
use tokio::sync::oneshot;

fn make_assistant_message(text: Option<&str>, stop_reason: StopReason, error_message: Option<&str>) -> AgentMessage {
    let content = match text {
        Some(t) => json!([{ "type": "text", "text": t }]),
        None => json!([]),
    };
    let asst: AssistantMessage = serde_json::from_value(json!({
        "role": "assistant",
        "content": content,
        "api": "openai-responses",
        "provider": "openai",
        "model": "gpt-4o-mini",
        "usage": {
            "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
        },
        "stopReason": stop_reason,
        "errorMessage": error_message,
        "timestamp": 1000
    }))
    .expect("valid assistant message");
    AgentMessage::Message(Message::Assistant(asst))
}

struct FakeSession {
    messages: Vec<AgentMessage>,
    prompts: Mutex<Vec<(String, Option<PrintPromptOptions>)>>,
    shutdown_emits: Arc<Mutex<usize>>,
}

impl FakeSession {
    fn new(msg: AgentMessage) -> Arc<Self> {
        Arc::new(Self {
            messages: vec![msg],
            prompts: Mutex::new(Vec::new()),
            shutdown_emits: Arc::new(Mutex::new(0)),
        })
    }
}

impl PrintSession for FakeSession {
    fn session_header_json(&self) -> Option<Value> {
        None
    }

    fn bind_extensions(&self, _mode: &str, _on_error: Arc<dyn Fn(&str, &str) + Send + Sync>) -> BoxFuture<Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn subscribe(&self, _listener: Arc<dyn Fn(AgentSessionEvent) + Send + Sync>) -> Unsubscribe {
        Unsubscribe::noop()
    }

    fn subscribe_backpressure(&self, _listener: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>) -> Unsubscribe {
        Unsubscribe::noop()
    }

    fn prompt(&self, text: &str, options: Option<PrintPromptOptions>) -> BoxFuture<Result<()>> {
        self.prompts.lock().unwrap().push((text.to_string(), options));
        Box::pin(async { Ok(()) })
    }

    fn messages(&self) -> Vec<AgentMessage> {
        self.messages.clone()
    }

    fn wait_for_idle(&self) -> BoxFuture<()> {
        Box::pin(async {})
    }

    fn reload(&self) -> BoxFuture<Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn navigate_tree(&self, _target_id: &str) -> BoxFuture<Result<bool>> {
        Box::pin(async { Ok(false) })
    }
}

struct FakeRuntimeHost {
    session: Arc<FakeSession>,
    dispose_count: Mutex<usize>,
    blocked_dispose: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    dispose_entered: Mutex<Option<oneshot::Sender<()>>>,
}

impl FakeRuntimeHost {
    fn new(session: Arc<FakeSession>) -> Arc<Self> {
        Arc::new(Self {
            session,
            dispose_count: Mutex::new(0),
            blocked_dispose: Mutex::new(None),
            dispose_entered: Mutex::new(None),
        })
    }

    fn with_blocked_dispose(session: Arc<FakeSession>) -> (Arc<Self>, oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (finish_tx, finish_rx) = oneshot::channel();
        let (entered_tx, entered_rx) = oneshot::channel();
        let (dummy_tx, _) = oneshot::channel();
        let host = Arc::new(Self {
            session,
            dispose_count: Mutex::new(0),
            blocked_dispose: Mutex::new(Some((dummy_tx, finish_rx))),
            dispose_entered: Mutex::new(Some(entered_tx)),
        });
        (host, entered_rx, finish_tx)
    }
}

impl PrintSessionRuntime for FakeRuntimeHost {
    fn session(&self) -> Arc<dyn PrintSession> {
        Arc::clone(&self.session) as Arc<dyn PrintSession>
    }

    fn set_rebind_session(&self, _cb: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>) {}

    fn new_session(&self, _parent_session: Option<String>) -> BoxFuture<Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    fn fork(&self, _entry_id: &str) -> BoxFuture<Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    fn switch_session(&self, _session_path: &str) -> BoxFuture<Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    fn dispose(&self) -> BoxFuture<()> {
        *self.dispose_count.lock().unwrap() += 1;
        if let Some(entered) = self.dispose_entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        let rx = self.blocked_dispose.lock().unwrap().take().map(|(_, rx)| rx);
        let emits = Arc::clone(&self.session.shutdown_emits);
        Box::pin(async move {
            if let Some(rx) = rx {
                let _ = rx.await;
            }
            *emits.lock().unwrap() += 1;
        })
    }
}

fn capture_hooks() -> (
    PrintModeHooks,
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Vec<i32>>>,
    Arc<Mutex<Option<PrintSignalTrigger>>>,
    Arc<Mutex<bool>>,
) {
    let stdout = Arc::new(Mutex::new(Vec::new()));
    let stderr = Arc::new(Mutex::new(Vec::new()));
    let exits = Arc::new(Mutex::new(Vec::new()));
    let trigger_slot: Arc<Mutex<Option<PrintSignalTrigger>>> = Arc::new(Mutex::new(None));
    let signal_cleaned = Arc::new(Mutex::new(false));

    let out_c = Arc::clone(&stdout);
    let err_c = Arc::clone(&stderr);
    let exit_c = Arc::clone(&exits);
    let trig_c = Arc::clone(&trigger_slot);
    let clean_c = Arc::clone(&signal_cleaned);

    let hooks = PrintModeHooks {
        write_stdout: Arc::new(move |s| out_c.lock().unwrap().push(s.to_string())),
        write_stderr: Arc::new(move |s| err_c.lock().unwrap().push(s.to_string())),
        wait_backpressure: Arc::new(|| Box::pin(async {})),
        flush_stdout: Arc::new(|| Box::pin(async { Ok(()) })),
        kill_detached_children: Arc::new(|| {}),
        exit_process: Arc::new(move |code| exit_c.lock().unwrap().push(code)),
        register_signal_handler: Arc::new(move |trigger| {
            *trig_c.lock().unwrap() = Some(trigger);
            let clean = Arc::clone(&clean_c);
            Box::new(move || {
                *clean.lock().unwrap() = true;
            })
        }),
    };
    (hooks, stdout, stderr, exits, trigger_slot, signal_cleaned)
}

#[tokio::test]
async fn emits_session_shutdown_in_text_mode() {
    let session = FakeSession::new(make_assistant_message(Some("done"), StopReason::Stop, None));
    let runtime = FakeRuntimeHost::new(Arc::clone(&session));
    let (hooks, stdout, _, _, _, _) = capture_hooks();
    let images = vec![ImageContent::new("abc", "image/png")];

    let exit_code = run_print_mode_with_hooks(
        runtime,
        PrintModeOptions {
            mode: PrintModeKind::Text,
            initial_message: Some("Say done".to_string()),
            initial_images: Some(images.clone()),
            messages: Vec::new(),
        },
        hooks,
    )
    .await;

    assert_eq!(exit_code, 0);
    let prompts = session.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].0, "Say done");
    assert_eq!(prompts[0].1.as_ref().unwrap().images.as_ref(), Some(&images));
    assert_eq!(*session.shutdown_emits.lock().unwrap(), 1);
    assert_eq!(*stdout.lock().unwrap(), vec!["done\n".to_string()]);
}

#[tokio::test]
async fn emits_session_shutdown_in_json_mode() {
    let session = FakeSession::new(make_assistant_message(Some("done"), StopReason::Stop, None));
    let runtime = FakeRuntimeHost::new(Arc::clone(&session));
    let (hooks, _, _, _, _, _) = capture_hooks();

    let exit_code = run_print_mode_with_hooks(
        runtime,
        PrintModeOptions {
            mode: PrintModeKind::Json,
            messages: vec!["hello".to_string()],
            initial_message: None,
            initial_images: None,
        },
        hooks,
    )
    .await;

    assert_eq!(exit_code, 0);
    let prompts = session.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].0, "hello");
    assert_eq!(*session.shutdown_emits.lock().unwrap(), 1);
}

#[tokio::test]
async fn emits_session_shutdown_and_returns_non_zero_on_assistant_error() {
    let session = FakeSession::new(make_assistant_message(
        None,
        StopReason::Error,
        Some("provider failure"),
    ));
    let runtime = FakeRuntimeHost::new(Arc::clone(&session));
    let (hooks, _, stderr, _, _, _) = capture_hooks();

    let exit_code = run_print_mode_with_hooks(runtime, PrintModeOptions::default(), hooks).await;

    assert_eq!(exit_code, 1);
    assert_eq!(*stderr.lock().unwrap(), vec!["provider failure".to_string()]);
    assert_eq!(*session.shutdown_emits.lock().unwrap(), 1);
}

// #10799
#[tokio::test]
async fn warns_on_stderr_and_returns_non_zero_when_response_was_truncated() {
    let session = FakeSession::new(make_assistant_message(Some("partial"), StopReason::Length, None));
    let runtime = FakeRuntimeHost::new(Arc::clone(&session));
    let (hooks, _, stderr, _, _, _) = capture_hooks();

    let exit_code = run_print_mode_with_hooks(runtime, PrintModeOptions::default(), hooks).await;

    assert_eq!(exit_code, 1);
    assert!(!stderr.lock().unwrap().is_empty());
}

#[tokio::test]
async fn waits_for_final_disposal_on_sigterm_and_exits_on_repeated_sigterm() {
    let session = FakeSession::new(make_assistant_message(Some("done"), StopReason::Stop, None));
    let (runtime, entered_rx, finish_tx) = FakeRuntimeHost::with_blocked_dispose(Arc::clone(&session));
    let (hooks, _, _, exits, trigger_slot, signal_cleaned) = capture_hooks();

    let runtime_clone = Arc::clone(&runtime);
    let run_handle = tokio::spawn(async move {
        run_print_mode_with_hooks(
            runtime_clone,
            PrintModeOptions {
                mode: PrintModeKind::Text,
                initial_message: Some("Say done".to_string()),
                initial_images: None,
                messages: Vec::new(),
            },
            hooks,
        )
        .await
    });

    entered_rx.await.unwrap();
    assert_eq!(*runtime.dispose_count.lock().unwrap(), 1);

    let trigger = trigger_slot.lock().unwrap().clone().unwrap();
    trigger(PrintSignalKind::SigTerm);
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert!(exits.lock().unwrap().is_empty());
    assert_eq!(*runtime.dispose_count.lock().unwrap(), 1);

    trigger(PrintSignalKind::SigTerm);
    assert_eq!(*exits.lock().unwrap(), vec![143]);

    let _ = finish_tx.send(());
    let code = run_handle.await.unwrap();
    assert_eq!(code, 0);
    assert!(*signal_cleaned.lock().unwrap());
}
