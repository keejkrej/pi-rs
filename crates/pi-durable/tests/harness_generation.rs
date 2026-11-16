//! Port of packages/durable/test/harness-generation.test.ts

mod support;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::auth::types::ProviderAuth;
use pi_ai::models::Provider;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message,
};
use pi_ai::types::{
    ApiStreamOptions, AssistantMessage, AssistantMessageEvent, CancelDeferredFunction, DeferredCancelOptions,
    DeferredHandle, Message, Model, SimpleStreamFunction, SimpleStreamOptions, StopReason, ThinkingLevel,
    TranscriptContext,
};
use pi_ai::utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use pi_durable::documents::{DocDefinitionInput, define_conversation_doc};
use pi_durable::entries::{ASSISTANT_ENTRY, USER_ENTRY};
use pi_durable::errors::StorageRejected;
use pi_durable::harness::agent::{DEFAULT_COMPACTION_POLICY, DEFAULT_RETRY_POLICY, resolve_settings};
use pi_durable::harness::define::{SectionOptions, define_extension, wrap_section};
use pi_durable::harness::generation::{GENERATION_TASK, GenerationInput, GenerationResult};
use pi_durable::harness::harness::{Conversation, Harness, RootConversationOptions};
use pi_durable::harness::live::{LIVE_DOC, LiveRun, LiveState};
use pi_durable::harness::registry::{Registry, create_registry};
use pi_durable::harness::scheduler::AbortTaskResult;
use pi_durable::harness::types::{
    AgentChange, ConversationCreateOptions, Extension, FieldChange, GenerationHooks, HarnessOptions, HarnessSettings,
    HarnessSettingsSnapshot, ModelRef, PartialCompactionPolicy, PartialRetryPolicy, PromptSection, QueueMode,
    RegistryReader, RegistrySnapshot, SubmissionDraft, ToolExecutionMode, UserInput, WhenBusy,
};
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::tasks::{Task, TaskDefinition, define_task};
use pi_durable::types::{
    ConversationOwnership, DocumentScope, InputSubmissionState, LatestFork, NextTaskState, Storage, SubmissionCreate,
    SubmissionSettlement, SubmissionState, SubmissionStatus, TaskId, TaskOptions, TaskOutcome, TaskOwnership,
    TaskQuery, TaskState, TypedEntryDraft,
};
use pi_js::BoxFuture;
use pretty_assertions::assert_eq;
use serde_json::json;
use support::chat_support::{OpenChatEnv, all_entries, chat_setup, open_chat, text_of, unanswered, wait_for};
use support::harness_support::add_section;
use support::session_support::{ControlledStorage, context, document_changes};

fn error_503() -> AssistantMessage {
    faux_assistant_message(
        FauxAssistantMessageContent::Text(String::new()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("503 Service Unavailable".to_owned()),
            ..Default::default()
        }),
    )
}

async fn live_state(harness: &Harness, conversation: &Conversation) -> Option<LiveState> {
    harness
        .snapshot_conversation(&*LIVE_DOC, conversation.id(), context())
        .await
        .expect("snapshot live")
}

async fn run_task(harness: &Harness, conversation: &Conversation) -> TaskId {
    let mut found = None;
    wait_for(
        || {
            let harness = harness.clone();
            let id = conversation.id();
            async move {
                harness
                    .snapshot_conversation(&*LIVE_DOC, id, context())
                    .await
                    .expect("live")
                    .and_then(|s| s.run)
                    .is_some()
            }
        },
        5_000,
    )
    .await;
    if let Some(live) = live_state(harness, conversation).await
        && let Some(run) = live.run
    {
        found = Some(run.task_id);
    }
    found.expect("run taskId")
}

struct ProxyProvider {
    inner: Arc<dyn Provider + Send + Sync>,
    stream_simple_override: Option<SimpleStreamFunction>,
    cancel_deferred_override: Option<CancelDeferredFunction>,
}

impl Provider for ProxyProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn base_url(&self) -> Option<&str> {
        self.inner.base_url()
    }

    fn headers(&self) -> Option<&pi_ai::types::ProviderHeaders> {
        self.inner.headers()
    }

    fn auth(&self) -> &ProviderAuth {
        self.inner.auth()
    }

    fn get_models(&self) -> Vec<Model> {
        self.inner.get_models()
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.inner.stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        if let Some(override_fn) = &self.stream_simple_override {
            return override_fn(model, context, options);
        }
        self.inner.stream_simple(model, context, options)
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<pi_ai::types::DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        self.inner.fetch_deferred(model, handle, options)
    }

    fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredCancelOptions>,
    ) -> Option<BoxFuture<pi_js::Result<()>>> {
        if let Some(override_fn) = &self.cancel_deferred_override {
            return Some(override_fn(model, handle, options));
        }
        self.inner.cancel_deferred(model, handle, options)
    }
}

fn live_publications(harness: &Harness) -> (Arc<Mutex<Vec<LiveState>>>, pi_js::Unsubscribe) {
    let values = Arc::new(Mutex::new(Vec::<LiveState>::new()));
    let values_cb = Arc::clone(&values);
    let sub = harness
        .subscribe_commits(move |publication, _| {
            for change in document_changes(publication) {
                if change.record.kind == "pi.live"
                    && let Some(val) = change.value
                    && let Ok(parsed) = serde_json::from_value::<LiveState>(pi_ai::JsonValue::Object(val))
                {
                    values_cb.lock().expect("lock values").push(parsed);
                }
            }
        })
        .expect("subscribe commits");
    (values, sub)
}

#[tokio::test]
async fn answers_an_input_and_settles_its_submission() {
    let setup = chat_setup(None);
    add_section(
        &setup.registry,
        "preamble",
        Arc::new(|_, _| Box::pin(async { Ok(Some("You are helpful.".to_owned())) })),
        Some(SectionOptions { tag: Some(false) }),
        None,
    )
    .expect("add preamble");
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Hello there".to_owned()),
            None,
        ))]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    opened.harness.resume().expect("resume");
    let submission = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    let settled = submission.wait(context()).await.expect("wait");
    let SubmissionState::Input(InputSubmissionState::Done {
        entry: sub_entry,
        answer,
    }) = settled.record.state
    else {
        panic!("Unexpected submission state: {:?}", settled.record.state);
    };

    let answer_entry = opened
        .root
        .commit(
            move |tx| async move { tx.typed_entry(&ASSISTANT_ENTRY, answer).await },
            context(),
        )
        .await
        .expect("read answer")
        .expect("answer present");
    assert_eq!(
        answer_entry
            .record
            .model
            .as_ref()
            .and_then(|m| m.first())
            .and_then(text_of),
        Some("Hello there")
    );
    let entries = all_entries(&opened.root, None).await.expect("entries");
    let kinds: Vec<&str> = entries.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, vec!["pi.user", "pi.system", "pi.assistant"]);
    assert_eq!(entries[0].id, sub_entry);
    let sys = match entries[1].model.as_ref().and_then(|m| m.first()) {
        Some(Message::System(s)) => s,
        other => panic!("Expected system message, got {other:?}"),
    };
    assert_eq!(
        sys.sections.as_ref().and_then(|s| s.get("preamble")).cloned(),
        Some(Some("You are helpful.".to_owned()))
    );
    assert_eq!(
        live_state(&opened.harness, &opened.root).await,
        Some(LiveState::default())
    );

    let root_id = opened.root.id();
    let task = opened
        .harness
        .session()
        .commit(
            move |tx| async move {
                tx.scan_tasks(
                    TaskQuery {
                        conversation_id: Some(root_id),
                        ..Default::default()
                    },
                    10,
                    None,
                )
                .await
            },
            context(),
        )
        .await
        .expect("scan tasks")
        .items
        .into_iter()
        .next()
        .expect("task");
    assert_eq!(task.kind, "pi.generation");
    // Entries written by the generation are attributed to it; the admitted user entry is not task work.
    let by_tasks: Vec<Option<TaskId>> = entries.iter().map(|e| e.by_task_id).collect();
    assert_eq!(by_tasks, vec![None, Some(task.id), Some(task.id)]);
    assert_eq!(
        task.state,
        TaskState::Terminal {
            outcome: TaskOutcome::Completed {
                result: json!({ "entryId": answer.as_i64() }),
            },
        }
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn stores_partials_as_deltas_and_a_complete_base_once_nothing_is_in_flight() {
    let setup = chat_setup(Some(pi_ai::providers::faux::RegisterFauxProviderOptions {
        tokens_per_second: Some(200.0),
        token_size: Some(pi_ai::providers::faux::FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    }));
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("w".repeat(200)),
            None,
        ))]);
    let storage = Arc::new(ControlledStorage::new());
    let opened = open_chat(Arc::clone(&storage) as _, &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    let record = storage
        .find_document(
            pi_durable::types::DocumentAddress {
                scope: DocumentScope::Conversation {
                    conversation_id: opened.root.id(),
                },
                kind: "pi.live".to_owned(),
                key: None,
            },
            pi_durable::types::DocumentPoint::Current,
            context(),
        )
        .await
        .expect("find doc")
        .expect("present");
    opened.harness.resume().expect("resume");
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");

    let contents: Vec<&'static str> = storage
        .commits_snapshot()
        .into_iter()
        .flatten()
        .filter_map(|write| match write {
            pi_durable::types::StorageWrite::DocumentChange { id, content } if id == record.id => Some(match content {
                pi_durable::types::DocumentContent::Base { .. } => "base",
                pi_durable::types::DocumentContent::Delta { .. } => "delta",
            }),
            _ => None,
        })
        .collect();
    // Streaming writes deltas; the commit that settles the answer clears generation and writes a base.
    assert!(contents.contains(&"delta"));
    assert_eq!(contents.last().copied(), Some("base"));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn still_ends_a_run_whose_input_something_else_already_settled() {
    let setup = chat_setup(None);
    let busy = unanswered();
    setup.faux.set_responses(vec![busy.step]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    let submission = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    busy.reached.wait().await.expect("reached");
    let sub_id = submission.id();
    opened
        .root
        .commit(
            move |tx| async move {
                tx.settle_submission(
                    sub_id,
                    SubmissionSettlement::Unanswered {
                        reason: "withdrawn".to_owned(),
                        detail: None,
                    },
                )
            },
            context(),
        )
        .await
        .expect("settle submission");
    let task_id = run_task(&opened.harness, &opened.root).await;
    opened.harness.abort_task(task_id, context()).await.expect("abort");
    let settled_task = opened
        .harness
        .wait_for_task(TaskId::<GenerationResult>::new_unchecked(task_id.as_i64()), context())
        .await
        .expect("wait_for_task");
    assert_eq!(
        settled_task.outcome,
        TaskOutcome::Aborted {
            reason: None,
            result: None,
        }
    );
    // The earlier settlement stays; the run's own settlement leaves it unchanged.
    let status = submission.status(context()).await.expect("status");
    assert!(matches!(
        status.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) if reason == "withdrawn"
    ));
    assert_eq!(
        live_state(&opened.harness, &opened.root).await,
        Some(LiveState::default())
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn fails_with_no_model_when_no_model_is_configured_or_the_model_is_unknown() {
    let setup = chat_setup(None);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    let plain = opened
        .harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("plain");
    opened.harness.resume().expect("resume");
    let unset = plain
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    assert!(matches!(
        unset.record.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) if reason == "no_model"
    ));

    opened
        .root
        .configure(
            AgentChange {
                model: FieldChange::Set(ModelRef {
                    provider: "faux".to_owned(),
                    model_id: "missing".to_owned(),
                }),
                ..Default::default()
            },
            context(),
        )
        .await
        .expect("configure");
    let unknown = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    match unknown.record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, entry, .. }) => {
            assert_eq!(reason, "no_model");
            assert!(entry.is_some());
        }
        other => panic!("Unexpected {other:?}"),
    }
    let root_entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(
        root_entries.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        vec!["pi.user"]
    );
    let root_id = opened.root.id();
    let tasks = opened
        .harness
        .session()
        .commit(
            move |tx| async move {
                tx.scan_tasks(
                    TaskQuery {
                        conversation_id: Some(root_id),
                        ..Default::default()
                    },
                    10,
                    None,
                )
                .await
            },
            context(),
        )
        .await
        .expect("tasks");
    assert_eq!(
        tasks.items[0].state,
        TaskState::Terminal {
            outcome: TaskOutcome::Failed {
                error: pi_durable::types::TaskOutcomeError {
                    message: "Model faux/missing is not available".to_owned(),
                    detail: Some(json!({ "reason": "no_model" })),
                },
                result: None,
            },
        }
    );
    assert_eq!(
        live_state(&opened.harness, &opened.root).await,
        Some(LiveState::default())
    );
    assert_eq!(live_state(&opened.harness, &plain).await, Some(LiveState::default()));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn retries_a_retryable_error_after_a_durable_backoff_and_then_answers() {
    let setup = chat_setup(None);
    add_section(
        &setup.registry,
        "preamble",
        Arc::new(|_, _| Box::pin(async { Ok(Some("p".to_owned())) })),
        Some(SectionOptions { tag: Some(false) }),
        None,
    )
    .expect("add preamble");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(error_503()),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("recovered".to_owned()),
            None,
        )),
    ]);
    setup.settings.update(|s| {
        s.retry = Some(PartialRetryPolicy {
            enabled: Some(true),
            max_retries: Some(3),
            base_delay_ms: Some(1),
            max_agent_delay_ms: None,
        });
    });
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    let (values, _sub) = live_publications(&opened.harness);
    opened.harness.resume().expect("resume");
    let settled = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    assert_eq!(settled.record.status(), SubmissionStatus::Done);
    let entries = all_entries(&opened.root, None).await.expect("entries");
    let kinds: Vec<&str> = entries.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, vec!["pi.user", "pi.system", "pi.assistant", "pi.assistant"]);
    let Message::Assistant(failed_msg) = &entries[2].model.as_ref().expect("model")[0] else {
        panic!("expected assistant");
    };
    assert_eq!(failed_msg.stop_reason, StopReason::Error);
    let pubs = values.lock().expect("lock values");
    assert!(pubs.iter().any(|v| {
        v.generation
            .as_ref()
            .and_then(|g| g.retry.as_ref())
            .is_some_and(|r| r.error == "503 Service Unavailable")
    }));
    assert!(
        pubs.iter()
            .any(|v| v.generation.as_ref().is_some_and(|g| g.attempt == 2))
    );
    drop(pubs);
    assert_eq!(
        live_state(&opened.harness, &opened.root).await,
        Some(LiveState::default())
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn fails_with_model_error_once_retries_are_exhausted() {
    let setup = chat_setup(None);
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(error_503()),
        FauxResponseStep::Message(error_503()),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("never".to_owned()),
            None,
        )),
    ]);
    setup.settings.update(|s| {
        s.retry = Some(PartialRetryPolicy {
            enabled: Some(true),
            max_retries: Some(1),
            base_delay_ms: Some(1),
            max_agent_delay_ms: None,
        });
    });
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    opened.harness.resume().expect("resume");
    let settled = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    match settled.record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, detail, .. }) => {
            assert_eq!(reason, "model_error");
            assert_eq!(
                detail.as_ref().and_then(|v| v.as_str()),
                Some("503 Service Unavailable")
            );
        }
        other => panic!("Unexpected {other:?}"),
    }
    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(
        entries.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        vec!["pi.user", "pi.assistant", "pi.assistant"]
    );
    assert_eq!(setup.faux.get_pending_response_count(), 1);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn fails_a_retryable_error_without_retrying_when_the_retry_policy_is_disabled() {
    let setup = chat_setup(None);
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(error_503()),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("never".to_owned()),
            None,
        )),
    ]);
    setup.settings.update(|s| {
        s.retry = Some(PartialRetryPolicy {
            enabled: Some(false),
            max_retries: None,
            base_delay_ms: None,
            max_agent_delay_ms: None,
        });
    });
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    opened.harness.resume().expect("resume");
    let settled = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    assert!(matches!(
        settled.record.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) if reason == "model_error"
    ));
    assert_eq!(setup.faux.state().call_count(), 1);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reports_section_wrapper_failures_while_preparing() {
    let setup = chat_setup(None);
    add_section(
        &setup.registry,
        "cwd",
        Arc::new(|_, _| Box::pin(async { Ok(Some("/repo".to_owned())) })),
        None,
        None,
    )
    .expect("add cwd");
    setup
        .registry
        .install(define_extension(Extension {
            name: "broken".to_owned(),
            wraps: vec![wrap_section("cwd", |inner| PromptSection {
                key: inner.key,
                tag: inner.tag,
                render: Arc::new(|_, _| Box::pin(async { Err(pi_js::Error::msg("wrapper failed")) })),
            })],
            ..Default::default()
        }))
        .expect("install broken");
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("ok".to_owned()),
            None,
        ))]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    opened.harness.resume().expect("resume");
    let settled = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    assert_eq!(settled.record.status(), SubmissionStatus::Done);
    let reports = setup.reports.lock().expect("lock reports");
    assert!(reports.iter().any(|e| e.to_string().contains("wrapper failed")));
    // The failed section is absent, so nothing was rendered.
    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(
        entries.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        vec!["pi.user", "pi.assistant"]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn fails_a_non_retryable_error_without_retrying() {
    let setup = chat_setup(None);
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text(String::new()),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("Invalid request".to_owned()),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("never".to_owned()),
            None,
        )),
    ]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    opened.harness.resume().expect("resume");
    let settled = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    match settled.record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, detail, .. }) => {
            assert_eq!(reason, "model_error");
            assert_eq!(detail.as_ref().and_then(|v| v.as_str()), Some("Invalid request"));
        }
        other => panic!("Unexpected {other:?}"),
    }
    let root_id = opened.root.id();
    let tasks = opened
        .harness
        .session()
        .commit(
            move |tx| async move {
                tx.scan_tasks(
                    TaskQuery {
                        conversation_id: Some(root_id),
                        ..Default::default()
                    },
                    10,
                    None,
                )
                .await
            },
            context(),
        )
        .await
        .expect("scan tasks");
    assert_eq!(
        tasks.items[0].state,
        TaskState::Terminal {
            outcome: TaskOutcome::Failed {
                error: pi_durable::types::TaskOutcomeError {
                    message: "Invalid request".to_owned(),
                    detail: Some(json!({ "reason": "model_error" })),
                },
                result: None,
            },
        }
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn polls_a_deferred_response_until_it_is_ready() {
    let setup = chat_setup(Some(pi_ai::providers::faux::RegisterFauxProviderOptions {
        deferred: Some(pi_ai::providers::faux::FauxDeferredOptions {
            pending_fetches: Some(1),
            poll_after_ms: Some(1),
        }),
        ..Default::default()
    }));
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("deferred answer".to_owned()),
            None,
        ))]);
    setup.settings.update(|s| {
        s.stream = Some(pi_durable::harness::types::ConversationStreamOptions {
            deferred: Some(pi_durable::harness::types::StreamDeferredOption::Bool(true)),
            ..Default::default()
        });
    });
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    let (values, _sub) = live_publications(&opened.harness);
    opened.harness.resume().expect("resume");
    let settled = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    let SubmissionState::Input(InputSubmissionState::Done { answer, .. }) = settled.record.state else {
        panic!("Unexpected state: {:?}", settled.record.state);
    };
    assert_eq!(setup.faux.state().deferred_fetch_count(), 2);
    let poll_times: Vec<i64> = values
        .lock()
        .expect("lock values")
        .iter()
        .filter_map(|v| v.generation.as_ref()?.deferred.as_ref().map(|d| d.poll_at))
        .collect();
    assert_eq!(poll_times.len(), 2);
    assert!(poll_times[1] > poll_times[0]);
    let entry = opened
        .root
        .commit(
            move |tx| async move { tx.typed_entry(&ASSISTANT_ENTRY, answer).await },
            context(),
        )
        .await
        .expect("read")
        .expect("present");
    assert_eq!(
        entry.record.model.as_ref().and_then(|m| m.first()).and_then(text_of),
        Some("deferred answer")
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn converts_the_committed_partial_when_aborted_during_streaming() {
    let setup = chat_setup(Some(pi_ai::providers::faux::RegisterFauxProviderOptions {
        tokens_per_second: Some(20.0),
        token_size: Some(pi_ai::providers::faux::FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    }));
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("x".repeat(400)),
            None,
        ))]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    opened.harness.resume().expect("resume");
    let submission = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    let task_id = run_task(&opened.harness, &opened.root).await;
    wait_for(
        || {
            let harness = opened.harness.clone();
            let root_id = opened.root.id();
            async move {
                harness
                    .snapshot_conversation(&*LIVE_DOC, root_id, context())
                    .await
                    .expect("live")
                    .and_then(|s| s.generation)
                    .and_then(|g| g.message)
                    .is_some_and(|m| text_of(&Message::Assistant(m)).is_some())
            }
        },
        5_000,
    )
    .await;
    let partial = live_state(&opened.harness, &opened.root)
        .await
        .expect("live")
        .generation
        .expect("gen")
        .message
        .expect("msg");
    let partial_msg = Message::Assistant(partial);
    let partial_text = text_of(&partial_msg).expect("partial text").to_owned();

    assert_eq!(
        opened.harness.abort_task(task_id, context()).await.expect("abort"),
        AbortTaskResult::Marked
    );
    let settled = submission.wait(context()).await.expect("wait");
    assert!(matches!(
        settled.record.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) if reason == "aborted"
    ));
    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(
        entries.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        vec!["pi.user", "pi.assistant"]
    );
    let Message::Assistant(converted) = &entries[1].model.as_ref().expect("model")[0] else {
        panic!("expected assistant");
    };
    assert_eq!(converted.stop_reason, StopReason::Aborted);
    let converted_msg = Message::Assistant(converted.clone());
    let converted_text = text_of(&converted_msg).expect("converted text");
    assert!(converted_text.starts_with(&partial_text));
    assert_eq!(
        live_state(&opened.harness, &opened.root).await,
        Some(LiveState::default())
    );
    let waited = opened
        .harness
        .wait_for_task(TaskId::<GenerationResult>::new_unchecked(task_id.as_i64()), context())
        .await
        .expect("wait_for_task");
    assert_eq!(
        waited.outcome,
        TaskOutcome::Aborted {
            reason: None,
            result: None,
        }
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn cancels_a_deferred_response_when_aborted_during_polling() {
    let setup = chat_setup(Some(pi_ai::providers::faux::RegisterFauxProviderOptions {
        deferred: Some(pi_ai::providers::faux::FauxDeferredOptions {
            pending_fetches: Some(100),
            poll_after_ms: Some(60_000),
        }),
        ..Default::default()
    }));
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("never".to_owned()),
            None,
        ))]);
    setup.settings.update(|s| {
        s.stream = Some(pi_durable::harness::types::ConversationStreamOptions {
            deferred: Some(pi_durable::harness::types::StreamDeferredOption::Bool(true)),
            ..Default::default()
        });
    });
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    opened.harness.resume().expect("resume");
    let submission = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    let task_id = run_task(&opened.harness, &opened.root).await;
    wait_for(
        || {
            let harness = opened.harness.clone();
            let root_id = opened.root.id();
            async move {
                harness
                    .snapshot_conversation(&*LIVE_DOC, root_id, context())
                    .await
                    .expect("live")
                    .and_then(|s| s.generation)
                    .is_some_and(|g| g.deferred.is_some())
            }
        },
        5_000,
    )
    .await;
    opened.harness.abort_task(task_id, context()).await.expect("abort");
    let settled = submission.wait(context()).await.expect("wait");
    assert!(matches!(
        settled.record.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) if reason == "aborted"
    ));
    assert_eq!(setup.faux.state().cancelled_deferred().len(), 1);
    assert_eq!(
        live_state(&opened.harness, &opened.root).await,
        Some(LiveState::default())
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reports_a_failed_deferred_cancellation_and_still_ends_the_run_aborted() {
    let mut setup = chat_setup(Some(pi_ai::providers::faux::RegisterFauxProviderOptions {
        deferred: Some(pi_ai::providers::faux::FauxDeferredOptions {
            pending_fetches: Some(100),
            poll_after_ms: Some(60_000),
        }),
        ..Default::default()
    }));
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("never".to_owned()),
            None,
        ))]);
    let proxy = Arc::new(ProxyProvider {
        inner: setup.faux.provider(),
        stream_simple_override: None,
        cancel_deferred_override: Some(Arc::new(|_, _, _| {
            Box::pin(async { Err(pi_js::Error::msg("cancel failed")) })
        })),
    });
    setup.models = pi_ai::models::create_models(None);
    setup.models.set_provider(proxy);
    setup.settings.update(|s| {
        s.stream = Some(pi_durable::harness::types::ConversationStreamOptions {
            deferred: Some(pi_durable::harness::types::StreamDeferredOption::Bool(true)),
            ..Default::default()
        });
    });
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    opened.harness.resume().expect("resume");
    let submission = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    let task_id = run_task(&opened.harness, &opened.root).await;
    wait_for(
        || {
            let harness = opened.harness.clone();
            let root_id = opened.root.id();
            async move {
                harness
                    .snapshot_conversation(&*LIVE_DOC, root_id, context())
                    .await
                    .expect("live")
                    .and_then(|s| s.generation)
                    .is_some_and(|g| g.deferred.is_some())
            }
        },
        5_000,
    )
    .await;
    opened.harness.abort_task(task_id, context()).await.expect("abort");
    let settled = submission.wait(context()).await.expect("wait");
    assert!(matches!(
        settled.record.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) if reason == "aborted"
    ));
    let reports = setup.reports.lock().expect("lock reports");
    assert!(reports.iter().any(|e| e.to_string().contains("cancel failed")));
    assert_eq!(
        live_state(&opened.harness, &opened.root).await,
        Some(LiveState::default())
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn forwards_stream_options_and_the_thinking_level() {
    let setup = chat_setup(None);
    let seen = Arc::new(Mutex::new(Vec::<Option<SimpleStreamOptions>>::new()));
    let seen_a = Arc::clone(&seen);
    let seen_b = Arc::clone(&seen);
    setup.faux.set_responses(vec![
        FauxResponseStep::Factory(Arc::new(move |_, opts, _, _| {
            seen_a.lock().expect("lock").push(opts);
            Box::pin(async {
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("a".to_owned()),
                    None,
                ))
            })
        })),
        FauxResponseStep::Factory(Arc::new(move |_, opts, _, _| {
            seen_b.lock().expect("lock").push(opts);
            Box::pin(async {
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("b".to_owned()),
                    None,
                ))
            })
        })),
    ]);
    let mut headers = IndexMap::new();
    headers.insert("x-test".to_owned(), "1".to_owned());
    setup.settings.update(|s| {
        s.stream = Some(pi_durable::harness::types::ConversationStreamOptions {
            timeout_ms: Some(1234),
            headers: Some(headers),
            ..Default::default()
        });
    });
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    opened
        .root
        .configure(
            AgentChange {
                thinking_level: FieldChange::Set(pi_ai::types::ModelThinkingLevel::High),
                ..Default::default()
            },
            context(),
        )
        .await
        .expect("configure high");
    opened.harness.resume().expect("resume");
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("one".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    // Both are read at the next preparation: the thinking level from pi.agent, the stream options live from settings.
    opened
        .root
        .configure(
            AgentChange {
                thinking_level: FieldChange::Clear,
                ..Default::default()
            },
            context(),
        )
        .await
        .expect("configure null");
    setup.settings.update(|s| {
        s.stream = Some(pi_durable::harness::types::ConversationStreamOptions {
            timeout_ms: Some(99),
            ..Default::default()
        });
    });
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("two".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");

    let recorded = seen.lock().expect("lock seen");
    let first = recorded[0].as_ref().expect("first opts");
    assert_eq!(first.base.timeout_ms, Some(1234));
    assert_eq!(
        first
            .base
            .headers
            .as_ref()
            .and_then(|h| h.get("x-test"))
            .and_then(|v| v.as_deref()),
        Some("1")
    );
    assert_eq!(first.reasoning, Some(ThinkingLevel::High));
    assert!(first.base.signal.is_some());

    let second = recorded[1].as_ref().expect("second opts");
    assert_eq!(second.reasoning, None);
    assert_eq!(second.base.timeout_ms, Some(99));
    assert!(second.base.headers.is_none());
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reads_settings_through_getters_at_every_decision() {
    struct DynamicSettings {
        timeout_ms: Arc<AtomicU64>,
    }
    impl HarnessSettings for DynamicSettings {
        fn get(&self) -> HarnessSettingsSnapshot {
            HarnessSettingsSnapshot {
                stream: Some(pi_durable::harness::types::ConversationStreamOptions {
                    timeout_ms: Some(self.timeout_ms.load(Ordering::Acquire)),
                    ..Default::default()
                }),
                retry: Some(PartialRetryPolicy {
                    base_delay_ms: Some(1),
                    ..Default::default()
                }),
                ..Default::default()
            }
        }
    }

    let setup = chat_setup(None);
    let timeout_ms = Arc::new(AtomicU64::new(111));
    let seen = Arc::new(Mutex::new(Vec::<Option<u64>>::new()));
    let timeout_cb = Arc::clone(&timeout_ms);
    let seen_a = Arc::clone(&seen);
    let seen_b = Arc::clone(&seen);
    setup.faux.set_responses(vec![
        FauxResponseStep::Factory(Arc::new(move |_, opts, _, _| {
            seen_a.lock().expect("lock").push(opts.and_then(|o| o.base.timeout_ms));
            // The user changes the setting while the first attempt runs.
            timeout_cb.store(222, Ordering::Release);
            Box::pin(async { Ok(error_503()) })
        })),
        FauxResponseStep::Factory(Arc::new(move |_, opts, _, _| {
            seen_b.lock().expect("lock").push(opts.and_then(|o| o.base.timeout_ms));
            Box::pin(async {
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("ok".to_owned()),
                    None,
                ))
            })
        })),
    ]);
    let settings: Arc<dyn HarnessSettings> = Arc::new(DynamicSettings { timeout_ms });
    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            models: setup.models.clone(),
            registry: setup.registry.clone(),
            settings: Some(settings),
            env: None,
            conversation_created: None,
            now: None,
            on_report: None,
        },
        context(),
    )
    .await
    .expect("open harness");
    let root = harness
        .root(
            context(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    model: FieldChange::Set(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "faux-1".to_owned(),
                    }),
                    ..Default::default()
                }),
                init: None,
            }),
        )
        .await
        .expect("root");
    harness.resume().expect("resume");
    let settled = root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    assert_eq!(settled.record.status(), SubmissionStatus::Done);
    // The retry prepares again, so it resolves the settings again and sends the new timeout.
    assert_eq!(*seen.lock().expect("lock"), vec![Some(111), Some(222)]);
    harness.close(context()).await.expect("close");
}

#[test]
fn resolves_settings_over_the_built_in_defaults() {
    let resolved = resolve_settings(None);
    assert!(resolved.extensions.is_none());
    assert_eq!(resolved.stream, Default::default());
    assert_eq!(resolved.retry, DEFAULT_RETRY_POLICY);
    assert_eq!(resolved.compaction, DEFAULT_COMPACTION_POLICY);
    assert_eq!(resolved.tool_execution, ToolExecutionMode::Parallel);
    assert_eq!(resolved.steering_mode, QueueMode::OneAtATime);
    assert_eq!(resolved.follow_up_mode, QueueMode::OneAtATime);
    let partial = HarnessSettingsSnapshot {
        retry: Some(PartialRetryPolicy {
            enabled: Some(false),
            ..Default::default()
        }),
        compaction: Some(PartialCompactionPolicy {
            background_tokens: Some(0),
            ..Default::default()
        }),
        ..Default::default()
    };
    let resolved = resolve_settings(Some(&partial));
    assert!(!resolved.retry.enabled);
    assert_eq!(resolved.retry.max_retries, 3);
    assert_eq!(resolved.retry.base_delay_ms, 2000);
    assert!(resolved.compaction.enabled);
    assert_eq!(resolved.compaction.background_tokens, 0);
}

#[tokio::test]
async fn renders_sections_that_read_conversation_documents_through_input_read() {
    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    struct TestAgentDoc {
        cwd: String,
        kind: String,
    }
    let agent_doc = Arc::new(
        define_conversation_doc(
            LatestFork::Current,
            DocDefinitionInput {
                kind: "test.agent",
                version: 1,
                initial: || TestAgentDoc {
                    cwd: "/".to_owned(),
                    kind: "main".to_owned(),
                },
                migrate: None,
                checkpoint_when: None,
            },
        )
        .expect("define doc"),
    );

    let setup = chat_setup(None);
    let doc_cwd = Arc::clone(&agent_doc);
    add_section(
        &setup.registry,
        "cwd",
        Arc::new(move |input, ctx| {
            let doc_cwd = Arc::clone(&doc_cwd);
            Box::pin(async move {
                let snap = input
                    .read
                    .snapshot_conversation(&*doc_cwd, input.conversation_id, ctx)
                    .await?;
                Ok(snap.map(|d| d.cwd))
            })
        }),
        None,
        None,
    )
    .expect("add cwd");
    let doc_agents = Arc::clone(&agent_doc);
    add_section(
        &setup.registry,
        "agents",
        Arc::new(move |input, ctx| {
            let doc_agents = Arc::clone(&doc_agents);
            Box::pin(async move {
                let snap = input
                    .read
                    .snapshot_conversation(&*doc_agents, input.conversation_id, ctx)
                    .await?;
                Ok(if snap.is_some_and(|d| d.kind == "sub") {
                    None
                } else {
                    Some("Read AGENTS.md".to_owned())
                })
            })
        }),
        None,
        None,
    )
    .expect("add agents");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("a".to_owned()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("b".to_owned()),
            None,
        )),
    ]);

    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    let root_id = opened.root.id();
    let doc_root = Arc::clone(&agent_doc);
    opened
        .root
        .commit(
            move |tx| async move {
                let draft = tx.doc_conversation(&*doc_root, root_id).await?;
                draft.update(|d| d.cwd = "/repo".to_owned())
            },
            context(),
        )
        .await
        .expect("set root cwd");
    let doc_sub = Arc::clone(&agent_doc);
    let sub = opened
        .harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: Some(AgentChange {
                    model: FieldChange::Set(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "faux-1".to_owned(),
                    }),
                    ..Default::default()
                }),
                init: Some(Arc::new(move |tx, id| {
                    let doc_sub = Arc::clone(&doc_sub);
                    Box::pin(async move {
                        let draft = tx.doc_conversation(&*doc_sub, id).await?;
                        draft.update(|d| {
                            d.kind = "sub".to_owned();
                            d.cwd = "/sub".to_owned();
                        })
                    })
                })),
            },
            context(),
        )
        .await
        .expect("sub");

    opened.harness.resume().expect("resume");
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("one".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    sub.submit(
        SubmissionDraft::Input {
            request_id: None,
            content: UserInput::Text("two".to_owned()),
            when_busy: None,
        },
        context(),
    )
    .await
    .expect("submit")
    .wait(context())
    .await
    .expect("wait");

    let root_sys = match all_entries(&opened.root, None)
        .await
        .expect("entries")
        .into_iter()
        .find(|e| e.kind == "pi.system")
        .and_then(|e| e.model)
        .and_then(|mut m| m.drain(..1).next())
    {
        Some(Message::System(s)) => s,
        other => panic!("Expected root system entry, got {other:?}"),
    };
    let root_sections = root_sys.sections.expect("root sections");
    assert_eq!(root_sections.get("cwd"), Some(&Some("<cwd>\n/repo\n</cwd>".to_owned())));
    assert_eq!(
        root_sections.get("agents"),
        Some(&Some("<agents>\nRead AGENTS.md\n</agents>".to_owned()))
    );

    let sub_sys = match all_entries(&sub, None)
        .await
        .expect("entries")
        .into_iter()
        .find(|e| e.kind == "pi.system")
        .and_then(|e| e.model)
        .and_then(|mut m| m.drain(..1).next())
    {
        Some(Message::System(s)) => s,
        other => panic!("Expected sub system entry, got {other:?}"),
    };
    let sub_sections = sub_sys.sections.expect("sub sections");
    assert_eq!(sub_sections.len(), 1);
    assert_eq!(sub_sections.get("cwd"), Some(&Some("<cwd>\n/sub\n</cwd>".to_owned())));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn commits_no_partial_for_a_response_that_turns_deferred_after_an_empty_start_event() {
    let mut setup = chat_setup(None);
    let proxy = Arc::new(ProxyProvider {
        inner: setup.faux.provider(),
        stream_simple_override: Some(Arc::new(|_, _, _| {
            let stream = create_assistant_message_event_stream();
            let stream_bg = stream.clone();
            tokio::spawn(async move {
                let empty_partial = faux_assistant_message(
                    FauxAssistantMessageContent::Blocks(Vec::new()),
                    Some(FauxAssistantMessageOptions {
                        stop_reason: Some(StopReason::Pending),
                        ..Default::default()
                    }),
                );
                stream_bg.push(AssistantMessageEvent::Start { partial: empty_partial });
                // Longer than the partial throttle: an empty partial would be committed here.
                let _ = pi_js::time::sleep(300, None).await;
                let mut final_msg = faux_assistant_message(
                    FauxAssistantMessageContent::Blocks(Vec::new()),
                    Some(FauxAssistantMessageOptions {
                        stop_reason: Some(StopReason::Deferred),
                        ..Default::default()
                    }),
                );
                final_msg.deferred = Some(DeferredHandle {
                    provider: "faux".to_owned(),
                    model_id: "faux-1".to_owned(),
                    api: "faux".to_owned(),
                    id: "handle-1".to_owned(),
                    expires_at: None,
                    poll_after_ms: Some(60_000),
                    data: None,
                });
                stream_bg.push(AssistantMessageEvent::Done {
                    reason: StopReason::Deferred,
                    message: final_msg,
                });
            });
            stream
        })),
        cancel_deferred_override: None,
    });
    setup.models = pi_ai::models::create_models(None);
    setup.models.set_provider(proxy);

    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    let (values, _sub) = live_publications(&opened.harness);
    let submission = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    wait_for(
        || {
            let harness = opened.harness.clone();
            let root_id = opened.root.id();
            async move {
                harness
                    .snapshot_conversation(&*LIVE_DOC, root_id, context())
                    .await
                    .expect("live")
                    .and_then(|s| s.generation)
                    .is_some_and(|g| g.deferred.is_some())
            }
        },
        5_000,
    )
    .await;
    assert!(
        !values
            .lock()
            .expect("lock")
            .iter()
            .any(|v| { v.generation.as_ref().and_then(|g| g.message.as_ref()).is_some() })
    );
    let task_id = live_state(&opened.harness, &opened.root)
        .await
        .expect("live")
        .run
        .expect("run")
        .task_id;
    opened.harness.abort_task(task_id, context()).await.expect("abort");
    let settled = submission.wait(context()).await.expect("wait");
    assert!(matches!(
        settled.record.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) if reason == "aborted"
    ));
    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(
        entries.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        vec!["pi.user"]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn faults_a_run_task_settling_its_inputs_and_converting_the_committed_partial() {
    let storage = Arc::new(ControlledStorage::new());
    let partial_committed = Arc::new(tokio::sync::Notify::new());
    let notified_once = Arc::new(AtomicBool::new(false));
    let storage_cb = Arc::clone(&storage);
    let committed_cb = Arc::clone(&partial_committed);
    let committed_flag = Arc::clone(&notified_once);

    let mut setup = chat_setup(None);
    let proxy = Arc::new(ProxyProvider {
        inner: setup.faux.provider(),
        stream_simple_override: Some(Arc::new(move |_, _, _| {
            let stream = create_assistant_message_event_stream();
            let stream_bg = stream.clone();
            let storage_cb = Arc::clone(&storage_cb);
            let committed_cb = Arc::clone(&committed_cb);
            let committed_flag = Arc::clone(&committed_flag);
            tokio::spawn(async move {
                let partial = faux_assistant_message(
                    FauxAssistantMessageContent::Text("partial".to_owned()),
                    Some(FauxAssistantMessageOptions {
                        stop_reason: Some(StopReason::Pending),
                        ..Default::default()
                    }),
                );
                stream_bg.push(AssistantMessageEvent::Start { partial });
                // Wait until the partial reaches pi.live, then reject the final classification commit.
                let _ = pi_js::time::sleep(300, None).await;
                loop {
                    let notified = committed_cb.notified();
                    if committed_flag.load(Ordering::Acquire) {
                        break;
                    }
                    notified.await;
                }
                storage_cb.fail_next_commit(StorageRejected::new("non-JSON function").into_error());
                let final_msg = faux_assistant_message(FauxAssistantMessageContent::Text("final".to_owned()), None);
                stream_bg.push(AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: final_msg,
                });
            });
            stream
        })),
        cancel_deferred_override: None,
    });
    setup.models = pi_ai::models::create_models(None);
    setup.models.set_provider(proxy);

    let opened = open_chat(Arc::clone(&storage) as _, &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    let values = Arc::new(Mutex::new(Vec::<LiveState>::new()));
    let values_cb = Arc::clone(&values);
    let notify_cb = Arc::clone(&partial_committed);
    let _sub = opened
        .harness
        .subscribe_commits(move |publication, _| {
            for change in document_changes(publication) {
                if change.record.kind == "pi.live"
                    && let Some(val) = change.value
                    && let Ok(parsed) = serde_json::from_value::<LiveState>(pi_ai::JsonValue::Object(val))
                {
                    if let Some(msg) = parsed.generation.as_ref().and_then(|g| g.message.as_ref())
                        && text_of(&Message::Assistant(msg.clone())).as_deref() == Some("partial")
                        && !notified_once.swap(true, Ordering::AcqRel)
                    {
                        notify_cb.notify_waiters();
                    }
                    values_cb.lock().expect("lock values").push(parsed);
                }
            }
        })
        .expect("subscribe");
    opened.harness.resume().expect("resume");
    let settled = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    match settled.record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, detail, .. }) => {
            assert_eq!(reason, "faulted");
            assert!(
                detail
                    .as_ref()
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| s.contains("non-JSON function"))
            );
        }
        other => panic!("Expected Faulted, got {other:?}"),
    }
    assert!(values.lock().expect("lock").iter().any(|v| {
        v.generation
            .as_ref()
            .and_then(|g| g.message.as_ref())
            .is_some_and(|m| text_of(&Message::Assistant(m.clone())) == Some("partial"))
    }));
    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(
        entries.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        vec!["pi.user", "pi.assistant"]
    );
    let Message::Assistant(converted) = &entries[1].model.as_ref().expect("model")[0] else {
        panic!("expected assistant");
    };
    assert_eq!(converted.stop_reason, StopReason::Aborted);
    assert_eq!(
        text_of(&Message::Assistant(converted.clone())).as_deref(),
        Some("partial")
    );
    assert_eq!(
        live_state(&opened.harness, &opened.root).await,
        Some(LiveState::default())
    );
    let root_id = opened.root.id();
    let tasks = opened
        .harness
        .session()
        .commit(
            move |tx| async move {
                tx.scan_tasks(
                    TaskQuery {
                        conversation_id: Some(root_id),
                        ..Default::default()
                    },
                    10,
                    None,
                )
                .await
            },
            context(),
        )
        .await
        .expect("scan tasks");
    assert!(matches!(
        tasks.items[0].state,
        TaskState::Terminal {
            outcome: TaskOutcome::Faulted { .. },
        }
    ));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn orphans_a_blocked_run_task_with_full_run_cleanup() {
    let setup = chat_setup(None);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    // A run whose task was stored by a newer generation definition this process cannot run.
    let newer: Task<GenerationInput, serde_json::Value, GenerationResult, GenerationHooks> =
        define_task(TaskDefinition {
            name: GENERATION_TASK.name().to_owned(),
            version: 2,
            initial: Arc::new(|_| Ok(json!({ "phase": "prepare", "attempt": 1 }))),
            phases: IndexMap::new(),
            abort: Arc::new(|_, runtime, ctx| {
                Box::pin(async move {
                    runtime
                        .commit(
                            |_tx, _current| {
                                Box::pin(async {
                                    Ok(Some(NextTaskState::Terminal {
                                        outcome: TaskOutcome::Aborted {
                                            reason: None,
                                            result: None,
                                        },
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            }),
            migrate: None,
            _hooks: std::marker::PhantomData,
        });
    let root_id = opened.root.id();
    let (task_id, submission_id) = opened
        .harness
        .session()
        .commit(
            move |tx| async move {
                let entry = tx
                    .append_typed_entry(
                        &USER_ENTRY,
                        root_id,
                        TypedEntryDraft {
                            model: Some(vec![Message::User(pi_ai::types::UserMessage {
                                content: pi_ai::types::UserMessageContent::Text("hi".to_owned()),
                                timestamp: 1,
                            })]),
                            data: (),
                            head: None,
                            edits: None,
                        },
                    )
                    .await?;
                let submission = tx
                    .create_submission(SubmissionCreate {
                        conversation_id: root_id,
                        request_id: None,
                        state: SubmissionState::Input(InputSubmissionState::Placed { entry: entry.record.id }),
                    })
                    .await?;
                let task_id = tx
                    .create_task(
                        &newer,
                        &GenerationInput {},
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(root_id),
                            background: false,
                        },
                    )
                    .await?;
                let live_draft = tx.doc_conversation(&*LIVE_DOC, root_id).await?;
                live_draft.update(|live| {
                    live.run = Some(LiveRun {
                        task_id: task_id.erased(),
                        inputs: vec![submission.id],
                    });
                })?;
                Ok((task_id, submission.id))
            },
            context(),
        )
        .await
        .expect("setup blocked run");
    opened.harness.resume().expect("resume");
    let err = match opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("busy".to_owned()),
                when_busy: Some(WhenBusy::Reject),
            },
            context(),
        )
        .await
    {
        Ok(_) => panic!("should reject"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("is busy"));
    assert_eq!(
        opened
            .harness
            .abort_task(task_id.erased(), context())
            .await
            .expect("abort"),
        AbortTaskResult::Marked
    );
    let settled_task = opened
        .harness
        .wait_for_task(task_id, context())
        .await
        .expect("wait_for_task");
    assert_eq!(
        settled_task.outcome,
        TaskOutcome::Orphaned {
            reason: "task_too_old".to_owned(),
        }
    );
    let sub_status = opened
        .harness
        .submission(submission_id, context())
        .await
        .expect("sub")
        .expect("present")
        .status(context())
        .await
        .expect("status");
    assert!(matches!(
        sub_status.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) if reason == "task_too_old"
    ));
    assert_eq!(
        live_state(&opened.harness, &opened.root).await,
        Some(LiveState::default())
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn rejects_a_registry_without_the_built_in_tasks() {
    struct MissingGenSnapshot {
        inner: Arc<dyn RegistrySnapshot>,
        filtered_tasks: Vec<pi_durable::tasks::AnyTask>,
    }
    impl RegistrySnapshot for MissingGenSnapshot {
        fn installed(&self) -> &[Extension] {
            self.inner.installed()
        }
        fn extension(&self, name: &str) -> Option<&Extension> {
            self.inner.extension(name)
        }
        fn tools(&self) -> Vec<pi_durable::harness::types::ExtensionTool> {
            self.inner.tools()
        }
        fn sections(&self) -> Vec<pi_durable::harness::types::ExtensionSection> {
            self.inner.sections()
        }
        fn tasks(&self) -> Vec<pi_durable::tasks::AnyTask> {
            self.filtered_tasks.clone()
        }
        fn task(&self, name: &str) -> Option<&pi_durable::tasks::AnyTask> {
            if name == "pi.generation" {
                None
            } else {
                self.inner.task(name)
            }
        }
    }
    struct MissingGenReader {
        snapshot: Arc<MissingGenSnapshot>,
    }
    impl RegistryReader for MissingGenReader {
        fn snapshot(&self) -> Arc<dyn RegistrySnapshot> {
            Arc::clone(&self.snapshot) as _
        }
        fn subscribe(&self, _listener: Arc<dyn Fn() + Send + Sync>) -> pi_js::Unsubscribe {
            pi_js::Unsubscribe::noop()
        }
    }

    let inner = create_registry().snapshot();
    let filtered_tasks = inner
        .tasks()
        .iter()
        .filter(|t| t.name() != "pi.generation")
        .cloned()
        .collect();
    let reader: Arc<dyn RegistryReader> = Arc::new(MissingGenReader {
        snapshot: Arc::new(MissingGenSnapshot { inner, filtered_tasks }),
    });
    let err = match Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            models: chat_setup(None).models,
            registry: reader,
            settings: None,
            env: None,
            conversation_created: None,
            now: None,
            on_report: None,
        },
        context(),
    )
    .await
    {
        Ok(_) => panic!("expected error"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("Registry lacks built-in tasks pi.generation"));
}
