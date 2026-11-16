//! Port of packages/durable/src/session/forks.ts

use indexmap::IndexSet;
use pi_chord::Context;

use crate::documents::address_id;
use crate::ids::mint_typed_id;
use crate::types::{
    ConversationId, Cursor, DocumentCopySource, DocumentCreate, DocumentFork, DocumentKind, DocumentPoint,
    DocumentQuery, DocumentScope, EntryId, Storage,
};

const SCAN_PAGE_SIZE: usize = 256;

/// One definition-free document copy to create with a forked conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkDocumentCopy {
    pub record: DocumentCreate,
    pub source: DocumentCopySource,
}

/// Select every persisted conversation document copied by one fork.
pub async fn prepare_fork_document_copies(
    storage: &dyn Storage,
    parent_conversation_id: ConversationId,
    at: EntryId,
    child_conversation_id: ConversationId,
    context: &Context,
) -> pi_js::Result<Vec<ForkDocumentCopy>> {
    let entry = storage
        .entry_in_conversation(parent_conversation_id, at, context.clone())
        .await?;
    let Some(entry) = entry else {
        return Err(pi_js::Error::msg(format!(
            "Entry {at} is not visible from conversation {parent_conversation_id}"
        )));
    };

    let mut copies = Vec::new();
    let mut copied_addresses = IndexSet::new();
    collect_copies(
        storage,
        DocumentScope::Conversation {
            conversation_id: entry.entry.conversation_id,
        },
        DocumentPoint::At(entry.commit_seq),
        DocumentFork::AsOf,
        child_conversation_id,
        &mut copies,
        &mut copied_addresses,
        context,
    )
    .await?;
    collect_copies(
        storage,
        DocumentScope::Conversation {
            conversation_id: parent_conversation_id,
        },
        DocumentPoint::Current,
        DocumentFork::Current,
        child_conversation_id,
        &mut copies,
        &mut copied_addresses,
        context,
    )
    .await?;
    Ok(copies)
}

#[allow(clippy::too_many_arguments)]
async fn collect_copies(
    storage: &dyn Storage,
    scope: DocumentScope,
    at: DocumentPoint,
    policy: DocumentFork,
    child_conversation_id: ConversationId,
    copies: &mut Vec<ForkDocumentCopy>,
    copied_addresses: &mut IndexSet<String>,
    context: &Context,
) -> pi_js::Result<()> {
    let mut cursor: Option<Cursor> = None;
    loop {
        let page = storage
            .scan_documents(
                DocumentQuery { scope, at, kind: None },
                SCAN_PAGE_SIZE,
                cursor,
                context.clone(),
            )
            .await?;
        for source in page.items {
            if !matches!(source.scope, DocumentScope::Conversation { .. }) || source.fork != Some(policy) {
                continue;
            }
            let id = mint_typed_id::<DocumentKind, _>(storage).await?;
            let record = DocumentCreate {
                id,
                kind: source.kind.clone(),
                key: source.key.clone(),
                scope: DocumentScope::Conversation {
                    conversation_id: child_conversation_id,
                },
                history: source.history,
                fork: source.fork,
            };
            let copy_address = address_id(&record.as_address());
            if !copied_addresses.insert(copy_address) {
                let member = match &record.key {
                    None => record.kind.clone(),
                    Some(key) => format!("{}/{key}", record.kind),
                };
                return Err(pi_js::Error::msg(format!(
                    "Fork selects multiple source documents for {member}"
                )));
            }
            copies.push(ForkDocumentCopy {
                record,
                source: DocumentCopySource { id: source.id, at },
            });
        }
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(())
}
