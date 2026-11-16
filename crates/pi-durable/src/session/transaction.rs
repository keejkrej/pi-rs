//! Port of packages/durable/src/session/transaction.ts

use std::future::Future;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use pi_chord::delta::{Change, DraftOverlay, Op, Prepared, Tracker, track};
use pi_chord::json::{copy_json, is_json_value};
use pi_chord::{Context, JsonValue};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::{Notify, OnceCell};

use crate::documents::{
    ConversationDoc, ConversationDocFamily, ErasedDocDefinition, ResolvedAddress, RewindableConversationDocFamilyToken,
    SessionDocFamilyToken, SessionDocToken, TaskDocFamilyToken, TaskDocToken, address_id, check_create_scope,
    check_record_scope, check_record_version, document_create, json_object_to_value, materialize_create_value,
    resolve_address, value_to_json_object,
};
use crate::entries::{Entry, TypedEntryDraft};
use crate::errors::{ConversationBusy, ReadAfterWrite, StorageRejected};
use crate::ids::mint_typed_id;
use crate::session::forks::prepare_fork_document_copies;
use crate::tasks::{AnyTask, Task};
use crate::types::{
    CheckpointInfo, ConversationId, ConversationOwner, ConversationOwnership, ConversationParent, ConversationQuery,
    ConversationRecord, Cursor, DocumentAddress, DocumentCommitChange, DocumentContent, DocumentCopySource,
    DocumentCreate, DocumentFork, DocumentId, DocumentPoint, DocumentQuery, DocumentRecord, DocumentScope, EntryDraft,
    EntryDraftHead, EntryId, EntryKind, EntryQuery, EntryRecord, ErasedTaskRecord, InputSubmissionState, JsonObject,
    Page, ROOT_CONVERSATION_ID, Seq, Storage, StorageWrite, SubmissionCreate, SubmissionId, SubmissionKind,
    SubmissionRecord, SubmissionSettlement, SubmissionState, TaskId, TaskKind, TaskOptions, TaskOwnership, TaskQuery,
    TaskRecord, TaskState, TypedEntry, WriteSubmissionState,
};

const INTERNAL_SCAN_PAGE_SIZE: usize = 256;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn clone_pi_error(err: &pi_js::Error) -> pi_js::Error {
    match err {
        pi_js::Error::Js(j) => pi_js::Error::Js(pi_js::JsError {
            name: j.name.clone(),
            message: j.message.clone(),
            code: j.code.clone(),
            cause: j.cause.as_deref().map(|c| Box::new(clone_pi_error(c))),
        }),
        pi_js::Error::Abort(reason) => pi_js::Error::Abort(reason.clone()),
        pi_js::Error::Node(node) => pi_js::Error::Node(node.clone()),
        pi_js::Error::Exit(code) => pi_js::Error::Exit(*code),
        pi_js::Error::Typed(name, _) => {
            if let Some(rej) = err.downcast_ref::<StorageRejected>() {
                return rej.clone().into_error();
            }
            if let Some(raw) = err.downcast_ref::<ReadAfterWrite>() {
                return raw.clone().into_error();
            }
            if let Some(busy) = err.downcast_ref::<ConversationBusy>() {
                return busy.clone().into_error();
            }
            pi_js::Error::js(name, err.message())
        }
    }
}

/// Active transactional handle to one acquired or newly created document.
///
/// Supports both structural Rust edits (`get`/`replace`/`update`) and live Chord
/// [`DraftOverlay`] edits via `overlay()`, settling atomically with the commit.
pub struct DocDraft<T> {
    change: Change<JsonObject>,
    staged_next: Arc<Mutex<Option<JsonObject>>>,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for DocDraft<T> {
    fn clone(&self) -> Self {
        Self {
            change: self.change.clone(),
            staged_next: Arc::clone(&self.staged_next),
            _marker: PhantomData,
        }
    }
}

impl<T> DocDraft<T> {
    pub fn overlay(&self) -> pi_js::Result<DraftOverlay> {
        if lock(&self.staged_next).is_some() {
            return Err(pi_js::Error::js(
                "TypeError",
                "Cannot access overlay() after DocDraft::replace() or update()",
            ));
        }
        self.change.overlay()
    }

    pub fn inspect_json(&self) -> pi_js::Result<JsonObject> {
        let ov = self.change.overlay()?;
        if let Some(next) = lock(&self.staged_next).as_ref() {
            return Ok(next.clone());
        }
        match ov.to_json()? {
            JsonValue::Object(map) => Ok(map),
            _ => Err(pi_js::Error::js("TypeError", "Document root must be a JSON object")),
        }
    }
}

impl<T: DeserializeOwned> DocDraft<T> {
    pub fn get(&self) -> pi_js::Result<T> {
        let obj = self.inspect_json()?;
        json_object_to_value(&obj)
    }
}

impl<T: Serialize> DocDraft<T> {
    pub fn replace(&self, next: T) -> pi_js::Result<()> {
        let _ = self.change.overlay()?;
        let obj = value_to_json_object(&next)?;
        *lock(&self.staged_next) = Some(obj);
        Ok(())
    }

    pub fn set(&self, next: T) -> pi_js::Result<()> {
        self.replace(next)
    }
}

impl<T: Serialize + DeserializeOwned> DocDraft<T> {
    pub fn update<R>(&self, f: impl FnOnce(&mut T) -> R) -> pi_js::Result<R> {
        let mut value = self.get()?;
        let out = f(&mut value);
        self.replace(value)?;
        Ok(out)
    }

    pub fn try_update<R>(&self, f: impl FnOnce(&mut T) -> pi_js::Result<R>) -> pi_js::Result<R> {
        let mut value = self.get()?;
        let out = f(&mut value)?;
        self.replace(value)?;
        Ok(out)
    }
}

/// A staged submission change: a settlement, or the placement of a queued submission at its entry.
#[derive(Clone, Debug)]
enum SubmissionChange {
    Settlement(SubmissionSettlement),
    Placed { entry: EntryId },
}

fn apply_submission_change(
    current: &SubmissionRecord,
    change: SubmissionChange,
) -> pi_js::Result<Option<SubmissionRecord>> {
    if current.is_settled() {
        return Ok(None);
    }
    match change {
        SubmissionChange::Placed { entry } => {
            let next_state = match &current.state {
                SubmissionState::Input(InputSubmissionState::Queued) => {
                    SubmissionState::Input(InputSubmissionState::Placed { entry })
                }
                SubmissionState::Write(WriteSubmissionState::Queued) => {
                    SubmissionState::Write(WriteSubmissionState::Done { entry })
                }
                _ => {
                    return Err(pi_js::Error::msg(format!("Submission {} is not queued", current.id)));
                }
            };
            Ok(Some(SubmissionRecord {
                id: current.id,
                conversation_id: current.conversation_id,
                request_id: current.request_id.clone(),
                state: next_state,
            }))
        }
        SubmissionChange::Settlement(SubmissionSettlement::Done { answer }) => {
            let SubmissionState::Input(InputSubmissionState::Placed { entry }) = &current.state else {
                return Err(pi_js::Error::msg(format!(
                    "Submission {} is not a placed input",
                    current.id
                )));
            };
            Ok(Some(SubmissionRecord {
                id: current.id,
                conversation_id: current.conversation_id,
                request_id: current.request_id.clone(),
                state: SubmissionState::Input(InputSubmissionState::Done { entry: *entry, answer }),
            }))
        }
        SubmissionChange::Settlement(SubmissionSettlement::Unanswered { reason, detail }) => {
            let copied_detail = match detail {
                None => None,
                Some(v) => {
                    let c = copy_json(&v, None)?;
                    if !is_json_value(&c) {
                        return Err(pi_js::Error::js("TypeError", "Submission detail must be strict JSON"));
                    }
                    Some(c)
                }
            };
            let next_state = match &current.state {
                SubmissionState::Input(InputSubmissionState::Queued) => {
                    SubmissionState::Input(InputSubmissionState::Unanswered {
                        entry: None,
                        reason,
                        detail: copied_detail,
                    })
                }
                SubmissionState::Input(InputSubmissionState::Placed { entry }) => {
                    SubmissionState::Input(InputSubmissionState::Unanswered {
                        entry: Some(*entry),
                        reason,
                        detail: copied_detail,
                    })
                }
                SubmissionState::Write(WriteSubmissionState::Queued) => {
                    SubmissionState::Write(WriteSubmissionState::Unanswered {
                        reason,
                        detail: copied_detail,
                    })
                }
                _ => return Ok(None),
            };
            Ok(Some(SubmissionRecord {
                id: current.id,
                conversation_id: current.conversation_id,
                request_id: current.request_id.clone(),
                state: next_state,
            }))
        }
    }
}

/// One committed document incarnation owned by the Session tracker cache.
pub struct LoadedDocument {
    pub address_id: String,
    pub record: DocumentRecord,
    /// Persisted definition version; older while the tracked value is migrated only in memory.
    stored_version: Mutex<i64>,
    /// Definition version whose shape the tracked value has; access with another version reloads from Storage.
    pub value_version: i64,
    /// Stored deltas after the newest base; advanced by adoption so the next predicate call needs no read.
    deltas_since_base: Mutex<usize>,
    pub tracker: Tracker<JsonObject>,
}

impl LoadedDocument {
    pub fn new(
        address_id: String,
        record: DocumentRecord,
        stored_version: i64,
        value_version: i64,
        deltas_since_base: usize,
        tracker: Tracker<JsonObject>,
    ) -> Self {
        Self {
            address_id,
            record,
            stored_version: Mutex::new(stored_version),
            value_version,
            deltas_since_base: Mutex::new(deltas_since_base),
            tracker,
        }
    }

    pub fn stored_version(&self) -> i64 {
        *lock(&self.stored_version)
    }

    pub fn set_stored_version(&self, version: i64) {
        *lock(&self.stored_version) = version;
    }

    pub fn deltas_since_base(&self) -> usize {
        *lock(&self.deltas_since_base)
    }

    pub fn reset_deltas_since_base(&self) {
        *lock(&self.deltas_since_base) = 0;
    }

    pub fn increment_deltas_since_base(&self) {
        *lock(&self.deltas_since_base) += 1;
    }
}

/// Session services used by a transaction while it holds the mutation line.
#[async_trait]
pub trait TransactionHost: Send + Sync {
    fn storage(&self) -> &Arc<dyn Storage>;
    fn cached(&self, address_id: &str) -> Option<Arc<LoadedDocument>>;
    async fn load(
        &self,
        definition: &ErasedDocDefinition,
        address_id: &str,
        address: &DocumentAddress,
        context: &Context,
    ) -> pi_js::Result<Option<Arc<LoadedDocument>>>;
    fn install(&self, document: Arc<LoadedDocument>);
    fn evict(&self, address_id: &str, record_id: DocumentId);
    async fn conversation_created(&self, tx: &Arc<Transaction>, record: &ConversationRecord) -> pi_js::Result<()>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskWriteKind {
    Create,
    Replace,
}

#[derive(Clone)]
struct TaskWrite {
    kind: TaskWriteKind,
    record: ErasedTaskRecord,
}

#[derive(Default)]
struct TransactionTask {
    committed_read: Arc<OnceCell<Result<Option<ErasedTaskRecord>, Arc<pi_js::Error>>>>,
    write: Option<TaskWrite>,
    publication_conversation_id: Option<ConversationId>,
}

/// Defaults a commit binds to: `tx.create_task()` conversation and the task attributed to appended entries.
#[derive(Clone, Copy, Debug, Default)]
pub struct TransactionScope {
    pub conversation_id: Option<ConversationId>,
    /// Task whose runtime commit this is; stamped as `by_task_id` on appended entries.
    pub task_id: Option<TaskId>,
}

#[derive(Clone)]
enum DocumentTarget {
    Loaded {
        document: Arc<LoadedDocument>,
    },
    Created {
        record: DocumentCreate,
        version: i64,
        tracker: Tracker<JsonObject>,
    },
    ForkCopy {
        record: DocumentCreate,
        source: DocumentCopySource,
    },
    RetireOnly {
        record: DocumentRecord,
    },
}

struct DocumentEntryState {
    definition: Option<Arc<ErasedDocDefinition>>,
    target: Option<DocumentTarget>,
    change: Option<Change<JsonObject>>,
    prepared: Option<Prepared<JsonObject>>,
}

struct DocumentEntry {
    address_id: String,
    address: DocumentAddress,
    retire_on_commit: Mutex<bool>,
    has_draft_acquisition: AtomicBool,
    staged_next: Arc<Mutex<Option<JsonObject>>>,
    init: OnceCell<Result<(), Arc<pi_js::Error>>>,
    inner: Mutex<DocumentEntryState>,
}

#[derive(Clone)]
enum PlanRecord {
    Committed(DocumentRecord),
    Create(DocumentCreate),
}

impl PlanRecord {
    fn id(&self) -> DocumentId {
        match self {
            Self::Committed(r) => r.id,
            Self::Create(c) => c.id,
        }
    }

    fn scope(&self) -> DocumentScope {
        match self {
            Self::Committed(r) => r.scope,
            Self::Create(c) => c.scope,
        }
    }

    fn fork(&self) -> Option<DocumentFork> {
        match self {
            Self::Committed(r) => r.fork,
            Self::Create(c) => c.fork,
        }
    }

    fn is_committed(&self) -> bool {
        matches!(self, Self::Committed(_))
    }
}

struct DocumentPlanChange {
    tracker: Tracker<JsonObject>,
    prepared: Prepared<JsonObject>,
    version: i64,
    loaded: Option<Arc<LoadedDocument>>,
    definition: Option<Arc<ErasedDocDefinition>>,
}

struct DocumentPlan {
    address_id: String,
    record: PlanRecord,
    retire: bool,
    content: Option<StorageWrite>,
    change: Option<DocumentPlanChange>,
    conversation_id: Option<ConversationId>,
}

struct PendingOpGuard<'a> {
    count: &'a AtomicUsize,
    notify: &'a Notify,
}

impl Drop for PendingOpGuard<'_> {
    fn drop(&mut self) {
        if self.count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.notify.notify_waiters();
        }
    }
}

struct TransactionState {
    has_table_write: bool,
    writes: Vec<StorageWrite>,
    created_conversation_ids: IndexSet<ConversationId>,
    fork_source_conversation_ids: IndexSet<ConversationId>,
    fork_source_document_ids: IndexSet<DocumentId>,
    tasks_by_id: IndexMap<TaskId, TransactionTask>,
    submissions: IndexMap<SubmissionId, SubmissionRecord>,
    submission_changes: Vec<(SubmissionId, SubmissionChange)>,
    plans: Vec<DocumentPlan>,
    documents: Vec<Arc<DocumentEntry>>,
    latest_document_by_address: IndexMap<String, Arc<DocumentEntry>>,
}

/// Public transaction handle passed to Session commit callbacks.
#[derive(Clone)]
pub struct Tx {
    inner: Arc<Transaction>,
}

impl Tx {
    pub fn from_transaction(inner: Arc<Transaction>) -> Self {
        Self { inner }
    }

    pub fn as_transaction(&self) -> &Arc<Transaction> {
        &self.inner
    }

    pub async fn conversation(&self, id: ConversationId) -> pi_js::Result<Option<ConversationRecord>> {
        self.inner.conversation(id).await
    }

    pub async fn entry(&self, id: EntryId) -> pi_js::Result<Option<EntryRecord>> {
        self.inner.entry(id).await
    }

    pub async fn typed_entry<D: DeserializeOwned>(
        &self,
        token: &Entry<D>,
        id: EntryId,
    ) -> pi_js::Result<Option<TypedEntry<D>>> {
        self.inner.typed_entry(token, id).await
    }

    pub async fn task(&self, id: TaskId) -> pi_js::Result<Option<ErasedTaskRecord>> {
        self.inner.task(id).await
    }

    pub async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
    ) -> pi_js::Result<Page<ConversationRecord, Cursor>> {
        self.inner.scan_conversations(query, limit, cursor).await
    }

    pub async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
    ) -> pi_js::Result<Page<EntryRecord, Cursor>> {
        self.inner.scan_entries(query, limit, cursor).await
    }

    pub async fn latest_head_marker(&self, conversation_id: ConversationId) -> pi_js::Result<Option<EntryRecord>> {
        self.inner.latest_head_marker(conversation_id).await
    }

    pub async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
    ) -> pi_js::Result<Page<ErasedTaskRecord, Cursor>> {
        self.inner.scan_tasks(query, limit, cursor).await
    }

    pub async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
    ) -> pi_js::Result<Option<SubmissionRecord>> {
        self.inner.submission_by_request(conversation_id, request_id).await
    }

    pub async fn create_conversation(&self, ownership: ConversationOwnership) -> pi_js::Result<ConversationRecord> {
        self.inner.create_conversation(ownership).await
    }

    pub async fn fork_conversation(
        &self,
        parent_conversation_id: ConversationId,
        at: EntryId,
        ownership: ConversationOwnership,
    ) -> pi_js::Result<ConversationRecord> {
        self.inner
            .fork_conversation(parent_conversation_id, at, ownership)
            .await
    }

    pub async fn append_entry(&self, conversation_id: ConversationId, value: EntryDraft) -> pi_js::Result<EntryRecord> {
        self.inner.append_entry(conversation_id, value).await
    }

    pub async fn append_typed_entry<D: Serialize>(
        &self,
        token: &Entry<D>,
        conversation_id: ConversationId,
        value: TypedEntryDraft<D>,
    ) -> pi_js::Result<TypedEntry<D>> {
        self.inner.append_typed_entry(token, conversation_id, value).await
    }

    pub async fn create_task<I: Serialize, S, R, H>(
        &self,
        task: &Task<I, S, R, H>,
        input: &I,
        options: TaskOptions,
    ) -> pi_js::Result<TaskId<R>> {
        self.inner.create_task(task, input, options).await
    }

    pub async fn create_task_erased(
        &self,
        task: &AnyTask,
        raw_input: JsonValue,
        options: TaskOptions,
    ) -> pi_js::Result<TaskId> {
        self.inner.create_task_erased(task, raw_input, options).await
    }

    pub async fn create_submission(&self, create: SubmissionCreate) -> pi_js::Result<SubmissionRecord> {
        self.inner.create_submission(create).await
    }

    pub fn settle_submission(&self, id: SubmissionId, settlement: SubmissionSettlement) -> pi_js::Result<()> {
        self.inner.settle_submission(id, settlement)
    }

    pub fn place_submission(&self, id: SubmissionId, entry: EntryId) -> pi_js::Result<()> {
        self.inner.place_submission(id, entry)
    }

    pub async fn doc_session<T: DeserializeOwned + Serialize>(
        &self,
        token: &SessionDocToken<T>,
    ) -> pi_js::Result<DocDraft<T>> {
        self.inner.doc_session(token).await
    }

    pub async fn doc_conversation<T: DeserializeOwned + Serialize>(
        &self,
        token: &impl ConversationDoc<T>,
        conversation_id: ConversationId,
    ) -> pi_js::Result<DocDraft<T>> {
        self.inner.doc_conversation(token, conversation_id).await
    }

    pub async fn doc_task<T: DeserializeOwned + Serialize>(
        &self,
        token: &TaskDocToken<T>,
        task_id: TaskId,
    ) -> pi_js::Result<DocDraft<T>> {
        self.inner.doc_task(token, task_id).await
    }

    pub async fn doc_session_family<T: DeserializeOwned + Serialize, I: Serialize>(
        &self,
        token: &SessionDocFamilyToken<T, I>,
        key: &str,
        seed: I,
    ) -> pi_js::Result<DocDraft<T>> {
        self.inner.doc_session_family(token, key, seed).await
    }

    pub async fn doc_conversation_family<T: DeserializeOwned + Serialize, I: Serialize>(
        &self,
        token: &impl ConversationDocFamily<T, I>,
        conversation_id: ConversationId,
        key: &str,
        seed: I,
    ) -> pi_js::Result<DocDraft<T>> {
        self.inner
            .doc_conversation_family(token, conversation_id, key, seed)
            .await
    }

    pub async fn doc_task_family<T: DeserializeOwned + Serialize, I: Serialize>(
        &self,
        token: &TaskDocFamilyToken<T, I>,
        task_id: TaskId,
        key: &str,
        seed: I,
    ) -> pi_js::Result<DocDraft<T>> {
        self.inner.doc_task_family(token, task_id, key, seed).await
    }

    pub async fn retire_doc_session<T>(&self, token: &SessionDocToken<T>) -> pi_js::Result<()> {
        self.inner.retire_doc_session(token).await
    }

    pub async fn retire_doc_conversation<T>(
        &self,
        token: &impl ConversationDoc<T>,
        conversation_id: ConversationId,
    ) -> pi_js::Result<()> {
        self.inner.retire_doc_conversation(token, conversation_id).await
    }

    pub async fn retire_doc_task<T>(&self, token: &TaskDocToken<T>, task_id: TaskId) -> pi_js::Result<()> {
        self.inner.retire_doc_task(token, task_id).await
    }

    pub async fn retire_doc_session_family<T, I>(
        &self,
        token: &SessionDocFamilyToken<T, I>,
        key: &str,
    ) -> pi_js::Result<()> {
        self.inner.retire_doc_session_family(token, key).await
    }

    pub async fn retire_doc_conversation_family<T, I>(
        &self,
        token: &impl ConversationDocFamily<T, I>,
        conversation_id: ConversationId,
        key: &str,
    ) -> pi_js::Result<()> {
        self.inner
            .retire_doc_conversation_family(token, conversation_id, key)
            .await
    }

    pub async fn retire_doc_task_family<T, I>(
        &self,
        token: &TaskDocFamilyToken<T, I>,
        task_id: TaskId,
        key: &str,
    ) -> pi_js::Result<()> {
        self.inner.retire_doc_task_family(token, task_id, key).await
    }
}

/// Concrete transaction backing one Session commit callback.
pub struct Transaction {
    host: Arc<dyn TransactionHost>,
    context: Context,
    scope: TransactionScope,
    sealed: AtomicBool,
    pending_count: AtomicUsize,
    pending_notify: Notify,
    state: Mutex<TransactionState>,
}

impl Transaction {
    pub fn new(host: Arc<dyn TransactionHost>, context: Context, scope: TransactionScope) -> Arc<Self> {
        Arc::new(Self {
            host,
            context,
            scope,
            sealed: AtomicBool::new(false),
            pending_count: AtomicUsize::new(0),
            pending_notify: Notify::new(),
            state: Mutex::new(TransactionState {
                has_table_write: false,
                writes: Vec::new(),
                created_conversation_ids: IndexSet::new(),
                fork_source_conversation_ids: IndexSet::new(),
                fork_source_document_ids: IndexSet::new(),
                tasks_by_id: IndexMap::new(),
                submissions: IndexMap::new(),
                submission_changes: Vec::new(),
                plans: Vec::new(),
                documents: Vec::new(),
                latest_document_by_address: IndexMap::new(),
            }),
        })
    }

    pub fn as_tx(self: &Arc<Self>) -> Tx {
        Tx::from_transaction(Arc::clone(self))
    }

    // ─── Table reads ────────────────────────────────────────────────────────

    pub async fn conversation(&self, id: ConversationId) -> pi_js::Result<Option<ConversationRecord>> {
        self.run_read("conversation", || async {
            self.host.storage().conversation(id, self.context.clone()).await
        })
        .await
    }

    pub async fn entry(&self, id: EntryId) -> pi_js::Result<Option<EntryRecord>> {
        self.run_read("entry", || async {
            Ok(self
                .host
                .storage()
                .entry(id, self.context.clone())
                .await?
                .map(|e| e.entry))
        })
        .await
    }

    pub async fn typed_entry<D: DeserializeOwned>(
        &self,
        token: &Entry<D>,
        id: EntryId,
    ) -> pi_js::Result<Option<TypedEntry<D>>> {
        self.run_read("entry", || async {
            let record = self
                .host
                .storage()
                .entry(id, self.context.clone())
                .await?
                .map(|e| e.entry);
            token.extract(record.as_ref())
        })
        .await
    }

    pub async fn task(&self, id: TaskId) -> pi_js::Result<Option<ErasedTaskRecord>> {
        self.run_read("task", || async { self.committed_task(id).await }).await
    }

    pub async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
    ) -> pi_js::Result<Page<ConversationRecord, Cursor>> {
        self.run_read("scanConversations", || async {
            self.host
                .storage()
                .scan_conversations(query, limit, cursor, self.context.clone())
                .await
        })
        .await
    }

    pub async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
    ) -> pi_js::Result<Page<EntryRecord, Cursor>> {
        self.run_read("scanEntries", || async {
            self.host
                .storage()
                .scan_entries(query, limit, cursor, self.context.clone())
                .await
        })
        .await
    }

    pub async fn latest_head_marker(&self, conversation_id: ConversationId) -> pi_js::Result<Option<EntryRecord>> {
        self.run_read("latestHeadMarker", || async {
            self.host
                .storage()
                .find_latest_head_marker(conversation_id, None, self.context.clone())
                .await
        })
        .await
    }

    pub async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
    ) -> pi_js::Result<Page<ErasedTaskRecord, Cursor>> {
        self.run_read("scanTasks", || async {
            self.host
                .storage()
                .scan_tasks(query, limit, cursor, self.context.clone())
                .await
        })
        .await
    }

    /// Internal: committed submission record.
    pub async fn submission(&self, id: SubmissionId) -> pi_js::Result<Option<SubmissionRecord>> {
        self.run_read("submission", || async {
            self.host.storage().submission(id, self.context.clone()).await
        })
        .await
    }

    pub async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
    ) -> pi_js::Result<Option<SubmissionRecord>> {
        self.run_read("submissionByRequest", || async {
            self.host
                .storage()
                .submission_by_request(conversation_id, request_id, self.context.clone())
                .await
        })
        .await
    }

    // ─── Table writes ───────────────────────────────────────────────────────

    pub async fn create_conversation(
        self: &Arc<Self>,
        ownership: ConversationOwnership,
    ) -> pi_js::Result<ConversationRecord> {
        self.run_write(|| self.stage_conversation(None, ownership, None)).await
    }

    /// Internal final-form bootstrap path for the reserved root identity.
    pub async fn create_root_conversation(self: &Arc<Self>) -> pi_js::Result<ConversationRecord> {
        self.run_write(|| self.stage_conversation(None, ConversationOwnership::Ownerless, Some(ROOT_CONVERSATION_ID)))
            .await
    }

    pub async fn fork_conversation(
        self: &Arc<Self>,
        parent_conversation_id: ConversationId,
        at: EntryId,
        ownership: ConversationOwnership,
    ) -> pi_js::Result<ConversationRecord> {
        self.run_write(|| {
            self.stage_conversation(
                Some(ConversationParent {
                    conversation_id: parent_conversation_id,
                    at,
                }),
                ownership,
                None,
            )
        })
        .await
    }

    async fn stage_conversation(
        self: &Arc<Self>,
        parent: Option<ConversationParent>,
        ownership: ConversationOwnership,
        reserved_id: Option<ConversationId>,
    ) -> pi_js::Result<ConversationRecord> {
        let owner_task_id = match ownership {
            ConversationOwnership::Ownerless => None,
            ConversationOwnership::Task { task_id } => Some(task_id),
        };
        let id = match reserved_id {
            Some(id) => id,
            None => mint_typed_id::<crate::ids::ConversationKind, _>(self.host.storage().as_ref()).await?,
        };
        self.assert_open()?;
        let owner = match owner_task_id {
            None => None,
            Some(task_id) => {
                let task = self.current_task(task_id).await?;
                self.assert_open()?;
                let Some(task) = task else {
                    return Err(pi_js::Error::msg(format!(
                        "Conversation owner task {task_id} does not exist"
                    )));
                };
                Some(ConversationOwner {
                    conversation_id: task.conversation_id,
                    task_id,
                })
            }
        };
        let record = ConversationRecord { id, parent, owner };
        let copies = match parent {
            None => Vec::new(),
            Some(parent) => {
                prepare_fork_document_copies(
                    self.host.storage().as_ref(),
                    parent.conversation_id,
                    parent.at,
                    id,
                    &self.context,
                )
                .await?
            }
        };
        self.assert_open()?;
        {
            let mut guard = lock(&self.state);
            for copy in copies {
                guard.fork_source_document_ids.insert(copy.source.id);
                let addr = copy.record.as_address();
                let addr_id = address_id(&addr);
                let entry = Arc::new(DocumentEntry {
                    address_id: addr_id.clone(),
                    address: addr,
                    retire_on_commit: Mutex::new(false),
                    has_draft_acquisition: AtomicBool::new(false),
                    staged_next: Arc::new(Mutex::new(None)),
                    init: OnceCell::new(),
                    inner: Mutex::new(DocumentEntryState {
                        definition: None,
                        target: Some(DocumentTarget::ForkCopy {
                            record: copy.record,
                            source: copy.source,
                        }),
                        change: None,
                        prepared: None,
                    }),
                });
                guard.documents.push(Arc::clone(&entry));
                guard.latest_document_by_address.insert(addr_id, entry);
            }
            if let Some(parent) = parent {
                guard.fork_source_conversation_ids.insert(parent.conversation_id);
            }
            guard.created_conversation_ids.insert(id);
            guard.writes.push(StorageWrite::Conversation { value: record.clone() });
        }
        self.host.conversation_created(self, &record).await?;
        self.assert_open()?;
        Ok(record)
    }

    pub async fn append_entry(&self, conversation_id: ConversationId, value: EntryDraft) -> pi_js::Result<EntryRecord> {
        self.run_write(|| async move {
            self.require_conversation(conversation_id).await?;
            self.assert_open()?;
            let id = mint_typed_id::<EntryKind, _>(self.host.storage().as_ref()).await?;
            self.assert_open()?;
            let data = match value.data {
                None => None,
                Some(raw) => {
                    let copied = copy_json(&raw, None)?;
                    if !is_json_value(&copied) {
                        return Err(pi_js::Error::js("TypeError", "Entry data must be strict JSON"));
                    }
                    Some(copied)
                }
            };
            let head = match value.head {
                Some(EntryDraftHead::SelfId(_)) => Some(id),
                Some(EntryDraftHead::Entry(target)) => Some(target),
                None => None,
            };
            let record = EntryRecord {
                id,
                conversation_id,
                kind: value.kind,
                model: value.model,
                data,
                head,
                edits: value.edits,
                by_task_id: self.scope.task_id,
            };
            lock(&self.state)
                .writes
                .push(StorageWrite::Entry { value: record.clone() });
            Ok(record)
        })
        .await
    }

    pub async fn append_typed_entry<D: Serialize>(
        &self,
        token: &Entry<D>,
        conversation_id: ConversationId,
        value: TypedEntryDraft<D>,
    ) -> pi_js::Result<TypedEntry<D>> {
        let encoded_data =
            serde_json::to_value(&value.data).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
        let draft = EntryDraft {
            kind: token.kind().to_owned(),
            model: value.model,
            data: Some(encoded_data),
            head: value.head,
            edits: value.edits,
        };
        let record = self.append_entry(conversation_id, draft).await?;
        Ok(TypedEntry {
            record,
            data: value.data,
        })
    }

    pub async fn create_task<I: Serialize, S, R, H>(
        &self,
        task: &Task<I, S, R, H>,
        input: &I,
        options: TaskOptions,
    ) -> pi_js::Result<TaskId<R>> {
        let raw_input = serde_json::to_value(input).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
        let id = self.create_task_erased(&task.erased, raw_input, options).await?;
        Ok(id.cast::<R>())
    }

    pub async fn create_task_erased(
        &self,
        task: &AnyTask,
        raw_input: JsonValue,
        options: TaskOptions,
    ) -> pi_js::Result<TaskId> {
        self.run_write(|| async move {
            let owner = match options.ownership {
                TaskOwnership::Conversation => None,
                TaskOwnership::Task { task_id } => {
                    let found = self.current_task(task_id).await?;
                    self.assert_open()?;
                    let Some(found) = found else {
                        return Err(pi_js::Error::msg(format!("Task owner {task_id} does not exist")));
                    };
                    if options.background {
                        return Err(pi_js::Error::js("TypeError", "A child task cannot be background"));
                    }
                    if let Some(cid) = options.conversation_id
                        && cid != found.conversation_id
                    {
                        return Err(pi_js::Error::msg(format!(
                            "A child task lives in its owner's conversation {}",
                            found.conversation_id
                        )));
                    }
                    Some(found)
                }
            };
            let conversation_id = owner
                .as_ref()
                .map(|o| o.conversation_id)
                .or(options.conversation_id)
                .or(self.scope.conversation_id)
                .ok_or_else(|| pi_js::Error::js("TypeError", "Tx.createTask() requires options.conversationId"))?;
            self.require_conversation(conversation_id).await?;
            self.assert_open()?;

            let raw_input = copy_json(&raw_input, None)?;
            if !is_json_value(&raw_input) {
                return Err(pi_js::Error::js("TypeError", "Task input must be strict JSON"));
            }

            let definition = &task.definition;
            let checkpoint = (definition.initial)(raw_input.clone())?;
            let checkpoint = copy_json(&checkpoint, None)?;
            if !is_json_value(&checkpoint) {
                return Err(pi_js::Error::js("TypeError", "Task checkpoint must be strict JSON"));
            }

            let id = mint_typed_id::<TaskKind, JsonValue>(self.host.storage().as_ref()).await?;
            self.assert_open()?;
            let record = TaskRecord {
                id,
                conversation_id,
                kind: definition.name.clone(),
                version: definition.version,
                input: raw_input,
                owner: owner.map(|o| o.id),
                background: options.background,
                abort_requested: false,
                state: TaskState::Pending { checkpoint },
                memos: None,
            };
            let mut guard = lock(&self.state);
            let entry = guard.tasks_by_id.entry(id).or_default();
            entry.write = Some(TaskWrite {
                kind: TaskWriteKind::Create,
                record,
            });
            Ok(id)
        })
        .await
    }

    pub async fn create_submission(&self, create: SubmissionCreate) -> pi_js::Result<SubmissionRecord> {
        self.run_write(|| async move {
            self.require_conversation(create.conversation_id).await?;
            self.assert_open()?;
            let id = mint_typed_id::<SubmissionKind, _>(self.host.storage().as_ref()).await?;
            self.assert_open()?;
            let state = match create.state {
                SubmissionState::Input(InputSubmissionState::Unanswered { entry, reason, detail }) => {
                    let copied_detail = match detail {
                        None => None,
                        Some(d) => Some(copy_json(&d, None)?),
                    };
                    SubmissionState::Input(InputSubmissionState::Unanswered {
                        entry,
                        reason,
                        detail: copied_detail,
                    })
                }
                SubmissionState::Write(WriteSubmissionState::Unanswered { reason, detail }) => {
                    let copied_detail = match detail {
                        None => None,
                        Some(d) => Some(copy_json(&d, None)?),
                    };
                    SubmissionState::Write(WriteSubmissionState::Unanswered {
                        reason,
                        detail: copied_detail,
                    })
                }
                other => other,
            };
            let record = SubmissionRecord {
                id,
                conversation_id: create.conversation_id,
                request_id: create.request_id,
                state,
            };
            lock(&self.state).submissions.insert(id, record.clone());
            Ok(record)
        })
        .await
    }

    pub fn settle_submission(&self, id: SubmissionId, settlement: SubmissionSettlement) -> pi_js::Result<()> {
        self.assert_open()?;
        let copied = match settlement {
            SubmissionSettlement::Done { answer } => SubmissionSettlement::Done { answer },
            SubmissionSettlement::Unanswered { reason, detail } => {
                let copied_detail = match detail {
                    None => None,
                    Some(v) => {
                        let c = copy_json(&v, None)?;
                        if !is_json_value(&c) {
                            return Err(pi_js::Error::js("TypeError", "Submission detail must be strict JSON"));
                        }
                        Some(c)
                    }
                };
                SubmissionSettlement::Unanswered {
                    reason,
                    detail: copied_detail,
                }
            }
        };
        let mut guard = lock(&self.state);
        guard.has_table_write = true;
        guard
            .submission_changes
            .push((id, SubmissionChange::Settlement(copied)));
        Ok(())
    }

    pub fn place_submission(&self, id: SubmissionId, entry: EntryId) -> pi_js::Result<()> {
        self.assert_open()?;
        let mut guard = lock(&self.state);
        guard.has_table_write = true;
        guard.submission_changes.push((id, SubmissionChange::Placed { entry }));
        Ok(())
    }

    /// Internal: replace one task record completely. Tasks change their own state through their runtime.
    pub fn set_task(&self, mut value: ErasedTaskRecord) -> pi_js::Result<()> {
        self.assert_open()?;
        value.input = copy_json(&value.input, None)?;
        value.state = match value.state {
            TaskState::Pending { checkpoint } => TaskState::Pending {
                checkpoint: copy_json(&checkpoint, None)?,
            },
            TaskState::Running { checkpoint } => TaskState::Running {
                checkpoint: copy_json(&checkpoint, None)?,
            },
            TaskState::Waiting { checkpoint, on, policy } => TaskState::Waiting {
                checkpoint: copy_json(&checkpoint, None)?,
                on,
                policy,
            },
            other => other,
        };
        let mut guard = lock(&self.state);
        guard.has_table_write = true;
        let entry = guard.tasks_by_id.entry(value.id).or_default();
        if let Some(candidate) = &entry.write {
            if candidate.record.state.is_terminal() {
                return Err(pi_js::Error::msg(format!(
                    "Task {} already has a terminal candidate",
                    value.id
                )));
            }
            if candidate.record.conversation_id != value.conversation_id {
                return Err(pi_js::Error::msg(format!(
                    "Task {} cannot change conversations",
                    value.id
                )));
            }
        }
        let kind = if entry.write.as_ref().is_some_and(|w| w.kind == TaskWriteKind::Create) {
            TaskWriteKind::Create
        } else {
            TaskWriteKind::Replace
        };
        entry.write = Some(TaskWrite { kind, record: value });
        Ok(())
    }

    /// Internal: candidate records of the tasks this transaction created or replaced so far.
    pub fn staged_tasks(&self) -> Vec<ErasedTaskRecord> {
        let guard = lock(&self.state);
        guard
            .tasks_by_id
            .values()
            .filter_map(|task| task.write.as_ref().map(|w| w.record.clone()))
            .collect()
    }

    /// Internal: conversations this transaction created or forked so far.
    pub fn staged_conversations(&self) -> Vec<ConversationRecord> {
        let guard = lock(&self.state);
        guard
            .writes
            .iter()
            .filter_map(|write| match write {
                StorageWrite::Conversation { value } => Some(value.clone()),
                _ => None,
            })
            .collect()
    }

    // ─── Documents ──────────────────────────────────────────────────────────

    pub async fn doc_session<T: DeserializeOwned + Serialize>(
        &self,
        token: &SessionDocToken<T>,
    ) -> pi_js::Result<DocDraft<T>> {
        self.doc_erased(&token.definition, None, None, None, None).await
    }

    pub async fn doc_conversation<T: DeserializeOwned + Serialize>(
        &self,
        token: &impl ConversationDoc<T>,
        conversation_id: ConversationId,
    ) -> pi_js::Result<DocDraft<T>> {
        self.doc_erased(token.definition(), Some(conversation_id), None, None, None)
            .await
    }

    pub async fn doc_task<T: DeserializeOwned + Serialize>(
        &self,
        token: &TaskDocToken<T>,
        task_id: TaskId,
    ) -> pi_js::Result<DocDraft<T>> {
        self.doc_erased(&token.definition, None, Some(task_id), None, None)
            .await
    }

    pub async fn doc_session_family<T: DeserializeOwned + Serialize, I: Serialize>(
        &self,
        token: &SessionDocFamilyToken<T, I>,
        key: &str,
        seed: I,
    ) -> pi_js::Result<DocDraft<T>> {
        let raw_seed = serde_json::to_value(seed).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
        self.doc_erased(&token.definition, None, None, Some(key.to_owned()), Some(raw_seed))
            .await
    }

    pub async fn doc_conversation_family<T: DeserializeOwned + Serialize, I: Serialize>(
        &self,
        token: &impl ConversationDocFamily<T, I>,
        conversation_id: ConversationId,
        key: &str,
        seed: I,
    ) -> pi_js::Result<DocDraft<T>> {
        let raw_seed = serde_json::to_value(seed).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
        self.doc_erased(
            token.definition(),
            Some(conversation_id),
            None,
            Some(key.to_owned()),
            Some(raw_seed),
        )
        .await
    }

    pub async fn doc_task_family<T: DeserializeOwned + Serialize, I: Serialize>(
        &self,
        token: &TaskDocFamilyToken<T, I>,
        task_id: TaskId,
        key: &str,
        seed: I,
    ) -> pi_js::Result<DocDraft<T>> {
        let raw_seed = serde_json::to_value(seed).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
        self.doc_erased(
            &token.definition,
            None,
            Some(task_id),
            Some(key.to_owned()),
            Some(raw_seed),
        )
        .await
    }

    pub async fn doc_erased<T>(
        &self,
        definition: &Arc<ErasedDocDefinition>,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<String>,
        seed: Option<JsonValue>,
    ) -> pi_js::Result<DocDraft<T>> {
        self.assert_open()?;
        let _pending = self.enter_pending();
        let resolved = resolve_address(definition, conversation_id, task_id, key)?;
        self.assert_task_documents_open(&resolved)?;

        let copied_seed = if definition.family {
            let s = seed.ok_or_else(|| pi_js::Error::js("TypeError", "Document family initial() requires a seed"))?;
            let copied = copy_json(&s, None)?;
            if !is_json_value(&copied) {
                return Err(pi_js::Error::js("TypeError", "Document seed must be strict JSON"));
            }
            Some(copied)
        } else {
            None
        };

        enum AcquireMode {
            JoinExisting,
            AcquireForkCopy(Arc<ErasedDocDefinition>, DocumentCreate, DocumentCopySource),
            AcquireFresh(Option<JsonValue>, bool),
        }

        let (doc_entry, mode) = {
            let mut guard = lock(&self.state);
            let latest = guard.latest_document_by_address.get(&resolved.id).cloned();
            if let Some(latest) = &latest
                && !*lock(&latest.retire_on_commit)
            {
                if latest.has_draft_acquisition.load(Ordering::Acquire) {
                    (Arc::clone(latest), AcquireMode::JoinExisting)
                } else {
                    let fork_target = {
                        let inner = lock(&latest.inner);
                        match &inner.target {
                            Some(DocumentTarget::ForkCopy { record, source }) => Some((record.clone(), *source)),
                            _ => None,
                        }
                    };
                    if let Some((record, source)) = fork_target {
                        latest.has_draft_acquisition.store(true, Ordering::Release);
                        (
                            Arc::clone(latest),
                            AcquireMode::AcquireForkCopy(Arc::clone(definition), record, source),
                        )
                    } else {
                        let entry = Arc::new(DocumentEntry {
                            address_id: resolved.id.clone(),
                            address: resolved.address.clone(),
                            retire_on_commit: Mutex::new(false),
                            has_draft_acquisition: AtomicBool::new(true),
                            staged_next: Arc::new(Mutex::new(None)),
                            init: OnceCell::new(),
                            inner: Mutex::new(DocumentEntryState {
                                definition: Some(Arc::clone(definition)),
                                target: None,
                                change: None,
                                prepared: None,
                            }),
                        });
                        guard.documents.push(Arc::clone(&entry));
                        guard
                            .latest_document_by_address
                            .insert(resolved.id.clone(), Arc::clone(&entry));
                        (entry, AcquireMode::AcquireFresh(copied_seed, false))
                    }
                }
            } else {
                let skip_load = latest.as_ref().is_some_and(|e| *lock(&e.retire_on_commit));
                let entry = Arc::new(DocumentEntry {
                    address_id: resolved.id.clone(),
                    address: resolved.address.clone(),
                    retire_on_commit: Mutex::new(false),
                    has_draft_acquisition: AtomicBool::new(true),
                    staged_next: Arc::new(Mutex::new(None)),
                    init: OnceCell::new(),
                    inner: Mutex::new(DocumentEntryState {
                        definition: Some(Arc::clone(definition)),
                        target: None,
                        change: None,
                        prepared: None,
                    }),
                });
                guard.documents.push(Arc::clone(&entry));
                guard
                    .latest_document_by_address
                    .insert(resolved.id.clone(), Arc::clone(&entry));
                (entry, AcquireMode::AcquireFresh(copied_seed, skip_load))
            }
        };

        let init_res = doc_entry
            .init
            .get_or_init(|| async {
                let res = match mode {
                    AcquireMode::JoinExisting => Ok(()),
                    AcquireMode::AcquireForkCopy(def, record, source) => {
                        self.acquire_fork_copy(&doc_entry, def, record, source).await
                    }
                    AcquireMode::AcquireFresh(seed_opt, skip_load) => {
                        self.acquire(&doc_entry, seed_opt, skip_load).await
                    }
                };
                res.map_err(Arc::new)
            })
            .await;

        if let Err(err) = init_res {
            return Err(clone_pi_error(err));
        }

        let change = lock(&doc_entry.inner)
            .change
            .clone()
            .ok_or_else(|| pi_js::Error::msg("Document acquisition produced no active change"))?;
        Ok(DocDraft {
            change,
            staged_next: Arc::clone(&doc_entry.staged_next),
            _marker: PhantomData,
        })
    }

    pub async fn retire_doc_session<T>(&self, token: &SessionDocToken<T>) -> pi_js::Result<()> {
        self.retire_doc_erased(&token.definition, None, None, None).await
    }

    pub async fn retire_doc_conversation<T>(
        &self,
        token: &impl ConversationDoc<T>,
        conversation_id: ConversationId,
    ) -> pi_js::Result<()> {
        self.retire_doc_erased(token.definition(), Some(conversation_id), None, None)
            .await
    }

    pub async fn retire_doc_task<T>(&self, token: &TaskDocToken<T>, task_id: TaskId) -> pi_js::Result<()> {
        self.retire_doc_erased(&token.definition, None, Some(task_id), None)
            .await
    }

    pub async fn retire_doc_session_family<T, I>(
        &self,
        token: &SessionDocFamilyToken<T, I>,
        key: &str,
    ) -> pi_js::Result<()> {
        self.retire_doc_erased(&token.definition, None, None, Some(key.to_owned()))
            .await
    }

    pub async fn retire_doc_conversation_family<T, I>(
        &self,
        token: &impl ConversationDocFamily<T, I>,
        conversation_id: ConversationId,
        key: &str,
    ) -> pi_js::Result<()> {
        self.retire_doc_erased(token.definition(), Some(conversation_id), None, Some(key.to_owned()))
            .await
    }

    pub async fn retire_doc_task_family<T, I>(
        &self,
        token: &TaskDocFamilyToken<T, I>,
        task_id: TaskId,
        key: &str,
    ) -> pi_js::Result<()> {
        self.retire_doc_erased(&token.definition, None, Some(task_id), Some(key.to_owned()))
            .await
    }

    pub async fn retire_doc_erased(
        &self,
        definition: &Arc<ErasedDocDefinition>,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<String>,
    ) -> pi_js::Result<()> {
        self.assert_open()?;
        let _pending = self.enter_pending();
        let resolved = resolve_address(definition, conversation_id, task_id, key)?;

        enum RetireAction {
            AlreadyRetired,
            MarkExistingDraft(Arc<DocumentEntry>),
            FindNew(Arc<DocumentEntry>),
        }

        let action = {
            let mut guard = lock(&self.state);
            let latest = guard.latest_document_by_address.get(&resolved.id).cloned();
            if let Some(latest) = &latest {
                let mut retired = lock(&latest.retire_on_commit);
                if *retired {
                    RetireAction::AlreadyRetired
                } else {
                    let fork_record = {
                        let inner = lock(&latest.inner);
                        match &inner.target {
                            Some(DocumentTarget::ForkCopy { record, .. })
                                if !latest.has_draft_acquisition.load(Ordering::Acquire) =>
                            {
                                Some(record.clone())
                            }
                            _ => None,
                        }
                    };
                    if let Some(record) = fork_record {
                        check_create_scope(definition, &record)?;
                        *retired = true;
                        RetireAction::AlreadyRetired
                    } else if latest.has_draft_acquisition.load(Ordering::Acquire) {
                        // Retirement of an acquired draft persists its final content before retirement.
                        *retired = true;
                        RetireAction::MarkExistingDraft(Arc::clone(latest))
                    } else {
                        let entry = Arc::new(DocumentEntry {
                            address_id: resolved.id.clone(),
                            address: resolved.address.clone(),
                            retire_on_commit: Mutex::new(true),
                            has_draft_acquisition: AtomicBool::new(false),
                            staged_next: Arc::new(Mutex::new(None)),
                            init: OnceCell::new(),
                            inner: Mutex::new(DocumentEntryState {
                                definition: Some(Arc::clone(definition)),
                                target: None,
                                change: None,
                                prepared: None,
                            }),
                        });
                        guard.documents.push(Arc::clone(&entry));
                        guard.latest_document_by_address.insert(resolved.id, Arc::clone(&entry));
                        RetireAction::FindNew(entry)
                    }
                }
            } else {
                let entry = Arc::new(DocumentEntry {
                    address_id: resolved.id.clone(),
                    address: resolved.address.clone(),
                    retire_on_commit: Mutex::new(true),
                    has_draft_acquisition: AtomicBool::new(false),
                    staged_next: Arc::new(Mutex::new(None)),
                    init: OnceCell::new(),
                    inner: Mutex::new(DocumentEntryState {
                        definition: Some(Arc::clone(definition)),
                        target: None,
                        change: None,
                        prepared: None,
                    }),
                });
                guard.documents.push(Arc::clone(&entry));
                guard.latest_document_by_address.insert(resolved.id, Arc::clone(&entry));
                RetireAction::FindNew(entry)
            }
        };

        match action {
            RetireAction::AlreadyRetired => Ok(()),
            RetireAction::MarkExistingDraft(latest) => {
                if let Some(res) = latest.init.get()
                    && let Err(err) = res
                {
                    return Err(clone_pi_error(err));
                }
                // If initialization is currently running concurrently, wait for it cleanly via get_or_init no-op:
                let joined = latest
                    .init
                    .get_or_init(|| async { Err(Arc::new(pi_js::Error::msg("Document acquisition did not start"))) })
                    .await;
                if let Err(err) = joined {
                    return Err(clone_pi_error(err));
                }
                Ok(())
            }
            RetireAction::FindNew(entry) => self.find_retirement(&entry).await,
        }
    }

    async fn acquire(&self, entry: &Arc<DocumentEntry>, seed: Option<JsonValue>, skip_load: bool) -> pi_js::Result<()> {
        let definition = lock(&entry.inner)
            .definition
            .clone()
            .ok_or_else(|| pi_js::Error::msg("Document entry missing definition"))?;
        let loaded = if skip_load {
            None
        } else {
            self.host
                .load(&definition, &entry.address_id, &entry.address, &self.context)
                .await?
        };
        self.assert_open()?;
        if let Some(loaded) = loaded {
            check_record_scope(&definition, &loaded.record)?;
            check_record_version(
                &definition,
                loaded.record.id,
                &loaded.record.kind,
                loaded.stored_version(),
            )?;
            let change = loaded.tracker.begin_change();
            let mut inner = lock(&entry.inner);
            inner.target = Some(DocumentTarget::Loaded { document: loaded });
            inner.change = Some(change);
            return Ok(());
        }

        match entry.address.scope {
            DocumentScope::Session => {}
            DocumentScope::Conversation { conversation_id } => {
                self.require_conversation(conversation_id).await?;
            }
            DocumentScope::Task { task_id } => {
                let task = self.current_task(task_id).await?;
                let Some(task) = task else {
                    return Err(pi_js::Error::msg(format!("Task {task_id} does not exist")));
                };
                if task.state.is_terminal() {
                    return Err(pi_js::Error::msg(format!("Task {task_id} is terminal")));
                }
            }
        }
        self.assert_open()?;

        let initial_obj = if definition.family {
            definition.initial(seed.as_ref())?
        } else {
            definition.initial(None)?
        };
        let copied_val = copy_json(&JsonValue::Object(initial_obj), None)?;
        let JsonValue::Object(value) = copied_val else {
            return Err(pi_js::Error::js("TypeError", "Document root must be a JSON object"));
        };
        let id = mint_typed_id::<crate::ids::DocumentKind, _>(self.host.storage().as_ref()).await?;
        self.assert_open()?;
        let tracker = track(value);
        let change = tracker.begin_change();
        let record = document_create(&definition, &entry.address, id);
        let mut inner = lock(&entry.inner);
        inner.target = Some(DocumentTarget::Created {
            record,
            version: definition.version,
            tracker,
        });
        inner.change = Some(change);
        Ok(())
    }

    async fn acquire_fork_copy(
        &self,
        entry: &Arc<DocumentEntry>,
        definition: Arc<ErasedDocDefinition>,
        record: DocumentCreate,
        source: DocumentCopySource,
    ) -> pi_js::Result<()> {
        let stored = self
            .host
            .storage()
            .document(source.id, source.at, self.context.clone())
            .await?;
        self.assert_open()?;
        let Some(stored) = stored else {
            return Err(pi_js::Error::msg(format!(
                "Fork source document {} cannot be read",
                source.id
            )));
        };
        if stored.record.scope.kind() != crate::types::DocumentScopeKind::Conversation
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
        let value = materialize_create_value(&definition, &record, stored.version, &stored.value)?;
        let tracker = track(value);
        let change = tracker.begin_change();
        let mut inner = lock(&entry.inner);
        inner.definition = Some(Arc::clone(&definition));
        inner.target = Some(DocumentTarget::Created {
            record,
            version: definition.version,
            tracker,
        });
        inner.change = Some(change);
        Ok(())
    }

    async fn find_retirement(&self, entry: &Arc<DocumentEntry>) -> pi_js::Result<()> {
        let definition = lock(&entry.inner)
            .definition
            .clone()
            .ok_or_else(|| pi_js::Error::msg("Document entry missing definition"))?;
        let record = match self.host.cached(&entry.address_id) {
            Some(cached) => Some(cached.record.clone()),
            None => {
                self.host
                    .storage()
                    .find_document(entry.address.clone(), DocumentPoint::Current, self.context.clone())
                    .await?
            }
        };
        self.assert_open()?;
        let Some(record) = record else {
            return Ok(());
        };
        check_record_scope(&definition, &record)?;
        lock(&entry.inner).target = Some(DocumentTarget::RetireOnly { record });
        Ok(())
    }

    // ─── Settlement ─────────────────────────────────────────────────────────

    /// Seal after callback failure: abort every change and drain any in-flight operations.
    pub async fn settle_failure(&self) {
        self.sealed.store(true, Ordering::Release);
        self.abort_changes();
        self.drain_pending().await;
    }

    /// Seal after callback success, prepare every open change, and assemble the atomic batch.
    pub async fn settle_success(&self) -> pi_js::Result<Vec<StorageWrite>> {
        self.sealed.store(true, Ordering::Release);
        if self.pending_count.load(Ordering::Acquire) > 0 {
            self.abort_changes();
            self.drain_pending().await;
            return Err(pi_js::Error::msg(
                "Session commit callback settled before its pending Tx operations",
            ));
        }
        let res = self.prepare_and_assemble().await;
        if res.is_err() {
            self.abort_changes();
        }
        res
    }

    async fn prepare_and_assemble(&self) -> pi_js::Result<Vec<StorageWrite>> {
        let docs: Vec<Arc<DocumentEntry>> = lock(&self.state).documents.clone();
        for doc in &docs {
            let change_opt = lock(&doc.inner).change.clone();
            if let Some(change) = change_opt {
                let next_opt = lock(&doc.staged_next).clone();
                let prepared = match next_opt {
                    Some(next_obj) => change.prepare_with(next_obj)?,
                    None => change.prepare()?,
                };
                lock(&doc.inner).prepared = Some(prepared);
            }
        }
        self.assemble().await
    }

    /// Abort every prepared change after Storage failure or when no write is required.
    pub fn discard(&self) {
        self.abort_changes();
    }

    /// Adopt every prepared change by pointer swap after Storage success and describe the publication.
    pub fn adopt(&self, seq: Seq) -> pi_js::Result<Vec<DocumentCommitChange>> {
        let mut publications = Vec::new();
        let mut guard = lock(&self.state);
        for plan in &guard.plans {
            let committed = plan.record.is_committed();
            let mut record = match &plan.record {
                PlanRecord::Committed(rec) => rec.clone(),
                PlanRecord::Create(create) => create.clone().into_record(seq),
            };
            if plan.retire {
                record.retired_at = Some(seq);
            }
            if let Some(change) = &plan.change {
                let adopt_it = match &change.loaded {
                    None => !plan.retire,
                    Some(_) => !change.prepared.ops().is_empty(),
                };
                if adopt_it {
                    change.tracker.adopt(&change.prepared)?;
                } else {
                    change.prepared.abort();
                }
                if let Some(loaded) = &change.loaded {
                    if loaded.stored_version() < change.version {
                        loaded.set_stored_version(change.version);
                    }
                    if let Some(StorageWrite::DocumentChange { content, .. }) = &plan.content {
                        match content {
                            DocumentContent::Base { .. } => loaded.reset_deltas_since_base(),
                            DocumentContent::Delta { .. } => loaded.increment_deltas_since_base(),
                        }
                    }
                } else if !plan.retire {
                    self.host.install(Arc::new(LoadedDocument::new(
                        plan.address_id.clone(),
                        record.clone(),
                        change.version,
                        change.version,
                        0,
                        change.tracker.clone(),
                    )));
                }
            }
            let conversation_id = plan.conversation_id;
            if plan.retire {
                if committed {
                    self.host.evict(&plan.address_id, record.id);
                }
                publications.push(DocumentCommitChange::Document {
                    record,
                    conversation_id,
                    version: None,
                    value: None,
                    ops: Vec::new(),
                });
            } else if let Some(StorageWrite::DocumentCopy { source, .. }) = &plan.content {
                let cid =
                    conversation_id.ok_or_else(|| pi_js::Error::msg("DocumentCopy missing resolved conversationId"))?;
                publications.push(DocumentCommitChange::DocumentCopy {
                    record,
                    conversation_id: cid,
                    source: *source,
                });
            } else if let Some(change) = &plan.change
                && publishes(plan)
            {
                let ops = if change.loaded.is_none() {
                    Vec::new()
                } else {
                    change.prepared.ops().to_vec()
                };
                publications.push(DocumentCommitChange::Document {
                    record,
                    conversation_id,
                    version: Some(change.version),
                    value: Some(change.prepared.value().clone()),
                    ops,
                });
            }
        }
        guard.plans.clear();
        Ok(publications)
    }

    async fn assemble(&self) -> pi_js::Result<Vec<StorageWrite>> {
        let storage = Arc::clone(self.host.storage());
        let docs: Vec<Arc<DocumentEntry>> = lock(&self.state).documents.clone();
        let mut plans = Vec::new();
        for document in &docs {
            if let Some(plan) = plan_document(document)? {
                plans.push(plan);
            }
        }
        self.reject_fork_source_writes(&plans)?;
        self.validate_owners().await?;

        let replace_candidates: Vec<(TaskId, ConversationId)> = {
            let guard = lock(&self.state);
            guard
                .tasks_by_id
                .iter()
                .filter_map(|(id, task)| match &task.write {
                    Some(w) if w.kind == TaskWriteKind::Replace => Some((*id, w.record.conversation_id)),
                    _ => None,
                })
                .collect()
        };
        for (id, candidate_cid) in replace_candidates {
            let committed = self.committed_task(id).await?;
            let Some(committed) = committed else {
                return Err(pi_js::Error::msg(format!("Task {id} does not exist")));
            };
            if committed.state.is_terminal() {
                return Err(pi_js::Error::msg(format!("Task {id} is already terminal")));
            }
            if committed.conversation_id != candidate_cid {
                return Err(pi_js::Error::msg(format!("Task {id} cannot change conversations")));
            }
        }

        // Terminal settlement retires every task document, including ones created by this transaction.
        let (terminal_task_ids, created_terminal_task_ids): (IndexSet<TaskId>, IndexSet<TaskId>) = {
            let guard = lock(&self.state);
            let mut all = IndexSet::new();
            let mut created = IndexSet::new();
            for task in guard.tasks_by_id.values() {
                if let Some(w) = &task.write
                    && w.record.state.is_terminal()
                {
                    all.insert(w.record.id);
                    if w.kind == TaskWriteKind::Create {
                        created.insert(w.record.id);
                    }
                }
            }
            (all, created)
        };
        if !terminal_task_ids.is_empty() {
            let mut retiring = IndexSet::new();
            for plan in &mut plans {
                if let DocumentScope::Task { task_id } = plan.record.scope()
                    && terminal_task_ids.contains(&task_id)
                {
                    plan.retire = true;
                    retiring.insert(plan.record.id());
                }
            }
            for task_id in &terminal_task_ids {
                if created_terminal_task_ids.contains(task_id) {
                    continue;
                }
                let mut cursor: Option<Cursor> = None;
                loop {
                    let page = storage
                        .scan_documents(
                            DocumentQuery {
                                scope: DocumentScope::Task { task_id: *task_id },
                                at: DocumentPoint::Current,
                                kind: None,
                            },
                            INTERNAL_SCAN_PAGE_SIZE,
                            cursor,
                            self.context.clone(),
                        )
                        .await?;
                    for record in page.items {
                        if retiring.contains(&record.id) {
                            continue;
                        }
                        let addr = DocumentAddress {
                            kind: record.kind.clone(),
                            scope: record.scope,
                            key: record.key.clone(),
                        };
                        retiring.insert(record.id);
                        plans.push(DocumentPlan {
                            address_id: address_id(&addr),
                            record: PlanRecord::Committed(record),
                            retire: true,
                            content: None,
                            change: None,
                            conversation_id: None,
                        });
                    }
                    match page.next {
                        Some(next) => cursor = Some(next),
                        None => break,
                    }
                }
            }
        }

        // Resolve publication ownership before Storage admission so adoption remains synchronous.
        for plan in &mut plans {
            if !publishes(plan) {
                continue;
            }
            match plan.record.scope() {
                DocumentScope::Session => {}
                DocumentScope::Conversation { conversation_id } => {
                    plan.conversation_id = Some(conversation_id);
                }
                DocumentScope::Task { task_id } => {
                    let existing = lock(&self.state)
                        .tasks_by_id
                        .get(&task_id)
                        .and_then(|t| t.publication_conversation_id);
                    let cid = match existing {
                        Some(cid) => Some(cid),
                        None => {
                            let resolved = self.current_task(task_id).await?.map(|t| t.conversation_id);
                            if let Some(cid) = resolved {
                                lock(&self.state)
                                    .tasks_by_id
                                    .entry(task_id)
                                    .or_default()
                                    .publication_conversation_id = Some(cid);
                            }
                            resolved
                        }
                    };
                    plan.conversation_id = cid;
                }
            }
        }

        let submission_changes = lock(&self.state).submission_changes.clone();
        for (id, change) in submission_changes {
            let staged = lock(&self.state).submissions.get(&id).cloned();
            let current = match staged {
                Some(rec) => rec,
                None => match storage.submission(id, self.context.clone()).await? {
                    Some(rec) => rec,
                    None => return Err(pi_js::Error::msg(format!("Submission {id} does not exist"))),
                },
            };
            if let Some(next) = apply_submission_change(&current, change)? {
                lock(&self.state).submissions.insert(id, next);
            }
        }

        let mut guard = lock(&self.state);
        let mut writes = guard.writes.clone();
        for value in guard.submissions.values() {
            writes.push(StorageWrite::Submission { value: value.clone() });
        }
        for task in guard.tasks_by_id.values() {
            if let Some(w) = &task.write {
                writes.push(StorageWrite::Task {
                    value: w.record.clone(),
                });
            }
        }
        for plan in &mut plans {
            if let Some(StorageWrite::DocumentChange {
                id,
                content: DocumentContent::Delta { version, .. },
            }) = &plan.content
                && let Some(change) = &plan.change
                && let Some(loaded) = &change.loaded
                && let Some(def) = &change.definition
            {
                let info = CheckpointInfo {
                    deltas_since_base: loaded.deltas_since_base(),
                };
                if def.checkpoint_when(change.prepared.value(), change.prepared.ops(), info)? {
                    plan.content = Some(StorageWrite::DocumentChange {
                        id: *id,
                        content: DocumentContent::Base {
                            version: *version,
                            value: change.prepared.value().clone(),
                        },
                    });
                }
            }
            if let Some(content) = &plan.content {
                writes.push(content.clone());
            }
            if plan.retire {
                writes.push(StorageWrite::DocumentRetire { id: plan.record.id() });
            }
        }
        guard.plans = plans;
        Ok(writes)
    }

    // ─── Helpers ────────────────────────────────────────────────────────────

    async fn validate_owners(&self) -> pi_js::Result<()> {
        let owners: Vec<(&'static str, TaskId)> = {
            let guard = lock(&self.state);
            let mut list = Vec::new();
            for write in &guard.writes {
                if let StorageWrite::Conversation { value } = write
                    && let Some(owner) = &value.owner
                {
                    list.push(("Conversation owner task", owner.task_id));
                }
            }
            for task in guard.tasks_by_id.values() {
                if let Some(w) = &task.write
                    && w.kind == TaskWriteKind::Create
                    && let Some(owner_id) = w.record.owner
                {
                    list.push(("Task owner", owner_id));
                }
            }
            list
        };
        for (what, task_id) in owners {
            let task = self.current_task(task_id).await?;
            let Some(task) = task else {
                return Err(pi_js::Error::msg(format!("{what} {task_id} does not exist")));
            };
            match task.state {
                TaskState::Terminal { .. } => {
                    return Err(pi_js::Error::msg(format!("{what} {task_id} is terminal")));
                }
                TaskState::Completing { .. } => {
                    return Err(pi_js::Error::msg(format!("{what} {task_id} is completing")));
                }
                _ => {}
            }
            if task.abort_requested {
                return Err(pi_js::Error::msg(format!("{what} {task_id} is abort-marked")));
            }
        }
        Ok(())
    }

    fn reject_fork_source_writes(&self, plans: &[DocumentPlan]) -> pi_js::Result<()> {
        let guard = lock(&self.state);
        for plan in plans {
            if plan.content.is_none() && !plan.retire {
                continue;
            }
            let id = plan.record.id();
            if guard.fork_source_document_ids.contains(&id) {
                return Err(pi_js::Error::msg(format!(
                    "Cannot change fork source document {id} in the fork transaction"
                )));
            }
            if let DocumentScope::Conversation { conversation_id } = plan.record.scope()
                && guard.fork_source_conversation_ids.contains(&conversation_id)
                && plan.record.fork() == Some(DocumentFork::Current)
            {
                return Err(pi_js::Error::msg(format!(
                    "Cannot fork conversation {conversation_id} while changing its current-policy documents"
                )));
            }
        }
        Ok(())
    }

    fn abort_changes(&self) {
        let docs: Vec<Arc<DocumentEntry>> = lock(&self.state).documents.clone();
        for doc in docs {
            if let Some(change) = lock(&doc.inner).change.clone() {
                change.abort();
            }
        }
    }

    fn assert_open(&self) -> pi_js::Result<()> {
        if self.sealed.load(Ordering::Acquire) {
            return Err(pi_js::Error::msg("Transaction has settled"));
        }
        Ok(())
    }

    fn assert_task_documents_open(&self, resolved: &ResolvedAddress) -> pi_js::Result<()> {
        if let DocumentScope::Task { task_id } = resolved.address.scope {
            let guard = lock(&self.state);
            if guard
                .tasks_by_id
                .get(&task_id)
                .and_then(|t| t.write.as_ref())
                .is_some_and(|w| w.record.state.is_terminal())
            {
                return Err(pi_js::Error::msg(format!("Task {task_id} is terminal")));
            }
        }
        Ok(())
    }

    fn enter_pending(&self) -> PendingOpGuard<'_> {
        self.pending_count.fetch_add(1, Ordering::AcqRel);
        PendingOpGuard {
            count: &self.pending_count,
            notify: &self.pending_notify,
        }
    }

    async fn drain_pending(&self) {
        while self.pending_count.load(Ordering::Acquire) > 0 {
            let notified = self.pending_notify.notified();
            if self.pending_count.load(Ordering::Acquire) == 0 {
                break;
            }
            notified.await;
        }
    }

    async fn run_read<T, F, Fut>(&self, method: &str, read: F) -> pi_js::Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = pi_js::Result<T>>,
    {
        self.assert_open()?;
        if lock(&self.state).has_table_write {
            return Err(ReadAfterWrite::new(method).into_error());
        }
        let _pending = self.enter_pending();
        read().await
    }

    async fn run_write<T, F, Fut>(&self, write: F) -> pi_js::Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = pi_js::Result<T>>,
    {
        self.assert_open()?;
        lock(&self.state).has_table_write = true;
        let _pending = self.enter_pending();
        write().await
    }

    async fn require_conversation(&self, id: ConversationId) -> pi_js::Result<()> {
        if lock(&self.state).created_conversation_ids.contains(&id) {
            return Ok(());
        }
        if self
            .host
            .storage()
            .conversation(id, self.context.clone())
            .await?
            .is_none()
        {
            return Err(pi_js::Error::msg(format!("Conversation {id} does not exist")));
        }
        Ok(())
    }

    async fn current_task(&self, id: TaskId) -> pi_js::Result<Option<ErasedTaskRecord>> {
        if let Some(rec) = lock(&self.state)
            .tasks_by_id
            .get(&id)
            .and_then(|t| t.write.as_ref().map(|w| w.record.clone()))
        {
            return Ok(Some(rec));
        }
        self.committed_task(id).await
    }

    async fn committed_task(&self, id: TaskId) -> pi_js::Result<Option<ErasedTaskRecord>> {
        let cell = {
            let mut guard = lock(&self.state);
            Arc::clone(&guard.tasks_by_id.entry(id).or_default().committed_read)
        };
        let res = cell
            .get_or_init(|| async {
                self.host
                    .storage()
                    .task(id, self.context.clone())
                    .await
                    .map_err(Arc::new)
            })
            .await;
        match res {
            Ok(opt) => Ok(opt.clone()),
            Err(err) => Err(clone_pi_error(err)),
        }
    }
}

fn plan_document(document: &Arc<DocumentEntry>) -> pi_js::Result<Option<DocumentPlan>> {
    let retire = *lock(&document.retire_on_commit);
    let inner = lock(&document.inner);
    let Some(target) = &inner.target else {
        return Ok(None);
    };
    match target {
        DocumentTarget::Created {
            record,
            version,
            tracker,
        } => {
            let prepared = inner
                .prepared
                .clone()
                .ok_or_else(|| pi_js::Error::msg("Created document missing prepared change"))?;
            let content = StorageWrite::DocumentCreate {
                record: record.clone(),
                version: *version,
                value: prepared.value().clone(),
            };
            Ok(Some(DocumentPlan {
                address_id: document.address_id.clone(),
                record: PlanRecord::Create(record.clone()),
                retire,
                content: Some(content),
                change: Some(DocumentPlanChange {
                    tracker: tracker.clone(),
                    prepared,
                    version: *version,
                    loaded: None,
                    definition: None,
                }),
                conversation_id: None,
            }))
        }
        DocumentTarget::ForkCopy { record, source } => Ok(Some(DocumentPlan {
            address_id: document.address_id.clone(),
            record: PlanRecord::Create(record.clone()),
            retire,
            content: Some(StorageWrite::DocumentCopy {
                record: record.clone(),
                source: *source,
            }),
            change: None,
            conversation_id: None,
        })),
        DocumentTarget::RetireOnly { record } => Ok(Some(DocumentPlan {
            address_id: document.address_id.clone(),
            record: PlanRecord::Committed(record.clone()),
            retire,
            content: None,
            change: None,
            conversation_id: None,
        })),
        DocumentTarget::Loaded { document: loaded } => {
            let definition = inner
                .definition
                .clone()
                .ok_or_else(|| pi_js::Error::msg("Loaded document missing definition"))?;
            let prepared = inner
                .prepared
                .clone()
                .ok_or_else(|| pi_js::Error::msg("Loaded document missing prepared change"))?;
            let version = definition.version;
            let id = loaded.record.id;
            let content = if loaded.stored_version() < version {
                Some(StorageWrite::DocumentChange {
                    id,
                    content: DocumentContent::Base {
                        version,
                        value: prepared.value().clone(),
                    },
                })
            } else if !prepared.ops().is_empty() {
                Some(StorageWrite::DocumentChange {
                    id,
                    content: DocumentContent::Delta {
                        version,
                        ops: prepared.ops().to_vec(),
                    },
                })
            } else {
                None
            };
            Ok(Some(DocumentPlan {
                address_id: document.address_id.clone(),
                record: PlanRecord::Committed(loaded.record.clone()),
                retire,
                content,
                change: Some(DocumentPlanChange {
                    tracker: loaded.tracker.clone(),
                    prepared,
                    version,
                    loaded: Some(Arc::clone(loaded)),
                    definition: Some(definition),
                }),
                conversation_id: None,
            }))
        }
    }
}

fn publishes(plan: &DocumentPlan) -> bool {
    if plan.retire || plan.content.is_some() {
        return true;
    }
    match &plan.change {
        None => true,
        Some(change) => change.loaded.is_none(),
    }
}
