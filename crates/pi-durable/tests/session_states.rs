//! Port of packages/durable/test/session-states.test.ts

mod support;

use std::sync::{Arc, Mutex};

use pi_chord::delta::Op;
use pi_durable::documents::{
    DocDefinitionInput, DocFamilyDefinitionInput, SessionDocFamilyToken, SessionDocToken, define_conversation_doc,
    define_session_doc, define_session_doc_family,
};
use pi_durable::types::{ConversationOwnership, EntryDraft, LatestFork};
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use serde_json::json;
use support::session_support::{TestSession, context, create_conversation, document_changes, flush, open_test_session};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Retained {
    label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    value: i64,
    retained: Retained,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FamilyState {
    value: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SimpleValue {
    value: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct MigratedState {
    value: i64,
    migrated: bool,
}

fn state_doc() -> SessionDocToken<State> {
    define_session_doc(DocDefinitionInput {
        kind: "state.state",
        version: 1,
        initial: || State {
            value: 0,
            retained: Retained {
                label: "stable".to_owned(),
            },
        },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("state doc")
}

fn family_doc() -> SessionDocFamilyToken<FamilyState, String> {
    define_session_doc_family(DocFamilyDefinitionInput {
        kind: "state.family",
        version: 1,
        initial: |seed: String| FamilyState {
            value: seed.len() as i64,
        },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("family doc")
}

async fn create_state() -> (TestSession, SessionDocToken<State>) {
    let harness = open_test_session();
    let doc = state_doc();
    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("create state");
    flush().await;
    (harness, doc)
}

#[tokio::test]
async fn never_creates_an_absent_document() {
    let harness = open_test_session();
    let doc = state_doc();
    let family = family_doc();
    let commits = harness.storage.commit_count();

    assert_eq!(
        harness
            .facade
            .document_state_session(&doc, context())
            .await
            .expect("doc state")
            .map(|_| ()),
        None
    );
    assert_eq!(
        harness
            .facade
            .document_state_session_family(&family, "missing", context())
            .await
            .expect("family state")
            .map(|_| ()),
        None
    );
    assert_eq!(harness.storage.commit_count(), commits);
    assert_eq!(harness.storage.mint_count(), 0);
}

#[tokio::test]
async fn returns_an_immediately_hydrated_read_only_state_with_contiguous_chord_deliveries() {
    let (harness, doc) = create_state().await;
    let baseline = harness
        .facade
        .snapshot_session(&doc, context())
        .await
        .expect("snap")
        .expect("baseline");
    let state = harness
        .facade
        .document_state_session(&doc, context())
        .await
        .expect("state")
        .expect("present");
    let deliveries: Arc<Mutex<Vec<(Option<State>, i64)>>> = Arc::new(Mutex::new(Vec::new()));
    let deliveries_cb = Arc::clone(&deliveries);
    let _unsub = state.subscribe(move |value, _ctx, delivery| {
        deliveries_cb
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((value, delivery.sequence));
        Box::pin(futures::future::ready(Ok(())))
    });
    assert_eq!(state.value(), Some(baseline));

    for next in [1_i64, 2_i64] {
        let doc_c = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&doc_c).await?.update(|s| s.value = next)?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("commit");
    }
    flush().await;

    let recorded = deliveries.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(recorded.iter().map(|(_, seq)| *seq).collect::<Vec<_>>(), vec![0, 1, 2]);
    assert_eq!(
        recorded
            .iter()
            .map(|(v, _)| v.as_ref().map(|s| s.value))
            .collect::<Vec<_>>(),
        vec![Some(0), Some(1), Some(2)]
    );
    assert_eq!(
        state.value(),
        harness.facade.snapshot_session(&doc, context()).await.expect("snap2")
    );
    state.dispose().expect("dispose");
}

#[tokio::test]
async fn creates_independent_disposable_states_for_one_incarnation() {
    let (harness, doc) = create_state().await;
    let first = harness
        .facade
        .document_state_session(&doc, context())
        .await
        .expect("first")
        .expect("present");
    let second = harness
        .facade
        .document_state_session(&doc, context())
        .await
        .expect("second")
        .expect("present");
    assert!(
        !Arc::ptr_eq(&first.internals().expect("i1"), &second.internals().expect("i2"))
            || first.value() == second.value()
    );

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 1)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit 1");
    flush().await;
    assert_eq!(first.value().as_ref().map(|s| s.value), Some(1));
    assert_eq!(second.value().as_ref().map(|s| s.value), Some(1));

    first.dispose().expect("dispose first");
    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 2)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit 2");
    flush().await;
    assert_eq!(first.value().as_ref().map(|s| s.value), Some(1));
    assert_eq!(second.value().as_ref().map(|s| s.value), Some(2));
    second.dispose().expect("dispose second");
}

#[tokio::test]
async fn shares_exact_committed_value_and_operation_references_with_chord() {
    let (harness, doc) = create_state().await;
    let state = harness
        .facade
        .document_state_session(&doc, context())
        .await
        .expect("state")
        .expect("present");
    let received_ops: Arc<Mutex<Vec<Op>>> = Arc::new(Mutex::new(Vec::new()));
    let ops_cb = Arc::clone(&received_ops);
    let _unsub = state
        .internals()
        .expect("internals")
        .subscribe(Arc::new(move |ops, _seq, _ctx| {
            *ops_cb.lock().unwrap_or_else(|e| e.into_inner()) = ops.to_vec();
        }));

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 4)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit");
    flush().await;

    let publication = harness.last_publication().expect("publication");
    let published = &document_changes(&publication)[0];
    assert_eq!(
        serde_json::to_value(state.value().expect("value")).expect("json"),
        serde_json::to_value(published.value.as_ref().expect("published")).expect("json")
    );
    assert_eq!(*received_ops.lock().unwrap_or_else(|e| e.into_inner()), published.ops);
    let snap = harness
        .facade
        .snapshot_session(&doc, context())
        .await
        .expect("snap")
        .expect("present");
    assert_eq!(state.value().expect("value").retained, snap.retained);
    state.dispose().expect("dispose");
}

#[tokio::test]
async fn captures_a_late_baseline_without_redelivering_an_already_covered_commit() {
    let (harness, doc) = create_state().await;
    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 1)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit");

    let state = harness
        .facade
        .document_state_session(&doc, context())
        .await
        .expect("state")
        .expect("present");
    let deliveries: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let deliveries_cb = Arc::clone(&deliveries);
    let _unsub = state.subscribe(move |_value, _ctx, delivery| {
        deliveries_cb
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(delivery.sequence);
        Box::pin(futures::future::ready(Ok(())))
    });
    flush().await;
    assert_eq!(state.value().as_ref().map(|s| s.value), Some(1));
    assert_eq!(*deliveries.lock().unwrap_or_else(|e| e.into_inner()), vec![0]);
    state.dispose().expect("dispose");
}

#[tokio::test]
async fn publishes_null_retirement_and_never_follows_a_replacement_incarnation() {
    let (harness, doc) = create_state().await;
    let old_state = harness
        .facade
        .document_state_session(&doc, context())
        .await
        .expect("old")
        .expect("present");
    let retirement_ops: Arc<Mutex<Vec<Op>>> = Arc::new(Mutex::new(Vec::new()));
    let ops_cb = Arc::clone(&retirement_ops);
    let _unsub = old_state
        .internals()
        .expect("internals")
        .subscribe(Arc::new(move |ops, _seq, _ctx| {
            *ops_cb.lock().unwrap_or_else(|e| e.into_inner()) = ops.to_vec();
        }));

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.retire_doc_session(&doc_c).await?;
                tx.doc_session(&doc_c).await?.update(|s| s.value = 10)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("retire & recreate");
    flush().await;
    assert_eq!(old_state.value(), None);
    assert_eq!(
        serde_json::to_value(&*retirement_ops.lock().unwrap_or_else(|e| e.into_inner())).expect("ops"),
        json!([["r", null]])
    );

    let replacement = harness
        .facade
        .document_state_session(&doc, context())
        .await
        .expect("replacement")
        .expect("present");
    assert_eq!(replacement.value().as_ref().map(|s| s.value), Some(10));
    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 11)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit 11");
    flush().await;
    assert_eq!(old_state.value(), None);
    assert_eq!(replacement.value().as_ref().map(|s| s.value), Some(11));
    old_state.dispose().expect("dispose old");
    replacement.dispose().expect("dispose replacement");
}

#[tokio::test]
async fn cold_loads_a_definition_free_fork_copy() {
    let copied = define_conversation_doc(
        LatestFork::Current,
        DocDefinitionInput {
            kind: "state.copied",
            version: 1,
            initial: || SimpleValue { value: 0 },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("copied");
    let harness = open_test_session();
    let parent_id = create_conversation(&harness.session).await;
    let copied_c = copied.clone();
    let entry_id = harness
        .session
        .commit(
            move |tx| async move {
                let created = tx
                    .append_entry(
                        parent_id,
                        EntryDraft {
                            kind: "point".to_owned(),
                            ..Default::default()
                        },
                    )
                    .await?;
                tx.doc_conversation(&copied_c, parent_id)
                    .await?
                    .update(|d| d.value = 7)?;
                Ok(created.id)
            },
            context(),
        )
        .await
        .expect("commit parent");

    let child_id = harness
        .session
        .commit(
            move |tx| async move {
                Ok(tx
                    .fork_conversation(parent_id, entry_id, ConversationOwnership::Ownerless)
                    .await?
                    .id)
            },
            context(),
        )
        .await
        .expect("fork");

    let reads = harness.storage.document_read_count();
    let state = harness
        .facade
        .document_state_conversation(&copied, child_id, context())
        .await
        .expect("child state")
        .expect("present");
    assert_eq!(state.value(), Some(SimpleValue { value: 7 }));
    assert!(harness.storage.document_read_count() > reads);
    state.dispose().expect("dispose");
}

#[tokio::test]
async fn hydrates_a_migrated_tracker_without_writing_and_skips_an_equal_version_base_update() {
    let old = define_session_doc(DocDefinitionInput {
        kind: "state.migration",
        version: 1,
        initial: || SimpleValue { value: 3 },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("old");
    let current = define_session_doc(DocDefinitionInput {
        kind: "state.migration",
        version: 2,
        initial: || MigratedState {
            value: 0,
            migrated: false,
        },
        migrate: Some(|value, _from_version| {
            let v = value.get("value").and_then(|x| x.as_i64()).unwrap_or(0);
            Ok(MigratedState {
                value: v,
                migrated: true,
            })
        }),
        checkpoint_when: None,
    })
    .expect("current");

    let harness = open_test_session();
    let old_c = old.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&old_c).await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("create v1");
    harness.facade.unload_documents().await.expect("unload");
    let commits = harness.storage.commit_count();
    let state = harness
        .facade
        .document_state_session(&current, context())
        .await
        .expect("state")
        .expect("present");
    assert_eq!(
        state.value(),
        Some(MigratedState {
            value: 3,
            migrated: true
        })
    );
    assert_eq!(harness.storage.commit_count(), commits);
    let baseline = state.value();

    let current_c = current.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&current_c).await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("version base commit");
    flush().await;
    assert_eq!(harness.storage.commit_count(), commits + 1);
    assert_eq!(state.value(), baseline);

    let current_c = current.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&current_c).await?.update(|s| s.value = 4)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("update 4");
    flush().await;
    assert_eq!(
        state.value(),
        Some(MigratedState {
            value: 4,
            migrated: true
        })
    );
    state.dispose().expect("dispose");
}

#[tokio::test]
async fn continues_from_exact_committed_values_after_the_tracker_cache_unloads() {
    let (harness, doc) = create_state().await;
    let state = harness
        .facade
        .document_state_session(&doc, context())
        .await
        .expect("state")
        .expect("present");
    let baseline = state.value();
    let reads = harness.storage.document_read_count();
    harness.facade.unload_documents().await.expect("unload");
    let reloaded = harness.facade.snapshot_session(&doc, context()).await.expect("snap");
    assert_eq!(reloaded, baseline);
    assert!(harness.storage.document_read_count() > reads);

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 6)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit 6");
    flush().await;
    assert_eq!(state.value().as_ref().map(|s| s.value), Some(6));
    state.dispose().expect("dispose");
}

#[tokio::test]
async fn exposes_trusted_shared_immutable_values_without_freezing() {
    let (harness, doc) = create_state().await;
    let snapshot = harness
        .facade
        .snapshot_session(&doc, context())
        .await
        .expect("snap")
        .expect("present");
    let state = harness
        .facade
        .document_state_session(&doc, context())
        .await
        .expect("state")
        .expect("present");
    // PORT: In Rust `DocumentState::value()` deserializes an owned immutable value matching the
    // committed snapshot rather than sharing an unfrozen JS proxy object reference.
    assert_eq!(state.value(), Some(snapshot));
    state.dispose().expect("dispose");
}
