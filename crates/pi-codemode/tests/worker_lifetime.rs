//! Port of packages/codemode/test/worker-lifetime.test.ts

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use pi_codemode::{
    CodemodeCallStatus, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeOutputItem, CodemodeResult,
    CodemodeRunOptions, CodemodeSandbox, CodemodeSandboxOptions, CodemodeTool, CodemodeWasmInput, CodemodeWorkerHooks,
};
use pi_js::abort::{AbortController, AbortReason};

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

#[tokio::test]
async fn does_not_start_worker_when_admission_is_denied() {
    let acquire_count = Arc::new(AtomicUsize::new(0));
    let started_count = Arc::new(AtomicUsize::new(0));
    let acquire_c = Arc::clone(&acquire_count);
    let started_c = Arc::clone(&started_count);

    let sandbox = CodemodeSandbox::with_worker_hooks(
        None,
        CodemodeWorkerHooks {
            acquire_worker: Some(Arc::new(move || {
                acquire_c.fetch_add(1, Ordering::SeqCst);
                Err(pi_js::Error::msg("worker capacity exhausted"))
            })),
            on_worker_started: Some(Arc::new(move |_| {
                started_c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })),
        },
    )
    .expect("sandbox");

    let res = sandbox.execute("while (true) {}", None).await.expect("execute");
    match res {
        CodemodeResult::Failure { error, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Sandbox);
            assert_eq!(error.message, "worker capacity exhausted");
        }
        other => panic!("unexpected result: {other:?}"),
    }
    assert_eq!(acquire_count.load(Ordering::SeqCst), 1);
    assert_eq!(started_count.load(Ordering::SeqCst), 0);
    sandbox.close().await;
}

#[tokio::test]
async fn does_not_acquire_admission_when_wasm_loading_fails() {
    let acquire_count = Arc::new(AtomicUsize::new(0));
    let acquire_c = Arc::clone(&acquire_count);

    let sandbox = CodemodeSandbox::with_worker_hooks(
        Some(CodemodeSandboxOptions {
            wasm: Some(CodemodeWasmInput::Pending(Arc::new(Mutex::new(Some(Box::pin(
                async { Err(pi_js::Error::msg("wasm unavailable")) },
            )))))),
            ..Default::default()
        }),
        CodemodeWorkerHooks {
            acquire_worker: Some(Arc::new(move || {
                acquire_c.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(|| {}))
            })),
            on_worker_started: None,
        },
    )
    .expect("sandbox");

    let res = sandbox.execute("return 1", None).await.expect("execute");
    match res {
        CodemodeResult::Failure { error, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Sandbox);
            assert_eq!(error.message, "Failed to load QuickJS: wasm unavailable");
        }
        other => panic!("unexpected result: {other:?}"),
    }
    assert_eq!(acquire_count.load(Ordering::SeqCst), 0);
    sandbox.close().await;
}

#[tokio::test]
async fn settles_promptly_and_tracks_exit_on_abort_timeout_and_close() {
    for how in ["abort", "timeout", "close"] {
        let called_count = Arc::new(AtomicUsize::new(0));
        let called_notify = Arc::new(tokio::sync::Notify::new());
        let tool_aborted = Arc::new(AtomicBool::new(false));
        let exits = Arc::new(Mutex::new(Vec::new()));
        let release_count = Arc::new(AtomicUsize::new(0));

        let called_c = Arc::clone(&called_count);
        let called_n = Arc::clone(&called_notify);
        let aborted_t = Arc::clone(&tool_aborted);
        let exits_cb = Arc::clone(&exits);
        let release_cb = Arc::clone(&release_count);

        let sandbox = CodemodeSandbox::with_worker_hooks(
            Some(CodemodeSandboxOptions {
                tools: Some(vec![make_tool("hang", move |_, ctx| {
                    let called_c = Arc::clone(&called_c);
                    let called_n = Arc::clone(&called_n);
                    let aborted_t = Arc::clone(&aborted_t);
                    async move {
                        called_c.fetch_add(1, Ordering::SeqCst);
                        called_n.notify_one();
                        ctx.signal.cancelled().await;
                        aborted_t.store(true, Ordering::SeqCst);
                        Err(pi_js::Error::msg("cancelled"))
                    }
                })]),
                timeout_ms: Some(if how == "timeout" { 100.0 } else { 10_000.0 }),
                ..Default::default()
            }),
            CodemodeWorkerHooks {
                acquire_worker: Some(Arc::new(move || {
                    let release_cb = Arc::clone(&release_cb);
                    Ok(Box::new(move || {
                        release_cb.fetch_add(1, Ordering::SeqCst);
                    }))
                })),
                on_worker_started: Some(Arc::new(move |exited| {
                    exits_cb.lock().unwrap().push(exited);
                    Ok(())
                })),
            },
        )
        .expect("sandbox");

        let controller = AbortController::new();
        let signal = controller.signal();
        let sb_exec = sandbox.clone();
        let exec_handle = tokio::spawn(async move {
            sb_exec
                .execute(
                    r#"text("partial"); await tools.hang()"#,
                    Some(CodemodeExecuteOptions {
                        signal: Some(signal),
                        ..Default::default()
                    }),
                )
                .await
                .expect("execute")
        });

        if called_count.load(Ordering::SeqCst) == 0 {
            called_notify.notified().await;
        }
        let started_cancel = pi_js::time::performance_now();
        if how == "abort" {
            controller.abort(Some(AbortReason::error("Error", "cancelled")));
        } else if how == "close" {
            sandbox.close().await;
        }

        let res = exec_handle.await.expect("join");
        let elapsed_ms = pi_js::time::performance_now() - started_cancel;
        assert!(elapsed_ms < 1000.0, "settlement took {elapsed_ms} ms for {how}");

        match &res {
            CodemodeResult::Failure {
                error, output, calls, ..
            } => {
                assert_eq!(
                    error.kind,
                    if how == "timeout" {
                        CodemodeErrorKind::Timeout
                    } else {
                        CodemodeErrorKind::Aborted
                    }
                );
                assert_eq!(
                    output,
                    &vec![CodemodeOutputItem::Text {
                        text: "partial".to_string()
                    }]
                );
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "hang");
                assert_eq!(calls[0].status, CodemodeCallStatus::Cancelled);
            }
            other => panic!("unexpected result for {how}: {other:?}"),
        }

        let _ = pi_js::time::sleep(10, None).await;
        assert!(
            tool_aborted.load(Ordering::SeqCst),
            "tool signal should abort for {how}"
        );

        let exit_futs: Vec<_> = std::mem::take(&mut *exits.lock().unwrap());
        assert_eq!(exit_futs.len(), 1);
        for fut in exit_futs {
            fut.await;
        }
        assert_eq!(release_count.load(Ordering::SeqCst), 1);
        sandbox.close().await;
    }
}

#[tokio::test]
async fn reports_actual_exit_of_execute_and_start_workers() {
    let acquire_count = Arc::new(AtomicUsize::new(0));
    let release_count = Arc::new(AtomicUsize::new(0));
    let exits = Arc::new(Mutex::new(Vec::new()));

    let acquire_c = Arc::clone(&acquire_count);
    let release_c = Arc::clone(&release_count);
    let exits_cb = Arc::clone(&exits);

    let sandbox = CodemodeSandbox::with_worker_hooks(
        None,
        CodemodeWorkerHooks {
            acquire_worker: Some(Arc::new(move || {
                acquire_c.fetch_add(1, Ordering::SeqCst);
                let rc = Arc::clone(&release_c);
                Ok(Box::new(move || {
                    rc.fetch_add(1, Ordering::SeqCst);
                }))
            })),
            on_worker_started: Some(Arc::new(move |exited| {
                exits_cb.lock().unwrap().push(exited);
                Ok(())
            })),
        },
    )
    .expect("sandbox");

    match sandbox.execute("return 1", None).await.expect("execute") {
        CodemodeResult::Success { value, .. } => assert_eq!(value, Some(json!(1))),
        other => panic!("unexpected execute result: {other:?}"),
    }

    let run = sandbox
        .start(
            "return 2",
            CodemodeRunOptions {
                started_at: 0.0,
                seed: 0,
                tools: None,
                globals: None,
                store: None,
                signal: None,
                timeout_ms: None,
                on_started: None,
                on_call: Arc::new(|_, _| {}),
                on_idle: None,
            },
        )
        .expect("start");
    match run.result().await {
        CodemodeResult::Success { value, .. } => assert_eq!(value, Some(json!(2))),
        other => panic!("unexpected start result: {other:?}"),
    }
    assert!(run.finished());

    let exit_futs: Vec<_> = std::mem::take(&mut *exits.lock().unwrap());
    assert_eq!(exit_futs.len(), 2);
    for fut in exit_futs {
        fut.await;
    }
    assert_eq!(acquire_count.load(Ordering::SeqCst), 2);
    assert_eq!(release_count.load(Ordering::SeqCst), 2);
    sandbox.close().await;
}

#[tokio::test]
async fn requests_termination_when_on_worker_started_fails() {
    let exited_slot = Arc::new(Mutex::new(None));
    let exited_cb = Arc::clone(&exited_slot);
    let release_count = Arc::new(AtomicUsize::new(0));
    let release_c = Arc::clone(&release_count);

    let sandbox = CodemodeSandbox::with_worker_hooks(
        None,
        CodemodeWorkerHooks {
            acquire_worker: Some(Arc::new(move || {
                let rc = Arc::clone(&release_c);
                Ok(Box::new(move || {
                    rc.fetch_add(1, Ordering::SeqCst);
                }))
            })),
            on_worker_started: Some(Arc::new(move |exited| {
                *exited_cb.lock().unwrap() = Some(exited);
                Err(pi_js::Error::msg("lifetime hook failed"))
            })),
        },
    )
    .expect("sandbox");

    match sandbox.execute("while (true) {}", None).await.expect("execute") {
        CodemodeResult::Failure { error, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Sandbox);
            assert_eq!(error.message, "lifetime hook failed");
        }
        other => panic!("unexpected result: {other:?}"),
    }

    let exited = exited_slot.lock().unwrap().take().expect("exited future");
    exited.await;
    assert_eq!(release_count.load(Ordering::SeqCst), 1);
    sandbox.close().await;
}
