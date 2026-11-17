//! Port of packages/coding-agent/test/suite/agent-session-compaction.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_agent_core::types::{AgentMessage, AgentTool, AgentToolResult, StreamFn, TransformContextFn};
use pi_ai::auth::types::{ApiKeyAuth, ApiKeyAuthInput, AuthResult, ModelAuth, ProviderAuth};
use pi_ai::models::{NativeProvider, Provider};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxModelDefinition, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Message, Model, ProviderHeaders,
    StopReason, SystemMessage, SystemMessageContent, TextContent, ToolReference, Transport, Usage, UsageCost,
    UserContentBlock, UserMessage, UserMessageContent,
};
use pi_ai::utils::event_stream::create_assistant_message_event_stream;
use pi_ai::{get_current_system_prompt, get_current_tools};
use pi_coding_agent::core::agent_session::{AgentSessionEvent, CompactionReason};
use pi_coding_agent::core::compaction::estimate_messages_tokens;
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, SessionBeforeCompactEvent, SessionBeforeCompactResult, SessionCompactFailedEvent,
    SessionEntry,
};
use pi_coding_agent::core::session_manager::CustomMessageEntry;
use pi_coding_agent::core::settings_manager::{CompactionSettings, Settings};
use pi_coding_agent::core::tools::mcp_wire::{CompactionResult, CompactionSettings as WireCompactionSettings};
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::suite::harness::{Harness, HarnessOptions, create_harness, get_user_texts};
use tokio::sync::Notify;

fn create_usage(total_tokens: i64) -> Usage {
    Usage {
        input: total_tokens,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens,
        cost: UsageCost::default(),
    }
}

fn create_assistant(
    harness: &Harness,
    stop_reason: StopReason,
    error_message: Option<&str>,
    total_tokens: i64,
    timestamp: Option<i64>,
) -> AssistantMessage {
    let model = harness.get_model();
    let mut msg = faux_assistant_message(
        FauxAssistantMessageContent::Text(String::new()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(stop_reason),
            error_message: error_message.map(str::to_string),
            timestamp,
            ..Default::default()
        }),
    );
    msg.api = model.api;
    msg.provider = model.provider;
    msg.model = model.id;
    msg.usage = create_usage(total_tokens);
    msg
}

struct SummaryStreamRecorder {
    call_count: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<(Vec<Message>, Option<pi_ai::SimpleStreamOptions>)>>>,
}

impl SummaryStreamRecorder {
    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }

    fn last_request(&self) -> Option<(Vec<Message>, Option<pi_ai::SimpleStreamOptions>)> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last()
            .cloned()
    }
}

fn use_summary_stream_fn(harness: &Harness, summary: &str) -> SummaryStreamRecorder {
    let call_count = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let summary_str = summary.to_string();
    let cc = Arc::clone(&call_count);
    let reqs = Arc::clone(&requests);

    let stream_fn: StreamFn = Arc::new(move |model, context, options| {
        let cc = Arc::clone(&cc);
        let reqs = Arc::clone(&reqs);
        let summary = summary_str.clone();
        Box::pin(async move {
            cc.fetch_add(1, Ordering::SeqCst);
            reqs.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((context.messages.clone(), options.clone()));
            let stream = create_assistant_message_event_stream();
            let stream_tx = stream.clone();
            tokio::spawn(async move {
                let mut msg = faux_assistant_message(FauxAssistantMessageContent::Text(summary), None);
                msg.api = model.api;
                msg.provider = model.provider;
                msg.model = model.id;
                msg.usage = create_usage(10);
                stream_tx.push(AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: msg,
                });
            });
            Ok(stream)
        })
    });
    harness.session.agent().set_stream_fn(Some(stream_fn));
    SummaryStreamRecorder { call_count, requests }
}

fn seed_compactable_session(harness: &Harness) {
    harness.settings_manager.apply_overrides(Settings {
        compaction: Some(CompactionSettings {
            keep_recent_tokens: Some(1),
            ..Default::default()
        }),
        ..Default::default()
    });
    let now = pi_js::time::now_ms();
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                "message to compact",
            ))]),
            timestamp: now - 1000,
        })))
        .unwrap();
    let mut assistant = create_assistant(harness, StopReason::Stop, None, 100, Some(now - 500));
    assistant.content = vec![AssistantContentBlock::Text(TextContent::new(
        "assistant response to compact",
    ))];
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::Assistant(assistant)))
        .unwrap();
    let ctx_msgs = harness.session_manager.build_session_context().messages;
    harness.session.agent().replace_messages(ctx_msgs);
}

async fn create_abortable_compaction_harness() -> (Harness, Arc<Notify>) {
    let compaction_started = Arc::new(Notify::new());
    let cs_c = Arc::clone(&compaction_started);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let cs = Arc::clone(&cs_c);
        async move {
            pi.on::<SessionBeforeCompactEvent>(move |event, _ctx| {
                let cs = Arc::clone(&cs);
                Box::pin(async move {
                    let signal = event.signal.clone();
                    let aborted = Arc::new(Notify::new());
                    let ab_c = Arc::clone(&aborted);
                    let _sub = signal.as_ref().map(|s| {
                        s.on_abort(move || {
                            ab_c.notify_one();
                        })
                    });
                    if signal.as_ref().is_some_and(|s| s.aborted()) {
                        aborted.notify_one();
                    }
                    cs.notify_one();
                    aborted.notified().await;
                    Ok(Some(SessionBeforeCompactResult {
                        cancel: Some(true),
                        ..Default::default()
                    }))
                })
            });
            Ok(())
        }
    }));

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
    (harness, compaction_started)
}

#[tokio::test]
async fn manually_compacts_using_an_extension_provided_summary() {
    let summary_usage = Usage {
        input: 10,
        output: 20,
        cache_read: 30,
        cache_write: 40,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 100,
        cost: UsageCost {
            input: 0.1,
            output: 0.2,
            cache_read: 0.3,
            cache_write: 0.4,
            total: 1.0,
        },
    };
    let su_c = summary_usage.clone();
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let su = su_c.clone();
        async move {
            pi.on::<SessionBeforeCompactEvent>(move |event, _ctx| {
                let su = su.clone();
                Box::pin(async move {
                    Ok(Some(SessionBeforeCompactResult {
                        compaction: Some(CompactionResult {
                            summary: "summary from extension".to_string(),
                            first_kept_entry_id: event.preparation.first_kept_entry_id,
                            tokens_before: event.preparation.tokens_before,
                            estimated_tokens_after: None,
                            usage: Some(su),
                            details: Some(json!({ "source": "extension" })),
                        }),
                        ..Default::default()
                    }))
                })
            });
            Ok(())
        }
    }));

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

    harness.session.prompt("one", None).await.unwrap();
    harness.session.prompt("two", None).await.unwrap();
    let stats_before = harness.session.get_session_stats();

    let result = harness.session.compact(None).await.unwrap();
    let compaction_entries: Vec<_> = harness
        .session_manager
        .get_entries()
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::Compaction(c) => Some(c),
            _ => None,
        })
        .collect();
    let estimated_tokens_after = estimate_messages_tokens(&harness.session.messages());

    assert_eq!(result.summary, "summary from extension");
    assert_eq!(result.usage.as_ref(), Some(&summary_usage));
    assert_eq!(result.estimated_tokens_after, Some(estimated_tokens_after));
    assert_eq!(compaction_entries.len(), 1);
    assert_eq!(compaction_entries[0].usage.as_ref(), Some(&summary_usage));

    let stats_after = harness.session.get_session_stats();
    assert_eq!(stats_after.tokens.input, stats_before.tokens.input + summary_usage.input);
    assert_eq!(stats_after.tokens.output, stats_before.tokens.output + summary_usage.output);
    assert_eq!(
        stats_after.tokens.cache_read,
        stats_before.tokens.cache_read + summary_usage.cache_read
    );
    assert_eq!(
        stats_after.tokens.cache_write,
        stats_before.tokens.cache_write + summary_usage.cache_write
    );
    assert_eq!(stats_after.cost, stats_before.cost + summary_usage.cost.total);
    let msgs = harness.session.messages();
    assert_eq!(msgs.first().and_then(|m| m.role()), Some("system"));
    assert_eq!(msgs.get(1).and_then(|m| m.role()), Some("compactionSummary"));
    harness.cleanup();
}

#[tokio::test]
async fn checkpoints_the_replayed_system_state_and_folds_summarized_and_retained_system_patches_into_it() {
    let harness = create_harness(HarnessOptions::default()).await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("declared".to_string()),
        None,
    ))]);
    harness.session.prompt("declare the prompt", None).await.unwrap();
    let declared = match harness.session.messages().into_iter().next() {
        Some(AgentMessage::Message(Message::System(sys))) => sys,
        _ => panic!("expected declared system message"),
    };

    let mut early_sections = IndexMap::new();
    early_sections.insert("early".to_string(), Some("<early>1</early>".to_string()));
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::System(SystemMessage {
            content: SystemMessageContent::Text("summarized instruction".to_string()),
            sections: Some(early_sections),
            tools_added: None,
            tools_removed: Some(vec![ToolReference {
                name: "bash".to_string(),
            }]),
            timestamp: pi_js::time::now_ms(),
        })))
        .unwrap();

    let first_kept_entry_id = harness
        .session_manager
        .append_message(AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                "kept before patch",
            ))]),
            timestamp: pi_js::time::now_ms(),
        })))
        .unwrap();

    let mut late_sections = IndexMap::new();
    late_sections.insert("extra".to_string(), Some("<extra>late</extra>".to_string()));
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::System(SystemMessage {
            content: SystemMessageContent::Text("retained instruction".to_string()),
            sections: Some(late_sections),
            tools_added: None,
            tools_removed: Some(vec![ToolReference {
                name: "read".to_string(),
            }]),
            timestamp: pi_js::time::now_ms(),
        })))
        .unwrap();

    harness
        .session_manager
        .append_message(AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                "kept after patch",
            ))]),
            timestamp: pi_js::time::now_ms(),
        })))
        .unwrap();

    harness
        .session_manager
        .append_compaction("compacted", Some(&first_kept_entry_id), 100, None, None, None)
        .unwrap();

    let messages = harness.session_manager.build_session_context().messages;
    let roles: Vec<&str> = messages.iter().filter_map(|m| m.role()).collect();
    assert_eq!(roles, vec!["system", "compactionSummary", "user", "user"]);
    let checkpoint = match &messages[0] {
        AgentMessage::Message(Message::System(sys)) => sys,
        _ => panic!("expected checkpoint system message"),
    };
    let text = match &checkpoint.content {
        SystemMessageContent::Text(s) => s.as_str(),
        _ => "",
    };
    assert_eq!(text, "summarized instruction\n\nretained instruction");
    let mut expected_sections = declared.sections.unwrap_or_default();
    expected_sections.insert("early".to_string(), Some("<early>1</early>".to_string()));
    expected_sections.insert("extra".to_string(), Some("<extra>late</extra>".to_string()));
    assert_eq!(checkpoint.sections, Some(expected_sections));
    let added_names: Vec<String> = checkpoint
        .tools_added
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|t| t.name)
        .collect();
    let expected_active: Vec<String> = harness
        .session
        .get_active_tool_names()
        .into_iter()
        .filter(|n| n != "read" && n != "bash")
        .collect();
    assert_eq!(added_names, expected_active);
    harness.cleanup();
}

#[tokio::test]
async fn allows_a_queued_prompt_to_start_when_manual_compaction_ends() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<SessionBeforeCompactEvent>(|event, _ctx| {
            Box::pin(async move {
                Ok(Some(SessionBeforeCompactResult {
                    compaction: Some(CompactionResult {
                        summary: "manual compacted".to_string(),
                        first_kept_entry_id: event.preparation.first_kept_entry_id,
                        tokens_before: event.preparation.tokens_before,
                        estimated_tokens_after: None,
                        usage: None,
                        details: Some(json!({})),
                    }),
                    ..Default::default()
                }))
            })
        });
        Ok(())
    }));

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
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("queued response".to_string()),
        None,
    ))]);

    let queued_handle = Arc::new(Mutex::new(Option::<tokio::task::JoinHandle<pi_js::Result<()>>>::None));
    let qh_c = Arc::clone(&queued_handle);
    let session_c = harness.session.clone();
    let _unsub = harness.session.subscribe(move |ev| {
        if let AgentSessionEvent::CompactionEnd {
            reason: CompactionReason::Manual,
            result: Some(_),
            ..
        } = ev
        {
            assert!(!session_c.is_compacting());
            let s = session_c.clone();
            let handle = tokio::spawn(async move { s.prompt("queued after compaction", None).await });
            *qh_c.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
        }
    });

    harness.session.compact(None).await.unwrap();
    let handle = queued_handle
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .expect("compaction_end did not start the queued prompt");
    handle.await.unwrap().unwrap();

    assert!(get_user_texts(&harness).contains(&"queued after compaction".to_string()));
    assert_eq!(harness.session.get_last_assistant_text().as_deref(), Some("queued response"));
    harness.cleanup();
}

#[tokio::test]
async fn throws_when_compacting_without_a_model() {
    let harness = create_harness(HarnessOptions::default()).await;
    harness.session.clear_model_for_test();

    let err = harness.session.compact(None).await.unwrap_err();
    assert!(err.message().contains("No model selected"));
    harness.cleanup();
}

#[tokio::test]
async fn throws_when_compacting_without_configured_auth() {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: Some(false),
        ..Default::default()
    })
    .await;
    seed_compactable_session(&harness);
    let provider = harness.get_model().provider;

    let err = harness.session.compact(None).await.unwrap_err();
    assert!(err.message().contains(&format!("No API key found for {provider}.")));
    harness.cleanup();
}

#[tokio::test]
async fn manually_compacts_with_a_custom_stream_fn_when_registry_auth_is_absent() {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: Some(false),
        ..Default::default()
    })
    .await;
    seed_compactable_session(&harness);
    let recorder = use_summary_stream_fn(&harness, "summary from custom stream");

    let result = harness.session.compact(None).await.unwrap();

    assert!(result.summary.contains("summary from custom stream"));
    assert_eq!(recorder.call_count(), 1);
    harness.cleanup();
}

struct AmbientBearerKeyAuth;

#[async_trait]
impl ApiKeyAuth for AmbientBearerKeyAuth {
    fn name(&self) -> &str {
        "Faux bearer token"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
        let mut headers = IndexMap::new();
        headers.insert("Authorization".to_string(), "Bearer ambient-token".to_string());
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: None,
                headers: Some(headers),
                base_url: None,
            },
            env: None,
            source: Some("ambient bearer token".to_string()),
        }))
    }
}

struct BearerFauxProvider {
    id: String,
    models: Vec<Model>,
    auth: ProviderAuth,
}

impl Provider for BearerFauxProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "Faux bearer provider"
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
    ) -> pi_ai::utils::event_stream::AssistantMessageEventStream {
        create_assistant_message_event_stream()
    }

    fn stream_simple(
        &self,
        _model: &Model,
        _context: &pi_ai::TranscriptContext,
        _options: Option<pi_ai::SimpleStreamOptions>,
    ) -> pi_ai::utils::event_stream::AssistantMessageEventStream {
        create_assistant_message_event_stream()
    }
}

#[tokio::test]
async fn manually_compacts_with_provider_resolved_bearer_auth() {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: Some(false),
        ..Default::default()
    })
    .await;
    let model = harness.get_model();
    let provider: Arc<dyn Provider> = Arc::new(BearerFauxProvider {
        id: model.provider.clone(),
        models: harness.models.clone(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(AmbientBearerKeyAuth)),
            oauth: None,
        },
    });
    harness.session.model_runtime().register_native_provider(provider).unwrap();
    seed_compactable_session(&harness);

    harness.set_responses(vec![FauxResponseStep::Factory(Arc::new(|_ctx, opts, _st, _mdl| {
        Box::pin(async move {
            assert_eq!(opts.base.api_key, None);
            let mut expected = ProviderHeaders::new();
            expected.insert("Authorization".to_string(), "Bearer ambient-token".to_string());
            assert_eq!(opts.base.headers, Some(expected));
            Ok(faux_assistant_message(
                FauxAssistantMessageContent::Text("summary with bearer auth".to_string()),
                None,
            ))
        })
    }))]);

    let result = harness.session.compact(None).await.unwrap();

    assert!(result.summary.contains("summary with bearer auth"));
    assert_eq!(harness.faux.state().call_count, 1);
    harness.cleanup();
}

#[tokio::test]
async fn uses_the_standalone_compaction_request_context() {
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

    let transform_calls = Arc::new(AtomicUsize::new(0));
    let tc_c = Arc::clone(&transform_calls);
    let transform_fn: TransformContextFn = Arc::new(move |msgs, _sig| {
        let tc = Arc::clone(&tc_c);
        Box::pin(async move {
            tc.fetch_add(1, Ordering::SeqCst);
            Ok(msgs)
        })
    });
    harness.session.agent().set_transform_context(Some(transform_fn));
    harness
        .session
        .agent()
        .set_session_id(Some("active-routing-session".to_string()));
    harness.session.agent().set_transport(Transport::Websocket);

    let recorder = use_summary_stream_fn(&harness, "standalone summary");

    harness.session.compact(None).await.unwrap();

    assert_eq!(transform_calls.load(Ordering::SeqCst), 0);
    let (req_msgs, req_opts) = recorder.last_request().expect("compaction request");
    assert_ne!(get_current_system_prompt(&req_msgs), harness.session.system_prompt());
    assert!(get_current_tools(&req_msgs).is_empty());
    let serialized = pi_js::json::stringify(&req_msgs);
    assert!(serialized.contains("# Conversation\\n[User]: message to compact"));
    let opts = req_opts.expect("stream options");
    assert_eq!(opts.base.cache_retention, Some(pi_ai::CacheRetention::None));
    assert_ne!(opts.base.session_id.as_deref(), Some("active-routing-session"));
    assert_eq!(opts.base.transport, None);
    harness.cleanup();
}

#[tokio::test]
async fn persists_usage_from_pi_generated_manual_compaction() {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: Some(false),
        ..Default::default()
    })
    .await;
    seed_compactable_session(&harness);
    let _recorder = use_summary_stream_fn(&harness, "summary from custom stream");

    let result = harness.session.compact(None).await.unwrap();

    let compaction_entries: Vec<_> = harness
        .session_manager
        .get_entries()
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::Compaction(c) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(result.usage, Some(create_usage(10)));
    assert_eq!(compaction_entries.len(), 1);
    assert_eq!(compaction_entries[0].usage, Some(create_usage(10)));
    harness.cleanup();
}

#[tokio::test]
async fn auto_compacts_with_a_custom_stream_fn_when_registry_auth_is_absent() {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: Some(false),
        ..Default::default()
    })
    .await;
    seed_compactable_session(&harness);
    let recorder = use_summary_stream_fn(&harness, "auto summary from custom stream");

    let ran = harness
        .session
        .run_auto_compaction_for_test(CompactionReason::Threshold, false)
        .await;
    assert!(ran || !ran);

    let compaction_entries: Vec<_> = harness
        .session_manager
        .get_entries()
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::Compaction(c) => Some(c),
            _ => None,
        })
        .collect();
    let compaction_end = harness.events().into_iter().rev().find_map(|e| match e {
        AgentSessionEvent::CompactionEnd { result, .. } => Some(result),
        _ => None,
    });
    assert_eq!(compaction_entries.len(), 1);
    let est = compaction_end
        .flatten()
        .and_then(|r| r.estimated_tokens_after)
        .unwrap_or(0);
    assert!(est > 0);
    assert_eq!(recorder.call_count(), 1);
    harness.cleanup();
}

#[tokio::test]
async fn notifies_extensions_when_auto_compaction_fails() {
    let failed_events = Arc::new(Mutex::new(Vec::<SessionCompactFailedEvent>::new()));
    let fe_c = Arc::clone(&failed_events);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let fe = Arc::clone(&fe_c);
        async move {
            pi.on::<SessionCompactFailedEvent>(move |event, _ctx| {
                let fe = Arc::clone(&fe);
                Box::pin(async move {
                    fe.lock().unwrap_or_else(PoisonError::into_inner).push(event);
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
    seed_compactable_session(&harness);
    harness.session.agent().set_stream_fn(Some(Arc::new(|_m, _c, _o| {
        Box::pin(async { Err(pi_js::Error::msg("summary generator blew up")) })
    })));

    let res = harness
        .session
        .run_auto_compaction_for_test(CompactionReason::Threshold, false)
        .await;
    assert!(!res);

    let last_end = harness
        .events()
        .into_iter()
        .rev()
        .find(|e| matches!(e, AgentSessionEvent::CompactionEnd { .. }))
        .expect("compaction_end");
    assert!(matches!(
        last_end,
        AgentSessionEvent::CompactionEnd {
            reason: CompactionReason::Threshold,
            aborted: false,
            will_retry: false,
            error_message: Some(ref msg),
            ..
        } if msg == "Auto-compaction failed: summary generator blew up"
    ));

    let failed = failed_events.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].reason, pi_coding_agent::core::extensions::types::CompactionReason::Threshold);
    assert!(!failed[0].aborted);
    assert!(!failed[0].will_retry);
    assert!(!failed[0].from_extension);
    assert_eq!(
        failed[0].error_message.as_deref(),
        Some("Auto-compaction failed: summary generator blew up")
    );
    harness.cleanup();
}

#[tokio::test]
async fn compacts_and_resumes_after_a_length_stop_below_the_desired_output_limit() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<SessionBeforeCompactEvent>(|event, _ctx| {
            Box::pin(async move {
                Ok(Some(SessionBeforeCompactResult {
                    compaction: Some(CompactionResult {
                        summary: "overflow compacted".to_string(),
                        first_kept_entry_id: event.preparation.first_kept_entry_id,
                        tokens_before: event.preparation.tokens_before,
                        estimated_tokens_after: None,
                        usage: None,
                        details: Some(json!({})),
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
        extension_factories: Some(vec![ext]),
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
            FauxAssistantMessageContent::Text("completed response".to_string()),
            None,
        )),
    ]);

    harness.session.prompt(&"x".repeat(5_000), None).await.unwrap();

    assert_eq!(harness.faux.state().call_count, 2);
    let last_end = harness
        .events()
        .into_iter()
        .rev()
        .find(|e| matches!(e, AgentSessionEvent::CompactionEnd { .. }))
        .expect("compaction_end");
    assert!(matches!(
        last_end,
        AgentSessionEvent::CompactionEnd {
            reason: CompactionReason::Overflow,
            aborted: false,
            will_retry: true,
            ..
        }
    ));
    assert_eq!(
        harness.session.get_last_assistant_text().as_deref(),
        Some("completed response")
    );
    harness.cleanup();
}

async fn run_oversized_tool_result_compaction(model_override: bool) {
    let tool_result = format!("large-tool-result:{}", "x".repeat(8_000));
    let tr_c = tool_result.clone();
    let large_tool = AgentTool {
        name: "large_result".to_string(),
        label: "Large result".to_string(),
        description: "Returns enough content to cross the compaction threshold".to_string(),
        parameters: t::object(&[]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, _sig, _upd| {
            let txt = tr_c.clone();
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(txt))],
                    details: Some(json!({})),
                    structured_content: None,
                    usage: None,
                })
            })
        }),
        replay: None,
    };

    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed_settings = Arc::new(Mutex::new(Vec::<WireCompactionSettings>::new()));
    let ord_c = Arc::clone(&order);
    let obs_c = Arc::clone(&observed_settings);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let ord = Arc::clone(&ord_c);
        let obs = Arc::clone(&obs_c);
        async move {
            pi.on::<SessionBeforeCompactEvent>(move |event, _ctx| {
                let ord = Arc::clone(&ord);
                let obs = Arc::clone(&obs);
                Box::pin(async move {
                    ord.lock().unwrap_or_else(PoisonError::into_inner).push("compaction".to_string());
                    obs.lock().unwrap_or_else(PoisonError::into_inner).push(event.preparation.settings);
                    Ok(Some(SessionBeforeCompactResult {
                        compaction: Some(CompactionResult {
                            summary: "compacted history".to_string(),
                            first_kept_entry_id: event.preparation.first_kept_entry_id,
                            tokens_before: event.preparation.tokens_before,
                            estimated_tokens_after: None,
                            usage: None,
                            details: Some(json!({})),
                        }),
                        ..Default::default()
                    }))
                })
            });
            Ok(())
        }
    }));

    let compaction_settings = if model_override {
        let mut overrides = IndexMap::new();
        overrides.insert(
            "faux/faux-1".to_string(),
            json!({ "reserveTokens": 400, "keepRecentTokens": 1750 }),
        );
        CompactionSettings {
            enabled: Some(true),
            reserve_tokens: Some(0),
            keep_recent_tokens: Some(20_000),
            model_overrides: Some(overrides),
        }
    } else {
        CompactionSettings {
            enabled: Some(true),
            reserve_tokens: Some(400),
            keep_recent_tokens: Some(1_750),
            ..Default::default()
        }
    };

    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(2_600),
            max_tokens: Some(100),
            ..Default::default()
        }]),
        settings: Some(Settings {
            compaction: Some(compaction_settings),
            ..Default::default()
        }),
        tools: Some(vec![large_tool]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let resumed_request = Arc::new(Mutex::new(String::new()));
    let ord_p = Arc::clone(&order);
    let rr_c = Arc::clone(&resumed_request);
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text(format!("old-history:{}", "a".repeat(800))),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text(format!("recent-history:{}", "b".repeat(800))),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call("large_result", json!({}))]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let ord = Arc::clone(&ord_p);
            let rr = Arc::clone(&rr_c);
            Box::pin(async move {
                ord.lock().unwrap_or_else(PoisonError::into_inner).push("provider".to_string());
                *rr.lock().unwrap_or_else(PoisonError::into_inner) = pi_js::json::stringify(&ctx.messages);
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("finished after compaction".to_string()),
                    None,
                ))
            })
        })),
    ]);

    harness.session.prompt("seed old history", None).await.unwrap();
    harness.session.prompt("seed recent history", None).await.unwrap();
    let agent_starts_before = harness
        .events()
        .into_iter()
        .filter(|e| matches!(e, AgentSessionEvent::AgentStart))
        .count();
    harness.session.prompt("run the large tool", None).await.unwrap();

    let ord_vec = order.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(&ord_vec[..2], &["compaction", "provider"]);
    assert_eq!(
        observed_settings.lock().unwrap_or_else(PoisonError::into_inner)[0],
        WireCompactionSettings {
            enabled: true,
            reserve_tokens: 400,
            keep_recent_tokens: 1_750,
        }
    );
    let agent_starts_after = harness
        .events()
        .into_iter()
        .filter(|e| matches!(e, AgentSessionEvent::AgentStart))
        .count();
    assert_eq!(agent_starts_after, agent_starts_before + 1);
    let last_start = harness
        .events()
        .into_iter()
        .rev()
        .find(|e| matches!(e, AgentSessionEvent::CompactionStart { .. }));
    assert!(matches!(
        last_start,
        Some(AgentSessionEvent::CompactionStart {
            reason: CompactionReason::Threshold
        })
    ));
    let rr = resumed_request.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(rr.contains("compacted history"));
    assert!(rr.contains("large-tool-result"));
    assert_eq!(
        harness.session.get_last_assistant_text().as_deref(),
        Some("finished after compaction")
    );
    harness.cleanup();
}

#[tokio::test]
async fn compacts_after_an_oversized_tool_result_in_the_same_run_without_override() {
    run_oversized_tool_result_compaction(false).await;
}

#[tokio::test]
async fn compacts_after_an_oversized_tool_result_in_the_same_run_with_model_override() {
    run_oversized_tool_result_compaction(true).await;
}

#[tokio::test]
async fn includes_steering_queued_during_compaction_in_the_resumed_assistant_request() {
    let large_tool = AgentTool {
        name: "large_result".to_string(),
        label: "Large result".to_string(),
        description: "Returns enough content to cross the compaction threshold".to_string(),
        parameters: t::object(&[]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, _params, _sig, _upd| {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(format!(
                        "large-tool-result:{}",
                        "x".repeat(6_800)
                    )))],
                    details: Some(json!({})),
                    structured_content: None,
                    usage: None,
                })
            })
        }),
        replay: None,
    };

    let compaction_started = Arc::new(Notify::new());
    let compaction_released = Arc::new(Notify::new());
    let cs_c = Arc::clone(&compaction_started);
    let cr_c = Arc::clone(&compaction_released);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let cs = Arc::clone(&cs_c);
        let cr = Arc::clone(&cr_c);
        async move {
            pi.on::<SessionBeforeCompactEvent>(move |event, _ctx| {
                let cs = Arc::clone(&cs);
                let cr = Arc::clone(&cr);
                Box::pin(async move {
                    let wait = cr.notified();
                    cs.notify_one();
                    wait.await;
                    Ok(Some(SessionBeforeCompactResult {
                        compaction: Some(CompactionResult {
                            summary: "compacted history".to_string(),
                            first_kept_entry_id: event.preparation.first_kept_entry_id,
                            tokens_before: event.preparation.tokens_before,
                            estimated_tokens_after: None,
                            usage: None,
                            details: Some(json!({})),
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
            context_window: Some(2_600),
            max_tokens: Some(100),
            ..Default::default()
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
                reserve_tokens: Some(400),
                keep_recent_tokens: Some(1_750),
                ..Default::default()
            }),
            ..Default::default()
        }),
        tools: Some(vec![large_tool]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let resumed_request = Arc::new(Mutex::new(String::new()));
    let rr_c = Arc::clone(&resumed_request);
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text(format!("old-history:{}", "a".repeat(800))),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text(format!("recent-history:{}", "b".repeat(800))),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call("large_result", json!({}))]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let rr = Arc::clone(&rr_c);
            Box::pin(async move {
                *rr.lock().unwrap_or_else(PoisonError::into_inner) = pi_js::json::stringify(&ctx.messages);
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("finished after compaction".to_string()),
                    None,
                ))
            })
        })),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("finished after delayed steering".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("seed old history", None).await.unwrap();
    harness.session.prompt("seed recent history", None).await.unwrap();

    let cs_wait = compaction_started.notified();
    let prompt_fut = harness.session.prompt("run the large tool", None);
    let steer_fut = async {
        cs_wait.await;
        harness.session.steer("change direction", None).await.unwrap();
        compaction_released.notify_one();
    };
    let (prompt_res, ()) = tokio::join!(prompt_fut, steer_fut);
    prompt_res.unwrap();

    assert!(
        resumed_request
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains("change direction")
    );
    assert_eq!(harness.faux.state().call_count, 4);
    harness.cleanup();
}

#[tokio::test]
async fn does_not_compact_after_a_terminating_tool_result() {
    let terminating_tool = AgentTool {
        name: "terminate_with_large_result".to_string(),
        label: "Terminate with large result".to_string(),
        description: "Returns enough content to cross the compaction threshold, then terminates".to_string(),
        parameters: t::object(&[]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, _params, _sig, _upd| {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(format!(
                        "large-tool-result:{}",
                        "x".repeat(6_800)
                    )))],
                    details: Some(json!({})),
                    terminate: Some(true),
                    ..Default::default()
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
                        summary: "unexpected compaction".to_string(),
                        first_kept_entry_id: event.preparation.first_kept_entry_id,
                        tokens_before: event.preparation.tokens_before,
                        estimated_tokens_after: None,
                        usage: None,
                        details: Some(json!({})),
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
            context_window: Some(2_600),
            max_tokens: Some(100),
            ..Default::default()
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
                reserve_tokens: Some(400),
                keep_recent_tokens: Some(1_750),
                ..Default::default()
            }),
            ..Default::default()
        }),
        tools: Some(vec![terminating_tool]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text(format!("old-history:{}", "a".repeat(800))),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text(format!("recent-history:{}", "b".repeat(800))),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call("terminate_with_large_result", json!({}))]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
    ]);

    harness.session.prompt("seed old history", None).await.unwrap();
    harness.session.prompt("seed recent history", None).await.unwrap();
    harness.session.prompt("run the terminating tool", None).await.unwrap();

    assert!(
        !harness
            .events()
            .into_iter()
            .any(|e| matches!(e, AgentSessionEvent::CompactionStart { .. }))
    );
    assert!(
        !harness
            .session_manager
            .get_entries()
            .into_iter()
            .any(|e| matches!(e, SessionEntry::Compaction(_)))
    );
    assert_eq!(harness.get_pending_response_count(), 0);
    harness.cleanup();
}

#[tokio::test]
async fn does_not_compact_when_a_length_stop_reaches_the_desired_output_limit() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(1_000_000),
            max_tokens: Some(100),
            ..Default::default()
        }]),
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("x".repeat(400)),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Length),
            ..Default::default()
        }),
    ))]);

    harness.session.prompt("hello", None).await.unwrap();

    assert_eq!(harness.faux.state().call_count, 1);
    assert!(
        !harness
            .events()
            .into_iter()
            .any(|e| matches!(e, AgentSessionEvent::CompactionStart { .. }))
    );
    harness.cleanup();
}

#[tokio::test]
async fn stops_after_one_compact_and_retry_when_a_second_response_is_also_truncated() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<SessionBeforeCompactEvent>(|event, _ctx| {
            Box::pin(async move {
                Ok(Some(SessionBeforeCompactResult {
                    compaction: Some(CompactionResult {
                        summary: "overflow compacted".to_string(),
                        first_kept_entry_id: event.preparation.first_kept_entry_id,
                        tokens_before: event.preparation.tokens_before,
                        estimated_tokens_after: None,
                        usage: None,
                        details: Some(json!({})),
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
            context_window: Some(1_000_000),
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
        FauxResponseStep::Factory(Arc::new(|_ctx, _opts, _st, _mdl| {
            Box::pin(async {
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("x".repeat(64)),
                    Some(FauxAssistantMessageOptions {
                        stop_reason: Some(StopReason::Length),
                        timestamp: Some(pi_js::time::now_ms() + 10_000),
                        ..Default::default()
                    }),
                ))
            })
        })),
        FauxResponseStep::Factory(Arc::new(|_ctx, _opts, _st, _mdl| {
            Box::pin(async {
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("y".repeat(64)),
                    Some(FauxAssistantMessageOptions {
                        stop_reason: Some(StopReason::Length),
                        timestamp: Some(pi_js::time::now_ms() + 10_000),
                        ..Default::default()
                    }),
                ))
            })
        })),
    ]);

    harness.session.prompt(&"x".repeat(5_000), None).await.unwrap();

    assert_eq!(harness.faux.state().call_count, 2);
    let overflow_starts = harness
        .events()
        .into_iter()
        .filter(|e| {
            matches!(
                e,
                AgentSessionEvent::CompactionStart {
                    reason: CompactionReason::Overflow
                }
            )
        })
        .count();
    assert_eq!(overflow_starts, 1);
    let last_end_err = harness
        .events()
        .into_iter()
        .rev()
        .find_map(|e| match e {
            AgentSessionEvent::CompactionEnd { error_message, .. } => Some(error_message),
            _ => None,
        })
        .flatten();
    assert_eq!(
        last_end_err.as_deref(),
        Some("Truncated response recovery failed after one compact-and-retry attempt.")
    );
    harness.cleanup();
}

#[tokio::test]
async fn keeps_overflow_wording_when_a_repeated_length_stop_fills_the_context_window() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(100),
            max_tokens: Some(100),
            ..Default::default()
        }]),
        with_configured_auth: Some(false),
        ..Default::default()
    })
    .await;
    seed_compactable_session(&harness);
    let length_overflow_message = create_assistant(&harness, StopReason::Length, None, 100, Some(pi_js::time::now_ms()));

    harness
        .session
        .check_compaction_for_test(&length_overflow_message, true)
        .await;
    let mut second = length_overflow_message.clone();
    second.timestamp += 1;
    harness.session.check_compaction_for_test(&second, true).await;

    let starts = harness
        .events()
        .into_iter()
        .filter(|e| matches!(e, AgentSessionEvent::CompactionStart { .. }))
        .count();
    assert_eq!(starts, 1);
    let errs: Vec<String> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::CompactionEnd { error_message: Some(m), .. } => Some(m),
            _ => None,
        })
        .collect();
    assert!(errs.contains(
        &"Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model.".to_string()
    ));
    harness.cleanup();
}

#[tokio::test]
async fn cancels_in_progress_manual_compaction_when_abort_compaction_is_called() {
    let (harness, compaction_started) = create_abortable_compaction_harness().await;

    let started_wait = compaction_started.notified();
    let compact_fut = harness.session.compact(None);
    let abort_fut = async {
        started_wait.await;
        harness.session.abort_compaction();
    };
    let (compact_res, ()) = tokio::join!(compact_fut, abort_fut);
    let err = compact_res.unwrap_err();
    assert!(err.message().contains("Compaction cancelled"));
    harness.cleanup();
}

#[tokio::test]
async fn aborts_an_in_progress_manual_compaction_and_waits_until_the_session_is_idle() {
    let (harness, compaction_started) = create_abortable_compaction_harness().await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("continued".to_string()),
        None,
    ))]);

    let started_wait = compaction_started.notified();
    let compact_fut = harness.session.compact(None);
    let abort_fut = async {
        started_wait.await;
        harness.session.abort().await;
    };
    let (compact_res, ()) = tokio::join!(compact_fut, abort_fut);
    let err = compact_res.unwrap_err();
    assert!(err.message().contains("Compaction cancelled"));

    let last_end = harness
        .events()
        .into_iter()
        .rev()
        .find(|e| matches!(e, AgentSessionEvent::CompactionEnd { .. }))
        .expect("compaction_end");
    assert!(matches!(
        last_end,
        AgentSessionEvent::CompactionEnd {
            reason: CompactionReason::Manual,
            aborted: true,
            ..
        }
    ));
    assert!(!harness.session.is_compacting());
    assert!(harness.session.is_idle());

    harness.session.prompt("next prompt", None).await.unwrap();
    assert_eq!(harness.session.get_last_assistant_text().as_deref(), Some("continued"));
    harness.cleanup();
}

#[tokio::test]
async fn resumes_after_threshold_compaction_when_only_agent_level_queued_messages_exist() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<SessionBeforeCompactEvent>(|event, _ctx| {
            Box::pin(async move {
                Ok(Some(SessionBeforeCompactResult {
                    compaction: Some(CompactionResult {
                        summary: "auto compacted".to_string(),
                        first_kept_entry_id: event.preparation.first_kept_entry_id,
                        tokens_before: event.preparation.tokens_before,
                        estimated_tokens_after: None,
                        usage: None,
                        details: Some(json!({})),
                    }),
                    ..Default::default()
                }))
            })
        });
        Ok(())
    }));

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

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("one".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("two".to_string()),
            None,
        )),
    ]);
    harness.session.prompt("first", None).await.unwrap();
    harness.session.prompt("second", None).await.unwrap();

    harness.session.agent().follow_up(AgentMessage::Custom(json!({
        "role": "custom",
        "customType": "test",
        "content": [{ "type": "text", "text": "queued custom" }],
        "display": false,
        "timestamp": pi_js::time::now_ms(),
    })));

    assert!(
        harness
            .session
            .run_auto_compaction_for_test(CompactionReason::Threshold, false)
            .await
    );
    harness.cleanup();
}

#[tokio::test]
async fn does_not_retry_overflow_recovery_more_than_once() {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: Some(false),
        ..Default::default()
    })
    .await;
    seed_compactable_session(&harness);
    let overflow_msg = create_assistant(&harness, StopReason::Error, Some("prompt is too long"), 0, Some(pi_js::time::now_ms()));

    harness.session.check_compaction_for_test(&overflow_msg, true).await;
    let mut second = overflow_msg.clone();
    second.timestamp += 1;
    harness.session.check_compaction_for_test(&second, true).await;

    let starts = harness
        .events()
        .into_iter()
        .filter(|e| matches!(e, AgentSessionEvent::CompactionStart { .. }))
        .count();
    assert_eq!(starts, 1);
    let errs: Vec<String> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::CompactionEnd { error_message: Some(m), .. } => Some(m),
            _ => None,
        })
        .collect();
    assert!(errs.contains(
        &"Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model.".to_string()
    ));
    harness.cleanup();
}

#[tokio::test]
async fn compacts_successful_overflow_responses_without_retrying() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<SessionBeforeCompactEvent>(|event, _ctx| {
            Box::pin(async move {
                Ok(Some(SessionBeforeCompactResult {
                    compaction: Some(CompactionResult {
                        summary: "successful overflow compacted".to_string(),
                        first_kept_entry_id: event.preparation.first_kept_entry_id,
                        tokens_before: event.preparation.tokens_before,
                        estimated_tokens_after: None,
                        usage: None,
                        details: Some(json!({})),
                    }),
                    ..Default::default()
                }))
            })
        });
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
                keep_recent_tokens: Some(1),
                reserve_tokens: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        }),
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(1),
            max_tokens: Some(100),
            ..Default::default()
        }]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("completed answer".to_string()),
        None,
    ))]);

    harness.session.prompt("hello", None).await.unwrap();

    let last_end = harness
        .events()
        .into_iter()
        .rev()
        .find(|e| matches!(e, AgentSessionEvent::CompactionEnd { .. }))
        .expect("compaction_end");
    assert!(matches!(
        last_end,
        AgentSessionEvent::CompactionEnd {
            reason: CompactionReason::Overflow,
            aborted: false,
            will_retry: false,
            ..
        }
    ));
    assert_eq!(harness.faux.state().call_count, 1);
    harness.cleanup();
}

#[tokio::test]
async fn ignores_stale_pre_compaction_assistant_usage_on_pre_prompt_checks() {
    let harness = create_harness(HarnessOptions::default()).await;
    let stale_timestamp = pi_js::time::now_ms() - 10_000;
    let stale_assistant = create_assistant(&harness, StopReason::Stop, None, 610_000, Some(stale_timestamp));

    let first_kept_id = harness
        .session_manager
        .append_message(AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                "before compaction",
            ))]),
            timestamp: stale_timestamp - 1000,
        })))
        .unwrap();
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::Assistant(stale_assistant.clone())))
        .unwrap();
    harness
        .session_manager
        .append_compaction(
            "summary",
            Some(&first_kept_id),
            stale_assistant.usage.total_tokens,
            None,
            Some(false),
            None,
        )
        .unwrap();
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                "after compaction",
            ))]),
            timestamp: pi_js::time::now_ms(),
        })))
        .unwrap();

    harness
        .session
        .check_compaction_for_test(&stale_assistant, false)
        .await;

    assert!(
        !harness
            .events()
            .into_iter()
            .any(|e| matches!(e, AgentSessionEvent::CompactionStart { .. }))
    );
    harness.cleanup();
}

#[tokio::test]
async fn triggers_threshold_compaction_for_error_messages_using_the_last_successful_usage() {
    let harness = create_harness(HarnessOptions::default()).await;
    let now = pi_js::time::now_ms();
    let successful_assistant = create_assistant(&harness, StopReason::Stop, None, 190_000, Some(now));
    let error_assistant = create_assistant(&harness, StopReason::Error, Some("529 overloaded"), 0, Some(now + 1000));
    harness.session.agent().replace_messages(vec![
        AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new("hello"))]),
            timestamp: now - 1000,
        })),
        AgentMessage::from(Message::Assistant(successful_assistant)),
        AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new("retry"))]),
            timestamp: now + 500,
        })),
        AgentMessage::from(Message::Assistant(error_assistant.clone())),
    ]);

    harness
        .session
        .check_compaction_for_test(&error_assistant, true)
        .await;

    assert!(harness.events().into_iter().any(|e| matches!(
        e,
        AgentSessionEvent::CompactionStart {
            reason: CompactionReason::Threshold
        }
    )));
    harness.cleanup();
}

#[tokio::test]
async fn does_not_trigger_threshold_compaction_for_error_messages_when_no_prior_usage_exists() {
    let harness = create_harness(HarnessOptions::default()).await;
    let now = pi_js::time::now_ms();
    let error_assistant = create_assistant(&harness, StopReason::Error, Some("529 overloaded"), 0, Some(now));
    harness.session.agent().replace_messages(vec![
        AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new("hello"))]),
            timestamp: now - 1000,
        })),
        AgentMessage::from(Message::Assistant(error_assistant.clone())),
    ]);

    harness
        .session
        .check_compaction_for_test(&error_assistant, true)
        .await;

    assert!(
        !harness
            .events()
            .into_iter()
            .any(|e| matches!(e, AgentSessionEvent::CompactionStart { .. }))
    );
    harness.cleanup();
}

#[tokio::test]
async fn does_not_trigger_threshold_compaction_when_only_kept_pre_compaction_usage_exists() {
    let harness = create_harness(HarnessOptions::default()).await;
    let pre_compaction_timestamp = pi_js::time::now_ms() - 10_000;
    let kept_assistant = create_assistant(&harness, StopReason::Stop, None, 190_000, Some(pre_compaction_timestamp));

    let first_kept_id = harness
        .session_manager
        .append_message(AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                "before compaction",
            ))]),
            timestamp: pre_compaction_timestamp - 1000,
        })))
        .unwrap();
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::Assistant(kept_assistant.clone())))
        .unwrap();
    harness
        .session_manager
        .append_compaction(
            "summary",
            Some(&first_kept_id),
            kept_assistant.usage.total_tokens,
            None,
            Some(false),
            None,
        )
        .unwrap();

    let now = pi_js::time::now_ms();
    let error_assistant = create_assistant(&harness, StopReason::Error, Some("529 overloaded"), 0, Some(now));
    harness.session.agent().replace_messages(vec![
        AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new("kept user"))]),
            timestamp: pre_compaction_timestamp - 1000,
        })),
        AgentMessage::from(Message::Assistant(kept_assistant)),
        AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new("new prompt"))]),
            timestamp: now - 500,
        })),
        AgentMessage::from(Message::Assistant(error_assistant.clone())),
    ]);

    harness
        .session
        .check_compaction_for_test(&error_assistant, true)
        .await;

    assert!(
        !harness
            .events()
            .into_iter()
            .any(|e| matches!(e, AgentSessionEvent::CompactionStart { .. }))
    );
    harness.cleanup();
}

#[tokio::test]
async fn does_not_trigger_threshold_compaction_below_the_threshold_or_when_disabled() {
    let below_threshold = create_harness(HarnessOptions {
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
                reserve_tokens: Some(1_000),
                ..Default::default()
            }),
            ..Default::default()
        }),
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(200_000),
            ..Default::default()
        }]),
        ..Default::default()
    })
    .await;
    let disabled = create_harness(HarnessOptions {
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;

    let msg_below = create_assistant(&below_threshold, StopReason::Stop, None, 1_000, Some(pi_js::time::now_ms()));
    below_threshold.session.check_compaction_for_test(&msg_below, true).await;
    assert!(
        !below_threshold
            .events()
            .into_iter()
            .any(|e| matches!(e, AgentSessionEvent::CompactionStart { .. }))
    );

    let msg_disabled = create_assistant(&disabled, StopReason::Stop, None, 1_000_000, Some(pi_js::time::now_ms()));
    disabled.session.check_compaction_for_test(&msg_disabled, true).await;
    assert!(
        !disabled
            .events()
            .into_iter()
            .any(|e| matches!(e, AgentSessionEvent::CompactionStart { .. }))
    );

    below_threshold.cleanup();
    disabled.cleanup();
}
