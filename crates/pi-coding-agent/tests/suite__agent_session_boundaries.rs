//! Port of packages/coding-agent/test/suite/agent-session-boundaries.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pi_agent_core::types::{AgentMessage, AgentTool, AgentToolResult};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxModelDefinition, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{Message, StopReason, TextContent, Usage, UserContentBlock, UserMessage, UserMessageContent};
use pi_coding_agent::core::agent_session::AgentSessionEvent;
use pi_coding_agent::core::extensions::types::{
    AgentActivityOutcome, AgentBeforeSettleEvent, AgentEndEvent, AgentSettledEvent, AgentStartEvent, BoundaryResult,
    ContextEditReplacement, CustomMessageContent, DeliverAs, ExtensionApi, InlineExtension, InputDeliveryBehavior,
    MessageEndEvent, MessageEndEventResult, SendMessageOptions, SendUserMessageOptions, SessionBeforeCompactEvent,
    SessionBeforeCompactResult, SessionBoundaryDraft, SessionEntry, TurnEndEvent,
};
use pi_coding_agent::core::session_manager::CompactionEntry;
use pi_coding_agent::core::settings_manager::{CompactionSettings, RetrySettings, Settings};
use pi_coding_agent::core::tools::mcp_wire::CompactionResult;
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::suite::harness::{Harness, HarnessOptions, create_harness, get_message_text};
use tokio::sync::Notify;

fn faux_usage(input: i64, output: i64) -> Usage {
    Usage {
        input,
        output,
        total_tokens: input + output,
        ..Default::default()
    }
}

fn find_compaction_entries(harness: &Harness) -> Vec<CompactionEntry> {
    harness
        .session_manager
        .get_entries()
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::Compaction(c) => Some(c),
            _ => None,
        })
        .collect()
}

fn has_compaction_start(harness: &Harness) -> bool {
    harness
        .events()
        .into_iter()
        .any(|e| matches!(e, AgentSessionEvent::CompactionStart { .. }))
}

fn count_event<F>(harness: &Harness, pred: F) -> usize
where
    F: Fn(&AgentSessionEvent) -> bool,
{
    harness.events().into_iter().filter(pred).count()
}

#[tokio::test]
async fn commits_a_retain_none_turn_end_compaction_and_explicitly_continues_once() {
    let handled = Arc::new(AtomicBool::new(false));
    let observed_ids = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));

    let handled_c = Arc::clone(&handled);
    let ids_c = Arc::clone(&observed_ids);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let h = Arc::clone(&handled_c);
        let ids = Arc::clone(&ids_c);
        async move {
            pi.on::<TurnEndEvent>(move |event, _ctx| {
                let h = Arc::clone(&h);
                let ids = Arc::clone(&ids);
                Box::pin(async move {
                    ids.lock().unwrap_or_else(PoisonError::into_inner).push(event.message_entry_id);
                    if h.swap(true, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    Ok(Some(BoundaryResult {
                        entries: Some(vec![SessionBoundaryDraft::Compaction {
                            summary: "exact handoff".to_string(),
                            first_kept_entry_id: None,
                            details: Some(json!({ "source": "test" })),
                            usage: None,
                        }]),
                        r#continue: Some(true),
                    }))
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

    let reqs_c = Arc::clone(&requests);
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("discarded response".to_string()),
            None,
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let reqs = Arc::clone(&reqs_c);
            Box::pin(async move {
                reqs.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(pi_js::json::stringify(&ctx.messages));
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("continued from handoff".to_string()),
                    None,
                ))
            })
        })),
    ]);

    harness.session.prompt("discarded prompt", None).await.unwrap();

    let compactions = find_compaction_entries(&harness);
    assert_eq!(compactions.len(), 1);
    assert_eq!(compactions[0].summary, "exact handoff");
    assert_eq!(compactions[0].first_kept_entry_id, compactions[0].id);
    let recorded = requests.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].contains("exact handoff"));
    assert!(!recorded[0].contains("discarded prompt"));
    assert!(!recorded[0].contains("discarded response"));
    assert_eq!(observed_ids.lock().unwrap_or_else(PoisonError::into_inner).len(), 2);
    assert_eq!(
        count_event(&harness, |e| matches!(e, AgentSessionEvent::AgentSettled)),
        1
    );
    harness.cleanup();
}

async fn run_queue_scheduling_around_handoff(queue_kind: &str) {
    let handled = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let kind = queue_kind.to_string();

    let handled_c = Arc::clone(&handled);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let h = Arc::clone(&handled_c);
        let k = kind.clone();
        async move {
            let pi_ev = pi.clone();
            pi.on::<TurnEndEvent>(move |_event, _ctx| {
                let h = Arc::clone(&h);
                let pi_h = pi_ev.clone();
                let k = k.clone();
                Box::pin(async move {
                    if h.swap(true, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    if k == "steering" || k == "both" {
                        pi_h.send_user_message(
                            CustomMessageContent::Text("queued steering".to_string()),
                            Some(SendUserMessageOptions {
                                deliver_as: Some(InputDeliveryBehavior::Steer),
                                ..Default::default()
                            }),
                        )?;
                    }
                    if k == "follow-up" || k == "both" {
                        pi_h.send_user_message(
                            CustomMessageContent::Text("queued follow-up".to_string()),
                            Some(SendUserMessageOptions {
                                deliver_as: Some(InputDeliveryBehavior::FollowUp),
                                ..Default::default()
                            }),
                        )?;
                    }
                    Ok(Some(BoundaryResult {
                        entries: Some(vec![SessionBoundaryDraft::Compaction {
                            summary: "exact handoff".to_string(),
                            first_kept_entry_id: None,
                            details: None,
                            usage: None,
                        }]),
                        r#continue: Some(true),
                    }))
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

    let req1 = Arc::clone(&requests);
    let req2 = Arc::clone(&requests);
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("first".to_string()),
            None,
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let reqs = Arc::clone(&req1);
            Box::pin(async move {
                reqs.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(pi_js::json::stringify(&ctx.messages));
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("second".to_string()),
                    None,
                ))
            })
        })),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let reqs = Arc::clone(&req2);
            Box::pin(async move {
                reqs.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(pi_js::json::stringify(&ctx.messages));
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("third".to_string()),
                    None,
                ))
            })
        })),
    ]);

    harness.session.prompt("start", None).await.unwrap();

    let recorded = requests.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(recorded[0].contains("exact handoff"));
    if queue_kind == "steering" {
        assert_eq!(harness.faux.state().call_count, 2);
        assert!(recorded[0].contains("queued steering"));
        assert!(!recorded[0].contains("queued follow-up"));
    } else if queue_kind == "follow-up" {
        assert_eq!(harness.faux.state().call_count, 2);
        assert!(recorded[0].contains("queued follow-up"));
    } else {
        assert_eq!(harness.faux.state().call_count, 3);
        assert!(recorded[0].contains("queued steering"));
        assert!(!recorded[0].contains("queued follow-up"));
        assert!(recorded[1].contains("queued follow-up"));
    }
    harness.cleanup();
}

#[tokio::test]
async fn preserves_steering_queue_scheduling_around_a_turn_end_handoff() {
    run_queue_scheduling_around_handoff("steering").await;
}

#[tokio::test]
async fn preserves_follow_up_queue_scheduling_around_a_turn_end_handoff() {
    run_queue_scheduling_around_handoff("follow-up").await;
}

#[tokio::test]
async fn preserves_both_queue_scheduling_around_a_turn_end_handoff() {
    run_queue_scheduling_around_handoff("both").await;
}

#[tokio::test]
async fn keeps_a_boundary_replacement_verbatim_through_threshold_compaction() {
    let handled = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let instruction = "EXACT-REPLACEMENT-INSTRUCTION ".repeat(100);

    let handled_c = Arc::clone(&handled);
    let instr_c = instruction.clone();
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let h = Arc::clone(&handled_c);
        let instr = instr_c.clone();
        async move {
            pi.on::<TurnEndEvent>(move |event, ctx| {
                let h = Arc::clone(&h);
                let instr = instr.clone();
                Box::pin(async move {
                    if h.swap(true, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    let branch = ctx.session_manager()?.get_branch(None);
                    let user_id = branch
                        .iter()
                        .rev()
                        .find_map(|entry| match entry {
                            SessionEntry::Message(m) if m.message.role() == Some("user") => Some(m.id.clone()),
                            _ => None,
                        })
                        .ok_or_else(|| pi_js::Error::msg("missing user entry"))?;
                    Ok(Some(BoundaryResult {
                        entries: Some(vec![
                            SessionBoundaryDraft::ContextEdit {
                                target_id: user_id,
                                replacement: Some(ContextEditReplacement {
                                    content: Value::String(instr),
                                }),
                            },
                            SessionBoundaryDraft::ContextEdit {
                                target_id: event.message_entry_id,
                                replacement: None,
                            },
                            SessionBoundaryDraft::Custom {
                                custom_type: "bookkeeping".to_string(),
                                data: Some(json!({ "source": "test" })),
                            },
                        ]),
                        r#continue: Some(true),
                    }))
                })
            });
            pi.on::<SessionBeforeCompactEvent>(|event, _ctx| {
                Box::pin(async move {
                    Ok(Some(SessionBeforeCompactResult {
                        compaction: Some(CompactionResult {
                            summary: "older history summary".to_string(),
                            first_kept_entry_id: event.preparation.first_kept_entry_id,
                            tokens_before: event.preparation.tokens_before,
                            estimated_tokens_after: None,
                            usage: None,
                            details: None,
                        }),
                        ..Default::default()
                    }))
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(2_000),
            max_tokens: Some(100),
            ..Default::default()
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
                keep_recent_tokens: Some(1),
                reserve_tokens: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        }),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let now = pi_js::time::now_ms();
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Text("older input".to_string()),
            timestamp: now - 2,
        })))
        .unwrap();
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::Assistant(faux_assistant_message(
            FauxAssistantMessageContent::Text("older answer".to_string()),
            Some(FauxAssistantMessageOptions {
                timestamp: Some(now - 1),
                ..Default::default()
            }),
        ))))
        .unwrap();
    harness.session.refresh_context();

    let reqs_c = Arc::clone(&requests);
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("answered original input".to_string()),
            None,
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let reqs = Arc::clone(&reqs_c);
            Box::pin(async move {
                reqs.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(pi_js::json::stringify(&ctx.messages));
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("answered replacement".to_string()),
                    None,
                ))
            })
        })),
    ]);

    harness.session.prompt("original input", None).await.unwrap();

    assert!(has_compaction_start(&harness));
    let recorded = requests.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].contains("EXACT-REPLACEMENT-INSTRUCTION"));
    harness.cleanup();
}

#[tokio::test]
async fn keeps_boundary_input_verbatim_through_threshold_compaction_when_metadata_follows_it() {
    let handled = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let instruction = "EXACT-UNSENT-INSTRUCTION ".repeat(100);

    let handled_c = Arc::clone(&handled);
    let instr_c = instruction.clone();
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let h = Arc::clone(&handled_c);
        let instr = instr_c.clone();
        async move {
            pi.on::<TurnEndEvent>(move |_event, _ctx| {
                let h = Arc::clone(&h);
                let instr = instr.clone();
                Box::pin(async move {
                    if h.swap(true, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    Ok(Some(BoundaryResult {
                        entries: Some(vec![
                            SessionBoundaryDraft::CustomMessage {
                                custom_type: "next-work".to_string(),
                                content: CustomMessageContent::Text(instr),
                                display: false,
                                details: None,
                            },
                            SessionBoundaryDraft::Custom {
                                custom_type: "bookkeeping".to_string(),
                                data: Some(json!({ "source": "test" })),
                            },
                        ]),
                        r#continue: Some(true),
                    }))
                })
            });
            pi.on::<SessionBeforeCompactEvent>(|event, _ctx| {
                Box::pin(async move {
                    Ok(Some(SessionBeforeCompactResult {
                        compaction: Some(CompactionResult {
                            summary: "older history summary".to_string(),
                            first_kept_entry_id: event.preparation.first_kept_entry_id,
                            tokens_before: event.preparation.tokens_before,
                            estimated_tokens_after: None,
                            usage: None,
                            details: None,
                        }),
                        ..Default::default()
                    }))
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(2_000),
            max_tokens: Some(100),
            ..Default::default()
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
                keep_recent_tokens: Some(1),
                reserve_tokens: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        }),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let reqs_c = Arc::clone(&requests);
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("first".to_string()),
            None,
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let reqs = Arc::clone(&reqs_c);
            Box::pin(async move {
                reqs.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(pi_js::json::stringify(&ctx.messages));
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("second".to_string()),
                    None,
                ))
            })
        })),
    ]);

    let old_input = "old input ".repeat(500);
    harness.session.prompt(&old_input, None).await.unwrap();

    assert!(has_compaction_start(&harness));
    let recorded = requests.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].contains("EXACT-UNSENT-INSTRUCTION"));
    harness.cleanup();
}

#[tokio::test]
async fn refreshes_canonical_context_before_publishing_boundary_entry_notifications() {
    let handled = Arc::new(AtomicBool::new(false));
    let handled_c = Arc::clone(&handled);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let h = Arc::clone(&handled_c);
        async move {
            pi.on::<TurnEndEvent>(move |_event, _ctx| {
                let h = Arc::clone(&h);
                Box::pin(async move {
                    if h.swap(true, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    Ok(Some(BoundaryResult {
                        entries: Some(vec![
                            SessionBoundaryDraft::Custom {
                                custom_type: "metadata".to_string(),
                                data: Some(Value::Bool(true)),
                            },
                            SessionBoundaryDraft::CustomMessage {
                                custom_type: "visible-context".to_string(),
                                content: CustomMessageContent::Text("committed context".to_string()),
                                display: true,
                                details: None,
                            },
                        ]),
                        r#continue: None,
                    }))
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

    let snapshots = Arc::new(Mutex::new(Vec::<String>::new()));
    let snap_c = Arc::clone(&snapshots);
    let session_c = harness.session.clone();
    let _unsub = harness.session.subscribe(move |ev| {
        if matches!(ev, AgentSessionEvent::EntryAppended { .. }) {
            let msgs = session_c.messages();
            snap_c
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(pi_js::json::stringify(&msgs));
        }
    });

    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("done".to_string()),
        None,
    ))]);

    harness.session.prompt("start", None).await.unwrap();

    let recorded = snapshots.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(recorded.len(), 2);
    assert!(recorded.iter().all(|s| s.contains("committed context")));
    harness.cleanup();
}

#[tokio::test]
async fn continues_from_an_agent_before_settle_custom_message_before_final_settlement() {
    let requested = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));

    let req_flag = Arc::clone(&requested);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let rf = Arc::clone(&req_flag);
        async move {
            pi.on::<AgentBeforeSettleEvent>(move |_ev, _ctx| {
                let rf = Arc::clone(&rf);
                Box::pin(async move {
                    if rf.swap(true, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    Ok(Some(BoundaryResult {
                        entries: Some(vec![SessionBoundaryDraft::CustomMessage {
                            custom_type: "test-continuation".to_string(),
                            content: CustomMessageContent::Text("continue now".to_string()),
                            display: false,
                            details: None,
                        }]),
                        r#continue: Some(true),
                    }))
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

    let reqs_c = Arc::clone(&requests);
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("first".to_string()),
            None,
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let reqs = Arc::clone(&reqs_c);
            Box::pin(async move {
                reqs.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(pi_js::json::stringify(&ctx.messages));
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("second".to_string()),
                    None,
                ))
            })
        })),
    ]);

    harness.session.prompt("start", None).await.unwrap();

    let recorded = requests.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(recorded[0].contains("continue now"));
    assert!(harness.session_manager.get_entries().into_iter().any(|e| match e {
        SessionEntry::CustomMessage(cm) => cm.custom_type == "test-continuation" && !cm.display,
        _ => false,
    }));
    assert_eq!(count_event(&harness, |e| matches!(e, AgentSessionEvent::AgentStart)), 2);
    assert_eq!(count_event(&harness, |e| matches!(e, AgentSessionEvent::AgentSettled)), 1);
    harness.cleanup();
}

#[tokio::test]
async fn persists_custom_context_queued_by_agent_end_before_pre_settlement_continuation() {
    let first_run = Arc::new(AtomicBool::new(true));
    let continued = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));

    let fr_c = Arc::clone(&first_run);
    let cont_c = Arc::clone(&continued);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let fr = Arc::clone(&fr_c);
        let cont = Arc::clone(&cont_c);
        async move {
            let pi_ev = pi.clone();
            pi.on::<AgentEndEvent>(move |_ev, _ctx| {
                let fr = Arc::clone(&fr);
                let pi_h = pi_ev.clone();
                Box::pin(async move {
                    if !fr.swap(false, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    pi_h.send_message(
                        pi_coding_agent::core::tools::mcp_wire::CustomMessageDraft {
                            custom_type: "agent-end-context".to_string(),
                            content: CustomMessageContent::Text("queued after agent end".to_string()),
                            display: false,
                            details: None,
                        },
                        Some(SendMessageOptions {
                            trigger_turn: Some(false),
                            deliver_as: None,
                        }),
                    )?;
                    Ok(None)
                })
            });
            pi.on::<AgentBeforeSettleEvent>(move |ev, _ctx| {
                let cont = Arc::clone(&cont);
                Box::pin(async move {
                    if cont.swap(true, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    let pending_json = pi_js::json::stringify(&ev.context.pending_messages);
                    let context_json = pi_js::json::stringify(&ev.context.context_messages);
                    assert!(pending_json.contains("queued after agent end"));
                    assert!(!context_json.contains("queued after agent end"));
                    Ok(Some(BoundaryResult {
                        entries: None,
                        r#continue: Some(true),
                    }))
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

    let reqs_c = Arc::clone(&requests);
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("first".to_string()),
            None,
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let reqs = Arc::clone(&reqs_c);
            Box::pin(async move {
                reqs.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(pi_js::json::stringify(&ctx.messages));
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("second".to_string()),
                    None,
                ))
            })
        })),
    ]);

    harness.session.prompt("start", None).await.unwrap();

    let recorded = requests.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(recorded[0].contains("queued after agent end"));
    assert!(harness.session_manager.get_entries().into_iter().any(|e| match e {
        SessionEntry::CustomMessage(cm) => cm.custom_type == "agent-end-context",
        _ => false,
    }));
    harness.cleanup();
}

#[tokio::test]
async fn keeps_a_pre_settlement_follow_up_deferred_until_the_explicit_continuation_would_stop() {
    let handled = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));

    let handled_c = Arc::clone(&handled);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let h = Arc::clone(&handled_c);
        async move {
            let pi_ev = pi.clone();
            pi.on::<AgentBeforeSettleEvent>(move |_ev, _ctx| {
                let h = Arc::clone(&h);
                let pi_h = pi_ev.clone();
                Box::pin(async move {
                    if h.swap(true, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    pi_h.send_user_message(
                        CustomMessageContent::Text("queued follow-up".to_string()),
                        Some(SendUserMessageOptions {
                            deliver_as: Some(InputDeliveryBehavior::FollowUp),
                            ..Default::default()
                        }),
                    )?;
                    Ok(Some(BoundaryResult {
                        entries: Some(vec![SessionBoundaryDraft::CustomMessage {
                            custom_type: "boundary".to_string(),
                            content: CustomMessageContent::Text("boundary context".to_string()),
                            display: false,
                            details: None,
                        }]),
                        r#continue: Some(true),
                    }))
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

    let req1 = Arc::clone(&requests);
    let req2 = Arc::clone(&requests);
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("first".to_string()),
            None,
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let reqs = Arc::clone(&req1);
            Box::pin(async move {
                reqs.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(pi_js::json::stringify(&ctx.messages));
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("second".to_string()),
                    None,
                ))
            })
        })),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let reqs = Arc::clone(&req2);
            Box::pin(async move {
                reqs.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(pi_js::json::stringify(&ctx.messages));
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("follow-up response".to_string()),
                    None,
                ))
            })
        })),
    ]);

    harness.session.prompt("start", None).await.unwrap();

    assert_eq!(harness.faux.state().call_count, 3);
    let recorded = requests.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(recorded[0].contains("boundary context"));
    assert!(!recorded[0].contains("queued follow-up"));
    assert!(recorded[1].contains("queued follow-up"));
    harness.cleanup();
}

#[tokio::test]
async fn defers_runs_started_by_agent_settled_handlers_until_every_settled_handler_completes() {
    let triggered = Arc::new(AtomicBool::new(false));
    let lifecycle = Arc::new(Mutex::new(Vec::<String>::new()));

    let trig_c = Arc::clone(&triggered);
    let life_c = Arc::clone(&lifecycle);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let tr = Arc::clone(&trig_c);
        let lf = Arc::clone(&life_c);
        async move {
            let lf1 = Arc::clone(&lf);
            pi.on::<AgentStartEvent>(move |_ev, _ctx| {
                let lf1 = Arc::clone(&lf1);
                Box::pin(async move {
                    lf1.lock().unwrap_or_else(PoisonError::into_inner).push("start".to_string());
                    Ok(None)
                })
            });

            let lf2 = Arc::clone(&lf);
            let pi_ev = pi.clone();
            pi.on::<AgentSettledEvent>(move |_ev, ctx| {
                let tr = Arc::clone(&tr);
                let lf2 = Arc::clone(&lf2);
                let pi_h = pi_ev.clone();
                Box::pin(async move {
                    let idle = ctx.is_idle()?;
                    lf2.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(format!("settled-first:{idle}"));
                    if tr.swap(true, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    pi_h.send_message(
                        pi_coding_agent::core::tools::mcp_wire::CustomMessageDraft {
                            custom_type: "settled-trigger".to_string(),
                            content: CustomMessageContent::Text("start later".to_string()),
                            display: false,
                            details: None,
                        },
                        Some(SendMessageOptions {
                            trigger_turn: Some(true),
                            deliver_as: None,
                        }),
                    )?;
                    Ok(None)
                })
            });

            let lf3 = Arc::clone(&lf);
            pi.on::<AgentSettledEvent>(move |_ev, ctx| {
                let lf3 = Arc::clone(&lf3);
                Box::pin(async move {
                    let idle = ctx.is_idle()?;
                    lf3.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(format!("settled-second:{idle}"));
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

    harness.session.prompt("start", None).await.unwrap();

    assert_eq!(
        *lifecycle.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            "start",
            "settled-first:true",
            "settled-second:true",
            "start",
            "settled-first:true",
            "settled-second:true",
        ]
    );
    harness.cleanup();
}

#[tokio::test]
async fn does_not_let_an_invalid_explicit_continuation_suppress_natural_tool_continuation() {
    let tool = AgentTool {
        name: "noop".to_string(),
        label: "Noop".to_string(),
        description: "Noop".to_string(),
        parameters: t::object(&[]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, _params, _sig, _upd| {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("done"))],
                    details: Some(json!({})),
                    structured_content: None,
                    usage: None,
                })
            })
        }),
        replay: None,
    };

    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<TurnEndEvent>(|event, ctx| {
            Box::pin(async move {
                let branch = ctx.session_manager()?.get_branch(None);
                let user_id = branch
                    .iter()
                    .rev()
                    .find_map(|entry| match entry {
                        SessionEntry::Message(m) if m.message.role() == Some("user") => Some(m.id.clone()),
                        _ => None,
                    })
                    .ok_or_else(|| pi_js::Error::msg("missing user entry"))?;
                let mut targets = vec![user_id, event.message_entry_id];
                targets.extend(event.tool_result_entry_ids);
                Ok(Some(BoundaryResult {
                    entries: Some(
                        targets
                            .into_iter()
                            .map(|target_id| SessionBoundaryDraft::ContextEdit {
                                target_id,
                                replacement: None,
                            })
                            .collect(),
                    ),
                    r#continue: Some(true),
                }))
            })
        });
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![tool]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call("noop", json!({}))]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("must not run".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("start", None).await.unwrap();

    assert_eq!(harness.faux.state().call_count, 2);
    harness.cleanup();
}

#[tokio::test]
async fn dispatches_actionable_turn_end_for_synthetic_run_failures() {
    let turn_ends = Arc::new(AtomicUsize::new(0));
    let te_c = Arc::clone(&turn_ends);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let te = Arc::clone(&te_c);
        async move {
            pi.on::<TurnEndEvent>(move |event, _ctx| {
                let te = Arc::clone(&te);
                Box::pin(async move {
                    te.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(event.outcome, AgentActivityOutcome::Error);
                    Ok(Some(BoundaryResult {
                        entries: Some(vec![SessionBoundaryDraft::Custom {
                            custom_type: "failure-boundary".to_string(),
                            data: Some(Value::Bool(true)),
                        }]),
                        r#continue: None,
                    }))
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
    harness.session.agent().set_prepare_request(Some(Arc::new(|_ctx, _sig| {
        Box::pin(async { Err(pi_js::Error::msg("request preparation failed")) })
    })));

    harness.session.prompt("start", None).await.unwrap();

    assert_eq!(turn_ends.load(Ordering::SeqCst), 1);
    assert_eq!(harness.faux.state().call_count, 0);
    assert!(harness.session_manager.get_entries().into_iter().any(|e| match e {
        SessionEntry::Custom(c) => c.custom_type == "failure-boundary",
        _ => false,
    }));
    harness.cleanup();
}

#[tokio::test]
async fn does_not_compact_from_usage_belonging_to_a_boundary_omitted_assistant() {
    let handled = Arc::new(AtomicBool::new(false));
    let h_c = Arc::clone(&handled);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let h = Arc::clone(&h_c);
        async move {
            pi.on::<MessageEndEvent>(|event, _ctx| {
                Box::pin(async move {
                    let AgentMessage::Message(Message::Assistant(mut a)) = event.message else {
                        return Ok(None);
                    };
                    a.usage.input = 9_800;
                    a.usage.output = 1;
                    a.usage.total_tokens = 9_801;
                    Ok(Some(MessageEndEventResult {
                        message: Some(AgentMessage::from(Message::Assistant(a))),
                    }))
                })
            });
            pi.on::<TurnEndEvent>(move |event, _ctx| {
                let h = Arc::clone(&h);
                Box::pin(async move {
                    if h.swap(true, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    Ok(Some(BoundaryResult {
                        entries: Some(vec![SessionBoundaryDraft::ContextEdit {
                            target_id: event.message_entry_id,
                            replacement: None,
                        }]),
                        r#continue: None,
                    }))
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(10_000),
            max_tokens: Some(100),
            ..Default::default()
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
                keep_recent_tokens: Some(1),
                reserve_tokens: Some(300),
                ..Default::default()
            }),
            ..Default::default()
        }),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("short response".to_string()),
        None,
    ))]);

    harness.session.prompt("small prompt", None).await.unwrap();

    assert!(!has_compaction_start(&harness));
    let tokens = harness.session.get_context_usage().and_then(|u| u.tokens).unwrap_or(0);
    assert!(tokens < 2_000);
    harness.cleanup();
}

#[tokio::test]
async fn does_not_trigger_successful_response_overflow_from_usage_captured_before_a_boundary_edit() {
    let handled = Arc::new(AtomicBool::new(false));
    let h_c = Arc::clone(&handled);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let h = Arc::clone(&h_c);
        async move {
            pi.on::<MessageEndEvent>(|event, _ctx| {
                Box::pin(async move {
                    let AgentMessage::Message(Message::Assistant(mut a)) = event.message else {
                        return Ok(None);
                    };
                    a.usage.input = 5_100;
                    a.usage.output = 1;
                    a.usage.total_tokens = 5_101;
                    Ok(Some(MessageEndEventResult {
                        message: Some(AgentMessage::from(Message::Assistant(a))),
                    }))
                })
            });
            pi.on::<TurnEndEvent>(move |_event, ctx| {
                let h = Arc::clone(&h);
                Box::pin(async move {
                    if h.swap(true, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    let branch = ctx.session_manager()?.get_branch(None);
                    let user_id = branch
                        .iter()
                        .rev()
                        .find_map(|entry| match entry {
                            SessionEntry::Message(m) if m.message.role() == Some("user") => Some(m.id.clone()),
                            _ => None,
                        })
                        .ok_or_else(|| pi_js::Error::msg("missing user entry"))?;
                    Ok(Some(BoundaryResult {
                        entries: Some(vec![SessionBoundaryDraft::ContextEdit {
                            target_id: user_id,
                            replacement: None,
                        }]),
                        r#continue: None,
                    }))
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(5_000),
            max_tokens: Some(100),
            ..Default::default()
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
                keep_recent_tokens: Some(1),
                reserve_tokens: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        }),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("done".to_string()),
        None,
    ))]);

    harness
        .session
        .prompt("large input that is later omitted", None)
        .await
        .unwrap();

    assert!(!has_compaction_start(&harness));
    let tokens = harness.session.get_context_usage().and_then(|u| u.tokens).unwrap_or(0);
    assert!(tokens < 2_000);
    harness.cleanup();
}

#[tokio::test]
async fn does_not_trigger_threshold_compaction_from_post_edit_usage_captured_before_a_later_compaction() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(10_000),
            max_tokens: Some(100),
            ..Default::default()
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
                keep_recent_tokens: Some(1),
                reserve_tokens: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;

    let now = pi_js::time::now_ms();
    let user_id = harness
        .session_manager
        .append_message(AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Text("small input".to_string()),
            timestamp: now - 3,
        })))
        .unwrap();
    harness
        .session_manager
        .append_context_edit(
            &user_id,
            Some(ContextEditReplacement {
                content: Value::String("edited input".to_string()),
            }),
        )
        .unwrap();
    let mut response = faux_assistant_message(
        FauxAssistantMessageContent::Text("answer".to_string()),
        Some(FauxAssistantMessageOptions {
            timestamp: Some(now - 2),
            ..Default::default()
        }),
    );
    response.usage = faux_usage(50_000, 1);
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::Assistant(response)))
        .unwrap();
    harness
        .session_manager
        .append_compaction("small summary", Some(&user_id), 50_001, None, None, None)
        .unwrap();
    harness.session.refresh_context();

    let error = faux_assistant_message(
        FauxAssistantMessageContent::Text(String::new()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("invalid_api_key".to_string()),
            timestamp: Some(now + 1_000),
            ..Default::default()
        }),
    );

    harness.session.check_compaction_for_test(&error, false).await;

    assert!(!has_compaction_start(&harness));
    harness.cleanup();
}

#[tokio::test]
async fn does_not_treat_retained_pre_compaction_assistant_usage_as_post_compaction_usage() {
    let harness = create_harness(HarnessOptions::default()).await;
    let mut retained = faux_assistant_message(FauxAssistantMessageContent::Text("retained".to_string()), None);
    retained.usage = faux_usage(10_000, 1);
    let retained_id = harness
        .session_manager
        .append_message(AgentMessage::from(Message::Assistant(retained)))
        .unwrap();
    harness
        .session_manager
        .append_compaction("summary", Some(&retained_id), 10_001, None, None, None)
        .unwrap();
    harness.session.refresh_context();

    assert_eq!(harness.session.get_context_usage().and_then(|u| u.tokens), None);
    harness.cleanup();
}

#[tokio::test]
async fn persists_custom_context_sent_during_pre_settlement_before_continuing() {
    let handled = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));

    let h_c = Arc::clone(&handled);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let h = Arc::clone(&h_c);
        async move {
            let pi_ev = pi.clone();
            pi.on::<AgentBeforeSettleEvent>(move |_ev, _ctx| {
                let h = Arc::clone(&h);
                let pi_h = pi_ev.clone();
                Box::pin(async move {
                    if h.swap(true, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    pi_h.send_message(
                        pi_coding_agent::core::tools::mcp_wire::CustomMessageDraft {
                            custom_type: "pending-boundary".to_string(),
                            content: CustomMessageContent::Text("persist before continue".to_string()),
                            display: false,
                            details: None,
                        },
                        Some(SendMessageOptions {
                            trigger_turn: Some(false),
                            deliver_as: None,
                        }),
                    )?;
                    Ok(Some(BoundaryResult {
                        entries: None,
                        r#continue: Some(true),
                    }))
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

    let reqs_c = Arc::clone(&requests);
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("first".to_string()),
            None,
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let reqs = Arc::clone(&reqs_c);
            Box::pin(async move {
                reqs.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(pi_js::json::stringify(&ctx.messages));
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("second".to_string()),
                    None,
                ))
            })
        })),
    ]);

    harness.session.prompt("start", None).await.unwrap();

    assert_eq!(harness.faux.state().call_count, 2);
    let recorded = requests.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(recorded[0].contains("persist before continue"));
    assert!(harness.session_manager.get_entries().into_iter().any(|e| match e {
        SessionEntry::CustomMessage(cm) => cm.custom_type == "pending-boundary",
        _ => false,
    }));
    harness.cleanup();
}

#[tokio::test]
async fn does_not_consume_queued_input_when_pre_settlement_drafts_leave_system_only_context() {
    let handled = Arc::new(AtomicBool::new(false));
    let h_c = Arc::clone(&handled);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let h = Arc::clone(&h_c);
        async move {
            let pi_ev = pi.clone();
            pi.on::<AgentBeforeSettleEvent>(move |_ev, ctx| {
                let h = Arc::clone(&h);
                let pi_h = pi_ev.clone();
                Box::pin(async move {
                    if h.swap(true, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    pi_h.send_user_message(
                        CustomMessageContent::Text("still queued".to_string()),
                        Some(SendUserMessageOptions {
                            deliver_as: Some(InputDeliveryBehavior::FollowUp),
                            ..Default::default()
                        }),
                    )?;
                    let targets: Vec<SessionBoundaryDraft> = ctx
                        .session_manager()?
                        .get_branch(None)
                        .into_iter()
                        .filter_map(|entry| match entry {
                            SessionEntry::Message(m)
                                if matches!(m.message.role(), Some("user" | "assistant" | "toolResult")) =>
                            {
                                Some(SessionBoundaryDraft::ContextEdit {
                                    target_id: m.id,
                                    replacement: None,
                                })
                            }
                            _ => None,
                        })
                        .collect();
                    Ok(Some(BoundaryResult {
                        entries: Some(targets),
                        r#continue: Some(true),
                    }))
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
            FauxAssistantMessageContent::Text("must not run".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("start", None).await.unwrap();

    assert_eq!(harness.faux.state().call_count, 1);
    assert_eq!(harness.session.pending_message_count(), 1);
    harness.cleanup();
}

#[tokio::test]
async fn commits_pre_settlement_drafts_but_suppresses_continuation_when_aborted_during_the_hook() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());

    let started_c = Arc::clone(&started);
    let release_c = Arc::clone(&release);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let st = Arc::clone(&started_c);
        let rl = Arc::clone(&release_c);
        async move {
            pi.on::<AgentBeforeSettleEvent>(move |_ev, _ctx| {
                let st = Arc::clone(&st);
                let rl = Arc::clone(&rl);
                Box::pin(async move {
                    let notified = rl.notified();
                    st.notify_one();
                    notified.await;
                    Ok(Some(BoundaryResult {
                        entries: Some(vec![SessionBoundaryDraft::Custom {
                            custom_type: "committed-after-abort".to_string(),
                            data: Some(Value::Bool(true)),
                        }]),
                        r#continue: Some(true),
                    }))
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
            FauxAssistantMessageContent::Text("must not run".to_string()),
            None,
        )),
    ]);

    let prompt_fut = harness.session.prompt("start", None);
    let started_wait = started.notified();
    tokio::pin!(prompt_fut);
    let ((), prompt_res) = tokio::join!(
        async {
            started_wait.await;
            let abort_fut = harness.session.abort();
            release.notify_one();
            abort_fut.await;
        },
        &mut prompt_fut,
    );
    prompt_res.unwrap();

    assert_eq!(harness.faux.state().call_count, 1);
    assert!(harness.session_manager.get_entries().into_iter().any(|e| match e {
        SessionEntry::Custom(c) => c.custom_type == "committed-after-abort" && c.data == Some(Value::Bool(true)),
        _ => false,
    }));
    assert_eq!(
        count_event(&harness, |e| matches!(e, AgentSessionEvent::AgentSettled)),
        1
    );
    harness.cleanup();
}

#[tokio::test]
async fn keeps_truncated_tool_attempts_in_context_for_the_natural_next_turn() {
    let executed = Arc::new(AtomicBool::new(false));
    let exec_c = Arc::clone(&executed);
    let tool = AgentTool {
        name: "restricted_truncated_tool".to_string(),
        label: "Restricted truncated tool".to_string(),
        description: "Must not execute from a length response".to_string(),
        parameters: t::object(&[t::prop("value", t::string())]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, _sig, _upd| {
            let ex = Arc::clone(&exec_c);
            Box::pin(async move {
                ex.store(true, Ordering::SeqCst);
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("executed"))],
                    details: Some(json!({})),
                    structured_content: None,
                    usage: None,
                })
            })
        }),
        replay: None,
    };

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![tool]),
        ..Default::default()
    })
    .await;

    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let reqs_c = Arc::clone(&requests);
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call(
                "restricted_truncated_tool",
                json!({ "value": "partial" }),
            )]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Length),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let reqs = Arc::clone(&reqs_c);
            Box::pin(async move {
                reqs.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(pi_js::json::stringify(&ctx.messages));
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("completed natural continuation".to_string()),
                    None,
                ))
            })
        })),
    ]);

    harness.session.prompt("start", None).await.unwrap();

    assert!(!executed.load(Ordering::SeqCst));
    assert_eq!(harness.faux.state().call_count, 2);
    let recorded = requests.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(recorded[0].contains("may be truncated"));
    assert!(
        !harness
            .session_manager
            .get_entries()
            .into_iter()
            .any(|e| matches!(e, SessionEntry::ContextEdit(_)))
    );
    harness.cleanup();
}

#[tokio::test]
async fn resets_length_recovery_after_a_successful_intermediate_assistant_turn() {
    let tool = AgentTool {
        name: "noop".to_string(),
        label: "Noop".to_string(),
        description: "Noop".to_string(),
        parameters: t::object(&[]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, _params, _sig, _upd| {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("done"))],
                    details: Some(json!({})),
                    structured_content: None,
                    usage: None,
                })
            })
        }),
        replay: None,
    };

    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<SessionBeforeCompactEvent>(|event, _ctx| {
            Box::pin(async move {
                Ok(Some(SessionBeforeCompactResult {
                    compaction: Some(CompactionResult {
                        summary: "recovered input".to_string(),
                        first_kept_entry_id: event.preparation.first_kept_entry_id,
                        tokens_before: event.preparation.tokens_before,
                        estimated_tokens_after: None,
                        usage: None,
                        details: None,
                    }),
                    ..Default::default()
                }))
            })
        });
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(1_000),
            max_tokens: Some(100),
            ..Default::default()
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                keep_recent_tokens: Some(1),
                reserve_tokens: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        }),
        tools: Some(vec![tool]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("first partial".to_string()),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Length),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call("noop", json!({}))]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Factory(Arc::new(|_ctx, _opts, _st, _mdl| {
            Box::pin(async {
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("second partial".to_string()),
                    Some(FauxAssistantMessageOptions {
                        stop_reason: Some(StopReason::Length),
                        timestamp: Some(pi_js::time::now_ms() + 1_000),
                        ..Default::default()
                    }),
                ))
            })
        })),
        FauxResponseStep::Factory(Arc::new(|_ctx, _opts, _st, _mdl| {
            Box::pin(async {
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("completed second recovery".to_string()),
                    Some(FauxAssistantMessageOptions {
                        timestamp: Some(pi_js::time::now_ms() + 2_000),
                        ..Default::default()
                    }),
                ))
            })
        })),
    ]);

    harness.session.prompt(&"x".repeat(5_000), None).await.unwrap();

    let entries = harness.session_manager.get_entries();
    let omitted_ids: Vec<String> = entries
        .iter()
        .filter_map(|e| match e {
            SessionEntry::ContextEdit(ce) => Some(ce.target_id.clone()),
            _ => None,
        })
        .collect();
    let length_ids: Vec<String> = entries
        .iter()
        .filter_map(|e| match e {
            SessionEntry::Message(m) => match &m.message {
                AgentMessage::Message(Message::Assistant(a)) if a.stop_reason == StopReason::Length => Some(m.id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(length_ids.len(), 2);
    for id in &length_ids {
        assert!(omitted_ids.contains(id));
    }
    assert_eq!(harness.faux.state().call_count, 3);
    harness.cleanup();
}

#[tokio::test]
async fn gives_a_distinct_queued_follow_up_its_own_length_recovery_budget() {
    let queued = Arc::new(AtomicBool::new(false));
    let q_c = Arc::clone(&queued);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let q = Arc::clone(&q_c);
        async move {
            pi.on::<SessionBeforeCompactEvent>(|event, _ctx| {
                Box::pin(async move {
                    Ok(Some(SessionBeforeCompactResult {
                        compaction: Some(CompactionResult {
                            summary: "recovered input".to_string(),
                            first_kept_entry_id: event.preparation.first_kept_entry_id,
                            tokens_before: event.preparation.tokens_before,
                            estimated_tokens_after: None,
                            usage: None,
                            details: None,
                        }),
                        ..Default::default()
                    }))
                })
            });
            let pi_ev = pi.clone();
            pi.on::<AgentEndEvent>(move |event, _ctx| {
                let q = Arc::clone(&q);
                let pi_h = pi_ev.clone();
                Box::pin(async move {
                    if !event
                        .messages
                        .iter()
                        .any(|m| get_message_text(m) == "first recovered")
                        || q.swap(true, Ordering::SeqCst)
                    {
                        return Ok(None);
                    }
                    pi_h.send_user_message(
                        CustomMessageContent::Text("distinct follow-up".to_string()),
                        Some(SendUserMessageOptions {
                            deliver_as: Some(InputDeliveryBehavior::FollowUp),
                            ..Default::default()
                        }),
                    )?;
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(1_000),
            max_tokens: Some(100),
            ..Default::default()
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                keep_recent_tokens: Some(1),
                reserve_tokens: Some(0),
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
            FauxAssistantMessageContent::Text("first partial".to_string()),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Length),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("first recovered".to_string()),
            None,
        )),
        FauxResponseStep::Factory(Arc::new(|_ctx, _opts, _st, _mdl| {
            Box::pin(async {
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("follow-up partial".to_string()),
                    Some(FauxAssistantMessageOptions {
                        stop_reason: Some(StopReason::Length),
                        timestamp: Some(pi_js::time::now_ms() + 1_000),
                        ..Default::default()
                    }),
                ))
            })
        })),
        FauxResponseStep::Factory(Arc::new(|_ctx, _opts, _st, _mdl| {
            Box::pin(async {
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("follow-up recovered".to_string()),
                    Some(FauxAssistantMessageOptions {
                        timestamp: Some(pi_js::time::now_ms() + 2_000),
                        ..Default::default()
                    }),
                ))
            })
        })),
    ]);

    harness.session.prompt(&"x".repeat(5_000), None).await.unwrap();

    let entries = harness.session_manager.get_entries();
    let length_ids: Vec<String> = entries
        .iter()
        .filter_map(|e| match e {
            SessionEntry::Message(m) => match &m.message {
                AgentMessage::Message(Message::Assistant(a)) if a.stop_reason == StopReason::Length => Some(m.id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let omitted: Vec<String> = entries
        .iter()
        .filter_map(|e| match e {
            SessionEntry::ContextEdit(ce) => Some(ce.target_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(length_ids.len(), 2);
    for id in &length_ids {
        assert!(omitted.contains(id));
    }
    assert_eq!(harness.faux.state().call_count, 4);
    harness.cleanup();
}

#[tokio::test]
async fn finishes_retry_bookkeeping_when_a_retry_receives_a_nonretryable_error() {
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
                error_message: Some("invalid_api_key".to_string()),
                ..Default::default()
            }),
        )),
    ]);

    harness.session.prompt("start", None).await.unwrap();

    assert_eq!(harness.faux.state().call_count, 2);
    assert!(harness.events().into_iter().any(|ev| matches!(
        ev,
        AgentSessionEvent::AutoRetryEnd {
            success: false,
            attempt: 1,
            final_error: Some(ref err),
        } if err == "invalid_api_key"
    )));
    harness.cleanup();
}

#[tokio::test]
async fn omits_a_recoverable_projected_replacement_by_its_source_entry_id() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<SessionBeforeCompactEvent>(|_event, _ctx| {
            Box::pin(async move {
                Ok(Some(SessionBeforeCompactResult {
                    cancel: Some(true),
                    ..Default::default()
                }))
            })
        });
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(1_000),
            max_tokens: Some(100),
            ..Default::default()
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
                keep_recent_tokens: Some(1),
                reserve_tokens: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        }),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let now = pi_js::time::now_ms();
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Text("x".repeat(5_000)),
            timestamp: now - 2,
        })))
        .unwrap();
    let partial = faux_assistant_message(
        FauxAssistantMessageContent::Text("original partial".to_string()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Length),
            timestamp: Some(now - 1),
            ..Default::default()
        }),
    );
    let partial_id = harness
        .session_manager
        .append_message(AgentMessage::from(Message::Assistant(partial)))
        .unwrap();
    harness
        .session_manager
        .append_context_edit(
            &partial_id,
            Some(ContextEditReplacement {
                content: json!([{ "type": "text", "text": "edited partial" }]),
            }),
        )
        .unwrap();
    harness.session.refresh_context();
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("new answer".to_string()),
        None,
    ))]);

    harness.session.prompt("next prompt", None).await.unwrap();

    let last_edit_for_partial = harness
        .session_manager
        .get_entries()
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::ContextEdit(ce) if ce.target_id == partial_id => Some(ce),
            _ => None,
        })
        .last();
    assert_eq!(last_edit_for_partial.and_then(|e| e.replacement), None);
    assert!(
        !harness
            .session_manager
            .build_session_projection()
            .messages
            .iter()
            .any(|m| get_message_text(m) == "edited partial")
    );
    harness.cleanup();
}

#[tokio::test]
async fn recovers_an_explicit_overflow_error_after_a_retained_boundary_replacement() {
    let replaced = Arc::new(AtomicBool::new(false));
    let overflow_id = Arc::new(Mutex::new(Option::<String>::None));

    let rep_c = Arc::clone(&replaced);
    let ov_c = Arc::clone(&overflow_id);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let rep = Arc::clone(&rep_c);
        let ov = Arc::clone(&ov_c);
        async move {
            pi.on::<TurnEndEvent>(move |event, _ctx| {
                let rep = Arc::clone(&rep);
                let ov = Arc::clone(&ov);
                Box::pin(async move {
                    if event.outcome != AgentActivityOutcome::Error || rep.swap(true, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    *ov.lock().unwrap_or_else(PoisonError::into_inner) = Some(event.message_entry_id.clone());
                    Ok(Some(BoundaryResult {
                        entries: Some(vec![SessionBoundaryDraft::ContextEdit {
                            target_id: event.message_entry_id,
                            replacement: Some(ContextEditReplacement {
                                content: json!([{ "type": "text", "text": "retained error" }]),
                            }),
                        }]),
                        r#continue: None,
                    }))
                })
            });
            pi.on::<SessionBeforeCompactEvent>(|event, _ctx| {
                Box::pin(async move {
                    Ok(Some(SessionBeforeCompactResult {
                        compaction: Some(CompactionResult {
                            summary: "recovered overflow".to_string(),
                            first_kept_entry_id: event.preparation.first_kept_entry_id,
                            tokens_before: event.preparation.tokens_before,
                            estimated_tokens_after: None,
                            usage: None,
                            details: None,
                        }),
                        ..Default::default()
                    }))
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(1_000),
            max_tokens: Some(100),
            ..Default::default()
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
                keep_recent_tokens: Some(1),
                reserve_tokens: Some(0),
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
            FauxAssistantMessageContent::Text("retained error".to_string()),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("prompt is too long".to_string()),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("recovered".to_string()),
            None,
        )),
    ]);

    harness.session.prompt(&"x".repeat(5_000), None).await.unwrap();

    assert_eq!(harness.faux.state().call_count, 2);
    let ov_id = overflow_id
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .expect("overflow id");
    let last_edit = harness
        .session_manager
        .get_entries()
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::ContextEdit(ce) if ce.target_id == ov_id => Some(ce),
            _ => None,
        })
        .last();
    assert_eq!(last_edit.and_then(|e| e.replacement), None);
    harness.cleanup();
}

#[tokio::test]
async fn keeps_follow_up_work_behind_an_automatic_error_retry() {
    let queued = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));

    let q_c = Arc::clone(&queued);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let q = Arc::clone(&q_c);
        async move {
            let pi_ev = pi.clone();
            pi.on::<TurnEndEvent>(move |event, _ctx| {
                let q = Arc::clone(&q);
                let pi_h = pi_ev.clone();
                Box::pin(async move {
                    if event.outcome != AgentActivityOutcome::Error || q.swap(true, Ordering::SeqCst) {
                        return Ok(None);
                    }
                    pi_h.send_user_message(
                        CustomMessageContent::Text("queued follow-up".to_string()),
                        Some(SendUserMessageOptions {
                            deliver_as: Some(InputDeliveryBehavior::FollowUp),
                            ..Default::default()
                        }),
                    )?;
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
                max_retries: Some(2),
                base_delay_ms: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let lifecycle = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let lf_c = Arc::clone(&lifecycle);
    let _unsub = harness.session.subscribe(move |ev| match ev {
        AgentSessionEvent::AgentEnd { .. } => {
            lf_c.lock().unwrap_or_else(PoisonError::into_inner).push("agent_end");
        }
        AgentSessionEvent::AutoRetryStart { .. } => {
            lf_c.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push("auto_retry_start");
        }
        _ => {}
    });

    let req1 = Arc::clone(&requests);
    let req2 = Arc::clone(&requests);
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text(String::new()),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("overloaded_error".to_string()),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let reqs = Arc::clone(&req1);
            Box::pin(async move {
                reqs.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(pi_js::json::stringify(&ctx.messages));
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("retry recovered".to_string()),
                    None,
                ))
            })
        })),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let reqs = Arc::clone(&req2);
            Box::pin(async move {
                reqs.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(pi_js::json::stringify(&ctx.messages));
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("follow-up completed".to_string()),
                    None,
                ))
            })
        })),
    ]);

    harness.session.prompt("start", None).await.unwrap();

    assert_eq!(harness.faux.state().call_count, 3);
    let recorded = requests.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(!recorded[0].contains("queued follow-up"));
    assert!(recorded[1].contains("queued follow-up"));
    let lf = lifecycle.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(&lf[..2], &["agent_end", "auto_retry_start"]);
    harness.cleanup();
}

#[tokio::test]
async fn marks_the_exhausted_retry_run_as_final() {
    let harness = create_harness(HarnessOptions {
        settings: Some(Settings {
            retry: Some(RetrySettings {
                enabled: Some(true),
                max_retries: Some(1),
                base_delay_ms: Some(1),
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

    harness.session.prompt("start", None).await.unwrap();

    let agent_ends: Vec<bool> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::AgentEnd { will_retry, .. } => Some(will_retry),
            _ => None,
        })
        .collect();
    assert_eq!(agent_ends, vec![true, false]);
    assert!(harness.events().into_iter().any(|ev| matches!(
        ev,
        AgentSessionEvent::AutoRetryEnd {
            success: false,
            attempt: 1,
            ..
        }
    )));
    harness.cleanup();
}

#[tokio::test]
async fn keeps_omissions_and_does_not_retry_when_recovery_compaction_fails() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(1_000),
            max_tokens: Some(100),
            ..Default::default()
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                keep_recent_tokens: Some(1),
                reserve_tokens: Some(0),
                ..Default::default()
            }),
            retry: Some(RetrySettings {
                enabled: Some(false),
                max_retries: Some(0),
                base_delay_ms: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("partial response".to_string()),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Length),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("summary failed".to_string()),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("summary failed".to_string()),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("must not retry".to_string()),
            None,
        )),
    ]);

    harness.session.prompt(&"x".repeat(5_000), None).await.unwrap();

    let entries = harness.session_manager.get_entries();
    assert!(entries.iter().any(|e| matches!(e, SessionEntry::ContextEdit(_))));
    assert!(!entries.iter().any(|e| matches!(e, SessionEntry::Compaction(_))));
    assert!(entries.iter().any(|e| match e {
        SessionEntry::Message(m) => get_message_text(&m.message) == "partial response",
        _ => false,
    }));
    assert!(
        !harness
            .session_manager
            .build_session_projection()
            .messages
            .iter()
            .any(|m| get_message_text(m) == "partial response")
    );
    assert_eq!(harness.faux.state().call_count, 2);
    harness.cleanup();
}
