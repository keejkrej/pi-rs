//! Port of packages/durable/src/storage/memory.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use pi_chord::Context;
use pi_chord::delta::{Op, apply_immutable_batches};
use pi_chord::json::copy_json;
use serde_json::Value as JsonValue;

use crate::errors::StorageRejected;
use crate::ids::Seq;
use crate::types::{
    ConversationId, ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentContent, DocumentCreate,
    DocumentId, DocumentPoint, DocumentQuery, DocumentRecord, DocumentScope, DocumentScopeKind, EntryId, EntryLookup,
    EntryQuery, EntryRecord, ErasedTaskRecord, JsonObject, Page, Storage, StorageWrite, StoredDocument, SubmissionId,
    SubmissionQuery, SubmissionRecord, SubmissionStatus, TaskId, TaskQuery, TaskStatus,
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
}

#[derive(Clone, Debug)]
struct DocumentRevision {
    seq: Seq,
    content: DocumentContent,
}

#[derive(Clone, Debug)]
struct StoredDocumentState {
    record: DocumentRecord,
    revisions: Vec<DocumentRevision>,
}

#[derive(Clone, Debug)]
struct DocumentAction {
    create: Option<DocumentCreate>,
    content: Option<DocumentContent>,
    retire: bool,
}

#[derive(Clone, Debug, Default)]
struct DocumentAddressIndex {
    ids: Vec<DocumentId>,
    current_id: Option<DocumentId>,
}

#[derive(Debug, Default)]
struct State {
    record_types: IndexMap<i64, TableName>,
    conversations: IndexMap<ConversationId, ConversationRecord>,
    conversation_ids: Vec<ConversationId>,
    conversation_ids_by_owner_conversation: IndexMap<ConversationId, Vec<ConversationId>>,
    conversation_ids_by_owner_task: IndexMap<TaskId, Vec<ConversationId>>,
    entries: IndexMap<EntryId, EntryRecord>,
    entry_ids: IndexMap<ConversationId, Vec<EntryId>>,
    head_entry_ids: IndexMap<ConversationId, Vec<EntryId>>,
    entry_commit_seqs: IndexMap<EntryId, Seq>,
    tasks: IndexMap<TaskId, ErasedTaskRecord>,
    task_ids: Vec<TaskId>,
    task_ids_by_status: [Vec<TaskId>; 5],
    submissions: IndexMap<SubmissionId, SubmissionRecord>,
    submission_ids: Vec<SubmissionId>,
    submission_ids_by_status: [Vec<SubmissionStatusMarker>; 4],
    submission_ids_by_request: IndexMap<ConversationId, IndexMap<String, SubmissionId>>,
    documents: IndexMap<DocumentId, StoredDocumentState>,
    document_addresses: IndexMap<String, DocumentAddressIndex>,
    document_ids_by_scope: IndexMap<String, Vec<DocumentId>>,
}

type SubmissionStatusMarker = SubmissionId;

#[derive(Debug)]
struct MemoryStorageInner {
    state: State,
    next_id: i64,
    next_seq: i64,
    closed: bool,
}

impl Default for MemoryStorageInner {
    fn default() -> Self {
        Self {
            state: State::default(),
            next_id: 2,
            next_seq: 1,
            closed: false,
        }
    }
}

/// A fully validated, detached state mutation whose application performs no fallible preparation.
pub struct PreparedMemoryCommit {
    pub seq: Seq,
    /// Detached writes with resolved document copies for persistence.
    pub writes: Vec<StorageWrite>,
    actions: IndexMap<DocumentId, DocumentAction>,
    inner: Arc<Mutex<MemoryStorageInner>>,
    applied: bool,
}

impl PreparedMemoryCommit {
    pub fn apply(&mut self) -> pi_js::Result<Seq> {
        if self.applied {
            return Ok(self.seq);
        }
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        if self.seq.as_i64() < guard.next_seq {
            return Err(pi_js::Error::msg(format!(
                "Commit sequence {} does not strictly increase",
                self.seq
            )));
        }
        self.applied = true;
        apply_prepared_commit(&mut guard, &self.writes, &self.actions, self.seq)?;
        Ok(self.seq)
    }
}

/// Detached in-memory reference implementation of [`Storage`].
#[derive(Clone, Debug)]
pub struct MemoryStorage {
    inner: Arc<Mutex<MemoryStorageInner>>,
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MemoryStorageInner::default())),
        }
    }

    /// Validate and detach one commit without changing observable state.
    pub fn prepare_commit(&self, writes: &[StorageWrite], seq: Option<Seq>) -> pi_js::Result<PreparedMemoryCommit> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        let target_seq = seq.unwrap_or_else(|| Seq::new_unchecked(guard.next_seq));
        if target_seq.as_i64() < guard.next_seq {
            return Err(pi_js::Error::msg(format!(
                "Commit sequence {} does not strictly increase",
                target_seq
            )));
        }

        let mut cloned = Vec::with_capacity(writes.len());
        for write in writes {
            cloned.push(clone_write(write)?);
        }
        let detached_writes = resolve_document_copies(&guard.state, cloned)?;
        check_global_ids(&guard.state, &detached_writes)?;
        let actions = prepare_document_actions(&detached_writes)?;
        check_document_actions(&guard.state, &actions)?;

        Ok(PreparedMemoryCommit {
            seq: target_seq,
            writes: detached_writes,
            actions,
            inner: Arc::clone(&self.inner),
            applied: false,
        })
    }
}

impl MemoryStorageInner {
    fn assert_open(&self) -> pi_js::Result<()> {
        if self.closed {
            return Err(pi_js::Error::msg("MemoryStorage is closed"));
        }
        Ok(())
    }
}

#[async_trait]
impl Storage for MemoryStorage {
    async fn commit(&self, writes: Vec<StorageWrite>, _context: Context) -> pi_js::Result<Seq> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        let target_seq = Seq::new_unchecked(guard.next_seq);
        let mut cloned = Vec::with_capacity(writes.len());
        for write in &writes {
            cloned.push(clone_write(write)?);
        }
        let detached_writes = resolve_document_copies(&guard.state, cloned)?;
        check_global_ids(&guard.state, &detached_writes)?;
        let actions = prepare_document_actions(&detached_writes)?;
        check_document_actions(&guard.state, &actions)?;
        apply_prepared_commit(&mut guard, &detached_writes, &actions, target_seq)?;
        Ok(target_seq)
    }

    async fn mint_id(&self) -> pi_js::Result<i64> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        if guard.next_id > pi_js::num::MAX_SAFE_INTEGER {
            return Err(pi_js::Error::msg("ID space is exhausted"));
        }
        let id = guard.next_id;
        guard.next_id += 1;
        Ok(id)
    }

    async fn conversation(&self, id: ConversationId, _context: Context) -> pi_js::Result<Option<ConversationRecord>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        Ok(guard.state.conversations.get(&id).cloned())
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: Context,
    ) -> pi_js::Result<Page<ConversationRecord, Cursor>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        let ids: &[ConversationId] = if let Some(owner_task_id) = query.owner_task_id {
            guard
                .state
                .conversation_ids_by_owner_task
                .get(&owner_task_id)
                .map(Vec::as_slice)
                .unwrap_or(&[])
        } else if let Some(owner_conversation_id) = query.owner_conversation_id {
            guard
                .state
                .conversation_ids_by_owner_conversation
                .get(&owner_conversation_id)
                .map(Vec::as_slice)
                .unwrap_or(&[])
        } else {
            &guard.state.conversation_ids
        };

        let after = cursor_id(cursor.as_ref())?;
        let start = match after {
            Some(a) => upper_bound_ids(ids, a),
            None => 0,
        };
        let mut values = Vec::new();
        for &id in &ids[start..] {
            if values.len() > limit {
                break;
            }
            if let Some(conv) = guard.state.conversations.get(&id) {
                if let Some(owner_conversation_id) = query.owner_conversation_id
                    && conv.owner.map(|o| o.conversation_id) != Some(owner_conversation_id)
                {
                    continue;
                }
                if let Some(owner_task_id) = query.owner_task_id
                    && conv.owner.map(|o| o.task_id) != Some(owner_task_id)
                {
                    continue;
                }
                values.push(conv.clone());
            }
        }
        Ok(build_page(values, limit, |c| c.id.as_i64()))
    }

    async fn entry(&self, id: EntryId, _context: Context) -> pi_js::Result<Option<EntryLookup>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        let Some(entry) = guard.state.entries.get(&id) else {
            return Ok(None);
        };
        let Some(&commit_seq) = guard.state.entry_commit_seqs.get(&id) else {
            return Err(pi_js::Error::msg(format!("Entry {id} is missing its commit sequence")));
        };
        Ok(Some(EntryLookup {
            entry: entry.clone(),
            commit_seq,
        }))
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        _context: Context,
    ) -> pi_js::Result<Option<EntryLookup>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        let Some(mut conversation) = guard.state.conversations.get(&conversation_id).cloned() else {
            return Err(pi_js::Error::msg(format!("Unknown conversation: {conversation_id}")));
        };
        let Some(entry) = guard.state.entries.get(&id) else {
            return Ok(None);
        };
        let mut upper_entry_id = i64::MAX;
        while conversation.id != entry.conversation_id {
            let Some(parent) = conversation.parent else {
                return Ok(None);
            };
            upper_entry_id = upper_entry_id.min(parent.at.as_i64());
            let Some(parent_conv) = guard.state.conversations.get(&parent.conversation_id) else {
                return Err(pi_js::Error::msg(format!(
                    "Unknown ancestor conversation: {}",
                    parent.conversation_id
                )));
            };
            conversation = parent_conv.clone();
        }
        if entry.id.as_i64() > upper_entry_id {
            return Ok(None);
        }
        let Some(&commit_seq) = guard.state.entry_commit_seqs.get(&id) else {
            return Err(pi_js::Error::msg(format!("Entry {id} is missing its commit sequence")));
        };
        Ok(Some(EntryLookup {
            entry: entry.clone(),
            commit_seq,
        }))
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        _context: Context,
    ) -> pi_js::Result<Option<EntryRecord>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        let Some(mut conversation) = guard.state.conversations.get(&conversation_id).cloned() else {
            return Err(pi_js::Error::msg(format!("Unknown conversation: {conversation_id}")));
        };
        let mut upper = at_or_before_entry_id.map(|id| id.as_i64());
        loop {
            if let Some(head_ids) = guard.state.head_entry_ids.get(&conversation.id) {
                let end = match upper {
                    Some(u) => upper_bound_ids(head_ids, u),
                    None => head_ids.len(),
                };
                if end > 0 {
                    let entry_id = head_ids[end - 1];
                    return Ok(guard.state.entries.get(&entry_id).cloned());
                }
            }
            let Some(parent) = conversation.parent else {
                return Ok(None);
            };
            let parent_at = parent.at.as_i64();
            upper = Some(match upper {
                Some(u) => u.min(parent_at),
                None => parent_at,
            });
            let Some(parent_conv) = guard.state.conversations.get(&parent.conversation_id) else {
                return Err(pi_js::Error::msg(format!(
                    "Unknown ancestor conversation: {}",
                    parent.conversation_id
                )));
            };
            conversation = parent_conv.clone();
        }
    }

    async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: Context,
    ) -> pi_js::Result<Page<EntryRecord, Cursor>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        let after = cursor_id(cursor.as_ref())?;
        let min_entry_id = query.min_entry_id.map(|id| id.as_i64()).unwrap_or(i64::MIN);
        let mut upper_entry_id = query.max_entry_id.map(|id| id.as_i64()).unwrap_or(i64::MAX);
        if let Some(a) = after {
            upper_entry_id = upper_entry_id.min(a.saturating_sub(1));
        }

        let Some(mut conversation) = guard.state.conversations.get(&query.conversation_id).cloned() else {
            return Err(pi_js::Error::msg(format!(
                "Unknown conversation: {}",
                query.conversation_id
            )));
        };

        let mut values = Vec::new();
        loop {
            if upper_entry_id >= min_entry_id
                && let Some(ids) = guard.state.entry_ids.get(&conversation.id)
            {
                let start = upper_bound_ids(ids, upper_entry_id);
                for &id in ids[..start].iter().rev() {
                    if id.as_i64() < min_entry_id {
                        break;
                    }
                    if let Some(entry) = guard.state.entries.get(&id) {
                        values.push(entry.clone());
                        if values.len() > limit {
                            break;
                        }
                    }
                }
            }
            if values.len() > limit {
                break;
            }
            let Some(parent) = conversation.parent else {
                break;
            };
            upper_entry_id = upper_entry_id.min(parent.at.as_i64());
            if upper_entry_id < min_entry_id {
                break;
            }
            let Some(parent_conv) = guard.state.conversations.get(&parent.conversation_id) else {
                return Err(pi_js::Error::msg(format!(
                    "Unknown ancestor conversation: {}",
                    parent.conversation_id
                )));
            };
            conversation = parent_conv.clone();
        }
        Ok(build_page(values, limit, |e| e.id.as_i64()))
    }

    async fn task(&self, id: TaskId, _context: Context) -> pi_js::Result<Option<ErasedTaskRecord>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        Ok(guard.state.tasks.get(&id).cloned())
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: Context,
    ) -> pi_js::Result<Page<ErasedTaskRecord, Cursor>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        let ids: &[TaskId] = match query.status {
            Some(status) => &guard.state.task_ids_by_status[task_status_index(status)],
            None => &guard.state.task_ids,
        };
        let after = cursor_id(cursor.as_ref())?;
        let start = match after {
            Some(a) => upper_bound_ids(ids, a),
            None => 0,
        };
        let mut values = Vec::new();
        for &id in &ids[start..] {
            if values.len() > limit {
                break;
            }
            if let Some(task) = guard.state.tasks.get(&id) {
                if let Some(conv_id) = query.conversation_id
                    && task.conversation_id != conv_id
                {
                    continue;
                }
                if let Some(ref kind) = query.kind
                    && task.kind != *kind
                {
                    continue;
                }
                if let Some(status) = query.status
                    && task.state.status() != status
                {
                    continue;
                }
                if let Some(abort_requested) = query.abort_requested
                    && task.abort_requested != abort_requested
                {
                    continue;
                }
                if let Some(background) = query.background
                    && task.background != background
                {
                    continue;
                }
                values.push(task.clone());
            }
        }
        Ok(build_page(values, limit, |t| t.id.as_i64()))
    }

    async fn submission(&self, id: SubmissionId, _context: Context) -> pi_js::Result<Option<SubmissionRecord>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        Ok(guard.state.submissions.get(&id).cloned())
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: Context,
    ) -> pi_js::Result<Page<SubmissionRecord, Cursor>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        let ids: &[SubmissionId] = match query.status {
            Some(status) => &guard.state.submission_ids_by_status[submission_status_index(status)],
            None => &guard.state.submission_ids,
        };
        let after = cursor_id(cursor.as_ref())?;
        let start = match after {
            Some(a) => upper_bound_ids(ids, a),
            None => 0,
        };
        let mut values = Vec::new();
        for &id in &ids[start..] {
            if values.len() > limit {
                break;
            }
            if let Some(sub) = guard.state.submissions.get(&id) {
                if let Some(conv_id) = query.conversation_id
                    && sub.conversation_id != conv_id
                {
                    continue;
                }
                if let Some(status) = query.status
                    && sub.status() != status
                {
                    continue;
                }
                values.push(sub.clone());
            }
        }
        Ok(build_page(values, limit, |s| s.id.as_i64()))
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        _context: Context,
    ) -> pi_js::Result<Option<SubmissionRecord>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        let Some(requests) = guard.state.submission_ids_by_request.get(&conversation_id) else {
            return Ok(None);
        };
        let Some(id) = requests.get(request_id) else {
            return Ok(None);
        };
        Ok(guard.state.submissions.get(id).cloned())
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        _context: Context,
    ) -> pi_js::Result<Option<DocumentRecord>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        let key = address_key(&address)?;
        let Some(index) = guard.state.document_addresses.get(&key) else {
            return Ok(None);
        };
        if at == DocumentPoint::Current {
            return Ok(index
                .current_id
                .and_then(|id| guard.state.documents.get(&id))
                .map(|doc| doc.record.clone()));
        }
        for &id in &index.ids {
            if let Some(doc) = guard.state.documents.get(&id)
                && is_alive_at(&doc.record, at)
            {
                return Ok(Some(doc.record.clone()));
            }
        }
        Ok(None)
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        _context: Context,
    ) -> pi_js::Result<Option<StoredDocument>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        materialize_document(&guard.state, id, at)
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        _context: Context,
    ) -> pi_js::Result<Page<DocumentRecord, Cursor>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.assert_open()?;
        let sk = scope_key(&query.scope)?;
        let ids: &[DocumentId] = guard
            .state
            .document_ids_by_scope
            .get(&sk)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let after = cursor_id(cursor.as_ref())?;
        let start = match after {
            Some(a) => upper_bound_ids(ids, a),
            None => 0,
        };
        let mut values = Vec::new();
        for &id in &ids[start..] {
            if values.len() > limit {
                break;
            }
            if let Some(doc) = guard.state.documents.get(&id) {
                if let Some(ref kind) = query.kind
                    && doc.record.kind != *kind
                {
                    continue;
                }
                if is_alive_at(&doc.record, query.at) {
                    values.push(doc.record.clone());
                }
            }
        }
        Ok(build_page(values, limit, |d| d.id.as_i64()))
    }

    async fn close(&self, _context: Context) -> pi_js::Result<()> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.closed = true;
        Ok(())
    }
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

fn lower_bound_ids<K: crate::ids::IdKind, T>(ids: &[crate::ids::Id<K, T>], target: i64) -> usize {
    let mut low = 0usize;
    let mut high = ids.len();
    while low < high {
        let middle = low + (high - low) / 2;
        if ids[middle].as_i64() < target {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

fn upper_bound_ids<K: crate::ids::IdKind, T>(ids: &[crate::ids::Id<K, T>], target: i64) -> usize {
    let mut low = 0usize;
    let mut high = ids.len();
    while low < high {
        let middle = low + (high - low) / 2;
        if ids[middle].as_i64() <= target {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

fn insert_sorted<K: crate::ids::IdKind, T>(ids: &mut Vec<crate::ids::Id<K, T>>, id: crate::ids::Id<K, T>) {
    let raw = id.as_i64();
    if ids.last().is_none_or(|last| last.as_i64() < raw) {
        ids.push(id);
    } else {
        let pos = lower_bound_ids(ids, raw);
        if ids.get(pos).is_none_or(|existing| existing.as_i64() != raw) {
            ids.insert(pos, id);
        }
    }
}

fn remove_sorted<K: crate::ids::IdKind, T>(ids: &mut Vec<crate::ids::Id<K, T>>, id: crate::ids::Id<K, T>) {
    let pos = lower_bound_ids(ids, id.as_i64());
    if ids.get(pos).is_some_and(|existing| existing.as_i64() == id.as_i64()) {
        ids.remove(pos);
    }
}

fn insert_map_id<MapKey, K, T>(
    index: &mut IndexMap<MapKey, Vec<crate::ids::Id<K, T>>>,
    key: MapKey,
    id: crate::ids::Id<K, T>,
) where
    MapKey: Eq + std::hash::Hash,
    K: crate::ids::IdKind,
{
    let list = index.entry(key).or_default();
    insert_sorted(list, id);
}

fn scope_key(scope: &DocumentScope) -> pi_js::Result<String> {
    match scope {
        DocumentScope::Session => pi_js::json::try_stringify(&["session"]),
        DocumentScope::Conversation { conversation_id } => {
            pi_js::json::try_stringify(&("conversation", conversation_id.as_i64()))
        }
        DocumentScope::Task { task_id } => pi_js::json::try_stringify(&("task", task_id.as_i64())),
    }
}

fn address_key(address: &DocumentAddress) -> pi_js::Result<String> {
    let sk = scope_key(&address.scope)?;
    match &address.key {
        None => pi_js::json::try_stringify(&(&address.kind, &sk, ("singleton",))),
        Some(k) => pi_js::json::try_stringify(&(&address.kind, &sk, ("family", k))),
    }
}

fn record_address_key(kind: &str, scope: &DocumentScope, key: Option<&str>) -> pi_js::Result<String> {
    let sk = scope_key(scope)?;
    match key {
        None => pi_js::json::try_stringify(&(kind, &sk, ("singleton",))),
        Some(k) => pi_js::json::try_stringify(&(kind, &sk, ("family", k))),
    }
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
    record.scope.kind() != DocumentScopeKind::Conversation
        || record.history == Some(crate::types::DocumentHistory::Latest)
}

fn task_status_index(status: TaskStatus) -> usize {
    match status {
        TaskStatus::Pending => 0,
        TaskStatus::Running => 1,
        TaskStatus::Waiting => 2,
        TaskStatus::Completing => 3,
        TaskStatus::Terminal => 4,
    }
}

fn submission_status_index(status: SubmissionStatus) -> usize {
    match status {
        SubmissionStatus::Queued => 0,
        SubmissionStatus::Placed => 1,
        SubmissionStatus::Done => 2,
        SubmissionStatus::Unanswered => 3,
    }
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

fn clone_json_object(map: &JsonObject) -> pi_js::Result<JsonObject> {
    let copied = copy_json(&JsonValue::Object(map.clone()), None)?;
    match copied {
        JsonValue::Object(m) => Ok(m),
        _ => Err(pi_js::Error::msg("Expected JSON object")),
    }
}

fn clone_write(write: &StorageWrite) -> pi_js::Result<StorageWrite> {
    match write {
        StorageWrite::Conversation { value } => Ok(StorageWrite::Conversation { value: value.clone() }),
        StorageWrite::Entry { value } => {
            let mut next = value.clone();
            if let Some(ref data) = next.data {
                next.data = Some(copy_json(data, None)?);
            }
            Ok(StorageWrite::Entry { value: next })
        }
        StorageWrite::Task { value } => {
            let mut next = value.clone();
            next.input = copy_json(&next.input, None)?;
            match &mut next.state {
                crate::types::TaskState::Pending { checkpoint }
                | crate::types::TaskState::Running { checkpoint }
                | crate::types::TaskState::Waiting { checkpoint, .. } => {
                    *checkpoint = copy_json(checkpoint, None)?;
                }
                crate::types::TaskState::Completing { outcome } | crate::types::TaskState::Terminal { outcome } => {
                    clone_task_outcome(outcome)?;
                }
            }
            if let Some(ref mut memos) = next.memos {
                for memo in memos.values_mut() {
                    *memo = copy_json(memo, None)?;
                }
            }
            Ok(StorageWrite::Task { value: next })
        }
        StorageWrite::Submission { value } => {
            let mut next = value.clone();
            match &mut next.state {
                crate::types::SubmissionState::Input(crate::types::InputSubmissionState::Unanswered {
                    detail: Some(detail),
                    ..
                })
                | crate::types::SubmissionState::Write(crate::types::WriteSubmissionState::Unanswered {
                    detail: Some(detail),
                    ..
                }) => {
                    *detail = copy_json(detail, None)?;
                }
                _ => {}
            }
            Ok(StorageWrite::Submission { value: next })
        }
        StorageWrite::DocumentCreate { record, version, value } => Ok(StorageWrite::DocumentCreate {
            record: record.clone(),
            version: *version,
            value: clone_json_object(value)?,
        }),
        StorageWrite::DocumentCopy { record, source } => Ok(StorageWrite::DocumentCopy {
            record: record.clone(),
            source: *source,
        }),
        StorageWrite::DocumentChange { id, content } => {
            let cloned_content = match content {
                DocumentContent::Base { version, value } => DocumentContent::Base {
                    version: *version,
                    value: clone_json_object(value)?,
                },
                DocumentContent::Delta { version, ops } => DocumentContent::Delta {
                    version: *version,
                    ops: clone_ops(ops)?,
                },
            };
            Ok(StorageWrite::DocumentChange {
                id: *id,
                content: cloned_content,
            })
        }
        StorageWrite::DocumentRetire { id } => Ok(StorageWrite::DocumentRetire { id: *id }),
    }
}

fn clone_task_outcome(outcome: &mut crate::types::TaskOutcome<JsonValue>) -> pi_js::Result<()> {
    match outcome {
        crate::types::TaskOutcome::Completed { result } => {
            *result = copy_json(result, None)?;
        }
        crate::types::TaskOutcome::Failed { error, result } => {
            if let Some(ref detail) = error.detail {
                error.detail = Some(copy_json(detail, None)?);
            }
            if let Some(res) = result {
                *res = copy_json(res, None)?;
            }
        }
        crate::types::TaskOutcome::Aborted { result, .. } => {
            if let Some(res) = result {
                *res = copy_json(res, None)?;
            }
        }
        crate::types::TaskOutcome::Orphaned { .. } => {}
        crate::types::TaskOutcome::Faulted { error } => {
            if let Some(ref detail) = error.detail {
                error.detail = Some(copy_json(detail, None)?);
            }
        }
    }
    Ok(())
}

fn clone_ops(ops: &[Op]) -> pi_js::Result<Vec<Op>> {
    let mut out = Vec::with_capacity(ops.len());
    for op in ops {
        let cloned = match op {
            Op::Replace(r) => Op::Replace(pi_chord::delta::OpReplace(r.0, copy_json(&r.1, None)?)),
            Op::Set(s) => Op::Set(pi_chord::delta::OpSet(s.0, s.1.clone(), copy_json(&s.2, None)?)),
            Op::Delete(d) => Op::Delete(d.clone()),
            Op::Append(a) => Op::Append(a.clone()),
            Op::Truncate(t) => Op::Truncate(t.clone()),
            Op::Splice(p) => {
                let mut items = Vec::with_capacity(p.4.len());
                for item in &p.4 {
                    items.push(copy_json(item, None)?);
                }
                Op::Splice(pi_chord::delta::OpSplice(p.0, p.1.clone(), p.2, p.3, items))
            }
            Op::Move(m) => Op::Move(m.clone()),
        };
        out.push(cloned);
    }
    Ok(out)
}

fn resolve_document_copies(state: &State, writes: Vec<StorageWrite>) -> pi_js::Result<Vec<StorageWrite>> {
    if !writes.iter().any(|w| matches!(w, StorageWrite::DocumentCopy { .. })) {
        return Ok(writes);
    }
    let mut changed_document_ids = IndexSet::new();
    for write in &writes {
        match write {
            StorageWrite::DocumentCreate { record, .. } | StorageWrite::DocumentCopy { record, .. } => {
                changed_document_ids.insert(record.id);
            }
            StorageWrite::DocumentChange { id, .. } | StorageWrite::DocumentRetire { id } => {
                changed_document_ids.insert(*id);
            }
            _ => {}
        }
    }

    let mut out = Vec::with_capacity(writes.len());
    for write in writes {
        match write {
            StorageWrite::DocumentCopy { record, source } => {
                let resolved = resolve_single_copy(state, &changed_document_ids, &record, source).map_err(|err| {
                    if err.name() == "StorageRejected" {
                        err
                    } else {
                        StorageRejected::with_cause(format!("Document copy {} was rejected", record.id), err)
                            .into_error()
                    }
                })?;
                out.push(resolved);
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

fn resolve_single_copy(
    state: &State,
    changed_document_ids: &IndexSet<DocumentId>,
    record: &DocumentCreate,
    source: crate::types::DocumentCopySource,
) -> pi_js::Result<StorageWrite> {
    if changed_document_ids.contains(&source.id) {
        return Err(pi_js::Error::msg(format!(
            "Fork source document {} is changed in the copy batch",
            source.id
        )));
    }
    let stored = materialize_document(state, source.id, source.at)?
        .ok_or_else(|| pi_js::Error::msg(format!("Fork source document {} cannot be read", source.id)))?;
    if stored.record.scope.kind() != DocumentScopeKind::Conversation
        || record.scope.kind() != DocumentScopeKind::Conversation
        || stored.record.kind != record.kind
        || stored.record.key != record.key
        || stored.record.history != record.history
        || stored.record.fork != record.fork
    {
        return Err(pi_js::Error::msg(format!(
            "Fork source document {} does not match the copied record",
            source.id
        )));
    }
    Ok(StorageWrite::DocumentCreate {
        record: record.clone(),
        version: stored.version,
        value: stored.value,
    })
}

fn materialize_document(state: &State, id: DocumentId, at: DocumentPoint) -> pi_js::Result<Option<StoredDocument>> {
    let Some(stored) = state.documents.get(&id) else {
        return Ok(None);
    };
    if at != DocumentPoint::Current && is_current_only(&stored.record) {
        return Err(pi_js::Error::msg(format!(
            "Document {id} does not retain historical content"
        )));
    }
    if !is_alive_at(&stored.record, at) {
        return Ok(None);
    }

    let cutoff = match at {
        DocumentPoint::Current => i64::MAX,
        DocumentPoint::At(seq) => seq.as_i64(),
    };
    let end = stored
        .revisions
        .iter()
        .position(|rev| rev.seq.as_i64() > cutoff)
        .unwrap_or(stored.revisions.len());
    let revisions = &stored.revisions[..end];

    let Some(base_index) = revisions
        .iter()
        .rposition(|rev| matches!(rev.content, DocumentContent::Base { .. }))
    else {
        return Err(pi_js::Error::msg(format!("Document {id} is missing a required base")));
    };
    let DocumentContent::Base {
        version: base_version,
        value: base_value,
    } = &revisions[base_index].content
    else {
        return Err(pi_js::Error::msg(format!("Document {id} is missing a required base")));
    };

    let tail = &revisions[base_index + 1..];
    let mut batches = Vec::with_capacity(tail.len());
    for rev in tail {
        match &rev.content {
            DocumentContent::Delta { version, ops } if *version == *base_version => {
                batches.push(ops.as_slice());
            }
            _ => {
                return Err(pi_js::Error::msg(format!(
                    "Document {id} crosses a stored version boundary without a base"
                )));
            }
        }
    }

    let applied = apply_immutable_batches::<JsonValue, _, _>(Some(JsonValue::Object(base_value.clone())), batches)?;
    let JsonValue::Object(value) = applied else {
        return Err(pi_js::Error::msg(format!(
            "Document {id} did not materialize to a JSON object"
        )));
    };

    Ok(Some(StoredDocument {
        record: stored.record.clone(),
        version: *base_version,
        value,
        deltas_since_base: tail.len(),
    }))
}

fn check_global_ids(state: &State, writes: &[StorageWrite]) -> pi_js::Result<()> {
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

        let existing = state.record_types.get(&id).copied();
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
                    content: None,
                    retire: false,
                });
                if action.create.is_some() || action.content.is_some() {
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
            StorageWrite::DocumentCopy { record, .. } => {
                return Err(pi_js::Error::msg(format!(
                    "Document {} copy was not resolved",
                    record.id
                )));
            }
            StorageWrite::DocumentChange { id, content } => {
                let action = actions.entry(*id).or_insert(DocumentAction {
                    create: None,
                    content: None,
                    retire: false,
                });
                if action.content.is_some() {
                    return Err(pi_js::Error::msg(format!(
                        "Document {id} has more than one content command"
                    )));
                }
                action.content = Some(content.clone());
            }
            StorageWrite::DocumentRetire { id } => {
                let action = actions.entry(*id).or_insert(DocumentAction {
                    create: None,
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

fn check_document_actions(state: &State, actions: &IndexMap<DocumentId, DocumentAction>) -> pi_js::Result<()> {
    let mut live_counts: IndexMap<String, i64> = IndexMap::new();
    for (&id, action) in actions {
        let existing = state.documents.get(&id);
        if action.create.is_none() && existing.is_none() {
            return Err(pi_js::Error::msg(format!("Unknown document: {id}")));
        }
        if action.create.is_some() && existing.is_some() {
            return Err(pi_js::Error::msg(format!("Document {id} already exists")));
        }
        if let Some(ex) = existing
            && ex.record.retired_at.is_some()
        {
            return Err(pi_js::Error::msg(format!("Document {id} is retired")));
        }
        if let Some(DocumentContent::Delta { version, .. }) = &action.content {
            let previous_version = existing
                .and_then(|ex| ex.revisions.last())
                .map(|rev| match &rev.content {
                    DocumentContent::Base { version, .. } | DocumentContent::Delta { version, .. } => *version,
                });
            let Some(prev_ver) = previous_version else {
                return Err(pi_js::Error::msg(format!("Document {id} delta has no base")));
            };
            if prev_ver != *version {
                return Err(pi_js::Error::msg(format!(
                    "Document {id} version transition requires a base"
                )));
            }
        }

        let key = match &action.create {
            Some(create) => record_address_key(&create.kind, &create.scope, create.key.as_deref())?,
            None => {
                let Some(ex) = existing else {
                    return Err(pi_js::Error::msg(format!("Unknown document: {id}")));
                };
                record_address_key(&ex.record.kind, &ex.record.scope, ex.record.key.as_deref())?
            }
        };

        let current_id = state.document_addresses.get(&key).and_then(|addr| addr.current_id);
        let live = live_counts
            .entry(key)
            .or_insert_with(|| if current_id.is_some() { 1 } else { 0 });
        if action.retire && current_id == Some(id) {
            *live -= 1;
        }
        if action.create.is_some() && !action.retire {
            *live += 1;
        }
    }

    for live in live_counts.values() {
        if *live > 1 {
            return Err(pi_js::Error::msg("Document address already has a current incarnation"));
        }
    }
    Ok(())
}

fn apply_prepared_commit(
    inner: &mut MemoryStorageInner,
    writes: &[StorageWrite],
    actions: &IndexMap<DocumentId, DocumentAction>,
    seq: Seq,
) -> pi_js::Result<()> {
    for write in writes {
        match write {
            StorageWrite::Conversation { value } => {
                let id = value.id;
                inner.state.record_types.insert(id.as_i64(), TableName::Conversation);
                inner.state.conversations.insert(id, value.clone());
                insert_sorted(&mut inner.state.conversation_ids, id);
                if let Some(owner) = value.owner {
                    insert_map_id(
                        &mut inner.state.conversation_ids_by_owner_conversation,
                        owner.conversation_id,
                        id,
                    );
                    insert_map_id(&mut inner.state.conversation_ids_by_owner_task, owner.task_id, id);
                }
                inner.next_id = inner.next_id.max(id.as_i64() + 1);
            }
            StorageWrite::Entry { value } => {
                let id = value.id;
                inner.state.record_types.insert(id.as_i64(), TableName::Entry);
                inner.state.entries.insert(id, value.clone());
                inner.state.entry_commit_seqs.insert(id, seq);
                insert_map_id(&mut inner.state.entry_ids, value.conversation_id, id);
                if value.head.is_some() {
                    insert_map_id(&mut inner.state.head_entry_ids, value.conversation_id, id);
                }
                inner.next_id = inner.next_id.max(id.as_i64() + 1);
            }
            StorageWrite::Task { value } => {
                let id = value.id;
                let new_status = value.state.status();
                inner.state.record_types.insert(id.as_i64(), TableName::Task);
                if let Some(previous) = inner.state.tasks.get(&id) {
                    let old_status = previous.state.status();
                    if old_status != new_status {
                        remove_sorted(&mut inner.state.task_ids_by_status[task_status_index(old_status)], id);
                        insert_sorted(&mut inner.state.task_ids_by_status[task_status_index(new_status)], id);
                    }
                } else {
                    insert_sorted(&mut inner.state.task_ids, id);
                    insert_sorted(&mut inner.state.task_ids_by_status[task_status_index(new_status)], id);
                }
                inner.state.tasks.insert(id, value.clone());
                inner.next_id = inner.next_id.max(id.as_i64() + 1);
            }
            StorageWrite::Submission { value } => {
                let id = value.id;
                let new_status = value.status();
                inner.state.record_types.insert(id.as_i64(), TableName::Submission);
                let previous = inner.state.submissions.get(&id).cloned();
                if let Some(ref prev) = previous {
                    let old_status = prev.status();
                    if old_status != new_status {
                        remove_sorted(
                            &mut inner.state.submission_ids_by_status[submission_status_index(old_status)],
                            id,
                        );
                        insert_sorted(
                            &mut inner.state.submission_ids_by_status[submission_status_index(new_status)],
                            id,
                        );
                    }
                    if let Some(ref prev_req_id) = prev.request_id
                        && let Some(prev_map) = inner.state.submission_ids_by_request.get_mut(&prev.conversation_id)
                        && prev_map.get(prev_req_id).copied() == Some(id)
                    {
                        prev_map.shift_remove(prev_req_id);
                        if prev_map.is_empty() {
                            inner
                                .state
                                .submission_ids_by_request
                                .shift_remove(&prev.conversation_id);
                        }
                    }
                } else {
                    insert_sorted(&mut inner.state.submission_ids, id);
                    insert_sorted(
                        &mut inner.state.submission_ids_by_status[submission_status_index(new_status)],
                        id,
                    );
                }
                inner.state.submissions.insert(id, value.clone());
                if let Some(ref req_id) = value.request_id {
                    inner
                        .state
                        .submission_ids_by_request
                        .entry(value.conversation_id)
                        .or_default()
                        .insert(req_id.clone(), id);
                }
                inner.next_id = inner.next_id.max(id.as_i64() + 1);
            }
            StorageWrite::DocumentCopy { .. } => {
                return Err(pi_js::Error::msg("Prepared document copy was not resolved"));
            }
            StorageWrite::DocumentCreate { .. }
            | StorageWrite::DocumentChange { .. }
            | StorageWrite::DocumentRetire { .. } => {}
        }
    }

    apply_document_actions(inner, actions, seq)?;
    inner.next_seq = seq.as_i64() + 1;
    Ok(())
}

fn apply_document_actions(
    inner: &mut MemoryStorageInner,
    actions: &IndexMap<DocumentId, DocumentAction>,
    seq: Seq,
) -> pi_js::Result<()> {
    for (&id, action) in actions {
        let mut created_record_key: Option<String> = None;
        let existing_key: Option<String>;

        if let Some(ref create) = action.create {
            let mut record = create.clone().into_record(seq);
            if action.retire {
                record.retired_at = Some(seq);
            }
            let Some(content) = action.content.clone() else {
                return Err(pi_js::Error::msg(format!(
                    "Document creation {id} is missing initial content"
                )));
            };
            let key = record_address_key(&record.kind, &record.scope, record.key.as_deref())?;
            let sk = scope_key(&record.scope)?;
            created_record_key = Some(key.clone());
            existing_key = Some(key.clone());

            inner.state.record_types.insert(id.as_i64(), TableName::Document);
            inner.state.documents.insert(
                id,
                StoredDocumentState {
                    record,
                    revisions: vec![DocumentRevision { seq, content }],
                },
            );
            let addr = inner.state.document_addresses.entry(key).or_default();
            insert_sorted(&mut addr.ids, id);
            insert_map_id(&mut inner.state.document_ids_by_scope, sk, id);
            inner.next_id = inner.next_id.max(id.as_i64() + 1);
        } else {
            let Some(stored) = inner.state.documents.get_mut(&id) else {
                return Err(pi_js::Error::msg(format!("Unknown document: {id}")));
            };
            if let Some(content) = action.content.clone() {
                let revision = DocumentRevision { seq, content };
                if matches!(revision.content, DocumentContent::Base { .. }) && is_current_only(&stored.record) {
                    stored.revisions = vec![revision];
                } else {
                    stored.revisions.push(revision);
                }
            }
            if action.retire {
                stored.record.retired_at = Some(seq);
            }
            existing_key = Some(record_address_key(
                &stored.record.kind,
                &stored.record.scope,
                stored.record.key.as_deref(),
            )?);
        }

        if action.retire
            && let Some(stored) = inner.state.documents.get_mut(&id)
            && is_current_only(&stored.record)
        {
            stored.revisions.clear();
        }

        if (created_record_key.is_some() || action.retire)
            && let Some(key) = existing_key
            && let Some(addr) = inner.state.document_addresses.get_mut(&key)
        {
            if action.retire && addr.current_id == Some(id) {
                addr.current_id = None;
            }
            if action.create.is_some() && !action.retire {
                addr.current_id = Some(id);
            }
        }
    }
    Ok(())
}
