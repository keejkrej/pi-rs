//! Port of packages/coding-agent/test/suite/agent-session-retry-events.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, PoisonError};

use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep,
    faux_assistant_message, faux_thinking, faux_tool_call,
};
use pi_ai::types::{AssistantMessageEvent, StopReason, TextContent, UserContentBlock};
use pi_coding_agent::core::agent_session::AgentSessionEvent;
use pi_coding_agent::core::extensions::types::{ExtensionApi, InlineExtension, MessageEndEvent, MessageStartEvent};
use pi_coding_agent::core::settings_manager::{RetrySettings, Settings};
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::suite::harness::{HarnessOptions, create_harness};

fn normalize_event_order(events: &[AgentSessionEvent]) -> Vec<String> {
    let mut normalized = Vec::new();
    for event in events {
        let label = match event {
            AgentSessionEvent::AgentStart => "agent_start".to_string(),
            AgentSessionEvent::AgentEnd { .. } => "agent_end".to_string(),
            AgentSessionEvent::AgentSettled { .. } => "agent_settled".to_string(),
            AgentSessionEvent::TurnStart => "turn_start".to_string(),
            AgentSessionEvent::TurnEnd { .. } => "turn_end".to_string(),
            AgentSessionEvent::MessageStart { message } => {
                format!("message_start:{}", message.role().unwrap_or("unknown"))
            }
            AgentSessionEvent::MessageEnd { message } => {
                format!("message_end:{}", message.role().unwrap_or("unknown"))
            }
            AgentSessionEvent::MessageUpdate { .. } => "message_update".to_string(),
            AgentSessionEvent::ToolExecutionStart { tool_name, .. } => format!("tool_execution_start:{tool_name}"),
            AgentSessionEvent::ToolExecutionEnd { tool_name, .. } => format!("tool_execution_end:{tool_name}"),
            AgentSessionEvent::ToolExecutionUpdate { .. } => "tool_execution_update".to_string(),
            AgentSessionEvent::QueueUpdate { .. } => "queue_update".to_string(),
            AgentSessionEvent::CompactionStart { .. } => "compaction_start".to_string(),
            AgentSessionEvent::CompactionEnd { .. } => "compaction_end".to_string(),
            AgentSessionEvent::AutoRetryStart { .. } => "auto_retry_start".to_string(),
            AgentSessionEvent::AutoRetryEnd { .. } => "auto_retry_end".to_string(),
            AgentSessionEvent::SummarizationRetryScheduled { .. } => "summarization_retry_scheduled".to_string(),
            AgentSessionEvent::SummarizationRetryAttemptStart { .. } => "summarization_retry_attempt_start".to_string(),
            AgentSessionEvent::SummarizationRetryFinished => "summarization_retry_finished".to_string(),
            AgentSessionEvent::BashExecutionUpdate { .. } => "bash_execution_update".to_string(),
            AgentSessionEvent::EntryAppended { .. }
            | AgentSessionEvent::SessionInfoChanged { .. }
            | AgentSessionEvent::ThinkingLevelChanged { .. } => continue,
        };
        if label == "message_update" && normalized.last().map(String::as_str) == Some("message_update") {
            continue;
        }
        normalized.push(label);
    }
    normalized
}

#[tokio::test]
async fn retries_after_a_transient_error_and_succeeds() {
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
        ..Default::default()
    })
    .await;

    let retry_events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let re_ref = Arc::clone(&retry_events);
    let _unsub = harness.session.subscribe(move |ev| match ev {
        AgentSessionEvent::AutoRetryStart { attempt, .. } => {
            re_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("start:{attempt}"));
        }
        AgentSessionEvent::AutoRetryEnd { success, .. } => {
            re_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("end:{success}"));
        }
        _ => {}
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

    assert_eq!(
        *retry_events.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["start:1".to_string(), "end:true".to_string()]
    );
    let agent_ends: Vec<bool> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::AgentEnd { will_retry, .. } => Some(will_retry),
            _ => None,
        })
        .collect();
    assert_eq!(agent_ends, vec![true, false]);
    assert_eq!(harness.faux.state().call_count(), 2);
    assert!(!harness.session.is_retrying());

    harness.cleanup();
}

#[tokio::test]
async fn retries_multiple_transient_failures_and_succeeds_on_the_final_attempt() {
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
        ..Default::default()
    })
    .await;

    let retry_events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let re_ref = Arc::clone(&retry_events);
    let _unsub = harness.session.subscribe(move |ev| match ev {
        AgentSessionEvent::AutoRetryStart { attempt, .. } => {
            re_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("start:{attempt}"));
        }
        AgentSessionEvent::AutoRetryEnd { success, .. } => {
            re_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("end:{success}"));
        }
        _ => {}
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
            FauxAssistantMessageContent::Text(String::new()),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("overloaded_error".to_string()),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("success".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("test", None).await.unwrap();

    assert_eq!(
        *retry_events.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["start:1".to_string(), "start:2".to_string(), "end:true".to_string()]
    );
    assert_eq!(harness.faux.state().call_count(), 3);
    harness.cleanup();
}

#[tokio::test]
async fn finalizes_retry_state_when_abort_is_requested_after_a_retry_attempt_fails() {
    let harness = create_harness(HarnessOptions {
        settings: Some(Settings {
            retry: Some(RetrySettings {
                enabled: Some(true),
                max_retries: Some(3),
                base_delay_ms: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;

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
            FauxAssistantMessageContent::Text(String::new()),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("overloaded_error".to_string()),
                ..Default::default()
            }),
        )),
    ]);

    let err_count = Arc::new(AtomicU32::new(0));
    let ec_ref = Arc::clone(&err_count);
    let sess = harness.session.clone();
    let _unsub = harness.session.subscribe(move |ev| {
        if let AgentSessionEvent::MessageEnd { message } = ev
            && let pi_agent_core::types::AgentMessage::Message(pi_ai::types::Message::Assistant(a)) = message
            && a.stop_reason == StopReason::Error
            && ec_ref.fetch_add(1, Ordering::SeqCst) + 1 == 2
        {
            sess.request_abort();
            let s = sess.clone();
            tokio::spawn(async move {
                s.wait_for_idle().await;
            });
        }
    });

    harness.session.prompt("test", None).await.unwrap();

    assert_eq!(harness.session.retry_attempt(), 0);
    let last_agent_end = harness.events().into_iter().rev().find_map(|e| match e {
        AgentSessionEvent::AgentEnd { will_retry, .. } => Some(will_retry),
        _ => None,
    });
    assert_eq!(last_agent_end, Some(false));
    let last_retry_end = harness.events().into_iter().rev().find_map(|e| match e {
        AgentSessionEvent::AutoRetryEnd {
            success,
            attempt,
            final_error,
        } => Some((success, attempt, final_error)),
        _ => None,
    });
    assert_eq!(last_retry_end, Some((false, 1, Some("Retry cancelled".to_string()))));
    harness.cleanup();
}

#[tokio::test]
async fn exhausts_max_retries_and_emits_a_failure_event() {
    let harness = create_harness(HarnessOptions {
        settings: Some(Settings {
            retry: Some(RetrySettings {
                enabled: Some(true),
                max_retries: Some(2),
                base_delay_ms: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;

    let retry_events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let re_ref = Arc::clone(&retry_events);
    let _unsub = harness.session.subscribe(move |ev| match ev {
        AgentSessionEvent::AutoRetryStart { attempt, .. } => {
            re_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("start:{attempt}"));
        }
        AgentSessionEvent::AutoRetryEnd { success, .. } => {
            re_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("end:{success}"));
        }
        _ => {}
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
            FauxAssistantMessageContent::Text(String::new()),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("overloaded_error".to_string()),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text(String::new()),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("overloaded_error".to_string()),
                ..Default::default()
            }),
        )),
    ]);

    harness.session.prompt("test", None).await.unwrap();

    assert_eq!(
        *retry_events.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["start:1".to_string(), "start:2".to_string(), "end:false".to_string()]
    );
    let agent_ends: Vec<bool> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::AgentEnd { will_retry, .. } => Some(will_retry),
            _ => None,
        })
        .collect();
    assert_eq!(agent_ends, vec![true, true, false]);
    assert_eq!(harness.faux.state().call_count(), 3);
    assert!(!harness.session.is_retrying());
    harness.cleanup();
}

#[tokio::test]
async fn prompt_waits_for_retry_completion_even_when_assistant_message_end_handling_is_delayed() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<MessageEndEvent>(|ev, _ctx| {
            Box::pin(async move {
                if ev.message.role() == Some("assistant") {
                    let _ = pi_js::time::sleep(40, None).await;
                }
                Ok(None)
            })
        });
        Ok(())
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
    assert_eq!(harness.faux.state().call_count(), 2);
    assert!(!harness.session.is_retrying());
    harness.cleanup();
}

#[tokio::test]
async fn does_not_retry_when_retry_is_disabled() {
    let harness = create_harness(HarnessOptions {
        settings: Some(Settings {
            retry: Some(RetrySettings {
                enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text(String::new()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("overloaded_error".to_string()),
            ..Default::default()
        }),
    ))]);

    harness.session.prompt("test", None).await.unwrap();

    assert_eq!(harness.faux.state().call_count(), 1);
    assert!(
        !harness
            .events()
            .into_iter()
            .any(|e| matches!(e, AgentSessionEvent::AutoRetryStart { .. }))
    );
    harness.cleanup();
}

#[tokio::test]
async fn does_not_retry_non_retryable_errors() {
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
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text(String::new()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("invalid_api_key".to_string()),
            ..Default::default()
        }),
    ))]);

    harness.session.prompt("test", None).await.unwrap();

    assert_eq!(harness.faux.state().call_count(), 1);
    assert!(
        !harness
            .events()
            .into_iter()
            .any(|e| matches!(e, AgentSessionEvent::AutoRetryStart { .. }))
    );
    harness.cleanup();
}

#[tokio::test]
async fn cancels_retry_sleep_when_abort_retry_is_called() {
    let harness = create_harness(HarnessOptions {
        settings: Some(Settings {
            retry: Some(RetrySettings {
                enabled: Some(true),
                max_retries: Some(3),
                base_delay_ms: Some(100),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text(String::new()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("overloaded_error".to_string()),
            ..Default::default()
        }),
    ))]);

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let tx_slot = Arc::new(Mutex::new(Some(tx)));
    let _unsub = harness.session.subscribe(move |ev| {
        if matches!(ev, AgentSessionEvent::AutoRetryStart { .. })
            && let Some(sender) = tx_slot.lock().unwrap_or_else(PoisonError::into_inner).take()
        {
            let _ = sender.send(());
        }
    });

    let sess = harness.session.clone();
    let prompt_task = tokio::spawn(async move { sess.prompt("test", None).await });
    let _ = rx.await;
    harness.session.abort_retry();
    prompt_task.await.unwrap().unwrap();

    assert!(!harness.session.is_retrying());
    let final_errors: Vec<Option<String>> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::AutoRetryEnd { final_error, .. } => Some(final_error),
            _ => None,
        })
        .collect();
    assert!(final_errors.contains(&Some("Retry cancelled".to_string())));
    assert_eq!(harness.faux.state().call_count(), 1);
    harness.cleanup();
}

#[tokio::test]
async fn waits_for_the_full_loop_when_retry_recovery_produces_tool_calls() {
    let tool_runs = Arc::new(Mutex::new(Vec::new()));
    let tr_ref = Arc::clone(&tool_runs);
    let echo_tool = AgentTool {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "Echo text back".to_string(),
        parameters: t::object([("text", t::string())]).into(),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, params, _sig, _upd| {
            let runs = Arc::clone(&tr_ref);
            Box::pin(async move {
                let text = params.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                runs.lock().unwrap_or_else(PoisonError::into_inner).push(text.clone());
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(format!("echo:{text}")))],
                    details: Some(json!({ "text": text })),
                    ..Default::default()
                })
            })
        }),
        replay: None,
    };

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![echo_tool]),
        settings: Some(Settings {
            retry: Some(RetrySettings {
                enabled: Some(true),
                max_retries: Some(3),
                base_delay_ms: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;

    let mut args = indexmap::IndexMap::new();
    args.insert("text".to_string(), Value::String("hello".to_string()));
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
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call("echo", args, None))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("final answer".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("follow-up ok".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("test", None).await.unwrap();
    assert_eq!(harness.faux.state().call_count(), 3);
    assert_eq!(
        *tool_runs.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["hello".to_string()]
    );
    assert!(!harness.session.is_streaming());

    harness.session.prompt("follow-up", None).await.unwrap();
    assert_eq!(harness.faux.state().call_count(), 4);
    harness.cleanup();
}

#[tokio::test]
async fn emits_extension_events_before_public_event_subscribers() {
    let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let ord_ms = Arc::clone(&order);
    let ord_me = Arc::clone(&order);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let o1 = Arc::clone(&ord_ms);
        let o2 = Arc::clone(&ord_me);
        async move {
            pi.on::<MessageStartEvent>(move |ev, _ctx| {
                let ord = Arc::clone(&o1);
                Box::pin(async move {
                    let role = ev.message.role().unwrap_or("unknown");
                    ord.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(format!("extension:message_start:{role}"));
                    Ok(None)
                })
            });
            pi.on::<MessageEndEvent>(move |ev, _ctx| {
                let ord = Arc::clone(&o2);
                Box::pin(async move {
                    let role = ev.message.role().unwrap_or("unknown");
                    ord.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(format!("extension:message_end:{role}"));
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

    let ord_pub = Arc::clone(&order);
    let _unsub = harness.session.subscribe(move |ev| match ev {
        AgentSessionEvent::MessageStart { message } => {
            let role = message.role().unwrap_or("unknown");
            ord_pub
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("public:message_start:{role}"));
        }
        AgentSessionEvent::MessageEnd { message } => {
            let role = message.role().unwrap_or("unknown");
            ord_pub
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("public:message_end:{role}"));
        }
        _ => {}
    });

    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("done".to_string()),
        None,
    ))]);

    harness.session.prompt("hi", None).await.unwrap();

    assert_eq!(
        *order.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            "extension:message_start:system",
            "public:message_start:system",
            "extension:message_end:system",
            "public:message_end:system",
            "extension:message_start:user",
            "public:message_start:user",
            "extension:message_end:user",
            "public:message_end:user",
            "extension:message_start:assistant",
            "public:message_start:assistant",
            "extension:message_end:assistant",
            "public:message_end:assistant",
        ]
    );
    harness.cleanup();
}

#[tokio::test]
async fn emits_the_expected_event_order_for_a_single_prompt() {
    let harness = create_harness(HarnessOptions::default()).await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("hello".to_string()),
        None,
    ))]);
    harness.session.prompt("hi", None).await.unwrap();

    assert_eq!(
        normalize_event_order(&harness.events()),
        vec![
            "agent_start",
            "turn_start",
            "message_start:system",
            "message_end:system",
            "message_start:user",
            "message_end:user",
            "message_start:assistant",
            "message_update",
            "message_end:assistant",
            "turn_end",
            "agent_end",
            "agent_settled",
        ]
    );
    harness.cleanup();
}

#[tokio::test]
async fn emits_the_expected_event_order_for_a_tool_call_turn() {
    let tool_runs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let tr_ref = Arc::clone(&tool_runs);
    let echo_tool = AgentTool {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "Echo text back".to_string(),
        parameters: t::object([("text", t::string())]).into(),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, params, _sig, _upd| {
            let runs = Arc::clone(&tr_ref);
            Box::pin(async move {
                let text = params.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                runs.lock().unwrap_or_else(PoisonError::into_inner).push(text.clone());
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(format!("echo:{text}")))],
                    details: Some(json!({ "text": text })),
                    ..Default::default()
                })
            })
        }),
        replay: None,
    };

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![echo_tool]),
        ..Default::default()
    })
    .await;
    let mut args = indexmap::IndexMap::new();
    args.insert("text".to_string(), Value::String("hello".to_string()));
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call("echo", args, None))),
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

    harness.session.prompt("hi", None).await.unwrap();

    assert_eq!(
        *tool_runs.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["hello".to_string()]
    );
    assert_eq!(
        normalize_event_order(&harness.events()),
        vec![
            "agent_start",
            "turn_start",
            "message_start:system",
            "message_end:system",
            "message_start:user",
            "message_end:user",
            "message_start:assistant",
            "message_update",
            "message_end:assistant",
            "tool_execution_start:echo",
            "tool_execution_end:echo",
            "message_start:toolResult",
            "message_end:toolResult",
            "turn_end",
            "turn_start",
            "message_start:assistant",
            "message_update",
            "message_end:assistant",
            "turn_end",
            "agent_end",
            "agent_settled",
        ]
    );
    harness.cleanup();
}

#[tokio::test]
async fn emits_streaming_deltas_for_text_thinking_and_tool_calls_in_message_update_events() {
    let harness = create_harness(HarnessOptions::default()).await;
    let mut args = indexmap::IndexMap::new();
    args.insert("text".to_string(), Value::String("hello".to_string()));
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![
            FauxContentBlock::Thinking(faux_thinking("plan")),
            FauxContentBlock::Text(TextContent::new("answer")),
            FauxContentBlock::ToolCall(faux_tool_call("echo", args, None)),
        ]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    ))]);

    let _ = harness.session.prompt("hi", None).await;

    let update_types: Vec<&'static str> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::MessageUpdate {
                assistant_message_event,
                ..
            } => Some(match assistant_message_event {
                AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
                AssistantMessageEvent::TextDelta { .. } => "text_delta",
                AssistantMessageEvent::ToolCallDelta { .. } => "toolcall_delta",
                _ => "other",
            }),
            _ => None,
        })
        .collect();
    assert!(update_types.contains(&"thinking_delta"));
    assert!(update_types.contains(&"text_delta"));
    assert!(update_types.contains(&"toolcall_delta"));
    harness.cleanup();
}

#[tokio::test]
async fn emits_agent_end_for_error_responses() {
    let harness = create_harness(HarnessOptions::default()).await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text(String::new()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("broken".to_string()),
            ..Default::default()
        }),
    ))]);

    harness.session.prompt("hi", None).await.unwrap();

    let events = harness.events();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentSessionEvent::AgentEnd { .. }))
            .count(),
        1
    );
    assert!(matches!(events.last(), Some(AgentSessionEvent::AgentSettled { .. })));
    harness.cleanup();
}

#[tokio::test]
async fn emits_agent_end_for_aborted_runs_and_persists_the_aborted_assistant_message() {
    let harness = create_harness(HarnessOptions::default()).await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("x".repeat(20_000)),
        None,
    ))]);

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let tx_slot = Arc::new(Mutex::new(Some(tx)));
    let _unsub = harness.session.subscribe(move |ev| {
        if matches!(ev, AgentSessionEvent::MessageUpdate { .. })
            && let Some(sender) = tx_slot.lock().unwrap_or_else(PoisonError::into_inner).take()
        {
            let _ = sender.send(());
        }
    });

    let sess = harness.session.clone();
    let prompt_task = tokio::spawn(async move { sess.prompt("hi", None).await });
    let _ = rx.await;
    harness.session.abort().await;
    let _ = prompt_task.await;

    let events = harness.events();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentSessionEvent::AgentEnd { .. }))
            .count(),
        1
    );
    assert!(matches!(events.last(), Some(AgentSessionEvent::AgentSettled { .. })));
    let msgs = harness.session.messages();
    match msgs.last() {
        Some(pi_agent_core::types::AgentMessage::Message(pi_ai::types::Message::Assistant(a))) => {
            assert_eq!(a.stop_reason, StopReason::Aborted);
        }
        other => panic!("expected aborted Assistant message, got {other:?}"),
    }
    harness.cleanup();
}
