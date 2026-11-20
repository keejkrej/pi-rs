//! Port of packages/coding-agent/test/suite/regressions/6363-agent-settled-event.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{StopReason, TextContent, UserContentBlock};
use pi_coding_agent::core::agent_session::{AgentSessionEvent, ExtensionBindings};
use pi_coding_agent::core::extensions::types::{
    AgentEndEvent, AgentSettledEvent, CancelledResult, CustomMessageContent, ExtensionApi,
    ExtensionCommandContextActions, InlineExtension, InputDeliveryBehavior, RegisteredCommandOptions,
    SendUserMessageOptions,
};
use pi_coding_agent::core::settings_manager::{RetrySettings, Settings};
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::json;
use support::suite::harness::{HarnessOptions, create_harness, get_user_texts};
use tokio::sync::Notify;

fn create_wait_tool(released: Arc<Notify>) -> AgentTool {
    AgentTool {
        name: "wait".to_string(),
        label: "Wait".to_string(),
        description: "Wait until released".to_string(),
        parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]).into(),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, _sig, _upd| {
            let rel = Arc::clone(&released);
            Box::pin(async move {
                rel.notified().await;
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("released"))],
                    details: Some(json!({})),
                    ..Default::default()
                })
            })
        }),
        replay: None,
    }
}

#[tokio::test]
async fn emits_one_agent_settled_event_after_automatic_retry_finishes() {
    let extension_events = Arc::new(Mutex::new(Vec::<String>::new()));
    let ee_c = Arc::clone(&extension_events);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let ee1 = Arc::clone(&ee_c);
        let ee2 = Arc::clone(&ee_c);
        async move {
            pi.on::<AgentEndEvent>(move |_ev, _ctx| {
                let ee1 = Arc::clone(&ee1);
                Box::pin(async move {
                    ee1.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push("agent_end".to_string());
                    Ok(None)
                })
            });
            pi.on::<AgentSettledEvent>(move |_ev, ctx| {
                let ee2 = Arc::clone(&ee2);
                Box::pin(async move {
                    let idle = ctx.is_idle()?;
                    ee2.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(format!("agent_settled:{idle}"));
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        settings: Some(Settings {
            retry: Some(RetrySettings {
                enabled: Some(true),
                max_retries: Some(3),
                base_delay_ms: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let public_events = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let pe_c = Arc::clone(&public_events);
    let _unsub = harness.session.subscribe(move |ev| {
        if matches!(ev, AgentSessionEvent::AgentSettled { .. }) {
            pe_c.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push("agent_settled");
        }
    });

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text(String::new()),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("overloaded_error".to_string()),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("recovered".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("test", None).await.unwrap();

    let will_retries: Vec<bool> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::AgentEnd { will_retry, .. } => Some(will_retry),
            _ => None,
        })
        .collect();
    assert_eq!(will_retries, vec![true, false]);
    let settled_count = harness
        .events()
        .into_iter()
        .filter(|e| matches!(e, AgentSessionEvent::AgentSettled { .. }))
        .count();
    assert_eq!(settled_count, 1);
    assert_eq!(
        *extension_events.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["agent_end", "agent_end", "agent_settled:true"]
    );
    assert_eq!(
        *public_events.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["agent_settled"]
    );
    harness.cleanup();
}

#[tokio::test]
async fn settles_only_after_follow_ups_queued_by_agent_end_handlers_run() {
    let queued_follow_up = Arc::new(AtomicBool::new(false));
    let settled_idle_states = Arc::new(Mutex::new(Vec::<bool>::new()));

    let qf_c = Arc::clone(&queued_follow_up);
    let sis_c = Arc::clone(&settled_idle_states);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let qf = Arc::clone(&qf_c);
        let sis = Arc::clone(&sis_c);
        async move {
            let pi_ev = pi.clone();
            pi.on::<AgentEndEvent>(move |_ev, _ctx| {
                let qf = Arc::clone(&qf);
                let pi_h = pi_ev.clone();
                Box::pin(async move {
                    if qf.swap(true, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    pi_h.send_user_message(
                        CustomMessageContent::Text("status follow-up".to_string()),
                        Some(SendUserMessageOptions {
                            deliver_as: Some(InputDeliveryBehavior::FollowUp),
                            ..Default::default()
                        }),
                    )?;
                    Ok(None)
                })
            });
            pi.on::<AgentSettledEvent>(move |_ev, ctx| {
                let sis = Arc::clone(&sis);
                Box::pin(async move {
                    sis.lock().unwrap_or_else(PoisonError::into_inner).push(ctx.is_idle()?);
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("first".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("second".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("hello", None).await.unwrap();

    assert_eq!(get_user_texts(&harness), vec!["hello", "status follow-up"]);
    assert_eq!(
        harness
            .events()
            .into_iter()
            .filter(|e| matches!(e, AgentSessionEvent::AgentEnd { .. }))
            .count(),
        2
    );
    assert_eq!(
        harness
            .events()
            .into_iter()
            .filter(|e| matches!(e, AgentSessionEvent::AgentSettled { .. }))
            .count(),
        1
    );
    assert_eq!(
        *settled_idle_states.lock().unwrap_or_else(PoisonError::into_inner),
        vec![true]
    );
    harness.cleanup();
}

#[tokio::test]
async fn extension_command_wait_for_idle_waits_for_session_level_settlement() {
    let released = Arc::new(Notify::new());
    let command_started = Arc::new(Notify::new());
    let command_results = Arc::new(Mutex::new(Vec::<bool>::new()));

    let cs_c = Arc::clone(&command_started);
    let cr_c = Arc::clone(&command_results);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let cs = Arc::clone(&cs_c);
        let cr = Arc::clone(&cr_c);
        async move {
            pi.register_command(
                "after-idle",
                RegisteredCommandOptions {
                    description: Some("Wait for idle".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |_args, ctx| {
                        let cs = Arc::clone(&cs);
                        let cr = Arc::clone(&cr);
                        Box::pin(async move {
                            cs.notify_one();
                            ctx.wait_for_idle().await?;
                            let idle = ctx.base.is_idle()?;
                            cr.lock().unwrap_or_else(PoisonError::into_inner).push(idle);
                            Ok(())
                        })
                    }),
                },
            )?;
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![create_wait_tool(Arc::clone(&released))]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let session_for_idle = harness.session.clone();
    harness
        .session
        .bind_extensions(ExtensionBindings {
            command_context_actions: Some(ExtensionCommandContextActions {
                wait_for_idle: Arc::new(move || {
                    let s = session_for_idle.clone();
                    Box::pin(async move {
                        s.wait_for_idle().await;
                        Ok(())
                    })
                }),
                new_session: Arc::new(|_| Box::pin(async { Ok(CancelledResult { cancelled: false }) })),
                fork: Arc::new(|_, _| Box::pin(async { Ok(CancelledResult { cancelled: false }) })),
                navigate_tree: Arc::new(|_, _| Box::pin(async { Ok(CancelledResult { cancelled: false }) })),
                switch_session: Arc::new(|_, _| Box::pin(async { Ok(CancelledResult { cancelled: false }) })),
                reload: Arc::new(|| Box::pin(async { Ok(()) })),
            }),
            ..Default::default()
        })
        .await
        .unwrap();

    let tool_started = Arc::new(Notify::new());
    let ts_c = Arc::clone(&tool_started);
    let _unsub = harness.session.subscribe(move |ev| {
        if let AgentSessionEvent::ToolExecutionStart { tool_name, .. } = ev
            && tool_name == "wait"
        {
            ts_c.notify_one();
        }
    });

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![FauxContentBlock::ToolCall(faux_tool_call(
                "wait",
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

    let ts_wait = tool_started.notified();
    let cs_wait = command_started.notified();
    let command_finished = Arc::new(AtomicBool::new(false));
    let cf_c = Arc::clone(&command_finished);
    let session_cmd = harness.session.clone();

    let prompt_fut = harness.session.prompt("start", None);
    let drive_fut = async {
        ts_wait.await;
        let cmd_handle = tokio::spawn(async move {
            let res = session_cmd.prompt("/after-idle", None).await;
            cf_c.store(true, Ordering::SeqCst);
            res
        });
        cs_wait.await;
        tokio::task::yield_now().await;
        assert!(!command_finished.load(Ordering::SeqCst));
        released.notify_one();
        cmd_handle.await.unwrap().unwrap();
    };
    let (prompt_res, ()) = tokio::join!(prompt_fut, drive_fut);
    prompt_res.unwrap();

    assert_eq!(
        *command_results.lock().unwrap_or_else(PoisonError::into_inner),
        vec![true]
    );
    assert_eq!(
        harness
            .events()
            .into_iter()
            .filter(|e| matches!(e, AgentSessionEvent::AgentSettled { .. }))
            .count(),
        1
    );
    harness.cleanup();
}
