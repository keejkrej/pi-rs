//! Port of packages/coding-agent/test/suite/regressions/9340-9777-auto-compaction-cancellation.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use pi_agent_core::types::AgentMessage;
use pi_ai::auth::types::{ApiKeyAuth, ApiKeyAuthInput, AuthResult, ProviderAuth};
use pi_ai::models::Provider;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxModelDefinition, FauxResponseStep,
    faux_assistant_message,
};
use pi_ai::types::{
    Message, Model, StopReason, TextContent, Usage, UsageCost, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_ai::utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use pi_coding_agent::core::agent_session::{AgentSessionEvent, CompactionReason};
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, SessionBeforeCompactEvent, SessionBeforeCompactResult,
};
use pi_coding_agent::core::settings_manager::{CompactionSettings, RetrySettings, Settings};
use pi_js::abort::AbortReason;
use pretty_assertions::assert_eq;
use support::suite::harness::{Harness, HarnessOptions, create_harness};
use tokio::sync::Notify;

fn seed_compactable_session(harness: &Harness) {
    let model = harness.get_model();
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(&"x".repeat(500)))]),
            timestamp: 1,
        })))
        .unwrap();
    let mut assistant = faux_assistant_message(
        FauxAssistantMessageContent::Text("y".repeat(200)),
        Some(FauxAssistantMessageOptions {
            timestamp: Some(2),
            ..Default::default()
        }),
    );
    assistant.api = model.api;
    assistant.provider = model.provider;
    assistant.model = model.id;
    assistant.usage = Usage {
        input: 100,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 100,
        cost: UsageCost::default(),
    };
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::Assistant(assistant)))
        .unwrap();
    let ctx_msgs = harness.session_manager.build_session_context().messages;
    harness.session.agent().replace_messages(ctx_msgs);
}

enum AuthMode {
    WaitForAbort {
        auth_started: Arc<Notify>,
        captured_aborted: Arc<AtomicBool>,
    },
    Fail {
        error: pi_js::Error,
    },
}

struct RegressionAuth {
    mode: AuthMode,
}

#[async_trait]
impl ApiKeyAuth for RegressionAuth {
    fn name(&self) -> &str {
        "Regression test key auth"
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
        match &self.mode {
            AuthMode::WaitForAbort {
                auth_started,
                captured_aborted,
            } => {
                let aborted = Arc::new(Notify::new());
                let ab_c = Arc::clone(&aborted);
                let flag = Arc::clone(captured_aborted);
                let _guard = input.signal.on_abort(move |_reason| {
                    flag.store(true, Ordering::SeqCst);
                    ab_c.notify_one();
                });
                if input.signal.aborted() {
                    captured_aborted.store(true, Ordering::SeqCst);
                    aborted.notify_one();
                }
                let wait = aborted.notified();
                auth_started.notify_one();
                wait.await;
                captured_aborted.store(input.signal.aborted(), Ordering::SeqCst);
                Err(pi_js::Error::Abort(AbortReason::abort()))
            }
            AuthMode::Fail { error } => Err(pi_js::Error::msg(error.to_string())),
        }
    }
}

struct FailingAuth {
    error_kind: bool,
    message: String,
}

#[async_trait]
impl ApiKeyAuth for FailingAuth {
    fn name(&self) -> &str {
        "Failing key auth"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
        if self.error_kind {
            Err(pi_js::Error::Abort(AbortReason::error("AbortError", &self.message)))
        } else {
            Err(pi_js::Error::msg(&self.message))
        }
    }
}

struct RegressionProvider {
    id: String,
    models: Vec<Model>,
    auth: ProviderAuth,
}

impl Provider for RegressionProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "Regression provider"
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        self.models.clone()
    }

    fn stream(
        &self,
        _model: &Model,
        _context: &pi_ai::TranscriptContext,
        _options: Option<pi_ai::ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        create_assistant_message_event_stream()
    }

    fn stream_simple(
        &self,
        _model: &Model,
        _context: &pi_ai::TranscriptContext,
        _options: Option<pi_ai::SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        create_assistant_message_event_stream()
    }
}

#[tokio::test]
async fn does_not_start_post_run_auto_compaction_after_abort() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<SessionBeforeCompactEvent>(|_ev, _ctx| {
            Box::pin(async move {
                Ok(Some(SessionBeforeCompactResult {
                    cancel: Some(true),
                    compaction: None,
                }))
            })
        });
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            name: None,
            reasoning: None,
            input: None,
            input_limits: None,
            cost: None,
            context_window: Some(200),
            max_tokens: Some(50),
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
                reserve_tokens: Some(50),
                keep_recent_tokens: Some(1),
                ..Default::default()
            }),
            retry: Some(RetrySettings {
                enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        }),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    seed_compactable_session(&harness);
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text(String::new()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("Synthetic network failure".to_string()),
            ..Default::default()
        }),
    ))]);

    let session_c = harness.session.clone();
    let _unsub = harness.session.subscribe(move |event| {
        if let AgentSessionEvent::MessageEnd { message } = event
            && message.role() == Some("assistant")
        {
            session_c.abort_compaction();
            let s = session_c.clone();
            tokio::spawn(async move {
                let _ = s.abort().await;
            });
        }
    });

    harness.session.prompt(&"z".repeat(1000), None).await.unwrap();

    let starts = harness
        .events()
        .into_iter()
        .filter(|e| matches!(e, AgentSessionEvent::CompactionStart { .. }))
        .count();
    assert_eq!(starts, 0);
    harness.cleanup();
}

#[tokio::test]
async fn cancels_summarization_authentication_on_session_abort() {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: Some(false),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                keep_recent_tokens: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;

    let auth_started = Arc::new(Notify::new());
    let captured_aborted = Arc::new(AtomicBool::new(false));
    let model = harness.get_model();
    let provider: Arc<dyn Provider> = Arc::new(RegressionProvider {
        id: model.provider.clone(),
        models: harness.models.clone(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(RegressionAuth {
                mode: AuthMode::WaitForAbort {
                    auth_started: Arc::clone(&auth_started),
                    captured_aborted: Arc::clone(&captured_aborted),
                },
            })),
            oauth: None,
        },
    });
    harness
        .session
        .model_runtime()
        .register_native_provider(provider)
        .unwrap();
    seed_compactable_session(&harness);

    let started_wait = auth_started.notified();
    let session = harness.session.clone();
    let compaction = tokio::spawn(async move {
        session
            .run_auto_compaction_for_test(CompactionReason::Threshold, false)
            .await
    });
    started_wait.await;

    let started = harness
        .events()
        .into_iter()
        .filter(|e| matches!(e, AgentSessionEvent::CompactionStart { .. }))
        .count();
    let was_compacting = harness.session.is_compacting();
    let abort_session = harness.session.clone();
    let (compact_res, ()) = tokio::join!(compaction, async move {
        abort_session.abort().await;
    });
    let _ = compact_res.unwrap();

    assert_eq!(started, 1);
    assert_eq!(was_compacting, true);
    assert_eq!(captured_aborted.load(Ordering::SeqCst), true);

    let last_aborted = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::CompactionEnd { aborted, .. } => Some(aborted),
            _ => None,
        })
        .last();
    assert_eq!(last_aborted, Some(true));
    harness.cleanup();
}

#[tokio::test]
async fn cancels_synchronously_from_compaction_start() {
    let harness = create_harness(HarnessOptions {
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                keep_recent_tokens: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;
    seed_compactable_session(&harness);

    let session_c = harness.session.clone();
    let _unsub = harness.session.subscribe(move |event| {
        if matches!(event, AgentSessionEvent::CompactionStart { .. }) {
            session_c.abort_compaction();
        }
    });

    harness
        .session
        .run_auto_compaction_for_test(CompactionReason::Threshold, false)
        .await;

    assert_eq!(harness.faux.state().call_count(), 0);
    let last_aborted = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::CompactionEnd { aborted, .. } => Some(aborted),
            _ => None,
        })
        .last();
    assert_eq!(last_aborted, Some(true));
    harness.cleanup();
}

async fn assert_auth_failure_not_treated_as_abort(is_abort_error: bool, expected_message: &str) {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: Some(false),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                keep_recent_tokens: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;
    let model = harness.get_model();
    let provider: Arc<dyn Provider> = Arc::new(RegressionProvider {
        id: model.provider.clone(),
        models: harness.models.clone(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(FailingAuth {
                error_kind: is_abort_error,
                message: expected_message.to_string(),
            })),
            oauth: None,
        },
    });
    harness
        .session
        .model_runtime()
        .register_native_provider(provider)
        .unwrap();
    seed_compactable_session(&harness);

    harness
        .session
        .run_auto_compaction_for_test(CompactionReason::Threshold, false)
        .await;

    let end = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::CompactionEnd {
                aborted, error_message, ..
            } => Some((aborted, error_message)),
            _ => None,
        })
        .last()
        .expect("expected CompactionEnd event");
    assert_eq!(end.0, false);
    assert!(
        end.1.as_deref().unwrap_or("").contains(expected_message),
        "expected error_message {:?} to contain {:?}",
        end.1,
        expected_message
    );
    harness.cleanup();
}

#[tokio::test]
async fn reports_matching_error_text_without_abort_signal_as_failure() {
    assert_auth_failure_not_treated_as_abort(false, "Compaction cancelled").await;
}

#[tokio::test]
async fn reports_unrelated_abort_error_as_failure() {
    assert_auth_failure_not_treated_as_abort(true, "auth failed").await;
}

#[tokio::test]
async fn reports_extension_cancellation_as_aborted() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<SessionBeforeCompactEvent>(|_ev, _ctx| {
            Box::pin(async move {
                Ok(Some(SessionBeforeCompactResult {
                    cancel: Some(true),
                    compaction: None,
                }))
            })
        });
        Ok(())
    }));
    let seen_calls: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let harness = create_harness(HarnessOptions {
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                keep_recent_tokens: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    seed_compactable_session(&harness);

    let ok = harness
        .session
        .run_auto_compaction_for_test(CompactionReason::Threshold, false)
        .await;
    *seen_calls.lock().unwrap_or_else(PoisonError::into_inner) += usize::from(!ok);

    let last_aborted = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::CompactionEnd { aborted, .. } => Some(aborted),
            _ => None,
        })
        .last();
    assert_eq!(last_aborted, Some(true));
    harness.cleanup();
}
