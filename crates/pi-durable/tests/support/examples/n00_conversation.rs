//! Port of packages/durable/test/examples/00-conversation.ts
#![allow(dead_code)]

// A Session stores conversations, transcript entries, tasks, and documents.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::types::ConversationOwnership;
use pi_durable::{MemoryStorage, create_session};

pub async fn run() -> pi_js::Result<()> {
    // MemoryStorage keeps everything in memory; other storage backends keep it on disk.
    let session = create_session(Arc::new(MemoryStorage::new()));

    // Every Session call takes a context, which is used for cancellation.
    // BACKGROUND_CONTEXT means "never cancel".
    let context = BACKGROUND_CONTEXT.clone();

    // All writes happen inside session.commit(). The callback receives a
    // transaction `tx`; everything it writes is saved together when the callback
    // returns, or discarded if it errors.
    // "ownerless" means no task created this conversation.
    let standalone = session
        .commit(
            |tx| async move { tx.create_conversation(ConversationOwnership::Ownerless).await },
            context.clone(),
        )
        .await?;
    pi_js::console::log(&format!("standalone conversation: {standalone:?}"));

    session.close(context).await?;
    Ok(())
}
