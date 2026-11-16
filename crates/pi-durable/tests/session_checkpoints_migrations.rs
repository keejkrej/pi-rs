//! Port of packages/durable/test/session-checkpoints-migrations.test.ts

mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_chord::delta::{Op, OpReplace, ReplaceVerb};
use pi_durable::documents::{
    DocDefinitionCallbacks, DocDefinitionInput, DocFamilyDefinitionInput, define_conversation_doc_family,
    define_rewindable_conversation_doc, define_rewindable_conversation_doc_with, define_session_doc,
    define_session_doc_with,
};
use pi_durable::types::{
    CheckpointInfo, ConversationOwnership, DocumentAddress, DocumentContent, DocumentPoint, DocumentScope, EntryDraft,
    EntryId, JsonObject, JsonValue, RewindableFork, Storage, StorageWrite,
};
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use serde_json::json;
use support::session_support::{context, create_conversation, document_changes, flush, open_test_session};

fn document_writes(writes: &[StorageWrite]) -> Vec<StorageWrite> {
    writes
        .iter()
        .filter(|write| {
            matches!(
                write,
                StorageWrite::DocumentCreate { .. }
                    | StorageWrite::DocumentChange { .. }
                    | StorageWrite::DocumentRetire { .. }
            )
        })
        .cloned()
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CountDoc {
    count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ItemsDoc {
    items: Vec<String>,
}

#[tokio::test]
async fn calls_checkpoint_when_on_definition() {
    let target_count = Arc::new(AtomicUsize::new(0));
    let target_cb = Arc::clone(&target_count);
    let doc = define_session_doc_with(DocDefinitionCallbacks {
        kind: "checkpoint.receiver",
        version: 1,
        initial: Arc::new(|| Ok(CountDoc { count: 0 })),
        migrate: None,
        checkpoint_when: Some(Arc::new(move |value, _ops, _info| {
            target_cb.fetch_add(1, Ordering::SeqCst);
            Ok(value.count == 2)
        })),
    })
    .expect("define doc");

    let harness = open_test_session();
    for count in 0..=2 {
        let doc_ref = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let draft = tx.doc_session(&doc_ref).await?;
                    draft.update(|v| v.count = count)
                },
                context(),
            )
            .await
            .expect("commit");
    }

    let kinds: Vec<&'static str> = harness
        .storage
        .commits_snapshot()
        .into_iter()
        .flatten()
        .filter_map(|write| match write {
            StorageWrite::DocumentChange {
                content: DocumentContent::Base { .. },
                ..
            } => Some("base"),
            StorageWrite::DocumentChange {
                content: DocumentContent::Delta { .. },
                ..
            } => Some("delta"),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, vec!["delta", "base"]);
}

#[tokio::test]
async fn selects_bases_only_for_nonempty_ordinary_batches_and_passes_exact_revision_and_ops() {
    #[derive(Clone, Debug)]
    struct CallRecord {
        value: JsonValue,
        ops: Vec<Op>,
    }

    let calls: Arc<Mutex<Vec<CallRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let false_calls: Arc<Mutex<Vec<CallRecord>>> = Arc::new(Mutex::new(Vec::new()));

    let calls_cb = Arc::clone(&calls);
    let base_doc = define_session_doc_with(DocDefinitionCallbacks {
        kind: "checkpoint.base",
        version: 1,
        initial: Arc::new(|| Ok(ItemsDoc { items: Vec::new() })),
        migrate: None,
        checkpoint_when: Some(Arc::new(move |value, ops, _info| {
            calls_cb.lock().unwrap_or_else(|e| e.into_inner()).push(CallRecord {
                value: serde_json::to_value(value).expect("serialize"),
                ops: ops.to_vec(),
            });
            Ok(true)
        })),
    })
    .expect("base doc");

    let false_calls_cb = Arc::clone(&false_calls);
    let delta_doc = define_session_doc_with(DocDefinitionCallbacks {
        kind: "checkpoint.delta",
        version: 1,
        initial: Arc::new(|| Ok(CountDoc { count: 0 })),
        migrate: None,
        checkpoint_when: Some(Arc::new(move |value, ops, _info| {
            false_calls_cb
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(CallRecord {
                    value: serde_json::to_value(value).expect("serialize"),
                    ops: ops.to_vec(),
                });
            Ok(false)
        })),
    })
    .expect("delta doc");

    let default_doc = define_session_doc(DocDefinitionInput {
        kind: "checkpoint.default",
        version: 1,
        initial: || CountDoc { count: 0 },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("default doc");

    let harness = open_test_session();

    {
        let b = base_doc.clone();
        let d = delta_doc.clone();
        let def = default_doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&b).await?;
                    tx.doc_session(&d).await?;
                    tx.doc_session(&def).await?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("create docs");
    }
    assert!(calls.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    assert!(false_calls.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    let first_writes = document_writes(&harness.storage.last_commit().expect("commit"));
    assert!(
        first_writes
            .iter()
            .all(|write| matches!(write, StorageWrite::DocumentCreate { .. }))
    );

    {
        let b = base_doc.clone();
        let d = delta_doc.clone();
        let def = default_doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&b).await?.update(|v| v.items.push("x".to_owned()))?;
                    tx.doc_session(&d).await?.update(|v| v.count += 1)?;
                    tx.doc_session(&def).await?.update(|v| v.count += 1)?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("update docs");
    }
    flush().await;

    let admitted_changes: Vec<StorageWrite> = harness
        .storage
        .last_admitted_commit()
        .expect("admitted")
        .into_iter()
        .filter(|w| matches!(w, StorageWrite::DocumentChange { .. }))
        .collect();
    let content_kinds: Vec<&'static str> = admitted_changes
        .iter()
        .filter_map(|w| match w {
            StorageWrite::DocumentChange {
                content: DocumentContent::Base { .. },
                ..
            } => Some("base"),
            StorageWrite::DocumentChange {
                content: DocumentContent::Delta { .. },
                ..
            } => Some("delta"),
            _ => None,
        })
        .collect();
    assert_eq!(content_kinds, vec!["base", "delta", "delta"]);

    let calls_guard = calls.lock().unwrap_or_else(|e| e.into_inner());
    let false_guard = false_calls.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls_guard.len(), 1);
    assert_eq!(false_guard.len(), 1);

    let snapshot = harness
        .facade
        .snapshot_session(&base_doc, context())
        .await
        .expect("snapshot base")
        .expect("base present");
    let delta_snapshot = harness
        .facade
        .snapshot_session(&delta_doc, context())
        .await
        .expect("snapshot delta")
        .expect("delta present");
    let last_pub = harness.last_publication().expect("pub");
    let pub_changes = document_changes(&last_pub);
    let published = &pub_changes[0];
    let published_delta = &pub_changes[1];

    assert_eq!(calls_guard[0].value, serde_json::to_value(&snapshot).expect("ser"));
    assert_eq!(calls_guard[0].ops, published.ops);
    assert_eq!(
        false_guard[0].value,
        serde_json::to_value(&delta_snapshot).expect("ser")
    );
    assert_eq!(false_guard[0].ops, published_delta.ops);

    match &admitted_changes[1] {
        StorageWrite::DocumentChange {
            content: DocumentContent::Delta { ops, .. },
            ..
        } => assert_eq!(ops, &false_guard[0].ops),
        other => panic!("Expected ordinary delta, got {other:?}"),
    }
    match &admitted_changes[0] {
        StorageWrite::DocumentChange {
            content: DocumentContent::Base { value, .. },
            ..
        } => assert_eq!(value, snapshot.items_as_json_object().expect("base obj")),
        other => panic!("Expected checkpoint base, got {other:?}"),
    }
}

trait AsJsonObjectExt {
    fn items_as_json_object(&self) -> Option<JsonObject>;
}

impl AsJsonObjectExt for ItemsDoc {
    fn items_as_json_object(&self) -> Option<JsonObject> {
        let mut map = JsonObject::new();
        map.insert("items".to_owned(), serde_json::to_value(&self.items).ok()?);
        Some(map)
    }
}

#[tokio::test]
async fn passes_stored_delta_count_since_newest_base_including_after_unload_and_version_bases() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_v1 = Arc::clone(&seen);
    let v1 = define_session_doc_with(DocDefinitionCallbacks {
        kind: "checkpoint.deltas-since-base",
        version: 1,
        initial: Arc::new(|| Ok(CountDoc { count: 0 })),
        migrate: None,
        checkpoint_when: Some(Arc::new(move |_val, _ops, info: CheckpointInfo| {
            seen_v1
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(info.deltas_since_base);
            Ok(info.deltas_since_base >= 2)
        })),
    })
    .expect("v1");

    let seen_v2 = Arc::clone(&seen);
    let v2 = define_session_doc_with(DocDefinitionCallbacks {
        kind: "checkpoint.deltas-since-base",
        version: 2,
        initial: Arc::new(|| Ok(CountDoc { count: 0 })),
        migrate: Some(Arc::new(|val, _from| {
            Ok(CountDoc {
                count: val.get("count").and_then(|v| v.as_i64()).unwrap_or(0),
            })
        })),
        checkpoint_when: Some(Arc::new(move |_val, _ops, info: CheckpointInfo| {
            seen_v2
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(info.deltas_since_base);
            Ok(false)
        })),
    })
    .expect("v2");

    let harness = open_test_session();
    {
        let token = v1.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("create");
    }

    for _ in 0..3 {
        let token = v1.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?.update(|v| v.count += 1)?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("inc v1");
    }

    harness.facade.unload_documents().await.expect("unload");
    {
        let token = v1.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?.update(|v| v.count += 1)?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("inc v1 after unload");
    }
    assert_eq!(*seen.lock().unwrap_or_else(|e| e.into_inner()), vec![0, 1, 2, 0]);

    let all_commits = harness.storage.commits_snapshot();
    let tail_kinds: Vec<&'static str> = all_commits[all_commits.len() - 4..]
        .iter()
        .map(|writes| match &writes[0] {
            StorageWrite::DocumentChange {
                content: DocumentContent::Base { .. },
                ..
            } => "base",
            StorageWrite::DocumentChange {
                content: DocumentContent::Delta { .. },
                ..
            } => "delta",
            _ => "other",
        })
        .collect();
    assert_eq!(tail_kinds, vec!["delta", "delta", "base", "delta"]);

    for _ in 0..2 {
        let token = v2.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?.update(|v| v.count += 1)?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("inc v2");
    }
    assert_eq!(*seen.lock().unwrap_or_else(|e| e.into_inner()), vec![0, 1, 2, 0, 0]);
    let all_commits = harness.storage.commits_snapshot();
    let penult = &all_commits[all_commits.len() - 2][0];
    match penult {
        StorageWrite::DocumentChange {
            content: DocumentContent::Base { version, .. },
            ..
        } => assert_eq!(*version, 2),
        other => panic!("Expected base version 2, got {other:?}"),
    }
}

#[tokio::test]
async fn skips_predicate_for_empty_batches_but_calls_it_for_nonempty_structural_noops() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_cb = Arc::clone(&calls);
    let doc = define_session_doc_with(DocDefinitionCallbacks {
        kind: "checkpoint.no-op",
        version: 1,
        initial: Arc::new(|| {
            Ok(ItemsDoc {
                items: vec!["a".to_owned(), "b".to_owned()],
            })
        }),
        migrate: None,
        checkpoint_when: Some(Arc::new(move |_val, _ops, _info| {
            calls_cb.fetch_add(1, Ordering::SeqCst);
            Ok(false)
        })),
    })
    .expect("doc");

    let harness = open_test_session();
    {
        let token = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("create");
    }
    let commits = harness.storage.commit_count();

    {
        let token = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let draft = tx.doc_session(&token).await?;
                    let overlay = draft.overlay()?;
                    let items = overlay.get("items")?.as_overlay().expect("items overlay").clone();
                    items.push(&["x".into()])?;
                    items.pop()?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("empty batch");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.storage.commit_count(), commits);

    {
        let token = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let draft = tx.doc_session(&token).await?;
                    let overlay = draft.overlay()?;
                    let items = overlay.get("items")?.as_overlay().expect("items overlay").clone();
                    let first = items.shift()?;
                    items.unshift(&[first.into()])?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("structural noop");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let last = harness.storage.last_commit().expect("last commit");
    assert!(matches!(
        last[0],
        StorageWrite::DocumentChange {
            content: DocumentContent::Delta { .. },
            ..
        }
    ));
}

#[tokio::test]
async fn rolls_back_every_prepared_document_when_checkpoint_predicate_throws() {
    let throw_checkpoint = Arc::new(AtomicBool::new(true));
    let first_calls = Arc::new(AtomicUsize::new(0));

    let first_calls_cb = Arc::clone(&first_calls);
    let first = define_session_doc_with(DocDefinitionCallbacks {
        kind: "checkpoint.rollback.first",
        version: 1,
        initial: Arc::new(|| Ok(CountDoc { count: 0 })),
        migrate: None,
        checkpoint_when: Some(Arc::new(move |_val, _ops, _info| {
            first_calls_cb.fetch_add(1, Ordering::SeqCst);
            Ok(false)
        })),
    })
    .expect("first");

    let throw_cb = Arc::clone(&throw_checkpoint);
    let second = define_session_doc_with(DocDefinitionCallbacks {
        kind: "checkpoint.rollback.second",
        version: 1,
        initial: Arc::new(|| Ok(CountDoc { count: 0 })),
        migrate: None,
        checkpoint_when: Some(Arc::new(move |_val, _ops, _info| {
            if throw_cb.load(Ordering::SeqCst) {
                return Err(pi_js::Error::msg("checkpoint failed"));
            }
            Ok(false)
        })),
    })
    .expect("second");

    let harness = open_test_session();
    {
        let f = first.clone();
        let s = second.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&f).await?;
                    tx.doc_session(&s).await?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("init");
    }
    flush().await;
    let first_snap = harness.facade.snapshot_session(&first, context()).await.expect("snap1");
    let second_snap = harness
        .facade
        .snapshot_session(&second, context())
        .await
        .expect("snap2");
    let commits = harness.storage.commit_count();
    let published = harness.publications_snapshot().len();

    {
        let f = first.clone();
        let s = second.clone();
        let err = harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&f).await?.update(|v| v.count = 1)?;
                    tx.doc_session(&s).await?.update(|v| v.count = 2)?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect_err("should fail");
        assert!(err.to_string().contains("checkpoint failed"));
    }
    flush().await;
    assert_eq!(first_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.storage.commit_count(), commits);
    assert_eq!(harness.publications_snapshot().len(), published);
    assert_eq!(
        harness.facade.snapshot_session(&first, context()).await.expect("snap1"),
        first_snap
    );
    assert_eq!(
        harness
            .facade
            .snapshot_session(&second, context())
            .await
            .expect("snap2"),
        second_snap
    );

    throw_checkpoint.store(false, Ordering::SeqCst);
    {
        let f = first.clone();
        let s = second.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&f).await?.update(|v| v.count = 3)?;
                    tx.doc_session(&s).await?.update(|v| v.count = 4)?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("commit after recovery");
    }
    assert_eq!(
        harness.facade.snapshot_session(&first, context()).await.expect("snap1"),
        Some(CountDoc { count: 3 })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_session(&second, context())
            .await
            .expect("snap2"),
        Some(CountDoc { count: 4 })
    );
}

#[tokio::test]
async fn persists_repeated_false_decisions_as_deltas_and_replays_complete_tail() {
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct ValuesDoc {
        values: Vec<i64>,
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_cb = Arc::clone(&calls);
    let doc = define_session_doc_with(DocDefinitionCallbacks {
        kind: "checkpoint.tail",
        version: 1,
        initial: Arc::new(|| Ok(ValuesDoc { values: Vec::new() })),
        migrate: None,
        checkpoint_when: Some(Arc::new(move |_val, _ops, _info| {
            calls_cb.fetch_add(1, Ordering::SeqCst);
            Ok(false)
        })),
    })
    .expect("doc");

    let harness = open_test_session();
    {
        let token = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("init");
    }
    for val in 1..=8 {
        let token = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move { tx.doc_session(&token).await?.update(|v| v.values.push(val)) },
                context(),
            )
            .await
            .expect("push");
    }
    let changes: Vec<StorageWrite> = harness
        .storage
        .commits_snapshot()
        .into_iter()
        .flatten()
        .filter(|w| matches!(w, StorageWrite::DocumentChange { .. }))
        .collect();
    assert_eq!(changes.len(), 8);
    assert!(changes.iter().all(|w| matches!(
        w,
        StorageWrite::DocumentChange {
            content: DocumentContent::Delta { .. },
            ..
        }
    )));
    assert_eq!(calls.load(Ordering::SeqCst), 8);
    harness.facade.unload_documents().await.expect("unload");
    assert_eq!(
        harness.facade.snapshot_session(&doc, context()).await.expect("snap"),
        Some(ValuesDoc {
            values: vec![1, 2, 3, 4, 5, 6, 7, 8]
        })
    );
}

#[tokio::test]
async fn keeps_prepared_root_replacement_as_delta_when_predicate_is_false() {
    let doc = define_session_doc_with(DocDefinitionCallbacks {
        kind: "checkpoint.root-replacement",
        version: 1,
        initial: Arc::new(|| {
            let mut map = serde_json::Map::new();
            for index in 0..4_100 {
                map.insert(format!("field{index}"), json!(0));
            }
            Ok(map)
        }),
        migrate: None,
        checkpoint_when: Some(Arc::new(|_val, _ops, _info| Ok(false))),
    })
    .expect("doc");

    let harness = open_test_session();
    {
        let token = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("init");
    }
    {
        let token = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let draft = tx.doc_session(&token).await?;
                    let overlay = draft.overlay()?;
                    for index in 0..4_100 {
                        overlay.set(&format!("field{index}"), 1_i64)?;
                    }
                    Ok(())
                },
                context(),
            )
            .await
            .expect("mutate");
    }
    let last = harness.storage.last_commit().expect("last commit");
    match &last[0] {
        StorageWrite::DocumentChange {
            content: DocumentContent::Delta { ops, .. },
            ..
        } => {
            assert_eq!(ops.len(), 1);
            assert!(matches!(ops[0], Op::Replace(_)));
        }
        other => panic!("Expected root replacement delta, got {other:?}"),
    }
    harness.facade.unload_documents().await.expect("unload");
    let snap = harness
        .facade
        .snapshot_session(&doc, context())
        .await
        .expect("snap")
        .expect("present");
    assert_eq!(snap.get("field4099"), Some(&json!(1)));
}

#[tokio::test]
async fn uses_ordinary_checkpoint_selection_before_retirement() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_cb = Arc::clone(&calls);
    let doc = define_session_doc_with(DocDefinitionCallbacks {
        kind: "checkpoint.retire",
        version: 1,
        initial: Arc::new(|| Ok(CountDoc { count: 0 })),
        migrate: None,
        checkpoint_when: Some(Arc::new(move |_val, _ops, _info| {
            calls_cb.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        })),
    })
    .expect("doc");

    let harness = open_test_session();
    {
        let token = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("init");
    }
    {
        let token = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?.update(|v| v.count = 1)?;
                    tx.retire_doc_session(&token).await
                },
                context(),
            )
            .await
            .expect("retire");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let writes = document_writes(&harness.storage.last_commit().expect("commit"));
    assert_eq!(writes.len(), 2);
    assert!(matches!(
        writes[0],
        StorageWrite::DocumentChange {
            content: DocumentContent::Base { .. },
            ..
        }
    ));
    assert!(matches!(writes[1], StorageWrite::DocumentRetire { .. }));
}

#[tokio::test]
async fn migrates_read_only_once_per_cold_load_copies_callback_result_and_writes_nothing() {
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct MigratedDoc {
        count: i64,
        labels: Vec<String>,
    }

    let old = define_session_doc(DocDefinitionInput {
        kind: "migration.read-only",
        version: 1,
        initial: || CountDoc { count: 2 },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("old");

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_cb = Arc::clone(&calls);
    let current = define_session_doc_with(DocDefinitionCallbacks {
        kind: "migration.read-only",
        version: 3,
        initial: Arc::new(|| {
            Ok(MigratedDoc {
                count: 0,
                labels: Vec::new(),
            })
        }),
        migrate: Some(Arc::new(move |value, from_version| {
            assert_eq!(from_version, 1);
            calls_cb.fetch_add(1, Ordering::SeqCst);
            Ok(MigratedDoc {
                count: value.get("count").and_then(|v| v.as_i64()).unwrap_or(0),
                labels: vec!["migrated".to_owned()],
            })
        })),
        checkpoint_when: None,
    })
    .expect("current");

    let harness = open_test_session();
    {
        let token = old.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("init old");
    }
    harness.facade.unload_documents().await.expect("unload");
    let commits = harness.storage.commit_count();

    let first = harness
        .facade
        .snapshot_session(&current, context())
        .await
        .expect("snap1")
        .expect("present");
    assert_eq!(
        harness
            .facade
            .snapshot_session(&current, context())
            .await
            .expect("snap2"),
        Some(first.clone())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.storage.commit_count(), commits);
    assert_eq!(
        first,
        MigratedDoc {
            count: 2,
            labels: vec!["migrated".to_owned()]
        }
    );

    harness.facade.unload_documents().await.expect("unload2");
    let second = harness
        .facade
        .snapshot_session(&current, context())
        .await
        .expect("snap3")
        .expect("present");
    assert_eq!(second, first);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(harness.storage.commit_count(), commits);
}

#[tokio::test]
async fn writes_required_base_on_first_successful_transaction_then_writes_deltas() {
    let old = define_session_doc(DocDefinitionInput {
        kind: "migration.transition",
        version: 1,
        initial: || CountDoc { count: 4 },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("old");

    let checkpoints = Arc::new(AtomicUsize::new(0));
    let cp_cb = Arc::clone(&checkpoints);
    let current = define_session_doc_with(DocDefinitionCallbacks {
        kind: "migration.transition",
        version: 3,
        initial: Arc::new(|| Ok(CountDoc { count: 0 })),
        migrate: Some(Arc::new(|value, from_version| {
            Ok(CountDoc {
                count: value.get("count").and_then(|v| v.as_i64()).unwrap_or(0) + from_version - 1,
            })
        })),
        checkpoint_when: Some(Arc::new(move |_val, _ops, _info| {
            cp_cb.fetch_add(1, Ordering::SeqCst);
            Ok(false)
        })),
    })
    .expect("current");

    let harness = open_test_session();
    {
        let token = old.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("init");
    }
    harness.facade.unload_documents().await.expect("unload");

    let watch = harness
        .facade
        .watch_doc_session(&old, context())
        .await
        .expect("watch old")
        .expect("old watch");
    let frames: Arc<Mutex<Vec<(Option<CountDoc>, Vec<Op>)>>> = Arc::new(Mutex::new(Vec::new()));
    let frames_cb = Arc::clone(&frames);
    watch
        .start(Arc::new(move |value, ops, _ctx| {
            let frames_cb = Arc::clone(&frames_cb);
            Box::pin(async move {
                frames_cb.lock().unwrap_or_else(|e| e.into_inner()).push((value, ops));
                Ok(())
            })
        }))
        .expect("start watch");

    assert_eq!(
        harness
            .facade
            .snapshot_session(&current, context())
            .await
            .expect("snap"),
        Some(CountDoc { count: 4 })
    );

    let current_watch = harness
        .facade
        .watch_doc_session(&current, context())
        .await
        .expect("watch cur")
        .expect("cur watch");
    let current_frames: Arc<Mutex<Vec<Option<CountDoc>>>> = Arc::new(Mutex::new(Vec::new()));
    let cur_cb = Arc::clone(&current_frames);
    current_watch
        .start(Arc::new(move |value, _ops, _ctx| {
            let cur_cb = Arc::clone(&cur_cb);
            Box::pin(async move {
                cur_cb.lock().unwrap_or_else(|e| e.into_inner()).push(value);
                Ok(())
            })
        }))
        .expect("start current watch");

    {
        let token = current.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("persist base");
    }
    flush().await;

    let last_commit = harness.storage.last_commit().expect("last");
    match &last_commit[0] {
        StorageWrite::DocumentChange {
            content: DocumentContent::Base { version, value },
            ..
        } => {
            assert_eq!(*version, 3);
            assert_eq!(value.get("count"), Some(&json!(4)));
        }
        other => panic!("Expected migration base, got {other:?}"),
    }
    assert_eq!(checkpoints.load(Ordering::SeqCst), 0);

    let last_pub = harness.last_publication().expect("pub");
    let doc_changes = document_changes(&last_pub);
    assert_eq!(doc_changes.len(), 1);
    assert_eq!(doc_changes[0].version, Some(3));
    assert_eq!(
        doc_changes[0].value.as_ref().and_then(|v| v.get("count")),
        Some(&json!(4))
    );
    assert!(doc_changes[0].ops.is_empty());

    assert_eq!(
        *frames.lock().unwrap_or_else(|e| e.into_inner()),
        vec![(
            Some(CountDoc { count: 4 }),
            vec![Op::Replace(OpReplace(ReplaceVerb::R, json!({ "count": 4 })))]
        )]
    );
    assert!(current_frames.lock().unwrap_or_else(|e| e.into_inner()).is_empty());

    watch.stop().await;
    current_watch.stop().await;
    assert_eq!(
        harness
            .facade
            .snapshot_session(&current, context())
            .await
            .expect("snap"),
        Some(CountDoc { count: 4 })
    );

    {
        let token = current.clone();
        harness
            .session
            .commit(
                move |tx| async move { tx.doc_session(&token).await?.update(|v| v.count = 7) },
                context(),
            )
            .await
            .expect("delta commit");
    }
    let last = harness.storage.last_commit().expect("last");
    match &last[0] {
        StorageWrite::DocumentChange {
            content: DocumentContent::Delta { version, .. },
            ..
        } => assert_eq!(*version, 3),
        other => panic!("Expected delta version 3, got {other:?}"),
    }
    assert_eq!(checkpoints.load(Ordering::SeqCst), 1);
    harness.facade.unload_documents().await.expect("unload");
    assert_eq!(
        harness
            .facade
            .snapshot_session(&current, context())
            .await
            .expect("snap"),
        Some(CountDoc { count: 7 })
    );
}

#[tokio::test]
async fn rolls_migration_and_edits_back_with_callback_then_coalesces_into_one_base() {
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Migrated {
        count: i64,
        migrated: bool,
    }

    let old = define_session_doc(DocDefinitionInput {
        kind: "migration.rollback",
        version: 1,
        initial: || CountDoc { count: 1 },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("old");

    let migrations = Arc::new(AtomicUsize::new(0));
    let mig_cb = Arc::clone(&migrations);
    let current = define_session_doc_with(DocDefinitionCallbacks {
        kind: "migration.rollback",
        version: 2,
        initial: Arc::new(|| {
            Ok(Migrated {
                count: 0,
                migrated: false,
            })
        }),
        migrate: Some(Arc::new(move |value, _from| {
            mig_cb.fetch_add(1, Ordering::SeqCst);
            Ok(Migrated {
                count: value.get("count").and_then(|v| v.as_i64()).unwrap_or(0),
                migrated: true,
            })
        })),
        checkpoint_when: None,
    })
    .expect("current");

    let harness = open_test_session();
    {
        let token = old.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("init");
    }
    harness.facade.unload_documents().await.expect("unload");
    let commits = harness.storage.commit_count();

    {
        let token = current.clone();
        let err = harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?.update(|v| v.count = 8)?;
                    Err::<(), _>(pi_js::Error::msg("rollback"))
                },
                context(),
            )
            .await
            .expect_err("rollback");
        assert!(err.to_string().contains("rollback"));
    }
    assert_eq!(harness.storage.commit_count(), commits);
    assert_eq!(
        harness
            .facade
            .snapshot_session(&current, context())
            .await
            .expect("snap"),
        Some(Migrated {
            count: 1,
            migrated: true
        })
    );
    assert_eq!(migrations.load(Ordering::SeqCst), 1);

    {
        let token = current.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?.update(|v| {
                        v.count = 9;
                        v.migrated = false;
                    })
                },
                context(),
            )
            .await
            .expect("commit");
    }
    let writes = document_writes(&harness.storage.last_commit().expect("commit"));
    assert_eq!(writes.len(), 1);
    match &writes[0] {
        StorageWrite::DocumentChange {
            content: DocumentContent::Base { version, value },
            ..
        } => {
            assert_eq!(*version, 2);
            assert_eq!(value.get("count"), Some(&json!(9)));
            assert_eq!(value.get("migrated"), Some(&json!(false)));
        }
        other => panic!("Expected base, got {other:?}"),
    }
    flush().await;
    let last_pub = harness.last_publication().expect("pub");
    let published = &document_changes(&last_pub)[0];
    let admitted = &harness.storage.last_admitted_commit().expect("admitted")[0];
    match admitted {
        StorageWrite::DocumentChange {
            content: DocumentContent::Base { value, .. },
            ..
        } => {
            assert_eq!(published.value.as_ref(), Some(value));
            assert_eq!(
                serde_json::to_value(
                    harness
                        .facade
                        .snapshot_session(&current, context())
                        .await
                        .expect("snap")
                )
                .expect("ser"),
                serde_json::to_value(published.value.as_ref()).expect("ser")
            );
        }
        other => panic!("Expected migration base, got {other:?}"),
    }
    assert!(!published.ops.is_empty());
    assert_eq!(migrations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn strict_checks_migration_results_before_tracker_ownership_and_remains_usable() {
    let old = define_session_doc(DocDefinitionInput {
        kind: "migration.invalid",
        version: 1,
        initial: || CountDoc { count: 1 },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("old");

    let invalid = define_session_doc_with(DocDefinitionCallbacks::<JsonObject> {
        kind: "migration.invalid",
        version: 2,
        initial: Arc::new(|| Ok(JsonObject::new())),
        migrate: Some(Arc::new(|_val, _from| {
            Err(pi_js::Error::js("TypeError", "Document value must be strict JSON"))
        })),
        checkpoint_when: None,
    })
    .expect("invalid");

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct OkDoc {
        ok: bool,
    }
    let other = define_session_doc(DocDefinitionInput {
        kind: "migration.invalid.other",
        version: 1,
        initial: || OkDoc { ok: true },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("other");

    let harness = open_test_session();
    {
        let token = old.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("init");
    }
    harness.facade.unload_documents().await.expect("unload");
    let commits = harness.storage.commit_count();

    let err1 = harness
        .facade
        .snapshot_session(&invalid, context())
        .await
        .expect_err("reject snap");
    assert!(err1.to_string().contains("strict JSON"));

    let inv_clone = invalid.clone();
    let err2 = harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&inv_clone).await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect_err("reject commit");
    assert!(err2.to_string().contains("strict JSON"));
    assert_eq!(harness.storage.commit_count(), commits);

    let oth_clone = other.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&oth_clone).await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("other");
    assert_eq!(
        harness.facade.snapshot_session(&other, context()).await.expect("snap"),
        Some(OkDoc { ok: true })
    );
}

#[tokio::test]
async fn rejects_newer_stored_versions_and_older_versions_without_migration() {
    let v2 = define_session_doc(DocDefinitionInput {
        kind: "migration.compatibility",
        version: 2,
        initial: || CountDoc { count: 2 },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("v2");
    let v1 = define_session_doc(DocDefinitionInput {
        kind: "migration.compatibility",
        version: 1,
        initial: || CountDoc { count: 1 },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("v1");
    let v3_no_mig = define_session_doc(DocDefinitionInput {
        kind: "migration.compatibility",
        version: 3,
        initial: || CountDoc { count: 3 },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("v3");

    let harness = open_test_session();
    {
        let token = v2.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("init");
    }
    harness.facade.unload_documents().await.expect("unload");
    let commits = harness.storage.commit_count();

    assert!(
        harness
            .facade
            .snapshot_session(&v1, context())
            .await
            .expect_err("v1 snap")
            .to_string()
            .contains("newer version 2 than 1")
    );
    let v1c = v1.clone();
    assert!(
        harness
            .session
            .commit(
                move |tx| async move { tx.doc_session(&v1c).await.map(|_| ()) },
                context()
            )
            .await
            .expect_err("v1 commit")
            .to_string()
            .contains("newer version 2 than 1")
    );
    assert!(
        harness
            .facade
            .snapshot_session(&v3_no_mig, context())
            .await
            .expect_err("v3 snap")
            .to_string()
            .contains("requires migration from version 2")
    );
    let v3c = v3_no_mig.clone();
    assert!(
        harness
            .session
            .commit(
                move |tx| async move { tx.doc_session(&v3c).await.map(|_| ()) },
                context()
            )
            .await
            .expect_err("v3 commit")
            .to_string()
            .contains("requires migration from version 2")
    );
    assert_eq!(harness.storage.commit_count(), commits);
}

#[tokio::test]
async fn persists_required_migration_base_before_retirement_without_consulting_checkpoint_predicate() {
    let old = define_session_doc(DocDefinitionInput {
        kind: "migration.retire",
        version: 1,
        initial: || CountDoc { count: 1 },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("old");
    let current = define_session_doc_with(DocDefinitionCallbacks {
        kind: "migration.retire",
        version: 2,
        initial: Arc::new(|| Ok(CountDoc { count: 0 })),
        migrate: Some(Arc::new(|val, _from| {
            Ok(CountDoc {
                count: val.get("count").and_then(|v| v.as_i64()).unwrap_or(0),
            })
        })),
        checkpoint_when: Some(Arc::new(|_val, _ops, _info| Err(pi_js::Error::msg("must not run")))),
    })
    .expect("current");

    let harness = open_test_session();
    {
        let token = old.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("init");
    }
    harness.facade.unload_documents().await.expect("unload");
    {
        let token = current.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token).await?;
                    tx.retire_doc_session(&token).await
                },
                context(),
            )
            .await
            .expect("retire");
    }
    let writes = document_writes(&harness.storage.last_commit().expect("commit"));
    assert_eq!(writes.len(), 2);
    match &writes[0] {
        StorageWrite::DocumentChange {
            content: DocumentContent::Base { version, value },
            ..
        } => {
            assert_eq!(*version, 2);
            assert_eq!(value.get("count"), Some(&json!(1)));
        }
        other => panic!("Expected migration base, got {other:?}"),
    }
    assert!(matches!(writes[1], StorageWrite::DocumentRetire { .. }));
}

#[tokio::test]
async fn leaves_unaccessed_older_documents_and_unavailable_definitions_untouched() {
    let first_v1 = define_session_doc(DocDefinitionInput {
        kind: "migration.lazy.first",
        version: 1,
        initial: || CountDoc { count: 1 },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("f1");
    let second_v1 = define_session_doc(DocDefinitionInput {
        kind: "migration.lazy.second",
        version: 1,
        initial: || CountDoc { count: 2 },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("s1");
    let first_v2 = define_session_doc(DocDefinitionInput {
        kind: "migration.lazy.first",
        version: 2,
        initial: || CountDoc { count: 0 },
        migrate: Some(|val, _from| {
            Ok(CountDoc {
                count: val.get("count").and_then(|v| v.as_i64()).unwrap_or(0),
            })
        }),
        checkpoint_when: None,
    })
    .expect("f2");
    let second_migrations = Arc::new(AtomicUsize::new(0));
    let sm_cb = Arc::clone(&second_migrations);
    let _second_v2 = define_session_doc_with(DocDefinitionCallbacks {
        kind: "migration.lazy.second",
        version: 2,
        initial: Arc::new(|| Ok(CountDoc { count: 0 })),
        migrate: Some(Arc::new(move |val, _from| {
            sm_cb.fetch_add(1, Ordering::SeqCst);
            Ok(CountDoc {
                count: val.get("count").and_then(|v| v.as_i64()).unwrap_or(0),
            })
        })),
        checkpoint_when: None,
    })
    .expect("s2");

    let harness = open_test_session();
    {
        let f1 = first_v1.clone();
        let s1 = second_v1.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&f1).await?;
                    tx.doc_session(&s1).await?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("init");
    }
    harness.facade.unload_documents().await.expect("unload");
    let commits = harness.storage.commit_count();
    assert_eq!(
        harness
            .facade
            .snapshot_session(&first_v2, context())
            .await
            .expect("snap"),
        Some(CountDoc { count: 1 })
    );
    assert_eq!(harness.storage.commit_count(), commits);
    assert_eq!(second_migrations.load(Ordering::SeqCst), 0);

    let second_rec = harness
        .storage
        .find_document(
            DocumentAddress {
                kind: "migration.lazy.second".to_owned(),
                scope: DocumentScope::Session,
                key: None,
            },
            DocumentPoint::Current,
            context(),
        )
        .await
        .expect("find")
        .expect("present");
    let stored = harness
        .storage
        .document(second_rec.id, DocumentPoint::Current, context())
        .await
        .expect("doc")
        .expect("stored");
    assert_eq!(stored.version, 1);
}

#[tokio::test]
async fn migrates_current_and_historical_rewindable_values_independently_and_follows_fork_ancestry() {
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct VersionedCount {
        count: i64,
        version: i64,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct FamilyCount {
        seed: String,
        count: i64,
    }

    let v1 = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "history.migration",
            version: 1,
            initial: || CountDoc { count: 0 },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("v1");
    let family = define_conversation_doc_family(
        RewindableFork::AsOf,
        DocFamilyDefinitionInput {
            kind: "history.family",
            version: 1,
            initial: |seed: String| FamilyCount { seed, count: 0 },
            migrate: None,
            checkpoint_when: None,
        },
    );
    // Use rewindable family!
    let family = pi_durable::documents::define_rewindable_conversation_doc_family(
        RewindableFork::AsOf,
        DocFamilyDefinitionInput {
            kind: "history.family",
            version: 1,
            initial: |seed: String| FamilyCount { seed, count: 0 },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("family");

    let migrations = Arc::new(Mutex::new(Vec::new()));
    let mig_cb = Arc::clone(&migrations);
    let v3 = define_rewindable_conversation_doc_with(
        RewindableFork::AsOf,
        DocDefinitionCallbacks {
            kind: "history.migration",
            version: 3,
            initial: Arc::new(|| Ok(VersionedCount { count: 0, version: 3 })),
            migrate: Some(Arc::new(move |val, from| {
                mig_cb.lock().unwrap_or_else(|e| e.into_inner()).push(from);
                Ok(VersionedCount {
                    count: val.get("count").and_then(|v| v.as_i64()).unwrap_or(0),
                    version: 3,
                })
            })),
            checkpoint_when: None,
        },
    )
    .expect("v3");

    let harness = open_test_session();
    let conversation_id = create_conversation(&harness.session).await;

    let first_entry: EntryId = {
        let v1c = v1.clone();
        let fam = family.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let e = tx
                        .append_entry(
                            conversation_id,
                            EntryDraft {
                                kind: "first".to_owned(),
                                ..Default::default()
                            },
                        )
                        .await?
                        .id;
                    tx.doc_conversation(&v1c, conversation_id)
                        .await?
                        .update(|d| d.count = 1)?;
                    tx.doc_conversation_family(&fam, conversation_id, "member", "seed".to_owned())
                        .await?
                        .update(|d| d.count = 1)?;
                    Ok(e)
                },
                context(),
            )
            .await
            .expect("first")
    };
    let second_entry: EntryId = {
        let v1c = v1.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let e = tx
                        .append_entry(
                            conversation_id,
                            EntryDraft {
                                kind: "second".to_owned(),
                                ..Default::default()
                            },
                        )
                        .await?
                        .id;
                    tx.doc_conversation(&v1c, conversation_id)
                        .await?
                        .update(|d| d.count = 2)?;
                    Ok(e)
                },
                context(),
            )
            .await
            .expect("second")
    };

    harness.facade.unload_documents().await.expect("unload");
    let commits = harness.storage.commit_count();
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&v3, conversation_id, context())
            .await
            .expect("snap v3"),
        Some(VersionedCount { count: 2, version: 3 })
    );
    assert_eq!(*migrations.lock().unwrap_or_else(|e| e.into_inner()), vec![1]);
    assert_eq!(harness.storage.commit_count(), commits);

    let third_entry: EntryId = {
        let v3c = v3.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let e = tx
                        .append_entry(
                            conversation_id,
                            EntryDraft {
                                kind: "third".to_owned(),
                                ..Default::default()
                            },
                        )
                        .await?
                        .id;
                    tx.doc_conversation(&v3c, conversation_id).await?;
                    Ok(e)
                },
                context(),
            )
            .await
            .expect("third")
    };
    assert!(harness.storage.last_commit().expect("last").iter().any(|w| matches!(
        w,
        StorageWrite::DocumentChange {
            content: DocumentContent::Base { version: 3, .. },
            ..
        }
    )));

    assert_eq!(
        harness
            .facade
            .snapshot_as_of(&v3, conversation_id, first_entry, context())
            .await
            .expect("e1"),
        Some(VersionedCount { count: 1, version: 3 })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_as_of(&v3, conversation_id, second_entry, context())
            .await
            .expect("e2"),
        Some(VersionedCount { count: 2, version: 3 })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_as_of(&v3, conversation_id, third_entry, context())
            .await
            .expect("e3"),
        Some(VersionedCount { count: 2, version: 3 })
    );
    assert_eq!(*migrations.lock().unwrap_or_else(|e| e.into_inner()), vec![1, 1, 1]);
    assert_eq!(
        harness
            .facade
            .snapshot_family_as_of(&family, conversation_id, "member", first_entry, context())
            .await
            .expect("fam"),
        Some(FamilyCount {
            seed: "seed".to_owned(),
            count: 1
        })
    );

    let child_id = harness
        .session
        .commit(
            move |tx| async move {
                Ok(tx
                    .fork_conversation(conversation_id, second_entry, ConversationOwnership::Ownerless)
                    .await?
                    .id)
            },
            context(),
        )
        .await
        .expect("fork");
    assert_eq!(
        harness
            .facade
            .snapshot_as_of(&v3, child_id, first_entry, context())
            .await
            .expect("c e1"),
        Some(VersionedCount { count: 1, version: 3 })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_as_of(&v3, child_id, second_entry, context())
            .await
            .expect("c e2"),
        Some(VersionedCount { count: 2, version: 3 })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_family_as_of(&family, child_id, "member", first_entry, context())
            .await
            .expect("c fam"),
        Some(FamilyCount {
            seed: "seed".to_owned(),
            count: 1
        })
    );
    let err = harness
        .facade
        .snapshot_as_of(&v3, child_id, third_entry, context())
        .await
        .expect_err("e3 invisible");
    assert!(err.to_string().contains(&format!("Entry {third_entry} is not visible")));
}

#[tokio::test]
async fn selects_incarnation_alive_at_entry_commit_across_retirement_and_recreation() {
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct ValueDoc {
        value: String,
    }

    let doc = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "history.incarnation",
            version: 1,
            initial: || ValueDoc {
                value: "initial".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("doc");

    let harness = open_test_session();
    let conversation_id = create_conversation(&harness.session).await;

    let before_creation = harness
        .session
        .commit(
            move |tx| async move {
                Ok(tx
                    .append_entry(
                        conversation_id,
                        EntryDraft {
                            kind: "before".to_owned(),
                            ..Default::default()
                        },
                    )
                    .await?
                    .id)
            },
            context(),
        )
        .await
        .expect("before");

    let created_at = {
        let d = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let e = tx
                        .append_entry(
                            conversation_id,
                            EntryDraft {
                                kind: "create".to_owned(),
                                ..Default::default()
                            },
                        )
                        .await?
                        .id;
                    tx.doc_conversation(&d, conversation_id)
                        .await?
                        .update(|v| v.value = "old".to_owned())?;
                    Ok(e)
                },
                context(),
            )
            .await
            .expect("create")
    };

    let retired_at = {
        let d = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let e = tx
                        .append_entry(
                            conversation_id,
                            EntryDraft {
                                kind: "retire".to_owned(),
                                ..Default::default()
                            },
                        )
                        .await?
                        .id;
                    tx.retire_doc_conversation(&d, conversation_id).await?;
                    Ok(e)
                },
                context(),
            )
            .await
            .expect("retire")
    };

    let recreated_at = {
        let d = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let e = tx
                        .append_entry(
                            conversation_id,
                            EntryDraft {
                                kind: "recreate".to_owned(),
                                ..Default::default()
                            },
                        )
                        .await?
                        .id;
                    tx.doc_conversation(&d, conversation_id)
                        .await?
                        .update(|v| v.value = "new".to_owned())?;
                    Ok(e)
                },
                context(),
            )
            .await
            .expect("recreate")
    };

    assert_eq!(
        harness
            .facade
            .snapshot_as_of(&doc, conversation_id, before_creation, context())
            .await
            .expect("before"),
        None
    );
    assert_eq!(
        harness
            .facade
            .snapshot_as_of(&doc, conversation_id, created_at, context())
            .await
            .expect("created"),
        Some(ValueDoc {
            value: "old".to_owned()
        })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_as_of(&doc, conversation_id, retired_at, context())
            .await
            .expect("retired"),
        None
    );
    assert_eq!(
        harness
            .facade
            .snapshot_as_of(&doc, conversation_id, recreated_at, context())
            .await
            .expect("recreated"),
        Some(ValueDoc {
            value: "new".to_owned()
        })
    );

    harness.facade.close(context()).await.expect("close");
    let err = harness
        .facade
        .snapshot_as_of(&doc, conversation_id, recreated_at, context())
        .await
        .expect_err("closed");
    assert!(err.to_string().contains("closed"));
}

#[tokio::test]
async fn migrates_document_cached_by_older_token_without_unloading() {
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct V1 {
        name: String,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct V2 {
        names: Vec<String>,
    }

    let v1_doc = define_session_doc(DocDefinitionInput {
        kind: "cache.versioned",
        version: 1,
        initial: || V1 {
            name: "first".to_owned(),
        },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("v1");
    let v2_doc = define_session_doc(DocDefinitionInput {
        kind: "cache.versioned",
        version: 2,
        initial: || V2 { names: Vec::new() },
        migrate: Some(|val, _from| {
            let name = val.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
            Ok(V2 { names: vec![name] })
        }),
        checkpoint_when: None,
    })
    .expect("v2");

    let harness = open_test_session();
    {
        let token = v1_doc.clone();
        harness
            .session
            .commit(
                move |tx| async move { tx.doc_session(&token).await.map(|_| ()) },
                context(),
            )
            .await
            .expect("init");
    }
    assert_eq!(
        harness
            .facade
            .snapshot_session(&v1_doc, context())
            .await
            .expect("snap1"),
        Some(V1 {
            name: "first".to_owned()
        })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_session(&v2_doc, context())
            .await
            .expect("snap2"),
        Some(V2 {
            names: vec!["first".to_owned()]
        })
    );
    {
        let token = v2_doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token)
                        .await?
                        .update(|v| v.names.push("second".to_owned()))
                },
                context(),
            )
            .await
            .expect("push");
    }
    let write = document_writes(&harness.storage.last_commit().expect("commit"))[0].clone();
    match write {
        StorageWrite::DocumentChange {
            content: DocumentContent::Base { version, value },
            ..
        } => {
            assert_eq!(version, 2);
            assert_eq!(value.get("names"), Some(&json!(["first", "second"])));
        }
        other => panic!("Expected base v2, got {other:?}"),
    }
    let err = harness
        .facade
        .snapshot_session(&v1_doc, context())
        .await
        .expect_err("newer");
    assert!(err.to_string().contains("newer version 2"));
}

#[tokio::test]
async fn sends_observers_of_older_shape_root_replacement_after_newer_token_writes() {
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct V1 {
        name: String,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct V2 {
        names: Vec<String>,
    }

    let v1_doc = define_session_doc(DocDefinitionInput {
        kind: "cache.versioned",
        version: 1,
        initial: || V1 {
            name: "first".to_owned(),
        },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("v1");
    let v2_doc = define_session_doc(DocDefinitionInput {
        kind: "cache.versioned",
        version: 2,
        initial: || V2 { names: Vec::new() },
        migrate: Some(|val, _from| {
            let name = val.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
            Ok(V2 { names: vec![name] })
        }),
        checkpoint_when: None,
    })
    .expect("v2");

    let harness = open_test_session();
    {
        let token = v1_doc.clone();
        harness
            .session
            .commit(
                move |tx| async move { tx.doc_session(&token).await.map(|_| ()) },
                context(),
            )
            .await
            .expect("init");
    }
    let state = harness
        .facade
        .document_state_session(&v1_doc, context())
        .await
        .expect("state")
        .expect("present");
    let watch = harness
        .facade
        .watch_doc_session(&v1_doc, context())
        .await
        .expect("watch")
        .expect("present");
    let frames: Arc<Mutex<Vec<Vec<Op>>>> = Arc::new(Mutex::new(Vec::new()));
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let done_tx = Arc::new(Mutex::new(Some(done_tx)));
    let frames_cb = Arc::clone(&frames);
    watch
        .start(Arc::new(move |_val, ops, _ctx| {
            let frames_cb = Arc::clone(&frames_cb);
            let done_tx = Arc::clone(&done_tx);
            Box::pin(async move {
                frames_cb.lock().unwrap_or_else(|e| e.into_inner()).push(ops);
                if let Some(tx) = done_tx.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    let _ = tx.send(());
                }
                Ok(())
            })
        }))
        .expect("start");

    {
        let token = v2_doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&token)
                        .await?
                        .update(|v| v.names.push("second".to_owned()))
                },
                context(),
            )
            .await
            .expect("write v2");
    }
    done_rx.await.expect("delivered");
    assert_eq!(state.as_raw().value(), json!({ "names": ["first", "second"] }));
    let raw_watch = harness
        .session
        .watch_erased(&v1_doc.definition, None, None, None, context())
        .await;
    drop(raw_watch);
    assert_eq!(
        *frames.lock().unwrap_or_else(|e| e.into_inner()),
        vec![vec![Op::Replace(OpReplace(
            ReplaceVerb::R,
            json!({ "names": ["first", "second"] })
        ))]]
    );
    state.dispose().expect("dispose");
    watch.stop().await;
}

#[tokio::test]
async fn serves_older_token_from_storage_after_newer_token_migrated_only_in_memory() {
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct V1 {
        name: String,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct V2 {
        names: Vec<String>,
    }

    let v1_doc = define_session_doc(DocDefinitionInput {
        kind: "cache.versioned",
        version: 1,
        initial: || V1 {
            name: "first".to_owned(),
        },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("v1");
    let v2_doc = define_session_doc(DocDefinitionInput {
        kind: "cache.versioned",
        version: 2,
        initial: || V2 { names: Vec::new() },
        migrate: Some(|val, _from| {
            let name = val.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
            Ok(V2 { names: vec![name] })
        }),
        checkpoint_when: None,
    })
    .expect("v2");

    let harness = open_test_session();
    {
        let token = v1_doc.clone();
        harness
            .session
            .commit(
                move |tx| async move { tx.doc_session(&token).await.map(|_| ()) },
                context(),
            )
            .await
            .expect("init");
    }
    harness.facade.unload_documents().await.expect("unload");
    assert_eq!(
        harness
            .facade
            .snapshot_session(&v2_doc, context())
            .await
            .expect("snap2"),
        Some(V2 {
            names: vec!["first".to_owned()]
        })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_session(&v1_doc, context())
            .await
            .expect("snap1"),
        Some(V1 {
            name: "first".to_owned()
        })
    );
    {
        let token = v1_doc.clone();
        harness
            .session
            .commit(
                move |tx| async move { tx.doc_session(&token).await?.update(|v| v.name = "renamed".to_owned()) },
                context(),
            )
            .await
            .expect("rename");
    }
    assert_eq!(
        harness
            .facade
            .snapshot_session(&v2_doc, context())
            .await
            .expect("snap2b"),
        Some(V2 {
            names: vec!["renamed".to_owned()]
        })
    );
}
