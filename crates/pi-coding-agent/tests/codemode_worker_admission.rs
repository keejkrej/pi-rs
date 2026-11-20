//! Port of packages/coding-agent/test/codemode-worker-admission.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{StopReason, UserContentBlock};
use pi_coding_agent::core::extensions::types::{ExtensionApi, ExtensionToolContext, InlineExtension};
use pi_coding_agent::core::settings_manager::{CodemodeMode, CodemodeSettings, Settings};
use pi_coding_agent::extensions::codemode::execute::execute_codemode;
use pi_coding_agent::extensions::codemode::tool::{
    CodemodeToolInput, CodemodeToolOptions, create_codemode_tool_definition,
};
use pi_coding_agent::extensions::codemode::{CodemodeExtensionOptions, create_codemode_extension};
use pi_js::abort::AbortController;
use pi_js::env::testing::EnvGuard;
use pi_js::vendor::typebox::t;
use serial_test::serial;
use support::suite::harness::{HarnessOptions, create_harness};
use tokio::sync::{Mutex, Notify, oneshot};

async fn capture_tool_context(extra_tools: Vec<AgentTool>) -> (support::suite::harness::Harness, ExtensionToolContext) {
    let captured_ctx = Arc::new(Mutex::new(None::<ExtensionToolContext>));
    let cap_c = Arc::clone(&captured_ctx);

    let capture_ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let cap_c = Arc::clone(&cap_c);
        async move {
            let _ = pi.register_tool(pi_coding_agent::core::extensions::types::ToolDefinition {
                name: "capture_ctx".to_string(),
                label: "Capture".to_string(),
                description: "Capture extension tool context".to_string(),
                prompt_snippet: None,
                prompt_guidelines: None,
                parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]),
                constrained_sampling: None,
                render_shell: None,
                prepare_arguments: None,
                output_schema: None,
                exposure: None,
                namespace: None,
                annotations: None,
                default_active: None,
                prepare_loadout: None,
                execution_mode: None,
                execute: Arc::new(move |_id, _params, _sig, _upd, ctx| {
                    let cap_c = Arc::clone(&cap_c);
                    Box::pin(async move {
                        *cap_c.lock().await = Some(ctx);
                        Ok(AgentToolResult::text("captured"))
                    })
                }),
                render_call: None,
                render_result: None,
            });
            Ok(())
        }
    }));

    let cm_ext = InlineExtension::Factory(create_codemode_extension(CodemodeExtensionOptions::default()));
    let mut tools = extra_tools;
    let harness = create_harness(HarnessOptions {
        tools: Some(std::mem::take(&mut tools)),
        extension_factories: Some(vec![capture_ext, cm_ext]),
        settings: Some(Settings {
            codemode: Some(CodemodeSettings {
                mode: Some(CodemodeMode::On),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![FauxContentBlock::ToolCall(faux_tool_call(
                "capture_ctx",
                Default::default(),
                None,
            ))]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("done".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("capture", None).await.expect("prompt");
    let ctx = captured_ctx.lock().await.take().expect("ExtensionToolContext captured");
    (harness, ctx)
}

fn make_blocking_hold_tool(
    entered: Arc<AtomicUsize>,
    entered_notify: Arc<Notify>,
    releases: Arc<Mutex<Vec<oneshot::Sender<()>>>>,
) -> AgentTool {
    AgentTool {
        name: "hold".to_string(),
        label: "Hold".to_string(),
        description: "Blocks until test releases one worker".to_string(),
        parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]).into(),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, signal, _upd| {
            let entered = Arc::clone(&entered);
            let entered_notify = Arc::clone(&entered_notify);
            let releases = Arc::clone(&releases);
            Box::pin(async move {
                let (tx, rx) = oneshot::channel();
                releases.lock().await.push(tx);
                entered.fetch_add(1, Ordering::SeqCst);
                entered_notify.notify_waiters();
                tokio::select! {
                    _ = rx => Ok(AgentToolResult::text("released")),
                    _ = async {
                        if let Some(sig) = signal {
                            sig.cancelled().await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                    } => Ok(AgentToolResult::text("aborted")),
                }
            })
        }),
        replay: None,
    }
}

fn result_text(res: &AgentToolResult) -> String {
    res.content
        .iter()
        .filter_map(|b| match b {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// #10748, #11184: codemode shares bounded worker capacity across scripts, queues waiters, and releases on exit.
#[tokio::test]
#[serial]
async fn bounds_workers_queues_waiters_and_respects_timeout_and_abort_while_waiting() {
    let entered = Arc::new(AtomicUsize::new(0));
    let entered_notify = Arc::new(Notify::new());
    let releases = Arc::new(Mutex::new(Vec::<oneshot::Sender<()>>::new()));
    let hold = make_blocking_hold_tool(Arc::clone(&entered), Arc::clone(&entered_notify), Arc::clone(&releases));

    let (harness, ctx) = capture_tool_context(vec![hold]).await;

    // 1. Timeout, abort, and success scripts release their worker slots properly.
    let timeout_res = execute_codemode(
        "t1".to_string(),
        CodemodeToolInput {
            code: "// @options: {\"timeout_ms\": 150}\nwhile (true) {}".to_string(),
        },
        None,
        None,
        Some(ctx.clone()),
        CodemodeToolOptions::default(),
    )
    .await
    .expect("timeout result");
    assert_eq!(timeout_res.is_error, Some(true));
    assert!(result_text(&timeout_res).contains("Script timed out:"));

    let ac = AbortController::new();
    let sig = ac.signal();
    let c_ctx = ctx.clone();
    let abort_task = tokio::spawn(async move {
        execute_codemode(
            "a1".to_string(),
            CodemodeToolInput {
                code: "while (true) {}".to_string(),
            },
            Some(sig),
            None,
            Some(c_ctx),
            CodemodeToolOptions::default(),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(60)).await;
    ac.abort(None);
    let abort_res = abort_task.await.expect("join").expect("abort res");
    assert_eq!(abort_res.is_error, Some(true));
    assert!(result_text(&abort_res).contains("Script aborted:"));

    let ok_res = execute_codemode(
        "s1".to_string(),
        CodemodeToolInput {
            code: "return 1".to_string(),
        },
        None,
        None,
        Some(ctx.clone()),
        CodemodeToolOptions::default(),
    )
    .await
    .expect("ok res");
    assert_eq!(ok_res.is_error, None);

    // 2. Saturate both default worker slots (2) with blocking scripts.
    let mut active_tasks = Vec::new();
    for i in 0..2 {
        let c = ctx.clone();
        active_tasks.push(tokio::spawn(async move {
            execute_codemode(
                format!("hold-{i}"),
                CodemodeToolInput {
                    code: "return await tools.hold({})".to_string(),
                },
                None,
                None,
                Some(c),
                CodemodeToolOptions::default(),
            )
            .await
        }));
    }
    while entered.load(Ordering::SeqCst) < 2 {
        entered_notify.notified().await;
    }

    // 3. Queue a waiting script ("return 3").
    let waiting_settled = Arc::new(AtomicUsize::new(0));
    let ws_c = Arc::clone(&waiting_settled);
    let w_ctx = ctx.clone();
    let waiting = tokio::spawn(async move {
        let r = execute_codemode(
            "wait-3".to_string(),
            CodemodeToolInput {
                code: "return 3".to_string(),
            },
            None,
            None,
            Some(w_ctx),
            CodemodeToolOptions::default(),
        )
        .await;
        ws_c.store(1, Ordering::SeqCst);
        r
    });

    // Abort while waiting fails before starting.
    let ac_wait = AbortController::new();
    let sig_wait = ac_wait.signal();
    let aw_ctx = ctx.clone();
    let aborted_waiter = tokio::spawn(async move {
        execute_codemode(
            "abort-wait".to_string(),
            CodemodeToolInput {
                code: "return 9".to_string(),
            },
            Some(sig_wait),
            None,
            Some(aw_ctx),
            CodemodeToolOptions::default(),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(40)).await;
    ac_wait.abort(None);
    let aborted_waiter_res = aborted_waiter.await.expect("join").expect("res");
    assert_eq!(aborted_waiter_res.is_error, Some(true));
    let aw_text = result_text(&aborted_waiter_res);
    assert!(aw_text.contains("Script aborted:"), "text: {aw_text}");
    assert!(aw_text.contains("The script did not start."), "text: {aw_text}");

    // Timeout while waiting fails with admission message.
    let timed_out = execute_codemode(
        "to-wait".to_string(),
        CodemodeToolInput {
            code: "// @options: {\"timeout_ms\": 50}\nreturn 1".to_string(),
        },
        None,
        None,
        Some(ctx.clone()),
        CodemodeToolOptions::default(),
    )
    .await
    .expect("timed_out");
    assert_eq!(timed_out.is_error, Some(true));
    assert!(
        result_text(&timed_out).contains("Execution timed out after 50 ms while waiting for a free codemode worker")
    );
    assert_eq!(waiting_settled.load(Ordering::SeqCst), 0);

    // Release one active worker -> waiting script ("return 3") runs and completes.
    if let Some(tx) = releases.lock().await.pop() {
        let _ = tx.send(());
    }
    let admitted = waiting.await.expect("join").expect("admitted");
    assert_eq!(admitted.is_error, None);
    assert!(
        result_text(&admitted).contains('3'),
        "admitted: {}",
        result_text(&admitted)
    );

    // Release remaining active worker.
    while let Some(tx) = releases.lock().await.pop() {
        let _ = tx.send(());
    }
    for task in active_tasks {
        let _ = task.await;
    }

    // 4. PI_CODEMODE_MAX_WORKERS=3 allows 3 concurrent workers; =0 falls back to 2.
    let entered3 = Arc::new(AtomicUsize::new(0));
    let notify3 = Arc::new(Notify::new());
    let releases3 = Arc::new(Mutex::new(Vec::<oneshot::Sender<()>>::new()));
    let hold3 = make_blocking_hold_tool(Arc::clone(&entered3), Arc::clone(&notify3), Arc::clone(&releases3));
    let (h3, ctx3) = capture_tool_context(vec![hold3]).await;

    let env3 = EnvGuard::set("PI_CODEMODE_MAX_WORKERS", Some("3"));
    let mut t3 = Vec::new();
    for i in 0..4 {
        let c = ctx3.clone();
        t3.push(tokio::spawn(async move {
            execute_codemode(
                format!("env3-{i}"),
                CodemodeToolInput {
                    code: "return await tools.hold({})".to_string(),
                },
                None,
                None,
                Some(c),
                CodemodeToolOptions::default(),
            )
            .await
        }));
    }
    while entered3.load(Ordering::SeqCst) < 3 {
        notify3.notified().await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(entered3.load(Ordering::SeqCst), 3);

    while entered3.load(Ordering::SeqCst) < 4 {
        let maybe_tx = releases3.lock().await.pop();
        if let Some(tx) = maybe_tx {
            let _ = tx.send(());
        } else {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    while let Some(tx) = releases3.lock().await.pop() {
        let _ = tx.send(());
    }
    for t in t3 {
        let _ = t.await;
    }
    drop(env3);

    // Invalid PI_CODEMODE_MAX_WORKERS=0 falls back to 2.
    let _env0 = EnvGuard::set("PI_CODEMODE_MAX_WORKERS", Some("0"));
    entered.store(0, Ordering::SeqCst);
    let _def = create_codemode_tool_definition(CodemodeToolOptions::default());
    let mut t0 = Vec::new();
    for i in 0..3 {
        let c = ctx.clone();
        t0.push(tokio::spawn(async move {
            execute_codemode(
                format!("env0-{i}"),
                CodemodeToolInput {
                    code: "return await tools.hold({})".to_string(),
                },
                None,
                None,
                Some(c),
                CodemodeToolOptions::default(),
            )
            .await
        }));
    }
    while entered.load(Ordering::SeqCst) < 2 {
        entered_notify.notified().await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(entered.load(Ordering::SeqCst), 2);
    while entered.load(Ordering::SeqCst) < 3 {
        let maybe_tx = releases.lock().await.pop();
        if let Some(tx) = maybe_tx {
            let _ = tx.send(());
        } else {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    while let Some(tx) = releases.lock().await.pop() {
        let _ = tx.send(());
    }
    for t in t0 {
        let _ = t.await;
    }

    h3.cleanup();
    harness.cleanup();
}
