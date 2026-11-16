//! Port of packages/durable/test/examples/02-forks.ts
#![allow(dead_code)]

// Fork a conversation.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::documents::{DocDefinitionInput, define_rewindable_conversation_doc};
use pi_durable::types::{ConversationOwnership, EntryDraft, EntryQuery, JsonValue, RewindableFork};
use pi_durable::{MemoryStorage, create_session};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Notes {
    text: String,
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();
    let session = create_session(Arc::new(MemoryStorage::new()));

    let notes = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "example.notes",
            version: 1,
            initial: || Notes { text: String::new() },
            migrate: None,
            checkpoint_when: None,
        },
    )?;

    let chat = session
        .commit(
            |tx| async move { tx.create_conversation(ConversationOwnership::Ownerless).await },
            context.clone(),
        )
        .await?;

    let notes_c = notes.clone();
    let first_entry = session
        .commit(
            move |tx| async move {
                let entry = tx
                    .append_entry(
                        chat.id,
                        EntryDraft {
                            kind: "note".to_owned(),
                            data: Some(JsonValue::String("hello".to_owned())),
                            ..Default::default()
                        },
                    )
                    .await?;
                tx.doc_conversation(&notes_c, chat.id)
                    .await?
                    .update(|doc| doc.text = "after hello".to_owned())?;
                Ok(entry)
            },
            context.clone(),
        )
        .await?;

    let notes_c = notes.clone();
    session
        .commit(
            move |tx| async move {
                tx.append_entry(
                    chat.id,
                    EntryDraft {
                        kind: "note".to_owned(),
                        data: Some(JsonValue::String("goodbye".to_owned())),
                        ..Default::default()
                    },
                )
                .await?;
                tx.doc_conversation(&notes_c, chat.id)
                    .await?
                    .update(|doc| doc.text = "after goodbye".to_owned())?;
                Ok(())
            },
            context.clone(),
        )
        .await?;

    // A fork is a new conversation that continues from one entry of another. It
    // sees the parent's transcript up to that entry, and each document follows its
    // own `fork` setting. Notes uses "asOf", so the fork starts with the notes
    // value from the fork entry.
    let branch = session
        .commit(
            move |tx| async move {
                tx.fork_conversation(chat.id, first_entry.id, ConversationOwnership::Ownerless)
                    .await
            },
            context.clone(),
        )
        .await?;

    // scan_entries() pages through visible entries, newest first. The fork sees
    // "hello" (inherited from the parent) but not "goodbye", which came later.
    let branch_id = branch.id;
    let branch_entries = session
        .commit(
            move |tx| async move {
                tx.scan_entries(
                    EntryQuery {
                        conversation_id: branch_id,
                        min_entry_id: None,
                        max_entry_id: None,
                    },
                    10,
                    None,
                )
                .await
            },
            context.clone(),
        )
        .await?;
    let transcript_data: Vec<Option<JsonValue>> = branch_entries.items.into_iter().map(|entry| entry.data).collect();
    pi_js::console::log(&format!("fork transcript: {transcript_data:?}"));
    let branch_notes = session
        .snapshot_conversation(&notes, branch.id, context.clone())
        .await?;
    pi_js::console::log(&format!("fork notes: {branch_notes:?}"));

    // The fork's copy is independent: editing it leaves the parent unchanged.
    let notes_c = notes.clone();
    session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&notes_c, branch_id)
                    .await?
                    .update(|doc| doc.text = "changed only in the fork".to_owned())?;
                Ok(())
            },
            context.clone(),
        )
        .await?;
    let branch_after = session
        .snapshot_conversation(&notes, branch.id, context.clone())
        .await?;
    let parent_after = session.snapshot_conversation(&notes, chat.id, context.clone()).await?;
    pi_js::console::log(&format!("fork notes after edit: {branch_after:?}"));
    pi_js::console::log(&format!("parent notes after edit: {parent_after:?}"));

    session.close(context).await?;
    Ok(())
}
