//! Port of packages/durable/test/harness-generation-recovery.test.ts

mod support;

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use pi_ai::models::create_models;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxDeferredOptions, FauxResponseStep, FauxTokenSize,
    RegisterFauxProviderOptions, faux_assistant_message,
};
use pi_ai::types::{Message, ModelThinkingLevel, StopReason};
use pi_durable::entries::ASSISTANT_ENTRY;
use pi_durable::harness::define::SectionOptions;
use pi_durable::harness::generation::{GenerationCheckpoint, GenerationResult};
use pi_durable::harness::harness::Harness;
use pi_durable::harness::live::{DurableRetryState, LIVE_DOC, LiveGeneration, LiveRun, LiveState};
use pi_durable::harness::types::{
    ConversationStreamOptions, PartialRetryPolicy, StreamDeferredOption, SubmissionDraft, UserInput,
};
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::types::{ConversationId, InputSubmissionState, SubmissionState, SubmissionStatus, TaskId, TaskState};
use pretty_assertions::assert_eq;
use support::chat_support::{
    ChatSetup, OpenChatEnv, OpenedChat, all_entries, chat_setup, open_chat, text_of, unanswered, wait_for,
};
use support::harness_support::add_section;
use support::session_support::context;
use support::task_support::{aborted, deferred};
use tempfile::TempDir;

async fn sqlite_path() -> (TempDir, String) {
    let dir = tempfile::Builder::new()
        .prefix("pi-durable-generation-")
        .tempdir()
        .expect("tempdir");
    let path = dir.path().join("session.sqlite").to_string_lossy().into_owned();
    (dir, path)
}

async fn open_sqlite(path: &str, setup: &ChatSetup) -> OpenedChat {
    let storage = open_node_sqlite_storage(path, NodeSqliteStorageOptions::default())
        .await
        .expect("open sqlite");
    open_chat(Arc::new(storage), setup, OpenChatEnv::None)
        .await
        .expect("open chat")
}

async fn run_task_id(harness: &Harness, root_id: ConversationId) -> TaskId {
    harness
        .snapshot_conversation(&*LIVE_DOC, root_id, context())
        .await
        .expect("live")
        .expect("present")
        .run
        .expect("run")
        .task_id
}

async fn generation_checkpoint(harness: &Harness, id: TaskId) -> Option<GenerationCheckpoint> {
    let record = harness
        .get_task(TaskId::<GenerationResult>::new_unchecked(id.as_i64()), context())
        .await
        .expect("get_task")?;
    match record.state {
        TaskState::Pending { checkpoint, .. }
        | TaskState::Running { checkpoint, .. }
        | TaskState::Waiting { checkpoint, .. } => Some(serde_json::from_value(checkpoint).expect("decode checkpoint")),
        TaskState::Completing { .. } | TaskState::Terminal { .. } => None,
    }
}

fn error_503() -> pi_ai::types::AssistantMessage {
    faux_assistant_message(
        FauxAssistantMessageContent::Text(String::new()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("503 Service Unavailable".to_owned()),
            ..Default::default()
        }),
    )
}

#[tokio::test]
async fn reruns_preparation_interrupted_before_its_commit() {
    let (_dir, path) = sqlite_path().await;
    let setup = chat_setup(None);
    let reached = deferred::<()>();
    let block = Arc::new(AtomicBool::new(true));
    let reached_cb = reached.clone();
    add_section(
        &setup.registry,
        "preamble",
        Arc::new(move |_, ctx| {
            let reached_cb = reached_cb.clone();
            let block = Arc::clone(&block);
            Box::pin(async move {
                if block.swap(false, Ordering::AcqRel) {
                    reached_cb.resolve(());
                    if let Some(sig) = ctx.abort_signal() {
                        aborted::<()>(&sig).await?;
                    }
                }
                Ok(Some("p".to_owned()))
            })
        }),
        None,
        None,
    )
    .expect("add preamble");
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("answer".to_owned()),
            None,
        ))]);
    let mut opened = open_sqlite(&path, &setup).await;
    opened.harness.resume().expect("resume");
    let sub_id = opened
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
        .id();
    reached.wait().await.expect("reached");
    let task_id = run_task_id(&opened.harness, opened.root.id()).await;
    opened.harness.close(context()).await.expect("close");

    opened = open_sqlite(&path, &setup).await;
    assert_eq!(
        generation_checkpoint(&opened.harness, task_id).await,
        Some(GenerationCheckpoint::Prepare {
            attempt: 1,
            compacted: None,
            overflow: None,
        })
    );
    opened.harness.resume().expect("resume");
    let settled = opened
        .harness
        .submission(sub_id, context())
        .await
        .expect("sub")
        .expect("present")
        .wait(context())
        .await
        .expect("wait");
    assert_eq!(settled.record.status(), SubmissionStatus::Done);
    let kinds: Vec<String> = all_entries(&opened.root, None)
        .await
        .expect("entries")
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, vec!["pi.user", "pi.system", "pi.assistant"]);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn resends_a_request_interrupted_before_any_partial_without_repeating_preparation() {
    let (_dir, path) = sqlite_path().await;
    let setup = chat_setup(None);
    add_section(
        &setup.registry,
        "preamble",
        Arc::new(|_, _| Box::pin(async { Ok(Some("p".to_owned())) })),
        Some(SectionOptions { tag: Some(false) }),
        None,
    )
    .expect("add preamble");
    let reached = deferred::<()>();
    let sent = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let timeouts = Arc::new(Mutex::new(Vec::<Option<u64>>::new()));
    let reached_cb = reached.clone();
    let sent_cb = Arc::clone(&sent);
    let timeouts_cb = Arc::clone(&timeouts);
    setup.faux.set_responses(vec![
        FauxResponseStep::Factory(Arc::new(move |_, opts, _, _| {
            let reached_cb = reached_cb.clone();
            Box::pin(async move {
                reached_cb.resolve(());
                let sig = opts.and_then(|o| o.base.signal).expect("signal");
                aborted(&sig).await
            })
        })),
        FauxResponseStep::Factory(Arc::new(move |req, opts, _, _| {
            let roles: Vec<String> = req
                .messages
                .iter()
                .map(|m| match m {
                    Message::User(_) => "user".to_owned(),
                    Message::Assistant(_) => "assistant".to_owned(),
                    Message::ToolResult(_) => "toolResult".to_owned(),
                    Message::System(_) => "system".to_owned(),
                })
                .collect();
            sent_cb.lock().expect("lock").push(roles);
            timeouts_cb
                .lock()
                .expect("lock")
                .push(opts.and_then(|o| o.base.timeout_ms));
            Box::pin(async {
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("answer".to_owned()),
                    None,
                ))
            })
        })),
    ]);

    let mut opened = open_sqlite(&path, &setup).await;
    setup.settings.update(|s| {
        s.stream = Some(ConversationStreamOptions {
            timeout_ms: Some(1234),
            ..Default::default()
        });
    });
    opened.harness.resume().expect("resume");
    let sub_id = opened
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
        .id();
    reached.wait().await.expect("reached");
    let task_id = run_task_id(&opened.harness, opened.root.id()).await;
    opened.harness.close(context()).await.expect("close");

    opened = open_sqlite(&path, &setup).await;
    match generation_checkpoint(&opened.harness, task_id).await {
        Some(GenerationCheckpoint::Request {
            attempt,
            thinking_level,
            stream_options,
            ..
        }) => {
            assert_eq!(attempt, 1);
            assert_eq!(thinking_level, ModelThinkingLevel::Off);
            assert_eq!(stream_options.timeout_ms, Some(1234));
        }
        other => panic!("Expected Request checkpoint, got {other:?}"),
    }
    // The resend uses the pinned request, not options changed after preparation.
    setup.settings.update(|s| {
        s.stream = Some(ConversationStreamOptions {
            timeout_ms: Some(999),
            ..Default::default()
        });
    });
    let snap = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("live")
        .expect("present");
    assert_eq!(snap.generation.and_then(|g| Some(g.attempt)), Some(1));
    opened.harness.resume().expect("resume");
    let settled = opened
        .harness
        .submission(sub_id, context())
        .await
        .expect("sub")
        .expect("present")
        .wait(context())
        .await
        .expect("wait");
    assert_eq!(settled.record.status(), SubmissionStatus::Done);
    assert_eq!(
        *sent.lock().expect("lock"),
        vec![vec!["user".to_owned(), "system".to_owned()]]
    );
    assert_eq!(*timeouts.lock().expect("lock"), vec![Some(1234)]);
    let kinds: Vec<String> = all_entries(&opened.root, None)
        .await
        .expect("entries")
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, vec!["pi.user", "pi.system", "pi.assistant"]);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn converts_a_committed_partial_into_an_aborted_entry_and_resends_the_same_messages() {
    let (_dir, path) = sqlite_path().await;
    let slow = chat_setup(Some(RegisterFauxProviderOptions {
        tokens_per_second: Some(20.0),
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    }));
    slow.faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("z".repeat(400)),
            None,
        ))]);
    let mut opened = open_sqlite(&path, &slow).await;
    opened.harness.resume().expect("resume");
    let watch = opened
        .harness
        .watch_doc_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("watch")
        .expect("present");
    let watched = Arc::new(Mutex::new(None::<String>));
    let watched_cb = Arc::clone(&watched);
    watch
        .start(Arc::new(move |value, _, _| {
            let watched_cb = Arc::clone(&watched_cb);
            Box::pin(async move {
                if let Some(live) = value
                    && let Some(msg) = live.generation.and_then(|g| g.message)
                    && let Some(text) = text_of(&Message::Assistant(msg))
                {
                    *watched_cb.lock().expect("lock") = Some(text.to_owned());
                }
                Ok(())
            })
        }))
        .expect("start");
    let sub_id = opened
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
        .id();
    wait_for(
        || {
            let watched = Arc::clone(&watched);
            async move { watched.lock().expect("lock").is_some() }
        },
        5_000,
    )
    .await;
    watch.stop().await;
    opened.harness.close(context()).await.expect("close");

    let setup = chat_setup(None);
    let sent = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let sent_cb = Arc::clone(&sent);
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Factory(Arc::new(move |req, _, _, _| {
            let roles: Vec<String> = req
                .messages
                .iter()
                .map(|m| match m {
                    Message::User(_) => "user".to_owned(),
                    Message::Assistant(_) => "assistant".to_owned(),
                    Message::ToolResult(_) => "toolResult".to_owned(),
                    Message::System(_) => "system".to_owned(),
                })
                .collect();
            sent_cb.lock().expect("lock").push(roles);
            Box::pin(async {
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("answer".to_owned()),
                    None,
                ))
            })
        }))]);
    opened = open_sqlite(&path, &setup).await;
    let stored = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("live")
        .expect("present");
    let partial = text_of(&Message::Assistant(
        stored.generation.expect("gen").message.expect("msg"),
    ))
    .expect("text")
    .to_owned();
    // Everything observers saw before the crash is durable.
    let watched_text = watched.lock().expect("lock").clone().expect("watched");
    assert!(partial.starts_with(&watched_text));
    opened.harness.resume().expect("resume");
    let settled = opened
        .harness
        .submission(sub_id, context())
        .await
        .expect("sub")
        .expect("present")
        .wait(context())
        .await
        .expect("wait");
    assert_eq!(settled.record.status(), SubmissionStatus::Done);
    assert_eq!(*sent.lock().expect("lock"), vec![vec!["user".to_owned()]]);
    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(
        entries.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        vec!["pi.user", "pi.assistant", "pi.assistant"]
    );
    let Message::Assistant(converted) = &entries[1].model.as_ref().expect("model")[0] else {
        panic!("expected assistant");
    };
    assert_eq!(converted.stop_reason, StopReason::Aborted);
    assert_eq!(
        text_of(&Message::Assistant(converted.clone())).as_deref(),
        Some(partial.as_str())
    );
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
            .await
            .expect("live"),
        Some(LiveState::default())
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn resumes_a_retry_backoff_after_reopen() {
    let (_dir, path) = sqlite_path().await;
    let setup = chat_setup(None);
    let now = Arc::new(AtomicI64::new(1_000));
    let now_cb = Arc::clone(&now);
    setup.now.set(move || now_cb.load(Ordering::Acquire));
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(error_503()),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("recovered".to_owned()),
            None,
        )),
    ]);
    let mut opened = open_sqlite(&path, &setup).await;
    opened.harness.resume().expect("resume");
    setup.settings.update(|s| {
        s.retry = Some(PartialRetryPolicy {
            enabled: Some(true),
            max_retries: Some(2),
            base_delay_ms: Some(60_000),
            max_agent_delay_ms: None,
        });
    });
    let sub_id = opened
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
        .id();
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
                    .is_some_and(|g| g.retry.is_some())
            }
        },
        5_000,
    )
    .await;
    let task_id = run_task_id(&opened.harness, opened.root.id()).await;
    opened.harness.close(context()).await.expect("close");

    opened = open_sqlite(&path, &setup).await;
    assert_eq!(
        generation_checkpoint(&opened.harness, task_id).await,
        Some(GenerationCheckpoint::Retry {
            attempt: 1,
            compacted: None,
            until: 61_000,
        })
    );
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
            .await
            .expect("live"),
        Some(LiveState {
            run: Some(LiveRun {
                task_id,
                inputs: vec![sub_id],
            }),
            generation: Some(LiveGeneration {
                attempt: 1,
                message: None,
                retry: Some(DurableRetryState {
                    at: 61_000,
                    error: "503 Service Unavailable".to_owned(),
                }),
                deferred: None,
            }),
            tools: None,
            compactions: None,
        })
    );
    now.store(61_000, Ordering::Release);
    opened.harness.resume().expect("resume");
    let settled = opened
        .harness
        .submission(sub_id, context())
        .await
        .expect("sub")
        .expect("present")
        .wait(context())
        .await
        .expect("wait");
    assert_eq!(settled.record.status(), SubmissionStatus::Done);
    let kinds: Vec<String> = all_entries(&opened.root, None)
        .await
        .expect("entries")
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, vec!["pi.user", "pi.assistant", "pi.assistant"]);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn resumes_polling_a_deferred_response_after_reopen() {
    let (_dir, path) = sqlite_path().await;
    let setup = chat_setup(Some(RegisterFauxProviderOptions {
        deferred: Some(FauxDeferredOptions {
            pending_fetches: None,
            poll_after_ms: Some(60_000),
        }),
        ..Default::default()
    }));
    let now = Arc::new(AtomicI64::new(1_000));
    let now_cb = Arc::clone(&now);
    setup.now.set(move || now_cb.load(Ordering::Acquire));
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("deferred answer".to_owned()),
            None,
        ))]);
    let mut opened = open_sqlite(&path, &setup).await;
    opened.harness.resume().expect("resume");
    setup.settings.update(|s| {
        s.stream = Some(ConversationStreamOptions {
            deferred: Some(StreamDeferredOption::Bool(true)),
            ..Default::default()
        });
    });
    let sub_id = opened
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
        .id();
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
    let task_id = run_task_id(&opened.harness, opened.root.id()).await;
    opened.harness.close(context()).await.expect("close");

    opened = open_sqlite(&path, &setup).await;
    match generation_checkpoint(&opened.harness, task_id).await {
        Some(GenerationCheckpoint::Poll { attempt, poll_at, .. }) => {
            assert_eq!(attempt, 1);
            assert_eq!(poll_at, 61_000);
        }
        other => panic!("Expected Poll checkpoint, got {other:?}"),
    }
    now.store(61_000, Ordering::Release);
    opened.harness.resume().expect("resume");
    let settled = opened
        .harness
        .submission(sub_id, context())
        .await
        .expect("sub")
        .expect("present")
        .wait(context())
        .await
        .expect("wait");
    let SubmissionState::Input(InputSubmissionState::Done { answer, .. }) = settled.record.state else {
        panic!("Expected Done, got {:?}", settled.record.state);
    };
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
    assert_eq!(setup.faux.state().deferred_fetch_count(), 1);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn fails_no_model_when_the_pinned_model_is_gone_after_reopen_in_request_and_in_poll() {
    let (_dir, path) = sqlite_path().await;
    let setup = chat_setup(Some(RegisterFauxProviderOptions {
        deferred: Some(FauxDeferredOptions {
            pending_fetches: None,
            poll_after_ms: Some(60_000),
        }),
        ..Default::default()
    }));
    let busy = unanswered();
    setup.faux.set_responses(vec![
        busy.step,
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("deferred".to_owned()),
            None,
        )),
    ]);
    let mut opened = open_sqlite(&path, &setup).await;
    opened.harness.resume().expect("resume");
    let requesting = opened
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
        .id();
    busy.reached.wait().await.expect("reached");
    opened.harness.close(context()).await.expect("close");

    // Reopened without the faux provider: the request's pinned model is unknown.
    let mut empty = setup.clone();
    empty.models = create_models(None);
    opened = open_sqlite(&path, &empty).await;
    opened.harness.resume().expect("resume");
    let req_settled = opened
        .harness
        .submission(requesting, context())
        .await
        .expect("sub")
        .expect("present")
        .wait(context())
        .await
        .expect("wait");
    assert!(matches!(
        req_settled.record.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) if reason == "no_model"
    ));
    opened.harness.close(context()).await.expect("close");

    opened = open_sqlite(&path, &setup).await;
    setup.settings.update(|s| {
        s.stream = Some(ConversationStreamOptions {
            deferred: Some(StreamDeferredOption::Bool(true)),
            ..Default::default()
        });
    });
    opened.harness.resume().expect("resume");
    let polling = opened
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
        .id();
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
    opened.harness.close(context()).await.expect("close");

    opened = open_sqlite(&path, &empty).await;
    opened.harness.resume().expect("resume");
    let poll_settled = opened
        .harness
        .submission(polling, context())
        .await
        .expect("sub")
        .expect("present")
        .wait(context())
        .await
        .expect("wait");
    assert!(matches!(
        poll_settled.record.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) if reason == "no_model"
    ));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn runs_a_print_style_turn_and_reads_the_durable_answer_after_reopen() {
    let (_dir, path) = sqlite_path().await;
    let setup = chat_setup(None);
    add_section(
        &setup.registry,
        "preamble",
        Arc::new(|_, _| Box::pin(async { Ok(Some("You are terse.".to_owned())) })),
        Some(SectionOptions { tag: Some(false) }),
        None,
    )
    .expect("add preamble");
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("42".to_owned()),
            None,
        ))]);
    let mut opened = open_sqlite(&path, &setup).await;
    opened.harness.resume().expect("resume");
    let submission = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("answer?".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    let settled = submission.wait(context()).await.expect("wait");
    let SubmissionState::Input(InputSubmissionState::Done { answer: answer_id, .. }) = settled.record.state else {
        panic!("Unexpected {:?}", settled.record.state);
    };
    let answer = opened
        .root
        .commit(
            move |tx| async move { tx.typed_entry(&ASSISTANT_ENTRY, answer_id).await },
            context(),
        )
        .await
        .expect("read")
        .expect("present");
    assert_eq!(
        answer.record.model.as_ref().and_then(|m| m.first()).and_then(text_of),
        Some("42")
    );
    opened.harness.close(context()).await.expect("close");

    opened = open_sqlite(&path, &setup).await;
    let after_status = opened
        .harness
        .submission(submission.id(), context())
        .await
        .expect("sub")
        .expect("present")
        .status(context())
        .await
        .expect("status");
    assert_eq!(after_status, settled.record);
    let after_answer = opened
        .root
        .commit(
            move |tx| async move { tx.typed_entry(&ASSISTANT_ENTRY, answer_id).await },
            context(),
        )
        .await
        .expect("read")
        .expect("present");
    assert_eq!(after_answer, answer);
    opened.harness.close(context()).await.expect("close");
}
