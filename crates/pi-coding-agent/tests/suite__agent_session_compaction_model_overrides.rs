//! Port of packages/coding-agent/test/suite/agent-session-compaction-model-overrides.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxModelDefinition, FauxResponseStep, faux_assistant_message,
};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, TextContent, Usage, UserMessage, UserMessageContent,
};
use pi_coding_agent::core::agent_session::{AgentSession, AgentSessionEvent};
use pi_coding_agent::core::compaction::{CompactionResult, CompactionSettings as CompactionPrepSettings};
use pi_coding_agent::core::extensions::types::{
    CompactionReason as ExtCompactionReason, ExtensionApi, InlineExtension, SessionBeforeCompactEvent,
    SessionBeforeCompactResult,
};
use pi_coding_agent::core::settings_manager::{CompactionSettings, ModelRef, Settings};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::suite::harness::{Harness, HarnessOptions, create_harness};

fn faux_def(id: &str, context_window: Option<i64>, max_tokens: Option<i64>) -> FauxModelDefinition {
    FauxModelDefinition {
        id: id.to_string(),
        name: None,
        reasoning: None,
        input: None,
        input_limits: None,
        cost: None,
        context_window,
        max_tokens,
    }
}

fn seed_history(harness: &Harness, total_tokens: i64) -> String {
    let model = harness.session.model().unwrap();
    let mut recent_user_id = String::new();
    for label in ["old", "recent"] {
        let user = UserMessage {
            content: UserMessageContent::Text(format!("{label}{}", "x".repeat(400 - label.len()))),
            timestamp: pi_js::time::now_ms() - 2000,
        };
        recent_user_id = harness
            .session_manager
            .append_message(Message::User(user).into())
            .unwrap();
        let assistant = AssistantMessage {
            content: vec![AssistantContentBlock::Text(TextContent::new(format!(
                "{label}{}",
                "y".repeat(400 - label.len())
            )))],
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage {
                input: total_tokens,
                total_tokens,
                ..Usage::default()
            },
            stop_reason: StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: pi_js::time::now_ms() - 1000,
            duration_ms: None,
        };
        harness
            .session_manager
            .append_message(Message::Assistant(assistant).into())
            .unwrap();
    }
    harness
        .session
        .agent()
        .replace_messages(harness.session_manager.build_session_context().messages);
    recent_user_id
}

#[tokio::test]
async fn uses_model_token_settings_for_all_compaction_paths_and_extension_preparation() {
    for path in ["manual", "pre-prompt", "post-run", "overflow"] {
        let preparations: Arc<Mutex<Vec<SessionBeforeCompactEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let prep_ref = Arc::clone(&preparations);
        let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
            let preps = Arc::clone(&prep_ref);
            async move {
                pi.on::<SessionBeforeCompactEvent>(move |ev, _ctx| {
                    let preps_inner = Arc::clone(&preps);
                    Box::pin(async move {
                        let first_kept = ev.preparation.first_kept_entry_id.clone();
                        let tokens_before = ev.preparation.tokens_before;
                        preps_inner.lock().unwrap_or_else(PoisonError::into_inner).push(ev);
                        Ok(Some(SessionBeforeCompactResult {
                            cancel: None,
                            compaction: Some(CompactionResult {
                                summary: "compacted history".to_string(),
                                first_kept_entry_id: first_kept,
                                tokens_before,
                                estimated_tokens_after: None,
                                usage: None,
                                details: None,
                            }),
                        }))
                    })
                });
                Ok(())
            }
        }));

        let mut model_overrides: IndexMap<String, Value> = IndexMap::new();
        model_overrides.insert(
            "faux/faux-1".to_string(),
            json!({ "reserveTokens": 2000, "keepRecentTokens": 150 }),
        );
        let harness = create_harness(HarnessOptions {
            models: Some(vec![faux_def("faux-1", Some(4000), None)]),
            tools: Some(vec![]),
            settings: Some(Settings {
                compaction: Some(CompactionSettings {
                    enabled: Some(path != "manual"),
                    reserve_tokens: Some(10),
                    keep_recent_tokens: Some(20_000),
                    model_overrides: Some(model_overrides),
                }),
                ..Default::default()
            }),
            extension_factories: Some(vec![ext]),
            ..Default::default()
        })
        .await;

        let recent_user_id = seed_history(&harness, if path == "pre-prompt" { 2500 } else { 650 });

        if path == "manual" {
            harness.session.compact(None).await.unwrap();
        } else {
            let responses = if path == "overflow" {
                vec![
                    FauxResponseStep::Message(faux_assistant_message(
                        FauxAssistantMessageContent::Text(String::new()),
                        Some(pi_ai::providers::faux::FauxAssistantMessageOptions {
                            stop_reason: Some(StopReason::Error),
                            error_message: Some("prompt is too long".to_string()),
                            ..Default::default()
                        }),
                    )),
                    FauxResponseStep::Message(faux_assistant_message(
                        FauxAssistantMessageContent::Text("recovered".to_string()),
                        None,
                    )),
                ]
            } else {
                vec![FauxResponseStep::Message(faux_assistant_message(
                    FauxAssistantMessageContent::Text(if path == "post-run" {
                        "z".repeat(8000)
                    } else {
                        "done".to_string()
                    }),
                    None,
                ))]
            };
            harness.set_responses(responses);
            harness.session.prompt("continue", None).await.unwrap();
        }

        let captured = preparations.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].preparation.settings,
            CompactionPrepSettings {
                enabled: path != "manual",
                reserve_tokens: 2000,
                keep_recent_tokens: 150,
            }
        );
        let expected_reason = match path {
            "manual" => ExtCompactionReason::Manual,
            "overflow" => ExtCompactionReason::Overflow,
            _ => ExtCompactionReason::Threshold,
        };
        assert_eq!(captured[0].reason, expected_reason);
        if path == "manual" || path == "pre-prompt" {
            assert_eq!(captured[0].preparation.first_kept_entry_id, recent_user_id);
        }

        let compaction_ends: Vec<_> = harness
            .events()
            .into_iter()
            .filter_map(|e| match e {
                AgentSessionEvent::CompactionEnd {
                    aborted,
                    will_retry,
                    result,
                    ..
                } => Some((aborted, will_retry, result)),
                _ => None,
            })
            .collect();
        assert_eq!(compaction_ends.len(), 1);
        assert!(!compaction_ends[0].0);
        assert_eq!(compaction_ends[0].1, path == "overflow");
        assert_eq!(
            compaction_ends[0].2.as_ref().map(|r| r.summary.as_str()),
            Some("compacted history")
        );
        assert_eq!(harness.get_pending_response_count(), 0);
        harness.cleanup();
    }
}

#[tokio::test]
async fn uses_the_newly_selected_model_without_changing_ordinary_settings() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<SessionBeforeCompactEvent>(|ev, _ctx| {
            Box::pin(async move {
                Ok(Some(SessionBeforeCompactResult {
                    cancel: None,
                    compaction: Some(CompactionResult {
                        summary: "big model summary".to_string(),
                        first_kept_entry_id: ev.preparation.first_kept_entry_id,
                        tokens_before: ev.preparation.tokens_before,
                        estimated_tokens_after: None,
                        usage: None,
                        details: None,
                    }),
                }))
            })
        });
        Ok(())
    }));

    let mut model_overrides: IndexMap<String, Value> = IndexMap::new();
    model_overrides.insert(
        "faux/big".to_string(),
        json!({ "reserveTokens": 8000, "keepRecentTokens": 150 }),
    );
    let harness = create_harness(HarnessOptions {
        models: Some(vec![
            faux_def("small", Some(4000), None),
            faux_def("big", Some(10_000), None),
        ]),
        tools: Some(vec![]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                reserve_tokens: Some(10),
                model_overrides: Some(model_overrides),
                ..Default::default()
            }),
            ..Default::default()
        }),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    seed_history(&harness, 2500);
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("small response".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("big response".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("continue on small", None).await.unwrap();
    assert!(
        !harness
            .events()
            .into_iter()
            .any(|e| matches!(e, AgentSessionEvent::CompactionStart { .. }))
    );

    seed_history(&harness, 2500);
    harness
        .session
        .set_model(harness.get_model_by_id("big").unwrap(), None)
        .await
        .unwrap();
    harness.session.prompt("continue on big", None).await.unwrap();

    let compaction_ends: Vec<_> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::CompactionEnd { result, .. } => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(compaction_ends.len(), 1);
    assert_eq!(
        compaction_ends[0].as_ref().map(|r| r.summary.as_str()),
        Some("big model summary")
    );
    assert_eq!(
        harness.settings_manager.get_compaction_reserve_tokens(None).unwrap(),
        10
    );

    harness
        .session
        .set_model(harness.get_model_by_id("small").unwrap(), None)
        .await
        .unwrap();
    let active_model = harness.session.model();
    let active_ref = active_model.as_ref().map(|m| ModelRef {
        provider: m.provider.clone(),
        id: m.id.clone(),
    });
    assert_eq!(
        harness
            .settings_manager
            .get_compaction_reserve_tokens(active_ref.as_ref())
            .unwrap(),
        10
    );
    harness.cleanup();
}

#[tokio::test]
async fn captures_model_identity_before_awaiting_summarization_auth() {
    let mut model_overrides: IndexMap<String, Value> = IndexMap::new();
    model_overrides.insert(
        "faux/first".to_string(),
        json!({ "reserveTokens": 2000, "keepRecentTokens": 150 }),
    );
    model_overrides.insert(
        "faux/second".to_string(),
        json!({ "reserveTokens": 4000, "keepRecentTokens": 20_000 }),
    );

    // Trigger model mutation on stream request or verify compaction resolves the first model's budget
    // before awaiting auth: even if `agent.state.model` flips before `stream` runs, the captured model stands.
    let harness = create_harness(HarnessOptions {
        models: Some(vec![faux_def("first", None, None), faux_def("second", None, None)]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                model_overrides: Some(model_overrides),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;
    seed_history(&harness, 650);

    let session_slot: Arc<Mutex<Option<AgentSession>>> = Arc::new(Mutex::new(Some(harness.session.clone())));
    let second_model = harness.get_model_by_id("second").unwrap();
    let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let req_ref = Arc::clone(&requests);
    let slot_ref = Arc::clone(&session_slot);

    harness.set_responses(vec![FauxResponseStep::Factory(Arc::new(
        move |_ctx, _opts, _st, req_model| {
            let r = Arc::clone(&req_ref);
            let s = Arc::clone(&slot_ref);
            let second = second_model.clone();
            Box::pin(async move {
                if let Some(sess) = s.lock().unwrap_or_else(PoisonError::into_inner).take() {
                    sess.agent().set_model(second);
                }
                r.lock().unwrap_or_else(PoisonError::into_inner).push(req_model.id);
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("summary".to_string()),
                    None,
                ))
            })
        },
    ))]);

    harness.session.compact(None).await.unwrap();
    assert_eq!(
        *requests.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["first".to_string()]
    );
    harness.cleanup();
}
