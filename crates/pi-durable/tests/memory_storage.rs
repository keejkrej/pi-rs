//! Port of packages/durable/test/memory-storage.test.ts

use std::sync::Arc;

use pi_chord::background_context;
use pi_durable::ids::{EntryId, id_from_number, seq_from_number};
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::testing::{make_storage_provider, run_storage_conformance};
use pi_durable::types::{ConversationRecord, EntryRecord, ROOT_CONVERSATION_ID, Storage, StorageWrite};
use serde_json::json;

#[tokio::test]
async fn memory_storage_conformance() {
    run_storage_conformance(
        "MemoryStorage",
        make_storage_provider(|callback| async move {
            let storage: Arc<dyn Storage> = Arc::new(MemoryStorage::new());
            callback(storage).await;
        }),
    )
    .await;
}

#[tokio::test]
async fn does_not_expose_retained_state_through_prepared_commit() {
    let context = background_context();
    let storage = MemoryStorage::new();
    storage
        .commit(
            vec![StorageWrite::Conversation {
                value: ConversationRecord {
                    id: ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            context.clone(),
        )
        .await
        .expect("commit root");

    let entry_id: EntryId = id_from_number(2);
    let mut source_writes = vec![StorageWrite::Entry {
        value: EntryRecord {
            id: entry_id,
            conversation_id: ROOT_CONVERSATION_ID,
            kind: "test".to_string(),
            model: None,
            data: Some(json!({ "nested": [1] })),
            head: None,
            edits: None,
            by_task_id: None,
        },
    }];

    let mut prepared = storage
        .prepare_commit(&source_writes, None)
        .expect("prepare commit");

    // Mutating caller input after preparation must not affect the prepared commit.
    if let StorageWrite::Entry { value } = &mut source_writes[0] {
        value.data = Some(json!({ "nested": [1, 2] }));
    }

    assert_eq!(prepared.apply().expect("first apply"), seq_from_number(2));
    assert_eq!(prepared.apply().expect("idempotent apply"), seq_from_number(2));

    // Mutating the exposed commit buffer after application must not mutate stored state.
    if let StorageWrite::Entry { value } = &mut prepared.writes[0] {
        value.data = Some(json!({ "nested": [1, 2, 3] }));
    }

    let lookup = storage
        .entry(entry_id, context)
        .await
        .expect("lookup entry")
        .expect("entry present");
    assert_eq!(lookup.entry.data, Some(json!({ "nested": [1] })));
}
