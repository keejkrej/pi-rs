//! Port of packages/durable/test/examples/01-documents.ts
#![allow(dead_code)]

// Store document state next to transcript entries.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::documents::{DocDefinitionInput, define_rewindable_conversation_doc};
use pi_durable::types::{ConversationOwnership, EntryDraft, JsonValue, RewindableFork};
use pi_durable::{MemoryStorage, create_session};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Notes {
    text: String,
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();
    let session = create_session(Arc::new(MemoryStorage::new()));

    // A document is a JSON object attached to something; here, one per conversation.
    // "rewindable" keeps old values readable, so you can ask what the document
    // looked like when a particular entry was written.
    // `fork` says what a forked copy of the conversation starts with (see 02-forks).
    let notes = define_rewindable_conversation_doc(
        RewindableFork::AsOf, // a fork starts with the value these notes had at the fork entry
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

    // tx.doc_conversation() returns an editable copy of the document (created on first use).
    // Mutations through it are saved when the commit finishes.
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
    let second_entry = session
        .commit(
            move |tx| async move {
                let entry = tx
                    .append_entry(
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
                Ok(entry)
            },
            context.clone(),
        )
        .await?;

    // snapshot_conversation() reads the latest value. snapshot_as_of() reads the value that was
    // saved in the same commit as the given entry.
    let latest = session.snapshot_conversation(&notes, chat.id, context.clone()).await?;
    let at_first = session
        .snapshot_as_of(&notes, chat.id, first_entry.id, context.clone())
        .await?;
    let at_second = session
        .snapshot_as_of(&notes, chat.id, second_entry.id, context.clone())
        .await?;
    pi_js::console::log(&format!("latest notes: {latest:?}"));
    pi_js::console::log(&format!("notes at first entry: {at_first:?}"));
    pi_js::console::log(&format!("notes at second entry: {at_second:?}"));

    session.close(context).await?;
    Ok(())
}
