//! Port of packages/durable/test/session-documents.test.ts

mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_chord::delta::tracker::DraftInput;
use pi_chord::delta::{Op, OpSet, Seg};
use pi_durable::documents::{
    ConversationDocToken, DocDefinitionCallbacks, DocDefinitionInput, DocFamilyDefinitionCallbacks,
    RewindableConversationDocToken, SessionDocFamilyToken, SessionDocToken, define_conversation_doc,
    define_rewindable_conversation_doc, define_session_doc, define_session_doc_family_with, define_session_doc_with,
};
use pi_durable::ids::id_from_number;
use pi_durable::session::TransactionScope;
use pi_durable::types::{
    ConversationId, DocumentAddress, DocumentContent, DocumentPoint, DocumentScope, ErasedTaskRecord, JsonObject,
    LatestFork, RewindableFork, Storage, StorageWrite, TaskId, TaskState,
};
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use serde_json::json;
use support::session_support::{TestSession, context, create_conversation, document_changes, flush, open_test_session};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct NestedCount {
    count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct OtherLabel {
    label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Live {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    items: Vec<String>,
    nested: NestedCount,
    other: OtherLabel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Counter {
    count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Member {
    seed: String,
    hits: i64,
}

fn live_doc(init_counter: Option<Arc<AtomicUsize>>) -> ConversationDocToken<Live> {
    let base = DocDefinitionInput {
        kind: "test.live",
        version: 1,
        initial: || Live {
            message: None,
            items: Vec::new(),
            nested: NestedCount { count: 0 },
            other: OtherLabel { label: "x".to_owned() },
        },
        migrate: None,
        checkpoint_when: None,
    };
    if let Some(counter) = init_counter {
        pi_durable::documents::define_conversation_doc_with(
            LatestFork::Initial,
            DocDefinitionCallbacks {
                kind: "test.live",
                version: 1,
                initial: Arc::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(Live {
                        message: None,
                        items: Vec::new(),
                        nested: NestedCount { count: 0 },
                        other: OtherLabel { label: "x".to_owned() },
                    })
                }),
                migrate: None,
                checkpoint_when: None,
            },
        )
        .expect("live doc with counter")
    } else {
        define_conversation_doc(LatestFork::Initial, base).expect("live doc")
    }
}

fn rewindable_live_doc() -> RewindableConversationDocToken<Live> {
    define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "test.live",
            version: 1,
            initial: || Live {
                message: None,
                items: Vec::new(),
                nested: NestedCount { count: 0 },
                other: OtherLabel { label: "x".to_owned() },
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("rewindable")
}

fn live_doc_v2() -> ConversationDocToken<Live> {
    define_conversation_doc(
        LatestFork::Initial,
        DocDefinitionInput {
            kind: "test.live",
            version: 2,
            initial: || Live {
                message: None,
                items: Vec::new(),
                nested: NestedCount { count: 0 },
                other: OtherLabel { label: "x".to_owned() },
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("live v2")
}

fn counter_doc() -> SessionDocToken<Counter> {
    define_session_doc(DocDefinitionInput {
        kind: "test.counter",
        version: 1,
        initial: || Counter { count: 0 },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("counter")
}

fn member_doc(seeds: Arc<Mutex<Vec<String>>>) -> SessionDocFamilyToken<Member, String> {
    define_session_doc_family_with(DocFamilyDefinitionCallbacks {
        kind: "test.member",
        version: 1,
        initial: Arc::new(move |seed: String| {
            seeds.lock().unwrap_or_else(|e| e.into_inner()).push(seed.clone());
            Ok(Member { seed, hits: 0 })
        }),
        migrate: None,
        checkpoint_when: None,
    })
    .expect("member")
}

async fn setup_live() -> (TestSession, ConversationId, ConversationDocToken<Live>) {
    let harness = open_test_session();
    let conversation_id = create_conversation(&harness.session).await;
    let doc = live_doc(None);
    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&doc_c, conversation_id)
                    .await?
                    .update(|live| live.items.extend(["a".to_owned(), "b".to_owned()]))
            },
            context(),
        )
        .await
        .expect("setup live");
    (harness, conversation_id, doc)
}

#[tokio::test]
async fn creates_initial_base_on_first_access_and_adopts_after_storage_success() {
    let harness = open_test_session();
    let conversation_id = create_conversation(&harness.session).await;
    let doc = live_doc(None);
    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&doc_c, conversation_id)
                    .await?
                    .update(|live| live.message = Some("hello".to_owned()))
            },
            context(),
        )
        .await
        .expect("commit");

    let writes = harness.storage.last_commit().expect("writes");
    assert_eq!(writes.len(), 1);
    match &writes[0] {
        StorageWrite::DocumentCreate { record, version, value } => {
            assert_eq!(record.kind, "test.live");
            assert_eq!(record.scope, DocumentScope::Conversation { conversation_id });
            assert_eq!(record.history, Some(pi_durable::types::DocumentHistory::Latest));
            assert_eq!(record.fork, Some(pi_durable::types::DocumentFork::Initial));
            assert_eq!(*version, 1);
            assert_eq!(value.get("message"), Some(&json!("hello")));
        }
        other => panic!("Expected DocumentCreate, got {other:?}"),
    }

    let snapshot = harness
        .facade
        .snapshot_conversation(&doc, conversation_id, context())
        .await
        .expect("snap")
        .expect("present");
    assert_eq!(
        snapshot,
        Live {
            message: Some("hello".to_owned()),
            items: Vec::new(),
            nested: NestedCount { count: 0 },
            other: OtherLabel { label: "x".to_owned() },
        }
    );
    flush().await;
    let pub_item = harness.last_publication().expect("pub");
    let published = &document_changes(&pub_item)[0];
    assert_eq!(published.record.created_at, pub_item.seq);
    assert_eq!(serde_json::to_value(&published.value).expect("s"), json!(snapshot));
    assert_eq!(published.conversation_id, Some(conversation_id));

    let admitted = harness.storage.last_admitted_commit().expect("admitted");
    match &admitted[0] {
        StorageWrite::DocumentCreate { value, .. } => {
            assert_eq!(published.value.as_ref(), Some(value));
        }
        other => panic!("Expected DocumentCreate, got {other:?}"),
    }
}

#[tokio::test]
async fn never_creates_on_snapshot_and_returns_none_when_absent() {
    let harness = open_test_session();
    let conversation_id = create_conversation(&harness.session).await;
    let before = harness.storage.commit_count();
    let doc = live_doc(None);
    let counter = counter_doc();
    let member = member_doc(Arc::new(Mutex::new(Vec::new())));

    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, conversation_id, context())
            .await
            .expect("s1"),
        None
    );
    assert_eq!(
        harness.facade.snapshot_session(&counter, context()).await.expect("s2"),
        None
    );
    assert_eq!(
        harness
            .facade
            .snapshot_session_family(&member, "k", context())
            .await
            .expect("s3"),
        None
    );
    assert_eq!(harness.storage.commit_count(), before);
    assert_eq!(harness.storage.mint_count(), 1);
}

#[tokio::test]
async fn keeps_prior_revisions_stable_after_mutation() {
    let (harness, conversation_id, doc) = setup_live().await;
    let first = harness
        .facade
        .snapshot_conversation(&doc, conversation_id, context())
        .await
        .expect("s1")
        .expect("present");
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, conversation_id, context())
            .await
            .expect("s1b"),
        Some(first.clone())
    );
    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&doc_c, conversation_id)
                    .await?
                    .update(|live| live.nested.count = 1)
            },
            context(),
        )
        .await
        .expect("mutate");
    let second = harness
        .facade
        .snapshot_conversation(&doc, conversation_id, context())
        .await
        .expect("s2")
        .expect("present");
    assert_ne!(second, first);
    assert_eq!(first.nested.count, 0);
    assert_eq!(second.nested.count, 1);
    assert_eq!(second.items, first.items);
    assert_eq!(second.other, first.other);
}

#[tokio::test]
async fn adopts_and_shares_operation_payloads_with_published_revision() {
    let (harness, conversation_id, doc) = setup_live().await;
    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&doc_c, conversation_id).await?.update(|live| {
                    live.other = OtherLabel { label: "y".to_owned() };
                    live.items.push("c".to_owned());
                })
            },
            context(),
        )
        .await
        .expect("mutate");
    flush().await;
    let snapshot = harness
        .facade
        .snapshot_conversation(&doc, conversation_id, context())
        .await
        .expect("snap")
        .expect("present");
    let published = &document_changes(&harness.last_publication().expect("pub"))[0];
    assert_eq!(serde_json::to_value(&published.value).expect("s"), json!(snapshot));
    let admitted = harness.storage.last_admitted_commit().expect("admitted");
    match &admitted[0] {
        StorageWrite::DocumentChange {
            content: DocumentContent::Delta { ops, .. },
            ..
        } => {
            assert_eq!(&published.ops, ops);
            assert!(published.ops.iter().any(|op| {
                matches!(op, Op::Set(OpSet(_, path, val)) if path == &vec![Seg::String("other".to_owned()), Seg::String("label".to_owned())] && val == &json!("y"))
            }));
        }
        other => panic!("Expected delta, got {other:?}"),
    }
}

#[tokio::test]
async fn suppresses_writes_and_publications_for_empty_batches() {
    let (harness, conversation_id, doc) = setup_live().await;
    flush().await;
    let commits = harness.storage.commit_count();
    let published = harness.publications_snapshot().len();

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                let draft = tx.doc_conversation(&doc_c, conversation_id).await?;
                let overlay = draft.overlay()?;
                overlay
                    .get("nested")?
                    .as_overlay()
                    .expect("nested")
                    .set("count", 0_i64)?;
                let items = overlay.get("items")?.as_overlay().expect("items").clone();
                items.push(&["z".into()])?;
                items.pop()?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("noop commit");
    flush().await;
    assert_eq!(harness.storage.commit_count(), commits);
    assert_eq!(harness.publications_snapshot().len(), published);
}

#[tokio::test]
async fn writes_and_publishes_replayable_nonempty_structural_noops() {
    let (harness, conversation_id, doc) = setup_live().await;
    let before = harness
        .facade
        .snapshot_conversation(&doc, conversation_id, context())
        .await
        .expect("before")
        .expect("present");
    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                let draft = tx.doc_conversation(&doc_c, conversation_id).await?;
                let overlay = draft.overlay()?;
                let items = overlay.get("items")?.as_overlay().expect("items").clone();
                let first = items.shift()?;
                items.unshift(&[first.into()])?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("structural noop");
    flush().await;
    let last = harness.storage.last_commit().expect("last");
    assert!(matches!(
        last[0],
        StorageWrite::DocumentChange {
            content: DocumentContent::Delta { .. },
            ..
        }
    ));
    let after = harness
        .facade
        .snapshot_conversation(&doc, conversation_id, context())
        .await
        .expect("after")
        .expect("present");
    assert_eq!(after, before);
    assert!(
        !document_changes(&harness.last_publication().expect("pub"))[0]
            .ops
            .is_empty()
    );
}

#[tokio::test]
async fn revokes_escaped_drafts_when_callback_settles() {
    let (harness, conversation_id, doc) = setup_live().await;
    let escaped = Arc::new(Mutex::new(None));
    let escaped_items = Arc::new(Mutex::new(None));
    let escaped_cb = Arc::clone(&escaped);
    let items_cb = Arc::clone(&escaped_items);
    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                let draft = tx.doc_conversation(&doc_c, conversation_id).await?;
                let overlay = draft.overlay()?;
                *escaped_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(overlay.clone());
                *items_cb.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(overlay.get("items")?.as_overlay().expect("items").clone());
                overlay.set("message", "inside")?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit");

    let ov = escaped.lock().unwrap_or_else(|e| e.into_inner()).clone().expect("ov");
    let items_ov = escaped_items
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .expect("items_ov");
    assert!(ov.get("message").is_err());
    assert!(ov.set("message", "outside").is_err());
    assert!(items_ov.len().is_err());
    assert!(items_ov.push(&["outside".into()]).is_err());

    let snap = harness
        .facade
        .snapshot_conversation(&doc, conversation_id, context())
        .await
        .expect("snap")
        .expect("present");
    assert_eq!(snap.message.as_deref(), Some("inside"));

    let doc_c2 = doc.clone();
    let returned = harness
        .session
        .commit(
            move |tx| async move {
                let draft = tx.doc_conversation(&doc_c2, conversation_id).await?;
                draft.overlay()
            },
            context(),
        )
        .await
        .expect("returned");
    assert!(returned.get("message").is_err());
}

#[tokio::test]
async fn aborts_every_change_when_callback_fails() {
    let (harness, conversation_id, doc) = setup_live().await;
    let before = harness
        .facade
        .snapshot_conversation(&doc, conversation_id, context())
        .await
        .expect("before")
        .expect("present");
    let counter = counter_doc();
    let commits = harness.storage.commit_count();

    let doc_c = doc.clone();
    let ctr_c = counter.clone();
    let err = harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&doc_c, conversation_id)
                    .await?
                    .update(|l| l.message = Some("lost".to_owned()))?;
                tx.doc_session(&ctr_c).await?.update(|c| c.count = 5)?;
                Err::<(), _>(pi_js::Error::msg("callback failed"))
            },
            context(),
        )
        .await
        .expect_err("should fail");
    assert!(err.to_string().contains("callback failed"));
    assert_eq!(harness.storage.commit_count(), commits);
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, conversation_id, context())
            .await
            .expect("snap"),
        Some(before)
    );
    assert_eq!(
        harness.facade.snapshot_session(&counter, context()).await.expect("ctr"),
        None
    );

    let doc_c2 = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&doc_c2, conversation_id)
                    .await?
                    .update(|l| l.message = Some("kept".to_owned()))
            },
            context(),
        )
        .await
        .expect("recover");
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, conversation_id, context())
            .await
            .expect("snap2")
            .expect("present")
            .message
            .as_deref(),
        Some("kept")
    );
}

#[tokio::test]
async fn memoizes_concurrent_duplicate_acquisition_and_initializes_once() {
    let init_counter = Arc::new(AtomicUsize::new(0));
    let doc = live_doc(Some(Arc::clone(&init_counter)));
    let harness = open_test_session();
    let conversation_id = create_conversation(&harness.session).await;
    let mints = harness.storage.mint_count();

    let d1 = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                let (first, second) = tokio::join!(
                    tx.doc_conversation(&d1, conversation_id),
                    tx.doc_conversation(&d1, conversation_id)
                );
                let first = first?;
                let second = second?;
                assert_eq!(first.overlay()?, second.overlay()?);
                assert_eq!(
                    tx.doc_conversation(&d1, conversation_id).await?.overlay()?,
                    first.overlay()?
                );
                first.overlay()?.set("message", "once")?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit");
    assert_eq!(init_counter.load(Ordering::SeqCst), 1);
    assert_eq!(harness.storage.mint_count(), mints + 1);
    let creates = harness
        .storage
        .last_commit()
        .expect("last")
        .into_iter()
        .filter(|w| matches!(w, StorageWrite::DocumentCreate { .. }))
        .count();
    assert_eq!(creates, 1);
}

#[tokio::test]
async fn uses_first_family_seed_and_ignores_seeds_for_existing_members() {
    let seeds = Arc::new(Mutex::new(Vec::new()));
    let member = member_doc(Arc::clone(&seeds));
    let harness = open_test_session();

    {
        let m = member.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let first = tx.doc_session_family(&m, "k", "first".to_owned()).await?;
                    let second = tx.doc_session_family(&m, "k", "second".to_owned()).await?;
                    assert_eq!(first.overlay()?, second.overlay()?);
                    first.overlay()?.set("hits", 1_i64)?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("commit 1");
    }
    {
        let m = member.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session_family(&m, "k", "third".to_owned())
                        .await?
                        .update(|v| v.hits += 1)?;
                    tx.doc_session_family(&m, "other", "fourth".to_owned())
                        .await?
                        .update(|v| v.hits += 1)?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("commit 2");
    }
    assert_eq!(
        *seeds.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["first".to_owned(), "fourth".to_owned()]
    );
    assert_eq!(
        harness
            .facade
            .snapshot_session_family(&member, "k", context())
            .await
            .expect("k"),
        Some(Member {
            seed: "first".to_owned(),
            hits: 2
        })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_session_family(&member, "other", context())
            .await
            .expect("other"),
        Some(Member {
            seed: "fourth".to_owned(),
            hits: 1
        })
    );
}

#[tokio::test]
async fn rejects_callback_that_succeeds_with_pending_acquisition_and_drains_it() {
    let (harness, conversation_id, doc) = setup_live().await;
    harness.facade.unload_documents().await.expect("unload");
    let gate = harness.storage.hold_find_document();
    let commits = harness.storage.commit_count();

    let (pending_tx, pending_rx) = tokio::sync::oneshot::channel();
    let gate_cb = gate.clone();
    let doc_c = doc.clone();
    let session_c = Arc::clone(&harness.session);
    let settled = Arc::new(AtomicBool::new(false));
    let settled_cb = Arc::clone(&settled);

    let commit_task = tokio::spawn(async move {
        let res = session_c
            .commit(
                move |tx| async move {
                    let tx_bg = tx.clone();
                    let pending_handle =
                        tokio::spawn(async move { tx_bg.doc_conversation(&doc_c, conversation_id).await.map(|_| ()) });
                    let _ = pending_tx.send(pending_handle);
                    gate_cb.entered().await;
                    Ok(())
                },
                context(),
            )
            .await;
        settled_cb.store(true, Ordering::SeqCst);
        res
    });

    let pending_handle = pending_rx.await.expect("rx");
    gate.entered().await;
    flush().await;
    assert!(!settled.load(Ordering::SeqCst));
    gate.release();

    let commit_err = commit_task.await.expect("join commit").expect_err("should reject");
    assert!(commit_err.to_string().contains("pending Tx operations"));
    let pending_err = pending_handle.await.expect("join pending").expect_err("should reject");
    assert!(pending_err.to_string().contains("Transaction has settled"));
    assert_eq!(harness.storage.commit_count(), commits);

    let doc_c2 = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&doc_c2, conversation_id)
                    .await?
                    .update(|l| l.message = Some("after".to_owned()))
            },
            context(),
        )
        .await
        .expect("recover");
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, conversation_id, context())
            .await
            .expect("snap")
            .expect("present")
            .message
            .as_deref(),
        Some("after")
    );
}

#[tokio::test]
async fn does_not_initialize_or_mint_for_absent_acquisition_finishing_after_settlement() {
    let harness = open_test_session();
    let initialized = Arc::new(AtomicUsize::new(0));
    let init_cb = Arc::clone(&initialized);
    let late_doc = define_session_doc_with(DocDefinitionCallbacks {
        kind: "test.late",
        version: 1,
        initial: Arc::new(move || {
            init_cb.fetch_add(1, Ordering::SeqCst);
            Ok(Counter { count: 0 })
        }),
        migrate: None,
        checkpoint_when: None,
    })
    .expect("late");

    let gate = harness.storage.hold_find_document();
    let mints = harness.storage.mint_count();
    let (pending_tx, pending_rx) = tokio::sync::oneshot::channel();
    let gate_cb = gate.clone();
    let session_c = Arc::clone(&harness.session);

    let commit_task = tokio::spawn(async move {
        session_c
            .commit(
                move |tx| async move {
                    let tx_bg = tx.clone();
                    let handle = tokio::spawn(async move { tx_bg.doc_session(&late_doc).await.map(|_| ()) });
                    let _ = pending_tx.send(handle);
                    gate_cb.entered().await;
                    Ok(())
                },
                context(),
            )
            .await
    });

    let pending_handle = pending_rx.await.expect("rx");
    gate.entered().await;
    gate.release();

    assert!(
        commit_task
            .await
            .expect("join")
            .expect_err("err")
            .to_string()
            .contains("pending Tx operations")
    );
    assert!(
        pending_handle
            .await
            .expect("join pending")
            .expect_err("err")
            .to_string()
            .contains("Transaction has settled")
    );
    assert_eq!(initialized.load(Ordering::SeqCst), 0);
    assert_eq!(harness.storage.mint_count(), mints);
}

#[tokio::test]
async fn rejects_with_callback_error_when_it_fails_with_pending_acquisition() {
    let (harness, conversation_id, doc) = setup_live().await;
    harness.facade.unload_documents().await.expect("unload");
    let gate = harness.storage.hold_find_document();
    let (pending_tx, pending_rx) = tokio::sync::oneshot::channel();
    let gate_cb = gate.clone();
    let doc_c = doc.clone();
    let session_c = Arc::clone(&harness.session);

    let commit_task = tokio::spawn(async move {
        session_c
            .commit(
                move |tx| async move {
                    let tx_bg = tx.clone();
                    let handle =
                        tokio::spawn(async move { tx_bg.doc_conversation(&doc_c, conversation_id).await.map(|_| ()) });
                    let _ = pending_tx.send(handle);
                    gate_cb.entered().await;
                    Err::<(), _>(pi_js::Error::msg("callback failed"))
                },
                context(),
            )
            .await
    });

    let pending_handle = pending_rx.await.expect("rx");
    gate.entered().await;
    gate.release();

    assert!(
        commit_task
            .await
            .expect("join")
            .expect_err("err")
            .to_string()
            .contains("callback failed")
    );
    assert!(
        pending_handle
            .await
            .expect("join pending")
            .expect_err("err")
            .to_string()
            .contains("Transaction has settled")
    );
}

#[tokio::test]
async fn rejects_tx_use_after_callback_settles() {
    let (harness, conversation_id, doc) = setup_live().await;
    let captured = Arc::new(Mutex::new(None));
    let captured_cb = Arc::clone(&captured);
    harness
        .session
        .commit_with(
            move |tx| async move {
                *captured_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
                Ok(())
            },
            context(),
            TransactionScope::default(),
        )
        .await
        .expect("commit");

    let tx = captured
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("captured");
    assert!(
        tx.doc_conversation(&doc, conversation_id)
            .await
            .err()
            .expect("err")
            .to_string()
            .contains("Transaction has settled")
    );
    assert!(
        tx.conversation(conversation_id)
            .await
            .err()
            .expect("err")
            .to_string()
            .contains("Transaction has settled")
    );
    let dummy_task = ErasedTaskRecord {
        id: id_from_number(1),
        conversation_id,
        kind: "k".to_owned(),
        version: 1,
        input: json!(null),
        owner: None,
        background: false,
        abort_requested: false,
        state: TaskState::Pending {
            checkpoint: json!(null),
        },
        memos: None,
    };
    assert!(
        tx.set_task(dummy_task)
            .err()
            .expect("err")
            .to_string()
            .contains("Transaction has settled")
    );
}

#[tokio::test]
async fn rejects_tokens_whose_semantics_or_version_disagree_with_stored_incarnation() {
    let (harness, conversation_id, _doc) = setup_live().await;
    let rew = rewindable_live_doc();
    let v2 = live_doc_v2();

    assert!(
        harness
            .facade
            .snapshot_conversation(&rew, conversation_id, context())
            .await
            .expect_err("sem")
            .to_string()
            .contains("does not match the supplied definition semantics")
    );
    let rew_c = rew.clone();
    assert!(
        harness
            .session
            .commit(
                move |tx| async move { tx.doc_conversation(&rew_c, conversation_id).await.map(|_| ()) },
                context()
            )
            .await
            .expect_err("sem tx")
            .to_string()
            .contains("does not match the supplied definition semantics")
    );
    let v2_c = v2.clone();
    assert!(
        harness
            .session
            .commit(
                move |tx| async move { tx.doc_conversation(&v2_c, conversation_id).await.map(|_| ()) },
                context()
            )
            .await
            .expect_err("ver tx")
            .to_string()
            .contains("requires migration from version 1")
    );
}

#[tokio::test]
async fn rejects_non_json_initializer_values_and_draft_placements_before_storage_admission() {
    let harness = open_test_session();
    let conversation_id = create_conversation(&harness.session).await;
    let invalid_doc = define_session_doc_with(DocDefinitionCallbacks::<JsonObject> {
        kind: "test.date",
        version: 1,
        initial: Arc::new(|| Err(pi_js::Error::js("TypeError", "Document value must be strict JSON"))),
        migrate: None,
        checkpoint_when: None,
    })
    .expect("invalid");

    let inv_c = invalid_doc.clone();
    assert!(
        harness
            .session
            .commit(
                move |tx| async move { tx.doc_session(&inv_c).await.map(|_| ()) },
                context()
            )
            .await
            .expect_err("init err")
            .to_string()
            .contains("strict JSON")
    );

    let commits = harness.storage.commit_count();
    let doc = live_doc(None);
    let doc_c = doc.clone();
    assert!(
        harness
            .session
            .commit(
                move |tx| async move {
                    let draft = tx.doc_conversation(&doc_c, conversation_id).await?;
                    let overlay = draft.overlay()?;
                    let items = overlay.get("items")?.as_overlay().expect("items").clone();
                    items.push(&[DraftInput::Undefined])?;
                    Ok(())
                },
                context()
            )
            .await
            .expect_err("undefined element")
            .to_string()
            .contains("strict JSON")
    );
    assert_eq!(harness.storage.commit_count(), commits);
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, conversation_id, context())
            .await
            .expect("snap"),
        None
    );
}

#[tokio::test]
async fn rolls_back_prepared_documents_when_batch_assembly_fails() {
    let (harness, conversation_id, doc) = setup_live().await;
    let counter = counter_doc();
    let ctr_c = counter.clone();
    harness
        .session
        .commit(
            move |tx| async move { tx.doc_session(&ctr_c).await?.update(|c| c.count = 1) },
            context(),
        )
        .await
        .expect("ctr");
    let live_snap = harness
        .facade
        .snapshot_conversation(&doc, conversation_id, context())
        .await
        .expect("s");
    let ctr_snap = harness.facade.snapshot_session(&counter, context()).await.expect("c");
    let commits = harness.storage.commit_count();

    let doc_c = doc.clone();
    let ctr_c2 = counter.clone();
    let err = harness
        .session
        .commit_with(
            move |tx| async move {
                tx.doc_conversation(&doc_c, conversation_id)
                    .await?
                    .update(|l| l.message = Some("lost".to_owned()))?;
                tx.doc_session(&ctr_c2).await?.update(|c| c.count = 2)?;
                tx.set_task(ErasedTaskRecord {
                    id: TaskId::new_unchecked(999),
                    conversation_id,
                    kind: "missing".to_owned(),
                    version: 1,
                    input: json!(null),
                    owner: None,
                    background: false,
                    abort_requested: false,
                    state: TaskState::Pending {
                        checkpoint: json!({ "phase": "start" }),
                    },
                    memos: None,
                })
            },
            context(),
            TransactionScope::default(),
        )
        .await
        .expect_err("should fail");
    assert!(err.to_string().contains("Task 999 does not exist"));
    assert_eq!(harness.storage.commit_count(), commits);
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, conversation_id, context())
            .await
            .expect("s"),
        live_snap
    );
    assert_eq!(
        harness.facade.snapshot_session(&counter, context()).await.expect("c"),
        ctr_snap
    );

    let doc_c2 = doc.clone();
    let ctr_c3 = counter.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&doc_c2, conversation_id)
                    .await?
                    .update(|l| l.message = Some("next".to_owned()))?;
                tx.doc_session(&ctr_c3).await?.update(|c| c.count = 3)
            },
            context(),
        )
        .await
        .expect("recover");
    assert_eq!(
        harness.facade.snapshot_session(&counter, context()).await.expect("c"),
        Some(Counter { count: 3 })
    );
}

#[tokio::test]
async fn poisons_session_after_uncertain_storage_failure_and_publishes_nothing() {
    let (harness, conversation_id, doc) = setup_live().await;
    flush().await;
    let before = harness
        .facade
        .snapshot_conversation(&doc, conversation_id, context())
        .await
        .expect("snap")
        .expect("present");
    let published = harness.publications_snapshot().len();
    harness.storage.fail_next_commit(pi_js::Error::msg("disk vanished"));

    let doc_c = doc.clone();
    let err = harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&doc_c, conversation_id)
                    .await?
                    .update(|l| l.message = Some("uncertain".to_owned()))
            },
            context(),
        )
        .await
        .expect_err("should fail");
    assert!(err.to_string().contains("disk vanished"));
    flush().await;
    assert_eq!(harness.publications_snapshot().len(), published);
    assert_eq!(before.message, None);
    assert!(
        harness
            .facade
            .snapshot_conversation(&doc, conversation_id, context())
            .await
            .expect_err("poisoned")
            .to_string()
            .contains("poisoned")
    );
    assert!(
        harness
            .session
            .commit(|_tx| async { Ok(()) }, context())
            .await
            .expect_err("poisoned")
            .to_string()
            .contains("poisoned")
    );
    harness.facade.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_previous_revision_unchanged_through_storage_settlement() {
    let (harness, conversation_id, doc) = setup_live().await;
    let before = harness
        .facade
        .snapshot_conversation(&doc, conversation_id, context())
        .await
        .expect("before")
        .expect("present");
    let copy = before.clone();
    let gate = harness.storage.hold_commits();
    let session_c = Arc::clone(&harness.session);
    let doc_c = doc.clone();
    let commit = tokio::spawn(async move {
        session_c
            .commit(
                move |tx| async move {
                    tx.doc_conversation(&doc_c, conversation_id).await?.update(|live| {
                        live.items.push("c".to_owned());
                        live.nested.count = 9;
                    })
                },
                context(),
            )
            .await
    });
    gate.entered().await;
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, conversation_id, context())
            .await
            .expect("during"),
        Some(copy.clone())
    );
    gate.release();
    commit.await.expect("join").expect("commit");
    let after = harness
        .facade
        .snapshot_conversation(&doc, conversation_id, context())
        .await
        .expect("after")
        .expect("present");
    assert_eq!(after.items, vec!["a", "b", "c"]);
    assert_eq!(before, copy);
}

#[tokio::test]
async fn retires_documents_and_creates_new_incarnation_at_same_address() {
    let (harness, conversation_id, doc) = setup_live().await;
    flush().await;
    let old_id = document_changes(&harness.last_publication().expect("pub"))[0].record.id;

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                let live = tx.doc_conversation(&doc_c, conversation_id).await?;
                live.overlay()?.set("message", "final")?;
                tx.retire_doc_conversation(&doc_c, conversation_id).await?;
                let replacement = tx.doc_conversation(&doc_c, conversation_id).await?;
                assert_ne!(replacement.overlay()?, live.overlay()?);
                replacement.overlay()?.set("message", "new")?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("retire & recreate");

    let writes = harness.storage.last_commit().expect("writes");
    assert_eq!(writes.len(), 3);
    assert!(
        writes
            .iter()
            .any(|w| matches!(w, StorageWrite::DocumentChange { id, .. } if *id == old_id))
    );
    assert!(writes.contains(&StorageWrite::DocumentRetire { id: old_id }));
    assert!(writes.iter().any(|w| matches!(
        w,
        StorageWrite::DocumentCreate { record, .. } if record.kind == "test.live"
    )));

    flush().await;
    let pub_item = harness.last_publication().expect("pub");
    let changes = document_changes(&pub_item);
    assert_eq!(changes.len(), 2);
    let retired = &changes[0];
    let created = &changes[1];
    assert_eq!(retired.record.id, old_id);
    assert_eq!(retired.value, None);
    assert!(retired.ops.is_empty());
    assert_eq!(retired.record.retired_at, Some(pub_item.seq));
    assert_ne!(created.record.id, old_id);
    assert_eq!(created.record.created_at, pub_item.seq);
    assert_eq!(
        created.value.as_ref().and_then(|v| v.get("message")),
        Some(&json!("new"))
    );
    assert_eq!(created.value.as_ref().and_then(|v| v.get("items")), Some(&json!([])));
    assert!(created.ops.is_empty());
    assert_eq!(
        serde_json::to_value(
            harness
                .facade
                .snapshot_conversation(&doc, conversation_id, context())
                .await
                .expect("snap")
        )
        .expect("s"),
        serde_json::to_value(&created.value).expect("s")
    );

    let doc_c2 = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move { tx.retire_doc_conversation(&doc_c2, conversation_id).await },
            context(),
        )
        .await
        .expect("retire");
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, conversation_id, context())
            .await
            .expect("none"),
        None
    );
    harness.facade.unload_documents().await.expect("unload");
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, conversation_id, context())
            .await
            .expect("none"),
        None
    );
    let commits = harness.storage.commit_count();
    let doc_c3 = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move { tx.retire_doc_conversation(&doc_c3, conversation_id).await },
            context(),
        )
        .await
        .expect("retire noop");
    assert_eq!(harness.storage.commit_count(), commits);
}

#[tokio::test]
async fn retires_without_acquisition_and_recreates_existing_and_absent_addresses() {
    let (harness, conversation_id, doc) = setup_live().await;
    flush().await;
    let old_id = document_changes(&harness.last_publication().expect("pub"))[0].record.id;
    harness.facade.unload_documents().await.expect("unload");

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                let (retired, replacement) = tokio::join!(
                    tx.retire_doc_conversation(&doc_c, conversation_id),
                    tx.doc_conversation(&doc_c, conversation_id)
                );
                let _ = retired?;
                replacement?.update(|live| live.message = Some("replacement".to_owned()))
            },
            context(),
        )
        .await
        .expect("recreate");
    let existing_writes = harness.storage.last_commit().expect("writes");
    assert_eq!(existing_writes.len(), 2);
    assert!(existing_writes.contains(&StorageWrite::DocumentRetire { id: old_id }));
    assert!(existing_writes.iter().any(|w| matches!(
        w,
        StorageWrite::DocumentCreate { record, .. } if record.kind == "test.live"
    )));

    let member = member_doc(Arc::new(Mutex::new(Vec::new())));
    let m_c = member.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                let (retired, replacement) = tokio::join!(
                    tx.retire_doc_session_family(&m_c, "absent"),
                    tx.doc_session_family(&m_c, "absent", "seed".to_owned())
                );
                let _ = retired?;
                replacement?.update(|m| m.hits = 1)
            },
            context(),
        )
        .await
        .expect("absent");
    let absent_writes = harness.storage.last_commit().expect("writes");
    assert_eq!(
        absent_writes
            .iter()
            .filter(|w| matches!(w, StorageWrite::DocumentRetire { .. }))
            .count(),
        0
    );
    assert_eq!(
        absent_writes
            .iter()
            .filter(|w| matches!(w, StorageWrite::DocumentCreate { .. }))
            .count(),
        1
    );
    assert_eq!(
        harness
            .facade
            .snapshot_session_family(&member, "absent", context())
            .await
            .expect("snap"),
        Some(Member {
            seed: "seed".to_owned(),
            hits: 1
        })
    );
}

#[tokio::test]
async fn retires_existing_incarnation_when_retirement_races_pending_acquisition() {
    let (harness, conversation_id, doc) = setup_live().await;
    flush().await;
    let old_id = document_changes(&harness.last_publication().expect("pub"))[0].record.id;

    {
        let doc_c = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let (acquired, retired) = tokio::join!(
                        tx.doc_conversation(&doc_c, conversation_id),
                        tx.retire_doc_conversation(&doc_c, conversation_id)
                    );
                    acquired?.update(|l| l.message = Some("final".to_owned()))?;
                    retired
                },
                context(),
            )
            .await
            .expect("race 1");
    }
    let first_writes = harness.storage.last_commit().expect("writes");
    assert_eq!(first_writes.len(), 2);
    assert!(first_writes.iter().any(|w| matches!(
        w,
        StorageWrite::DocumentChange {
            id,
            content: DocumentContent::Delta { .. }
        } if *id == old_id
    )));
    assert!(first_writes.contains(&StorageWrite::DocumentRetire { id: old_id }));
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, conversation_id, context())
            .await
            .expect("none"),
        None
    );

    {
        let doc_c = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_conversation(&doc_c, conversation_id)
                        .await?
                        .update(|l| l.message = Some("second".to_owned()))
                },
                context(),
            )
            .await
            .expect("second");
    }
    flush().await;
    let second_id = document_changes(&harness.last_publication().expect("pub"))[0].record.id;

    {
        let doc_c = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let (old_r, retired_r, fresh_r) = tokio::join!(
                        tx.doc_conversation(&doc_c, conversation_id),
                        tx.retire_doc_conversation(&doc_c, conversation_id),
                        tx.doc_conversation(&doc_c, conversation_id)
                    );
                    let old = old_r?;
                    let () = retired_r?;
                    let fresh = fresh_r?;
                    assert_ne!(fresh.overlay()?, old.overlay()?);
                    fresh.update(|l| l.message = Some("third".to_owned()))
                },
                context(),
            )
            .await
            .expect("race 2");
    }
    let writes = harness.storage.last_commit().expect("writes");
    assert_eq!(writes.len(), 2);
    assert!(writes.contains(&StorageWrite::DocumentRetire { id: second_id }));
    assert!(writes.iter().any(|w| matches!(
        w,
        StorageWrite::DocumentCreate { record, .. } if record.kind == "test.live"
    )));
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, conversation_id, context())
            .await
            .expect("snap")
            .expect("present")
            .message
            .as_deref(),
        Some("third")
    );
}

#[tokio::test]
async fn reloads_unloaded_document_from_storage() {
    let (harness, conversation_id, doc) = setup_live().await;
    let loaded = harness
        .facade
        .snapshot_conversation(&doc, conversation_id, context())
        .await
        .expect("loaded")
        .expect("present");
    harness.facade.unload_documents().await.expect("unload");
    let reloaded = harness
        .facade
        .snapshot_conversation(&doc, conversation_id, context())
        .await
        .expect("reloaded")
        .expect("present");
    assert_eq!(reloaded, loaded);
    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&doc_c, conversation_id)
                    .await?
                    .update(|l| l.items.push("c".to_owned()))
            },
            context(),
        )
        .await
        .expect("push");
    harness.facade.unload_documents().await.expect("unload2");
    let after = harness
        .facade
        .snapshot_conversation(&doc, conversation_id, context())
        .await
        .expect("after")
        .expect("present");
    assert_eq!(after.items, vec!["a", "b", "c"]);
}

#[tokio::test]
async fn delivers_complete_publications_synchronously_after_adoption() {
    let (harness, conversation_id, doc) = setup_live().await;
    let published = harness.publications_snapshot().len();
    let marker_key = pi_chord::context::create_context_key::<&'static str>("sync-pub-test");
    let ctx = pi_chord::context::with_context_value(marker_key.clone(), "ok", context());
    let seen_value = Arc::new(Mutex::new(None));
    let seen_cb = Arc::clone(&seen_value);
    let unsub = harness
        .facade
        .subscribe_commits(move |_pub, delivered| {
            *seen_cb.lock().unwrap_or_else(|e| e.into_inner()) = delivered.value(&marker_key);
        })
        .expect("sub");

    let doc_c = doc.clone();
    let result = harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&doc_c, conversation_id)
                    .await?
                    .update(|l| l.message = Some("m".to_owned()))?;
                Ok("done")
            },
            ctx,
        )
        .await
        .expect("commit");
    assert_eq!(result, "done");
    assert_eq!(*seen_value.lock().unwrap_or_else(|e| e.into_inner()), Some("ok"));
    assert_eq!(harness.publications_snapshot().len(), published + 1);
    unsub.call();
}

#[tokio::test]
async fn publishes_close_synchronously_and_supports_unsubscription() {
    let harness = open_test_session();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let c1 = Arc::clone(&calls);
    harness
        .facade
        .subscribe_close(move || {
            c1.lock().unwrap_or_else(|e| e.into_inner()).push("active");
        })
        .expect("sub1");
    let c2 = Arc::clone(&calls);
    let unsub = harness
        .facade
        .subscribe_close(move || {
            c2.lock().unwrap_or_else(|e| e.into_inner()).push("removed");
        })
        .expect("sub2");
    unsub.call();
    harness.facade.close(context()).await.expect("close");
    assert_eq!(*calls.lock().unwrap_or_else(|e| e.into_inner()), vec!["active"]);
}

#[tokio::test]
async fn settles_admitted_commits_before_close_and_rejects_later_admission() {
    let (harness, conversation_id, doc) = setup_live().await;
    let gate = harness.storage.hold_commits();
    let session1 = Arc::clone(&harness.session);
    let doc1 = doc.clone();
    let commit = tokio::spawn(async move {
        session1
            .commit(
                move |tx| async move {
                    tx.doc_conversation(&doc1, conversation_id)
                        .await?
                        .update(|l| l.message = Some("admitted".to_owned()))
                },
                context(),
            )
            .await
    });
    gate.entered().await;

    let session2 = Arc::clone(&harness.session);
    let doc2 = doc.clone();
    let queued = tokio::spawn(async move {
        session2
            .commit(
                move |tx| async move {
                    tx.doc_conversation(&doc2, conversation_id)
                        .await?
                        .update(|l| l.message = Some("queued".to_owned()))
                },
                context(),
            )
            .await
    });

    let member = member_doc(Arc::new(Mutex::new(Vec::new())));
    let facade_snap = harness.facade.clone();
    let member_c = member.clone();
    let admitted_snapshot = tokio::spawn(async move {
        facade_snap
            .snapshot_session_family(&member_c, "absent", context())
            .await
    });
    tokio::task::yield_now().await;

    let facade_close = harness.facade.clone();
    let closed = tokio::spawn(async move { facade_close.close(context()).await });
    tokio::task::yield_now().await;

    assert!(
        harness
            .session
            .commit(|_tx| async { Ok(()) }, context())
            .await
            .expect_err("closed")
            .to_string()
            .contains("closed")
    );
    assert!(
        harness
            .facade
            .snapshot_conversation(&doc, conversation_id, context())
            .await
            .expect_err("closed")
            .to_string()
            .contains("closed")
    );

    gate.release();
    commit.await.expect("join commit").expect("commit ok");
    queued.await.expect("join queued").expect("queued ok");
    assert_eq!(admitted_snapshot.await.expect("join snap").expect("snap ok"), None);
    closed.await.expect("join closed").expect("close ok");

    assert!(
        harness
            .storage
            .find_document(
                DocumentAddress {
                    kind: "test.live".to_owned(),
                    scope: DocumentScope::Conversation { conversation_id },
                    key: None,
                },
                DocumentPoint::Current,
                context(),
            )
            .await
            .is_err()
    );
}
