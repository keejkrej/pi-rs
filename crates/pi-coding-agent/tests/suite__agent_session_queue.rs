//! Port of packages/coding-agent/test/suite/agent-session-queue.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use pi_agent_core::types::{AgentTool, AgentToolResult, QueueMode};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{StopReason, TextContent, UserContentBlock, UserMessageContent};
use pi_coding_agent::core::agent_session::{
    AgentSessionEvent, CustomMessageDelivery, ExtensionBindings, QueueInputOptions, SendCustomMessageDraft,
    SendCustomMessageOptions,
};
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, InputDeliveryBehavior, InputEvent, InputEventResult, InputSource,
    RegisteredCommandOptions, SendUserMessageOptions,
};
use pi_coding_agent::core::messages::CustomMessageContent;
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::suite::harness::{
    Harness, HarnessOptions, create_harness, get_assistant_texts, get_message_text, get_user_texts,
};

fn user_content_text(content: &UserMessageContent) -> String {
    match content {
        UserMessageContent::Text(t) => t.clone(),
        UserMessageContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|p| match p {
                UserContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

async fn create_waiting_harness(
    extension_factories: Vec<InlineExtension>,
) -> (
    Harness,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<pi_js::Result<()>>,
    tokio::sync::oneshot::Receiver<()>,
) {
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let wait_tool = AgentTool {
        name: "wait".to_string(),
        label: "Wait".to_string(),
        description: "Wait for release".to_string(),
        parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]).into(),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, _sig, _upd| {
            let rx_slot = Arc::clone(&release_rx);
            Box::pin(async move {
                if let Some(rx) = rx_slot.lock().await.take() {
                    let _ = rx.await;
                }
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("released"))],
                    details: Some(json!({})),
                    ..Default::default()
                })
            })
        }),
        replay: None,
    };

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![wait_tool]),
        extension_factories: (!extension_factories.is_empty()).then_some(extension_factories),
        ..Default::default()
    })
    .await;

    let (start_tx, start_rx) = tokio::sync::oneshot::channel::<()>();
    let start_slot = Arc::new(Mutex::new(Some(start_tx)));
    let _unsub = harness.session.subscribe(move |ev| {
        if let AgentSessionEvent::ToolExecutionStart { tool_name, .. } = ev
            && tool_name == "wait"
            && let Some(sender) = start_slot.lock().unwrap_or_else(PoisonError::into_inner).take()
        {
            let _ = sender.send(());
        }
    });

    let sess = harness.session.clone();
    let prompt_handle = tokio::spawn(async move { sess.prompt("start", None).await });
    (harness, release_tx, prompt_handle, start_rx)
}

#[tokio::test]
async fn dispatches_extension_commands_immediately_when_prompted_while_idle() {
    let command_runs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let cr_ref = Arc::clone(&command_runs);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let cr = Arc::clone(&cr_ref);
        async move {
            pi.register_command(
                "testcmd",
                RegisteredCommandOptions {
                    description: Some("Test command".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |args, _ctx| {
                        let cr_inner = Arc::clone(&cr);
                        Box::pin(async move {
                            cr_inner.lock().unwrap_or_else(PoisonError::into_inner).push(args);
                            Ok(())
                        })
                    }),
                },
            )?;
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    harness
        .session
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();

    harness.session.prompt("/testcmd hello world", None).await.unwrap();
    assert_eq!(
        *command_runs.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["hello world".to_string()]
    );
    assert_eq!(harness.get_pending_response_count(), 0);
    assert!(harness.session.messages().is_empty());
    harness.cleanup();
}

#[tokio::test]
async fn delivers_extension_origin_steering_messages_before_the_next_llm_call() {
    let ext_api: Arc<Mutex<Option<ExtensionApi>>> = Arc::new(Mutex::new(None));
    let ea_ref = Arc::clone(&ext_api);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let slot = Arc::clone(&ea_ref);
        async move {
            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(pi);
            Ok(())
        }
    }));

    let (harness, release_tx, prompt_handle, start_rx) = create_waiting_harness(vec![ext]).await;
    harness
        .session
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "wait",
                indexmap::IndexMap::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Factory(Arc::new(|ctx, _opts, _st, _mdl| {
            Box::pin(async move {
                let saw_steer = ctx.messages.iter().any(|m| match m {
                    pi_ai::types::Message::User(u) => user_content_text(&u.content) == "steer now",
                    _ => false,
                });
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text(
                        if saw_steer { "saw steer" } else { "missing steer" }.to_string(),
                    ),
                    None,
                ))
            })
        })),
    ]);

    let _ = start_rx.await;
    let _ = pi_js::time::sleep(1, None).await;

    let api = ext_api.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap();
    api.send_user_message(
        CustomMessageContent::Text("steer now".to_string()),
        Some(SendUserMessageOptions {
            deliver_as: Some(InputDeliveryBehavior::Steer),
            expand_prompt_templates: None,
        }),
    )
    .unwrap();
    let _ = release_tx.send(());
    prompt_handle.await.unwrap().unwrap();

    assert_eq!(get_user_texts(&harness), vec!["start", "steer now"]);
    assert!(get_assistant_texts(&harness).contains(&"saw steer".to_string()));
    harness.cleanup();
}

#[tokio::test]
async fn delivers_follow_up_messages_only_after_the_current_run_finishes() {
    let (harness, release_tx, prompt_handle, start_rx) = create_waiting_harness(vec![]).await;
    let assistant_seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let as_ref = Arc::clone(&assistant_seen);

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "wait",
                indexmap::IndexMap::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let as_inner = Arc::clone(&as_ref);
            Box::pin(async move {
                for m in &ctx.messages {
                    if let pi_ai::types::Message::Assistant(a) = m {
                        let txt = a
                            .content
                            .iter()
                            .filter_map(|b| match b {
                                pi_ai::types::AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        as_inner.lock().unwrap_or_else(PoisonError::into_inner).push(txt);
                    }
                }
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("follow-up response".to_string()),
                    None,
                ))
            })
        })),
    ]);

    let _ = start_rx.await;
    harness
        .session
        .follow_up("after current run", None, None)
        .await
        .unwrap();
    let _ = release_tx.send(());
    prompt_handle.await.unwrap().unwrap();

    assert_eq!(get_user_texts(&harness), vec!["start", "after current run"]);
    assert!(
        assistant_seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&String::new())
    );
    assert!(get_assistant_texts(&harness).contains(&"follow-up response".to_string()));
    harness.cleanup();
}

#[tokio::test]
async fn runs_direct_steering_and_follow_up_messages_through_input_handlers() {
    let input_events: Arc<Mutex<Vec<(String, InputSource, Option<InputDeliveryBehavior>)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let ie_ref = Arc::clone(&input_events);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let ie = Arc::clone(&ie_ref);
        async move {
            pi.on::<InputEvent>(move |ev, _ctx| {
                let list = Arc::clone(&ie);
                Box::pin(async move {
                    list.lock().unwrap_or_else(PoisonError::into_inner).push((
                        ev.text.clone(),
                        ev.source,
                        ev.streaming_behavior,
                    ));
                    if ev.text.starts_with("handle") {
                        return Ok(Some(InputEventResult::Handled));
                    }
                    Ok(Some(InputEventResult::Transform {
                        text: format!("transformed: {}", ev.text),
                        images: None,
                    }))
                })
            });
            Ok(())
        }
    }));

    let (harness, release_tx, prompt_handle, start_rx) = create_waiting_harness(vec![ext]).await;
    harness
        .session
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "wait",
                indexmap::IndexMap::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("steered".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("followed up".to_string()),
            None,
        )),
    ]);

    let _ = start_rx.await;
    input_events.lock().unwrap_or_else(PoisonError::into_inner).clear();

    let rpc_opts = Some(QueueInputOptions {
        source: Some(InputSource::Rpc),
    });
    harness.session.steer("steer me", None, rpc_opts).await.unwrap();
    harness.session.steer("handle steer", None, rpc_opts).await.unwrap();
    harness.session.follow_up("follow me", None, rpc_opts).await.unwrap();
    harness
        .session
        .follow_up("handle follow", None, rpc_opts)
        .await
        .unwrap();

    assert_eq!(
        *input_events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            (
                "steer me".to_string(),
                InputSource::Rpc,
                Some(InputDeliveryBehavior::Steer)
            ),
            (
                "handle steer".to_string(),
                InputSource::Rpc,
                Some(InputDeliveryBehavior::Steer)
            ),
            (
                "follow me".to_string(),
                InputSource::Rpc,
                Some(InputDeliveryBehavior::FollowUp)
            ),
            (
                "handle follow".to_string(),
                InputSource::Rpc,
                Some(InputDeliveryBehavior::FollowUp)
            ),
        ]
    );
    assert_eq!(harness.session.get_steering_messages(), vec!["transformed: steer me"]);
    assert_eq!(harness.session.get_follow_up_messages(), vec!["transformed: follow me"]);

    let _ = release_tx.send(());
    prompt_handle.await.unwrap().unwrap();
    harness.cleanup();
}

#[tokio::test]
async fn delivers_multiple_steering_messages_in_order_in_one_at_a_time_mode() {
    let (harness, release_tx, prompt_handle, start_rx) = create_waiting_harness(vec![]).await;
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "wait",
                indexmap::IndexMap::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("handled steer 1".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("handled steer 2".to_string()),
            None,
        )),
    ]);

    let _ = start_rx.await;
    harness.session.steer("steer 1", None, None).await.unwrap();
    harness.session.steer("steer 2", None, None).await.unwrap();
    let _ = release_tx.send(());
    prompt_handle.await.unwrap().unwrap();

    assert_eq!(get_user_texts(&harness), vec!["start", "steer 1", "steer 2"]);
    assert_eq!(
        get_assistant_texts(&harness),
        vec!["", "handled steer 1", "handled steer 2"]
    );
    harness.cleanup();
}

#[tokio::test]
async fn delivers_multiple_follow_up_messages_in_order_in_one_at_a_time_mode() {
    let (harness, release_tx, prompt_handle, start_rx) = create_waiting_harness(vec![]).await;
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "wait",
                indexmap::IndexMap::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("original turn complete".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("handled follow-up 1".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("handled follow-up 2".to_string()),
            None,
        )),
    ]);

    let _ = start_rx.await;
    harness.session.follow_up("follow-up 1", None, None).await.unwrap();
    harness.session.follow_up("follow-up 2", None, None).await.unwrap();
    let _ = release_tx.send(());
    prompt_handle.await.unwrap().unwrap();

    assert_eq!(get_user_texts(&harness), vec!["start", "follow-up 1", "follow-up 2"]);
    assert_eq!(
        get_assistant_texts(&harness),
        vec![
            "",
            "original turn complete",
            "handled follow-up 1",
            "handled follow-up 2"
        ]
    );
    harness.cleanup();
}

#[tokio::test]
async fn delivers_all_steering_messages_in_one_batch_in_all_mode() {
    let (harness, release_tx, prompt_handle, start_rx) = create_waiting_harness(vec![]).await;
    harness.session.set_steering_mode(QueueMode::All);
    let batched: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let b_ref = Arc::clone(&batched);

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "wait",
                indexmap::IndexMap::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let b = Arc::clone(&b_ref);
            Box::pin(async move {
                let users: Vec<String> = ctx
                    .messages
                    .iter()
                    .filter_map(|m| match m {
                        pi_ai::types::Message::User(u) => Some(user_content_text(&u.content)),
                        _ => None,
                    })
                    .collect();
                *b.lock().unwrap_or_else(PoisonError::into_inner) = users;
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("batched steer response".to_string()),
                    None,
                ))
            })
        })),
    ]);

    let _ = start_rx.await;
    harness.session.steer("steer 1", None, None).await.unwrap();
    harness.session.steer("steer 2", None, None).await.unwrap();
    let _ = release_tx.send(());
    prompt_handle.await.unwrap().unwrap();

    assert_eq!(
        *batched.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["start", "steer 1", "steer 2"]
    );
    assert_eq!(get_assistant_texts(&harness), vec!["", "batched steer response"]);
    harness.cleanup();
}

#[tokio::test]
async fn delivers_all_follow_up_messages_in_one_batch_in_all_mode() {
    let (harness, release_tx, prompt_handle, start_rx) = create_waiting_harness(vec![]).await;
    harness.session.set_follow_up_mode(QueueMode::All);
    let batched: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let b_ref = Arc::clone(&batched);

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "wait",
                indexmap::IndexMap::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("original turn complete".to_string()),
            None,
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let b = Arc::clone(&b_ref);
            Box::pin(async move {
                let users: Vec<String> = ctx
                    .messages
                    .iter()
                    .filter_map(|m| match m {
                        pi_ai::types::Message::User(u) => Some(user_content_text(&u.content)),
                        _ => None,
                    })
                    .collect();
                *b.lock().unwrap_or_else(PoisonError::into_inner) = users;
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("batched follow-up response".to_string()),
                    None,
                ))
            })
        })),
    ]);

    let _ = start_rx.await;
    harness.session.follow_up("follow-up 1", None, None).await.unwrap();
    harness.session.follow_up("follow-up 2", None, None).await.unwrap();
    let _ = release_tx.send(());
    prompt_handle.await.unwrap().unwrap();

    assert_eq!(
        *batched.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["start", "follow-up 1", "follow-up 2"]
    );
    assert_eq!(
        get_assistant_texts(&harness),
        vec!["", "original turn complete", "batched follow-up response"]
    );
    harness.cleanup();
}

#[tokio::test]
async fn queues_custom_messages_with_deliver_as_steer_while_streaming() {
    let (harness, release_tx, prompt_handle, start_rx) = create_waiting_harness(vec![]).await;
    let saw_custom = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sc_ref = Arc::clone(&saw_custom);

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "wait",
                indexmap::IndexMap::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let sc = Arc::clone(&sc_ref);
            Box::pin(async move {
                let found = ctx.messages.iter().any(|m| match m {
                    pi_ai::types::Message::User(u) => match &u.content {
                        UserMessageContent::Text(s) => s == "steer custom",
                        UserMessageContent::Blocks(blocks) => blocks.iter().any(|p| match p {
                            UserContentBlock::Text(t) => t.text == "steer custom",
                            _ => false,
                        }),
                    },
                    _ => false,
                });
                sc.store(found, std::sync::atomic::Ordering::SeqCst);
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("done".to_string()),
                    None,
                ))
            })
        })),
    ]);

    let _ = start_rx.await;
    harness
        .session
        .send_custom_message(
            SendCustomMessageDraft {
                custom_type: "queue-test".to_string(),
                content: CustomMessageContent::Text("steer custom".to_string()),
                display: true,
                details: Some(json!({ "value": 1 })),
            },
            Some(SendCustomMessageOptions {
                deliver_as: Some(CustomMessageDelivery::Steer),
                trigger_turn: None,
            }),
        )
        .await
        .unwrap();
    let _ = release_tx.send(());
    prompt_handle.await.unwrap().unwrap();

    assert!(saw_custom.load(std::sync::atomic::Ordering::SeqCst));
    assert!(harness.session.messages().into_iter().any(|m| match m {
        pi_agent_core::types::AgentMessage::Custom(v) => {
            v.get("role").and_then(Value::as_str) == Some("custom")
                && v.get("customType").and_then(Value::as_str) == Some("queue-test")
        }
        _ => false,
    }));
    harness.cleanup();
}

#[tokio::test]
async fn queues_custom_messages_with_deliver_as_follow_up_while_streaming() {
    let (harness, release_tx, prompt_handle, start_rx) = create_waiting_harness(vec![]).await;
    let saw_custom = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sc_ref = Arc::clone(&saw_custom);

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "wait",
                indexmap::IndexMap::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("original turn complete".to_string()),
            None,
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let sc = Arc::clone(&sc_ref);
            Box::pin(async move {
                let found = ctx.messages.iter().any(|m| match m {
                    pi_ai::types::Message::User(u) => match &u.content {
                        UserMessageContent::Text(s) => s == "follow-up custom",
                        UserMessageContent::Blocks(blocks) => blocks.iter().any(|p| match p {
                            UserContentBlock::Text(t) => t.text == "follow-up custom",
                            _ => false,
                        }),
                    },
                    _ => false,
                });
                sc.store(found, std::sync::atomic::Ordering::SeqCst);
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("done".to_string()),
                    None,
                ))
            })
        })),
    ]);

    let _ = start_rx.await;
    harness
        .session
        .send_custom_message(
            SendCustomMessageDraft {
                custom_type: "queue-test".to_string(),
                content: CustomMessageContent::Text("follow-up custom".to_string()),
                display: true,
                details: Some(json!({ "value": 1 })),
            },
            Some(SendCustomMessageOptions {
                deliver_as: Some(CustomMessageDelivery::FollowUp),
                trigger_turn: None,
            }),
        )
        .await
        .unwrap();
    let _ = release_tx.send(());
    prompt_handle.await.unwrap().unwrap();

    assert!(saw_custom.load(std::sync::atomic::Ordering::SeqCst));
    assert!(harness.session.messages().into_iter().any(|m| match m {
        pi_agent_core::types::AgentMessage::Custom(v) => {
            v.get("role").and_then(Value::as_str) == Some("custom")
                && v.get("customType").and_then(Value::as_str) == Some("queue-test")
        }
        _ => false,
    }));
    harness.cleanup();
}

#[tokio::test]
async fn injects_next_turn_custom_messages_into_the_next_prompt() {
    let harness = create_harness(HarnessOptions::default()).await;
    let saw_custom = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sc_ref = Arc::clone(&saw_custom);

    harness
        .session
        .send_custom_message(
            SendCustomMessageDraft {
                custom_type: "next-turn".to_string(),
                content: CustomMessageContent::Text("carry this".to_string()),
                display: true,
                details: Some(json!({})),
            },
            Some(SendCustomMessageOptions {
                deliver_as: Some(CustomMessageDelivery::NextTurn),
                trigger_turn: None,
            }),
        )
        .await
        .unwrap();

    harness.set_responses(vec![FauxResponseStep::Factory(Arc::new(
        move |ctx, _opts, _st, _mdl| {
            let sc = Arc::clone(&sc_ref);
            Box::pin(async move {
                let found = ctx.messages.iter().any(|m| match m {
                    pi_ai::types::Message::User(u) => match &u.content {
                        UserMessageContent::Text(s) => s == "carry this",
                        UserMessageContent::Blocks(blocks) => blocks.iter().any(|p| match p {
                            UserContentBlock::Text(t) => t.text == "carry this",
                            _ => false,
                        }),
                    },
                    _ => false,
                });
                sc.store(found, std::sync::atomic::Ordering::SeqCst);
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("done".to_string()),
                    None,
                ))
            })
        },
    ))]);

    harness.session.prompt("normal prompt", None).await.unwrap();
    assert!(saw_custom.load(std::sync::atomic::Ordering::SeqCst));
    let msgs = harness.session.messages();
    let roles: Vec<&str> = msgs
        .iter()
        .filter_map(|m| m.role())
        .collect::<Vec<_>>()
        .into_iter()
        .map(|s| match s {
            "system" => "system",
            "user" => "user",
            "custom" => "custom",
            "assistant" => "assistant",
            other => other,
        })
        .collect();
    assert_eq!(roles, vec!["system", "user", "custom", "assistant"]);
    harness.cleanup();
}

#[tokio::test]
async fn updates_pending_message_count_and_removes_queued_text_before_message_start_is_emitted() {
    let (harness, release_tx, prompt_handle, start_rx) = create_waiting_harness(vec![]).await;
    let counts: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let c_ref = Arc::clone(&counts);

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "wait",
                indexmap::IndexMap::new(),
                None,
            ))),
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

    let sess_sub = harness.session.clone();
    let _unsub = harness.session.subscribe(move |ev| {
        if let AgentSessionEvent::MessageStart { message } = ev
            && message.role() == Some("user")
            && get_message_text(&message) == "queued"
        {
            c_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(sess_sub.pending_message_count());
        }
    });

    let _ = start_rx.await;
    harness.session.steer("queued", None, None).await.unwrap();
    assert_eq!(harness.session.pending_message_count(), 1);
    let _ = release_tx.send(());
    prompt_handle.await.unwrap().unwrap();

    assert_eq!(*counts.lock().unwrap_or_else(PoisonError::into_inner), vec![0]);
    assert_eq!(harness.session.pending_message_count(), 0);
    harness.cleanup();
}

#[tokio::test]
async fn throws_when_queueing_an_extension_command_with_steer() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.register_command(
            "testcmd",
            RegisteredCommandOptions {
                description: Some("Test command".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, _ctx| Box::pin(async { Ok(()) })),
            },
        )?;
        Ok(())
    }));
    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    harness
        .session
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();

    let err = harness.session.steer("/testcmd queued", None, None).await.unwrap_err();
    assert!(err.to_string().contains(
        r#"Extension command "/testcmd" cannot be queued. Use prompt() or execute the command when not streaming."#
    ));
    harness.cleanup();
}

#[tokio::test]
async fn throws_when_queueing_an_extension_command_with_follow_up() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.register_command(
            "testcmd",
            RegisteredCommandOptions {
                description: Some("Test command".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, _ctx| Box::pin(async { Ok(()) })),
            },
        )?;
        Ok(())
    }));
    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    harness
        .session
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();

    let err = harness
        .session
        .follow_up("/testcmd queued", None, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains(
        r#"Extension command "/testcmd" cannot be queued. Use prompt() or execute the command when not streaming."#
    ));
    harness.cleanup();
}

#[tokio::test]
async fn delivers_follow_ups_queued_during_agent_end() {
    let sent = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s_ref = Arc::clone(&sent);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let s = Arc::clone(&s_ref);
        async move {
            let pi_ev = pi.clone();
            pi.on::<pi_coding_agent::core::extensions::types::AgentEndEvent>(move |_ev, _ctx| {
                let s_inner = Arc::clone(&s);
                let pi_handle = pi_ev.clone();
                Box::pin(async move {
                    if s_inner.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        return Ok(None);
                    }
                    let _ = pi_handle.send_user_message(
                        CustomMessageContent::Text("conflict report".to_string()),
                        Some(SendUserMessageOptions {
                            deliver_as: Some(InputDeliveryBehavior::FollowUp),
                            expand_prompt_templates: None,
                        }),
                    );
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
    harness
        .session
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("reply".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("follow-up reply".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("hello", None).await.unwrap();
    harness.session.agent().wait_for_idle().await;

    assert_eq!(get_user_texts(&harness), vec!["hello", "conflict report"]);
    harness.cleanup();
}
