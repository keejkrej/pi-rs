//! Port of packages/durable/test/examples/05-watches.ts
#![allow(dead_code)]

// Serialize asynchronous document work with a watch.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::documents::{DocDefinitionInput, define_rewindable_conversation_doc};
use pi_durable::types::{ConversationOwnership, RewindableFork};
use pi_durable::{MemoryStorage, create_session};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

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

    // A watch starts from one stable acquisition revision. Slow callbacks never
    // overlap; exact committed frames buffer, with a full-value reset after 100.
    let notes_watch = session
        .watch_doc_conversation(&notes, chat.id, context.clone())
        .await?
        .ok_or_else(|| pi_js::Error::msg("notes are absent"))?;
    pi_js::console::log(&format!("watch baseline: {:?}", notes_watch.value()));

    let delivered = Arc::new(Notify::new());
    let delivered_cb = Arc::clone(&delivered);
    notes_watch.start(Arc::new(move |value, _ops, _delivery_context| {
        let delivered = Arc::clone(&delivered_cb);
        Box::pin(async move {
            pi_js::console::log(&format!("watch update: {value:?}"));
            delivered.notify_one();
            Ok(())
        })
    }))?;

    let notes_c = notes.clone();
    session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&notes_c, chat.id)
                    .await?
                    .update(|doc| doc.text = "observed asynchronously".to_owned())?;
                Ok(())
            },
            context.clone(),
        )
        .await?;

    delivered.notified().await;
    notes_watch.stop().await;

    session.close(context).await?;
    Ok(())
}
