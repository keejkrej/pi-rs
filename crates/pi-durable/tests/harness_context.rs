//! Port of packages/durable/test/harness-context.test.ts

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::types::{Message, StopReason};
use pi_durable::harness::harness::{Conversation, Harness};
use pi_durable::harness::types::ConversationCreateOptions;
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::types::{
    ContextEdit, ConversationOwnership, EntryDraft, EntryDraftHead, EntryId, EntryRecord, SelfHead,
};
use pretty_assertions::assert_eq;
use serde_json::json;
use support::harness_support::{
    OpenHarnessOptions, assistant, describe_message, open_harness, system, tool_result, user,
};
use support::session_support::context;

struct ContextSetup {
    _harness: Harness,
    root: Conversation,
}

impl ContextSetup {
    async fn append(&self, draft: EntryDraft) -> EntryRecord {
        let root_id = self.root.id();
        self.root
            .commit(
                move |tx| async move { tx.append_entry(root_id, draft).await },
                context(),
            )
            .await
            .expect("append entry")
    }

    async fn message(&self, model: Message, kind: &str) -> EntryRecord {
        self.append(EntryDraft {
            kind: kind.to_owned(),
            model: Some(vec![model]),
            data: None,
            head: None,
            edits: None,
        })
        .await
    }
}

async fn setup() -> ContextSetup {
    let opened = open_harness(Arc::new(MemoryStorage::new()), &[], OpenHarnessOptions::default())
        .await
        .expect("open harness");
    let root = opened.harness.root(context(), None).await.expect("root");
    ContextSetup {
        _harness: opened.harness,
        root,
    }
}

fn ids(entries: &[EntryRecord]) -> Vec<EntryId> {
    entries.iter().map(|entry| entry.id).collect()
}

fn descriptions(messages: &[Message]) -> Vec<String> {
    messages.iter().map(describe_message).collect()
}

#[tokio::test]
async fn returns_the_whole_transcript_without_a_head_and_excludes_model_less_entries_from_messages() {
    let s = setup().await;
    let first = s.message(user("hi").into(), "message").await;
    let note = s
        .append(EntryDraft {
            kind: "note".to_owned(),
            model: None,
            data: Some(json!({ "text": "display only" })),
            head: None,
            edits: None,
        })
        .await;
    let answer = s.message(assistant("hello", &[], None).into(), "message").await;
    let view = s.root.context(context()).await.expect("context");
    assert!(view.head.is_none());
    assert_eq!(ids(&view.entries), vec![first.id, note.id, answer.id]);
    assert_eq!(descriptions(&view.messages), vec!["user:hi", "assistant:hello"]);
}

#[tokio::test]
async fn excludes_aborted_error_and_deferred_assistant_messages_but_keeps_their_raw_entries() {
    let s = setup().await;
    s.message(user("q").into(), "message").await;
    let aborted = s
        .message(assistant("partial", &[], Some(StopReason::Aborted)).into(), "message")
        .await;
    s.message(assistant("failed", &[], Some(StopReason::Error)).into(), "message")
        .await;
    s.message(assistant("later", &[], Some(StopReason::Deferred)).into(), "message")
        .await;
    s.message(assistant("done", &[], Some(StopReason::Length)).into(), "message")
        .await;
    let view = s.root.context(context()).await.expect("context");
    assert_eq!(view.entries.len(), 5);
    assert_eq!(view.entries[1].id, aborted.id);
    assert_eq!(descriptions(&view.messages), vec!["user:q", "assistant:done"]);
}

#[tokio::test]
async fn resolves_self_heads_and_uses_the_newest_head_marker() {
    let s = setup().await;
    s.message(user("old").into(), "message").await;
    let reset = s
        .append(EntryDraft {
            kind: "reset".to_owned(),
            model: Some(vec![user("fresh start").into()]),
            data: None,
            head: Some(EntryDraftHead::SelfId(SelfHead)),
            edits: None,
        })
        .await;
    assert_eq!(reset.head, Some(reset.id));
    let after = s.message(assistant("after reset", &[], None).into(), "message").await;
    let view = s.root.context(context()).await.expect("context");
    assert_eq!(view.head.as_ref().map(|h| h.id), Some(reset.id));
    assert_eq!(ids(&view.entries), vec![reset.id, after.id]);
    assert_eq!(
        descriptions(&view.messages),
        vec!["user:fresh start", "assistant:after reset"]
    );

    // A compaction summary heads an earlier kept entry; older head markers in range drop out.
    let summary = s
        .append(EntryDraft {
            kind: "summary".to_owned(),
            model: Some(vec![user("summary").into()]),
            data: None,
            head: Some(EntryDraftHead::Entry(after.id)),
            edits: None,
        })
        .await;
    let tail = s.message(user("next").into(), "message").await;
    let view = s.root.context(context()).await.expect("context");
    assert_eq!(view.head.as_ref().map(|h| h.id), Some(summary.id));
    assert_eq!(ids(&view.entries), vec![summary.id, after.id, tail.id]);
    assert_eq!(
        descriptions(&view.messages),
        vec!["user:summary", "assistant:after reset", "user:next"]
    );
}

#[tokio::test]
async fn applies_the_newest_edit_per_target_within_the_active_range() {
    let s = setup().await;
    let first = s.message(user("first").into(), "message").await;
    let second = s.message(user("second").into(), "message").await;
    s.append(EntryDraft {
        kind: "edit".to_owned(),
        model: None,
        data: None,
        head: None,
        edits: Some(vec![ContextEdit::Replace {
            target: first.id,
            messages: vec![user("first v2").into()],
        }]),
    })
    .await;
    s.append(EntryDraft {
        kind: "edit".to_owned(),
        model: None,
        data: None,
        head: None,
        edits: Some(vec![ContextEdit::Replace {
            target: first.id,
            messages: vec![user("first v3").into()],
        }]),
    })
    .await;
    s.append(EntryDraft {
        kind: "edit".to_owned(),
        model: None,
        data: None,
        head: None,
        edits: Some(vec![ContextEdit::Omit { target: second.id }]),
    })
    .await;
    let view = s.root.context(context()).await.expect("context");
    assert_eq!(view.entries.len(), 5);
    assert_eq!(descriptions(&view.messages), vec!["user:first v3"]);

    // Edits before the active range no longer apply.
    let reset = s
        .append(EntryDraft {
            kind: "reset".to_owned(),
            model: None,
            data: None,
            head: Some(EntryDraftHead::Entry(second.id)),
            edits: None,
        })
        .await;
    let view = s.root.context(context()).await.expect("context");
    assert_eq!(view.head.as_ref().map(|h| h.id), Some(reset.id));
    assert!(view.messages.is_empty());

    s.append(EntryDraft {
        kind: "edit".to_owned(),
        model: None,
        data: None,
        head: None,
        edits: Some(vec![ContextEdit::Replace {
            target: second.id,
            messages: vec![user("second v2").into()],
        }]),
    })
    .await;
    let view = s.root.context(context()).await.expect("context");
    assert_eq!(descriptions(&view.messages), vec!["user:second v2"]);
}

#[tokio::test]
async fn keeps_positional_system_messages_and_orders_tool_results_by_call_order() {
    let s = setup().await;
    let mut preamble = IndexMap::new();
    preamble.insert("preamble".to_owned(), Some("You help.".to_owned()));
    s.message(system(preamble).into(), "pi.system").await;
    s.message(user("run tools").into(), "message").await;
    s.message(assistant("calling", &["b", "a"], None).into(), "message")
        .await;
    s.message(tool_result("a", None).into(), "message").await;
    let mut cwd = IndexMap::new();
    cwd.insert("cwd".to_owned(), Some("/repo".to_owned()));
    s.append(EntryDraft {
        kind: "pi.system".to_owned(),
        model: Some(vec![system(cwd).into()]),
        data: None,
        head: None,
        edits: None,
    })
    .await;
    s.message(tool_result("b", None).into(), "message").await;
    s.message(tool_result("zz", None).into(), "message").await;
    s.message(assistant("done", &[], None).into(), "message").await;
    let view = s.root.context(context()).await.expect("context");
    assert_eq!(
        descriptions(&view.messages),
        vec![
            "system:preamble",
            "user:run tools",
            "assistant:calling",
            "result:b:result b",
            "result:a:result a",
            "system:cwd",
            "assistant:done",
        ]
    );
}

#[tokio::test]
async fn synthesizes_missing_tool_results_after_a_fork_and_drops_results_cut_from_their_call() {
    let s = setup().await;
    s.message(user("go").into(), "message").await;
    let call = s
        .message(assistant("calling", &["x", "y"], None).into(), "message")
        .await;
    s.message(tool_result("x", None).into(), "message").await;
    let second = s.message(tool_result("y", None).into(), "message").await;
    let child = s
        .root
        .fork(
            call.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("fork");
    let child_view = child.context(context()).await.expect("child context");
    assert_eq!(
        descriptions(&child_view.messages),
        vec!["user:go", "assistant:calling", "result:x:error", "result:y:error",]
    );
    match &child_view.messages[2] {
        Message::ToolResult(tr) => {
            assert_eq!(tr.tool_name, "tool-x");
            assert_eq!(tr.details, Some(json!({ "reason": "missing_result" })));
        }
        other => panic!("expected ToolResult message, got {other:?}"),
    }

    // A head between a call and its results leaves stray results that are not sent.
    let root_id = s.root.id();
    s.root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "reset".to_owned(),
                        model: None,
                        data: None,
                        head: Some(EntryDraftHead::Entry(second.id)),
                        edits: None,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("append reset");
    let parent_view = s.root.context(context()).await.expect("parent context");
    assert!(parent_view.messages.is_empty());
    let kinds: Vec<&str> = parent_view.entries.iter().map(|entry| entry.kind.as_str()).collect();
    assert_eq!(kinds, vec!["reset", "message"]);
}
