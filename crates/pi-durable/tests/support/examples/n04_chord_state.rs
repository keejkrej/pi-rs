//! Port of packages/durable/test/examples/04-chord-state.ts
#![allow(dead_code)]

// Expose a document through Chord.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::documents::{DocDefinitionInput, define_rewindable_conversation_doc};
use pi_durable::types::{ConversationOwnership, RewindableFork};
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

    let notes_c = notes.clone();
    let chat = session
        .commit(
            move |tx| async move {
                let conversation = tx.create_conversation(ConversationOwnership::Ownerless).await?;
                tx.doc_conversation(&notes_c, conversation.id)
                    .await?
                    .update(|doc| doc.text = "first".to_owned())?;
                Ok(conversation)
            },
            context.clone(),
        )
        .await?;

    // document_state_conversation() never creates a document. It returns a hydrated read-only
    // Chord state bound to the current concrete incarnation.
    let notes_state = session
        .document_state_conversation(&notes, chat.id, context.clone())
        .await?
        .ok_or_else(|| pi_js::Error::msg("notes are absent"))?;
    let stop_notes = notes_state.subscribe(|value, _delivery_context, delivery| {
        Box::pin(async move {
            pi_js::console::log(&format!(
                "Chord notes: {:?} {} {:?}",
                delivery.kind, delivery.sequence, value
            ));
            Ok(())
        })
    });

    let notes_c = notes.clone();
    session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&notes_c, chat.id)
                    .await?
                    .update(|doc| doc.text = "published through Chord".to_owned())?;
                Ok(())
            },
            context.clone(),
        )
        .await?;

    stop_notes.call();
    notes_state.dispose()?;

    session.close(context).await?;
    Ok(())
}
