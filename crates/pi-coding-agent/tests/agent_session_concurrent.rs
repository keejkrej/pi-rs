//! Port of packages/coding-agent/test/agent-session-concurrent.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_agent_core::agent::{Agent, AgentInitialState, AgentOptions};
use pi_agent_core::types::{AgentTool, AgentToolResult, StreamFn};
use pi_ai::compat::get_model;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Message, StopReason, TextContent, ToolCall, Usage,
    UserContentBlock, UserMessageContent,
};
use pi_ai::utils::event_stream::create_assistant_message_event_stream;
use pi_ai::{ApiKeyCredential, Credential, CredentialStore};
use pi_coding_agent::core::agent_session::{
    AgentSession, AgentSessionConfig, AgentSessionEvent, ExtensionBindings, QueuedInputDisposition,
};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, InputDeliveryBehavior, InputEvent, InputSource, MessageEndEvent,
    SendUserMessageOptions, ToolCallEvent,
};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::SettingsManager;
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::model_runtime_test_utils::{create_model_registry, get_model_runtime};
use support::utilities::{TestExtensionInput, create_test_extensions_result, create_test_resource_loader};

fn create_assistant_message(text: &str) -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new(text))],
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        model: "mock".to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
    }
}

async fn create_abortable_session() -> (AgentSession, String) {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-concurrent-test-"])).unwrap();
    let model = get_model("anthropic", "claude-sonnet-4-5").unwrap();

    let stream_fn: StreamFn = Arc::new(|_model, _context, options| {
        Box::pin(async move {
            let signal = options.and_then(|o| o.base.signal);
            let stream = create_assistant_message_event_stream();
            let stream_clone = stream.clone();
            tokio::spawn(async move {
                stream_clone.push(AssistantMessageEvent::Start {
                    partial: create_assistant_message(""),
                });
                loop {
                    if signal.as_ref().is_some_and(|s| s.aborted()) {
                        stream_clone.push(AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: create_assistant_message("Aborted"),
                        });
                        break;
                    }
                    pi_js::time::sleep(5).await;
                }
            });
            Ok(stream)
        })
    });

    let agent = Agent::new(AgentOptions {
        get_api_key: Some(Arc::new(|_| Box::pin(async { Ok(Some("test-key".to_string())) }))),
        initial_state: Some(AgentInitialState {
            model: Some(model),
            system_prompt: Some("Test".to_string()),
            tools: Some(vec![]),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    let session_manager = SessionManager::in_memory(None, None, None).unwrap();
    let settings_manager = SettingsManager::create(&temp_dir, Some(&temp_dir), None);
    let auth_storage = Arc::new(AuthStorage::create(Some(&pi_js::path::join(&[&temp_dir, "auth.json"]))));
    let model_registry = create_model_registry(auth_storage.clone(), Some(&temp_dir)).await;
    auth_storage
        .modify(
            "anthropic",
            Box::new(|_prev| {
                Box::pin(async move {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("test-key".to_string()),
                        env: None,
                    })))
                })
            }),
            None,
        )
        .await
        .unwrap();

    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager,
        settings_manager,
        cwd: temp_dir.clone(),
        model_runtime: get_model_runtime(&model_registry),
        resource_loader: create_test_resource_loader(None),
        scoped_models: None,
        custom_tools: None,
        cache_warmer: None,
        initial_active_tool_names: None,
        uses_default_tools: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        extension_runner_ref: None,
        session_start_event: None,
        initial_model: None,
        initial_thinking_level: None,
    });
    (session, temp_dir)
}

#[tokio::test]
async fn should_throw_when_prompt_called_while_streaming() {
    let (session, temp_dir) = create_abortable_session().await;

    let session_bg = session.clone();
    let first_task = tokio::spawn(async move { session_bg.prompt("First message", None).await });
    pi_js::time::sleep(10).await;

    assert!(session.is_streaming());
    let err = session.prompt("Second message", None).await.unwrap_err();
    assert!(err.to_string().contains(
        "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message."
    ));

    session.abort().await;
    let _ = first_task.await;
    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn should_allow_steer_while_streaming() {
    let (session, temp_dir) = create_abortable_session().await;

    let session_bg = session.clone();
    let first_task = tokio::spawn(async move { session_bg.prompt("First message", None).await });
    pi_js::time::sleep(10).await;

    let res = session.steer("Steering message", None, None).await.unwrap();
    assert_eq!(res, QueuedInputDisposition::Queued);
    assert_eq!(session.pending_message_count(), 1);

    session.abort().await;
    let _ = first_task.await;
    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn should_allow_follow_up_while_streaming() {
    let (session, temp_dir) = create_abortable_session().await;

    let session_bg = session.clone();
    let first_task = tokio::spawn(async move { session_bg.prompt("First message", None).await });
    pi_js::time::sleep(10).await;

    let res = session.follow_up("Follow-up message", None, None).await.unwrap();
    assert_eq!(res, QueuedInputDisposition::Queued);
    assert_eq!(session.pending_message_count(), 1);

    session.abort().await;
    let _ = first_task.await;
    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn should_queue_extension_origin_steering_messages_while_streaming() {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-concurrent-test-"])).unwrap();
    let model = get_model("anthropic", "claude-sonnet-4-5").unwrap();
    let saw_steering_message = Arc::new(AtomicBool::new(false));
    let last_input_source: Arc<Mutex<Option<InputSource>>> = Arc::new(Mutex::new(None));
    let queue_events: Arc<Mutex<Vec<(Vec<String>, Vec<String>)>>> = Arc::new(Mutex::new(Vec::new()));

    let saw_ref = Arc::clone(&saw_steering_message);
    let stream_fn: StreamFn = Arc::new(move |_model, context, options| {
        let saw = Arc::clone(&saw_ref);
        Box::pin(async move {
            let signal = options.and_then(|o| o.base.signal);
            let user_texts: Vec<String> = context
                .messages
                .iter()
                .filter_map(|m| match m {
                    Message::User(u) => Some(match &u.content {
                        UserMessageContent::Text(s) => s.clone(),
                        UserMessageContent::Blocks(blocks) => blocks
                            .iter()
                            .filter_map(|b| match b {
                                UserContentBlock::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    }),
                    _ => None,
                })
                .collect();

            let stream = create_assistant_message_event_stream();
            let stream_clone = stream.clone();
            tokio::spawn(async move {
                if user_texts.iter().any(|t| t == "Steer from extension") {
                    saw.store(true, Ordering::SeqCst);
                    stream_clone.push(AssistantMessageEvent::Start {
                        partial: create_assistant_message(""),
                    });
                    stream_clone.push(AssistantMessageEvent::Done {
                        reason: StopReason::Stop,
                        message: create_assistant_message("Steered"),
                    });
                    return;
                }
                stream_clone.push(AssistantMessageEvent::Start {
                    partial: create_assistant_message(""),
                });
                loop {
                    if signal.as_ref().is_some_and(|s| s.aborted()) {
                        stream_clone.push(AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: create_assistant_message("Aborted"),
                        });
                        break;
                    }
                    pi_js::time::sleep(5).await;
                }
            });
            Ok(stream)
        })
    });

    let agent = Agent::new(AgentOptions {
        get_api_key: Some(Arc::new(|_| Box::pin(async { Ok(Some("test-key".to_string())) }))),
        initial_state: Some(AgentInitialState {
            model: Some(model),
            system_prompt: Some("Test".to_string()),
            tools: Some(vec![]),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    let session_manager = SessionManager::in_memory(None, None, None).unwrap();
    let settings_manager = SettingsManager::create(&temp_dir, Some(&temp_dir), None);
    let auth_storage = Arc::new(AuthStorage::create(Some(&pi_js::path::join(&[&temp_dir, "auth.json"]))));
    let model_registry = create_model_registry(auth_storage.clone(), Some(&temp_dir)).await;
    auth_storage
        .modify(
            "anthropic",
            Box::new(|_prev| {
                Box::pin(async move {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("test-key".to_string()),
                        env: None,
                    })))
                })
            }),
            None,
        )
        .await
        .unwrap();

    let ext_api_slot: Arc<Mutex<Option<ExtensionApi>>> = Arc::new(Mutex::new(None));
    let slot_ref = Arc::clone(&ext_api_slot);
    let src_ref = Arc::clone(&last_input_source);

    let ext1 = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let s = Arc::clone(&slot_ref);
        async move {
            *s.lock().unwrap_or_else(PoisonError::into_inner) = Some(pi);
            Ok(())
        }
    }));
    let ext2 = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let src = Arc::clone(&src_ref);
        async move {
            pi.on::<InputEvent>(move |ev, _ctx| {
                let s = Arc::clone(&src);
                Box::pin(async move {
                    *s.lock().unwrap_or_else(PoisonError::into_inner) = Some(ev.source);
                    Ok(None)
                })
            });
            Ok(())
        }
    }));
    let extensions_result = create_test_extensions_result(
        &[TestExtensionInput::Inline(ext1), TestExtensionInput::Inline(ext2)],
        &temp_dir,
    )
    .await;

    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager,
        settings_manager,
        cwd: temp_dir.clone(),
        model_runtime: get_model_runtime(&model_registry),
        resource_loader: create_test_resource_loader(Some(extensions_result)),
        scoped_models: None,
        custom_tools: None,
        cache_warmer: None,
        initial_active_tool_names: None,
        uses_default_tools: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        extension_runner_ref: None,
        session_start_event: None,
        initial_model: None,
        initial_thinking_level: None,
    });
    session.bind_extensions(ExtensionBindings::default()).await.unwrap();

    let qe_ref = Arc::clone(&queue_events);
    let _unsub = session.subscribe(move |ev| {
        if let AgentSessionEvent::QueueUpdate { steering, follow_up } = ev {
            qe_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((steering, follow_up));
        }
    });

    let session_bg = session.clone();
    let first_task = tokio::spawn(async move { session_bg.prompt("First message", None).await });
    pi_js::time::sleep(10).await;
    assert!(session.is_streaming());

    let api = ext_api_slot
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .unwrap();
    api.send_user_message(
        pi_coding_agent::core::tools::mcp_wire::CustomMessageContent::Text("Steer from extension".to_string()),
        Some(SendUserMessageOptions {
            deliver_as: Some(InputDeliveryBehavior::Steer),
            expand_prompt_templates: None,
        }),
    )
    .unwrap();
    pi_js::time::sleep(25).await;

    assert_eq!(session.pending_message_count(), 1);
    assert!(
        session
            .get_steering_messages()
            .iter()
            .any(|m| m == "Steer from extension")
    );
    assert_eq!(
        *last_input_source.lock().unwrap_or_else(PoisonError::into_inner),
        Some(InputSource::Extension)
    );
    assert!(
        queue_events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|(st, _)| st.iter().any(|m| m == "Steer from extension"))
    );

    session.abort().await;
    let _ = first_task.await;
    assert!(saw_steering_message.load(Ordering::SeqCst));

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn should_allow_prompt_after_previous_completes() {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-concurrent-test-"])).unwrap();
    let model = get_model("anthropic", "claude-sonnet-4-5").unwrap();

    let stream_fn: StreamFn = Arc::new(|_model, _ctx, _opts| {
        Box::pin(async move {
            let stream = create_assistant_message_event_stream();
            let stream_clone = stream.clone();
            tokio::spawn(async move {
                stream_clone.push(AssistantMessageEvent::Start {
                    partial: create_assistant_message(""),
                });
                stream_clone.push(AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: create_assistant_message("Done"),
                });
            });
            Ok(stream)
        })
    });

    let agent = Agent::new(AgentOptions {
        get_api_key: Some(Arc::new(|_| Box::pin(async { Ok(Some("test-key".to_string())) }))),
        initial_state: Some(AgentInitialState {
            model: Some(model),
            system_prompt: Some("Test".to_string()),
            tools: Some(vec![]),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    let session_manager = SessionManager::in_memory(None, None, None).unwrap();
    let settings_manager = SettingsManager::create(&temp_dir, Some(&temp_dir), None);
    let auth_storage = Arc::new(AuthStorage::create(Some(&pi_js::path::join(&[&temp_dir, "auth.json"]))));
    let model_registry = create_model_registry(auth_storage.clone(), Some(&temp_dir)).await;
    auth_storage
        .modify(
            "anthropic",
            Box::new(|_prev| {
                Box::pin(async move {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("test-key".to_string()),
                        env: None,
                    })))
                })
            }),
            None,
        )
        .await
        .unwrap();

    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager,
        settings_manager,
        cwd: temp_dir.clone(),
        model_runtime: get_model_runtime(&model_registry),
        resource_loader: create_test_resource_loader(None),
        scoped_models: None,
        custom_tools: None,
        cache_warmer: None,
        initial_active_tool_names: None,
        uses_default_tools: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        extension_runner_ref: None,
        session_start_event: None,
        initial_model: None,
        initial_thinking_level: None,
    });

    session.prompt("First message", None).await.unwrap();
    assert!(!session.is_streaming());
    session.prompt("Second message", None).await.unwrap();

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn should_wait_for_queued_agent_events_before_emitting_tool_call() {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-concurrent-test-"])).unwrap();
    let model = get_model("anthropic", "claude-sonnet-4-5").unwrap();

    let dummy_tool = AgentTool {
        name: "dummy".to_string(),
        label: "dummy".to_string(),
        description: "Dummy tool".to_string(),
        parameters: t::object(&[t::prop("q", t::string())]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, params, _sig, _upd| {
            Box::pin(async move {
                let q = params.get("q").and_then(Value::as_str).unwrap_or("");
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(format!("result:{q}")))],
                    details: Some(json!({})),
                    structured_content: None,
                    usage: None,
                })
            })
        }),
        replay: None,
    };

    let stream_fn: StreamFn = Arc::new(|_model, context, _opts| {
        Box::pin(async move {
            let tool_result_count = context
                .messages
                .iter()
                .filter(|m| matches!(m, Message::ToolResult(_)))
                .count();
            let stream = create_assistant_message_event_stream();
            let stream_clone = stream.clone();
            tokio::spawn(async move {
                if tool_result_count > 0 {
                    let msg = AssistantMessage {
                        usage: Usage {
                            input: 1,
                            output: 1,
                            total_tokens: 2,
                            ..Usage::default()
                        },
                        ..create_assistant_message("done")
                    };
                    stream_clone.push(AssistantMessageEvent::Start {
                        partial: AssistantMessage {
                            content: vec![],
                            ..msg.clone()
                        },
                    });
                    stream_clone.push(AssistantMessageEvent::Done {
                        reason: StopReason::Stop,
                        message: msg,
                    });
                } else {
                    let msg = AssistantMessage {
                        content: vec![
                            AssistantContentBlock::ToolCall(ToolCall::new(
                                "toolu_1",
                                "dummy",
                                serde_json::from_value(json!({ "q": "x" })).unwrap(),
                            )),
                            AssistantContentBlock::ToolCall(ToolCall::new(
                                "toolu_2",
                                "dummy",
                                serde_json::from_value(json!({ "q": "y" })).unwrap(),
                            )),
                        ],
                        stop_reason: StopReason::ToolUse,
                        usage: Usage {
                            input: 1,
                            output: 1,
                            total_tokens: 2,
                            ..Usage::default()
                        },
                        ..create_assistant_message("")
                    };
                    stream_clone.push(AssistantMessageEvent::Start {
                        partial: AssistantMessage {
                            content: vec![],
                            ..msg.clone()
                        },
                    });
                    stream_clone.push(AssistantMessageEvent::Done {
                        reason: StopReason::ToolUse,
                        message: msg,
                    });
                }
            });
            Ok(stream)
        })
    });

    let agent = Agent::new(AgentOptions {
        get_api_key: Some(Arc::new(|_| Box::pin(async { Ok(Some("test-key".to_string())) }))),
        initial_state: Some(AgentInitialState {
            model: Some(model),
            system_prompt: Some("Test".to_string()),
            tools: Some(vec![dummy_tool.clone()]),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    let session_manager = SessionManager::in_memory(None, None, None).unwrap();
    let settings_manager = SettingsManager::create(&temp_dir, Some(&temp_dir), None);
    let auth_storage = Arc::new(AuthStorage::create(Some(&pi_js::path::join(&[&temp_dir, "auth.json"]))));
    let model_registry = create_model_registry(auth_storage.clone(), Some(&temp_dir)).await;
    auth_storage
        .modify(
            "anthropic",
            Box::new(|_prev| {
                Box::pin(async move {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("test-key".to_string()),
                        env: None,
                    })))
                })
            }),
            None,
        )
        .await
        .unwrap();

    let snapshots: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let snap_ref = Arc::clone(&snapshots);
    let sm_for_ext = session_manager.clone();
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let sn = Arc::clone(&snap_ref);
        let sm = sm_for_ext.clone();
        async move {
            pi.on::<ToolCallEvent>(move |_ev, _ctx| {
                let s = Arc::clone(&sn);
                let sm_inner = sm.clone();
                Box::pin(async move {
                    let roles: Vec<String> = sm_inner
                        .get_entries()
                        .into_iter()
                        .filter_map(|e| match e {
                            pi_coding_agent::core::session_manager::SessionEntry::Message(me) => {
                                me.message.role().map(str::to_string)
                            }
                            _ => None,
                        })
                        .collect();
                    s.lock().unwrap_or_else(PoisonError::into_inner).push(roles);
                    Ok(None)
                })
            });
            Ok(())
        }
    }));
    let extensions_result = create_test_extensions_result(&[TestExtensionInput::Inline(ext)], &temp_dir).await;

    let mut base_tools = IndexMap::new();
    base_tools.insert("dummy".to_string(), dummy_tool);

    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager,
        settings_manager,
        cwd: temp_dir.clone(),
        model_runtime: get_model_runtime(&model_registry),
        resource_loader: create_test_resource_loader(Some(extensions_result)),
        base_tools_override: Some(base_tools),
        scoped_models: None,
        custom_tools: None,
        cache_warmer: None,
        initial_active_tool_names: None,
        uses_default_tools: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        extension_runner_ref: None,
        session_start_event: None,
        initial_model: None,
        initial_thinking_level: None,
    });
    session.bind_extensions(ExtensionBindings::default()).await.unwrap();

    session.prompt("hi", None).await.unwrap();
    session.agent().wait_for_idle().await;

    assert_eq!(
        *snapshots.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            vec!["system".to_string(), "user".to_string(), "assistant".to_string()],
            vec!["system".to_string(), "user".to_string(), "assistant".to_string()],
        ]
    );

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn should_persist_message_end_events_in_order_with_slow_extension_handlers() {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-concurrent-test-"])).unwrap();
    let model = get_model("anthropic", "claude-sonnet-4-5").unwrap();

    let dummy_tool = AgentTool {
        name: "dummy".to_string(),
        label: "dummy".to_string(),
        description: "Dummy tool".to_string(),
        parameters: t::object(&[t::prop("q", t::string())]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, params, _sig, _upd| {
            Box::pin(async move {
                let q = params.get("q").and_then(Value::as_str).unwrap_or("");
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(format!("result:{q}")))],
                    details: Some(json!({})),
                    structured_content: None,
                    usage: None,
                })
            })
        }),
        replay: None,
    };

    let stream_fn: StreamFn = Arc::new(|_model, context, _opts| {
        Box::pin(async move {
            let has_tool_result = context.messages.iter().any(|m| matches!(m, Message::ToolResult(_)));
            let stream = create_assistant_message_event_stream();
            let stream_clone = stream.clone();
            tokio::spawn(async move {
                if has_tool_result {
                    let msg = AssistantMessage {
                        usage: Usage {
                            input: 1,
                            output: 1,
                            total_tokens: 2,
                            ..Usage::default()
                        },
                        ..create_assistant_message("done")
                    };
                    stream_clone.push(AssistantMessageEvent::Start {
                        partial: AssistantMessage {
                            content: vec![],
                            ..msg.clone()
                        },
                    });
                    stream_clone.push(AssistantMessageEvent::Done {
                        reason: StopReason::Stop,
                        message: msg,
                    });
                } else {
                    let msg = AssistantMessage {
                        content: vec![
                            AssistantContentBlock::Text(TextContent::new("calling tool")),
                            AssistantContentBlock::ToolCall(ToolCall::new(
                                "toolu_1",
                                "dummy",
                                serde_json::from_value(json!({ "q": "x" })).unwrap(),
                            )),
                        ],
                        stop_reason: StopReason::ToolUse,
                        usage: Usage {
                            input: 1,
                            output: 1,
                            total_tokens: 2,
                            ..Usage::default()
                        },
                        ..create_assistant_message("")
                    };
                    stream_clone.push(AssistantMessageEvent::Start {
                        partial: AssistantMessage {
                            content: vec![],
                            ..msg.clone()
                        },
                    });
                    stream_clone.push(AssistantMessageEvent::Done {
                        reason: StopReason::ToolUse,
                        message: msg,
                    });
                }
            });
            Ok(stream)
        })
    });

    let agent = Agent::new(AgentOptions {
        get_api_key: Some(Arc::new(|_| Box::pin(async { Ok(Some("test-key".to_string())) }))),
        initial_state: Some(AgentInitialState {
            model: Some(model),
            system_prompt: Some("Test".to_string()),
            tools: Some(vec![dummy_tool.clone()]),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    let session_manager = SessionManager::in_memory(None, None, None).unwrap();
    let settings_manager = SettingsManager::create(&temp_dir, Some(&temp_dir), None);
    let auth_storage = Arc::new(AuthStorage::create(Some(&pi_js::path::join(&[&temp_dir, "auth.json"]))));
    let model_registry = create_model_registry(auth_storage.clone(), Some(&temp_dir)).await;
    auth_storage
        .modify(
            "anthropic",
            Box::new(|_prev| {
                Box::pin(async move {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("test-key".to_string()),
                        env: None,
                    })))
                })
            }),
            None,
        )
        .await
        .unwrap();

    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<MessageEndEvent>(|ev, _ctx| {
            Box::pin(async move {
                if ev.message.role() == Some("assistant") {
                    pi_js::time::sleep(40).await;
                }
                Ok(None)
            })
        });
        Ok(())
    }));
    let extensions_result = create_test_extensions_result(&[TestExtensionInput::Inline(ext)], &temp_dir).await;

    let mut base_tools = IndexMap::new();
    base_tools.insert("dummy".to_string(), dummy_tool);

    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager: session_manager.clone(),
        settings_manager,
        cwd: temp_dir.clone(),
        model_runtime: get_model_runtime(&model_registry),
        resource_loader: create_test_resource_loader(Some(extensions_result)),
        base_tools_override: Some(base_tools),
        scoped_models: None,
        custom_tools: None,
        cache_warmer: None,
        initial_active_tool_names: None,
        uses_default_tools: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        extension_runner_ref: None,
        session_start_event: None,
        initial_model: None,
        initial_thinking_level: None,
    });
    session.bind_extensions(ExtensionBindings::default()).await.unwrap();

    session.prompt("hi", None).await.unwrap();
    session.agent().wait_for_idle().await;
    pi_js::time::sleep(100).await;

    let roles: Vec<String> = session_manager
        .get_entries()
        .into_iter()
        .filter_map(|e| match e {
            pi_coding_agent::core::session_manager::SessionEntry::Message(me) => me.message.role().map(str::to_string),
            _ => None,
        })
        .collect();
    assert_eq!(
        roles,
        vec!["system", "user", "assistant", "toolResult", "system", "assistant"]
    );

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}
