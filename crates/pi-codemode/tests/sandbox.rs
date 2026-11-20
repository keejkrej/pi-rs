//! Port of packages/codemode/test/sandbox.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{Value, json};

use pi_codemode::runtime::execution::{CodemodeSandboxBase, RawVmMessage, VmChannel, VmEvents, VmLauncher};
use pi_codemode::runtime::host::is_watch_worker_message;
use pi_codemode::runtime::protocol::HostToWorkerMessage;
use pi_codemode::{
    CodemodeCallStatus, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeOutputItem, CodemodeRemote,
    CodemodeRemoteExchange, CodemodeRemoteMessage, CodemodeRemoteReply, CodemodeRemoteRequest, CodemodeResult,
    CodemodeSandbox, CodemodeSandboxBaseOptions, CodemodeSandboxOptions, CodemodeServeOptions, CodemodeTool,
    CodemodeWasmInput, InlineCodemodeSandbox, InlineCodemodeSandboxOptions, MAX_OUTPUT_CHARS, MAX_OUTPUT_ITEMS,
    RemoteCodemodeSandbox, RemoteCodemodeSandboxOptions, load_quick_js_wasm, serve_codemode_remote,
};
use pi_js::abort::{AbortController, AbortReason};

const INTERRUPT_BUDGET: usize = 20_000;
const PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
const JPEG_B64: &str = "/9j/4AAQSkZJRgABAQEASABIAAD/2wBDAAMCAgICAgMCAgIDAwMDBAYEBAQEBAgGBgUGCQgKCgkICQkKDA8MCgsOCwkJDRENDg8QEBEQCgwSExIQEw8QEBD/yQALCAABAAEBAREA/8wABgAQEAX/2gAIAQEAAD8A0s8g/9k=";
const GIF_B64: &str = "R0lGODlhAQABAAAAACw=";
const WEBP_B64: &str = "UklGRh4AAABXRUJQVlA4TBEAAAAvAAAAAAfQ//73v/+BiOh/AAA=";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    Worker,
    Inline,
    Remote,
}

impl Backend {
    const ALL: [Self; 3] = [Self::Worker, Self::Inline, Self::Remote];
}

#[derive(Clone)]
enum TestSandbox {
    Worker(CodemodeSandbox),
    Inline(InlineCodemodeSandbox),
    Remote(RemoteCodemodeSandbox),
}

struct LoopbackRemote;

#[async_trait]
impl CodemodeRemote for LoopbackRemote {
    async fn execute(&self, request: CodemodeRemoteRequest, exchange: CodemodeRemoteExchange) -> pi_js::Result<()> {
        let wasm = load_quick_js_wasm(None).await?;
        let req_wire: CodemodeRemoteRequest =
            serde_json::from_str(&pi_js::json::stringify(&request)).map_err(|e| pi_js::Error::msg(e.to_string()))?;
        let wrapped_exchange: CodemodeRemoteExchange = Arc::new(move |messages: Vec<CodemodeRemoteMessage>| {
            let exchange = Arc::clone(&exchange);
            Box::pin(async move {
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
        serve_codemode_remote(
            req_wire,
            wrapped_exchange,
            CodemodeServeOptions {
                wasm: CodemodeWasmInput::Module(wasm),
                max_interrupt_budget: Some(INTERRUPT_BUDGET),
                max_memory_limit_bytes: None,
            },
        )
        .await;
        Ok(())
    }
}

async fn new_sandbox(backend: Backend, options: CodemodeSandboxBaseOptions) -> pi_js::Result<TestSandbox> {
    match backend {
        Backend::Worker => CodemodeSandbox::new(Some(CodemodeSandboxOptions {
            tools: options.tools,
            globals: options.globals,
            timeout_ms: options.timeout_ms,
            memory_limit_bytes: options.memory_limit_bytes,
            wasm: None,
            worker_url: None,
        }))
        .map(TestSandbox::Worker),
        Backend::Inline => {
            let wasm = load_quick_js_wasm(None).await?;
            InlineCodemodeSandbox::new(InlineCodemodeSandboxOptions {
                tools: options.tools,
                globals: options.globals,
                timeout_ms: options.timeout_ms,
                memory_limit_bytes: options.memory_limit_bytes,
                wasm: CodemodeWasmInput::Module(wasm),
                interrupt_budget: Some(INTERRUPT_BUDGET),
            })
            .map(TestSandbox::Inline)
        }
        Backend::Remote => RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
            tools: options.tools,
            globals: options.globals,
            timeout_ms: options.timeout_ms,
            memory_limit_bytes: options.memory_limit_bytes,
            remote: Arc::new(|| Ok(Arc::new(LoopbackRemote) as Arc<dyn CodemodeRemote>)),
            interrupt_budget: Some(INTERRUPT_BUDGET),
        })
        .map(TestSandbox::Remote),
    }
}

impl TestSandbox {
    fn register_tool(&self, tool: CodemodeTool) -> pi_js::Result<()> {
        match self {
            Self::Worker(s) => s.register_tool(tool),
            Self::Inline(s) => s.register_tool(tool),
            Self::Remote(s) => s.register_tool(tool),
        }
    }

    fn unregister_tool(&self, name: &str) -> bool {
        match self {
            Self::Worker(s) => s.unregister_tool(name),
            Self::Inline(s) => s.unregister_tool(name),
            Self::Remote(s) => s.unregister_tool(name),
        }
    }

    fn tools(&self) -> Vec<CodemodeTool> {
        match self {
            Self::Worker(s) => s.tools(),
            Self::Inline(s) => s.tools(),
            Self::Remote(s) => s.tools(),
        }
    }

    fn globals(&self) -> Vec<CodemodeTool> {
        match self {
            Self::Worker(s) => s.globals(),
            Self::Inline(s) => s.globals(),
            Self::Remote(s) => s.globals(),
        }
    }

    async fn execute(&self, code: &str, options: Option<CodemodeExecuteOptions>) -> pi_js::Result<CodemodeResult> {
        match self {
            Self::Worker(s) => s.execute(code, options).await,
            Self::Inline(s) => s.execute(code, options).await,
            Self::Remote(s) => s.execute(code, options).await,
        }
    }

    async fn close(&self) {
        match self {
            Self::Worker(s) => s.close().await,
            Self::Inline(s) => s.close().await,
            Self::Remote(s) => s.close().await,
        }
    }
}

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
    for backend in Backend::ALL {
        let sandbox = new_sandbox(backend, CodemodeSandboxBaseOptions::default())
            .await
            .expect("sandbox");

        match sandbox.execute("return 20 + 22;", None).await.expect("execute") {
            CodemodeResult::Success { ok, value, .. } => {
                assert!(ok);
                assert_eq!(value, Some(json!(42)));
            }
            other => panic!("unexpected result: {other:?}"),
        }

        match sandbox.execute("const x = 1;", None).await.expect("execute") {
            CodemodeResult::Success { ok, value, .. } => {
                assert!(ok);
                assert_eq!(value, None);
            }
            other => panic!("unexpected result: {other:?}"),
        }

        match sandbox.execute("return null;", None).await.expect("execute") {
            CodemodeResult::Success { ok, value, .. } => {
                assert!(ok);
                assert_eq!(value, Some(Value::Null));
            }
            other => panic!("unexpected result: {other:?}"),
        }

        match sandbox
            .execute(
                "const v = await Promise.resolve({ a: 1, b: [true, 'x'] }); return v;",
                None,
            )
            .await
            .expect("execute")
        {
            CodemodeResult::Success { ok, value, .. } => {
                assert!(ok);
                assert_eq!(value, Some(json!({ "a": 1, "b": [true, "x"] })));
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }
}

#[tokio::test]
async fn exposes_tools_all_tools_and_call_tracking() {
    for backend in Backend::ALL {
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

        let sandbox = new_sandbox(
            backend,
            CodemodeSandboxBaseOptions {
                tools: Some(vec![echo, check_none, slow_first, fast_second, fail_tool]),
                ..Default::default()
            },
        )
        .await
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
}

#[tokio::test]
async fn manages_tools_and_validates_globals() {
    for backend in Backend::ALL {
        let t1 = make_tool("dup", None, |_, _| async move { Ok(None) });
        assert!(
            new_sandbox(
                backend,
                CodemodeSandboxBaseOptions {
                    tools: Some(vec![t1.clone(), t1]),
                    ..Default::default()
                }
            )
            .await
            .is_err()
        );

        for bad in [
            "123bad",
            "not-valid",
            "a.b.c",
            "a.",
            ".a",
            "a.not-valid",
            "tools",
            "tools.x",
            "store",
            "store.x",
            "load",
            "console",
        ] {
            let bad_global = make_tool(bad, None, |_, _| async move { Ok(None) });
            let err = new_sandbox(
                backend,
                CodemodeSandboxBaseOptions {
                    globals: Some(vec![bad_global]),
                    ..Default::default()
                },
            )
            .await
            .err()
            .expect("expected invalid global error");
            assert_eq!(err.message(), format!("Invalid global name \"{bad}\""), "name={bad}");
        }

        let dup_g = make_tool("helper", None, |_, _| async move { Ok(None) });
        let dup_err = new_sandbox(
            backend,
            CodemodeSandboxBaseOptions {
                globals: Some(vec![dup_g.clone(), dup_g]),
                ..Default::default()
            },
        )
        .await
        .err()
        .expect("expected duplicate global error");
        assert_eq!(dup_err.message(), "Global \"helper\" is already registered");

        let ns_flat = make_tool("models", None, |_, _| async move { Ok(None) });
        let ns_member = make_tool("models.list", None, |_, _| async move { Ok(None) });
        let conflict_err = new_sandbox(
            backend,
            CodemodeSandboxBaseOptions {
                globals: Some(vec![ns_flat, ns_member]),
                ..Default::default()
            },
        )
        .await
        .err()
        .expect("expected namespace conflict");
        assert_eq!(
            conflict_err.message(),
            "Global \"models\" conflicts with the namespace \"models\""
        );

        let mut g_spread = make_tool("sumAll", None, |args, _| async move {
            let arr = args.and_then(|v| v.as_array().cloned()).unwrap_or_default();
            let sum: i64 = arr.iter().filter_map(|v| v.as_i64()).sum();
            Ok(Some(json!(sum)))
        });
        g_spread.spread = Some(true);

        let g_ns = make_tool("ui.badge", None, |args, _| async move { Ok(args) });

        let sandbox = new_sandbox(
            backend,
            CodemodeSandboxBaseOptions {
                globals: Some(vec![g_spread, g_ns]),
                ..Default::default()
            },
        )
        .await
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
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "ping");
            }
            other => panic!("unexpected result: {other:?}"),
        }

        assert!(sandbox.unregister_tool("ping"));
        assert!(!sandbox.unregister_tool("ping"));
        assert!(sandbox.tools().is_empty());
    }
}

#[tokio::test]
async fn captures_text_console_image_and_exit_output() {
    for backend in Backend::ALL {
        let sandbox = new_sandbox(backend, CodemodeSandboxBaseOptions::default())
            .await
            .expect("sandbox");

        let res = sandbox
            .execute(
                &format!(
                    r#"
                    console.log("hello", 1, {{ a: 1 }});
                    text({{ json: true }});
                    text(undefined);
                    text(7);
                    image("data:image/png;base64,{PNG_B64}");
                    image({{ image_url: "data:image/jpeg;base64,{JPEG_B64}" }});
                    image({{ type: "image", data: "{GIF_B64}", mimeType: "image/gif" }});
                    image("data:image/png;base64,{WEBP_B64}");
                    image({{ type: "image", data: "{PNG_B64}" }});
                    console.error(new Error("bad"));
                    return null;
                    "#
                ),
                None,
            )
            .await
            .expect("execute");

        assert!(res.is_ok(), "unexpected result: {res:?}");
        assert_eq!(
            &res.output()[..res.output().len() - 1],
            &[
                CodemodeOutputItem::Text {
                    text: r#"hello 1 {"a":1}"#.to_string(),
                },
                CodemodeOutputItem::Text {
                    text: r#"{"json":true}"#.to_string(),
                },
                CodemodeOutputItem::Text {
                    text: "undefined".to_string(),
                },
                CodemodeOutputItem::Text { text: "7".to_string() },
                CodemodeOutputItem::Image {
                    data: PNG_B64.to_string(),
                    mime_type: "image/png".to_string(),
                },
                CodemodeOutputItem::Image {
                    data: JPEG_B64.to_string(),
                    mime_type: "image/jpeg".to_string(),
                },
                CodemodeOutputItem::Image {
                    data: GIF_B64.to_string(),
                    mime_type: "image/gif".to_string(),
                },
                CodemodeOutputItem::Image {
                    data: WEBP_B64.to_string(),
                    mime_type: "image/webp".to_string(),
                },
                CodemodeOutputItem::Image {
                    data: PNG_B64.to_string(),
                    mime_type: "image/png".to_string(),
                },
            ]
        );
        assert!(res.is_console_output(0));
        assert!(!res.is_console_output(1));
        let last_idx = res.output().len() - 1;
        assert!(res.is_console_output(last_idx));
        let CodemodeOutputItem::Text { text } = &res.output()[last_idx] else {
            panic!("expected last console error item");
        };
        assert!(text.starts_with("Error: bad"));

        // #11901
        let logged = sandbox
            .execute(
                r#"console.log("avg", 0 / 0, 1 / 0, -1 / 0, { a: 1 }); text(0 / 0);"#,
                None,
            )
            .await
            .expect("execute");
        assert!(logged.is_ok());
        assert_eq!(
            logged.output(),
            &[
                CodemodeOutputItem::Text {
                    text: r#"avg NaN Infinity -Infinity {"a":1}"#.to_string()
                },
                CodemodeOutputItem::Text {
                    text: "NaN".to_string()
                },
            ]
        );
        assert!(logged.is_console_output(0));
        assert!(!logged.is_console_output(1));

        match sandbox.execute("throw NaN", None).await.expect("execute") {
            CodemodeResult::Failure { error, .. } => {
                assert_eq!(error.kind, CodemodeErrorKind::Script);
                assert_eq!(error.message, "NaN");
            }
            other => panic!("expected throw NaN failure, got {other:?}"),
        }

        // Reject invalid text() and image() arguments.
        let invalid_res = sandbox
            .execute(
                r#"
                const errors = [];
                const circular = {};
                circular.self = circular;
                for (const run of [
                    () => text(circular),
                    () => image(""),
                    () => image("https://example.com/a.png"),
                    () => image("data:image/png,raw"),
                    () => image({ type: "text", text: "x" }),
                    () => image({ type: "image", data: "" }),
                    () => image(42),
                    () => image("data:image/png;base64,AAAA!"),
                    () => image("data:image/png;base64,AAAAA"),
                    () => image("data:image/png;base64,AA=A"),
                    () => image("data:image/png;base64,"),
                    () => image("data:image/png;base64,AAAA\n[Output truncated]"),
                    () => image({ type: "image", data: "AAAA!", mimeType: "image/png" }),
                    () => image("data:image/png;base64,AAAA"),
                    () => image("data:image/png;base64,QUJD"),
                    () => image("data:image/jpeg;base64,/9j/9w=="),
                ]) {
                    try { run(); errors.push("no error"); } catch (error) { errors.push(error.name + ": " + error.message); }
                }
                return errors;
                "#,
                None,
            )
            .await
            .expect("execute");
        let CodemodeResult::Success {
            value: Some(Value::Array(errors)),
            output,
            ..
        } = invalid_res
        else {
            panic!("expected invalid errors list");
        };
        assert!(output.is_empty());
        assert!(errors[0].as_str().unwrap().contains("circular"));
        assert_eq!(
            errors[1],
            json!(
                "TypeError: image expects a non-empty image URL string, an object with image_url, or an image block { type: \"image\", data, mimeType }"
            )
        );
        assert_eq!(
            errors[2],
            json!("TypeError: remote image URLs are not supported in tool outputs. Pass a base64 data URI instead")
        );
        assert_eq!(
            errors[3],
            json!("TypeError: invalid image output. Pass a base64 data URI instead")
        );
        assert_eq!(
            errors[4],
            json!("TypeError: image only accepts image blocks, got \"text\"")
        );
        assert_eq!(errors[5], json!("TypeError: image expected image block data"));
        assert_eq!(
            errors[6],
            json!(
                "TypeError: image expects a non-empty image URL string, an object with image_url, or an image block { type: \"image\", data, mimeType }"
            )
        );
        for err in &errors[7..13] {
            assert_eq!(
                err,
                &json!("TypeError: invalid image output. The image data is not valid base64 (truncated or corrupted?)")
            );
        }
        for err in &errors[13..16] {
            assert_eq!(
                err,
                &json!("TypeError: invalid image output. The image data is not a PNG, JPEG, GIF, or WebP image")
            );
        }

        // #10215 wrapped & large base64 images.
        let large = format!("iVBORw0KGgoA{}", "QUJD".repeat(256 * 1024));
        let wrapped_res = sandbox
            .execute(
                &format!(
                    "image(\"data:image/png;base64,iVBORw0K\\r\\nGgo=\\n\"); image(\"data:image/png;base64,{large}\");"
                ),
                None,
            )
            .await
            .expect("execute");
        assert!(wrapped_res.is_ok());
        assert_eq!(
            wrapped_res.output(),
            &[
                CodemodeOutputItem::Image {
                    data: "iVBORw0KGgo=".to_string(),
                    mime_type: "image/png".to_string(),
                },
                CodemodeOutputItem::Image {
                    data: large,
                    mime_type: "image/png".to_string(),
                },
            ]
        );

        // exit() keeps output and store writes.
        let exit_sb = new_sandbox(
            backend,
            CodemodeSandboxBaseOptions {
                tools: Some(vec![make_tool("echo", None, |args, _| async move { Ok(args) })]),
                ..Default::default()
            },
        )
        .await
        .expect("exit sandbox");
        let exited = exit_sb
            .execute(
                r#"
                text("before");
                store("k", 1);
                await tools.echo(1);
                try { exit(); } catch {}
                text("after");
                return "unreachable";
                "#,
                None,
            )
            .await
            .expect("execute");
        match exited {
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
                    vec![CodemodeOutputItem::Text {
                        text: "before".to_string()
                    }]
                );
                assert_eq!(store_writes.set.get("k"), Some(&json!(1)));
                assert!(store_writes.r#delete.is_empty());
            }
            other => panic!("unexpected exit result: {other:?}"),
        }
    }
}

#[tokio::test]
async fn manages_store_load_writes_deletes_and_limits() {
    for backend in Backend::ALL {
        let sandbox = new_sandbox(backend, CodemodeSandboxBaseOptions::default())
            .await
            .expect("sandbox");
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
}

#[tokio::test]
async fn reports_syntax_runtime_stalled_and_isolation_errors() {
    for backend in Backend::ALL {
        let sandbox = new_sandbox(backend, CodemodeSandboxBaseOptions::default())
            .await
            .expect("sandbox");

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
}

#[tokio::test]
async fn handles_timeouts_aborts_output_limits_and_close() {
    for backend in Backend::ALL {
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
        let delayed_wait = make_tool("wait", None, |_, _| async move {
            let _ = pi_js::time::sleep(50, None).await;
            Ok(Some(json!("late")))
        });

        let sandbox = new_sandbox(
            backend,
            CodemodeSandboxBaseOptions {
                tools: Some(vec![wait_forever, delayed_wait]),
                ..Default::default()
            },
        )
        .await
        .expect("sandbox");

        for spin in ["while (true) {}", "while (true) await null"] {
            let timed_out = sandbox
                .execute(
                    spin,
                    Some(CodemodeExecuteOptions {
                        timeout_ms: Some(200.0),
                        ..Default::default()
                    }),
                )
                .await
                .expect("execute");
            match timed_out {
                CodemodeResult::Failure { error, .. } => {
                    assert_eq!(error.kind, CodemodeErrorKind::Timeout, "spin={spin}");
                }
                other => panic!("unexpected timeout result for {spin}: {other:?}"),
            }
        }

        let no_deadline = sandbox
            .execute(
                "return await tools.wait()",
                Some(CodemodeExecuteOptions {
                    timeout_ms: Some(f64::INFINITY),
                    ..Default::default()
                }),
            )
            .await
            .expect("execute");
        match no_deadline {
            CodemodeResult::Success { value, .. } => assert_eq!(value, Some(json!("late"))),
            other => panic!("unexpected no_deadline result: {other:?}"),
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

        // #10283: output limits fail script even if caught.
        for print in ["text(s)", "console.log(s)", "image(\"data:image/png;base64,\" + p)"] {
            let over = sandbox
                .execute(
                    &format!(
                        "const s = \"x\".repeat(1 << 20); const p = \"iVBORw0KGgoA\" + \"A\".repeat(1 << 20); for (;;) {{ try {{ {print}; }} catch {{}} }}"
                    ),
                    None,
                )
                .await
                .expect("execute");
            match &over {
                CodemodeResult::Failure { error, .. } => {
                    assert_eq!(error.kind, CodemodeErrorKind::Script, "print={print}");
                    assert_eq!(error.name.as_deref(), Some("RangeError"), "print={print}");
                    assert!(error.message.contains("script output exceeded"));
                }
                other => panic!("expected output limit failure for {print}, got {other:?}"),
            }
            let total_chars: i64 = over
                .output()
                .iter()
                .map(|item| match item {
                    CodemodeOutputItem::Text { text } => text.len() as i64,
                    CodemodeOutputItem::Image { data, .. } => data.len() as i64,
                })
                .sum();
            assert!(total_chars <= MAX_OUTPUT_CHARS);
            assert!(total_chars > MAX_OUTPUT_CHARS - (2 << 20));
        }

        let empty_items = sandbox.execute("for (;;) text('');", None).await.expect("execute");
        match &empty_items {
            CodemodeResult::Failure { error, output, .. } => {
                assert_eq!(error.name.as_deref(), Some("RangeError"));
                assert_eq!(output.len() as i64, MAX_OUTPUT_ITEMS);
            }
            other => panic!("expected empty items limit failure, got {other:?}"),
        }

        // Unawaited in-flight tool calls are cancelled when script returns.
        let before = abort_seen.load(Ordering::SeqCst);
        let unawaited = sandbox
            .execute("tools.wait_forever(); return 'finished early';", None)
            .await
            .expect("execute");
        match unawaited {
            CodemodeResult::Success { value, calls, .. } => {
                assert_eq!(value, Some(json!("finished early")));
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].status, CodemodeCallStatus::Cancelled);
            }
            other => panic!("unexpected result: {other:?}"),
        }
        let _ = pi_js::time::sleep(10, None).await;
        assert!(abort_seen.load(Ordering::SeqCst) > before);

        // Close aborts running execution and rejects new ones.
        let sandbox_close = sandbox.clone();
        let exec_task = tokio::spawn(async move { sandbox_close.execute("await tools.wait_forever();", None).await });
        let _ = pi_js::time::sleep(30, None).await;
        sandbox.close().await;
        match exec_task.await.expect("join").expect("execute result") {
            CodemodeResult::Failure { error, .. } => {
                assert_eq!(error.kind, CodemodeErrorKind::Aborted);
                assert_eq!(error.message, "Sandbox closed");
            }
            other => panic!("unexpected close result: {other:?}"),
        }
        assert!(sandbox.execute("return 1;", None).await.is_err());

        // Parallel executions do not share state.
        let par_sandbox = new_sandbox(backend, CodemodeSandboxBaseOptions::default())
            .await
            .expect("par sandbox");
        let (r1, r2, r3) = tokio::join!(
            par_sandbox.execute("globalThis.shared = 'a'; await null; return globalThis.shared", None),
            par_sandbox.execute("globalThis.shared = 'b'; await null; return globalThis.shared", None),
            par_sandbox.execute("return typeof globalThis.shared", None),
        );
        let vals: Vec<Value> = [r1, r2, r3]
            .into_iter()
            .map(|r| match r.expect("ok") {
                CodemodeResult::Success { value: Some(v), .. } => v,
                other => panic!("unexpected parallel result: {other:?}"),
            })
            .collect();
        assert_eq!(vals, vec![json!("a"), json!("b"), json!("undefined")]);
    }
}

#[tokio::test]
async fn built_ins_lockdown_and_escape_hatch_checks() {
    for backend in Backend::ALL {
        let echo = make_tool("echo", None, |args, _| async move { Ok(args) });
        let sandbox = new_sandbox(
            backend,
            CodemodeSandboxBaseOptions {
                tools: Some(vec![echo]),
                ..Default::default()
            },
        )
        .await
        .expect("sandbox");

        // #10444: ignores patches to built-ins and built-in globals.
        let patched = sandbox
            .execute(
                r#"
                Array.prototype.toJSON = () => null;
                Object.prototype.toJSON = () => 5;
                Promise.prototype.then = () => {};
                Map.prototype.get = () => undefined;
                globalThis.JSON = { stringify: () => "x", parse: () => "x" };
                store("k", [1]);
                return [await tools.echo([2]), JSON.stringify({ a: 1 })];
                "#,
                None,
            )
            .await
            .expect("execute");
        match patched {
            CodemodeResult::Success {
                value, store_writes, ..
            } => {
                assert_eq!(value, Some(json!([[2], "{\"a\":1}"])));
                assert_eq!(store_writes.set.get("k"), Some(&json!([1])));
            }
            other => panic!("unexpected patched result: {other:?}"),
        }

        // Freezes intrinsics only reachable from instances.
        let frozen_inst = sandbox
            .execute(
                r#"
                return [
                    Object.getPrototypeOf(function* () {}).prototype,
                    Object.getPrototypeOf(async function () {}),
                    Object.getPrototypeOf(Int8Array).prototype,
                    Object.getPrototypeOf([][Symbol.iterator]()),
                    Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]())),
                    Object.getPrototypeOf(new Map()[Symbol.iterator]()),
                    Object.getPrototypeOf(/a/[Symbol.matchAll]("")),
                ].every((object) => Object.isFrozen(object));
                "#,
                None,
            )
            .await
            .expect("execute");
        match frozen_inst {
            CodemodeResult::Success { value, .. } => assert_eq!(value, Some(json!(true))),
            other => panic!("unexpected frozen_inst result: {other:?}"),
        }

        // Override mistake: instances still override properties on frozen prototypes.
        let override_res = sandbox
            .execute(
                r#"
                const object = {};
                object.toString = () => "custom";
                function Legacy() {}
                Legacy.prototype = Object.create(Error.prototype);
                Legacy.prototype.constructor = Legacy;
                const bare = new Error();
                bare.message = "set later";
                class MyError extends Error {
                    constructor(message) {
                        super(message);
                        this.name = "MyError";
                    }
                }
                let patched = "silent";
                try { Error.prototype.name = "Patched"; } catch (error) { patched = error.constructor.name; }
                return [String(object), new Legacy().constructor === Legacy, bare.message, new MyError("x").name, Error.prototype.name, patched];
                "#,
                None,
            )
            .await
            .expect("execute");
        match override_res {
            CodemodeResult::Success { value, .. } => {
                assert_eq!(
                    value,
                    Some(json!(["custom", true, "set later", "MyError", "Error", "TypeError"]))
                );
            }
            other => panic!("unexpected override result: {other:?}"),
        }

        // Reports errors whose name or message is not a string.
        let non_str_err = sandbox
            .execute("const error = new Error('x'); error.message = 42; throw error;", None)
            .await
            .expect("execute");
        match non_str_err {
            CodemodeResult::Failure { error, .. } => {
                assert_eq!(error.kind, CodemodeErrorKind::Script);
                assert_eq!(error.name.as_deref(), Some("Error"));
                assert_eq!(error.message, "42");
            }
            other => panic!("unexpected non_str_err result: {other:?}"),
        }

        // Deep recursion -> RangeError.
        let recursion = sandbox
            .execute(
                r#"
                let depth = 0;
                function dive() { depth++; dive(); }
                try { dive(); } catch (error) { return [error.name, depth > 1000]; }
                "#,
                None,
            )
            .await
            .expect("execute");
        match recursion {
            CodemodeResult::Success { value, .. } => {
                assert_eq!(value, Some(json!(["RangeError", true])));
            }
            other => panic!("unexpected recursion result: {other:?}"),
        }

        // Escape hatches: no host globals, eval stays inside VM, dynamic import fails, tools/console frozen.
        let hatch = sandbox
            .execute(
                r#"
                return [
                    typeof process, typeof require, typeof module, typeof setTimeout, typeof fetch,
                    typeof WebAssembly, typeof std, typeof os, typeof globalThis.constructor,
                    eval("typeof process"),
                    new Function("return typeof process")(),
                    tools.echo.constructor("return typeof require")(),
                    (async () => {}).constructor("return typeof setTimeout")() instanceof Promise,
                ];
                "#,
                None,
            )
            .await
            .expect("execute");
        match hatch {
            CodemodeResult::Success { value, .. } => {
                assert_eq!(
                    value,
                    Some(json!([
                        "undefined",
                        "undefined",
                        "undefined",
                        "undefined",
                        "undefined",
                        "undefined",
                        "undefined",
                        "undefined",
                        "function",
                        "undefined",
                        "undefined",
                        "undefined",
                        true
                    ]))
                );
            }
            other => panic!("unexpected hatch result: {other:?}"),
        }
    }
}

#[tokio::test]
async fn worker_host_wasm_failure_watch_filter_and_broken_bridge() {
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

    let failing_wasm = CodemodeSandbox::new(Some(CodemodeSandboxOptions {
        wasm: Some(CodemodeWasmInput::Pending(Arc::new(std::sync::Mutex::new(Some(
            Box::pin(async { Err(pi_js::Error::msg("no wasm")) }),
        ))))),
        ..Default::default()
    }))
    .expect("sandbox");
    match failing_wasm.execute("return 1;", None).await.expect("execute") {
        CodemodeResult::Failure { error, .. } => {
            assert_eq!(error.kind, CodemodeErrorKind::Sandbox);
            assert_eq!(error.message, "Failed to load QuickJS: no wasm");
        }
        other => panic!("unexpected result: {other:?}"),
    }

    // #10725: Node `--watch` notifications are ignored by host filter.
    assert!(is_watch_worker_message(&json!({ "watch:require": ["/worker.js"] })));
    assert!(is_watch_worker_message(
        &json!({ "watch:import": ["file:///worker.js"] })
    ));
    assert!(!is_watch_worker_message(
        &json!({ "type": "done", "ok": true, "value": "42", "writes": "[]" })
    ));

    struct NoopCh(Arc<AtomicBool>);
    impl VmChannel for NoopCh {
        fn post(&self, _: HostToWorkerMessage) {}
        fn stop(&self) -> pi_js::BoxFuture<()> {
            self.0.store(true, Ordering::SeqCst);
            Box::pin(async {})
        }
    }
    let watch_launcher: VmLauncher = Arc::new(|_data, events: VmEvents| {
        Box::pin(async move {
            for msg in [
                json!({ "watch:require": ["/worker.js"] }),
                json!({ "watch:import": ["file:///worker.js"] }),
                json!({ "type": "done", "ok": true, "value": "42", "writes": "[]" }),
            ] {
                if !is_watch_worker_message(&msg) {
                    (events.message)(RawVmMessage::Json(msg));
                }
            }
            Ok(Arc::new(NoopCh(Arc::new(AtomicBool::new(false)))) as Arc<dyn VmChannel>)
        })
    });
    let watch_sb = CodemodeSandboxBase::new(CodemodeSandboxBaseOptions::default(), watch_launcher).unwrap();
    match watch_sb.execute("", None).await.expect("execute") {
        CodemodeResult::Success { value, .. } => assert_eq!(value, Some(json!(42))),
        other => panic!("expected 42, got {other:?}"),
    }

    // #10444: a malformed payload from the bridge fails the run cleanly as a sandbox error.
    let cases = [
        (
            json!({ "type": "done", "ok": true, "value": "1", "writes": "null" }),
            "store writes are not an array",
        ),
        (
            json!({ "type": "done", "ok": true, "value": "1", "writes": "[1]" }),
            "store writes contain a malformed entry",
        ),
        (
            json!({ "type": "done", "ok": true, "value": "1", "writes": "[[\"k\", \"{\"]]" }),
            "store value for \"k\" is not valid JSON",
        ),
        (
            json!({ "type": "done", "ok": true, "value": "{", "writes": "[]" }),
            "return value is not valid JSON",
        ),
        (
            json!({ "type": "done", "ok": false, "error": "5" }),
            "script error is not an object",
        ),
        (
            json!({ "type": "done", "ok": false, "error": "{}" }),
            "script error is malformed",
        ),
        (json!({ "type": "nonsense" }), "unknown message from the worker"),
    ];

    struct MalformedRemote(Value);
    #[async_trait]
    impl CodemodeRemote for MalformedRemote {
        async fn execute(&self, _: CodemodeRemoteRequest, exchange: CodemodeRemoteExchange) -> pi_js::Result<()> {
            let _ = exchange(vec![CodemodeRemoteMessage::Raw(self.0.clone())]).await;
            Ok(())
        }
    }

    for (payload, expected_reason) in cases {
        let p = payload.clone();
        let sb = RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
            tools: None,
            globals: None,
            timeout_ms: None,
            memory_limit_bytes: None,
            remote: Arc::new(move || Ok(Arc::new(MalformedRemote(p.clone())) as Arc<dyn CodemodeRemote>)),
            interrupt_budget: None,
        })
        .expect("sb");
        match sb.execute("return 1", None).await.expect("execute") {
            CodemodeResult::Failure { error, .. } => {
                assert_eq!(error.kind, CodemodeErrorKind::Sandbox);
                let expected = format!("Sandbox bridge broken: {expected_reason}");
                assert!(
                    error.message.contains(&expected),
                    "payload={payload}: expected {:?} in {:?}",
                    expected,
                    error.message
                );
            }
            other => panic!("expected broken bridge error for {payload}, got {other:?}"),
        }
    }
}
