//! Port of packages/codemode/test/portable.test.ts

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};

use pi_codemode::{
    CodemodeCallStatus, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeOutputItem, CodemodeRemote,
    CodemodeRemoteExchange, CodemodeRemoteMessage, CodemodeRemoteReply, CodemodeRemoteRequest, CodemodeResult,
    CodemodeServeOptions, CodemodeTool, CodemodeWasmInput, InlineCodemodeSandbox, InlineCodemodeSandboxOptions,
    RemoteCodemodeSandbox, RemoteCodemodeSandboxOptions, load_quick_js_wasm, serve_codemode_remote,
};
use pi_js::abort::AbortController;

fn make_tool<F, Fut>(name: &str, f: F) -> CodemodeTool
where
    F: Fn(Option<Value>, pi_codemode::CodemodeToolContext) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = pi_js::Result<Option<Value>>> + Send + 'static,
{
    let f = Arc::new(f);
    CodemodeTool {
        name: name.to_string(),
        description: None,
        input_schema: None,
        output_schema: None,
        spread: None,
        signature: None,
        execute: Arc::new(move |args, ctx| {
            let f = Arc::clone(&f);
            Box::pin(async move { f(args, ctx).await })
        }),
    }
}

struct RecordingRemote {
    max_interrupt_budget: Option<usize>,
    max_memory_limit_bytes: Option<f64>,
    exchanges: Arc<Mutex<Vec<usize>>>,
    served: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl RecordingRemote {
    fn new(max_interrupt_budget: Option<usize>, max_memory_limit_bytes: Option<f64>) -> Arc<Self> {
        Arc::new(Self {
            max_interrupt_budget,
            max_memory_limit_bytes,
            exchanges: Arc::new(Mutex::new(Vec::new())),
            served: Arc::new(Mutex::new(Vec::new())),
        })
    }

    async fn wait_served(&self) {
        let handles = std::mem::take(&mut *self.served.lock().unwrap());
        for handle in handles {
            let _ = handle.await;
        }
    }
}

#[async_trait]
impl CodemodeRemote for RecordingRemote {
    async fn execute(&self, request: CodemodeRemoteRequest, exchange: CodemodeRemoteExchange) -> pi_js::Result<()> {
        let wasm = load_quick_js_wasm(None).await?;
        let req_wire: CodemodeRemoteRequest =
            serde_json::from_str(&pi_js::json::stringify(&request)).map_err(|e| pi_js::Error::msg(e.to_string()))?;
        let exchanges = Arc::clone(&self.exchanges);
        let wrapped_exchange: CodemodeRemoteExchange = Arc::new(move |messages: Vec<CodemodeRemoteMessage>| {
            let exchanges = Arc::clone(&exchanges);
            let exchange = Arc::clone(&exchange);
            Box::pin(async move {
                exchanges.lock().unwrap().push(messages.len());
                let cloned_msgs: Vec<CodemodeRemoteMessage> = serde_json::from_str(&pi_js::json::stringify(&messages))
                    .map_err(|e| pi_js::Error::msg(e.to_string()))?;
                let reply = exchange(cloned_msgs).await?;
                let cloned_reply: Option<CodemodeRemoteReply> = match reply {
                    Some(r) => Some(
                        serde_json::from_str(&pi_js::json::stringify(&r))
                            .map_err(|e| pi_js::Error::msg(e.to_string()))?,
                    ),
                    None => None,
                };
                Ok(cloned_reply)
            })
        });

        let opts = CodemodeServeOptions {
            wasm: CodemodeWasmInput::Module(wasm),
            max_interrupt_budget: self.max_interrupt_budget,
            max_memory_limit_bytes: self.max_memory_limit_bytes,
        };
        let handle = tokio::spawn(async move {
            serve_codemode_remote(req_wire, wrapped_exchange, opts).await;
        });
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let watcher = tokio::spawn(async move {
            let _ = handle.await;
            let _ = done_tx.send(());
        });
        self.served.lock().unwrap().push(watcher);
        let _ = done_rx.await;
        Ok(())
    }
}

#[tokio::test]
async fn inline_sandbox_interrupt_budget_timeout() {
    let wasm = load_quick_js_wasm(None).await.expect("wasm");
    let sandbox = InlineCodemodeSandbox::new(InlineCodemodeSandboxOptions {
        tools: None,
        globals: None,
        timeout_ms: None,
        memory_limit_bytes: None,
        wasm: CodemodeWasmInput::Module(wasm.clone()),
        interrupt_budget: Some(1_000),
    })
    .expect("inline sandbox");

    for code in ["while (true) {}", "while (true) await null"] {
        let res = sandbox
            .execute(
                code,
                Some(CodemodeExecuteOptions {
                    timeout_ms: Some(f64::INFINITY),
                    ..Default::default()
                }),
            )
            .await
            .expect("execute");
        match res {
            CodemodeResult::Failure { error, .. } => {
                assert_eq!(error.kind, CodemodeErrorKind::Timeout);
                assert_eq!(error.message, "Execution exceeded its interrupt budget of 1000");
            }
            other => panic!("expected timeout failure for {code}, got {other:?}"),
        }
    }

    let counting = InlineCodemodeSandbox::new(InlineCodemodeSandboxOptions {
        tools: Some(vec![make_tool("tick", |_, _| async { Ok(Some(json!(1))) })]),
        globals: None,
        timeout_ms: None,
        memory_limit_bytes: None,
        wasm: CodemodeWasmInput::Module(wasm),
        interrupt_budget: Some(1_000),
    })
    .expect("counting sandbox");

    let counted = counting
        .execute("for (;;) { for (let i = 0; i < 1e5; i++); await tools.tick(); }", None)
        .await
        .expect("execute");
    match counted {
        CodemodeResult::Failure { error, calls, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Timeout);
            assert!(calls.len() > 1, "expected multiple tool calls, got {}", calls.len());
        }
        other => panic!("expected timeout failure, got {other:?}"),
    }
}

#[tokio::test]
async fn remote_sandbox_tools_batching_caps_abort_and_failures() {
    // Runs tools in the caller and script on the remote.
    let rec = RecordingRemote::new(None, None);
    let rec_for_closure = Arc::clone(&rec);
    let seen = Arc::new(Mutex::new(Vec::<Option<Value>>::new()));
    let seen_tool = Arc::clone(&seen);
    let sandbox = RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
        tools: Some(vec![make_tool("add", move |args, _| {
            let seen_tool = Arc::clone(&seen_tool);
            async move {
                seen_tool.lock().unwrap().push(args.clone());
                let a = args
                    .as_ref()
                    .and_then(|v| v.get("a"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                Ok(Some(json!(a + 1)))
            }
        })]),
        globals: None,
        timeout_ms: None,
        memory_limit_bytes: None,
        remote: Arc::new(move || Ok(Arc::clone(&rec_for_closure) as Arc<dyn CodemodeRemote>)),
        interrupt_budget: None,
    })
    .expect("remote sandbox");

    let mut store = indexmap::IndexMap::new();
    store.insert("k".to_string(), json!(1));
    let res = sandbox
        .execute(
            "text('hi'); return await tools.add({ a: 1 })",
            Some(CodemodeExecuteOptions {
                store: Some(store),
                ..Default::default()
            }),
        )
        .await
        .expect("execute");
    match res {
        CodemodeResult::Success {
            value, output, calls, ..
        } => {
            assert_eq!(value, Some(json!(2)));
            assert_eq!(output, vec![CodemodeOutputItem::Text { text: "hi".to_string() }]);
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].name, "add");
            assert_eq!(calls[0].status, CodemodeCallStatus::Ok);
        }
        other => panic!("unexpected result: {other:?}"),
    }
    assert_eq!(*seen.lock().unwrap(), vec![Some(json!({ "a": 1 }))]);
    rec.wait_served().await;

    // Batches output instead of making one exchange per line.
    let rec_batch = RecordingRemote::new(None, None);
    let rec_batch_fn = Arc::clone(&rec_batch);
    let batch_sandbox = RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
        tools: None,
        globals: None,
        timeout_ms: None,
        memory_limit_bytes: None,
        remote: Arc::new(move || Ok(Arc::clone(&rec_batch_fn) as Arc<dyn CodemodeRemote>)),
        interrupt_budget: None,
    })
    .expect("batch sandbox");
    let batched = batch_sandbox
        .execute("for (let i = 0; i < 5000; i++) console.log(i); return 'ok'", None)
        .await
        .expect("execute");
    assert!(batched.is_ok());
    assert_eq!(batched.output().len(), 5000);
    assert_eq!(
        batched.output()[4999],
        CodemodeOutputItem::Text {
            text: "4999".to_string()
        }
    );
    assert!(batched.is_console_output(4999));
    assert!(
        rec_batch.exchanges.lock().unwrap().len() <= 6,
        "expected <= 6 batched exchanges, got {}",
        rec_batch.exchanges.lock().unwrap().len()
    );

    // Caps the interrupt budget and memory at the remote's limits.
    let rec_capped = RecordingRemote::new(Some(1_000), Some((8 * 1024 * 1024) as f64));
    let rec_capped_fn = Arc::clone(&rec_capped);
    let capped_sandbox = RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
        tools: None,
        globals: None,
        timeout_ms: None,
        memory_limit_bytes: None,
        remote: Arc::new(move || Ok(Arc::clone(&rec_capped_fn) as Arc<dyn CodemodeRemote>)),
        interrupt_budget: Some(1_000_000_000),
    })
    .expect("capped sandbox");
    match capped_sandbox.execute("while (true) {}", None).await.expect("execute") {
        CodemodeResult::Failure { error, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Timeout);
            assert_eq!(error.message, "Execution exceeded its interrupt budget of 1000");
        }
        other => panic!("expected capped timeout, got {other:?}"),
    }
    match capped_sandbox
        .execute(
            "const parts = []; for (;;) parts.push('x'.repeat(1 << 20) + parts.length);",
            None,
        )
        .await
        .expect("execute")
    {
        CodemodeResult::Failure { error, .. } => {
            assert!(
                error.message.to_lowercase().contains("out of memory"),
                "expected OOM error, got: {}",
                error.message
            );
        }
        other => panic!("expected OOM, got {other:?}"),
    }

    // Tells the remote to stop when the caller aborts.
    let rec_abort = RecordingRemote::new(None, None);
    let rec_abort_fn = Arc::clone(&rec_abort);
    let called_count = Arc::new(AtomicUsize::new(0));
    let called_notify = Arc::new(tokio::sync::Notify::new());
    let called_count_t = Arc::clone(&called_count);
    let called_notify_t = Arc::clone(&called_notify);
    let abort_sandbox = RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
        tools: Some(vec![make_tool("hang", move |_, ctx| {
            let called_count_t = Arc::clone(&called_count_t);
            let called_notify_t = Arc::clone(&called_notify_t);
            async move {
                called_count_t.fetch_add(1, Ordering::SeqCst);
                called_notify_t.notify_one();
                ctx.signal.cancelled().await;
                Err(pi_js::Error::msg("aborted"))
            }
        })]),
        globals: None,
        timeout_ms: None,
        memory_limit_bytes: None,
        remote: Arc::new(move || Ok(Arc::clone(&rec_abort_fn) as Arc<dyn CodemodeRemote>)),
        interrupt_budget: None,
    })
    .expect("abort sandbox");

    let controller = AbortController::new();
    let abort_sandbox_exec = abort_sandbox.clone();
    let signal = controller.signal();
    let pending = tokio::spawn(async move {
        abort_sandbox_exec
            .execute(
                "await tools.hang(); return 'never'",
                Some(CodemodeExecuteOptions {
                    signal: Some(signal),
                    ..Default::default()
                }),
            )
            .await
    });
    if called_count.load(Ordering::SeqCst) == 0 {
        called_notify.notified().await;
    }
    controller.abort(None);
    match pending.await.expect("join").expect("execute") {
        CodemodeResult::Failure { error, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Aborted);
        }
        other => panic!("expected aborted, got {other:?}"),
    }
    rec_abort.wait_served().await;

    // Reports a failing, silent, or unreachable remote as a sandbox error.
    struct FailingRemote;
    #[async_trait]
    impl CodemodeRemote for FailingRemote {
        async fn execute(&self, _: CodemodeRemoteRequest, _: CodemodeRemoteExchange) -> pi_js::Result<()> {
            Err(pi_js::Error::msg("object reset"))
        }
    }
    let failing = RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
        tools: None,
        globals: None,
        timeout_ms: None,
        memory_limit_bytes: None,
        remote: Arc::new(|| Ok(Arc::new(FailingRemote) as Arc<dyn CodemodeRemote>)),
        interrupt_budget: None,
    })
    .expect("failing");
    match failing.execute("return 1", None).await.expect("execute") {
        CodemodeResult::Failure { error, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Sandbox);
            assert_eq!(error.message, "Remote sandbox failed: object reset");
        }
        other => panic!("expected failing remote error, got {other:?}"),
    }

    struct SilentRemote;
    #[async_trait]
    impl CodemodeRemote for SilentRemote {
        async fn execute(&self, _: CodemodeRemoteRequest, _: CodemodeRemoteExchange) -> pi_js::Result<()> {
            Ok(())
        }
    }
    let silent = RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
        tools: None,
        globals: None,
        timeout_ms: None,
        memory_limit_bytes: None,
        remote: Arc::new(|| Ok(Arc::new(SilentRemote) as Arc<dyn CodemodeRemote>)),
        interrupt_budget: None,
    })
    .expect("silent");
    match silent.execute("return 1", None).await.expect("execute") {
        CodemodeResult::Failure { error, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Sandbox);
            assert_eq!(error.message, "Remote sandbox returned before the script settled");
        }
        other => panic!("expected silent remote error, got {other:?}"),
    }

    let unreachable = RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
        tools: None,
        globals: None,
        timeout_ms: None,
        memory_limit_bytes: None,
        remote: Arc::new(|| Err(pi_js::Error::msg("no binding"))),
        interrupt_budget: None,
    })
    .expect("unreachable");
    match unreachable.execute("return 1", None).await.expect("execute") {
        CodemodeResult::Failure { error, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Sandbox);
            assert_eq!(error.message, "Failed to reach the remote sandbox: no binding");
        }
        other => panic!("expected unreachable error, got {other:?}"),
    }
}
