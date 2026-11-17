//! Port of packages/durable/test/session-definitions.test.ts

mod support;

use std::sync::Arc;

use pi_durable::documents::{
    DocDefinitionInput, DocFamilyDefinitionInput, define_conversation_doc, define_conversation_doc_family,
    define_rewindable_conversation_doc, define_rewindable_conversation_doc_family, define_session_doc,
    define_session_doc_family, define_task_doc, define_task_doc_family,
};
use pi_durable::entries::define_entry;
use pi_durable::ids::{EntryId, TaskId, id_from_number};
use pi_durable::session::create_session;
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::types::{
    ConversationOwnership, LatestFork, ROOT_CONVERSATION_ID, RewindableFork, Storage, TypedEntryDraft,
};
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use support::session_support::context;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    value: i64,
}

fn initial_state() -> State {
    State { value: 0 }
}

fn initial_seeded(seed: i64) -> State {
    State { value: seed }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct NoteData {
    text: String,
}

#[test]
fn validates_persisted_version_semantics() {
    let err = define_session_doc(DocDefinitionInput {
        kind: "k",
        version: 0,
        initial: initial_state,
        migrate: None,
        checkpoint_when: None,
    })
    .err()
    .expect("version 0 rejected");
    assert!(err.to_string().contains("positive integer"), "got: {err}");

    let err_neg = define_session_doc(DocDefinitionInput {
        kind: "k",
        version: -2,
        initial: initial_state,
        migrate: None,
        checkpoint_when: None,
    })
    .err()
    .expect("negative version rejected");
    assert!(err_neg.to_string().contains("positive integer"), "got: {err_neg}");

    let empty_kind = define_session_doc(DocDefinitionInput {
        kind: "",
        version: 1,
        initial: initial_state,
        migrate: None,
        checkpoint_when: None,
    })
    .expect("empty kind allowed");
    assert_eq!(empty_kind.definition.kind, "");

    let session_doc = define_session_doc(DocDefinitionInput {
        kind: "t.session",
        version: 1,
        initial: initial_state,
        migrate: None,
        checkpoint_when: None,
    })
    .expect("session doc");
    assert_eq!(session_doc.definition.kind, "t.session");
}

#[tokio::test]
async fn exercises_every_owner_key_and_seed_token_overload_and_typed_entries() {
    let session_doc = define_session_doc(DocDefinitionInput {
        kind: "t.session",
        version: 1,
        initial: initial_state,
        migrate: None,
        checkpoint_when: None,
    })
    .expect("session doc");
    let latest_doc = define_conversation_doc(
        LatestFork::Current,
        DocDefinitionInput {
            kind: "t.latest",
            version: 1,
            initial: initial_state,
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("latest doc");
    let rewindable_doc = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "t.rewindable",
            version: 1,
            initial: initial_state,
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("rewindable doc");
    let _task_doc = define_task_doc(DocDefinitionInput {
        kind: "t.task",
        version: 1,
        initial: initial_state,
        migrate: None,
        checkpoint_when: None,
    })
    .expect("task doc");
    let session_family = define_session_doc_family(DocFamilyDefinitionInput {
        kind: "t.session-family",
        version: 1,
        initial: initial_seeded,
        migrate: None,
        checkpoint_when: None,
    })
    .expect("session family");
    let conversation_family = define_rewindable_conversation_doc_family(
        RewindableFork::Initial,
        DocFamilyDefinitionInput {
            kind: "t.conversation-family",
            version: 1,
            initial: initial_seeded,
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("conversation family");
    let _latest_family = define_conversation_doc_family(
        LatestFork::Current,
        DocFamilyDefinitionInput {
            kind: "t.latest-family",
            version: 1,
            initial: initial_seeded,
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("latest family");
    let _task_family = define_task_doc_family(DocFamilyDefinitionInput {
        kind: "t.task-family",
        version: 1,
        initial: initial_seeded,
        migrate: None,
        checkpoint_when: None,
    })
    .expect("task family");

    let note = define_entry::<NoteData>("t.note").expect("note");
    let marker = define_entry::<()>("t.marker").expect("marker");

    let session = create_session(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>);
    let task_id: TaskId = id_from_number(1);

    assert_eq!(
        session.snapshot_session(&session_doc, context()).await.expect("snap"),
        None
    );
    assert_eq!(
        session
            .snapshot_conversation(&latest_doc, ROOT_CONVERSATION_ID, context())
            .await
            .expect("snap"),
        None
    );
    assert_eq!(
        session
            .snapshot_task(&_task_doc, task_id, context())
            .await
            .expect("snap"),
        None
    );
    assert_eq!(
        session
            .snapshot_session_family(&session_family, "k", context())
            .await
            .expect("snap"),
        None
    );
    assert_eq!(
        session
            .snapshot_conversation_family(&conversation_family, ROOT_CONVERSATION_ID, "k", context())
            .await
            .expect("snap"),
        None
    );
    assert_eq!(
        session
            .snapshot_task_family(&_task_family, task_id, "k", context())
            .await
            .expect("snap"),
        None
    );

    let (cid, note_entry_id, marker_entry_id): (_, EntryId, EntryId) = {
        let sdoc = session_doc.clone();
        let ldoc = latest_doc.clone();
        let rdoc = rewindable_doc.clone();
        let sfam = session_family.clone();
        let cfam = conversation_family.clone();
        let note_tok = note.clone();
        let marker_tok = marker.clone();
        session
            .commit(
                move |tx| async move {
                    let conv = tx.create_conversation(ConversationOwnership::Ownerless).await?;
                    tx.doc_session(&sdoc).await?.update(|s| s.value = 1)?;
                    tx.doc_conversation(&ldoc, conv.id).await?.update(|s| s.value = 2)?;
                    tx.doc_conversation(&rdoc, conv.id).await?.update(|s| s.value = 3)?;
                    tx.doc_session_family(&sfam, "k", 7).await?;
                    tx.doc_conversation_family(&cfam, conv.id, "k", 9).await?;

                    let ne = tx
                        .append_typed_entry(
                            &note_tok,
                            conv.id,
                            TypedEntryDraft {
                                data: NoteData { text: "x".to_owned() },
                                model: None,
                                head: None,
                                edits: None,
                            },
                        )
                        .await?;
                    assert_eq!(ne.data.text, "x");
                    let me = tx
                        .append_typed_entry(
                            &marker_tok,
                            conv.id,
                            TypedEntryDraft {
                                data: (),
                                model: None,
                                head: None,
                                edits: None,
                            },
                        )
                        .await?;
                    Ok((conv.id, ne.record.id, me.record.id))
                },
                context(),
            )
            .await
            .expect("commit")
    };

    assert_eq!(
        session.snapshot_session(&session_doc, context()).await.expect("s"),
        Some(State { value: 1 })
    );
    assert_eq!(
        session
            .snapshot_conversation(&latest_doc, cid, context())
            .await
            .expect("l"),
        Some(State { value: 2 })
    );
    assert_eq!(
        session
            .snapshot_as_of(&rewindable_doc, cid, note_entry_id, context())
            .await
            .expect("rew"),
        Some(State { value: 3 })
    );
    assert_eq!(
        session
            .snapshot_family_as_of(&conversation_family, cid, "k", marker_entry_id, context())
            .await
            .expect("fam rew"),
        Some(State { value: 9 })
    );
    assert!(
        session
            .document_state_session(&session_doc, context())
            .await
            .expect("st")
            .is_some()
    );
    assert!(
        session
            .watch_doc_session(&session_doc, context())
            .await
            .expect("w")
            .is_some()
    );

    {
        let note_tok = note.clone();
        session
            .commit(
                move |tx| async move {
                    let raw = tx.entry(note_entry_id).await?;
                    assert!(raw.is_some());
                    let typed = tx.typed_entry(&note_tok, note_entry_id).await?.expect("typed note");
                    assert_eq!(typed.data.text, "x");
                    Ok(())
                },
                context(),
            )
            .await
            .expect("read entry");
    }

    session.close(context()).await.expect("close");
}
