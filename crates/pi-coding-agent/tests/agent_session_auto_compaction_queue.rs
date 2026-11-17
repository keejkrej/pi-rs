//! Port of packages/coding-agent/test/agent-session-auto-compaction-queue.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pi_agent_core::agent::{Agent, AgentInitialState, AgentOptions};
use pi_agent_core::types::{AgentMessage, StreamFn};
use pi_ai::compat::get_model;
use pi_ai::providers::faux::{FauxAssistantMessageContent, faux_assistant_message};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Model, StopReason, TextContent, Usage,
    UserMessage,
};
use pi_ai::utils::event_stream::create_assistant_message_event_stream;
use pi_ai::{ApiKeyCredential, Credential, CredentialStore};
use pi_coding_agent::core::agent_session::{
    AgentSession, AgentSessionConfig, AgentSessionEvent, CompactionReason, ExtensionBindings,
};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, SessionBeforeCompactEvent, SessionBeforeCompactResult,
};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::{CompactionSettings, Settings, SettingsManager};
use serde_json::json;
use support::model_runtime_test_utils::{create_model_registry, get_model_runtime};
use support::utilities::{TestExtensionInput, create_test_extensions_result, create_test_resource_loader};

async fn create_session(
    extensions: Option<Vec<InlineExtension>>,
) -> (AgentSession, SessionManager, SettingsManager, String) {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[
        &pi_js::env::tmp_dir(),
        "pi-auto-compaction-queue-",
    ]))
    .unwrap();

    let model = get_model("anthropic", "claude-sonnet-4-5").unwrap();
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(|m, ctx, opts| {
            Box::pin(async move { Ok(pi_ai::compat::stream_simple(&m, &ctx, opts)) })
        })),
        initial_state: Some(AgentInitialState {
            model: Some(model),
            system_prompt: Some("Test".to_string()),
            tools: Some(vec![]),
            ..Default::default()
        }),
        ..Default::default()
    });

    let session_manager = SessionManager::in_memory(None, None, None).unwrap();
    let settings_manager = SettingsManager::create(&temp_dir, Some(&temp_dir), None);
    let auth_storage = Arc::new(AuthStorage::create(Some(&pi_js::path::join(&[&temp_dir, "auth.json"]))));
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
    let model_registry = create_model_registry(auth_storage, Some(&temp_dir)).await;

    let ext_res = if let Some(list) = extensions {
        let inputs: Vec<TestExtensionInput> = list.into_iter().map(TestExtensionInput::Inline).collect();
        Some(create_test_extensions_result(&inputs, &temp_dir).await)
    } else {
        None
    };

    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager: session_manager.clone(),
        settings_manager: settings_manager.clone(),
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

    (session, session_manager, settings_manager, temp_dir)
}

#[tokio::test]
async fn should_resume_after_threshold_compaction_when_only_agent_level_queued_messages_exist() {
    let (session, session_manager, settings_manager, temp_dir) = create_session(None).await;
    settings_manager.apply_overrides(&Settings {
        compaction: Some(CompactionSettings {
            keep_recent_tokens: Some(1),
            ..Default::default()
        }),
        ..Default::default()
    });

    let model = session.model().unwrap();
    let now = pi_js::time::now_ms();
    let mut u = UserMessage::text("message to compact");
    u.timestamp = now - 1000;
    session_manager
        .append_message(pi_ai::types::Message::User(u).into())
        .unwrap();
    let assistant = AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new(
            "assistant response to compact",
        ))],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: 100,
            total_tokens: 100,
            ..Usage::default()
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: now - 500,
    };
    session_manager
        .append_message(pi_ai::types::Message::Assistant(assistant).into())
        .unwrap();
    session
        .agent()
        .replace_messages(session_manager.build_session_context(None, None).messages);

    let summary_stream: StreamFn = Arc::new(|summary_model: Model, _ctx: Context, _opts| {
        Box::pin(async move {
            let stream = create_assistant_message_event_stream();
            let mut msg = faux_assistant_message(FauxAssistantMessageContent::Text("compacted".to_string()), None);
            msg.api = summary_model.api;
            msg.provider = summary_model.provider;
            msg.model = summary_model.id;
            msg.usage = Usage {
                input: 10,
                total_tokens: 10,
                ..Usage::default()
            };
            stream.push(AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: msg,
            });
            Ok(stream)
        })
    });
    session.agent().set_stream_fn(Some(summary_stream));

    session.agent().follow_up(AgentMessage::custom(json!({
        "role": "custom",
        "customType": "test",
        "content": [{ "type": "text", "text": "Queued custom" }],
        "display": false,
        "timestamp": pi_js::time::now_ms(),
    })));

    assert_eq!(session.pending_message_count(), 0);
    assert!(session.agent().has_queued_messages());

    let compacted = session
        .run_auto_compaction_for_test(CompactionReason::Threshold, false)
        .await;
    assert!(compacted);

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn should_not_compact_repeatedly_after_overflow_recovery_already_attempted() {
    let cancel_ext_count = Arc::new(AtomicUsize::new(0));
    let cnt = Arc::clone(&cancel_ext_count);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let count_ref = Arc::clone(&cnt);
        async move {
            pi.on::<SessionBeforeCompactEvent>(move |_ev, _ctx| {
                let c = Arc::clone(&count_ref);
                Box::pin(async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(Some(SessionBeforeCompactResult {
                        cancel: Some(true),
                        compaction: None,
                    }))
                })
            });
            Ok(())
        }
    }));

    let (session, session_manager, _sm, temp_dir) = create_session(Some(vec![ext])).await;
    session.bind_extensions(ExtensionBindings::default()).await.unwrap();

    let model = session.model().unwrap();
    let mut u = UserMessage::text("before overflow");
    u.timestamp = pi_js::time::now_ms() - 1000;
    session_manager
        .append_message(pi_ai::types::Message::User(u).into())
        .unwrap();
    session
        .agent()
        .replace_messages(session_manager.build_session_context(None, None).messages);

    let overflow_msg = AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new(""))],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Error,
        deferred: None,
        error_message: Some("prompt is too long".to_string()),
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
    };

    let compaction_ends: Arc<Mutex<Vec<(CompactionReason, Option<String>)>>> = Arc::new(Mutex::new(Vec::new()));
    let ce_ref = Arc::clone(&compaction_ends);
    let _unsub = session.subscribe(move |ev| {
        if let AgentSessionEvent::CompactionEnd {
            reason, error_message, ..
        } = ev
        {
            ce_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((reason, error_message));
        }
    });

    session.check_compaction_for_test(&overflow_msg, true).await;
    let mut overflow_msg2 = overflow_msg;
    overflow_msg2.timestamp += 1;
    session.check_compaction_for_test(&overflow_msg2, true).await;

    assert_eq!(cancel_ext_count.load(Ordering::SeqCst), 1);
    let recorded = compaction_ends.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(recorded.contains(&(
        CompactionReason::Overflow,
        Some(
            "Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model.".to_string()
        )
    )));

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn should_ignore_stale_pre_compaction_assistant_usage_on_pre_prompt_compaction_checks() {
    let compactions = Arc::new(AtomicUsize::new(0));
    let (session, session_manager, _sm, temp_dir) = create_session(None).await;
    let comp_ref = Arc::clone(&compactions);
    let _unsub = session.subscribe(move |ev| {
        if matches!(ev, AgentSessionEvent::CompactionStart { .. }) {
            comp_ref.fetch_add(1, Ordering::SeqCst);
        }
    });

    let model = session.model().unwrap();
    let stale_ts = pi_js::time::now_ms() - 10_000;
    let stale_assistant = AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new(
            "large response before compaction",
        ))],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: 600_000,
            output: 10_000,
            total_tokens: 610_000,
            ..Usage::default()
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: stale_ts,
    };

    let mut u1 = UserMessage::text("before compaction");
    u1.timestamp = stale_ts - 1000;
    let first_kept = session_manager
        .append_message(pi_ai::types::Message::User(u1).into())
        .unwrap();
    session_manager
        .append_message(pi_ai::types::Message::Assistant(stale_assistant.clone()).into())
        .unwrap();
    session_manager
        .append_compaction(
            "summary",
            Some(&first_kept),
            stale_assistant.usage.total_tokens,
            None,
            Some(false),
            None,
        )
        .unwrap();
    let mut u2 = UserMessage::text("session recovery payload");
    u2.timestamp = pi_js::time::now_ms();
    session_manager
        .append_message(pi_ai::types::Message::User(u2).into())
        .unwrap();

    session.check_compaction_for_test(&stale_assistant, false).await;
    assert_eq!(compactions.load(Ordering::SeqCst), 0);

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn should_trigger_threshold_compaction_for_error_messages_using_last_successful_usage() {
    let threshold_starts = Arc::new(AtomicUsize::new(0));
    let cnt = Arc::clone(&threshold_starts);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let c = Arc::clone(&cnt);
        async move {
            pi.on::<SessionBeforeCompactEvent>(move |ev, _ctx| {
                let c_inner = Arc::clone(&c);
                Box::pin(async move {
                    if ev.reason == pi_coding_agent::core::extensions::types::CompactionReason::Threshold
                        && !ev.will_retry
                    {
                        c_inner.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(Some(SessionBeforeCompactResult {
                        cancel: Some(true),
                        compaction: None,
                    }))
                })
            });
            Ok(())
        }
    }));

    let (session, _sm, settings_manager, temp_dir) = create_session(Some(vec![ext])).await;
    session.bind_extensions(ExtensionBindings::default()).await.unwrap();
    let model = session.model().unwrap();

    let compaction_settings = settings_manager.get_compaction_settings(Some(&model));
    let threshold_tokens = model.context_window - compaction_settings.reserve_tokens + 1;
    let successful_assistant = AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new(
            "large successful response",
        ))],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: threshold_tokens - 10_000,
            output: 10_000,
            total_tokens: threshold_tokens,
            ..Usage::default()
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
    };
    let error_assistant = AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new(""))],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Error,
        deferred: None,
        error_message: Some("529 overloaded".to_string()),
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms() + 1000,
    };

    let mut u1 = UserMessage::text("hello");
    u1.timestamp = pi_js::time::now_ms() - 1000;
    let mut u2 = UserMessage::text("another prompt");
    u2.timestamp = pi_js::time::now_ms() + 500;
    session.agent().replace_messages(vec![
        pi_ai::types::Message::User(u1).into(),
        pi_ai::types::Message::Assistant(successful_assistant).into(),
        pi_ai::types::Message::User(u2).into(),
        pi_ai::types::Message::Assistant(error_assistant.clone()).into(),
    ]);

    session.check_compaction_for_test(&error_assistant, true).await;
    assert_eq!(threshold_starts.load(Ordering::SeqCst), 1);

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn should_not_trigger_threshold_compaction_for_error_messages_when_no_prior_usage_exists() {
    let compactions = Arc::new(AtomicUsize::new(0));
    let (session, _sm, _settings, temp_dir) = create_session(None).await;
    let c = Arc::clone(&compactions);
    let _unsub = session.subscribe(move |ev| {
        if matches!(ev, AgentSessionEvent::CompactionStart { .. }) {
            c.fetch_add(1, Ordering::SeqCst);
        }
    });

    let model = session.model().unwrap();
    let error_assistant = AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new(""))],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Error,
        deferred: None,
        error_message: Some("529 overloaded".to_string()),
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
    };

    let mut u = UserMessage::text("hello");
    u.timestamp = pi_js::time::now_ms() - 1000;
    session.agent().replace_messages(vec![
        pi_ai::types::Message::User(u).into(),
        pi_ai::types::Message::Assistant(error_assistant.clone()).into(),
    ]);

    session.check_compaction_for_test(&error_assistant, true).await;
    assert_eq!(compactions.load(Ordering::SeqCst), 0);

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn should_not_trigger_threshold_compaction_for_error_messages_when_only_kept_pre_compaction_usage_exists() {
    let compactions = Arc::new(AtomicUsize::new(0));
    let (session, session_manager, _settings, temp_dir) = create_session(None).await;
    let c = Arc::clone(&compactions);
    let _unsub = session.subscribe(move |ev| {
        if matches!(ev, AgentSessionEvent::CompactionStart { .. }) {
            c.fetch_add(1, Ordering::SeqCst);
        }
    });

    let model = session.model().unwrap();
    let pre_compaction_ts = pi_js::time::now_ms() - 10_000;
    let kept_assistant = AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new(
            "kept response from before compaction",
        ))],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: 180_000,
            output: 10_000,
            total_tokens: 190_000,
            ..Usage::default()
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pre_compaction_ts,
    };

    let mut u_before = UserMessage::text("before compaction");
    u_before.timestamp = pre_compaction_ts - 1000;
    let first_kept = session_manager
        .append_message(pi_ai::types::Message::User(u_before).into())
        .unwrap();
    session_manager
        .append_message(pi_ai::types::Message::Assistant(kept_assistant.clone()).into())
        .unwrap();
    session_manager
        .append_compaction(
            "summary",
            Some(&first_kept),
            kept_assistant.usage.total_tokens,
            None,
            Some(false),
            None,
        )
        .unwrap();

    let error_assistant = AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new(""))],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Error,
        deferred: None,
        error_message: Some("529 overloaded".to_string()),
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
    };

    let mut u_kept = UserMessage::text("kept user msg");
    u_kept.timestamp = pre_compaction_ts - 1000;
    let mut u_new = UserMessage::text("new prompt");
    u_new.timestamp = pi_js::time::now_ms() - 500;
    session.agent().replace_messages(vec![
        pi_ai::types::Message::User(u_kept).into(),
        pi_ai::types::Message::Assistant(kept_assistant).into(),
        pi_ai::types::Message::User(u_new).into(),
        pi_ai::types::Message::Assistant(error_assistant.clone()).into(),
    ]);

    session.check_compaction_for_test(&error_assistant, true).await;
    assert_eq!(compactions.load(Ordering::SeqCst), 0);

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}
