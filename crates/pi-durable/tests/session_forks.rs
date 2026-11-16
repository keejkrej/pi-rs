//! Port of packages/durable/test/session-forks.test.ts

mod support;

use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use indexmap::IndexMap;
use pi_durable::documents::{
    DocDefinitionCallbacks, DocDefinitionInput, DocFamilyDefinitionInput, define_conversation_doc,
    define_conversation_doc_family, define_rewindable_conversation_doc, define_rewindable_conversation_doc_family,
    define_rewindable_conversation_doc_with, define_session_doc, define_task_doc,
};
use pi_durable::errors::StorageRejected;
use pi_durable::tasks::{TaskDefinition, define_task};
use pi_durable::types::{
    ConversationOwnership, DocumentAddress, DocumentCreate, DocumentPoint, DocumentScope, EntryDraft, EntryQuery,
    LatestFork, RewindableFork, Storage, StorageWrite, TaskOptions, TaskOwnership,
};
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use support::session_support::{
    context, create_conversation, document_changes, document_copy_changes, flush, open_test_session,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StrDoc {
    value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CountDoc {
    count: i64,
}

fn document_creates(writes: &[StorageWrite]) -> Vec<(DocumentCreate, i64, pi_durable::types::JsonObject)> {
    writes
        .iter()
        .filter_map(|write| match write {
            StorageWrite::DocumentCreate { record, version, value } => Some((record.clone(), *version, value.clone())),
            _ => None,
        })
        .collect()
}

fn document_copies(writes: &[StorageWrite]) -> Vec<(DocumentCreate, pi_durable::types::DocumentCopySource)> {
    writes
        .iter()
        .filter_map(|write| match write {
            StorageWrite::DocumentCopy { record, source } => Some((record.clone(), *source)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn copies_as_of_and_current_singleton_and_family_bases_while_leaving_initial_absent() {
    let as_of = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "fork.policies.as-of",
            version: 1,
            initial: || StrDoc {
                value: "as-initial".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("as_of");
    let current = define_conversation_doc(
        LatestFork::Current,
        DocDefinitionInput {
            kind: "fork.policies.current",
            version: 1,
            initial: || StrDoc {
                value: "current-initial".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("current");
    let initial = define_conversation_doc(
        LatestFork::Initial,
        DocDefinitionInput {
            kind: "fork.policies.initial",
            version: 1,
            initial: || StrDoc {
                value: "fresh".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("initial");
    let as_of_family = define_rewindable_conversation_doc_family(
        RewindableFork::AsOf,
        DocFamilyDefinitionInput {
            kind: "fork.policies.as-of-family",
            version: 1,
            initial: |seed: String| StrDoc { value: seed },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("as_of_family");
    let current_family = define_conversation_doc_family(
        LatestFork::Current,
        DocFamilyDefinitionInput {
            kind: "fork.policies.current-family",
            version: 1,
            initial: |seed: String| StrDoc { value: seed },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("current_family");

    let harness = open_test_session();
    let parent_id = create_conversation(&harness.session).await;

    let fork_at = {
        let ao = as_of.clone();
        let cu = current.clone();
        let ini = initial.clone();
        let aof = as_of_family.clone();
        let cuf = current_family.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let e = tx
                        .append_entry(
                            parent_id,
                            EntryDraft {
                                kind: "fork-point".to_owned(),
                                ..Default::default()
                            },
                        )
                        .await?
                        .id;
                    tx.doc_conversation(&ao, parent_id)
                        .await?
                        .update(|d| d.value = "as-at-fork".to_owned())?;
                    tx.doc_conversation(&cu, parent_id)
                        .await?
                        .update(|d| d.value = "current-at-fork".to_owned())?;
                    tx.doc_conversation(&ini, parent_id)
                        .await?
                        .update(|d| d.value = "parent-only".to_owned())?;
                    tx.doc_conversation_family(&aof, parent_id, "a", "unused".to_owned())
                        .await?
                        .update(|d| d.value = "family-as-a".to_owned())?;
                    tx.doc_conversation_family(&aof, parent_id, "b", "unused".to_owned())
                        .await?
                        .update(|d| d.value = "family-as-b".to_owned())?;
                    tx.doc_conversation_family(&cuf, parent_id, "a", "unused".to_owned())
                        .await?
                        .update(|d| d.value = "family-current-a".to_owned())?;
                    Ok(e)
                },
                context(),
            )
            .await
            .expect("commit 1")
    };

    {
        let ao = as_of.clone();
        let cu = current.clone();
        let aof = as_of_family.clone();
        let cuf = current_family.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_conversation(&ao, parent_id)
                        .await?
                        .update(|d| d.value = "as-after-fork".to_owned())?;
                    tx.doc_conversation(&cu, parent_id)
                        .await?
                        .update(|d| d.value = "current-when-copied".to_owned())?;
                    tx.doc_conversation_family(&aof, parent_id, "a", "unused".to_owned())
                        .await?
                        .update(|d| d.value = "family-as-after".to_owned())?;
                    tx.doc_conversation_family(&cuf, parent_id, "a", "unused".to_owned())
                        .await?
                        .update(|d| d.value = "family-current-when-copied".to_owned())?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("commit 2")
    };
    flush().await;
    let document_reads = harness.storage.document_read_count();

    let child = harness
        .session
        .commit(
            move |tx| async move {
                tx.fork_conversation(parent_id, fork_at, ConversationOwnership::Ownerless)
                    .await
            },
            context(),
        )
        .await
        .expect("fork");
    flush().await;
    assert_eq!(harness.storage.document_read_count(), document_reads);

    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&as_of, child.id, context())
            .await
            .expect("s1"),
        Some(StrDoc {
            value: "as-at-fork".to_owned()
        })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&current, child.id, context())
            .await
            .expect("s2"),
        Some(StrDoc {
            value: "current-when-copied".to_owned()
        })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&initial, child.id, context())
            .await
            .expect("s3"),
        None
    );
    assert_eq!(
        harness
            .facade
            .snapshot_conversation_family(&as_of_family, child.id, "a", context())
            .await
            .expect("s4"),
        Some(StrDoc {
            value: "family-as-a".to_owned()
        })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_conversation_family(&as_of_family, child.id, "b", context())
            .await
            .expect("s5"),
        Some(StrDoc {
            value: "family-as-b".to_owned()
        })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_conversation_family(&current_family, child.id, "a", context())
            .await
            .expect("s6"),
        Some(StrDoc {
            value: "family-current-when-copied".to_owned()
        })
    );

    let admitted = harness.storage.last_admitted_commit().expect("admitted");
    let copies = document_copies(&admitted);
    assert_eq!(copies.len(), 5);
    assert!(copies.iter().all(|(rec, _)| rec.scope
        == DocumentScope::Conversation {
            conversation_id: child.id
        }));

    let pub_item = harness.last_publication().expect("pub");
    let copied = document_copy_changes(&pub_item);
    assert_eq!(copied.len(), 5);
    for change in &copied {
        assert_eq!(change.record.created_at, pub_item.seq);
        assert_eq!(change.conversation_id, child.id);
        let (_, src) = copies
            .iter()
            .find(|(rec, _)| rec.id == change.record.id)
            .expect("matching write");
        assert_eq!(&change.source, src);
    }

    let parent_rec = harness
        .storage
        .find_document(
            DocumentAddress {
                kind: as_of.definition.kind.clone(),
                scope: DocumentScope::Conversation {
                    conversation_id: parent_id,
                },
                key: None,
            },
            DocumentPoint::Current,
            context(),
        )
        .await
        .expect("find")
        .expect("parent rec");
    let child_rec = harness
        .storage
        .find_document(
            DocumentAddress {
                kind: as_of.definition.kind.clone(),
                scope: DocumentScope::Conversation {
                    conversation_id: child.id,
                },
                key: None,
            },
            DocumentPoint::Current,
            context(),
        )
        .await
        .expect("find")
        .expect("child rec");
    assert_ne!(child_rec.id, parent_rec.id);

    let ao_c = as_of.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&ao_c, child.id)
                    .await?
                    .update(|d| d.value = "child-independent".to_owned())
            },
            context(),
        )
        .await
        .expect("child update");
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&as_of, parent_id, context())
            .await
            .expect("p"),
        Some(StrDoc {
            value: "as-after-fork".to_owned()
        })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&as_of, child.id, context())
            .await
            .expect("c"),
        Some(StrDoc {
            value: "child-independent".to_owned()
        })
    );

    let ini_c = initial.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&ini_c, child.id)
                    .await?
                    .update(|d| d.value = "child-created".to_owned())
            },
            context(),
        )
        .await
        .expect("child initial");
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&initial, child.id, context())
            .await
            .expect("c ini"),
        Some(StrDoc {
            value: "child-created".to_owned()
        })
    );
}

#[tokio::test]
async fn uses_final_document_state_from_fork_entry_commit_while_excluding_later_same_commit_entries() {
    let doc = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "fork.same-commit",
            version: 1,
            initial: || StrDoc {
                value: "initial".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("doc");

    let harness = open_test_session();
    let parent_id = create_conversation(&harness.session).await;
    let doc_c = doc.clone();
    let (fork_at, excluded) = harness
        .session
        .commit(
            move |tx| async move {
                let f = tx
                    .append_entry(
                        parent_id,
                        EntryDraft {
                            kind: "included".to_owned(),
                            ..Default::default()
                        },
                    )
                    .await?
                    .id;
                tx.doc_conversation(&doc_c, parent_id)
                    .await?
                    .update(|d| d.value = "final-state-of-commit".to_owned())?;
                let ex = tx
                    .append_entry(
                        parent_id,
                        EntryDraft {
                            kind: "excluded".to_owned(),
                            ..Default::default()
                        },
                    )
                    .await?
                    .id;
                Ok((f, ex))
            },
            context(),
        )
        .await
        .expect("parent commit");

    let child = harness
        .session
        .commit(
            move |tx| async move {
                tx.fork_conversation(parent_id, fork_at, ConversationOwnership::Ownerless)
                    .await
            },
            context(),
        )
        .await
        .expect("fork");
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, child.id, context())
            .await
            .expect("snap"),
        Some(StrDoc {
            value: "final-state-of-commit".to_owned()
        })
    );
    let visible = harness
        .session
        .commit(
            move |tx| async move {
                tx.scan_entries(
                    EntryQuery {
                        conversation_id: child.id,
                        min_entry_id: None,
                        max_entry_id: None,
                    },
                    10,
                    None,
                )
                .await
            },
            context(),
        )
        .await
        .expect("scan");
    let visible_ids: Vec<_> = visible.items.into_iter().map(|e| e.id).collect();
    assert!(visible_ids.contains(&fork_at));
    assert!(!visible_ids.contains(&excluded));
}

#[tokio::test]
async fn selects_entry_owning_ancestor_for_as_of_and_immediate_parent_for_current() {
    let as_of = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "fork.ancestry.as-of",
            version: 1,
            initial: || StrDoc {
                value: "initial".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("as_of");
    let current = define_conversation_doc(
        LatestFork::Current,
        DocDefinitionInput {
            kind: "fork.ancestry.current",
            version: 1,
            initial: || StrDoc {
                value: "initial".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("current");

    let harness = open_test_session();
    let root_id = create_conversation(&harness.session).await;

    let inherited = {
        let ao = as_of.clone();
        let cu = current.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let e = tx
                        .append_entry(
                            root_id,
                            EntryDraft {
                                kind: "root".to_owned(),
                                ..Default::default()
                            },
                        )
                        .await?
                        .id;
                    tx.doc_conversation(&ao, root_id)
                        .await?
                        .update(|d| d.value = "root-at-entry".to_owned())?;
                    tx.doc_conversation(&cu, root_id)
                        .await?
                        .update(|d| d.value = "root-current".to_owned())?;
                    Ok(e)
                },
                context(),
            )
            .await
            .expect("root")
    };

    let parent = harness
        .session
        .commit(
            move |tx| async move {
                tx.fork_conversation(root_id, inherited, ConversationOwnership::Ownerless)
                    .await
            },
            context(),
        )
        .await
        .expect("fork 1");

    let parent_entry = {
        let ao = as_of.clone();
        let cu = current.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let e = tx
                        .append_entry(
                            parent.id,
                            EntryDraft {
                                kind: "parent".to_owned(),
                                ..Default::default()
                            },
                        )
                        .await?
                        .id;
                    tx.doc_conversation(&ao, parent.id)
                        .await?
                        .update(|d| d.value = "parent-at-own-entry".to_owned())?;
                    tx.doc_conversation(&cu, parent.id)
                        .await?
                        .update(|d| d.value = "parent-current".to_owned())?;
                    Ok(e)
                },
                context(),
            )
            .await
            .expect("parent commit")
    };

    let inherited_fork = harness
        .session
        .commit(
            move |tx| async move {
                tx.fork_conversation(parent.id, inherited, ConversationOwnership::Ownerless)
                    .await
            },
            context(),
        )
        .await
        .expect("fork 2");
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&as_of, inherited_fork.id, context())
            .await
            .expect("s1"),
        Some(StrDoc {
            value: "root-at-entry".to_owned()
        })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&current, inherited_fork.id, context())
            .await
            .expect("s2"),
        Some(StrDoc {
            value: "parent-current".to_owned()
        })
    );

    let own_entry_fork = harness
        .session
        .commit(
            move |tx| async move {
                tx.fork_conversation(parent.id, parent_entry, ConversationOwnership::Ownerless)
                    .await
            },
            context(),
        )
        .await
        .expect("fork 3");
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&as_of, own_entry_fork.id, context())
            .await
            .expect("s3"),
        Some(StrDoc {
            value: "parent-at-own-entry".to_owned()
        })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&current, own_entry_fork.id, context())
            .await
            .expect("s4"),
        Some(StrDoc {
            value: "parent-current".to_owned()
        })
    );
}

#[tokio::test]
async fn copies_stored_value_and_version_without_consulting_migration_definitions_or_migrated_caches() {
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct V3Doc {
        count: i64,
        migrated: bool,
    }

    let v1 = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "fork.stored-version",
            version: 1,
            initial: || CountDoc { count: 1 },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("v1");
    let migrations = Arc::new(AtomicUsize::new(0));
    let mig_cb = Arc::clone(&migrations);
    let v3 = define_rewindable_conversation_doc_with(
        RewindableFork::AsOf,
        DocDefinitionCallbacks {
            kind: "fork.stored-version",
            version: 3,
            initial: Arc::new(|| {
                Ok(V3Doc {
                    count: 0,
                    migrated: false,
                })
            }),
            migrate: Some(Arc::new(move |val, from| {
                assert_eq!(from, 1);
                mig_cb.fetch_add(1, Ordering::SeqCst);
                Ok(V3Doc {
                    count: val.get("count").and_then(|v| v.as_i64()).unwrap_or(0),
                    migrated: true,
                })
            })),
            checkpoint_when: None,
        },
    )
    .expect("v3");

    let harness = open_test_session();
    let parent_id = create_conversation(&harness.session).await;
    let v1_c = v1.clone();
    let fork_at = harness
        .session
        .commit(
            move |tx| async move {
                let e = tx
                    .append_entry(
                        parent_id,
                        EntryDraft {
                            kind: "point".to_owned(),
                            ..Default::default()
                        },
                    )
                    .await?
                    .id;
                tx.doc_conversation(&v1_c, parent_id).await?;
                Ok(e)
            },
            context(),
        )
        .await
        .expect("init");
    harness.facade.unload_documents().await.expect("unload");
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&v3, parent_id, context())
            .await
            .expect("snap"),
        Some(V3Doc {
            count: 1,
            migrated: true
        })
    );
    assert_eq!(migrations.load(Ordering::SeqCst), 1);

    let child = harness
        .session
        .commit(
            move |tx| async move {
                tx.fork_conversation(parent_id, fork_at, ConversationOwnership::Ownerless)
                    .await
            },
            context(),
        )
        .await
        .expect("fork");
    assert_eq!(migrations.load(Ordering::SeqCst), 1);
    let child_rec = harness
        .storage
        .find_document(
            DocumentAddress {
                kind: v1.definition.kind.clone(),
                scope: DocumentScope::Conversation {
                    conversation_id: child.id,
                },
                key: None,
            },
            DocumentPoint::Current,
            context(),
        )
        .await
        .expect("find")
        .expect("present");
    let child_stored = harness
        .storage
        .document(child_rec.id, DocumentPoint::Current, context())
        .await
        .expect("doc")
        .expect("present");
    assert_eq!(child_stored.version, 1);
    assert_eq!(child_stored.value.get("count"), Some(&serde_json::json!(1)));
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&v3, child.id, context())
            .await
            .expect("snap c"),
        Some(V3Doc {
            count: 1,
            migrated: true
        })
    );
    assert_eq!(migrations.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn coalesces_typed_migration_and_override_into_copied_creation_base() {
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct V2Doc {
        count: i64,
        migrated: bool,
    }

    let v1 = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "fork.override",
            version: 1,
            initial: || CountDoc { count: 2 },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("v1");
    let checkpoints = Arc::new(AtomicUsize::new(0));
    let cp_cb = Arc::clone(&checkpoints);
    let v2 = define_rewindable_conversation_doc_with(
        RewindableFork::AsOf,
        DocDefinitionCallbacks {
            kind: "fork.override",
            version: 2,
            initial: Arc::new(|| {
                Ok(V2Doc {
                    count: 0,
                    migrated: false,
                })
            }),
            migrate: Some(Arc::new(|val, _from| {
                Ok(V2Doc {
                    count: val.get("count").and_then(|v| v.as_i64()).unwrap_or(0),
                    migrated: true,
                })
            })),
            checkpoint_when: Some(Arc::new(move |_val, _ops, _info| {
                cp_cb.fetch_add(1, Ordering::SeqCst);
                Ok(false)
            })),
        },
    )
    .expect("v2");

    let harness = open_test_session();
    let parent_id = create_conversation(&harness.session).await;
    let v1_c = v1.clone();
    let fork_at = harness
        .session
        .commit(
            move |tx| async move {
                let e = tx
                    .append_entry(
                        parent_id,
                        EntryDraft {
                            kind: "point".to_owned(),
                            ..Default::default()
                        },
                    )
                    .await?
                    .id;
                tx.doc_conversation(&v1_c, parent_id).await?;
                Ok(e)
            },
            context(),
        )
        .await
        .expect("init");

    let document_reads = harness.storage.document_read_count();
    let v2_c = v2.clone();
    let child = harness
        .session
        .commit(
            move |tx| async move {
                let created = tx
                    .fork_conversation(parent_id, fork_at, ConversationOwnership::Ownerless)
                    .await?;
                tx.doc_conversation(&v2_c, created.id).await?.update(|v| v.count = 9)?;
                Ok(created)
            },
            context(),
        )
        .await
        .expect("fork override");
    flush().await;
    assert_eq!(harness.storage.document_read_count(), document_reads + 1);
    let admitted = harness.storage.last_admitted_commit().expect("admitted");
    let creates = document_creates(&admitted);
    assert_eq!(creates.len(), 1);
    assert_eq!(creates[0].1, 2);
    assert_eq!(creates[0].2.get("count"), Some(&serde_json::json!(9)));
    assert_eq!(creates[0].2.get("migrated"), Some(&serde_json::json!(true)));
    assert_eq!(checkpoints.load(Ordering::SeqCst), 0);
    assert_eq!(
        document_changes(&harness.last_publication().expect("pub"))[0].version,
        Some(2)
    );
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&v2, child.id, context())
            .await
            .expect("snap"),
        Some(V2Doc {
            count: 9,
            migrated: true
        })
    );
    let parent_rec = harness
        .storage
        .find_document(
            DocumentAddress {
                kind: v1.definition.kind.clone(),
                scope: DocumentScope::Conversation {
                    conversation_id: parent_id,
                },
                key: None,
            },
            DocumentPoint::Current,
            context(),
        )
        .await
        .expect("find")
        .expect("present");
    let parent_stored = harness
        .storage
        .document(parent_rec.id, DocumentPoint::Current, context())
        .await
        .expect("doc")
        .expect("present");
    assert_eq!(parent_stored.version, 1);
}

#[tokio::test]
async fn copies_incarnation_alive_at_fork_point_across_retirement_and_recreation() {
    let doc = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "fork.incarnations",
            version: 1,
            initial: || StrDoc {
                value: "initial".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("doc");

    let harness = open_test_session();
    let parent_id = create_conversation(&harness.session).await;

    let old_at = {
        let d = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let e = tx
                        .append_entry(
                            parent_id,
                            EntryDraft {
                                kind: "old".to_owned(),
                                ..Default::default()
                            },
                        )
                        .await?
                        .id;
                    tx.doc_conversation(&d, parent_id)
                        .await?
                        .update(|v| v.value = "old".to_owned())?;
                    Ok(e)
                },
                context(),
            )
            .await
            .expect("old")
    };
    let retired_at = {
        let d = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let e = tx
                        .append_entry(
                            parent_id,
                            EntryDraft {
                                kind: "retired".to_owned(),
                                ..Default::default()
                            },
                        )
                        .await?
                        .id;
                    tx.retire_doc_conversation(&d, parent_id).await?;
                    Ok(e)
                },
                context(),
            )
            .await
            .expect("retired")
    };
    let new_at = {
        let d = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    let e = tx
                        .append_entry(
                            parent_id,
                            EntryDraft {
                                kind: "new".to_owned(),
                                ..Default::default()
                            },
                        )
                        .await?
                        .id;
                    tx.doc_conversation(&d, parent_id)
                        .await?
                        .update(|v| v.value = "new".to_owned())?;
                    Ok(e)
                },
                context(),
            )
            .await
            .expect("new")
    };

    let old_child = harness
        .session
        .commit(
            move |tx| async move {
                tx.fork_conversation(parent_id, old_at, ConversationOwnership::Ownerless)
                    .await
            },
            context(),
        )
        .await
        .expect("old child");
    let empty_child = harness
        .session
        .commit(
            move |tx| async move {
                tx.fork_conversation(parent_id, retired_at, ConversationOwnership::Ownerless)
                    .await
            },
            context(),
        )
        .await
        .expect("empty child");
    let new_child = harness
        .session
        .commit(
            move |tx| async move {
                tx.fork_conversation(parent_id, new_at, ConversationOwnership::Ownerless)
                    .await
            },
            context(),
        )
        .await
        .expect("new child");

    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, old_child.id, context())
            .await
            .expect("old"),
        Some(StrDoc {
            value: "old".to_owned()
        })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, empty_child.id, context())
            .await
            .expect("empty"),
        None
    );
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, new_child.id, context())
            .await
            .expect("new"),
        Some(StrDoc {
            value: "new".to_owned()
        })
    );
}

#[tokio::test]
async fn rejects_invisible_fork_points_before_admission_and_remains_usable() {
    let harness = open_test_session();
    let root_id = create_conversation(&harness.session).await;
    let (visible, hidden) = harness
        .session
        .commit(
            move |tx| async move {
                let v = tx
                    .append_entry(
                        root_id,
                        EntryDraft {
                            kind: "visible".to_owned(),
                            ..Default::default()
                        },
                    )
                    .await?
                    .id;
                let h = tx
                    .append_entry(
                        root_id,
                        EntryDraft {
                            kind: "hidden".to_owned(),
                            ..Default::default()
                        },
                    )
                    .await?
                    .id;
                Ok((v, h))
            },
            context(),
        )
        .await
        .expect("entries");
    let parent = harness
        .session
        .commit(
            move |tx| async move {
                tx.fork_conversation(root_id, visible, ConversationOwnership::Ownerless)
                    .await
            },
            context(),
        )
        .await
        .expect("parent");
    flush().await;
    let commits = harness.storage.commit_count();
    let published = harness.publications_snapshot().len();

    let err = harness
        .session
        .commit(
            move |tx| async move {
                tx.fork_conversation(parent.id, hidden, ConversationOwnership::Ownerless)
                    .await
            },
            context(),
        )
        .await
        .expect_err("invisible");
    assert!(err.to_string().contains(&format!("Entry {hidden} is not visible")));
    flush().await;
    assert_eq!(harness.storage.commit_count(), commits);
    assert_eq!(harness.publications_snapshot().len(), published);
    let independent = harness
        .session
        .commit(
            |tx| async move { tx.create_conversation(ConversationOwnership::Ownerless).await },
            context(),
        )
        .await
        .expect("independent");
    assert_eq!(
        harness
            .storage
            .conversation(independent.id, context())
            .await
            .expect("get"),
        Some(independent)
    );
}

#[tokio::test]
async fn rejects_duplicate_as_of_and_current_selections_for_one_child_address() {
    let as_of = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "fork.duplicate-policy",
            version: 1,
            initial: || StrDoc {
                value: "as-of".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("as_of");
    let current = define_conversation_doc(
        LatestFork::Current,
        DocDefinitionInput {
            kind: "fork.duplicate-policy",
            version: 1,
            initial: || StrDoc {
                value: "current".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("current");

    let harness = open_test_session();
    let parent_id = create_conversation(&harness.session).await;
    let ao_c = as_of.clone();
    let old_at = harness
        .session
        .commit(
            move |tx| async move {
                let e = tx
                    .append_entry(
                        parent_id,
                        EntryDraft {
                            kind: "old".to_owned(),
                            ..Default::default()
                        },
                    )
                    .await?
                    .id;
                tx.doc_conversation(&ao_c, parent_id).await?;
                Ok(e)
            },
            context(),
        )
        .await
        .expect("old");
    let ao_c2 = as_of.clone();
    harness
        .session
        .commit(
            move |tx| async move { tx.retire_doc_conversation(&ao_c2, parent_id).await },
            context(),
        )
        .await
        .expect("retire");
    let cu_c = current.clone();
    harness
        .session
        .commit(
            move |tx| async move { tx.doc_conversation(&cu_c, parent_id).await.map(|_| ()) },
            context(),
        )
        .await
        .expect("current");

    let commits = harness.storage.commit_count();
    let err = harness
        .session
        .commit(
            move |tx| async move {
                tx.fork_conversation(parent_id, old_at, ConversationOwnership::Ownerless)
                    .await
            },
            context(),
        )
        .await
        .expect_err("dup");
    assert!(err.to_string().contains("Fork selects multiple source documents"));
    assert_eq!(harness.storage.commit_count(), commits);

    let independent = harness
        .session
        .commit(
            |tx| async move { tx.create_conversation(ConversationOwnership::Ownerless).await },
            context(),
        )
        .await
        .expect("independent");
    assert_eq!(
        harness
            .storage
            .conversation(independent.id, context())
            .await
            .expect("get"),
        Some(independent)
    );
}

#[tokio::test]
async fn rejects_current_and_as_of_source_writes_in_fork_transaction() {
    let current = define_conversation_doc(
        LatestFork::Current,
        DocDefinitionInput {
            kind: "fork.same-transaction-current",
            version: 1,
            initial: || StrDoc {
                value: "committed".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("current");
    let as_of = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "fork.same-transaction-as-of",
            version: 1,
            initial: || StrDoc {
                value: "committed".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("as_of");

    let harness = open_test_session();
    let parent_id = create_conversation(&harness.session).await;
    let cu_c = current.clone();
    let ao_c = as_of.clone();
    let fork_at = harness
        .session
        .commit(
            move |tx| async move {
                let e = tx
                    .append_entry(
                        parent_id,
                        EntryDraft {
                            kind: "point".to_owned(),
                            ..Default::default()
                        },
                    )
                    .await?
                    .id;
                tx.doc_conversation(&cu_c, parent_id).await?;
                tx.doc_conversation(&ao_c, parent_id).await?;
                Ok(e)
            },
            context(),
        )
        .await
        .expect("init");
    let commits = harness.storage.commit_count();

    let cu1 = current.clone();
    assert!(
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_conversation(&cu1, parent_id)
                        .await?
                        .update(|v| v.value = "before-fork".to_owned())?;
                    tx.fork_conversation(parent_id, fork_at, ConversationOwnership::Ownerless)
                        .await
                },
                context(),
            )
            .await
            .expect_err("before fork")
            .to_string()
            .contains("Cannot change fork source document")
    );
    let cu2 = current.clone();
    assert!(
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.fork_conversation(parent_id, fork_at, ConversationOwnership::Ownerless)
                        .await?;
                    tx.doc_conversation(&cu2, parent_id)
                        .await?
                        .update(|v| v.value = "after-fork".to_owned())
                },
                context(),
            )
            .await
            .expect_err("after fork")
            .to_string()
            .contains("Cannot change fork source document")
    );
    let ao1 = as_of.clone();
    assert!(
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_conversation(&ao1, parent_id)
                        .await?
                        .update(|v| v.value = "as-of-write".to_owned())?;
                    tx.fork_conversation(parent_id, fork_at, ConversationOwnership::Ownerless)
                        .await
                },
                context(),
            )
            .await
            .expect_err("as_of write")
            .to_string()
            .contains("Cannot change fork source document")
    );

    assert_eq!(harness.storage.commit_count(), commits);
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&current, parent_id, context())
            .await
            .expect("p"),
        Some(StrDoc {
            value: "committed".to_owned()
        })
    );
    let child = harness
        .session
        .commit(
            move |tx| async move {
                tx.fork_conversation(parent_id, fork_at, ConversationOwnership::Ownerless)
                    .await
            },
            context(),
        )
        .await
        .expect("clean fork");
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&current, child.id, context())
            .await
            .expect("c"),
        Some(StrDoc {
            value: "committed".to_owned()
        })
    );
}

#[tokio::test]
async fn rolls_every_copied_base_back_when_later_pre_admission_assembly_fails() {
    let copied = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "fork.rollback.copied",
            version: 1,
            initial: || StrDoc {
                value: "copied".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("copied");

    let fail_checkpoint = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let fail_cb = Arc::clone(&fail_checkpoint);
    let failure = pi_durable::documents::define_session_doc_with(DocDefinitionCallbacks {
        kind: "fork.rollback.failure",
        version: 1,
        initial: Arc::new(|| Ok(CountDoc { count: 0 })),
        migrate: None,
        checkpoint_when: Some(Arc::new(move |_v, _ops, _info| {
            if fail_cb.load(Ordering::SeqCst) {
                return Err(pi_js::Error::msg("checkpoint failed"));
            }
            Ok(false)
        })),
    })
    .expect("failure");

    let harness = open_test_session();
    let parent_id = create_conversation(&harness.session).await;
    let cp_c = copied.clone();
    let fl_c = failure.clone();
    let fork_at = harness
        .session
        .commit(
            move |tx| async move {
                let e = tx
                    .append_entry(
                        parent_id,
                        EntryDraft {
                            kind: "point".to_owned(),
                            ..Default::default()
                        },
                    )
                    .await?
                    .id;
                tx.doc_conversation(&cp_c, parent_id).await?;
                tx.doc_session(&fl_c).await?;
                Ok(e)
            },
            context(),
        )
        .await
        .expect("init");
    flush().await;
    let commits = harness.storage.commit_count();
    let published = harness.publications_snapshot().len();

    let captured_child_id = Arc::new(std::sync::Mutex::new(None));
    let child_id_cb = Arc::clone(&captured_child_id);
    let fl_c2 = failure.clone();
    let err = harness
        .session
        .commit(
            move |tx| async move {
                let cid = tx
                    .fork_conversation(parent_id, fork_at, ConversationOwnership::Ownerless)
                    .await?
                    .id;
                *child_id_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(cid);
                tx.doc_session(&fl_c2).await?.update(|v| v.count = 1)
            },
            context(),
        )
        .await
        .expect_err("fail");
    assert!(err.to_string().contains("checkpoint failed"));
    flush().await;
    assert_eq!(harness.storage.commit_count(), commits);
    assert_eq!(harness.publications_snapshot().len(), published);
    let child_id = captured_child_id
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .expect("child_id");
    assert_eq!(
        harness.storage.conversation(child_id, context()).await.expect("conv"),
        None
    );

    fail_checkpoint.store(false, Ordering::SeqCst);
    let fl_c3 = failure.clone();
    harness
        .session
        .commit(
            move |tx| async move { tx.doc_session(&fl_c3).await?.update(|v| v.count = 2) },
            context(),
        )
        .await
        .expect("recover");
    assert_eq!(
        harness.facade.snapshot_session(&failure, context()).await.expect("s"),
        Some(CountDoc { count: 2 })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&copied, parent_id, context())
            .await
            .expect("c"),
        Some(StrDoc {
            value: "copied".to_owned()
        })
    );
}

#[tokio::test]
async fn rolls_back_guaranteed_storage_rejection_without_poisoning_session() {
    let doc = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "fork.storage-rejected",
            version: 1,
            initial: || StrDoc {
                value: "source".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("doc");

    let harness = open_test_session();
    let parent_id = create_conversation(&harness.session).await;
    let doc_c = doc.clone();
    let fork_at = harness
        .session
        .commit(
            move |tx| async move {
                let e = tx
                    .append_entry(
                        parent_id,
                        EntryDraft {
                            kind: "point".to_owned(),
                            ..Default::default()
                        },
                    )
                    .await?
                    .id;
                tx.doc_conversation(&doc_c, parent_id).await?;
                Ok(e)
            },
            context(),
        )
        .await
        .expect("init");

    let rejected_child_id = Arc::new(std::sync::Mutex::new(None));
    let rej_cb = Arc::clone(&rejected_child_id);
    harness
        .storage
        .fail_next_commit(StorageRejected::new("copy rejected").into());
    let err = harness
        .session
        .commit(
            move |tx| async move {
                let cid = tx
                    .fork_conversation(parent_id, fork_at, ConversationOwnership::Ownerless)
                    .await?
                    .id;
                *rej_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(cid);
                Ok(())
            },
            context(),
        )
        .await
        .expect_err("rejected");
    assert!(err.to_string().contains("copy rejected"));
    let cid = rejected_child_id
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .expect("cid");
    assert_eq!(harness.storage.conversation(cid, context()).await.expect("conv"), None);
    let next = harness
        .session
        .commit(
            |tx| async move { tx.create_conversation(ConversationOwnership::Ownerless).await },
            context(),
        )
        .await
        .expect("next");
    assert_eq!(
        harness.storage.conversation(next.id, context()).await.expect("conv"),
        Some(next)
    );
}

#[tokio::test]
async fn retires_copied_document_and_recreates_address_in_fork_transaction() {
    let doc = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "fork.retire-recreate",
            version: 1,
            initial: || StrDoc {
                value: "fresh".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("doc");

    let harness = open_test_session();
    let parent_id = create_conversation(&harness.session).await;
    let d1 = doc.clone();
    let fork_at = harness
        .session
        .commit(
            move |tx| async move {
                let e = tx
                    .append_entry(
                        parent_id,
                        EntryDraft {
                            kind: "point".to_owned(),
                            ..Default::default()
                        },
                    )
                    .await?
                    .id;
                tx.doc_conversation(&d1, parent_id)
                    .await?
                    .update(|v| v.value = "copied".to_owned())?;
                Ok(e)
            },
            context(),
        )
        .await
        .expect("init");

    let d2 = doc.clone();
    let child = harness
        .session
        .commit(
            move |tx| async move {
                let created = tx
                    .fork_conversation(parent_id, fork_at, ConversationOwnership::Ownerless)
                    .await?;
                tx.retire_doc_conversation(&d2, created.id).await?;
                tx.doc_conversation(&d2, created.id)
                    .await?
                    .update(|v| v.value = "replacement".to_owned())?;
                Ok(created)
            },
            context(),
        )
        .await
        .expect("fork retire recreate");
    let writes = harness.storage.last_commit().expect("writes");
    assert_eq!(document_copies(&writes).len(), 1);
    assert_eq!(document_creates(&writes).len(), 1);
    assert_eq!(
        writes
            .iter()
            .filter(|w| matches!(w, StorageWrite::DocumentRetire { .. }))
            .count(),
        1
    );
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&doc, child.id, context())
            .await
            .expect("snap"),
        Some(StrDoc {
            value: "replacement".to_owned()
        })
    );
}

#[tokio::test]
async fn copies_only_conversation_documents_leaving_session_and_task_documents_in_original_scopes() {
    let copied = define_conversation_doc(
        LatestFork::Current,
        DocDefinitionInput {
            kind: "fork.scope.conversation",
            version: 1,
            initial: || StrDoc {
                value: "conversation".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("copied");
    let session_only = define_session_doc(DocDefinitionInput {
        kind: "fork.scope.session",
        version: 1,
        initial: || StrDoc {
            value: "session".to_owned(),
        },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("session_only");
    let task_only = define_task_doc(DocDefinitionInput {
        kind: "fork.scope.task",
        version: 1,
        initial: || StrDoc {
            value: "task".to_owned(),
        },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("task_only");

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct PhaseStart {
        phase: String,
    }
    let mut phases = IndexMap::new();
    phases.insert(
        "start".to_owned(),
        Arc::new(|_, _, _| Box::pin(async { Ok(()) }) as pi_js::BoxFuture<pi_js::Result<()>>)
            as pi_durable::tasks::TypedPhaseHandler<(), PhaseStart, (), ()>,
    );
    let work = define_task(TaskDefinition {
        name: "fork.scope.work".to_owned(),
        version: 1,
        initial: Arc::new(|_| {
            Ok(PhaseStart {
                phase: "start".to_owned(),
            })
        }),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: PhantomData,
    });

    let harness = open_test_session();
    let parent_id = create_conversation(&harness.session).await;

    let cp_c = copied.clone();
    let so_c = session_only.clone();
    let to_c = task_only.clone();
    let w_c = work.clone();
    let (fork_at, task_id) = harness
        .session
        .commit(
            move |tx| async move {
                let e = tx
                    .append_entry(
                        parent_id,
                        EntryDraft {
                            kind: "point".to_owned(),
                            ..Default::default()
                        },
                    )
                    .await?
                    .id;
                let tid = tx
                    .create_task(
                        &w_c,
                        &(),
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(parent_id),
                            background: false,
                        },
                    )
                    .await?;
                tx.doc_conversation(&cp_c, parent_id).await?;
                tx.doc_session(&so_c).await?;
                tx.doc_task(&to_c, tid.erased()).await?;
                Ok((e, tid))
            },
            context(),
        )
        .await
        .expect("init");

    let child = harness
        .session
        .commit(
            move |tx| async move {
                tx.fork_conversation(parent_id, fork_at, ConversationOwnership::Ownerless)
                    .await
            },
            context(),
        )
        .await
        .expect("fork");
    flush().await;

    let copies = document_copies(&harness.storage.last_commit().expect("writes"));
    assert_eq!(
        copies.iter().map(|(r, _)| r.kind.clone()).collect::<Vec<_>>(),
        vec![copied.definition.kind.clone()]
    );
    let copy_changes = document_copy_changes(&harness.last_publication().expect("pub"));
    assert_eq!(
        copy_changes.iter().map(|c| c.record.kind.clone()).collect::<Vec<_>>(),
        vec![copied.definition.kind.clone()]
    );
    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&copied, child.id, context())
            .await
            .expect("s1"),
        Some(StrDoc {
            value: "conversation".to_owned()
        })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_session(&session_only, context())
            .await
            .expect("s2"),
        Some(StrDoc {
            value: "session".to_owned()
        })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_task(&task_only, task_id.erased(), context())
            .await
            .expect("s3"),
        Some(StrDoc {
            value: "task".to_owned()
        })
    );
}

#[tokio::test]
async fn copies_every_family_member_across_storage_scan_pages() {
    let family = define_conversation_doc_family(
        LatestFork::Current,
        DocFamilyDefinitionInput {
            kind: "fork.pagination",
            version: 1,
            initial: |seed: i64| CountDoc { count: seed },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("family");

    let harness = open_test_session();
    let parent_id = create_conversation(&harness.session).await;
    let fork_at = harness
        .session
        .commit(
            move |tx| async move {
                Ok(tx
                    .append_entry(
                        parent_id,
                        EntryDraft {
                            kind: "point".to_owned(),
                            ..Default::default()
                        },
                    )
                    .await?
                    .id)
            },
            context(),
        )
        .await
        .expect("entry");

    let fam_c = family.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                for index in 0..260_i64 {
                    tx.doc_conversation_family(&fam_c, parent_id, &format!("member-{index}"), index)
                        .await?;
                }
                Ok(())
            },
            context(),
        )
        .await
        .expect("create 260");

    let child = harness
        .session
        .commit(
            move |tx| async move {
                tx.fork_conversation(parent_id, fork_at, ConversationOwnership::Ownerless)
                    .await
            },
            context(),
        )
        .await
        .expect("fork");
    assert_eq!(
        document_copies(&harness.storage.last_commit().expect("writes")).len(),
        260
    );
    assert_eq!(
        harness
            .facade
            .snapshot_conversation_family(&family, child.id, "member-0", context())
            .await
            .expect("0"),
        Some(CountDoc { count: 0 })
    );
    assert_eq!(
        harness
            .facade
            .snapshot_conversation_family(&family, child.id, "member-259", context())
            .await
            .expect("259"),
        Some(CountDoc { count: 259 })
    );
}
