//! Port of packages/durable/test/harness-view.test.ts

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxContentBlock, FauxResponseFactory, FauxResponseStep, faux_assistant_message,
    faux_text,
};
use pi_ai::types::AssistantMessage;
use pi_chord::context::with_abort_signal;
use pi_chord::delta::{
    DeleteVerb, Op, OpDelete, OpReplace, OpSet, OpSplice, ReplaceVerb, Seg, SetVerb, SpliceVerb, apply_immutable,
};
use pi_durable::documents::{DocDefinitionInput, define_conversation_doc};
use pi_durable::harness::agent::AGENT_DOC;
use pi_durable::harness::harness::Conversation;
use pi_durable::harness::inbox::INBOX_DOC;
use pi_durable::harness::live::LIVE_DOC;
use pi_durable::harness::types::{
    AgentChange, ConversationCreateOptions, ConversationView, ConversationWatch, FieldChange, SubmissionDraft,
    UserInput,
};
use pi_durable::harness::usage::USAGE_DOC;
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::types::{
    CommitChange, ConversationOwnership, ConversationRecord, DocumentCommitChange, EntryDraft, EntryDraftHead, EntryId,
    JsonObject, LatestFork, TableCommitChange, WatchEnd,
};
use pi_js::abort::{AbortController, AbortReason};
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use serde_json::json;
use support::chat_support::{OpenChatEnv, all_entries, chat_setup, open_chat, wait_for};
use support::session_support::{context, flush};
use support::task_support::{aborted, deferred};

const MOUNTED: [&str; 4] = ["pi.agent", "pi.inbox", "pi.live", "pi.usage"];

#[derive(Clone)]
struct Frame {
    value: ConversationView,
    ops: Vec<Op>,
}

struct Recorded {
    initial: ConversationView,
    frames: Arc<Mutex<Vec<Frame>>>,
    watch: ConversationWatch,
}

impl Recorded {
    fn snapshot_frames(&self) -> Vec<Frame> {
        self.frames.lock().expect("lock frames").clone()
    }
}

async fn record(conversation: &Conversation) -> Recorded {
    let watch = conversation.watch(context()).await.expect("watch");
    let initial = watch.value();
    let frames = Arc::new(Mutex::new(Vec::new()));
    let frames_cb = Arc::clone(&frames);
    watch
        .start(Arc::new(move |value, ops, _| {
            let frames_cb = Arc::clone(&frames_cb);
            Box::pin(async move {
                frames_cb.lock().expect("lock frames").push(Frame { value, ops });
                Ok(())
            })
        }))
        .expect("start watch");
    Recorded { initial, frames, watch }
}

async fn fresh(conversation: &Conversation) -> ConversationView {
    let state = conversation.view_state(context()).await.expect("viewState");
    let value = state.value();
    state.dispose().expect("dispose");
    value
}

async fn committed(
    harness: &pi_durable::harness::harness::Harness,
    conversation: &Conversation,
    record: ConversationRecord,
) -> ConversationView {
    let mut docs: IndexMap<String, JsonObject> = IndexMap::new();
    if let Some(v) = harness
        .snapshot_conversation(&*AGENT_DOC, conversation.id(), context())
        .await
        .expect("agent")
    {
        docs.insert(
            AGENT_DOC.definition.kind.clone(),
            serde_json::to_value(v).expect("val").as_object().expect("obj").clone(),
        );
    }
    if let Some(v) = harness
        .snapshot_conversation(&*LIVE_DOC, conversation.id(), context())
        .await
        .expect("live")
    {
        docs.insert(
            LIVE_DOC.definition.kind.clone(),
            serde_json::to_value(v).expect("val").as_object().expect("obj").clone(),
        );
    }
    if let Some(v) = harness
        .snapshot_conversation(&*INBOX_DOC, conversation.id(), context())
        .await
        .expect("inbox")
    {
        docs.insert(
            INBOX_DOC.definition.kind.clone(),
            serde_json::to_value(v).expect("val").as_object().expect("obj").clone(),
        );
    }
    if let Some(v) = harness
        .snapshot_conversation(&*USAGE_DOC, conversation.id(), context())
        .await
        .expect("usage")
    {
        docs.insert(
            USAGE_DOC.definition.kind.clone(),
            serde_json::to_value(v).expect("val").as_object().expect("obj").clone(),
        );
    }
    let entries = conversation.context(context()).await.expect("ctx").entries;
    ConversationView {
        conversation: record,
        entries,
        docs,
    }
}

fn replay(initial: &ConversationView, frames: &[Frame]) -> ConversationView {
    let mut value = serde_json::to_value(initial).expect("to value");
    for frame in frames {
        value = apply_immutable(Some(value), &frame.ops).expect("apply ops");
        let parsed: ConversationView = serde_json::from_value(value.clone()).expect("from value");
        assert_eq!(parsed, frame.value);
    }
    serde_json::from_value(value).expect("final from value")
}

fn answer(text: &str) -> AssistantMessage {
    faux_assistant_message(
        FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text(text))),
        None,
    )
}

async fn drained() {
    flush().await;
}

fn count_touches(
    harness: &pi_durable::harness::harness::Harness,
    conversation_id: pi_durable::types::ConversationId,
) -> (Arc<AtomicUsize>, pi_js::Unsubscribe) {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_cb = Arc::clone(&counter);
    let sub = harness
        .subscribe_commits(move |publication, _| {
            let touched = publication.changes.iter().any(|change| match change {
                CommitChange::Table(TableCommitChange::Entry(e)) => e.conversation_id == conversation_id,
                CommitChange::Document(DocumentCommitChange::Document {
                    record,
                    conversation_id: Some(cid),
                    ops,
                    ..
                }) => *cid == conversation_id && MOUNTED.contains(&record.kind.as_str()) && !ops.is_empty(),
                _ => false,
            });
            if touched {
                counter_cb.fetch_add(1, Ordering::AcqRel);
            }
        })
        .expect("subscribe");
    (counter, sub)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct OtherState {
    n: i64,
}

#[tokio::test]
async fn hydrates_the_active_entries_and_the_built_in_documents() {
    let setup = chat_setup(None);
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(answer("hello"))]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
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
    let view = fresh(&opened.root).await;
    assert_eq!(view.conversation.id, opened.root.id());
    assert!(view.conversation.parent.is_none());
    assert_eq!(view.entries, all_entries(&opened.root, None).await.expect("entries"));
    let mut keys: Vec<&str> = view.docs.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, MOUNTED);
    assert_eq!(view.docs.get("pi.live"), Some(&JsonObject::new()));
    assert_eq!(
        view.docs.get("pi.inbox"),
        Some(json!({ "items": [] }).as_object().expect("obj"))
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn publishes_one_frame_per_touching_commit_whose_operations_rebuild_every_revision() {
    let setup = chat_setup(None);
    let release = deferred::<()>();
    let release_cb = release.clone();
    let step: FauxResponseFactory = Arc::new(move |_, options, _, _| {
        let release_cb = release_cb.clone();
        Box::pin(async move {
            let sig = options.and_then(|o| o.base.signal);
            tokio::select! {
                _ = release_cb.wait() => Ok(answer("a longer answer")),
                res = async {
                    match sig {
                        Some(s) => aborted::<()>(&s).await,
                        None => std::future::pending().await,
                    }
                } => Err(res.expect_err("aborted")),
            }
        })
    });
    setup.faux.set_responses(vec![FauxResponseStep::Factory(step)]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    let rec = record(&opened.root).await;
    let (touching, _sub) = count_touches(&opened.harness, opened.root.id());
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
            let frames = Arc::clone(&rec.frames);
            async move {
                frames
                    .lock()
                    .expect("lock")
                    .iter()
                    .any(|f| f.value.docs.get("pi.live").and_then(|d| d.get("generation")).is_some())
            }
        },
        5_000,
    )
    .await;
    release.resolve(());
    submission.wait(context()).await.expect("wait");
    opened.harness.wait_for_idle(context()).await.expect("idle");
    drained().await;
    let frames = rec.snapshot_frames();
    assert_eq!(frames.len(), touching.load(Ordering::Acquire));
    assert_eq!(
        replay(&rec.initial, &frames),
        committed(&opened.harness, &opened.root, rec.initial.conversation.clone()).await
    );
    let has_user_splice = frames[0].ops.iter().any(|op| match op {
        Op::Splice(OpSplice(SpliceVerb::P, path, 0, 0, items)) => {
            path == &vec![Seg::String("entries".to_owned())]
                && items.first().and_then(|v| v.get("kind")).and_then(|k| k.as_str()) == Some("pi.user")
        }
        _ => false,
    });
    assert!(has_user_splice);
    let expected_gen_op = Op::Set(OpSet(
        SetVerb::S,
        vec![
            Seg::String("docs".to_owned()),
            Seg::String("pi.live".to_owned()),
            Seg::String("generation".to_owned()),
        ],
        json!({ "attempt": 1 }),
    ));
    assert!(
        frames
            .iter()
            .flat_map(|f| f.ops.iter())
            .any(|op| op == &expected_gen_op)
    );
    rec.watch.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn shares_unchanged_parts_between_revisions_and_skips_commits_that_touch_nothing_mounted() {
    let setup = chat_setup(None);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    let other = opened
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
        .expect("other");
    let rec = record(&opened.root).await;
    let other_id = other.id();
    other
        .commit(
            move |tx| async move {
                tx.append_entry(
                    other_id,
                    EntryDraft {
                        kind: "note".to_owned(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("other commit");
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
        .expect("configure");
    let root_id = opened.root.id();
    opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "note".to_owned(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("root commit");
    drained().await;
    let frames = rec.snapshot_frames();
    assert_eq!(frames.len(), 2);
    assert_eq!(
        frames[0].ops,
        vec![Op::Set(OpSet(
            SetVerb::S,
            vec![
                Seg::String("docs".to_owned()),
                Seg::String("pi.agent".to_owned()),
                Seg::String("thinkingLevel".to_owned()),
            ],
            json!("high"),
        ))]
    );
    assert_eq!(frames[0].value.entries, rec.initial.entries);
    assert_eq!(frames[0].value.docs.get("pi.live"), rec.initial.docs.get("pi.live"));
    assert_eq!(frames[1].value.docs, frames[0].value.docs);
    rec.watch.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn cuts_the_entries_at_a_head_marker_keeping_the_entries_from_its_head() {
    let opened = open_chat(Arc::new(MemoryStorage::new()), &chat_setup(None), OpenChatEnv::None)
        .await
        .expect("open");
    let root_id = opened.root.id();
    let append_note = |kind: &'static str| {
        let root = opened.root.clone();
        async move {
            root.commit(
                move |tx| async move {
                    tx.append_entry(
                        root_id,
                        EntryDraft {
                            kind: kind.to_owned(),
                            model: None,
                            data: None,
                            head: None,
                            edits: None,
                        },
                    )
                    .await
                },
                context(),
            )
            .await
            .expect("append note")
        }
    };
    append_note("a").await;
    let b = append_note("b").await;
    append_note("c").await;
    let rec = record(&opened.root).await;
    let b_id = b.id;
    let summary = opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "summary".to_owned(),
                        model: None,
                        data: None,
                        head: Some(EntryDraftHead::Entry(b_id)),
                        edits: None,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("append summary");
    append_note("d").await;
    opened.root.reset(None, context()).await.expect("reset");
    drained().await;
    let frames = rec.snapshot_frames();
    let kinds: Vec<Vec<&str>> = frames
        .iter()
        .map(|f| f.value.entries.iter().map(|e| e.kind.as_str()).collect())
        .collect();
    assert_eq!(
        kinds,
        vec![
            vec!["summary", "b", "c"],
            vec!["summary", "b", "c", "d"],
            vec!["pi.reset"]
        ]
    );
    assert_eq!(
        frames[0].ops,
        vec![Op::Splice(OpSplice(
            SpliceVerb::P,
            vec![Seg::String("entries".to_owned())],
            0,
            1,
            vec![serde_json::to_value(&summary).expect("json")],
        ))]
    );
    assert_eq!(
        replay(&rec.initial, &frames),
        committed(&opened.harness, &opened.root, rec.initial.conversation.clone()).await
    );
    rec.watch.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_only_mounted_entries_for_a_raw_head_write_that_targets_before_the_active_range() {
    let opened = open_chat(Arc::new(MemoryStorage::new()), &chat_setup(None), OpenChatEnv::None)
        .await
        .expect("open");
    let root_id = opened.root.id();
    let old = opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "old".to_owned(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("append old");
    opened.root.reset(None, context()).await.expect("reset");
    let rec = record(&opened.root).await;
    let old_id = old.id;
    opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "summary".to_owned(),
                        model: None,
                        data: None,
                        head: Some(EntryDraftHead::Entry(old_id)),
                        edits: None,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("summary");
    drained().await;
    let frames = rec.snapshot_frames();
    let frame_kinds: Vec<&str> = frames[0].value.entries.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(frame_kinds, vec!["summary"]);
    rec.watch.stop().await;
    let rebuilt_kinds: Vec<String> = fresh(&opened.root).await.entries.into_iter().map(|e| e.kind).collect();
    assert_eq!(rebuilt_kinds, vec!["summary", "old"]);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn cuts_a_forks_view_into_its_inherited_entries() {
    let opened = open_chat(Arc::new(MemoryStorage::new()), &chat_setup(None), OpenChatEnv::None)
        .await
        .expect("open");
    let root_id = opened.root.id();
    let a = opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "a".to_owned(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("a");
    let b = opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "b".to_owned(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("b");
    let fork = opened
        .root
        .fork(
            b.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("fork");
    let rec = record(&fork).await;
    let fork_id = fork.id();
    let b_id = b.id;
    fork.commit(
        move |tx| async move {
            tx.append_entry(
                fork_id,
                EntryDraft {
                    kind: "summary".to_owned(),
                    model: None,
                    data: None,
                    head: Some(EntryDraftHead::Entry(b_id)),
                    edits: None,
                },
            )
            .await
        },
        context(),
    )
    .await
    .expect("summary");
    drained().await;
    let initial_ids: Vec<EntryId> = rec.initial.entries.iter().map(|e| e.id).collect();
    assert_eq!(initial_ids, vec![a.id, b.id]);
    let frames = rec.snapshot_frames();
    let kinds: Vec<&str> = frames[0].value.entries.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, vec!["summary", "b"]);
    assert_eq!(
        frames[0].value,
        committed(&opened.harness, &fork, rec.initial.conversation.clone()).await
    );
    rec.watch.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn shows_a_forks_inherited_entries_and_follows_only_the_forks_own_commits() {
    let opened = open_chat(Arc::new(MemoryStorage::new()), &chat_setup(None), OpenChatEnv::None)
        .await
        .expect("open");
    let root_id = opened.root.id();
    let first = opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "first".to_owned(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("first");
    let fork = opened
        .root
        .fork(
            first.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("fork");
    let rec = record(&fork).await;
    let init_kinds: Vec<&str> = rec.initial.entries.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(init_kinds, vec!["first"]);
    assert_eq!(
        rec.initial.conversation.parent,
        Some(pi_durable::types::ConversationParent {
            conversation_id: opened.root.id(),
            at: first.id,
        })
    );
    opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "parent".to_owned(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("parent");
    let fork_id = fork.id();
    fork.commit(
        move |tx| async move {
            tx.append_entry(
                fork_id,
                EntryDraft {
                    kind: "child".to_owned(),
                    model: None,
                    data: None,
                    head: None,
                    edits: None,
                },
            )
            .await
        },
        context(),
    )
    .await
    .expect("child");
    drained().await;
    let frames = rec.snapshot_frames();
    let all_kinds: Vec<Vec<&str>> = frames
        .iter()
        .map(|f| f.value.entries.iter().map(|e| e.kind.as_str()).collect())
        .collect();
    assert_eq!(all_kinds, vec![vec!["first", "child"]]);
    rec.watch.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn unmounts_a_retired_document_and_mounts_its_recreation_whole() {
    let opened = open_chat(Arc::new(MemoryStorage::new()), &chat_setup(None), OpenChatEnv::None)
        .await
        .expect("open");
    let rec = record(&opened.root).await;
    let root_id = opened.root.id();
    opened
        .root
        .commit(
            move |tx| async move { tx.retire_doc_conversation(&*LIVE_DOC, root_id).await },
            context(),
        )
        .await
        .expect("retire");
    opened
        .root
        .commit(
            move |tx| async move {
                tx.doc_conversation(&*LIVE_DOC, root_id)
                    .await?
                    .update(|live| live.tools = Some(Vec::new()))
            },
            context(),
        )
        .await
        .expect("recreate");
    drained().await;
    let frames = rec.snapshot_frames();
    let frame_ops: Vec<&Vec<Op>> = frames.iter().map(|f| &f.ops).collect();
    assert_eq!(
        frame_ops,
        vec![
            &vec![Op::Delete(OpDelete(
                DeleteVerb::D,
                vec![Seg::String("docs".to_owned()), Seg::String("pi.live".to_owned()),],
            ))],
            &vec![Op::Set(OpSet(
                SetVerb::S,
                vec![Seg::String("docs".to_owned()), Seg::String("pi.live".to_owned())],
                json!({ "tools": [] }),
            ))],
        ]
    );
    assert!(frames[0].value.docs.get("pi.live").is_none());
    rec.watch.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn replaces_undelivered_frames_with_the_newest_view_after_100_pending_frames() {
    let opened = open_chat(Arc::new(MemoryStorage::new()), &chat_setup(None), OpenChatEnv::None)
        .await
        .expect("open");
    let watch = opened.root.watch(context()).await.expect("watch");
    let root_id = opened.root.id();
    for _ in 0..101 {
        opened
            .root
            .commit(
                move |tx| async move {
                    tx.append_entry(
                        root_id,
                        EntryDraft {
                            kind: "note".to_owned(),
                            model: None,
                            data: None,
                            head: None,
                            edits: None,
                        },
                    )
                    .await
                },
                context(),
            )
            .await
            .expect("append");
    }
    let frames = Arc::new(Mutex::new(Vec::<Frame>::new()));
    let frames_cb = Arc::clone(&frames);
    watch
        .start(Arc::new(move |value, ops, _| {
            let frames_cb = Arc::clone(&frames_cb);
            Box::pin(async move {
                frames_cb.lock().expect("lock").push(Frame { value, ops });
                Ok(())
            })
        }))
        .expect("start");
    drained().await;
    let guard = frames.lock().expect("lock");
    assert_eq!(guard.len(), 1);
    assert_eq!(
        guard[0].ops,
        vec![Op::Replace(OpReplace(
            ReplaceVerb::R,
            serde_json::to_value(&guard[0].value).expect("json")
        ))]
    );
    assert_eq!(guard[0].value.entries.len(), 101);
    drop(guard);
    watch.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_states_and_watches_of_one_conversation_independent_and_remounts_after_the_last_one_detaches() {
    let opened = open_chat(Arc::new(MemoryStorage::new()), &chat_setup(None), OpenChatEnv::None)
        .await
        .expect("open");
    let state = opened.root.view_state(context()).await.expect("viewState");
    let rec = record(&opened.root).await;
    let root_id = opened.root.id();
    opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "one".to_owned(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("one");
    drained().await;
    let s1: Vec<String> = state.value().entries.into_iter().map(|e| e.kind).collect();
    assert_eq!(s1, vec!["one"]);
    rec.watch.stop().await;
    assert_eq!(rec.snapshot_frames().len(), 1);
    opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "two".to_owned(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("two");
    drained().await;
    let s2: Vec<String> = state.value().entries.into_iter().map(|e| e.kind).collect();
    assert_eq!(s2, vec!["one", "two"]);
    let last = state.value();
    state.dispose().expect("dispose");
    let rebuilt = fresh(&opened.root).await;
    assert_eq!(rebuilt, last);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn ends_states_and_watches_at_close_and_rejects_later_acquisition() {
    let opened = open_chat(Arc::new(MemoryStorage::new()), &chat_setup(None), OpenChatEnv::None)
        .await
        .expect("open");
    let watch = opened.root.watch(context()).await.expect("watch");
    let state = opened.root.view_state(context()).await.expect("viewState");
    opened.harness.close(context()).await.expect("close");
    assert!(matches!(watch.closed().await, WatchEnd::SessionClosed));
    assert_eq!(state.value().entries, Vec::new());
    assert!(opened.root.watch(context()).await.is_err());
    assert!(opened.root.view_state(context()).await.is_err());
}

#[tokio::test]
async fn rejects_an_acquisition_cancelled_or_closed_while_it_waits_for_the_session_line() {
    let opened = open_chat(Arc::new(MemoryStorage::new()), &chat_setup(None), OpenChatEnv::None)
        .await
        .expect("open");

    let hold = || {
        let release = deferred::<()>();
        let release_wait = release.clone();
        let root_cb = opened.root.clone();
        let blocking = tokio::spawn(async move {
            root_cb
                .commit(move |_| async move { release_wait.wait().await }, context())
                .await
        });
        (release, blocking)
    };

    let (release, blocking) = hold();
    let controller = AbortController::new();
    let cancel_ctx = with_abort_signal(controller.signal(), context());
    let root_for_watch = opened.root.clone();
    let cancelled = tokio::spawn(async move { root_for_watch.watch(cancel_ctx).await });
    flush().await;
    controller.abort(Some(AbortReason::from_error(&pi_js::Error::msg("cancelled"))));
    release.resolve(());
    blocking.await.expect("join blocking").expect("commit ok");
    let err = match cancelled.await.expect("join cancelled") {
        Ok(_) => panic!("expected err"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("cancelled"));

    let (release2, blocking2) = hold();
    let root_for_close = opened.root.clone();
    let closed_while_queued = tokio::spawn(async move { root_for_close.watch(context()).await });
    flush().await;
    let harness_close = opened.harness.clone();
    let closing = tokio::spawn(async move { harness_close.close(context()).await });
    flush().await;
    release2.resolve(());
    blocking2.await.expect("join blocking2").expect("commit ok");
    let err2 = match closed_while_queued.await.expect("join closed") {
        Ok(_) => panic!("expected closed err"),
        Err(err) => err,
    };
    assert!(err2.to_string().contains("Harness is closed"));
    closing.await.expect("join closing").expect("close ok");
}

#[tokio::test]
async fn shares_one_mount_between_concurrent_observers_and_isolates_a_failing_listener() {
    let opened = open_chat(Arc::new(MemoryStorage::new()), &chat_setup(None), OpenChatEnv::None)
        .await
        .expect("open");
    let failing = opened.root.watch(context()).await.expect("watch");
    let rec = record(&opened.root).await;
    let shared = opened.root.view_state(context()).await.expect("viewState");
    assert_eq!(failing.value(), shared.value());
    shared.dispose().expect("dispose");
    failing
        .start(Arc::new(|_, _, _| {
            Box::pin(async { Err(pi_js::Error::msg("listener failed")) })
        }))
        .expect("start failing");
    let root_id = opened.root.id();
    for kind in ["one", "two"] {
        opened
            .root
            .commit(
                move |tx| async move {
                    tx.append_entry(
                        root_id,
                        EntryDraft {
                            kind: kind.to_owned(),
                            model: None,
                            data: None,
                            head: None,
                            edits: None,
                        },
                    )
                    .await
                },
                context(),
            )
            .await
            .expect("append");
    }
    drained().await;
    assert!(matches!(failing.closed().await, WatchEnd::ListenerError { .. }));
    assert_eq!(rec.snapshot_frames().len(), 2);
    rec.watch.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn publishes_one_frame_for_a_commit_that_appends_several_entries_and_edits_a_document() {
    let opened = open_chat(Arc::new(MemoryStorage::new()), &chat_setup(None), OpenChatEnv::None)
        .await
        .expect("open");
    let other = define_conversation_doc(
        LatestFork::Initial,
        DocDefinitionInput {
            kind: "app.other",
            version: 1,
            initial: OtherState::default,
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("other doc");
    let rec = record(&opened.root).await;
    let root_id = opened.root.id();
    opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "a".to_owned(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await?;
                tx.doc_conversation(&*LIVE_DOC, root_id)
                    .await?
                    .update(|live| live.tools = Some(Vec::new()))?;
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "b".to_owned(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit a+b");
    let other_ref = other.clone();
    opened
        .root
        .commit(
            move |tx| async move { tx.doc_conversation(&other_ref, root_id).await?.update(|o| o.n = 1) },
            context(),
        )
        .await
        .expect("commit other");
    drained().await;
    let frames = rec.snapshot_frames();
    assert_eq!(frames.len(), 1);
    let kinds: Vec<&str> = frames[0].value.entries.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, vec!["a", "b"]);
    assert_eq!(
        replay(&rec.initial, &frames),
        committed(&opened.harness, &opened.root, rec.initial.conversation.clone()).await
    );
    rec.watch.stop().await;
    opened.harness.close(context()).await.expect("close");
}
