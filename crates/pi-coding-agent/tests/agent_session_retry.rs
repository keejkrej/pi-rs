//! Port of packages/coding-agent/test/agent-session-retry.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_agent_core::agent::{Agent, AgentInitialState, AgentOptions};
use pi_agent_core::types::{AgentTool, AgentToolResult, StreamFn};
use pi_ai::compat::get_model;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, StopReason, TextContent, ToolCall, Usage,
    UserContentBlock,
};
use pi_ai::utils::event_stream::create_assistant_message_event_stream;
use pi_ai::{ApiKeyCredential, Credential, CredentialStore};
use pi_coding_agent::core::agent_session::{AgentSession, AgentSessionConfig, AgentSessionEvent, ExtensionBindings};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::extensions::types::{ExtensionApi, InlineExtension, MessageEndEvent};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::{RetrySettings, Settings, SettingsManager};
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::Value;
use support::model_runtime_test_utils::{create_model_registry, get_model_runtime};
use support::utilities::{TestExtensionInput, create_test_extensions_result, create_test_resource_loader};

fn make_assistant_message(text: &str, stop_reason: StopReason, error_message: Option<&str>) -> AssistantMessage {
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
        stop_reason,
        deferred: None,
        error_message: error_message.map(str::to_string),
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
    }
}

async fn create_retry_session(
    fail_count: usize,
    max_retries: i64,
    max_agent_delay_ms: i64,
    delay_message_end_ms: u64,
) -> (AgentSession, Arc<AtomicUsize>, String) {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-retry-test-"])).unwrap();
    let call_count = Arc::new(AtomicUsize::new(0));
    let count_ref = Arc::clone(&call_count);

    let model = get_model("anthropic", "claude-sonnet-4-5").unwrap();
    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let count = Arc::clone(&count_ref);
        Box::pin(async move {
            let n = count.fetch_add(1, Ordering::SeqCst) + 1;
            let stream = create_assistant_message_event_stream();
            let stream_clone = stream.clone();
            tokio::spawn(async move {
                if n <= fail_count {
                    let msg = make_assistant_message("", StopReason::Error, Some("overloaded_error"));
                    stream_clone.push(AssistantMessageEvent::Start { partial: msg.clone() });
                    stream_clone.push(AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        error: msg,
                    });
                } else {
                    let msg = make_assistant_message("Success", StopReason::Stop, None);
                    stream_clone.push(AssistantMessageEvent::Start { partial: msg.clone() });
                    stream_clone.push(AssistantMessageEvent::Done {
                        reason: StopReason::Stop,
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

    settings_manager.apply_overrides(&Settings {
        retry: Some(RetrySettings {
            enabled: Some(true),
            max_retries: Some(max_retries),
            base_delay_ms: Some(1),
            max_agent_delay_ms: Some(max_agent_delay_ms),
            provider: None,
        }),
        ..Default::default()
    });

    let ext_res = if delay_message_end_ms > 0 {
        let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| async move {
            pi.on::<MessageEndEvent>(move |ev, _ctx| {
                Box::pin(async move {
                    if ev.message.role() == Some("assistant") {
                        pi_js::time::sleep(delay_message_end_ms).await;
                    }
                    Ok(None)
                })
            });
            Ok(())
        }));
        Some(create_test_extensions_result(&[TestExtensionInput::Inline(ext)], &temp_dir).await)
    } else {
        None
    };

    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager,
        settings_manager,
        cwd: temp_dir.clone(),
        model_runtime: get_model_runtime(&model_registry),
        resource_loader: create_test_resource_loader(ext_res),
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
    if delay_message_end_ms > 0 {
        session.bind_extensions(ExtensionBindings::default()).await.unwrap();
    }

    (session, call_count, temp_dir)
}

#[tokio::test]
async fn retries_after_a_transient_error_and_succeeds() {
    let (session, call_count, temp_dir) = create_retry_session(1, 3, 60_000, 0).await;
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let ev_ref = Arc::clone(&events);
    let _unsub = session.subscribe(move |ev| match ev {
        AgentSessionEvent::AutoRetryStart { attempt, .. } => {
            ev_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("start:{attempt}"));
        }
        AgentSessionEvent::AutoRetryEnd { success, .. } => {
            ev_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("end:success={success}"));
        }
        _ => {}
    });

    session.prompt("Test", None).await.unwrap();

    assert_eq!(call_count.load(Ordering::SeqCst), 2);
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["start:1".to_string(), "end:success=true".to_string()]
    );
    assert!(!session.is_retrying());

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn exhausts_max_retries_and_emits_failure() {
    let (session, call_count, temp_dir) = create_retry_session(99, 2, 60_000, 0).await;
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let ev_ref = Arc::clone(&events);
    let _unsub = session.subscribe(move |ev| match ev {
        AgentSessionEvent::AutoRetryStart { attempt, .. } => {
            ev_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("start:{attempt}"));
        }
        AgentSessionEvent::AutoRetryEnd { success, .. } => {
            ev_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("end:success={success}"));
        }
        _ => {}
    });

    session.prompt("Test", None).await.unwrap();

    assert_eq!(call_count.load(Ordering::SeqCst), 3);
    let recorded = events.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(recorded.contains(&"start:1".to_string()));
    assert!(recorded.contains(&"start:2".to_string()));
    assert!(recorded.contains(&"end:success=false".to_string()));
    assert!(!session.is_retrying());

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn caps_agent_retry_delay() {
    let (session, _call_count, temp_dir) = create_retry_session(4, 5, 5, 0).await;
    let delays: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let d_ref = Arc::clone(&delays);
    let _unsub = session.subscribe(move |ev| {
        if let AgentSessionEvent::AutoRetryStart { delay_ms, .. } = ev {
            d_ref.lock().unwrap_or_else(PoisonError::into_inner).push(delay_ms);
        }
    });

    session.prompt("Test", None).await.unwrap();
    assert_eq!(*delays.lock().unwrap_or_else(PoisonError::into_inner), vec![1, 2, 4, 5]);

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn prompt_waits_for_retry_completion_even_when_assistant_message_end_handling_is_delayed() {
    let (session, call_count, temp_dir) = create_retry_session(1, 3, 60_000, 40).await;

    session.prompt("Test", None).await.unwrap();

    assert_eq!(call_count.load(Ordering::SeqCst), 2);
    assert!(!session.is_retrying());

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn retries_provider_network_error_failures() {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-retry-test-"])).unwrap();
    let call_count = Arc::new(AtomicUsize::new(0));
    let count_ref = Arc::clone(&call_count);

    let model = get_model("anthropic", "claude-sonnet-4-5").unwrap();
    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let c = Arc::clone(&count_ref);
        Box::pin(async move {
            let n = c.fetch_add(1, Ordering::SeqCst) + 1;
            let stream = create_assistant_message_event_stream();
            let stream_clone = stream.clone();
            tokio::spawn(async move {
                if n == 1 {
                    let msg =
                        make_assistant_message("", StopReason::Error, Some("Provider finish_reason: network_error"));
                    stream_clone.push(AssistantMessageEvent::Start { partial: msg.clone() });
                    stream_clone.push(AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        error: msg,
                    });
                } else {
                    let msg = make_assistant_message("Recovered after retry", StopReason::Stop, None);
                    stream_clone.push(AssistantMessageEvent::Start { partial: msg.clone() });
                    stream_clone.push(AssistantMessageEvent::Done {
                        reason: StopReason::Stop,
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
    settings_manager.apply_overrides(&Settings {
        retry: Some(RetrySettings {
            enabled: Some(true),
            max_retries: Some(3),
            base_delay_ms: Some(1),
            ..Default::default()
        }),
        ..Default::default()
    });

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

    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let ev_ref = Arc::clone(&events);
    let _unsub = session.subscribe(move |ev| match ev {
        AgentSessionEvent::AutoRetryStart { attempt, .. } => {
            ev_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("start:{attempt}"));
        }
        AgentSessionEvent::AutoRetryEnd { success, .. } => {
            ev_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("end:success={success}"));
        }
        _ => {}
    });

    session.prompt("Test", None).await.unwrap();

    assert_eq!(call_count.load(Ordering::SeqCst), 2);
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["start:1".to_string(), "end:success=true".to_string()]
    );

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn prompt_waits_for_full_agent_loop_when_retry_produces_tool_calls() {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-retry-test-"])).unwrap();
    let call_count = Arc::new(AtomicUsize::new(0));
    let tool_executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let te_ref = Arc::clone(&tool_executed);

    let echo_tool = AgentTool {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "Echo text back".to_string(),
        parameters: t::object(&[t::prop("text", t::string())]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, _sig, _upd| {
            let te = Arc::clone(&te_ref);
            Box::pin(async move {
                te.store(true, Ordering::SeqCst);
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("echoed"))],
                    details: None,
                    structured_content: None,
                    usage: None,
                })
            })
        }),
        replay: None,
    };

    let count_ref = Arc::clone(&call_count);
    let model = get_model("anthropic", "claude-sonnet-4-5").unwrap();
    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let c = Arc::clone(&count_ref);
        Box::pin(async move {
            let n = c.fetch_add(1, Ordering::SeqCst) + 1;
            let stream = create_assistant_message_event_stream();
            let stream_clone = stream.clone();
            tokio::spawn(async move {
                if n == 1 {
                    let msg = make_assistant_message("", StopReason::Error, Some("overloaded_error"));
                    stream_clone.push(AssistantMessageEvent::Start { partial: msg.clone() });
                    stream_clone.push(AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        error: msg,
                    });
                } else if n == 2 {
                    let msg = AssistantMessage {
                        content: vec![
                            AssistantContentBlock::Text(TextContent::new("Looking that up now.")),
                            AssistantContentBlock::ToolCall(ToolCall::new(
                                "call_1",
                                "echo",
                                serde_json::from_value(Value::Object({
                                    let mut map = serde_json::Map::new();
                                    map.insert("text".to_string(), Value::String("hello".to_string()));
                                    map
                                }))
                                .unwrap(),
                            )),
                        ],
                        stop_reason: StopReason::ToolUse,
                        ..make_assistant_message("", StopReason::ToolUse, None)
                    };
                    stream_clone.push(AssistantMessageEvent::Start { partial: msg.clone() });
                    stream_clone.push(AssistantMessageEvent::Done {
                        reason: StopReason::ToolUse,
                        message: msg,
                    });
                } else {
                    let msg = make_assistant_message("Final answer.", StopReason::Stop, None);
                    stream_clone.push(AssistantMessageEvent::Start { partial: msg.clone() });
                    stream_clone.push(AssistantMessageEvent::Done {
                        reason: StopReason::Stop,
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
    settings_manager.apply_overrides(&Settings {
        retry: Some(RetrySettings {
            enabled: Some(true),
            max_retries: Some(3),
            base_delay_ms: Some(1),
            ..Default::default()
        }),
        ..Default::default()
    });

    let mut base_tools = IndexMap::new();
    base_tools.insert("echo".to_string(), echo_tool);

    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager,
        settings_manager,
        cwd: temp_dir.clone(),
        model_runtime: get_model_runtime(&model_registry),
        resource_loader: create_test_resource_loader(None),
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

    session.prompt("Test", None).await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), 3);
    assert!(tool_executed.load(Ordering::SeqCst));
    assert!(!session.is_streaming());

    session.prompt("Follow-up", None).await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), 4);

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}
