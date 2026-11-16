//! Port of packages/durable/src/storage/sqlite/storage.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_chord::Context;
use pi_chord::delta::{Op, apply_immutable_batches};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;
use tokio::sync::{Notify, OnceCell};

use crate::errors::StorageRejected;
use crate::ids::Seq;
use crate::storage::sqlite::database::{SqliteDatabase, SqliteDatabaseExt, SqliteExecutor, SqliteRowExt, SqliteValue};
use crate::storage::sqlite::migrations::apply_sqlite_migrations;
use crate::types::{
    ConversationId, ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentContent,
    DocumentCopySource, DocumentCreate, DocumentHistory, DocumentId, DocumentPoint, DocumentQuery, DocumentRecord,
    DocumentScope, DocumentScopeKind, EntryId, EntryLookup, EntryQuery, EntryRecord, ErasedTaskRecord, JsonObject,
    Page, Storage, StorageWrite, StoredDocument, SubmissionId, SubmissionQuery, SubmissionRecord, TaskId, TaskQuery,
    TaskStatus,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TableName {
    Conversation,
    Entry,
    Task,
    Submission,
    Document,
}

impl TableName {
    fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Entry => "entry",
            Self::Task => "task",
            Self::Submission => "submission",
            Self::Document => "document",
        }
    }

    fn parse(raw: &str) -> pi_js::Result<Self> {
        match raw {
            "conversation" => Ok(Self::Conversation),
            "entry" => Ok(Self::Entry),
            "task" => Ok(Self::Task),
            "submission" => Ok(Self::Submission),
            "document" => Ok(Self::Document),
            other => Err(pi_js::Error::msg(format!("Unknown record_type '{other}'"))),
        }
    }
}

#[derive(Clone, Debug)]
struct DocumentAction {
    create: Option<DocumentCreate>,
    copy: Option<DocumentCopySource>,
    content: Option<DocumentContent>,
    retire: bool,
}

#[derive(Debug)]
struct SqliteRuntimeState {
    next_id: i64,
    closed: bool,
    admitted_reads: usize,
}

/// Guard that decrements `admitted_reads` even if an async read future is cancelled.
struct AdmitReadGuard<'a> {
    state: &'a Mutex<SqliteRuntimeState>,
    reads_drained: &'a Notify,
}

impl Drop for AdmitReadGuard<'_> {
    fn drop(&mut self) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.admitted_reads = guard.admitted_reads.saturating_sub(1);
        if guard.admitted_reads == 0 {
            self.reads_drained.notify_waiters();
        }
    }
}

/// Portable SQLite implementation of the durable [`Storage`] contract.
pub struct SqliteStorage {
    db: Arc<dyn SqliteDatabase>,
    state: Mutex<SqliteRuntimeState>,
    reads_drained: Notify,
    close_result: OnceCell<std::result::Result<(), String>>,
}

impl SqliteStorage {
    /// Initialize storage over an owned SQLite database facade.
    pub async fn open(db: Arc<dyn SqliteDatabase>) -> pi_js::Result<Self> {
        let init_result: pi_js::Result<i64> = async {
            apply_sqlite_migrations(db.as_ref(), None).await?;
            let metadata = db
                .get(
                    "SELECT next_id, next_seq FROM durable_metadata WHERE singleton = 1",
                    &[],
                )
                .await?
                .ok_or_else(|| pi_js::Error::msg("Durable SQLite metadata is missing"))?;
            let next_id_str = metadata.get_str("next_id")?;
            let next_id = next_id_str
                .parse::<i64>()
                .map_err(|err| pi_js::Error::msg(format!("Invalid durable_metadata.next_id '{next_id_str}': {err}")))?;
            Ok(next_id)
        }
        .await;

        match init_result {
            Ok(next_id) => Ok(Self {
                db,
                state: Mutex::new(SqliteRuntimeState {
                    next_id,
                    closed: false,
                    admitted_reads: 0,
                }),
                reads_drained: Notify::new(),
                close_result: OnceCell::new(),
            }),
            Err(err) => {
                let _ = db.close().await;
                Err(err)
            }
        }
    }

    fn assert_open(&self) -> pi_js::Result<()> {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.closed {
            return Err(pi_js::Error::msg("SqliteStorage is closed"));
        }
        Ok(())
    }

    async fn admit_read<T, F, Fut>(&self, read: F) -> pi_js::Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = pi_js::Result<T>>,
    {
        {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.closed {
                return Err(pi_js::Error::msg("SqliteStorage is closed"));
            }
            guard.admitted_reads += 1;
        }
        let _guard = AdmitReadGuard {
            state: &self.state,
            reads_drained: &self.reads_drained,
        };
        read().await
    }

    fn candidate_next_id(&self, writes: &[StorageWrite]) -> i64 {
        let mut next_id = self.state.lock().unwrap_or_else(|e| e.into_inner()).next_id;
        for write in writes {
            if let Some(id) = write_id(write) {
                next_id = next_id.max(id + 1);
            }
        }
        next_id
    }

    async fn read_entry(
        &self,
        conversation_id: Option<ConversationId>,
        id: EntryId,
    ) -> pi_js::Result<Option<EntryLookup>> {
        let mut conv: Option<ConversationRecord> = None;
        if let Some(cid) = conversation_id {
            let loaded = read_conversation(self.db.as_ref(), cid)
                .await?
                .ok_or_else(|| pi_js::Error::msg(format!("Unknown conversation: {cid}")))?;
            conv = Some(loaded);
        }

        let Some(row) = self
            .db
            .get(
                "SELECT record, commit_seq FROM entries WHERE id = ?",
                &[SqliteValue::from(id)],
            )
            .await?
        else {
            return Ok(None);
        };
        let entry: EntryRecord = parse_json(row.get_str("record")?)?;
        let commit_seq = Seq::new_unchecked(row.get_i64("commit_seq")?);

        if let Some(mut conversation) = conv {
            let mut upper_entry_id = i64::MAX;
            while conversation.id != entry.conversation_id {
                let Some(parent) = conversation.parent else {
                    return Ok(None);
                };
                upper_entry_id = upper_entry_id.min(parent.at.as_i64());
                conversation = read_conversation(self.db.as_ref(), parent.conversation_id)
                    .await?
                    .ok_or_else(|| {
                        pi_js::Error::msg(format!("Unknown ancestor conversation: {}", parent.conversation_id))
                    })?;
            }
            if entry.id.as_i64() > upper_entry_id {
                return Ok(None);
            }
        }

        Ok(Some(EntryLookup { entry, commit_seq }))
    }

    async fn read_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
    ) -> pi_js::Result<Option<EntryRecord>> {
        let mut conversation = read_conversation(self.db.as_ref(), conversation_id)
            .await?
            .ok_or_else(|| pi_js::Error::msg(format!("Unknown conversation: {conversation_id}")))?;
        let mut upper = at_or_before_entry_id.map(|id| id.as_i64());

        loop {
            let row = match upper {
                None => {
                    self.db
                        .get(
                            "SELECT record FROM entries WHERE conversation_id = ? AND head IS NOT NULL ORDER BY id DESC LIMIT 1",
                            &[SqliteValue::from(conversation.id)],
                        )
                        .await?
                }
                Some(u) => {
                    self.db
                        .get(
                            "SELECT record FROM entries WHERE conversation_id = ? AND head IS NOT NULL AND id <= ? ORDER BY id DESC LIMIT 1",
                            &[SqliteValue::from(conversation.id), SqliteValue::from(u)],
                        )
                        .await?
                }
            };
            if let Some(row) = row {
                return Ok(Some(parse_json(row.get_str("record")?)?));
            }
            let Some(parent) = conversation.parent else {
                return Ok(None);
            };
            let parent_at = parent.at.as_i64();
            upper = Some(match upper {
                Some(u) => u.min(parent_at),
                None => parent_at,
            });
            conversation = read_conversation(self.db.as_ref(), parent.conversation_id)
                .await?
                .ok_or_else(|| {
                    pi_js::Error::msg(format!("Unknown ancestor conversation: {}", parent.conversation_id))
                })?;
        }
    }

    async fn read_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
    ) -> pi_js::Result<Page<EntryRecord, Cursor>> {
        let mut conversation = read_conversation(self.db.as_ref(), query.conversation_id)
            .await?
            .ok_or_else(|| pi_js::Error::msg(format!("Unknown conversation: {}", query.conversation_id)))?;
        let after = cursor_id(cursor.as_ref())?;
        let mut upper = query.max_entry_id.map(|id| id.as_i64());
        if let Some(a) = after {
            let cap = a.saturating_sub(1);
            upper = Some(match upper {
                Some(u) => u.min(cap),
                None => cap,
            });
        }
        let mut values: Vec<EntryRecord> = Vec::new();
        loop {
            let mut clauses = vec!["conversation_id = ?"];
            let mut params: Vec<SqliteValue> = vec![SqliteValue::from(conversation.id)];
            if let Some(min_id) = query.min_entry_id {
                clauses.push("id >= ?");
                params.push(SqliteValue::from(min_id));
            }
            if let Some(u) = upper {
                clauses.push("id <= ?");
                params.push(SqliteValue::from(u));
            }
            let remaining = (limit + 1).saturating_sub(values.len()) as i64;
            params.push(SqliteValue::from(remaining));
            let sql = format!(
                "SELECT record FROM entries WHERE {} ORDER BY id DESC LIMIT ?",
                clauses.join(" AND ")
            );
            let rows = self.db.all(&sql, &params).await?;
            for row in rows {
                values.push(parse_json(row.get_str("record")?)?);
            }
            if values.len() > limit {
                break;
            }
            let Some(parent) = conversation.parent else {
                break;
            };
            let parent_at = parent.at.as_i64();
            let next_upper = match upper {
                Some(u) => u.min(parent_at),
                None => parent_at,
            };
            upper = Some(next_upper);
            if let Some(min_id) = query.min_entry_id
                && next_upper < min_id.as_i64()
            {
                break;
            }
            conversation = read_conversation(self.db.as_ref(), parent.conversation_id)
                .await?
                .ok_or_else(|| {
                    pi_js::Error::msg(format!("Unknown ancestor conversation: {}", parent.conversation_id))
                })?;
        }
        Ok(build_page(values, limit, |e| e.id.as_i64()))
    }
}

#[async_trait]
impl Storage for SqliteStorage {
    async fn commit(&self, writes: Vec<StorageWrite>, _context: Context) -> pi_js::Result<Seq> {
        self.assert_open()?;
        let document_actions = prepare_document_actions(&writes)?;
        let candidate_next_id = self.candidate_next_id(&writes);

        let seq = self
            .db
            .transaction(move |transaction| {
                let writes = writes;
                let document_actions = document_actions;
                Box::pin(async move {
                    let metadata = transaction
                        .get(
                            "SELECT next_id, next_seq FROM durable_metadata WHERE singleton = 1",
                            &[],
                        )
                        .await?
                        .ok_or_else(|| pi_js::Error::msg("Durable SQLite metadata is missing"))?;
                    let meta_next_id = metadata
                        .get_str("next_id")?
                        .parse::<i64>()
                        .map_err(|err| pi_js::Error::msg(format!("Invalid durable_metadata.next_id: {err}")))?;
                    let committed_seq = Seq::new_unchecked(metadata.get_i64("next_seq")?);

                    check_global_ids(transaction.as_ref(), &writes).await?;
                    check_document_actions(transaction.as_ref(), &document_actions).await?;

                    for write in &writes {
                        apply_table_write(transaction.as_ref(), write, committed_seq).await?;
                    }
                    apply_document_actions(transaction.as_ref(), &document_actions, committed_seq).await?;

                    let updated_next_id = meta_next_id.max(candidate_next_id).to_string();
                    transaction
                        .run(
                            "UPDATE durable_metadata SET next_id = ?, next_seq = ? WHERE singleton = 1",
                            &[
                                SqliteValue::Text(updated_next_id),
                                SqliteValue::Integer(committed_seq.as_i64() + 1),
                            ],
                        )
                        .await?;
                    Ok(committed_seq)
                })
            })
            .await?;

        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.next_id = guard.next_id.max(candidate_next_id);
        Ok(seq)
    }

    async fn mint_id(&self) -> pi_js::Result<i64> {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.closed {
            return Err(pi_js::Error::msg("SqliteStorage is closed"));
        }
        if guard.next_id > pi_js::num::MAX_SAFE_INTEGER {
            return Err(pi_js::Error::msg("ID space is exhausted"));
        }
        let id = guard.next_id;
        guard.next_id += 1;
        Ok(id)
    }

    async fn conversation(&self, id: ConversationId, _context: Context) -> pi_js::Result<Option<ConversationRecord>> {
        self.assert_open()?;
        read_conversation(self.db.as_ref(), id).await
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: Context,
    ) -> pi_js::Result<Page<ConversationRecord, Cursor>> {
        self.assert_open()?;
        let after = cursor_id(cursor.as_ref())?.unwrap_or(-1);
        let mut clauses = vec!["id > ?"];
        let mut params: Vec<SqliteValue> = vec![SqliteValue::from(after)];
        if let Some(owner_cid) = query.owner_conversation_id {
            clauses.push("owner_conversation_id = ?");
            params.push(SqliteValue::from(owner_cid));
        }
        if let Some(owner_tid) = query.owner_task_id {
            clauses.push("owner_task_id = ?");
            params.push(SqliteValue::from(owner_tid));
        }
        params.push(SqliteValue::from((limit + 1) as i64));
        let sql = format!(
            "SELECT record FROM conversations WHERE {} ORDER BY id LIMIT ?",
            clauses.join(" AND ")
        );
        let rows = self.db.all(&sql, &params).await?;
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            records.push(parse_json(row.get_str("record")?)?);
        }
        Ok(build_page(records, limit, |c| c.id.as_i64()))
    }

    async fn entry(&self, id: EntryId, _context: Context) -> pi_js::Result<Option<EntryLookup>> {
        self.admit_read(|| self.read_entry(None, id)).await
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        _context: Context,
    ) -> pi_js::Result<Option<EntryLookup>> {
        self.admit_read(|| self.read_entry(Some(conversation_id), id)).await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        _context: Context,
    ) -> pi_js::Result<Option<EntryRecord>> {
        self.admit_read(|| self.read_latest_head_marker(conversation_id, at_or_before_entry_id))
            .await
    }

    async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: Context,
    ) -> pi_js::Result<Page<EntryRecord, Cursor>> {
        self.admit_read(|| self.read_entries(query, limit, cursor)).await
    }

    async fn task(&self, id: TaskId, _context: Context) -> pi_js::Result<Option<ErasedTaskRecord>> {
        self.assert_open()?;
        let row = self
            .db
            .get("SELECT record FROM tasks WHERE id = ?", &[SqliteValue::from(id)])
            .await?;
        match row {
            Some(r) => Ok(Some(parse_json(r.get_str("record")?)?)),
            None => Ok(None),
        }
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: Context,
    ) -> pi_js::Result<Page<ErasedTaskRecord, Cursor>> {
        self.assert_open()?;
        let after = cursor_id(cursor.as_ref())?.unwrap_or(-1);
        let mut clauses = vec!["id > ?"];
        let mut params: Vec<SqliteValue> = vec![SqliteValue::from(after)];
        if let Some(conv_id) = query.conversation_id {
            clauses.push("conversation_id = ?");
            params.push(SqliteValue::from(conv_id));
        }
        if let Some(ref kind) = query.kind {
            clauses.push("kind = ?");
            params.push(SqliteValue::Text(encode_indexed_string(kind)?));
        }
        if let Some(status) = query.status {
            clauses.push("status = ?");
            params.push(SqliteValue::Text(task_status_str(status).to_string()));
        }
        if let Some(abort_requested) = query.abort_requested {
            clauses.push("abort_requested = ?");
            params.push(SqliteValue::from(abort_requested));
        }
        if let Some(background) = query.background {
            clauses.push("background = ?");
            params.push(SqliteValue::from(background));
        }
        params.push(SqliteValue::from((limit + 1) as i64));
        let sql = format!(
            "SELECT record FROM tasks WHERE {} ORDER BY id LIMIT ?",
            clauses.join(" AND ")
        );
        let rows = self.db.all(&sql, &params).await?;
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            records.push(parse_json(row.get_str("record")?)?);
        }
        Ok(build_page(records, limit, |t| t.id.as_i64()))
    }

    async fn submission(&self, id: SubmissionId, _context: Context) -> pi_js::Result<Option<SubmissionRecord>> {
        self.assert_open()?;
        let row = self
            .db
            .get("SELECT record FROM submissions WHERE id = ?", &[SqliteValue::from(id)])
            .await?;
        match row {
            Some(r) => Ok(Some(parse_json(r.get_str("record")?)?)),
            None => Ok(None),
        }
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: Context,
    ) -> pi_js::Result<Page<SubmissionRecord, Cursor>> {
        self.assert_open()?;
        let after = cursor_id(cursor.as_ref())?.unwrap_or(-1);
        let mut clauses = vec!["id > ?"];
        let mut params: Vec<SqliteValue> = vec![SqliteValue::from(after)];
        if let Some(conv_id) = query.conversation_id {
            clauses.push("conversation_id = ?");
            params.push(SqliteValue::from(conv_id));
        }
        if let Some(status) = query.status {
            clauses.push("status = ?");
            params.push(SqliteValue::Text(submission_status_str(status).to_string()));
        }
        params.push(SqliteValue::from((limit + 1) as i64));
        let sql = format!(
            "SELECT record FROM submissions WHERE {} ORDER BY id LIMIT ?",
            clauses.join(" AND ")
        );
        let rows = self.db.all(&sql, &params).await?;
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            records.push(parse_json(row.get_str("record")?)?);
        }
        Ok(build_page(records, limit, |s| s.id.as_i64()))
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        _context: Context,
    ) -> pi_js::Result<Option<SubmissionRecord>> {
        self.assert_open()?;
        let row = self
            .db
            .get(
                "SELECT record FROM submissions WHERE conversation_id = ? AND request_id = ?",
                &[
                    SqliteValue::from(conversation_id),
                    SqliteValue::Text(encode_indexed_string(request_id)?),
                ],
            )
            .await?;
        match row {
            Some(r) => Ok(Some(parse_json(r.get_str("record")?)?)),
            None => Ok(None),
        }
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        _context: Context,
    ) -> pi_js::Result<Option<DocumentRecord>> {
        self.assert_open()?;
        let parts = address_parts(&address.kind, &address.scope, address.key.as_deref())?;
        let mut params: Vec<SqliteValue> = vec![
            SqliteValue::Text(parts.kind),
            SqliteValue::Text(parts.scope_kind.to_string()),
            SqliteValue::Integer(parts.owner_id),
            SqliteValue::Integer(parts.family),
            SqliteValue::Text(parts.key_value),
        ];
        let sql = match at {
            DocumentPoint::Current => {
                "SELECT record FROM documents
                 WHERE kind = ? AND scope_kind = ? AND owner_id = ? AND family = ? AND key_value = ?
                 AND retired_at IS NULL ORDER BY created_at DESC LIMIT 1"
            }
            DocumentPoint::At(seq) => {
                params.push(SqliteValue::from(seq));
                params.push(SqliteValue::from(seq));
                "SELECT record FROM documents
                 WHERE kind = ? AND scope_kind = ? AND owner_id = ? AND family = ? AND key_value = ?
                 AND created_at <= ? AND (retired_at IS NULL OR retired_at > ?)
                 ORDER BY created_at DESC LIMIT 1"
            }
        };
        let row = self.db.get(sql, &params).await?;
        match row {
            Some(r) => Ok(Some(parse_json(r.get_str("record")?)?)),
            None => Ok(None),
        }
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        _context: Context,
    ) -> pi_js::Result<Option<StoredDocument>> {
        self.assert_open()?;
        self.db
            .transaction(move |transaction| {
                Box::pin(async move { materialize_document(transaction.as_ref(), id, at).await })
            })
            .await
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: Context,
    ) -> pi_js::Result<Page<DocumentRecord, Cursor>> {
        self.assert_open()?;
        let (scope_kind, owner_id) = scope_columns(&query.scope);
        let after = cursor_id(cursor.as_ref())?.unwrap_or(-1);
        let mut clauses = vec!["scope_kind = ?", "owner_id = ?", "id > ?"];
        let mut params: Vec<SqliteValue> = vec![
            SqliteValue::Text(scope_kind.to_string()),
            SqliteValue::Integer(owner_id),
            SqliteValue::Integer(after),
        ];
        if let Some(ref kind) = query.kind {
            clauses.push("kind = ?");
            params.push(SqliteValue::Text(encode_indexed_string(kind)?));
        }
        match query.at {
            DocumentPoint::Current => {
                clauses.push("retired_at IS NULL");
            }
            DocumentPoint::At(seq) => {
                clauses.push("created_at <= ?");
                clauses.push("(retired_at IS NULL OR retired_at > ?)");
                params.push(SqliteValue::from(seq));
                params.push(SqliteValue::from(seq));
            }
        }
        params.push(SqliteValue::from((limit + 1) as i64));
        let sql = format!(
            "SELECT record FROM documents WHERE {} ORDER BY id LIMIT ?",
            clauses.join(" AND ")
        );
        let rows = self.db.all(&sql, &params).await?;
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            records.push(parse_json(row.get_str("record")?)?);
        }
        Ok(build_page(records, limit, |d| d.id.as_i64()))
    }

    async fn close(&self, _context: Context) -> pi_js::Result<()> {
        {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.closed = true;
        }
        let outcome = self
            .close_result
            .get_or_init(|| async {
                loop {
                    let notified = self.reads_drained.notified();
                    let active = self.state.lock().unwrap_or_else(|e| e.into_inner()).admitted_reads;
                    if active == 0 {
                        break;
                    }
                    notified.await;
                }
                self.db.close().await.map_err(|err| err.to_string())
            })
            .await;
        match outcome {
            Ok(()) => Ok(()),
            Err(msg) => Err(pi_js::Error::msg(msg.clone())),
        }
    }
}

fn parse_json<T: DeserializeOwned>(value: &str) -> pi_js::Result<T> {
    serde_json::from_str::<T>(value).map_err(|err| pi_js::Error::msg(format!("Failed to parse stored JSON: {err}")))
}

fn encode_json<T: Serialize>(value: &T) -> pi_js::Result<String> {
    pi_js::json::stringify(value)
}

fn encode_indexed_string(value: &str) -> pi_js::Result<String> {
    pi_js::json::stringify(&value)
}

fn cursor_id(cursor: Option<&Cursor>) -> pi_js::Result<Option<i64>> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    let Some(after) = cursor.get("after") else {
        return Ok(None);
    };
    if let Some(n) = after.as_i64() {
        return Ok(Some(n));
    }
    if let Some(n) = after.as_f64()
        && n.is_finite()
        && n.fract() == 0.0
        && n.abs() <= pi_js::num::MAX_SAFE_INTEGER as f64
    {
        return Ok(Some(n as i64));
    }
    Err(pi_js::Error::js("TypeError", "Invalid storage cursor"))
}

fn build_page<T>(mut values: Vec<T>, limit: usize, id_of: impl Fn(&T) -> i64) -> Page<T, Cursor> {
    if values.len() <= limit {
        return Page {
            items: values,
            next: None,
        };
    }
    values.truncate(limit);
    let next = values.last().map(|last| {
        let mut map = Cursor::new();
        map.insert("after".to_string(), JsonValue::from(id_of(last)));
        map
    });
    Page { items: values, next }
}

fn scope_columns(scope: &DocumentScope) -> (&'static str, i64) {
    match scope {
        DocumentScope::Session => ("session", 0),
        DocumentScope::Conversation { conversation_id } => ("conversation", conversation_id.as_i64()),
        DocumentScope::Task { task_id } => ("task", task_id.as_i64()),
    }
}

struct AddressParts {
    kind: String,
    scope_kind: &'static str,
    owner_id: i64,
    family: i64,
    key_value: String,
}

fn address_parts(kind: &str, scope: &DocumentScope, key: Option<&str>) -> pi_js::Result<AddressParts> {
    let (scope_kind, owner_id) = scope_columns(scope);
    Ok(AddressParts {
        kind: encode_indexed_string(kind)?,
        scope_kind,
        owner_id,
        family: if key.is_none() { 0 } else { 1 },
        key_value: encode_indexed_string(key.unwrap_or(""))?,
    })
}

fn address_key(kind: &str, scope: &DocumentScope, key: Option<&str>) -> pi_js::Result<String> {
    let parts = address_parts(kind, scope, key)?;
    pi_js::json::stringify(&(
        parts.kind,
        parts.scope_kind,
        parts.owner_id,
        parts.family,
        parts.key_value,
    ))
}

fn is_alive_at(record: &DocumentRecord, at: DocumentPoint) -> bool {
    match at {
        DocumentPoint::Current => record.retired_at.is_none(),
        DocumentPoint::At(seq) => {
            let s = seq.as_i64();
            record.created_at.as_i64() <= s && record.retired_at.is_none_or(|retired| s < retired.as_i64())
        }
    }
}

fn is_current_only(record: &DocumentRecord) -> bool {
    record.scope.kind() != DocumentScopeKind::Conversation || record.history == Some(DocumentHistory::Latest)
}

fn write_id(write: &StorageWrite) -> Option<i64> {
    match write {
        StorageWrite::Conversation { value } => Some(value.id.as_i64()),
        StorageWrite::Entry { value } => Some(value.id.as_i64()),
        StorageWrite::Task { value } => Some(value.id.as_i64()),
        StorageWrite::Submission { value } => Some(value.id.as_i64()),
        StorageWrite::DocumentCreate { record, .. } | StorageWrite::DocumentCopy { record, .. } => {
            Some(record.id.as_i64())
        }
        StorageWrite::DocumentChange { .. } | StorageWrite::DocumentRetire { .. } => None,
    }
}

fn task_status_str(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::Waiting => "waiting",
        TaskStatus::Completing => "completing",
        TaskStatus::Terminal => "terminal",
    }
}

fn submission_status_str(status: crate::types::SubmissionStatus) -> &'static str {
    match status {
        crate::types::SubmissionStatus::Queued => "queued",
        crate::types::SubmissionStatus::Placed => "placed",
        crate::types::SubmissionStatus::Done => "done",
        crate::types::SubmissionStatus::Unanswered => "unanswered",
    }
}

async fn read_conversation(
    executor: &dyn SqliteExecutor,
    id: ConversationId,
) -> pi_js::Result<Option<ConversationRecord>> {
    let row = executor
        .get(
            "SELECT record FROM conversations WHERE id = ?",
            &[SqliteValue::from(id)],
        )
        .await?;
    match row {
        Some(r) => Ok(Some(parse_json(r.get_str("record")?)?)),
        None => Ok(None),
    }
}

async fn materialize_document(
    executor: &dyn SqliteExecutor,
    id: DocumentId,
    at: DocumentPoint,
) -> pi_js::Result<Option<StoredDocument>> {
    let Some(row) = executor
        .get("SELECT record FROM documents WHERE id = ?", &[SqliteValue::from(id)])
        .await?
    else {
        return Ok(None);
    };
    let record: DocumentRecord = parse_json(row.get_str("record")?)?;
    if at != DocumentPoint::Current && is_current_only(&record) {
        return Err(pi_js::Error::msg(format!(
            "Document {id} does not retain historical content"
        )));
    }
    if !is_alive_at(&record, at) {
        return Ok(None);
    }

    let upper = match at {
        DocumentPoint::Current => i64::MAX,
        DocumentPoint::At(seq) => seq.as_i64(),
    };
    let base = executor
        .get(
            "SELECT seq, kind, version, content FROM document_revisions
             WHERE document_id = ? AND kind = 'base' AND seq <= ? ORDER BY seq DESC LIMIT 1",
            &[SqliteValue::from(id), SqliteValue::Integer(upper)],
        )
        .await?
        .ok_or_else(|| pi_js::Error::msg(format!("Document {id} is missing a required base")))?;

    let base_seq = base.get_i64("seq")?;
    let base_version = base.get_i64("version")?;
    let base_value: JsonObject = parse_json(base.get_str("content")?)?;

    let tail = executor
        .all(
            "SELECT seq, kind, version, content FROM document_revisions
             WHERE document_id = ? AND seq > ? AND seq <= ? ORDER BY seq",
            &[
                SqliteValue::from(id),
                SqliteValue::Integer(base_seq),
                SqliteValue::Integer(upper),
            ],
        )
        .await?;

    let mut ops_batches: Vec<Vec<Op>> = Vec::with_capacity(tail.len());
    for rev in &tail {
        if rev.get_str("kind")? != "delta" || rev.get_i64("version")? != base_version {
            return Err(pi_js::Error::msg(format!(
                "Document {id} crosses a stored version boundary without a base"
            )));
        }
        ops_batches.push(parse_json(rev.get_str("content")?)?);
    }

    let deltas_since_base = ops_batches.len();
    let applied = apply_immutable_batches::<JsonValue, _, _>(
        Some(JsonValue::Object(base_value)),
        ops_batches.iter().map(Vec::as_slice),
    )?;
    let JsonValue::Object(value) = applied else {
        return Err(pi_js::Error::msg(format!(
            "Document {id} did not materialize to a JSON object"
        )));
    };

    Ok(Some(StoredDocument {
        record,
        version: base_version,
        value,
        deltas_since_base,
    }))
}

async fn check_global_ids(executor: &dyn SqliteExecutor, writes: &[StorageWrite]) -> pi_js::Result<()> {
    let mut claimed: IndexMap<i64, TableName> = IndexMap::new();
    for write in writes {
        let (id, table) = match write {
            StorageWrite::Conversation { value } => (value.id.as_i64(), TableName::Conversation),
            StorageWrite::Entry { value } => (value.id.as_i64(), TableName::Entry),
            StorageWrite::Task { value } => (value.id.as_i64(), TableName::Task),
            StorageWrite::Submission { value } => (value.id.as_i64(), TableName::Submission),
            StorageWrite::DocumentCreate { record, .. } | StorageWrite::DocumentCopy { record, .. } => {
                (record.id.as_i64(), TableName::Document)
            }
            StorageWrite::DocumentChange { .. } | StorageWrite::DocumentRetire { .. } => continue,
        };

        let existing = match executor
            .get(
                "SELECT record_type FROM record_ids WHERE id = ?",
                &[SqliteValue::Integer(id)],
            )
            .await?
        {
            Some(row) => Some(TableName::parse(row.get_str("record_type")?)?),
            None => None,
        };
        let earlier = claimed.get(&id).copied();

        if matches!(table, TableName::Conversation | TableName::Entry | TableName::Document) {
            if let Some(ex) = existing {
                return Err(pi_js::Error::msg(format!("ID {id} already belongs to {}", ex.as_str())));
            }
            if earlier.is_some() {
                return Err(pi_js::Error::msg(format!("ID {id} is written more than once")));
            }
        } else {
            if let Some(ex) = existing
                && ex != table
            {
                return Err(pi_js::Error::msg(format!("ID {id} already belongs to {}", ex.as_str())));
            }
            if let Some(ea) = earlier
                && ea != table
            {
                return Err(pi_js::Error::msg(format!("ID {id} is written as two record types")));
            }
        }
        claimed.insert(id, table);
    }
    Ok(())
}

fn prepare_document_actions(writes: &[StorageWrite]) -> pi_js::Result<IndexMap<DocumentId, DocumentAction>> {
    let mut actions: IndexMap<DocumentId, DocumentAction> = IndexMap::new();
    for write in writes {
        match write {
            StorageWrite::DocumentCreate { record, version, value } => {
                let action = actions.entry(record.id).or_insert(DocumentAction {
                    create: None,
                    copy: None,
                    content: None,
                    retire: false,
                });
                if action.create.is_some() || action.content.is_some() || action.copy.is_some() {
                    return Err(pi_js::Error::msg(format!(
                        "Document {} has more than one content command",
                        record.id
                    )));
                }
                action.create = Some(record.clone());
                action.content = Some(DocumentContent::Base {
                    version: *version,
                    value: value.clone(),
                });
            }
            StorageWrite::DocumentCopy { record, source } => {
                let action = actions.entry(record.id).or_insert(DocumentAction {
                    create: None,
                    copy: None,
                    content: None,
                    retire: false,
                });
                if action.create.is_some() || action.content.is_some() || action.copy.is_some() {
                    return Err(pi_js::Error::msg(format!(
                        "Document {} has more than one content command",
                        record.id
                    )));
                }
                action.create = Some(record.clone());
                action.copy = Some(*source);
            }
            StorageWrite::DocumentChange { id, content } => {
                let action = actions.entry(*id).or_insert(DocumentAction {
                    create: None,
                    copy: None,
                    content: None,
                    retire: false,
                });
                if action.content.is_some() || action.copy.is_some() {
                    return Err(pi_js::Error::msg(format!(
                        "Document {id} has more than one content command"
                    )));
                }
                action.content = Some(content.clone());
            }
            StorageWrite::DocumentRetire { id } => {
                let action = actions.entry(*id).or_insert(DocumentAction {
                    create: None,
                    copy: None,
                    content: None,
                    retire: false,
                });
                if action.retire {
                    return Err(pi_js::Error::msg(format!("Document {id} is retired more than once")));
                }
                action.retire = true;
            }
            _ => {}
        }
    }
    Ok(actions)
}

async fn check_document_actions(
    executor: &dyn SqliteExecutor,
    actions: &IndexMap<DocumentId, DocumentAction>,
) -> pi_js::Result<()> {
    let mut live_counts: IndexMap<String, i64> = IndexMap::new();
    for (&id, action) in actions {
        if let Some(copy) = action.copy
            && actions.contains_key(&copy.id)
        {
            return Err(
                StorageRejected::new(format!("Document copy {id} source is changed in the copy batch")).into_error(),
            );
        }
        let row = executor
            .get("SELECT record FROM documents WHERE id = ?", &[SqliteValue::from(id)])
            .await?;
        let existing: Option<DocumentRecord> = match row {
            Some(r) => Some(parse_json(r.get_str("record")?)?),
            None => None,
        };
        if action.create.is_none() && existing.is_none() {
            return Err(pi_js::Error::msg(format!("Unknown document: {id}")));
        }
        if action.create.is_some() && existing.is_some() {
            return Err(pi_js::Error::msg(format!("Document {id} already exists")));
        }
        if let Some(ref ex) = existing
            && ex.retired_at.is_some()
        {
            return Err(pi_js::Error::msg(format!("Document {id} is retired")));
        }
        if let Some(DocumentContent::Delta { version, .. }) = &action.content {
            let previous = executor
                .get(
                    "SELECT version FROM document_revisions WHERE document_id = ? ORDER BY seq DESC LIMIT 1",
                    &[SqliteValue::from(id)],
                )
                .await?;
            let Some(prev_row) = previous else {
                return Err(pi_js::Error::msg(format!("Document {id} delta has no base")));
            };
            if prev_row.get_i64("version")? != *version {
                return Err(pi_js::Error::msg(format!(
                    "Document {id} version transition requires a base"
                )));
            }
        }

        let key = match &action.create {
            Some(create) => address_key(&create.kind, &create.scope, create.key.as_deref())?,
            None => {
                let Some(ex) = existing.as_ref() else {
                    return Err(pi_js::Error::msg(format!("Unknown document: {id}")));
                };
                address_key(&ex.kind, &ex.scope, ex.key.as_deref())?
            }
        };

        let live = match live_counts.get(&key) {
            Some(&v) => v,
            None => {
                let existing_live = match &action.create {
                    Some(create) => {
                        current_document_id(executor, &create.kind, &create.scope, create.key.as_deref()).await?
                    }
                    None => {
                        let Some(ex) = existing.as_ref() else {
                            return Err(pi_js::Error::msg(format!("Unknown document: {id}")));
                        };
                        current_document_id(executor, &ex.kind, &ex.scope, ex.key.as_deref()).await?
                    }
                };
                if existing_live.is_some() { 1 } else { 0 }
            }
        };
        let mut updated_live = live;
        if action.retire && existing.is_some() {
            updated_live -= 1;
        }
        if action.create.is_some() && !action.retire {
            updated_live += 1;
        }
        live_counts.insert(key, updated_live);
    }

    for live in live_counts.values() {
        if *live > 1 {
            return Err(pi_js::Error::msg("Document address already has a current incarnation"));
        }
    }
    Ok(())
}

async fn current_document_id(
    executor: &dyn SqliteExecutor,
    kind: &str,
    scope: &DocumentScope,
    key: Option<&str>,
) -> pi_js::Result<Option<DocumentId>> {
    let parts = address_parts(kind, scope, key)?;
    let row = executor
        .get(
            "SELECT id FROM documents
             WHERE kind = ? AND scope_kind = ? AND owner_id = ? AND family = ? AND key_value = ? AND retired_at IS NULL
             LIMIT 1",
            &[
                SqliteValue::Text(parts.kind),
                SqliteValue::Text(parts.scope_kind.to_string()),
                SqliteValue::Integer(parts.owner_id),
                SqliteValue::Integer(parts.family),
                SqliteValue::Text(parts.key_value),
            ],
        )
        .await?;
    match row {
        Some(r) => Ok(Some(DocumentId::new_unchecked(r.get_i64("id")?))),
        None => Ok(None),
    }
}

async fn claim_id(executor: &dyn SqliteExecutor, id: i64, table: TableName) -> pi_js::Result<()> {
    executor
        .run(
            "INSERT OR IGNORE INTO record_ids (id, record_type) VALUES (?, ?)",
            &[SqliteValue::Integer(id), SqliteValue::Text(table.as_str().to_string())],
        )
        .await
}

async fn apply_table_write(executor: &dyn SqliteExecutor, write: &StorageWrite, seq: Seq) -> pi_js::Result<()> {
    match write {
        StorageWrite::Conversation { value } => {
            claim_id(executor, value.id.as_i64(), TableName::Conversation).await?;
            executor
                .run(
                    "INSERT INTO conversations (id, owner_conversation_id, owner_task_id, record) VALUES (?, ?, ?, ?)",
                    &[
                        SqliteValue::from(value.id),
                        SqliteValue::from(value.owner.map(|o| o.conversation_id)),
                        SqliteValue::from(value.owner.map(|o| o.task_id)),
                        SqliteValue::Text(encode_json(value)?),
                    ],
                )
                .await
        }
        StorageWrite::Entry { value } => {
            claim_id(executor, value.id.as_i64(), TableName::Entry).await?;
            executor
                .run(
                    "INSERT INTO entries (id, conversation_id, head, commit_seq, record) VALUES (?, ?, ?, ?, ?)",
                    &[
                        SqliteValue::from(value.id),
                        SqliteValue::from(value.conversation_id),
                        SqliteValue::from(value.head),
                        SqliteValue::from(seq),
                        SqliteValue::Text(encode_json(value)?),
                    ],
                )
                .await
        }
        StorageWrite::Task { value } => {
            claim_id(executor, value.id.as_i64(), TableName::Task).await?;
            executor
                .run(
                    "INSERT INTO tasks (id, conversation_id, kind, status, abort_requested, background, record)
                     VALUES (?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(id) DO UPDATE SET conversation_id = excluded.conversation_id, kind = excluded.kind,
                     status = excluded.status, abort_requested = excluded.abort_requested,
                     background = excluded.background, record = excluded.record",
                    &[
                        SqliteValue::from(value.id),
                        SqliteValue::from(value.conversation_id),
                        SqliteValue::Text(encode_indexed_string(&value.kind)?),
                        SqliteValue::Text(task_status_str(value.state.status()).to_string()),
                        SqliteValue::from(value.abort_requested),
                        SqliteValue::from(value.background),
                        SqliteValue::Text(encode_json(value)?),
                    ],
                )
                .await
        }
        StorageWrite::Submission { value } => {
            claim_id(executor, value.id.as_i64(), TableName::Submission).await?;
            let encoded_request_id = match value.request_id.as_deref() {
                Some(req) => SqliteValue::Text(encode_indexed_string(req)?),
                None => SqliteValue::Null,
            };
            executor
                .run(
                    "INSERT INTO submissions (id, conversation_id, request_id, status, record) VALUES (?, ?, ?, ?, ?)
                     ON CONFLICT(id) DO UPDATE SET conversation_id = excluded.conversation_id,
                     request_id = excluded.request_id, status = excluded.status, record = excluded.record",
                    &[
                        SqliteValue::from(value.id),
                        SqliteValue::from(value.conversation_id),
                        encoded_request_id,
                        SqliteValue::Text(submission_status_str(value.status()).to_string()),
                        SqliteValue::Text(encode_json(value)?),
                    ],
                )
                .await
        }
        StorageWrite::DocumentCreate { .. }
        | StorageWrite::DocumentCopy { .. }
        | StorageWrite::DocumentChange { .. }
        | StorageWrite::DocumentRetire { .. } => Ok(()),
    }
}

async fn apply_document_actions(
    executor: &dyn SqliteExecutor,
    actions: &IndexMap<DocumentId, DocumentAction>,
    seq: Seq,
) -> pi_js::Result<()> {
    for (&id, action) in actions {
        let mut content = action.content.clone();
        if let Some(copy) = action.copy {
            let copied = resolve_copy_for_sqlite(executor, id, copy, action.create.as_ref()).await?;
            content = Some(copied);
        }

        let mut record: DocumentRecord;
        if let Some(ref create) = action.create {
            record = create.clone().into_record(seq);
            if action.retire {
                record.retired_at = Some(seq);
            }
            let parts = address_parts(&record.kind, &record.scope, record.key.as_deref())?;
            claim_id(executor, id.as_i64(), TableName::Document).await?;
            executor
                .run(
                    "INSERT INTO documents
                     (id, kind, family, key_value, scope_kind, owner_id, created_at, retired_at, record)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    &[
                        SqliteValue::from(id),
                        SqliteValue::Text(parts.kind),
                        SqliteValue::Integer(parts.family),
                        SqliteValue::Text(parts.key_value),
                        SqliteValue::Text(parts.scope_kind.to_string()),
                        SqliteValue::Integer(parts.owner_id),
                        SqliteValue::from(seq),
                        SqliteValue::from(record.retired_at),
                        SqliteValue::Text(encode_json(&record)?),
                    ],
                )
                .await?;
        } else {
            let row = executor
                .get("SELECT record FROM documents WHERE id = ?", &[SqliteValue::from(id)])
                .await?
                .ok_or_else(|| pi_js::Error::msg(format!("Unknown document: {id}")))?;
            record = parse_json(row.get_str("record")?)?;
        }

        if let Some(c) = content {
            if matches!(c, DocumentContent::Base { .. }) && is_current_only(&record) {
                executor
                    .run(
                        "DELETE FROM document_revisions WHERE document_id = ?",
                        &[SqliteValue::from(id)],
                    )
                    .await?;
            }
            let (kind_str, version, encoded_content) = match &c {
                DocumentContent::Base { version, value } => ("base", *version, encode_json(value)?),
                DocumentContent::Delta { version, ops } => ("delta", *version, encode_json(ops)?),
            };
            executor
                .run(
                    "INSERT INTO document_revisions (document_id, seq, kind, version, content) VALUES (?, ?, ?, ?, ?)",
                    &[
                        SqliteValue::from(id),
                        SqliteValue::from(seq),
                        SqliteValue::Text(kind_str.to_string()),
                        SqliteValue::Integer(version),
                        SqliteValue::Text(encoded_content),
                    ],
                )
                .await?;
        }

        if action.retire {
            if action.create.is_none() {
                record.retired_at = Some(seq);
                executor
                    .run(
                        "UPDATE documents SET retired_at = ?, record = ? WHERE id = ?",
                        &[
                            SqliteValue::from(seq),
                            SqliteValue::Text(encode_json(&record)?),
                            SqliteValue::from(id),
                        ],
                    )
                    .await?;
            }
            if is_current_only(&record) {
                executor
                    .run(
                        "DELETE FROM document_revisions WHERE document_id = ?",
                        &[SqliteValue::from(id)],
                    )
                    .await?;
            }
        }
    }
    Ok(())
}

async fn resolve_copy_for_sqlite(
    executor: &dyn SqliteExecutor,
    id: DocumentId,
    copy: DocumentCopySource,
    create: Option<&DocumentCreate>,
) -> pi_js::Result<DocumentContent> {
    let inner = async {
        let stored = materialize_document(executor, copy.id, copy.at)
            .await?
            .ok_or_else(|| pi_js::Error::msg(format!("Fork source document {} cannot be read", copy.id)))?;
        let create = create.ok_or_else(|| pi_js::Error::msg("Missing create record for copy"))?;
        if stored.record.scope.kind() != DocumentScopeKind::Conversation
            || create.scope.kind() != DocumentScopeKind::Conversation
            || stored.record.kind != create.kind
            || stored.record.key != create.key
            || stored.record.history != create.history
            || stored.record.fork != create.fork
        {
            return Err(pi_js::Error::msg(format!(
                "Fork source document {} does not match the copied record",
                copy.id
            )));
        }
        Ok(DocumentContent::Base {
            version: stored.version,
            value: stored.value,
        })
    }
    .await;

    inner.map_err(|err| {
        if err.name() == "StorageRejected" {
            err
        } else {
            StorageRejected::with_cause(format!("Document copy {id} was rejected"), err).into_error()
        }
    })
}
