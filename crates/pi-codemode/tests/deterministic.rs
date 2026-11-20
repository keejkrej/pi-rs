//! Port of packages/codemode/test/deterministic.test.ts

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{Value, json};
use serial_test::serial;

use pi_codemode::{
    CodemodeCallStatus, CodemodeCallTarget, CodemodeDeclaredGlobal, CodemodeDeclaredTool, CodemodeDuring,
    CodemodeErrorKind, CodemodeHostCall, CodemodeOutcome, CodemodeOutputItem, CodemodeRemote, CodemodeRemoteExchange,
    CodemodeRemoteMessage, CodemodeRemoteReply, CodemodeRemoteRequest, CodemodeResult, CodemodeRun, CodemodeRunOptions,
    CodemodeSandbox, CodemodeSandboxBaseOptions, CodemodeSandboxOptions, CodemodeServeOptions, CodemodeWasmInput,
    RemoteCodemodeSandbox, RemoteCodemodeSandboxOptions, load_quick_js_wasm, serve_codemode_remote,
};
use pi_js::abort::{AbortController, AbortReason};

const BUDGET: usize = 50_000;
const STARTED_AT: f64 = 1_768_471_200_000.0; // Date.UTC(2026, 0, 15, 10, 0, 0)
const PINNED_SEED_ZERO: [f64; 2] = [0.6711372530266764, 0.7258461452833667];
const PINNED_FIRST_RANDOM: f64 = 0.34227450605335275;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DetBackend {
    Worker,
    Remote,
}

#[derive(Clone)]
enum DetSandbox {
    Worker(CodemodeSandbox),
    Remote(RemoteCodemodeSandbox),
}

impl DetSandbox {
    async fn execute(&self, code: &str) -> CodemodeResult {
        match self {
            Self::Worker(s) => s.execute(code, None).await.expect("execute"),
            Self::Remote(s) => s.execute(code, None).await.expect("execute"),
        }
    }

    fn start(&self, code: &str, options: CodemodeRunOptions) -> pi_js::Result<CodemodeRun> {
        match self {
            Self::Worker(s) => s.start(code, options),
            Self::Remote(s) => s.start(code, options),
        }
    }
}

struct DetRemote {
    delay_first: bool,
}

#[async_trait]
impl CodemodeRemote for DetRemote {
    async fn execute(&self, request: CodemodeRemoteRequest, exchange: CodemodeRemoteExchange) -> pi_js::Result<()> {
        let wasm = load_quick_js_wasm(None).await?;
        let delay_first = self.delay_first;
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
                if delay_first && let Some(CodemodeRemoteReply::Result { seq: Some(1), .. }) = &cloned_reply {
                    let _ = pi_js::time::sleep(30, None).await;
                }
                Ok(cloned_reply)
            })
        });
        let req_wire: CodemodeRemoteRequest =
            serde_json::from_str(&pi_js::json::stringify(&request)).map_err(|e| pi_js::Error::msg(e.to_string()))?;
        serve_codemode_remote(
            req_wire,
            wrapped_exchange,
            CodemodeServeOptions {
                wasm: CodemodeWasmInput::Module(wasm),
                max_interrupt_budget: Some(BUDGET),
                max_memory_limit_bytes: None,
            },
        )
        .await;
        Ok(())
    }
}

fn create_det_sandbox(backend: DetBackend, base: CodemodeSandboxBaseOptions, delay_first: bool) -> DetSandbox {
    match backend {
        DetBackend::Worker => DetSandbox::Worker(
            CodemodeSandbox::new(Some(CodemodeSandboxOptions {
                tools: base.tools,
                globals: base.globals,
                timeout_ms: base.timeout_ms,
                memory_limit_bytes: base.memory_limit_bytes,
                wasm: None,
                worker_url: None,
            }))
            .expect("worker sandbox"),
        ),
        DetBackend::Remote => DetSandbox::Remote(
            RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
                tools: base.tools,
                globals: base.globals,
                timeout_ms: base.timeout_ms,
                memory_limit_bytes: base.memory_limit_bytes,
                remote: Arc::new(move || Ok(Arc::new(DetRemote { delay_first }) as Arc<dyn CodemodeRemote>)),
                interrupt_budget: None,
            })
            .expect("remote sandbox"),
        ),
    }
}

struct Recorded {
    run: CodemodeRun,
    calls: Arc<Mutex<Vec<CodemodeHostCall>>>,
    idles: Arc<Mutex<Vec<(i64, i64)>>>,
    events: Arc<Mutex<Vec<String>>>,
    fingerprint: Arc<Mutex<Option<String>>>,
}

fn start_with(
    sandbox: &DetSandbox,
    code: &str,
    started_at: f64,
    seed: u32,
    tools: Option<Vec<CodemodeDeclaredTool>>,
    globals: Option<Vec<CodemodeDeclaredGlobal>>,
    signal: Option<pi_js::abort::AbortSignal>,
    answer: impl Fn(&CodemodeHostCall) -> CodemodeOutcome + Send + Sync + 'static,
) -> Recorded {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let idles = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::new(Mutex::new(Vec::new()));
    let fingerprint = Arc::new(Mutex::new(None));
    let seq = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let answer = Arc::new(answer);

    let fp_cb = Arc::clone(&fingerprint);
    let ev_started = Arc::clone(&events);
    let calls_cb = Arc::clone(&calls);
    let ev_call = Arc::clone(&events);
    let seq_cb = Arc::clone(&seq);
    let idles_cb = Arc::clone(&idles);
    let ev_idle = Arc::clone(&events);

    let default_tools = vec![
        CodemodeDeclaredTool {
            name: "echo".to_string(),
            js_name: None,
            description: None,
        },
        CodemodeDeclaredTool {
            name: "wait".to_string(),
            js_name: None,
            description: None,
        },
    ];

    let run = sandbox
        .start(
            code,
            CodemodeRunOptions {
                started_at,
                seed,
                tools: Some(tools.unwrap_or(default_tools)),
                globals,
                store: None,
                signal,
                timeout_ms: Some(20_000.0),
                on_started: Some(Arc::new(move |value| {
                    *fp_cb.lock().unwrap() = Some(value);
                    ev_started.lock().unwrap().push("started".to_string());
                })),
                on_call: Arc::new(move |call, run| {
                    calls_cb.lock().unwrap().push(call.clone());
                    ev_call.lock().unwrap().push(format!("call {}", call.id));
                    if call.name == "wait" {
                        return;
                    }
                    let next_seq = seq_cb.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    let outcome = answer(&call);
                    let call_id = call.id;
                    tokio::spawn(async move {
                        let _ = run.deliver(next_seq, started_at + 1000.0 * (next_seq as f64), call_id, outcome);
                    });
                }),
                on_idle: Some(Arc::new(move |seq_val, issued, _run| {
                    idles_cb.lock().unwrap().push((seq_val, issued));
                    ev_idle.lock().unwrap().push(format!("idle {seq_val}"));
                })),
            },
        )
        .expect("start");

    Recorded {
        run,
        calls,
        idles,
        events,
        fingerprint,
    }
}

fn default_echo_answer(call: &CodemodeHostCall) -> CodemodeOutcome {
    CodemodeOutcome::ok(call.args.clone())
}

async fn wait_for(check: impl Fn() -> bool) {
    for _ in 0..1000 {
        if check() {
            return;
        }
        let _ = pi_js::time::sleep(5, None).await;
    }
    panic!("Condition was not reached");
}

fn strip_call_durations(result: CodemodeResult) -> Value {
    match result {
        CodemodeResult::Success {
            ok,
            value,
            output,
            calls,
            store_writes,
            during,
            ..
        } => json!({
            "ok": ok,
            "value": value,
            "output": output,
            "calls": calls.into_iter().map(|c| json!({ "name": c.name, "status": c.status })).collect::<Vec<_>>(),
            "storeWrites": store_writes,
            "during": during,
        }),
        CodemodeResult::Failure {
            ok,
            error,
            output,
            calls,
            during,
            ..
        } => json!({
            "ok": ok,
            "error": error,
            "output": output,
            "calls": calls.into_iter().map(|c| json!({ "name": c.name, "status": c.status })).collect::<Vec<_>>(),
            "during": during,
        }),
    }
}

#[tokio::test]
async fn makes_same_calls_and_output_for_same_start_seed_and_results() {
    for backend in [DetBackend::Worker, DetBackend::Remote] {
        let code = r#"
            const values = [Date.now(), performance.now(), Math.random(), Math.random(), new Date().toISOString()];
            const first = await tools.echo(values);
            text(JSON.stringify([Date.now(), performance.now(), Math.random()]));
            return await tools.echo(first.concat(Math.random()));
        "#;
        let sandbox = create_det_sandbox(backend, CodemodeSandboxBaseOptions::default(), false);
        let one = start_with(&sandbox, code, STARTED_AT, 1, None, None, None, default_echo_answer);
        let two = start_with(&sandbox, code, STARTED_AT, 1, None, None, None, default_echo_answer);
        let (a, b) = tokio::join!(one.run.result(), two.run.result());
        assert_eq!(strip_call_durations(a.clone()), strip_call_durations(b));
        assert_eq!(*one.calls.lock().unwrap(), *two.calls.lock().unwrap());

        let first_args = one.calls.lock().unwrap()[0].args.clone().expect("args");
        assert_eq!(first_args[0], json!(STARTED_AT as i64));
        assert_eq!(first_args[1], json!(0));
        let random = first_args[2].as_f64().expect("random f64");
        assert_eq!(random, PINNED_FIRST_RANDOM);
        assert_eq!(first_args[4], json!("2026-01-15T10:00:00.000Z"));

        let CodemodeOutputItem::Text { text } = &a.output()[0] else {
            panic!("expected text output");
        };
        let printed: Vec<f64> = serde_json::from_str(text).expect("printed json");
        assert_eq!(printed[0], STARTED_AT + 1000.0);
        assert_eq!(printed[1], 1000.0);

        let later = start_with(
            &sandbox,
            code,
            STARTED_AT + 5000.0,
            1,
            None,
            None,
            None,
            default_echo_answer,
        );
        let reseeded = start_with(&sandbox, code, STARTED_AT, 2, None, None, None, default_echo_answer);
        let _ = tokio::join!(later.run.result(), reseeded.run.result());
        assert_eq!(
            later.calls.lock().unwrap()[0].args.as_ref().unwrap()[2].as_f64(),
            Some(random)
        );
        assert_ne!(
            reseeded.calls.lock().unwrap()[0].args.as_ref().unwrap()[2].as_f64(),
            Some(random)
        );

        let zero = start_with(
            &sandbox,
            "return [Math.random(), Math.random()]",
            STARTED_AT,
            0,
            None,
            None,
            None,
            default_echo_answer,
        );
        match zero.run.result().await {
            CodemodeResult::Success { value: Some(v), .. } => {
                assert_eq!(v, json!(PINNED_SEED_ZERO));
                let vals: Vec<f64> = serde_json::from_value(v).unwrap();
                assert!(!vals.contains(&random));
            }
            other => panic!("unexpected seed 0 result: {other:?}"),
        }
    }
}

#[tokio::test]
#[serial]
async fn runs_in_utc_regardless_of_host_timezone_and_omits_weakref() {
    let _tz_guard = pi_js::env::testing::EnvGuard::new("TZ", Some("Asia/Kolkata"));
    let code =
        "return [new Date(0).getHours(), new Date(0).getTimezoneOffset(), typeof WeakRef, typeof FinalizationRegistry]";
    for backend in [DetBackend::Worker, DetBackend::Remote] {
        let sandbox = create_det_sandbox(backend, CodemodeSandboxBaseOptions::default(), false);
        let rec = start_with(&sandbox, code, STARTED_AT, 1, None, None, None, default_echo_answer);
        match rec.run.result().await {
            CodemodeResult::Success { value, .. } => {
                assert_eq!(value, Some(json!([0, 0, "undefined", "undefined"])));
            }
            other => panic!("unexpected deterministic TZ result: {other:?}"),
        }
        match sandbox.execute(code).await {
            CodemodeResult::Success { value, .. } => {
                assert_eq!(value, Some(json!([5, -330, "function", "function"])));
            }
            other => panic!("unexpected host execute TZ result: {other:?}"),
        }
    }
}

#[tokio::test]
async fn applies_results_in_seq_order_whatever_order_delivered() {
    for backend in [DetBackend::Worker, DetBackend::Remote] {
        let sandbox = create_det_sandbox(backend, CodemodeSandboxBaseOptions::default(), false);
        let rec = start_with(
            &sandbox,
            r#"const order = [];
            await Promise.all([tools.wait("a").then(() => order.push("a")), tools.wait("b").then(() => order.push("b"))]);
            return order;"#,
            STARTED_AT,
            1,
            None,
            None,
            None,
            default_echo_answer,
        );
        wait_for(|| rec.calls.lock().unwrap().len() == 2 && rec.idles.lock().unwrap().len() == 1).await;
        let (a_id, b_id) = {
            let calls = rec.calls.lock().unwrap();
            (calls[0].id, calls[1].id)
        };
        rec.run
            .deliver(2, STARTED_AT + 2000.0, a_id, CodemodeOutcome::ok(Some(Value::Null)))
            .expect("deliver 2");
        rec.run
            .deliver(1, STARTED_AT + 1000.0, b_id, CodemodeOutcome::ok(Some(Value::Null)))
            .expect("deliver 1");
        match rec.run.result().await {
            CodemodeResult::Success { value, .. } => assert_eq!(value, Some(json!(["b", "a"]))),
            other => panic!("unexpected seq order result: {other:?}"),
        }
        assert_eq!(*rec.idles.lock().unwrap(), vec![(0, 2), (1, 2)]);
    }
}

#[tokio::test]
async fn reports_fingerprint_and_issued_calls_after_each_result() {
    let re = Regex::new(
        r"^quickjs-wasi \S+, quickjs \S+, prelude [0-9a-f]{8}, deterministic \d+, stack \d+, heap 67108864, budget \S+$",
    )
    .unwrap();

    for backend in [DetBackend::Worker, DetBackend::Remote] {
        let sandbox = create_det_sandbox(
            backend,
            CodemodeSandboxBaseOptions {
                memory_limit_bytes: Some((64 * 1024 * 1024) as f64),
                ..Default::default()
            },
            false,
        );
        let rec = start_with(
            &sandbox,
            "await tools.echo(1); await Promise.all([tools.echo(2), tools.echo(3)]);",
            STARTED_AT,
            1,
            None,
            None,
            None,
            default_echo_answer,
        );
        assert!(rec.run.result().await.is_ok());
        assert_eq!(rec.events.lock().unwrap()[..3], ["started", "call 1", "idle 0"]);
        assert_eq!(*rec.idles.lock().unwrap(), vec![(0, 1), (1, 3), (2, 3)]);
        let call_tuples: Vec<(i64, CodemodeCallTarget, String, Option<Value>)> = rec
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| (c.id, c.target, c.name.clone(), c.args.clone()))
            .collect();
        assert_eq!(
            call_tuples,
            vec![
                (1, CodemodeCallTarget::Tool, "echo".to_string(), Some(json!(1))),
                (2, CodemodeCallTarget::Tool, "echo".to_string(), Some(json!(2))),
                (3, CodemodeCallTarget::Tool, "echo".to_string(), Some(json!(3))),
            ]
        );
        let fp = rec.fingerprint.lock().unwrap().clone().expect("fingerprint");
        assert!(re.is_match(&fp), "unexpected fingerprint: {fp}");
        if backend == DetBackend::Remote {
            assert!(fp.contains(&format!("budget {BUDGET}")), "fp={fp}");
        } else {
            assert!(fp.contains("budget none"), "fp={fp}");
        }

        let same = start_with(
            &sandbox,
            "return 1",
            STARTED_AT,
            1,
            None,
            None,
            None,
            default_echo_answer,
        );
        let other_sb = create_det_sandbox(
            backend,
            CodemodeSandboxBaseOptions {
                memory_limit_bytes: Some((32 * 1024 * 1024) as f64),
                ..Default::default()
            },
            false,
        );
        let other = start_with(
            &other_sb,
            "return 1",
            STARTED_AT,
            1,
            None,
            None,
            None,
            default_echo_answer,
        );
        let _ = tokio::join!(same.run.result(), other.run.result());
        assert_eq!(same.fingerprint.lock().unwrap().as_deref(), Some(fp.as_str()));
        assert_ne!(other.fingerprint.lock().unwrap().as_deref(), Some(fp.as_str()));
    }
}

#[tokio::test]
async fn numbers_globals_with_tools_and_answers_errors_as_rejections() {
    for backend in [DetBackend::Worker, DetBackend::Remote] {
        let sandbox = create_det_sandbox(backend, CodemodeSandboxBaseOptions::default(), false);
        let rec = start_with(
            &sandbox,
            r#"const found = await search("x", 2);
            try { await tools.echo("fail"); } catch (error) { text(error.message); }
            return found;"#,
            STARTED_AT,
            1,
            None,
            Some(vec![CodemodeDeclaredGlobal {
                name: "search".to_string(),
                spread: Some(true),
            }]),
            None,
            |call| {
                if call.args == Some(json!("fail")) {
                    CodemodeOutcome::err("no")
                } else if call.name == "search" {
                    CodemodeOutcome::ok(Some(json!(["found"])))
                } else {
                    CodemodeOutcome::ok(call.args.clone())
                }
            },
        );
        let res = rec.run.result().await;
        match &res {
            CodemodeResult::Success {
                value, output, calls, ..
            } => {
                assert_eq!(value, &Some(json!(["found"])));
                assert_eq!(output, &vec![CodemodeOutputItem::Text { text: "no".to_string() }]);
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "echo");
                assert_eq!(calls[0].status, CodemodeCallStatus::Error);
            }
            other => panic!("unexpected result: {other:?}"),
        }
        let recorded_calls = rec.calls.lock().unwrap();
        assert_eq!(
            recorded_calls[0],
            CodemodeHostCall {
                id: 1,
                target: CodemodeCallTarget::Global,
                name: "search".to_string(),
                args: Some(json!(["x", 2])),
            }
        );
        assert_eq!(recorded_calls[1].id, 2);
        assert_eq!(recorded_calls[1].target, CodemodeCallTarget::Tool);
        assert_eq!(recorded_calls[1].name, "echo");
        assert_eq!(rec.idles.lock().unwrap()[..2], [(0, 1), (1, 2)]);
    }
}

#[tokio::test]
async fn keeps_clock_monotonic_and_rejects_non_finite_clock() {
    for backend in [DetBackend::Worker, DetBackend::Remote] {
        let sandbox = create_det_sandbox(backend, CodemodeSandboxBaseOptions::default(), false);
        let rec = start_with(
            &sandbox,
            "const before = Date.now(); await tools.wait(1); return [before, Date.now(), performance.now()]",
            STARTED_AT,
            1,
            None,
            None,
            None,
            default_echo_answer,
        );
        wait_for(|| rec.calls.lock().unwrap().len() == 1).await;
        let err = rec
            .run
            .deliver(1, f64::NAN, 1, CodemodeOutcome::ok(Some(Value::Null)))
            .expect_err("NaN clock should fail");
        assert!(err.message().contains("not a finite number"));
        rec.run
            .deliver(1, STARTED_AT - 2000.0, 1, CodemodeOutcome::ok(Some(Value::Null)))
            .expect("earlier clock clamps monotonically");
        match rec.run.result().await {
            CodemodeResult::Success { value, .. } => {
                assert_eq!(value, Some(json!([STARTED_AT as i64, STARTED_AT as i64, 0])));
            }
            other => panic!("unexpected clock result: {other:?}"),
        }
    }
}

#[tokio::test]
async fn reports_during_when_script_ends() {
    for backend in [DetBackend::Worker, DetBackend::Remote] {
        let sandbox = create_det_sandbox(backend, CodemodeSandboxBaseOptions::default(), false);
        let returned = start_with(
            &sandbox,
            "await tools.echo(1); return await tools.echo(2)",
            STARTED_AT,
            1,
            None,
            None,
            None,
            default_echo_answer,
        );
        let thrown = start_with(
            &sandbox,
            "await tools.echo(1); await Promise.all([tools.echo(2), tools.echo(3)]); throw new Error('x')",
            STARTED_AT,
            1,
            None,
            None,
            None,
            default_echo_answer,
        );
        let first = start_with(
            &sandbox,
            "return 1",
            STARTED_AT,
            1,
            None,
            None,
            None,
            default_echo_answer,
        );

        let r_ret = returned.run.result().await;
        assert_eq!(r_ret.during(), Some(&CodemodeDuring { seq: 2, issued: 2 }));
        let r_thrown = thrown.run.result().await;
        assert!(!r_thrown.is_ok());
        assert_eq!(r_thrown.during(), Some(&CodemodeDuring { seq: 3, issued: 3 }));
        let r_first = first.run.result().await;
        assert_eq!(r_first.during(), Some(&CodemodeDuring { seq: 0, issued: 0 }));
        let r_exec = sandbox.execute("return 1").await;
        assert_eq!(r_exec.during(), None);
    }
}

#[tokio::test]
async fn rejects_with_error_carrying_answer_properties() {
    for backend in [DetBackend::Worker, DetBackend::Remote] {
        let sandbox = create_det_sandbox(backend, CodemodeSandboxBaseOptions::default(), false);
        let rec = start_with(
            &sandbox,
            "try { await tools.echo(1); } catch (e) { return [e instanceof Error, e.message, e.name, e.structuredOutput]; }",
            STARTED_AT,
            1,
            None,
            None,
            None,
            |_| {
                let mut props = IndexMap::new();
                props.insert("structuredOutput".to_string(), json!({ "code": 7 }));
                props.insert("name".to_string(), json!("Ignored"));
                props.insert("message".to_string(), json!("ignored"));
                CodemodeOutcome::err_with_properties("denied", props)
            },
        );
        match rec.run.result().await {
            CodemodeResult::Success { value, .. } => {
                assert_eq!(value, Some(json!([true, "denied", "Error", { "code": 7 }])));
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }
}

#[tokio::test]
async fn rejects_duplicate_script_names_and_uses_custom_js_name() {
    for backend in [DetBackend::Worker, DetBackend::Remote] {
        let sandbox = create_det_sandbox(backend, CodemodeSandboxBaseOptions::default(), false);
        let err = sandbox
            .start(
                "",
                CodemodeRunOptions {
                    started_at: STARTED_AT,
                    seed: 1,
                    tools: Some(vec![
                        CodemodeDeclaredTool {
                            name: "a-b".to_string(),
                            js_name: None,
                            description: None,
                        },
                        CodemodeDeclaredTool {
                            name: "a_b".to_string(),
                            js_name: None,
                            description: None,
                        },
                    ]),
                    globals: None,
                    store: None,
                    signal: None,
                    timeout_ms: None,
                    on_started: None,
                    on_call: Arc::new(|_, _| {}),
                    on_idle: None,
                },
            )
            .err()
            .expect("duplicate script name error");
        assert!(err.message().contains("script names must be unique"));

        let rec = start_with(
            &sandbox,
            "return [typeof tools.renamed, Object.keys(tools), ALL_TOOLS.map((tool) => tool.name)]",
            STARTED_AT,
            1,
            Some(vec![CodemodeDeclaredTool {
                name: "my-tool".to_string(),
                js_name: Some("renamed".to_string()),
                description: None,
            }]),
            None,
            None,
            default_echo_answer,
        );
        match rec.run.result().await {
            CodemodeResult::Success { value, .. } => {
                assert_eq!(value, Some(json!(["function", ["renamed", "my-tool"], ["renamed"]])));
            }
            other => panic!("unexpected jsName result: {other:?}"),
        }
    }
}

#[tokio::test]
async fn aborts_non_catchably_and_tracks_finished_and_invalid_deliveries() {
    for backend in [DetBackend::Worker, DetBackend::Remote] {
        let sandbox = create_det_sandbox(backend, CodemodeSandboxBaseOptions::default(), false);

        // Aborts cleanly without running catch/after blocks.
        let controller = AbortController::new();
        let rec_abort = start_with(
            &sandbox,
            "try { await tools.wait(1); } catch { text(\"caught\"); } text(\"after\");",
            STARTED_AT,
            1,
            None,
            None,
            Some(controller.signal()),
            default_echo_answer,
        );
        wait_for(|| rec_abort.calls.lock().unwrap().len() == 1).await;
        controller.abort(Some(AbortReason::error("Error", "diverged")));
        match rec_abort.run.result().await {
            CodemodeResult::Failure { error, output, .. } => {
                assert_eq!(error.kind, CodemodeErrorKind::Aborted);
                assert_eq!(error.message, "diverged");
                assert!(output.is_empty());
            }
            other => panic!("expected abort failure, got {other:?}"),
        }
        assert!(
            rec_abort
                .run
                .deliver(1, STARTED_AT, 1, CodemodeOutcome::ok(Some(Value::Null)))
                .is_ok()
        );

        // Finished state tracks outcome before/after settlement.
        let rec_fin = start_with(
            &sandbox,
            "await tools.wait(1); return 2",
            STARTED_AT,
            1,
            None,
            None,
            None,
            default_echo_answer,
        );
        wait_for(|| rec_fin.calls.lock().unwrap().len() == 1).await;
        assert!(!rec_fin.run.finished());
        rec_fin
            .run
            .deliver(1, STARTED_AT, 1, CodemodeOutcome::ok(Some(Value::Null)))
            .expect("deliver");
        let _ = rec_fin.run.result().await;
        assert!(rec_fin.run.finished());

        // Rejects non-waiting call id, duplicate seq, and non-positive seq.
        let rec_inv = start_with(
            &sandbox,
            "await Promise.all([tools.wait(1), tools.wait(2)])",
            STARTED_AT,
            1,
            None,
            None,
            None,
            default_echo_answer,
        );
        wait_for(|| rec_inv.calls.lock().unwrap().len() == 2).await;
        let err_unknown = rec_inv
            .run
            .deliver(1, STARTED_AT, 99, CodemodeOutcome::ok(Some(Value::Null)))
            .expect_err("unknown id");
        assert!(err_unknown.message().contains("Call 99 is not waiting"));
        rec_inv
            .run
            .deliver(1, STARTED_AT, 1, CodemodeOutcome::ok(Some(Value::Null)))
            .expect("deliver 1");
        let err_dup = rec_inv
            .run
            .deliver(1, STARTED_AT, 2, CodemodeOutcome::ok(Some(Value::Null)))
            .expect_err("dup seq");
        assert!(err_dup.message().contains("used once"));
        let err_zero = rec_inv
            .run
            .deliver(0, STARTED_AT, 2, CodemodeOutcome::ok(Some(Value::Null)))
            .expect_err("seq 0");
        assert!(err_zero.message().contains("used once"));
        rec_inv
            .run
            .deliver(2, STARTED_AT, 2, CodemodeOutcome::ok(Some(Value::Null)))
            .expect("deliver 2");
        assert!(rec_inv.run.result().await.is_ok());
    }
}

#[tokio::test]
async fn applies_results_in_seq_order_when_remote_transport_reorders_replies() {
    let sandbox = create_det_sandbox(DetBackend::Remote, CodemodeSandboxBaseOptions::default(), true);
    let rec = start_with(
        &sandbox,
        r#"const order = [];
        await Promise.all([tools.wait("a").then(() => order.push("a")), tools.wait("b").then(() => order.push("b"))]);
        return order;"#,
        STARTED_AT,
        1,
        None,
        None,
        None,
        default_echo_answer,
    );
    wait_for(|| rec.calls.lock().unwrap().len() == 2).await;
    let (a_id, b_id) = {
        let calls = rec.calls.lock().unwrap();
        (calls[0].id, calls[1].id)
    };
    rec.run
        .deliver(1, STARTED_AT + 1000.0, a_id, CodemodeOutcome::ok(Some(Value::Null)))
        .expect("deliver 1");
    rec.run
        .deliver(2, STARTED_AT + 2000.0, b_id, CodemodeOutcome::ok(Some(Value::Null)))
        .expect("deliver 2");
    match rec.run.result().await {
        CodemodeResult::Success { value, .. } => assert_eq!(value, Some(json!(["a", "b"]))),
        other => panic!("unexpected reordered result: {other:?}"),
    }
}

#[tokio::test]
async fn execute_is_not_deterministic() {
    let sandbox = CodemodeSandbox::new(None).expect("sandbox");
    let before = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as f64 - 1.0;
    let first = sandbox
        .execute("return [Date.now(), Math.random()]", None)
        .await
        .expect("first");
    let second = sandbox
        .execute("return [Date.now(), Math.random()]", None)
        .await
        .expect("second");
    let after = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as f64 + 1.0;
    let (CodemodeResult::Success { value: Some(v1), .. }, CodemodeResult::Success { value: Some(v2), .. }) =
        (first, second)
    else {
        panic!("expected both executions to succeed");
    };
    let now = v1[0].as_f64().unwrap();
    let r1 = v1[1].as_f64().unwrap();
    let r2 = v2[1].as_f64().unwrap();
    assert!(now >= before && now <= after, "now={now} outside [{before}, {after}]");
    assert_ne!(r1, r2);
}
