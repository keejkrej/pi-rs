//! Port of packages/durable/test/harness-inbox.test.ts

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep, FauxTokenSize,
    FauxToolCallOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_text, faux_tool_call,
};
use pi_ai::types::{AssistantMessage, Message, StopReason, Usage, UsageCost, UserMessage, UserMessageContent};
use pi_ai::{TSchema as Schema, Type as t};
use pi_chord::Context;
use pi_chord::delta::Op;
use pi_durable::documents::{DocDefinitionInput, define_session_doc};
use pi_durable::entries::RESET_ENTRY;
use pi_durable::harness::define::define_tool;
use pi_durable::harness::generation::GENERATION_TASK;
use pi_durable::harness::harness::{Conversation, Harness};
use pi_durable::harness::inbox::{INBOX_DOC, InboxItem, InboxState};
use pi_durable::harness::live::{LIVE_DOC, ToolSlotStatus};
use pi_durable::harness::submissions::SubmissionHandle;
use pi_durable::harness::tool::TOOL_TASK;
use pi_durable::harness::types::{
    AbortSubmissionResult, ConversationCreateOptions, GenerationHooks, PartialRetryPolicy, QueueMode, SubmissionDraft,
    ToolControl, ToolExecutionResult, ToolHooks, ToolRegistration, UserInput, WhenBusy,
};
use pi_durable::harness::usage::{USAGE_DOC, UsageBucket, UsageState, record_usage};
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::types::{
    CheckpointInfo, ConversationId, ConversationOwnership, ConversationQuery, ConversationRecord, Cursor,
    DocumentAddress, DocumentContent, DocumentId, DocumentPoint, DocumentQuery, DocumentRecord, EntryDraft, EntryId,
    EntryLookup, EntryQuery, EntryRecord, ErasedTaskRecord, InputSubmissionState, JsonValue, LatestFork, Page, Seq,
    Storage, StorageWrite, StoredDocument, SubmissionId, SubmissionQuery, SubmissionRecord, SubmissionState,
    SubmissionStatus, TaskId, TaskQuery,
};
use pretty_assertions::assert_eq;
use serde_json::json;
use support::chat_support::{ChatSetup, OpenChatEnv, all_entries, chat_setup, open_chat, text_of, wait_for};
use support::harness_support::{add_hooks, add_tool};
use support::session_support::{ControlledStorage, context, document_changes, flush};
use support::task_support::{Deferred, aborted, deferred};
use tempfile::TempDir;

fn answer(text: &str) -> AssistantMessage {
    faux_assistant_message(
        FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text(text))),
        None,
    )
}

struct GatedResponse {
    step: FauxResponseStep,
    reached: Deferred<()>,
    gate: Deferred<()>,
}

fn gated(message: AssistantMessage) -> GatedResponse {
    let reached = deferred();
    let gate = deferred();
    let reached_cb = reached.clone();
    let gate_cb = gate.clone();
    let step: FauxResponseStep = FauxResponseStep::Factory(Arc::new(move |_, opts, _, _| {
        let reached_cb = reached_cb.clone();
        let gate_cb = gate_cb.clone();
        let message = message.clone();
        let signal = opts.and_then(|o| o.base.signal);
        Box::pin(async move {
            reached_cb.resolve(());
            let gate_fut = gate_cb.wait();
            let abort_fut = async {
                if let Some(sig) = signal {
                    aborted::<()>(&sig).await
                } else {
                    std::future::pending().await
                }
            };
            tokio::select! {
                res = gate_fut => {
                    res?;
                    Ok(message)
                }
                res = abort_fut => {
                    Err(res.expect_err("aborted"))
                }
            }
        })
    }));
    GatedResponse { step, reached, gate }
}

fn hold_tool(setup: &ChatSetup, gate: Deferred<()>, result: ToolExecutionResult) {
    add_tool(
        &setup.registry,
        define_tool(ToolRegistration {
            name: "hold".to_owned(),
            description: "Waits for the test".to_owned(),
            parameters: t::object([] as [(&str, Schema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(move |_, _, _| {
                let gate = gate.clone();
                let result = result.clone();
                Box::pin(async move {
                    gate.wait().await?;
                    Ok(result)
                })
            }),
        }),
        None,
    )
    .expect("add hold");
}

fn hold_call() -> AssistantMessage {
    faux_assistant_message(
        FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
            "hold",
            IndexMap::new(),
            Some(FauxToolCallOptions {
                id: Some("c1".to_owned()),
            }),
        ))),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    )
}

async fn status(submission: &SubmissionHandle) -> SubmissionRecord {
    submission.status(context()).await.expect("status")
}

fn transcript(entries: &[EntryRecord]) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| entry.kind != "pi.system")
        .map(|entry| {
            let text = entry.model.as_ref().and_then(|m| m.first()).and_then(|msg| match msg {
                Message::ToolResult(_) => None,
                other => text_of(other),
            });
            match text {
                None => entry.kind.clone(),
                Some(t) => format!("{}:{t}", entry.kind),
            }
        })
        .collect()
}

fn item_mode_str(item: &InboxItem) -> (SubmissionId, &'static str) {
    match item {
        InboxItem::Steer { id, .. } => (*id, "steer"),
        InboxItem::FollowUp { id, .. } => (*id, "followUp"),
        InboxItem::Write { id, .. } => (*id, "write"),
    }
}

async fn inbox_summary(harness: &Harness, root: &Conversation) -> Vec<(SubmissionId, &'static str)> {
    harness
        .snapshot_conversation(&*INBOX_DOC, root.id(), context())
        .await
        .expect("inbox snapshot")
        .expect("inbox present")
        .items
        .iter()
        .map(item_mode_str)
        .collect()
}

async fn tool_running(harness: &Harness, root: &Conversation) {
    let harness_c = harness.clone();
    let id = root.id();
    wait_for(
        move || {
            let h = harness_c.clone();
            async move {
                h.snapshot_conversation(&*LIVE_DOC, id, context())
                    .await
                    .expect("live")
                    .and_then(|s| s.tools)
                    .and_then(|v| v.first().map(|slot| slot.status == ToolSlotStatus::Running))
                    == Some(true)
            }
        },
        5_000,
    )
    .await;
}

async fn open_mem(setup: &ChatSetup) -> support::chat_support::OpenedChat {
    let opened = open_chat(Arc::new(MemoryStorage::new()), setup, OpenChatEnv::None)
        .await
        .expect("open mem");
    opened.harness.resume().expect("resume");
    opened
}

async fn open_sqlite(path: &str, setup: &ChatSetup) -> support::chat_support::OpenedChat {
    let storage: Arc<dyn Storage> = Arc::new(
        open_node_sqlite_storage(path, NodeSqliteStorageOptions::default())
            .await
            .expect("open sqlite"),
    );
    let opened = open_chat(storage, setup, OpenChatEnv::None)
        .await
        .expect("open sqlite chat");
    opened.harness.resume().expect("resume");
    opened
}

// ---------------------------------------------------------------------------
// describe("inbox")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn queues_busy_submissions_and_places_writes_before_user_items_at_the_final_boundary_one_follow_up_per_run() {
    let setup = chat_setup(None);
    let first = gated(answer("first"));
    setup.faux.set_responses(vec![
        first.step,
        FauxResponseStep::Message(answer("second")),
        FauxResponseStep::Message(answer("third")),
    ]);
    let opened = open_mem(&setup).await;
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit a");
    first.reached.wait().await.expect("first reached");

    let f1 = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f1".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit f1");
    let write = opened
        .root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "note".to_owned(),
                    data: Some(json!("w")),
                    ..Default::default()
                },
            },
            context(),
        )
        .await
        .expect("submit write");
    let f2 = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f2".to_owned()),
                when_busy: Some(WhenBusy::FollowUp),
            },
            context(),
        )
        .await
        .expect("submit f2");
    for sub in [&f1, &write, &f2] {
        assert_eq!(status(sub).await.status(), SubmissionStatus::Queued);
    }
    assert_eq!(
        inbox_summary(&opened.harness, &opened.root).await,
        vec![(f1.id(), "followUp"), (write.id(), "write"), (f2.id(), "followUp"),]
    );
    assert_eq!(
        transcript(&all_entries(&opened.root, None).await.expect("entries")),
        vec!["pi.user:a"]
    );

    first.gate.resolve(());
    f2.wait(context()).await.expect("wait f2");
    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(
        transcript(&entries),
        vec![
            "pi.user:a",
            "pi.assistant:first",
            "note",
            "pi.user:f1",
            "pi.assistant:second",
            "pi.user:f2",
            "pi.assistant:third",
        ]
    );
    let answers: Vec<_> = entries.iter().filter(|e| e.kind == "pi.assistant").collect();
    assert_eq!(
        status(&input).await.state,
        SubmissionState::Input(InputSubmissionState::Done {
            entry: status(&input).await.state.entry().unwrap(),
            answer: answers[0].id
        })
    );
    assert_eq!(status(&write).await.status(), SubmissionStatus::Done);
    assert_eq!(
        status(&f1).await.state,
        SubmissionState::Input(InputSubmissionState::Done {
            entry: status(&f1).await.state.entry().unwrap(),
            answer: answers[1].id
        })
    );
    assert_eq!(
        status(&f2).await.state,
        SubmissionState::Input(InputSubmissionState::Done {
            entry: status(&f2).await.state.entry().unwrap(),
            answer: answers[2].id
        })
    );
    assert!(inbox_summary(&opened.harness, &opened.root).await.is_empty());
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
            .await
            .expect("live"),
        Some(pi_durable::harness::live::LiveState::default())
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn places_every_follow_up_in_one_successor_run_with_follow_up_mode_all() {
    let setup = chat_setup(None);
    let first = gated(answer("first"));
    setup
        .faux
        .set_responses(vec![first.step, FauxResponseStep::Message(answer("both"))]);
    let opened = open_mem(&setup).await;
    setup.settings.update(|s| s.follow_up_mode = Some(QueueMode::All));
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit a");
    first.reached.wait().await.expect("first reached");
    let f1 = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f1".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit f1");
    let f2 = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f2".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit f2");
    first.gate.resolve(());
    let settled = f2.wait(context()).await.expect("wait f2");
    let st1 = status(&f1).await;
    assert_eq!(st1.id, f1.id());
    assert_eq!(st1.status(), SubmissionStatus::Done);
    if let (
        SubmissionState::Input(InputSubmissionState::Done { answer: a1, .. }),
        SubmissionState::Input(InputSubmissionState::Done { answer: a2, .. }),
    ) = (&st1.state, &settled.record.state)
    {
        assert_eq!(a1, a2);
    } else {
        panic!("expected both done");
    }
    assert_eq!(
        transcript(&all_entries(&opened.root, None).await.expect("entries")),
        vec![
            "pi.user:a",
            "pi.assistant:first",
            "pi.user:f1",
            "pi.user:f2",
            "pi.assistant:both",
        ]
    );
    assert_eq!(setup.faux.state().call_count(), 2);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reads_queue_modes_when_the_final_boundarys_commit_runs_on_the_session_line() {
    let setup = chat_setup(None);
    let first = gated(answer("first"));
    setup
        .faux
        .set_responses(vec![first.step, FauxResponseStep::Message(answer("both"))]);
    let yielded = deferred();
    let yielded_cb = yielded.clone();
    add_hooks(
        &setup.registry,
        &*GENERATION_TASK,
        GenerationHooks {
            on_yield: Some(Arc::new(move |_, _, _| {
                let yielded_cb = yielded_cb.clone();
                Box::pin(async move {
                    yielded_cb.resolve(());
                    Ok(None)
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("add yield hook");

    let storage = Arc::new(ControlledStorage::new());
    let opened = open_chat(Arc::clone(&storage) as Arc<dyn Storage>, &setup, OpenChatEnv::None)
        .await
        .expect("open");
    opened.harness.resume().expect("resume");

    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit a");
    first.reached.wait().await.expect("first reached");
    let f1 = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f1".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit f1");
    let f2 = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f2".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit f2");

    let held = storage.hold_commits();
    let marker = define_session_doc(DocDefinitionInput {
        kind: "test.marker",
        version: 1,
        initial: || json!({ "n": 0 }),
        migrate: None,
        checkpoint_when: None,
    })
    .expect("marker doc");
    let occupying = tokio::spawn({
        let root = opened.root.clone();
        async move {
            root.commit(
                move |tx| async move {
                    let draft = tx.doc_session(&marker).await?;
                    draft.update(|v| {
                        let n = v.get("n").and_then(|x| x.as_i64()).unwrap_or(0) + 1;
                        *v = json!({ "n": n });
                    })
                },
                context(),
            )
            .await
        }
    });
    held.entered().await;
    first.gate.resolve(());
    yielded.wait().await.expect("yielded");
    flush().await;
    setup.settings.update(|s| s.follow_up_mode = Some(QueueMode::All));
    held.release();
    occupying.await.expect("join occupying").expect("occupying ok");
    let settled = f2.wait(context()).await.expect("wait f2");
    assert_eq!(status(&f1).await.status(), SubmissionStatus::Done);
    if let (
        SubmissionState::Input(InputSubmissionState::Done { answer: a1, .. }),
        SubmissionState::Input(InputSubmissionState::Done { answer: a2, .. }),
    ) = (status(&f1).await.state, &settled.record.state)
    {
        assert_eq!(&a1, a2);
    } else {
        panic!("expected both done");
    }
    assert_eq!(setup.faux.state().call_count(), 2);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn adds_steers_to_the_run_at_the_post_tools_boundary_and_holds_follow_ups_for_the_final_boundary() {
    let setup = chat_setup(None);
    let gate = deferred();
    hold_tool(
        &setup,
        gate.clone(),
        ToolExecutionResult {
            content: Some(vec![]),
            ..Default::default()
        },
    );
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(hold_call()),
        FauxResponseStep::Message(answer("after tools")),
        FauxResponseStep::Message(answer("follow-up")),
    ]);
    let opened = open_mem(&setup).await;
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit a");
    tool_running(&opened.harness, &opened.root).await;
    let steer = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("s".to_owned()),
                when_busy: Some(WhenBusy::Steer),
            },
            context(),
        )
        .await
        .expect("steer");
    let follow_up = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("follow up");
    gate.resolve(());
    follow_up.wait(context()).await.expect("wait f");
    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(
        transcript(&entries),
        vec![
            "pi.user:a",
            "pi.assistant",
            "pi.tool-result",
            "pi.user:s",
            "pi.assistant:after tools",
            "pi.user:f",
            "pi.assistant:follow-up",
        ]
    );
    let answers: Vec<_> = entries.iter().filter(|e| e.kind == "pi.assistant").collect();
    for (sub, ans_idx) in [(&input, 1usize), (&steer, 1usize), (&follow_up, 2usize)] {
        let st = status(sub).await;
        assert_eq!(
            st.state,
            SubmissionState::Input(InputSubmissionState::Done {
                entry: st.state.entry().unwrap(),
                answer: answers[ans_idx].id,
            })
        );
    }
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn ends_the_run_at_a_queued_reset_after_tools_and_runs_earlier_follow_ups_in_the_new_context() {
    let setup = chat_setup(None);
    let gate = deferred();
    hold_tool(
        &setup,
        gate.clone(),
        ToolExecutionResult {
            content: Some(vec![]),
            ..Default::default()
        },
    );
    let requests = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let req_cb = Arc::clone(&requests);
    let record = FauxResponseStep::Factory(Arc::new(move |request, _, _, _| {
        let req_cb = Arc::clone(&req_cb);
        Box::pin(async move {
            let line = request
                .messages
                .iter()
                .map(|m| {
                    let role = match m {
                        Message::User(_) => "user",
                        Message::Assistant(_) => "assistant",
                        Message::ToolResult(_) => "toolResult",
                        Message::System(_) => "system",
                    };
                    format!("{role}:{}", text_of(m).unwrap_or(""))
                })
                .collect();
            req_cb.lock().unwrap().push(line);
            Ok(answer("fresh"))
        })
    }));
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(hold_call()), record]);
    let opened = open_mem(&setup).await;
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit a");
    tool_running(&opened.harness, &opened.root).await;
    let follow_up = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("follow up");
    opened.root.reset(None, context()).await.expect("reset");
    gate.resolve(());
    follow_up.wait(context()).await.expect("wait f");
    match status(&input).await.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) => assert_eq!(reason, "reset"),
        other => panic!("unexpected state: {other:?}"),
    }
    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(
        transcript(&entries),
        vec![
            "pi.user:a",
            "pi.assistant",
            "pi.tool-result",
            "pi.reset",
            "pi.user:f",
            "pi.assistant:fresh",
        ]
    );
    let reset = entries
        .iter()
        .find(|entry| RESET_ENTRY.matches(Some(*entry)))
        .expect("reset entry");
    assert_eq!(reset.head, Some(reset.id));
    assert_eq!(*requests.lock().unwrap(), vec![vec!["user:f", "system:"]]);
    assert_eq!(setup.faux.state().call_count(), 2);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn places_a_queued_reset_after_the_answer_at_the_final_boundary() {
    let setup = chat_setup(None);
    let first = gated(answer("first"));
    setup.faux.set_responses(vec![first.step]);
    let opened = open_mem(&setup).await;
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit a");
    first.reached.wait().await.expect("reached");
    opened
        .root
        .reset(Some("handoff".to_owned()), context())
        .await
        .expect("reset");
    first.gate.resolve(());
    input.wait(context()).await.expect("wait");
    opened.harness.wait_for_idle(context()).await.expect("idle");
    assert_eq!(status(&input).await.status(), SubmissionStatus::Done);
    assert_eq!(
        transcript(&all_entries(&opened.root, None).await.expect("entries")),
        vec!["pi.user:a", "pi.assistant:first", "pi.reset:handoff"]
    );
    let ctx_view = opened.root.context(context()).await.expect("ctx view");
    assert_eq!(ctx_view.messages.len(), 1);
    match &ctx_view.messages[0] {
        Message::User(u) => assert_eq!(u.content, UserMessageContent::Text("handoff".to_owned())),
        other => panic!("expected user handoff, got {other:?}"),
    }
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn resets_an_idle_conversation_at_once_with_or_without_handoff_text() {
    let setup = chat_setup(None);
    setup.now.set_fixed(7);
    let opened = open_mem(&setup).await;
    let root_id = opened.root.id();
    opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "note".to_owned(),
                        ..Default::default()
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("note");
    opened.root.reset(None, context()).await.expect("reset empty");
    let view = opened.root.context(context()).await.expect("view");
    assert_eq!(view.head.as_ref().map(|h| h.kind.as_str()), Some("pi.reset"));
    assert_eq!(view.head.as_ref().and_then(|h| h.model.as_ref()), None);
    assert!(view.messages.is_empty());

    opened
        .root
        .reset(Some("carry on".to_owned()), context())
        .await
        .expect("reset handoff");
    let view = opened.root.context(context()).await.expect("view 2");
    assert_eq!(
        view.head.as_ref().and_then(|h| h.head),
        view.head.as_ref().map(|h| h.id)
    );
    assert_eq!(
        view.messages,
        vec![Message::User(UserMessage {
            content: UserMessageContent::Text("carry on".to_owned()),
            timestamp: 7,
        })]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn makes_a_queued_head_write_stale_when_it_targets_an_entry_before_the_active_range() {
    let setup = chat_setup(None);
    let first = gated(answer("first"));
    setup
        .faux
        .set_responses(vec![first.step, FauxResponseStep::Message(answer("second"))]);
    let opened = open_mem(&setup).await;
    let root_id = opened.root.id();
    let old = opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "note".to_owned(),
                        ..Default::default()
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("old note");
    opened.root.reset(None, context()).await.expect("reset");
    let reset = opened.root.context(context()).await.expect("view").head.expect("head");
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit a");
    first.reached.wait().await.expect("reached");

    let stale = opened
        .root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "summary".to_owned(),
                    head: Some(pi_durable::types::EntryDraftHead::Entry(old.id)),
                    ..Default::default()
                },
            },
            context(),
        )
        .await
        .expect("stale write");
    let fresh = opened
        .root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "summary".to_owned(),
                    head: Some(pi_durable::types::EntryDraftHead::Entry(reset.id)),
                    ..Default::default()
                },
            },
            context(),
        )
        .await
        .expect("fresh write");
    first.gate.resolve(());
    input.wait(context()).await.expect("wait");
    match status(&stale).await.state {
        SubmissionState::Write(pi_durable::types::WriteSubmissionState::Unanswered { reason, .. }) => {
            assert_eq!(reason, "stale")
        }
        other => panic!("expected stale unanswered, got {other:?}"),
    }
    assert_eq!(status(&fresh).await.status(), SubmissionStatus::Done);
    assert!(inbox_summary(&opened.harness, &opened.root).await.is_empty());

    let inside = all_entries(&opened.root, None)
        .await
        .expect("entries")
        .into_iter()
        .find(|e| e.kind == "pi.user")
        .expect("user");
    let second = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("b".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit b");
    let kept = opened
        .root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "summary".to_owned(),
                    head: Some(pi_durable::types::EntryDraftHead::Entry(inside.id)),
                    ..Default::default()
                },
            },
            context(),
        )
        .await
        .expect("submit kept");
    second.wait(context()).await.expect("wait b");
    assert_eq!(status(&kept).await.status(), SubmissionStatus::Done);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn makes_a_head_write_stale_behind_a_reset_placed_earlier_in_the_same_boundary() {
    let setup = chat_setup(None);
    let first = gated(answer("first"));
    setup.faux.set_responses(vec![first.step]);
    let opened = open_mem(&setup).await;
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit a");
    first.reached.wait().await.expect("reached");
    let target = all_entries(&opened.root, None).await.expect("entries")[0].clone();
    opened.root.reset(None, context()).await.expect("reset");
    let summary = opened
        .root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "summary".to_owned(),
                    head: Some(pi_durable::types::EntryDraftHead::Entry(target.id)),
                    ..Default::default()
                },
            },
            context(),
        )
        .await
        .expect("submit summary");
    first.gate.resolve(());
    input.wait(context()).await.expect("wait");
    match status(&summary).await.state {
        SubmissionState::Write(pi_durable::types::WriteSubmissionState::Unanswered { reason, .. }) => {
            assert_eq!(reason, "stale")
        }
        other => panic!("expected stale, got {other:?}"),
    }
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn ends_the_run_with_a_pi_reset_entry_when_a_tool_requests_a_handoff() {
    let setup = chat_setup(None);
    let gate = deferred();
    gate.resolve(());
    hold_tool(
        &setup,
        gate,
        ToolExecutionResult {
            content: Some(vec![]),
            control: Some(ToolControl {
                handoff: Some("continue here".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    setup.faux.set_responses(vec![FauxResponseStep::Message(hold_call())]);
    let opened = open_mem(&setup).await;
    let settled = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    let entries = all_entries(&opened.root, None).await.expect("entries");
    let calling = entries.iter().find(|e| e.kind == "pi.assistant").expect("calling");
    assert_eq!(
        settled.record.state,
        SubmissionState::Input(InputSubmissionState::Done {
            entry: settled.record.state.entry().unwrap(),
            answer: calling.id
        })
    );
    assert_eq!(
        transcript(&entries),
        vec!["pi.user:a", "pi.assistant", "pi.tool-result", "pi.reset:continue here"]
    );
    assert_eq!(entries.last().unwrap().head, Some(entries.last().unwrap().id));
    assert_eq!(setup.faux.state().call_count(), 1);
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
            .await
            .expect("live"),
        Some(pi_durable::harness::live::LiveState::default())
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn drops_an_on_yield_continuation_when_the_final_boundary_selects_a_follow_up() {
    let setup = chat_setup(None);
    let first = gated(answer("first"));
    setup
        .faux
        .set_responses(vec![first.step, FauxResponseStep::Message(answer("second"))]);
    let yields = Arc::new(AtomicUsize::new(0));
    add_hooks(
        &setup.registry,
        &*GENERATION_TASK,
        GenerationHooks {
            on_yield: Some(Arc::new(move |_, _, _| {
                let yields = Arc::clone(&yields);
                Box::pin(async move {
                    if yields.fetch_add(1, Ordering::SeqCst) == 0 {
                        Ok(Some(UserInput::Text("more".to_owned())))
                    } else {
                        Ok(None)
                    }
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("add yield");

    let opened = open_mem(&setup).await;
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit a");
    first.reached.wait().await.expect("reached");
    let follow_up = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit f");
    first.gate.resolve(());
    follow_up.wait(context()).await.expect("wait f");
    assert_eq!(status(&input).await.status(), SubmissionStatus::Done);
    assert_eq!(
        transcript(&all_entries(&opened.root, None).await.expect("entries")),
        vec!["pi.user:a", "pi.assistant:first", "pi.user:f", "pi.assistant:second",]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn withdraws_a_queued_submission_and_removes_its_item() {
    let setup = chat_setup(None);
    let first = gated(answer("first"));
    setup.faux.set_responses(vec![first.step]);
    let opened = open_mem(&setup).await;
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit a");
    first.reached.wait().await.expect("reached");
    let kept = opened
        .root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "note".to_owned(),
                    ..Default::default()
                },
            },
            context(),
        )
        .await
        .expect("kept");
    let withdrawn = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("withdrawn");
    assert_eq!(
        withdrawn.abort(context()).await.expect("abort"),
        AbortSubmissionResult::Aborted
    );
    let settled = withdrawn.wait(context()).await.expect("wait");
    match settled.record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) => assert_eq!(reason, "aborted"),
        other => panic!("expected aborted, got {other:?}"),
    }
    assert_eq!(
        inbox_summary(&opened.harness, &opened.root).await,
        vec![(kept.id(), "write")]
    );
    first.gate.resolve(());
    input.wait(context()).await.expect("wait input");
    assert_eq!(status(&kept).await.status(), SubmissionStatus::Done);
    assert_eq!(setup.faux.state().call_count(), 1);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn leaves_queued_items_after_a_failed_run_until_the_next_submission_places_them_in_order() {
    let setup = chat_setup(None);
    let failing = gated(faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("invalid request".to_owned()),
            ..Default::default()
        }),
    ));
    setup.faux.set_responses(vec![
        failing.step,
        FauxResponseStep::Message(answer("for f")),
        FauxResponseStep::Message(answer("for g")),
    ]);
    let opened = open_mem(&setup).await;
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit a");
    failing.reached.wait().await.expect("reached");
    let f = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit f");
    failing.gate.resolve(());
    match input.wait(context()).await.expect("wait a").record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) => assert_eq!(reason, "model_error"),
        other => panic!("expected model_error, got {other:?}"),
    }
    opened.harness.wait_for_idle(context()).await.expect("idle");
    assert_eq!(status(&f).await.status(), SubmissionStatus::Queued);
    assert_eq!(
        inbox_summary(&opened.harness, &opened.root).await,
        vec![(f.id(), "followUp")]
    );

    let g = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("g".to_owned()),
                when_busy: Some(WhenBusy::Reject),
            },
            context(),
        )
        .await
        .expect("submit g");
    g.wait(context()).await.expect("wait g");
    assert_eq!(status(&f).await.status(), SubmissionStatus::Done);
    let trans = transcript(&all_entries(&opened.root, None).await.expect("entries"));
    assert_eq!(
        &trans[trans.len() - 4..],
        &[
            "pi.user:f".to_owned(),
            "pi.assistant:for f".to_owned(),
            "pi.user:g".to_owned(),
            "pi.assistant:for g".to_owned()
        ]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_queued_submissions_across_reopen_and_settles_them_afterwards() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("session.sqlite").to_string_lossy().into_owned();
    let setup = chat_setup(None);
    let first = gated(answer("first"));
    setup.faux.set_responses(vec![
        first.step,
        FauxResponseStep::Message(answer("first again")),
        FauxResponseStep::Message(answer("f")),
    ]);
    let opened = open_sqlite(&path, &setup).await;
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit a");
    first.reached.wait().await.expect("reached");
    let f = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit f")
        .id();
    opened.harness.close(context()).await.expect("close 1");

    let opened = open_sqlite(&path, &setup).await;
    let settled = opened
        .harness
        .submission(f, context())
        .await
        .expect("sub")
        .expect("some")
        .wait(context())
        .await
        .expect("wait f");
    assert_eq!(settled.record.status(), SubmissionStatus::Done);
    let trans = transcript(&all_entries(&opened.root, None).await.expect("entries"));
    assert_eq!(
        &trans[trans.len() - 2..],
        &["pi.user:f".to_owned(), "pi.assistant:f".to_owned()]
    );
    opened.harness.close(context()).await.expect("close 2");
}

#[tokio::test]
async fn commits_inbox_changes_as_positional_chord_operations_and_a_base_when_empty() {
    let setup = chat_setup(None);
    let first = gated(answer("first"));
    setup
        .faux
        .set_responses(vec![first.step, FauxResponseStep::Message(answer("second"))]);
    let opened = open_mem(&setup).await;
    let ops_log = Arc::new(Mutex::new(Vec::<Vec<Op>>::new()));
    let ops_cb = Arc::clone(&ops_log);
    let _sub = opened
        .harness
        .subscribe_commits(move |publication, _| {
            for change in document_changes(publication) {
                if change.record.kind == "pi.inbox" && !change.ops.is_empty() {
                    ops_cb.lock().unwrap().push(change.ops);
                }
            }
        })
        .expect("sub");

    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("a");
    first.reached.wait().await.expect("reached");
    let w1 = opened
        .root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "note".to_owned(),
                    ..Default::default()
                },
            },
            context(),
        )
        .await
        .expect("w1");
    let f1 = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f1".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("f1");
    let w2 = opened
        .root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "note".to_owned(),
                    ..Default::default()
                },
            },
            context(),
        )
        .await
        .expect("w2");
    let f2 = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f2".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("f2");
    let w3 = opened
        .root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "note".to_owned(),
                    ..Default::default()
                },
            },
            context(),
        )
        .await
        .expect("w3");

    let first_five: Vec<JsonValue> = ops_log
        .lock()
        .unwrap()
        .iter()
        .take(5)
        .map(|batch| serde_json::to_value(batch).expect("ser"))
        .collect();
    assert_eq!(
        first_five,
        vec![
            json!([["p", ["items"], 0, 0, [{ "id": w1.id(), "mode": "write", "entry": { "kind": "note" } }]]]),
            json!([["p", ["items"], 1, 0, [{ "id": f1.id(), "mode": "followUp", "content": "f1" }]]]),
            json!([["p", ["items"], 2, 0, [{ "id": w2.id(), "mode": "write", "entry": { "kind": "note" } }]]]),
            json!([["p", ["items"], 3, 0, [{ "id": f2.id(), "mode": "followUp", "content": "f2" }]]]),
            json!([["p", ["items"], 4, 0, [{ "id": w3.id(), "mode": "write", "entry": { "kind": "note" } }]]]),
        ]
    );
    first.gate.resolve(());
    input.wait(context()).await.expect("wait input");
    {
        let ops_guard = ops_log.lock().unwrap();
        let batch5 = &ops_guard[5];
        assert!(batch5.iter().all(|op| matches!(op, Op::Splice(sp) if sp.4.is_empty())));
        let batch5_val = serde_json::to_value(batch5).expect("ser");
        assert!(
            batch5_val
                .as_array()
                .unwrap()
                .contains(&json!(["p", ["items"], 4, 1, []]))
        );
        assert!(!pi_js::json::stringify(batch5).contains("f2"));
    }
    f2.wait(context()).await.expect("wait f2");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn settles_a_stale_write_at_once_while_idle_and_a_queued_one_behind_waiting_items() {
    let setup = chat_setup(None);
    let failing = gated(faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("invalid request".to_owned()),
            ..Default::default()
        }),
    ));
    setup
        .faux
        .set_responses(vec![failing.step, FauxResponseStep::Message(answer("for f"))]);
    let opened = open_mem(&setup).await;
    let root_id = opened.root.id();
    let old = opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "note".to_owned(),
                        ..Default::default()
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("old");
    opened.root.reset(None, context()).await.expect("reset");
    let before = all_entries(&opened.root, None).await.expect("before").len();
    let idle = opened
        .root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "summary".to_owned(),
                    head: Some(pi_durable::types::EntryDraftHead::Entry(old.id)),
                    ..Default::default()
                },
            },
            context(),
        )
        .await
        .expect("idle stale");
    match idle.wait(context()).await.expect("wait idle").record.state {
        SubmissionState::Write(pi_durable::types::WriteSubmissionState::Unanswered { reason, .. }) => {
            assert_eq!(reason, "stale")
        }
        other => panic!("expected stale, got {other:?}"),
    }
    assert_eq!(all_entries(&opened.root, None).await.expect("after").len(), before);

    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("a");
    failing.reached.wait().await.expect("reached");
    let f = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("f");
    failing.gate.resolve(());
    input.wait(context()).await.expect("wait input");
    opened.harness.wait_for_idle(context()).await.expect("idle");
    let queued = opened
        .root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "summary".to_owned(),
                    head: Some(pi_durable::types::EntryDraftHead::Entry(old.id)),
                    ..Default::default()
                },
            },
            context(),
        )
        .await
        .expect("queued stale");
    match queued.wait(context()).await.expect("wait queued").record.state {
        SubmissionState::Write(pi_durable::types::WriteSubmissionState::Unanswered { reason, .. }) => {
            assert_eq!(reason, "stale")
        }
        other => panic!("expected stale, got {other:?}"),
    }
    assert_eq!(
        f.wait(context()).await.expect("wait f").record.status(),
        SubmissionStatus::Done
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_an_on_yield_continuation_across_a_queued_plain_write_with_the_runs_original_input() {
    let setup = chat_setup(None);
    let first = gated(answer("first"));
    setup
        .faux
        .set_responses(vec![first.step, FauxResponseStep::Message(answer("second"))]);
    let yields = Arc::new(AtomicUsize::new(0));
    add_hooks(
        &setup.registry,
        &*GENERATION_TASK,
        GenerationHooks {
            on_yield: Some(Arc::new(move |_, _, _| {
                let yields = Arc::clone(&yields);
                Box::pin(async move {
                    if yields.fetch_add(1, Ordering::SeqCst) == 0 {
                        Ok(Some(UserInput::Text("more".to_owned())))
                    } else {
                        Ok(None)
                    }
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("yield");

    let opened = open_mem(&setup).await;
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("a");
    first.reached.wait().await.expect("reached");
    let write = opened
        .root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "note".to_owned(),
                    ..Default::default()
                },
            },
            context(),
        )
        .await
        .expect("write");
    first.gate.resolve(());
    input.wait(context()).await.expect("wait input");
    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(
        transcript(&entries),
        vec![
            "pi.user:a",
            "pi.assistant:first",
            "note",
            "pi.user:more",
            "pi.assistant:second",
        ]
    );
    let st_in = status(&input).await;
    assert_eq!(
        st_in.state,
        SubmissionState::Input(InputSubmissionState::Done {
            entry: st_in.state.entry().unwrap(),
            answer: entries.last().unwrap().id
        })
    );
    assert_eq!(status(&write).await.status(), SubmissionStatus::Done);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn drops_an_on_yield_continuation_for_a_queued_reset() {
    let setup = chat_setup(None);
    let first = gated(answer("first"));
    setup.faux.set_responses(vec![first.step]);
    add_hooks(
        &setup.registry,
        &*GENERATION_TASK,
        GenerationHooks {
            on_yield: Some(Arc::new(|_, _, _| {
                Box::pin(async { Ok(Some(UserInput::Text("more".to_owned()))) })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("yield");

    let opened = open_mem(&setup).await;
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("a");
    first.reached.wait().await.expect("reached");
    opened.root.reset(None, context()).await.expect("reset");
    first.gate.resolve(());
    input.wait(context()).await.expect("wait");
    opened.harness.wait_for_idle(context()).await.expect("idle");
    assert_eq!(status(&input).await.status(), SubmissionStatus::Done);
    assert_eq!(
        transcript(&all_entries(&opened.root, None).await.expect("entries")),
        vec!["pi.user:a", "pi.assistant:first", "pi.reset"]
    );
    assert_eq!(setup.faux.state().call_count(), 1);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn adds_every_steer_to_the_run_at_the_post_tools_boundary_with_steering_mode_all() {
    let setup = chat_setup(None);
    let gate = deferred();
    hold_tool(
        &setup,
        gate.clone(),
        ToolExecutionResult {
            content: Some(vec![]),
            ..Default::default()
        },
    );
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(hold_call()),
        FauxResponseStep::Message(answer("after tools")),
        FauxResponseStep::Message(answer("follow-up")),
    ]);
    let opened = open_mem(&setup).await;
    setup.settings.update(|s| s.steering_mode = Some(QueueMode::All));
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("a");
    tool_running(&opened.harness, &opened.root).await;
    let s1 = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("s1".to_owned()),
                when_busy: Some(WhenBusy::Steer),
            },
            context(),
        )
        .await
        .expect("s1");
    let f = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("f");
    let s2 = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("s2".to_owned()),
                when_busy: Some(WhenBusy::Steer),
            },
            context(),
        )
        .await
        .expect("s2");
    gate.resolve(());
    f.wait(context()).await.expect("wait f");
    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(
        transcript(&entries),
        vec![
            "pi.user:a",
            "pi.assistant",
            "pi.tool-result",
            "pi.user:s1",
            "pi.user:s2",
            "pi.assistant:after tools",
            "pi.user:f",
            "pi.assistant:follow-up",
        ]
    );
    let answers: Vec<_> = entries.iter().filter(|e| e.kind == "pi.assistant").collect();
    for sub in [&input, &s1, &s2] {
        let st = status(sub).await;
        assert_eq!(
            st.state,
            SubmissionState::Input(InputSubmissionState::Done {
                entry: st.state.entry().unwrap(),
                answer: answers[1].id
            })
        );
    }
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn queues_an_idle_steer_behind_waiting_items_and_places_it_with_the_first_follow_up_in_id_order() {
    let setup = chat_setup(None);
    let failing = gated(faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("invalid request".to_owned()),
            ..Default::default()
        }),
    ));
    setup
        .faux
        .set_responses(vec![failing.step, FauxResponseStep::Message(answer("both"))]);
    let opened = open_mem(&setup).await;
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("a");
    failing.reached.wait().await.expect("reached");
    let f = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("f");
    failing.gate.resolve(());
    input.wait(context()).await.expect("wait input");
    opened.harness.wait_for_idle(context()).await.expect("idle");
    let steer = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("s".to_owned()),
                when_busy: Some(WhenBusy::Steer),
            },
            context(),
        )
        .await
        .expect("steer");
    let settled = steer.wait(context()).await.expect("wait steer");
    let SubmissionState::Input(InputSubmissionState::Done { answer: steer_ans, .. }) = settled.record.state else {
        panic!("expected done");
    };
    let st_f = status(&f).await;
    assert_eq!(
        st_f.state,
        SubmissionState::Input(InputSubmissionState::Done {
            entry: st_f.state.entry().unwrap(),
            answer: steer_ans
        })
    );
    let trans = transcript(&all_entries(&opened.root, None).await.expect("entries"));
    assert_eq!(
        &trans[trans.len() - 3..],
        &[
            "pi.user:f".to_owned(),
            "pi.user:s".to_owned(),
            "pi.assistant:both".to_owned()
        ]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn starts_a_queued_follow_up_after_a_terminating_round() {
    let setup = chat_setup(None);
    let gate = deferred();
    hold_tool(
        &setup,
        gate.clone(),
        ToolExecutionResult {
            content: Some(vec![]),
            control: Some(ToolControl {
                terminate: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(hold_call()),
        FauxResponseStep::Message(answer("follow-up")),
    ]);
    let opened = open_mem(&setup).await;
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("a");
    tool_running(&opened.harness, &opened.root).await;
    let f = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("f");
    gate.resolve(());
    f.wait(context()).await.expect("wait f");
    let entries = all_entries(&opened.root, None).await.expect("entries");
    let calling = entries.iter().find(|e| e.kind == "pi.assistant").expect("calling");
    let st_in = status(&input).await;
    assert_eq!(
        st_in.state,
        SubmissionState::Input(InputSubmissionState::Done {
            entry: st_in.state.entry().unwrap(),
            answer: calling.id
        })
    );
    assert_eq!(
        transcript(&entries),
        vec![
            "pi.user:a",
            "pi.assistant",
            "pi.tool-result",
            "pi.user:f",
            "pi.assistant:follow-up",
        ]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn writes_the_last_handoff_in_call_order_and_then_runs_queued_follow_ups_in_the_new_context() {
    let setup = chat_setup(None);
    let first_gate = deferred();
    let second_gate = deferred();
    hold_tool(
        &setup,
        first_gate.clone(),
        ToolExecutionResult {
            content: Some(vec![]),
            control: Some(ToolControl {
                handoff: Some("one".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    let sg_cb = second_gate.clone();
    add_tool(
        &setup.registry,
        define_tool(ToolRegistration {
            name: "later".to_owned(),
            description: "Finishes first".to_owned(),
            parameters: t::object([] as [(&str, Schema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(move |_, _, _| {
                let sg_cb = sg_cb.clone();
                Box::pin(async move {
                    sg_cb.wait().await?;
                    Ok(ToolExecutionResult {
                        content: Some(vec![]),
                        control: Some(ToolControl {
                            handoff: Some("two".to_owned()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })
                })
            }),
        }),
        None,
    )
    .expect("add later");

    let round = faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![
            FauxContentBlock::ToolCall(faux_tool_call(
                "hold",
                IndexMap::new(),
                Some(FauxToolCallOptions {
                    id: Some("c1".to_owned()),
                }),
            )),
            FauxContentBlock::ToolCall(faux_tool_call(
                "later",
                IndexMap::new(),
                Some(FauxToolCallOptions {
                    id: Some("c2".to_owned()),
                }),
            )),
        ]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    );
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(round),
        FauxResponseStep::Message(answer("follow-up")),
    ]);
    let opened = open_mem(&setup).await;
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("a");
    tool_running(&opened.harness, &opened.root).await;
    let f = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("f");
    second_gate.resolve(());
    let harness_c = opened.harness.clone();
    let root_id = opened.root.id();
    wait_for(
        move || {
            let h = harness_c.clone();
            async move {
                h.snapshot_conversation(&*LIVE_DOC, root_id, context())
                    .await
                    .expect("live")
                    .and_then(|s| s.tools)
                    .and_then(|v| v.get(1).map(|slot| slot.status == ToolSlotStatus::Done))
                    == Some(true)
            }
        },
        5_000,
    )
    .await;
    first_gate.resolve(());
    f.wait(context()).await.expect("wait f");
    assert_eq!(status(&input).await.status(), SubmissionStatus::Done);
    let trans = transcript(&all_entries(&opened.root, None).await.expect("entries"));
    assert_eq!(
        &trans[trans.len() - 4..],
        &[
            "pi.tool-result".to_owned(),
            "pi.reset:two".to_owned(),
            "pi.user:f".to_owned(),
            "pi.assistant:follow-up".to_owned()
        ]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn leaves_the_inbox_alone_when_the_runs_task_is_aborted() {
    let setup = chat_setup(None);
    let first = gated(answer("never"));
    setup.faux.set_responses(vec![first.step]);
    let opened = open_mem(&setup).await;
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("a");
    first.reached.wait().await.expect("reached");
    let f = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("f");
    let task_id = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("live")
        .expect("some")
        .run
        .expect("run")
        .task_id;
    opened.harness.abort_task(task_id, context()).await.expect("abort");
    match input.wait(context()).await.expect("wait input").record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) => assert_eq!(reason, "aborted"),
        other => panic!("expected aborted, got {other:?}"),
    }
    opened.harness.wait_for_idle(context()).await.expect("idle");
    assert_eq!(status(&f).await.status(), SubmissionStatus::Queued);
    assert_eq!(
        inbox_summary(&opened.harness, &opened.root).await,
        vec![(f.id(), "followUp")]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn returns_a_queued_submission_for_its_repeated_request_id_without_a_second_item() {
    let setup = chat_setup(None);
    let first = gated(answer("first"));
    setup.faux.set_responses(vec![first.step]);
    let opened = open_mem(&setup).await;
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("a");
    first.reached.wait().await.expect("reached");
    let queued = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: Some("r".to_owned()),
                content: UserInput::Text("f".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("queued");
    let again = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: Some("r".to_owned()),
                content: UserInput::Text("f".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("again");
    assert_eq!(again.id(), queued.id());
    assert_eq!(
        inbox_summary(&opened.harness, &opened.root).await,
        vec![(queued.id(), "followUp")]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn withdraws_a_middle_item_with_one_positional_removal() {
    let setup = chat_setup(None);
    let first = gated(answer("first"));
    setup.faux.set_responses(vec![first.step]);
    let opened = open_mem(&setup).await;
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("a");
    first.reached.wait().await.expect("reached");
    let mut items = Vec::new();
    for text in ["x", "y", "z"] {
        items.push(
            opened
                .root
                .submit(
                    SubmissionDraft::Input {
                        request_id: None,
                        content: UserInput::Text(text.to_owned()),
                        when_busy: None,
                    },
                    context(),
                )
                .await
                .expect("submit item"),
        );
    }
    let ops_log = Arc::new(Mutex::new(Vec::<Vec<Op>>::new()));
    let ops_cb = Arc::clone(&ops_log);
    let _sub = opened
        .harness
        .subscribe_commits(move |publication, _| {
            for change in document_changes(publication) {
                if change.record.kind == "pi.inbox" && !change.ops.is_empty() {
                    ops_cb.lock().unwrap().push(change.ops);
                }
            }
        })
        .expect("sub");

    items[1].abort(context()).await.expect("abort middle");
    let vals: Vec<JsonValue> = ops_log
        .lock()
        .unwrap()
        .iter()
        .map(|batch| serde_json::to_value(batch).expect("ser"))
        .collect();
    assert_eq!(vals, vec![json!([["p", ["items"], 1, 1, []]])]);
    assert_eq!(
        inbox_summary(&opened.harness, &opened.root).await,
        vec![(items[0].id(), "followUp"), (items[2].id(), "followUp")]
    );
    opened.harness.close(context()).await.expect("close");
}

struct RecordingInboxStorage {
    inner: MemoryStorage,
    inbox_id: Arc<Mutex<Option<DocumentId>>>,
    written: Arc<Mutex<Vec<(&'static str, bool)>>>,
}

#[async_trait]
impl Storage for RecordingInboxStorage {
    async fn commit(&self, writes: Vec<StorageWrite>, context: Context) -> pi_js::Result<Seq> {
        let target = *self.inbox_id.lock().unwrap();
        if let Some(target_id) = target {
            for write in &writes {
                if let StorageWrite::DocumentChange { id, content } = write
                    && *id == target_id
                {
                    match content {
                        DocumentContent::Base { value, .. } => {
                            let state: InboxState =
                                serde_json::from_value(JsonValue::Object(value.clone())).expect("inbox state");
                            self.written.lock().unwrap().push(("base", state.items.is_empty()));
                        }
                        DocumentContent::Delta { .. } => {
                            self.written.lock().unwrap().push(("delta", false));
                        }
                    }
                }
            }
        }
        self.inner.commit(writes, context).await
    }

    async fn mint_id(&self) -> pi_js::Result<i64> {
        self.inner.mint_id().await
    }

    async fn conversation(&self, id: ConversationId, context: Context) -> pi_js::Result<Option<ConversationRecord>> {
        self.inner.conversation(id, context).await
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<ConversationRecord, Cursor>> {
        self.inner.scan_conversations(query, limit, cursor, context).await
    }

    async fn entry(&self, id: EntryId, context: Context) -> pi_js::Result<Option<EntryLookup>> {
        self.inner.entry(id, context).await
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<EntryLookup>> {
        self.inner.entry_in_conversation(conversation_id, id, context).await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: Context,
    ) -> pi_js::Result<Option<EntryRecord>> {
        self.inner
            .find_latest_head_marker(conversation_id, at_or_before_entry_id, context)
            .await
    }

    async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<EntryRecord, Cursor>> {
        self.inner.scan_entries(query, limit, cursor, context).await
    }

    async fn task(&self, id: TaskId, context: Context) -> pi_js::Result<Option<ErasedTaskRecord>> {
        self.inner.task(id, context).await
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<ErasedTaskRecord, Cursor>> {
        self.inner.scan_tasks(query, limit, cursor, context).await
    }

    async fn submission(&self, id: SubmissionId, context: Context) -> pi_js::Result<Option<SubmissionRecord>> {
        self.inner.submission(id, context).await
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<SubmissionRecord, Cursor>> {
        self.inner.scan_submissions(query, limit, cursor, context).await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: Context,
    ) -> pi_js::Result<Option<SubmissionRecord>> {
        self.inner
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<DocumentRecord>> {
        self.inner.find_document(address, at, context).await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<StoredDocument>> {
        self.inner.document(id, at, context).await
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<DocumentRecord, Cursor>> {
        self.inner.scan_documents(query, limit, cursor, context).await
    }

    async fn close(&self, context: Context) -> pi_js::Result<()> {
        self.inner.close(context).await
    }
}

#[tokio::test]
async fn stores_the_inbox_as_a_base_exactly_when_it_becomes_empty() {
    let inbox_id = Arc::new(Mutex::new(None::<DocumentId>));
    let written = Arc::new(Mutex::new(Vec::<(&'static str, bool)>::new()));
    let storage = Arc::new(RecordingInboxStorage {
        inner: MemoryStorage::new(),
        inbox_id: Arc::clone(&inbox_id),
        written: Arc::clone(&written),
    });
    let setup = chat_setup(None);
    let first = gated(answer("first"));
    setup
        .faux
        .set_responses(vec![first.step, FauxResponseStep::Message(answer("second"))]);
    let opened = open_chat(storage, &setup, OpenChatEnv::None).await.expect("open");
    opened.harness.resume().expect("resume");

    let inbox_cb = Arc::clone(&inbox_id);
    let _sub = opened
        .harness
        .subscribe_commits(move |publication, _| {
            for change in document_changes(publication) {
                if change.record.kind == "pi.inbox" {
                    *inbox_cb.lock().unwrap() = Some(change.record.id);
                }
            }
        })
        .expect("sub");

    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("a");
    first.reached.wait().await.expect("reached");
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f1".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("f1");
    let f2 = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f2".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("f2");
    first.gate.resolve(());
    input.wait(context()).await.expect("wait input");
    f2.wait(context()).await.expect("wait f2");
    assert_eq!(
        *written.lock().unwrap(),
        vec![("delta", false), ("delta", false), ("base", true)]
    );
    opened.harness.close(context()).await.expect("close");
}

#[test]
fn keeps_a_complete_inbox_base_exactly_while_it_is_empty_and_a_usage_base_on_every_change() {
    let info = CheckpointInfo {
        deltas_since_base: 1000,
    };
    let empty_inbox = serde_json::to_value(InboxState { items: vec![] })
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    let nonempty_inbox = serde_json::to_value(InboxState {
        items: vec![InboxItem::FollowUp {
            id: SubmissionId::new_unchecked(1),
            content: UserInput::Text("x".to_owned()),
        }],
    })
    .unwrap()
    .as_object()
    .unwrap()
    .clone();
    assert_eq!(
        INBOX_DOC
            .definition
            .checkpoint_when(&empty_inbox, &[], info)
            .expect("ck1"),
        true
    );
    assert_eq!(
        INBOX_DOC
            .definition
            .checkpoint_when(&nonempty_inbox, &[], info)
            .expect("ck2"),
        false
    );
    let empty_usage = serde_json::to_value(UsageState::default())
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    assert_eq!(
        USAGE_DOC
            .definition
            .checkpoint_when(&empty_usage, &[], info)
            .expect("ck3"),
        true
    );
}

// ---------------------------------------------------------------------------
// describe("usage")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn totals_assistant_usage_per_model_and_tool_usage_per_tool_and_sums_the_session() {
    let setup = chat_setup(None);
    let spent = Usage {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        total_tokens: 10,
        cost: UsageCost {
            input: 0.1,
            output: 0.2,
            cache_read: 0.3,
            cache_write: 0.4,
            total: 1.0,
        },
        ..Default::default()
    };
    let gate = deferred();
    gate.resolve(());
    hold_tool(
        &setup,
        gate,
        ToolExecutionResult {
            content: Some(vec![]),
            usage: Some(spent.clone()),
            ..Default::default()
        },
    );
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(hold_call()),
        FauxResponseStep::Message(answer("done")),
        FauxResponseStep::Message(answer("other")),
    ]);
    let opened = open_mem(&setup).await;
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("a")
        .wait(context())
        .await
        .expect("wait");
    let entries = all_entries(&opened.root, None).await.expect("entries");
    let assistants: Vec<AssistantMessage> = entries
        .iter()
        .filter(|e| e.kind == "pi.assistant")
        .filter_map(|e| match e.model.as_ref()?.first() {
            Some(Message::Assistant(a)) => Some(a.clone()),
            _ => None,
        })
        .collect();
    let expected_in: i64 = assistants.iter().map(|m| m.usage.input as i64).sum();
    let expected_out: i64 = assistants.iter().map(|m| m.usage.output as i64).sum();
    let expected_tok: i64 = assistants.iter().map(|m| m.usage.total_tokens as i64).sum();

    let usage = opened
        .harness
        .snapshot_conversation(&*USAGE_DOC, opened.root.id(), context())
        .await
        .expect("usage")
        .expect("some");
    let model_u = usage.models.get("faux/faux-1").expect("model usage");
    assert_eq!(model_u.input, expected_in);
    assert_eq!(model_u.output, expected_out);
    assert_eq!(model_u.total_tokens, expected_tok);
    assert_eq!(usage.tools.get("hold"), Some(&spent));
    let result_msg = match entries
        .iter()
        .find(|e| e.kind == "pi.tool-result")
        .expect("tr")
        .model
        .as_ref()
        .unwrap()[0]
    {
        Message::ToolResult(ref tr) => tr.clone(),
        _ => panic!("expected tool result"),
    };
    assert_eq!(result_msg.usage, Some(spent.clone()));

    let fork = opened
        .root
        .fork(
            entries.last().unwrap().id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("fork");
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*USAGE_DOC, fork.id(), context())
            .await
            .expect("fork usage"),
        Some(UsageState::default())
    );
    fork.submit(
        SubmissionDraft::Input {
            request_id: None,
            content: UserInput::Text("b".to_owned()),
            when_busy: None,
        },
        context(),
    )
    .await
    .expect("b")
    .wait(context())
    .await
    .expect("wait b");
    let fork_out = opened
        .harness
        .snapshot_conversation(&*USAGE_DOC, fork.id(), context())
        .await
        .expect("fork usage")
        .expect("some")
        .models
        .get("faux/faux-1")
        .expect("model")
        .output;
    let total = opened.harness.usage(context()).await.expect("total");
    assert_eq!(
        total.models.get("faux/faux-1").expect("m").output,
        expected_out + fork_out
    );
    assert_eq!(total.tools.get("hold"), Some(&spent));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn counts_failed_attempts_converted_partials_and_tool_usage_replaced_by_after_tool() {
    let setup = chat_setup(Some(RegisterFauxProviderOptions {
        tokens_per_second: Some(200.0),
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    }));
    let spent = Usage {
        input: 5,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        total_tokens: 5,
        cost: UsageCost::default(),
        ..Default::default()
    };
    let gate = deferred();
    gate.resolve(());
    hold_tool(
        &setup,
        gate,
        ToolExecutionResult {
            content: Some(vec![]),
            ..Default::default()
        },
    );
    let spent_cb = spent.clone();
    add_hooks(
        &setup.registry,
        &*TOOL_TASK,
        ToolHooks {
            after_tool: Some(Arc::new(move |_, mut result, _, _| {
                let spent_cb = spent_cb.clone();
                Box::pin(async move {
                    result.usage = Some(spent_cb);
                    Ok(Some(result))
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("after tool");

    let retryable = faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("503 Service Unavailable".to_owned()),
            ..Default::default()
        }),
    );
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(retryable),
        FauxResponseStep::Message(hold_call()),
        FauxResponseStep::Message(answer("done")),
        FauxResponseStep::Message(answer(&"x".repeat(400))),
    ]);
    let opened = open_mem(&setup).await;
    setup.settings.update(|s| {
        s.retry = Some(PartialRetryPolicy {
            enabled: Some(true),
            max_retries: Some(1),
            base_delay_ms: Some(1),
            max_agent_delay_ms: None,
        });
    });
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("a")
        .wait(context())
        .await
        .expect("wait a");
    let usage_a = opened
        .harness
        .snapshot_conversation(&*USAGE_DOC, opened.root.id(), context())
        .await
        .expect("usage")
        .expect("some");
    assert_eq!(usage_a.tools.get("hold"), Some(&spent));

    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("b".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("b");
    let harness_c = opened.harness.clone();
    let root_id = opened.root.id();
    wait_for(
        move || {
            let h = harness_c.clone();
            async move {
                h.snapshot_conversation(&*LIVE_DOC, root_id, context())
                    .await
                    .expect("live")
                    .and_then(|s| s.generation)
                    .and_then(|g| g.message)
                    .is_some()
            }
        },
        5_000,
    )
    .await;
    let task_id = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("live")
        .expect("some")
        .run
        .expect("run")
        .task_id;
    opened.harness.abort_task(task_id, context()).await.expect("abort");
    input.wait(context()).await.expect("wait b");

    let assistants: Vec<AssistantMessage> = all_entries(&opened.root, None)
        .await
        .expect("entries")
        .into_iter()
        .filter(|e| e.kind == "pi.assistant")
        .filter_map(|e| match e.model?.into_iter().next() {
            Some(Message::Assistant(a)) => Some(a),
            _ => None,
        })
        .collect();
    let reasons: Vec<StopReason> = assistants.iter().map(|m| m.stop_reason).collect();
    assert_eq!(
        reasons,
        vec![
            StopReason::Error,
            StopReason::ToolUse,
            StopReason::Stop,
            StopReason::Aborted
        ]
    );
    let output: i64 = assistants.iter().map(|m| m.usage.output as i64).sum();
    let usage_b = opened
        .harness
        .snapshot_conversation(&*USAGE_DOC, opened.root.id(), context())
        .await
        .expect("usage")
        .expect("some");
    assert_eq!(usage_b.models.get("faux/faux-1").expect("m").output, output);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_tools_named_like_object_prototype_keys_in_the_ledger_and_the_session_total() {
    let opened = open_mem(&chat_setup(None)).await;
    let usage = Usage {
        input: 1,
        output: 1,
        cache_read: 0,
        cache_write: 0,
        total_tokens: 2,
        cost: UsageCost::default(),
        ..Default::default()
    };
    let names = ["constructor", "__proto__", "toString"];
    let root_id = opened.root.id();
    for name in names {
        for _ in 0..2 {
            let u = usage.clone();
            opened
                .root
                .commit(
                    move |tx| async move { record_usage(&tx, root_id, UsageBucket::Tools, name, &u).await },
                    context(),
                )
                .await
                .expect("record usage");
        }
    }
    let total = opened.harness.usage(context()).await.expect("usage");
    let keys: Vec<&str> = total.tools.keys().map(String::as_str).collect();
    assert_eq!(keys, names);
    for name in names {
        assert_eq!(total.tools.get(name).expect("present").output, 2);
    }
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_usage_totals_exact_across_reopen_counting_a_partial_converted_after_reopen_once() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("session.sqlite").to_string_lossy().into_owned();
    let setup = chat_setup(Some(RegisterFauxProviderOptions {
        tokens_per_second: Some(200.0),
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    }));
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(answer("first")),
        FauxResponseStep::Message(answer(&"x".repeat(400))),
        FauxResponseStep::Message(answer("again")),
    ]);
    let opened = open_sqlite(&path, &setup).await;
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("a")
        .wait(context())
        .await
        .expect("wait a");
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("b".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("b");
    let harness_c = opened.harness.clone();
    let root_id = opened.root.id();
    wait_for(
        move || {
            let h = harness_c.clone();
            async move {
                h.snapshot_conversation(&*LIVE_DOC, root_id, context())
                    .await
                    .expect("live")
                    .and_then(|s| s.generation)
                    .and_then(|g| g.message)
                    .is_some()
            }
        },
        5_000,
    )
    .await;
    opened.harness.close(context()).await.expect("close 1");

    let opened = open_sqlite(&path, &setup).await;
    opened.harness.wait_for_idle(context()).await.expect("idle");
    let assistants: Vec<AssistantMessage> = all_entries(&opened.root, None)
        .await
        .expect("entries")
        .into_iter()
        .filter(|e| e.kind == "pi.assistant")
        .filter_map(|e| match e.model?.into_iter().next() {
            Some(Message::Assistant(a)) => Some(a),
            _ => None,
        })
        .collect();
    let reasons: Vec<StopReason> = assistants.iter().map(|m| m.stop_reason).collect();
    assert_eq!(reasons, vec![StopReason::Stop, StopReason::Aborted, StopReason::Stop]);
    let total = opened
        .harness
        .usage(context())
        .await
        .expect("usage")
        .models
        .get("faux/faux-1")
        .expect("model")
        .clone();
    let sum_in: i64 = assistants.iter().map(|m| m.usage.input as i64).sum();
    let sum_out: i64 = assistants.iter().map(|m| m.usage.output as i64).sum();
    let sum_tok: i64 = assistants.iter().map(|m| m.usage.total_tokens as i64).sum();
    assert_eq!(
        [total.input, total.output, total.total_tokens],
        [sum_in, sum_out, sum_tok]
    );
    opened.harness.close(context()).await.expect("close 2");
}

#[tokio::test]
async fn records_usage_as_numeric_sets_on_the_ledger_in_the_entrys_commit() {
    let setup = chat_setup(None);
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(answer("one")),
        FauxResponseStep::Message(answer("two")),
    ]);
    let opened = open_mem(&setup).await;
    let commits = Arc::new(Mutex::new(Vec::<(usize, Vec<Op>)>::new()));
    let commits_cb = Arc::clone(&commits);
    let _sub = opened
        .harness
        .subscribe_commits(move |publication, _| {
            for change in document_changes(publication) {
                if change.record.kind != "pi.usage" {
                    continue;
                }
                let entries = publication
                    .changes
                    .iter()
                    .filter(|c| {
                        matches!(
                            c,
                            pi_durable::types::CommitChange::Table(pi_durable::types::TableCommitChange::Entry(_))
                        )
                    })
                    .count();
                commits_cb.lock().unwrap().push((entries, change.ops));
            }
        })
        .expect("sub");

    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("a")
        .wait(context())
        .await
        .expect("wait a");
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("b".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("b")
        .wait(context())
        .await
        .expect("wait b");

    let g = commits.lock().unwrap();
    assert_eq!(g.len(), 2);
    let first_ops = serde_json::to_value(&g[0].1).expect("ser 0");
    assert_eq!(first_ops.as_array().unwrap().len(), 1);
    let first_op = first_ops[0].as_array().unwrap();
    assert_eq!(first_op[0], json!("s"));
    assert_eq!(first_op[1], json!(["models", "faux/faux-1"]));
    assert!(first_op[2].is_object());
    let second_ops = serde_json::to_value(&g[1].1).expect("ser 1");
    assert!(second_ops.as_array().unwrap().iter().all(|op| {
        let arr = op.as_array().unwrap();
        arr[0] == json!("s") && arr[1].get(1) == Some(&json!("faux/faux-1"))
    }));
    assert!(g.iter().all(|c| c.0 >= 1));
    opened.harness.close(context()).await.expect("close");
    let _ = LatestFork::Initial;
}
