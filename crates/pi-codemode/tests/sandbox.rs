//! Port of packages/codemode/test/sandbox.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use indexmap::IndexMap;
use serde_json::{Value, json};

use pi_codemode::{
    CodemodeCallStatus, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeOutputItem, CodemodeResult, CodemodeSandbox,
    CodemodeSandboxOptions, CodemodeTool, CodemodeWasmInput, load_quick_js_wasm,
};
use pi_js::abort::{AbortController, AbortReason};

fn make_tool<F, Fut>(name: &str, description: Option<&str>, f: F) -> CodemodeTool
where
    F: Fn(Option<Value>, pi_codemode::CodemodeToolContext) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = pi_js::Result<Option<Value>>> + Send + 'static,
{
    let f = Arc::new(f);
    CodemodeTool {
        name: name.to_string(),
        description: description.map(str::to_string),
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
async fn evaluates_values_and_top_level_await() {
    let sandbox = CodemodeSandbox::new(None).expect("sandbox");

    let res = sandbox.execute("return 20 + 22;", None).await.expect("execute");
    match res {
        CodemodeResult::Success { ok, value, .. } => {
            assert!(ok);
            assert_eq!(value, Some(json!(42)));
        }
        other => panic!("unexpected result: {other:?}"),
    }

    let undef = sandbox.execute("const x = 1;", None).await.expect("execute");
    match undef {
        CodemodeResult::Success { ok, value, .. } => {
            assert!(ok);
            assert_eq!(value, None);
        }
        other => panic!("unexpected result: {other:?}"),
    }

    let null_res = sandbox.execute("return null;", None).await.expect("execute");
    match null_res {
        CodemodeResult::Success { ok, value, .. } => {
            assert!(ok);
            assert_eq!(value, Some(Value::Null));
        }
        other => panic!("unexpected result: {other:?}"),
    }

    let await_res = sandbox
        .execute(
            "const v = await Promise.resolve({ a: 1, b: [true, 'x'] }); return v;",
            None,
        )
        .await
        .expect("execute");
    match await_res {
        CodemodeResult::Success { ok, value, .. } => {
            assert!(ok);
            assert_eq!(value, Some(json!({ "a": 1, "b": [true, "x"] })));
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn exposes_tools_all_tools_and_call_tracking() {
    let echo = make_tool("read-file", Some("Read a file"), |args, _ctx| async move {
        let path = args
            .as_ref()
            .and_then(|v| v.get("path"))
            .and_then(Value::as_str)
            .unwrap_or("missing");
        Ok(Some(json!(format!("contents:{path}"))))
    });

    let check_none = make_tool("check_arg", None, |args, _ctx| async move {
        Ok(Some(
            json!({ "wasNone": args.is_none(), "wasNull": args == Some(Value::Null) }),
        ))
    });

    let slow_first = make_tool("slow_first", None, |_, _| async move {
        let _ = pi_js::time::sleep(30, None).await;
        Ok(Some(json!("first")))
    });

    let fast_second = make_tool("fast_second", None, |_, _| async move { Ok(Some(json!("second"))) });

    let fail_tool = make_tool("fail_tool", None, |_, _| async move {
        Err(pi_js::Error::msg("boom from host"))
    });

    let sandbox = CodemodeSandbox::new(Some(CodemodeSandboxOptions {
        tools: Some(vec![echo, check_none, slow_first, fast_second, fail_tool]),
        ..Default::default()
    }))
    .expect("sandbox");

    let res = sandbox
        .execute(
            r#"
            const a = await tools.read_file({ path: "foo.txt" });
            const b = await tools["read-file"]({ path: "bar.txt" });
            const argUndef = await tools.check_arg(undefined);
            const argNull = await tools.check_arg(null);
            const both = await Promise.all([tools.slow_first(), tools.fast_second()]);
            let errMsg = null;
            try {
                await tools.fail_tool();
            } catch (e) {
                errMsg = e.message;
            }
            return { a, b, argUndef, argNull, both, errMsg, allTools: ALL_TOOLS };
            "#,
            None,
        )
        .await
        .expect("execute");

    match res {
        CodemodeResult::Success { value, calls, .. } => {
            let v = value.expect("value");
            assert_eq!(v["a"], json!("contents:foo.txt"));
            assert_eq!(v["b"], json!("contents:bar.txt"));
            assert_eq!(v["argUndef"], json!({ "wasNone": true, "wasNull": false }));
            assert_eq!(v["argNull"], json!({ "wasNone": false, "wasNull": true }));
            assert_eq!(v["both"], json!(["first", "second"]));
            assert_eq!(v["errMsg"], json!("boom from host"));
            assert_eq!(v["allTools"][0]["name"], json!("read_file"));
            assert_eq!(v["allTools"][0]["description"], json!("Read a file"));

            let call_names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
            assert_eq!(
                call_names,
                vec![
                    "read-file",
                    "read-file",
                    "check_arg",
                    "check_arg",
                    "slow_first",
                    "fast_second",
                    "fail_tool"
                ]
            );
            assert_eq!(calls[4].status, CodemodeCallStatus::Ok);
            assert_eq!(calls[6].status, CodemodeCallStatus::Error);
            for c in &calls {
                assert!(c.duration_ms >= 0.0);
            }
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn manages_tools_and_validates_globals() {
    let t1 = make_tool("dup", None, |_, _| async move { Ok(None) });
    assert!(
        CodemodeSandbox::new(Some(CodemodeSandboxOptions {
            tools: Some(vec![t1.clone(), t1]),
            ..Default::default()
        }))
        .is_err()
    );

    let bad_global = make_tool("123bad", None, |_, _| async move { Ok(None) });
    assert!(
        CodemodeSandbox::new(Some(CodemodeSandboxOptions {
            globals: Some(vec![bad_global]),
            ..Default::default()
        }))
        .is_err()
    );

    let reserved_global = make_tool("console", None, |_, _| async move { Ok(None) });
    assert!(
        CodemodeSandbox::new(Some(CodemodeSandboxOptions {
            globals: Some(vec![reserved_global]),
            ..Default::default()
        }))
        .is_err()
    );

    let reserved_ns = make_tool("tools.extra", None, |_, _| async move { Ok(None) });
    assert!(
        CodemodeSandbox::new(Some(CodemodeSandboxOptions {
            globals: Some(vec![reserved_ns]),
            ..Default::default()
        }))
        .is_err()
    );

    let mut g_spread = make_tool("sumAll", None, |args, _| async move {
        let arr = args.and_then(|v| v.as_array().cloned()).unwrap_or_default();
        let sum: i64 = arr.iter().filter_map(|v| v.as_i64()).sum();
        Ok(Some(json!(sum)))
    });
    g_spread.spread = Some(true);

    let g_ns = make_tool("ui.badge", None, |args, _| async move { Ok(args) });

    let sandbox = CodemodeSandbox::new(Some(CodemodeSandboxOptions {
        globals: Some(vec![g_spread, g_ns]),
        ..Default::default()
    }))
    .expect("sandbox");
    assert_eq!(sandbox.globals().len(), 2);

    let added = make_tool("ping", None, |_, _| async move { Ok(Some(json!("pong"))) });
    sandbox.register_tool(added.clone()).expect("register");
    assert!(sandbox.register_tool(added).is_err());
    assert_eq!(sandbox.tools().len(), 1);

    let res = sandbox
        .execute(
            r#"
            const total = await sumAll(10, 20, 30);
            const badge = await ui.badge({ label: "ok" });
            const pinged = await tools.ping();
            return { total, badge, pinged, frozen: Object.isFrozen(ui) };
            "#,
            None,
        )
        .await
        .expect("execute");

    match res {
        CodemodeResult::Success { value, calls, .. } => {
            assert_eq!(
                value,
                Some(json!({
                    "total": 60,
                    "badge": { "label": "ok" },
                    "pinged": "pong",
                    "frozen": true
                }))
            );
            // Globals are not recorded in `calls`.
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].name, "ping");
        }
        other => panic!("unexpected result: {other:?}"),
    }

    assert!(sandbox.unregister_tool("ping"));
    assert!(!sandbox.unregister_tool("ping"));
    assert!(sandbox.tools().is_empty());
}

#[tokio::test]
async fn captures_text_console_image_and_exit_output() {
    let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
    let sandbox = CodemodeSandbox::new(None).expect("sandbox");

    let res = sandbox
        .execute(
            &format!(
                r#"
                text("hello");
                text({{ count: 3 }});
                console.log("a", 2, {{ c: true }});
                image("data:image/jpeg;base64,{png_b64}");
                image({{ type: "image", data: "{png_b64}" }});
                store("saved", {{ ok: true }});
                exit();
                text("unreachable");
                return 999;
                "#
            ),
            None,
        )
        .await
        .expect("execute");

    match res {
        CodemodeResult::Success {
            ok,
            value,
            output,
            store_writes,
            ..
        } => {
            assert!(ok);
            assert_eq!(value, None);
            assert_eq!(
                output,
                vec![
                    CodemodeOutputItem::Text {
                        text: "hello".to_string()
                    },
                    CodemodeOutputItem::Text {
                        text: "{\"count\":3}".to_string()
                    },
                    CodemodeOutputItem::Text {
                        text: "a 2 {\"c\":true}".to_string()
                    },
                    CodemodeOutputItem::Image {
                        data: png_b64.to_string(),
                        mime_type: "image/png".to_string()
                    },
                    CodemodeOutputItem::Image {
                        data: png_b64.to_string(),
                        mime_type: "image/png".to_string()
                    },
                ]
            );
            assert_eq!(store_writes.set.get("saved"), Some(&json!({ "ok": true })));
        }
        other => panic!("unexpected result: {other:?}"),
    }

    for invalid_img in [
        "https://example.com/a.png",
        "data:image/png;utf8,hello",
        "data:image/png;base64,not_valid_base64!",
        "data:image/png;base64,AAAA",
    ] {
        let res = sandbox
            .execute(&format!("image({});", pi_js::json::stringify(invalid_img)), None)
            .await
            .expect("execute");
        match res {
            CodemodeResult::Failure { ok, error, .. } => {
                assert!(!ok);
                assert_eq!(error.kind, CodemodeErrorKind::Script);
            }
            other => panic!("expected failure for {invalid_img}, got: {other:?}"),
        }
    }
}

#[tokio::test]
async fn manages_store_load_writes_deletes_and_limits() {
    let sandbox = CodemodeSandbox::new(None).expect("sandbox");
    let mut initial = IndexMap::new();
    initial.insert("existing".to_string(), json!({ "n": 1 }));
    initial.insert("removeMe".to_string(), json!("bye"));

    let res = sandbox
        .execute(
            r#"
            const before = load("existing");
            const missing = load("absent");
            store("existing", { n: 2 });
            store("added", [1, 2]);
            store("removeMe", undefined);
            return {
                before,
                missingIsUndef: missing === undefined,
                after: load("existing"),
                goneIsUndef: load("removeMe") === undefined
            };
            "#,
            Some(CodemodeExecuteOptions {
                store: Some(initial),
                ..Default::default()
            }),
        )
        .await
        .expect("execute");

    match res {
        CodemodeResult::Success {
            value, store_writes, ..
        } => {
            assert_eq!(
                value,
                Some(json!({
                    "before": { "n": 1 },
                    "missingIsUndef": true,
                    "after": { "n": 2 },
                    "goneIsUndef": true
                }))
            );
            assert_eq!(store_writes.set.get("existing"), Some(&json!({ "n": 2 })));
            assert_eq!(store_writes.set.get("added"), Some(&json!([1, 2])));
            assert_eq!(store_writes.r#delete, vec!["removeMe".to_string()]);
        }
        other => panic!("unexpected result: {other:?}"),
    }

    let oversize = sandbox
        .execute(r#"store("big", "x".repeat(300000));"#, None)
        .await
        .expect("execute");
    match oversize {
        CodemodeResult::Failure { error, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Script);
            assert_eq!(error.name.as_deref(), Some("RangeError"));
        }
        other => panic!("expected RangeError, got: {other:?}"),
    }
}

#[tokio::test]
async fn reports_syntax_runtime_stalled_and_isolation_errors() {
    let sandbox = CodemodeSandbox::new(None).expect("sandbox");

    let syntax = sandbox.execute("function ( {", None).await.expect("execute");
    match syntax {
        CodemodeResult::Failure { error, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Script);
            assert_eq!(error.name.as_deref(), Some("SyntaxError"));
        }
        other => panic!("unexpected result: {other:?}"),
    }

    let runtime = sandbox
        .execute(r#"text("before"); throw new TypeError("bad input");"#, None)
        .await
        .expect("execute");
    match runtime {
        CodemodeResult::Failure { error, output, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Script);
            assert_eq!(error.name.as_deref(), Some("TypeError"));
            assert!(error.message.contains("bad input"));
            assert_eq!(
                output,
                vec![CodemodeOutputItem::Text {
                    text: "before".to_string()
                }]
            );
        }
        other => panic!("unexpected result: {other:?}"),
    }

    let stalled = sandbox
        .execute("await new Promise(() => {});", None)
        .await
        .expect("execute");
    match stalled {
        CodemodeResult::Failure { error, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Script);
            assert!(error.message.contains("can never settle"));
        }
        other => panic!("unexpected result: {other:?}"),
    }

    let isolation = sandbox
        .execute(
            r#"return {
                setTimeout: typeof setTimeout,
                fetch: typeof fetch,
                require: typeof require,
                process: typeof process
            };"#,
            None,
        )
        .await
        .expect("execute");
    match isolation {
        CodemodeResult::Success { value, .. } => {
            assert_eq!(
                value,
                Some(json!({
                    "setTimeout": "undefined",
                    "fetch": "undefined",
                    "require": "undefined",
                    "process": "undefined"
                }))
            );
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn handles_timeouts_aborts_memory_limit_and_close() {
    let abort_seen = Arc::new(AtomicUsize::new(0));
    let abort_seen_tool = Arc::clone(&abort_seen);
    let wait_forever = make_tool("wait_forever", None, move |_, ctx| {
        let abort_seen_tool = Arc::clone(&abort_seen_tool);
        async move {
            ctx.signal.cancelled().await;
            abort_seen_tool.fetch_add(1, Ordering::SeqCst);
            Err(pi_js::Error::msg("cancelled"))
        }
    });

    let sandbox = CodemodeSandbox::new(Some(CodemodeSandboxOptions {
        tools: Some(vec![wait_forever]),
        ..Default::default()
    }))
    .expect("sandbox");

    let timed_out = sandbox
        .execute(
            "while (true) {}",
            Some(CodemodeExecuteOptions {
                timeout_ms: Some(40.0),
                ..Default::default()
            }),
        )
        .await
        .expect("execute");
    match timed_out {
        CodemodeResult::Failure { error, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Timeout);
        }
        other => panic!("unexpected result: {other:?}"),
    }

    // Pre-aborted signal returns immediately.
    let pre = AbortController::new();
    pre.abort(Some(AbortReason::error("Error", "stop before start")));
    let pre_res = sandbox
        .execute(
            "return 1;",
            Some(CodemodeExecuteOptions {
                signal: Some(pre.signal()),
                ..Default::default()
            }),
        )
        .await
        .expect("execute");
    match pre_res {
        CodemodeResult::Failure { error, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Aborted);
            assert_eq!(error.message, "stop before start");
        }
        other => panic!("unexpected result: {other:?}"),
    }

    // Mid-execution abort cancels pending tool call.
    let controller = AbortController::new();
    let signal = controller.signal();
    pi_js::time::set_timeout(30, move || {
        controller.abort(Some(AbortReason::error("Error", "user cancelled")));
    });
    let aborted = sandbox
        .execute(
            "await tools.wait_forever();",
            Some(CodemodeExecuteOptions {
                signal: Some(signal),
                ..Default::default()
            }),
        )
        .await
        .expect("execute");
    match aborted {
        CodemodeResult::Failure { error, calls, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Aborted);
            assert_eq!(error.message, "user cancelled");
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].status, CodemodeCallStatus::Cancelled);
        }
        other => panic!("unexpected result: {other:?}"),
    }
    assert!(abort_seen.load(Ordering::SeqCst) >= 1);

    // Memory limit stops excessive allocation.
    let oom_sandbox = CodemodeSandbox::new(Some(CodemodeSandboxOptions {
        memory_limit_bytes: Some((1024 * 1024) as f64),
        ..Default::default()
    }))
    .expect("oom sandbox");
    let oom = oom_sandbox
        .execute("const b = new Uint8Array(2 * 1024 * 1024); return b.length;", None)
        .await
        .expect("execute");
    match oom {
        CodemodeResult::Failure { error, .. } => {
            assert!(error.message.to_lowercase().contains("out of memory"));
        }
        other => panic!("expected OOM failure, got: {other:?}"),
    }

    // Unawaited in-flight tool calls are cancelled when the script finishes.
    let before = abort_seen.load(Ordering::SeqCst);
    let unawaited = sandbox
        .execute("tools.wait_forever(); return 'finished early';", None)
        .await
        .expect("execute");
    match unawaited {
        CodemodeResult::Success { value, calls, .. } => {
            assert_eq!(value, Some(json!("finished early")));
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].name, "wait_forever");
            assert_eq!(calls[0].status, CodemodeCallStatus::Cancelled);
        }
        other => panic!("unexpected: {other:?}"),
    }
    let _ = pi_js::time::sleep(10, None).await;
    assert!(abort_seen.load(Ordering::SeqCst) > before);

    // Close aborts running executions and rejects new ones.
    let sandbox_close = sandbox.clone();
    let exec_task = tokio::spawn(async move { sandbox_close.execute("while (true) {}", None).await });
    let _ = pi_js::time::sleep(30, None).await;
    sandbox.close().await;
    let in_flight = exec_task.await.expect("join").expect("execute result");
    match in_flight {
        CodemodeResult::Failure { error, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Aborted);
            assert_eq!(error.message, "Sandbox closed");
        }
        other => panic!("unexpected result: {other:?}"),
    }
    assert!(sandbox.execute("return 1;", None).await.is_err());

    // Explicit compiled module and Pending wasm loader input work.
    let compiled = load_quick_js_wasm(None).await.expect("wasm");
    let mod_sandbox = CodemodeSandbox::new(Some(CodemodeSandboxOptions {
        wasm: Some(CodemodeWasmInput::Module(compiled)),
        ..Default::default()
    }))
    .expect("mod sandbox");
    match mod_sandbox.execute("return 'ok';", None).await.expect("ok") {
        CodemodeResult::Success { value, .. } => assert_eq!(value, Some(json!("ok"))),
        other => panic!("unexpected: {other:?}"),
    }

    let pending_sandbox = CodemodeSandbox::new(Some(CodemodeSandboxOptions {
        wasm: Some(CodemodeWasmInput::Pending(Arc::new(std::sync::Mutex::new(Some(
            Box::pin(async { load_quick_js_wasm(None).await }),
        ))))),
        ..Default::default()
    }))
    .expect("pending sandbox");
    match pending_sandbox.execute("return 7;", None).await.expect("ok") {
        CodemodeResult::Success { value, .. } => assert_eq!(value, Some(json!(7))),
        other => panic!("unexpected: {other:?}"),
    }
}
