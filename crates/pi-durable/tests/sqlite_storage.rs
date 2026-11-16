//! Port of packages/durable/test/sqlite-storage.test.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_chord::delta::Op;
use pi_chord::{Context, background_context};
use pi_durable::ids::{ConversationId, DocumentId, EntryId, Seq, TaskId, id_from_number, seq_from_number};
use pi_durable::storage::sqlite::SqliteStorage;
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::testing::{make_storage_provider, run_storage_conformance};
use pi_durable::types::{
    ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentContent, DocumentCreate,
    DocumentFork, DocumentHistory, DocumentPoint, DocumentQuery, DocumentRecord, DocumentScope,
    EntryLookup, EntryQuery, EntryRecord, ErasedTaskRecord, JsonObject, Page, ROOT_CONVERSATION_ID,
    Storage, StorageWrite, StoredDocument, SubmissionId, SubmissionQuery, SubmissionRecord,
    TaskQuery, TaskState,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value as JsonValue, json};
use tokio::sync::Mutex as AsyncMutex;

struct ReopeningStorage {
    current: AsyncMutex<Option<SqliteStorage>>,
    path: String,
    options: NodeSqliteStorageOptions,
    closed: Mutex<bool>,
}

impl ReopeningStorage {
    fn new(current: SqliteStorage, path: String, options: NodeSqliteStorageOptions) -> Self {
        Self {
            current: AsyncMutex::new(Some(current)),
            path,
            options,
            closed: Mutex::new(false),
        }
    }
}

#[async_trait]
impl Storage for ReopeningStorage {
    async fn commit(&self, writes: Vec<StorageWrite>, commit_context: Context) -> pi_js::Result<Seq> {
        let mut guard = self.current.lock().await;
        let storage = guard
            .take()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?;
        let is_closed = *self.closed.lock().expect("lock");
        if is_closed {
            let res = storage.commit(writes, commit_context).await;
            *guard = Some(storage);
            return res;
        }
        let result = storage.commit(writes, commit_context).await;
        let _ = storage.close(background_context()).await;
        let reopened = open_node_sqlite_storage(&self.path, self.options).await?;
        *guard = Some(reopened);
        result
    }

    async fn mint_id(&self) -> pi_js::Result<i64> {
        let guard = self.current.lock().await;
        let storage = guard
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?;
        storage.mint_id().await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: Context,
    ) -> pi_js::Result<Option<ConversationRecord>> {
        let guard = self.current.lock().await;
        guard
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?
            .conversation(id, context)
            .await
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<ConversationRecord, Cursor>> {
        let guard = self.current.lock().await;
        guard
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?
            .scan_conversations(query, limit, cursor, context)
            .await
    }

    async fn entry(&self, id: EntryId, context: Context) -> pi_js::Result<Option<EntryLookup>> {
        let guard = self.current.lock().await;
        guard
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?
            .entry(id, context)
            .await
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<EntryLookup>> {
        let guard = self.current.lock().await;
        guard
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?
            .entry_in_conversation(conversation_id, id, context)
            .await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: Context,
    ) -> pi_js::Result<Option<EntryRecord>> {
        let guard = self.current.lock().await;
        guard
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?
            .find_latest_head_marker(conversation_id, at_or_before_entry_id, context)
            .await
    }

    async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<EntryRecord, Cursor>> {
        let guard = self.current.lock().await;
        guard
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?
            .scan_entries(query, limit, cursor, context)
            .await
    }

    async fn task(&self, id: TaskId, context: Context) -> pi_js::Result<Option<ErasedTaskRecord>> {
        let guard = self.current.lock().await;
        guard
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?
            .task(id, context)
            .await
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<ErasedTaskRecord, Cursor>> {
        let guard = self.current.lock().await;
        guard
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?
            .scan_tasks(query, limit, cursor, context)
            .await
    }

    async fn submission(
        &self,
        id: SubmissionId,
        context: Context,
    ) -> pi_js::Result<Option<SubmissionRecord>> {
        let guard = self.current.lock().await;
        guard
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?
            .submission(id, context)
            .await
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<SubmissionRecord, Cursor>> {
        let guard = self.current.lock().await;
        guard
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?
            .scan_submissions(query, limit, cursor, context)
            .await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: Context,
    ) -> pi_js::Result<Option<SubmissionRecord>> {
        let guard = self.current.lock().await;
        guard
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<DocumentRecord>> {
        let guard = self.current.lock().await;
        guard
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?
            .find_document(address, at, context)
            .await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<StoredDocument>> {
        let guard = self.current.lock().await;
        guard
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?
            .document(id, at, context)
            .await
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<DocumentRecord, Cursor>> {
        let guard = self.current.lock().await;
        guard
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?
            .scan_documents(query, limit, cursor, context)
            .await
    }

    async fn close(&self, close_context: Context) -> pi_js::Result<()> {
        *self.closed.lock().expect("lock") = true;
        let guard = self.current.lock().await;
        if let Some(storage) = guard.as_ref() {
            storage.close(close_context).await?;
        }
        Ok(())
    }
}

async fn create_sqlite_storage(
    options: NodeSqliteStorageOptions,
) -> (tempfile::TempDir, SqliteStorage, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("storage.sqlite").to_string_lossy().into_owned();
    let storage = open_node_sqlite_storage(&path, options)
        .await
        .expect("open sqlite storage");
    (dir, storage, path)
}

fn entry(id: EntryId, conversation_id: ConversationId, data: Option<JsonValue>) -> EntryRecord {
    EntryRecord {
        id,
        conversation_id,
        kind: "message".to_string(),
        model: None,
        data,
        head: None,
        edits: None,
        by_task_id: None,
    }
}

async fn create_root(storage: &dyn Storage) -> Seq {
    storage
        .commit(
            vec![StorageWrite::Conversation {
                value: ConversationRecord {
                    id: ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            background_context(),
        )
        .await
        .expect("commit root")
}

fn pending_task(id: TaskId) -> ErasedTaskRecord {
    ErasedTaskRecord {
        id,
        conversation_id: ROOT_CONVERSATION_ID,
        kind: "test.task".to_string(),
        version: 1,
        input: JsonValue::Null,
        owner: None,
        background: false,
        abort_requested: false,
        state: TaskState::Pending {
            checkpoint: json!({ "phase": "ready" }),
        },
        memos: None,
    }
}

fn obj(value: JsonValue) -> JsonObject {
    serde_json::from_value(value).expect("JsonObject")
}

fn scalar(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |row| row.get::<_, i64>(0))
        .expect("scalar query")
}

fn revision_count(path: &str, document_id: DocumentId) -> i64 {
    let db =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open ro db");
    db.query_row(
        "SELECT count(*) FROM document_revisions WHERE document_id = ?",
        [document_id.as_i64()],
        |row| row.get::<_, i64>(0),
    )
    .expect("revision count")
}

#[tokio::test]
async fn sqlite_storage_conformance() {
    run_storage_conformance(
        "SqliteStorage",
        make_storage_provider(|callback| async move {
            let (_dir, storage, _path) =
                create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
            let s: Arc<dyn Storage> = Arc::new(storage);
            callback(Arc::clone(&s)).await;
            let _ = s.close(background_context()).await;
        }),
    )
    .await;
}

#[tokio::test]
async fn sqlite_storage_across_reopen_conformance() {
    run_storage_conformance(
        "SqliteStorage across reopen",
        make_storage_provider(|callback| async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("storage.sqlite").to_string_lossy().into_owned();
            let created = open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
                .await
                .expect("create");
            created.close(background_context()).await.expect("close");
            let current = open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
                .await
                .expect("reopen");
            let storage: Arc<dyn Storage> = Arc::new(ReopeningStorage::new(
                current,
                path,
                NodeSqliteStorageOptions::default(),
            ));
            callback(Arc::clone(&storage)).await;
            let _ = storage.close(background_context()).await;
        }),
    )
    .await;
}

#[tokio::test]
async fn persists_records_sequence_allocation_and_global_id_allocation_across_reopen() {
    let context = background_context();
    let (_dir, storage, path) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    assert_eq!(create_root(&storage).await, seq_from_number(1));
    let entry_id: EntryId = id_from_number(storage.mint_id().await.expect("mint"));
    assert_eq!(
        storage
            .commit(
                vec![StorageWrite::Entry {
                    value: entry(entry_id, ROOT_CONVERSATION_ID, None),
                }],
                context.clone(),
            )
            .await
            .expect("commit entry"),
        seq_from_number(2)
    );
    storage.close(context.clone()).await.expect("close");

    let reopened = open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
        .await
        .expect("reopen");
    assert_eq!(
        reopened.entry(entry_id, context.clone()).await.expect("entry"),
        Some(EntryLookup {
            entry: entry(entry_id, ROOT_CONVERSATION_ID, None),
            commit_seq: seq_from_number(2),
        })
    );
    assert_eq!(reopened.mint_id().await.expect("mint next"), entry_id.as_i64() + 1);
    let task_id: TaskId = id_from_number(reopened.mint_id().await.expect("mint task"));
    assert_eq!(
        reopened
            .commit(
                vec![StorageWrite::Task {
                    value: pending_task(task_id),
                }],
                context.clone(),
            )
            .await
            .expect("commit task"),
        seq_from_number(3)
    );
    reopened.close(context).await.expect("close reopened");
}

#[tokio::test]
async fn rejects_persisted_metadata_corruption_on_reopen() {
    let context = background_context();
    let (_dir, storage, path) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    storage.close(context).await.expect("close");

    let db = Connection::open(&path).expect("open raw db");
    db.execute_batch("DELETE FROM durable_metadata")
        .expect("delete metadata");
    db.close().expect("close raw db");

    let err = match open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default()).await {
        Ok(_) => panic!("expected metadata missing error"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("Durable SQLite metadata is missing"));
}

#[tokio::test]
async fn rejects_document_whose_required_base_is_missing() {
    let context = background_context();
    let (_dir, storage, path) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    create_root(&storage).await;
    let id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));
    storage
        .commit(
            vec![StorageWrite::DocumentCreate {
                record: DocumentCreate {
                    id,
                    kind: "corrupt".to_string(),
                    key: None,
                    scope: DocumentScope::Session,
                    history: None,
                    fork: None,
                },
                version: 1,
                value: obj(json!({ "retained": true })),
            }],
            context.clone(),
        )
        .await
        .expect("create doc");

    let db = Connection::open(&path).expect("open raw db");
    db.execute(
        "DELETE FROM document_revisions WHERE document_id = ?",
        [id.as_i64()],
    )
    .expect("delete base");
    db.close().expect("close raw db");

    let err = storage
        .document(id, DocumentPoint::Current, context.clone())
        .await
        .expect_err("expected missing base");
    assert!(err
        .to_string()
        .contains(&format!("Document {id} is missing a required base")));
    storage.close(context).await.expect("close");
}

#[tokio::test]
async fn replays_detached_root_replacements_and_follow_up_edits_while_rejecting_corrupt_ops() {
    let context = background_context();
    let (_dir, storage, path) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    create_root(&storage).await;
    let id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));
    storage
        .commit(
            vec![StorageWrite::DocumentCreate {
                record: DocumentCreate {
                    id,
                    kind: "replay".to_string(),
                    key: None,
                    scope: DocumentScope::Session,
                    history: None,
                    fork: None,
                },
                version: 1,
                value: obj(json!({ "nested": { "value": 1 }, "rows": [] })),
            }],
            context.clone(),
        )
        .await
        .expect("create doc");
    storage
        .commit(
            vec![StorageWrite::DocumentChange {
                id,
                content: DocumentContent::Delta {
                    version: 1,
                    ops: vec![Op::ReplaceRoot {
                        value: obj(json!({ "nested": { "value": 2 }, "rows": [{ "id": 1 }] })),
                    }],
                },
            }],
            context.clone(),
        )
        .await
        .expect("root replace delta");
    storage
        .commit(
            vec![StorageWrite::DocumentChange {
                id,
                content: DocumentContent::Delta {
                    version: 1,
                    ops: vec![
                        Op::Set {
                            path: vec!["nested".into(), "value".into()],
                            value: json!(3),
                        },
                        Op::Splice {
                            path: vec!["rows".into()],
                            index: 1,
                            delete_count: 0,
                            items: vec![json!({ "id": 2 })],
                        },
                        Op::Move {
                            path: vec!["rows".into()],
                            from_index: 1,
                            to_index: 0,
                        },
                    ],
                },
            }],
            context.clone(),
        )
        .await
        .expect("edit delta");
    storage
        .commit(
            vec![StorageWrite::DocumentChange {
                id,
                content: DocumentContent::Delta {
                    version: 1,
                    ops: vec![Op::Set {
                        path: vec!["nested".into(), "value".into()],
                        value: json!(4),
                    }],
                },
            }],
            context.clone(),
        )
        .await
        .expect("set 4");

    let expected = obj(json!({ "nested": { "value": 4 }, "rows": [{ "id": 2 }, { "id": 1 }] }));
    let mut first = storage
        .document(id, DocumentPoint::Current, context.clone())
        .await
        .expect("read")
        .expect("present");
    assert_eq!(first.value, expected);
    first.value.insert("nested".to_string(), json!({ "value": 99 }));
    assert_eq!(
        storage
            .document(id, DocumentPoint::Current, context.clone())
            .await
            .expect("re-read")
            .expect("present")
            .value,
        expected
    );

    let db = Connection::open(&path).expect("open raw db");
    db.execute(
        "UPDATE document_revisions SET content = ? WHERE document_id = ? AND seq = (SELECT max(seq) FROM document_revisions WHERE document_id = ?)",
        rusqlite::params![r#"[["unknown"]]"#, id.as_i64(), id.as_i64()],
    )
    .expect("corrupt delta");
    db.close().expect("close raw");

    let err = storage
        .document(id, DocumentPoint::Current, context.clone())
        .await
        .expect_err("expected corrupt op error");
    assert!(
        err.to_string().to_ascii_lowercase().contains("unknown"),
        "unexpected error: {err}"
    );
    storage.close(context).await.expect("close");
}

#[tokio::test]
async fn rolls_sql_rows_and_sequence_allocation_back_as_one_transaction() {
    let context = background_context();
    let (_dir, storage, path) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    create_root(&storage).await;
    let transient_id: EntryId = id_from_number(storage.mint_id().await.expect("mint entry"));
    let transient_task_id: TaskId = id_from_number(storage.mint_id().await.expect("mint task"));

    // Second write fails (duplicate ID / missing conversation), rolling back the first write & seq allocation.
    let bad_write = StorageWrite::Task {
        value: ErasedTaskRecord {
            id: transient_task_id,
            conversation_id: id_from_number(999),
            kind: "test.task".to_string(),
            version: 1,
            input: JsonValue::Null,
            owner: None,
            background: false,
            abort_requested: false,
            state: TaskState::Pending {
                checkpoint: json!({ "phase": "ready" }),
            },
            memos: None,
        },
    };
    assert!(
        storage
            .commit(
                vec![
                    StorageWrite::Entry {
                        value: entry(transient_id, ROOT_CONVERSATION_ID, None),
                    },
                    bad_write,
                ],
                context.clone(),
            )
            .await
            .is_err()
    );
    assert!(storage.entry(transient_id, context.clone()).await.expect("entry").is_none());
    assert!(storage.task(transient_task_id, context.clone()).await.expect("task").is_none());

    let committed_id: EntryId = id_from_number(storage.mint_id().await.expect("mint committed"));
    assert_eq!(
        storage
            .commit(
                vec![StorageWrite::Entry {
                    value: entry(committed_id, ROOT_CONVERSATION_ID, None),
                }],
                context.clone(),
            )
            .await
            .expect("commit valid"),
        seq_from_number(2)
    );
    storage.close(context).await.expect("close");

    let db =
        Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open ro");
    assert_eq!(scalar(&db, "SELECT count(*) FROM entries"), 1);
    assert_eq!(
        scalar(&db, "SELECT next_seq FROM durable_metadata WHERE singleton = 1"),
        3
    );
}

#[tokio::test]
async fn reconstructs_recent_and_ancient_rewindable_points_after_reopen() {
    let context = background_context();
    let (_dir, storage, path) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    create_root(&storage).await;
    let id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));
    let created_at = storage
        .commit(
            vec![StorageWrite::DocumentCreate {
                record: DocumentCreate {
                    id,
                    kind: "history".to_string(),
                    key: None,
                    scope: DocumentScope::Conversation {
                        conversation_id: ROOT_CONVERSATION_ID,
                    },
                    history: Some(DocumentHistory::Rewindable),
                    fork: Some(DocumentFork::AsOf),
                },
                version: 1,
                value: obj(json!({ "count": 0 })),
            }],
            context.clone(),
        )
        .await
        .expect("create doc");

    let mut ancient_at = created_at;
    let mut recent_at = created_at;
    for count in 1..=40 {
        let content = if count == 20 {
            DocumentContent::Base {
                version: 1,
                value: obj(json!({ "count": count })),
            }
        } else {
            DocumentContent::Delta {
                version: 1,
                ops: vec![Op::Set {
                    path: vec!["count".into()],
                    value: json!(count),
                }],
            }
        };
        recent_at = storage
            .commit(vec![StorageWrite::DocumentChange { id, content }], context.clone())
            .await
            .expect("commit change");
        if count == 5 {
            ancient_at = recent_at;
        }
    }
    storage.close(context.clone()).await.expect("close");

    let reopened = open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
        .await
        .expect("reopen");
    assert_eq!(
        reopened
            .document(id, DocumentPoint::At(ancient_at), context.clone())
            .await
            .expect("ancient")
            .expect("present")
            .value,
        obj(json!({ "count": 5 }))
    );
    assert_eq!(
        reopened
            .document(id, DocumentPoint::At(recent_at), context.clone())
            .await
            .expect("recent")
            .expect("present")
            .value,
        obj(json!({ "count": 40 }))
    );
    reopened.close(context).await.expect("close");
}

#[tokio::test]
async fn uses_indexes_for_exact_addresses_exact_scopes_entry_history_and_revision_tails() {
    let context = background_context();
    let (_dir, storage, path) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    storage.close(context).await.expect("close");

    let db =
        Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open ro");
    let plan = |sql: &str, params: &[&dyn rusqlite::ToSql]| -> String {
        let mut stmt = db.prepare(sql).expect("prepare explain");
        let rows = stmt
            .query_map(params, |row| row.get::<_, String>(3))
            .expect("query explain");
        let mut lines = Vec::new();
        for r in rows {
            lines.push(r.expect("detail"));
        }
        lines.join("\n")
    };

    let d0 = plan(
        "EXPLAIN QUERY PLAN SELECT record FROM documents
         WHERE kind = ? AND scope_kind = ? AND owner_id = ? AND family = ? AND key_value = ?
         AND retired_at IS NULL ORDER BY created_at DESC LIMIT 1",
        &[&"kind", &"session", &0i64, &0i64, &""],
    );
    let d1 = plan(
        "EXPLAIN QUERY PLAN SELECT record FROM documents
         WHERE kind = ? AND scope_kind = ? AND owner_id = ? AND family = ? AND key_value = ?
         AND created_at <= ? AND (retired_at IS NULL OR retired_at > ?)
         ORDER BY created_at DESC LIMIT 1",
        &[&"kind", &"conversation", &1i64, &0i64, &"", &10i64, &10i64],
    );
    let d2 = plan(
        "EXPLAIN QUERY PLAN SELECT record FROM documents
         WHERE scope_kind = ? AND owner_id = ? AND kind = ? AND id > ? ORDER BY id LIMIT ?",
        &[&"task", &1i64, &"kind", &0i64, &10i64],
    );
    let d3 = plan(
        "EXPLAIN QUERY PLAN SELECT record FROM entries
         WHERE conversation_id = ? AND id <= ? ORDER BY id DESC LIMIT ?",
        &[&1i64, &10i64, &10i64],
    );
    let d4 = plan(
        "EXPLAIN QUERY PLAN SELECT record FROM entries
         WHERE conversation_id = ? AND head IS NOT NULL AND id <= ? ORDER BY id DESC LIMIT 1",
        &[&1i64, &10i64],
    );
    let d5 = plan(
        "EXPLAIN QUERY PLAN SELECT record FROM tasks WHERE status = ? AND id > ? ORDER BY id LIMIT ?",
        &[&"pending", &0i64, &10i64],
    );
    let d6 = plan(
        "EXPLAIN QUERY PLAN SELECT seq, kind, version, content FROM document_revisions
         WHERE document_id = ? AND kind = 'base' AND seq <= ? ORDER BY seq DESC LIMIT 1",
        &[&1i64, &10i64],
    );
    let d7 = plan(
        "EXPLAIN QUERY PLAN SELECT seq, kind, version, content FROM document_revisions
         WHERE document_id = ? AND seq > ? AND seq <= ? ORDER BY seq",
        &[&1i64, &5i64, &10i64],
    );

    assert!(d0.contains("documents_by_address"), "{d0}");
    assert!(d1.contains("documents_by_address"), "{d1}");
    assert!(d2.contains("documents_by_scope_kind"), "{d2}");
    assert!(d3.contains("entries_by_conversation"), "{d3}");
    assert!(d4.contains("entry_heads_by_conversation"), "{d4}");
    assert!(d5.contains("tasks_by_status"), "{d5}");
    assert!(d6.contains("document_revisions_by_kind"), "{d6}");
    assert!(d7.contains("sqlite_autoindex_document_revisions_1"), "{d7}");
    for detail in [&d0, &d1, &d2] {
        assert!(!detail.contains("SCAN documents"), "{detail}");
    }
    assert!(!d7.contains("SCAN document_revisions"), "{d7}");
}

#[tokio::test]
async fn reclaims_current_only_revisions_only_after_base_or_retirement() {
    let context = background_context();
    let (_dir, storage, path) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    create_root(&storage).await;
    let id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));
    storage
        .commit(
            vec![StorageWrite::DocumentCreate {
                record: DocumentCreate {
                    id,
                    kind: "latest".to_string(),
                    key: None,
                    scope: DocumentScope::Session,
                    history: None,
                    fork: None,
                },
                version: 1,
                value: obj(json!({ "count": 0 })),
            }],
            context.clone(),
        )
        .await
        .expect("create");
    for count in 1..=10 {
        storage
            .commit(
                vec![StorageWrite::DocumentChange {
                    id,
                    content: DocumentContent::Delta {
                        version: 1,
                        ops: vec![Op::Set {
                            path: vec!["count".into()],
                            value: json!(count),
                        }],
                    },
                }],
                context.clone(),
            )
            .await
            .expect("delta");
    }
    assert_eq!(revision_count(&path, id), 11);

    let db =
        Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open ro");
    let content: String = db
        .query_row(
            "SELECT content FROM document_revisions WHERE document_id = ? AND kind = 'delta' ORDER BY seq DESC LIMIT 1",
            [id.as_i64()],
            |row| row.get(0),
        )
        .expect("get latest delta");
    db.close().expect("close ro");
    assert_eq!(
        serde_json::from_str::<JsonValue>(&content).expect("json"),
        json!([["s", ["count"], 10]])
    );

    storage
        .commit(
            vec![StorageWrite::DocumentChange {
                id,
                content: DocumentContent::Base {
                    version: 1,
                    value: obj(json!({ "count": 11 })),
                },
            }],
            context.clone(),
        )
        .await
        .expect("base");
    assert_eq!(revision_count(&path, id), 1);

    storage
        .commit(
            vec![StorageWrite::DocumentChange {
                id,
                content: DocumentContent::Delta {
                    version: 1,
                    ops: vec![Op::ReplaceRoot {
                        value: obj(json!({ "count": 12 })),
                    }],
                },
            }],
            context.clone(),
        )
        .await
        .expect("replace root");
    assert_eq!(revision_count(&path, id), 2);

    storage
        .commit(vec![StorageWrite::DocumentRetire { id }], context.clone())
        .await
        .expect("retire");
    assert_eq!(revision_count(&path, id), 0);
    storage.close(context).await.expect("close");
}

#[tokio::test]
async fn auto_checkpoints_wal_frames_and_truncates_wal_on_close() {
    let context = background_context();
    let (_dir, storage, path) = create_sqlite_storage(NodeSqliteStorageOptions {
        wal_auto_checkpoint_pages: Some(1),
        busy_timeout_ms: None,
    })
    .await;
    create_root(&storage).await;
    let payload = "x".repeat(32 * 1024);
    for index in 0..20 {
        let entry_id: EntryId = id_from_number(storage.mint_id().await.expect("mint"));
        storage
            .commit(
                vec![StorageWrite::Entry {
                    value: entry(
                        entry_id,
                        ROOT_CONVERSATION_ID,
                        Some(json!({ "text": payload, "index": index })),
                    ),
                }],
                context.clone(),
            )
            .await
            .expect("commit large entry");
    }

    let wal_path = format!("{path}-wal");
    let wal_size = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
    assert!(wal_size < 512 * 1024, "unexpected WAL size {wal_size}");

    let observer =
        Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open observer");
    assert_eq!(scalar(&observer, "SELECT count(*) FROM entries"), 20);
    storage.close(context).await.expect("close");
    let wal_after = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
    assert_eq!(wal_after, 0);
}

#[tokio::test]
async fn reuses_pages_released_by_current_only_checkpoints() {
    let context = background_context();
    let (_dir, storage, path) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    create_root(&storage).await;
    let id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));
    let large = "x".repeat(512 * 1024);

    let mut large_obj: JsonObject = Map::new();
    large_obj.insert("text".to_string(), json!(large));
    storage
        .commit(
            vec![StorageWrite::DocumentCreate {
                record: DocumentCreate {
                    id,
                    kind: "reuse".to_string(),
                    key: None,
                    scope: DocumentScope::Session,
                    history: None,
                    fork: None,
                },
                version: 1,
                value: large_obj.clone(),
            }],
            context.clone(),
        )
        .await
        .expect("create large");

    let mut small_obj: JsonObject = Map::new();
    small_obj.insert("text".to_string(), json!("small"));
    storage
        .commit(
            vec![StorageWrite::DocumentChange {
                id,
                content: DocumentContent::Base {
                    version: 1,
                    value: small_obj,
                },
            }],
            context.clone(),
        )
        .await
        .expect("shrink base");

    let before =
        Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open ro");
    let pages_after_delete = scalar(&before, "SELECT page_count FROM pragma_page_count()");
    let free_after_delete = scalar(&before, "SELECT freelist_count FROM pragma_freelist_count()");
    before.close().expect("close before");
    assert!(free_after_delete > 0);

    storage
        .commit(
            vec![StorageWrite::DocumentChange {
                id,
                content: DocumentContent::Base {
                    version: 1,
                    value: large_obj,
                },
            }],
            context.clone(),
        )
        .await
        .expect("grow again");

    let after =
        Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open ro");
    let pages_after_reuse = scalar(&after, "SELECT page_count FROM pragma_page_count()");
    let free_after_reuse = scalar(&after, "SELECT freelist_count FROM pragma_freelist_count()");
    assert!(pages_after_reuse <= pages_after_delete + 2);
    assert!(free_after_reuse < free_after_delete);
    storage.close(context).await.expect("close");
}

#[tokio::test]
async fn keeps_representative_row_and_document_storage_bounded() {
    let context = background_context();
    let (_dir, storage, path) = create_sqlite_storage(NodeSqliteStorageOptions::default()).await;
    create_root(&storage).await;
    let payload = "x".repeat(1024);
    for index in 0..100 {
        let entry_id: EntryId = id_from_number(storage.mint_id().await.expect("mint entry"));
        storage
            .commit(
                vec![StorageWrite::Entry {
                    value: entry(
                        entry_id,
                        ROOT_CONVERSATION_ID,
                        Some(json!({ "index": index, "text": payload })),
                    ),
                }],
                context.clone(),
            )
            .await
            .expect("commit entry");
    }
    let doc_id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));
    storage
        .commit(
            vec![StorageWrite::DocumentCreate {
                record: DocumentCreate {
                    id: doc_id,
                    kind: "size.history".to_string(),
                    key: None,
                    scope: DocumentScope::Conversation {
                        conversation_id: ROOT_CONVERSATION_ID,
                    },
                    history: Some(DocumentHistory::Rewindable),
                    fork: Some(DocumentFork::AsOf),
                },
                version: 1,
                value: obj(json!({ "count": 0 })),
            }],
            context.clone(),
        )
        .await
        .expect("create doc");
    for count in 1..=100 {
        storage
            .commit(
                vec![StorageWrite::DocumentChange {
                    id: doc_id,
                    content: DocumentContent::Delta {
                        version: 1,
                        ops: vec![Op::Set {
                            path: vec!["count".into()],
                            value: json!(count),
                        }],
                    },
                }],
                context.clone(),
            )
            .await
            .expect("delta");
    }
    storage.close(context).await.expect("close");
    let size = std::fs::metadata(&path).expect("stat").len();
    assert!(size < 1024 * 1024, "unexpected file size {size}");
}
