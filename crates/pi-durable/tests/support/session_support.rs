//! Port of packages/durable/test/session-support.ts

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_chord::{Context, background_context};
use pi_durable::session::{CommitListener, Session, SessionImpl};
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::types::{
    CommitChange, CommitPublication, ConversationId, ConversationOwnership, ConversationQuery, ConversationRecord,
    Cursor, DocumentAddress, DocumentCommitChange, DocumentId, DocumentPoint, DocumentQuery, DocumentRecord, EntryId,
    EntryLookup, EntryQuery, EntryRecord, ErasedTaskRecord, Page, Seq, Storage, StorageWrite, StoredDocument,
    SubmissionId, SubmissionQuery, SubmissionRecord, TaskId, TaskQuery,
};
use tokio::sync::watch;

pub fn context() -> Context {
    background_context()
}

struct HeldGate {
    entered_tx: watch::Sender<bool>,
    entered_rx: watch::Receiver<bool>,
    release_tx: watch::Sender<bool>,
    release_rx: watch::Receiver<bool>,
}

impl HeldGate {
    fn new() -> Arc<Self> {
        let (entered_tx, entered_rx) = watch::channel(false);
        let (release_tx, release_rx) = watch::channel(false);
        Arc::new(Self {
            entered_tx,
            entered_rx,
            release_tx,
            release_rx,
        })
    }

    async fn arrive_and_wait(&self) {
        let _ = self.entered_tx.send(true);
        let mut rx = self.release_rx.clone();
        let _ = rx.wait_for(|released| *released).await;
    }
}

#[derive(Clone, Copy)]
enum GateKind {
    Commit,
    Find,
}

/// A gate that holds calls until released and reports when the first held call arrives.
#[derive(Clone)]
pub struct Gate {
    kind: GateKind,
    held: Arc<HeldGate>,
    commit_gate: Arc<Mutex<Option<Arc<HeldGate>>>>,
    find_gate: Arc<Mutex<Option<Arc<HeldGate>>>>,
}

impl Gate {
    pub async fn entered(&self) {
        let mut rx = self.held.entered_rx.clone();
        let _ = rx.wait_for(|entered| *entered).await;
    }

    pub fn release(&self) {
        match self.kind {
            GateKind::Commit => {
                let mut guard = self.commit_gate.lock().unwrap_or_else(|e| e.into_inner());
                if guard.as_ref().is_some_and(|current| Arc::ptr_eq(current, &self.held)) {
                    *guard = None;
                }
            }
            GateKind::Find => {
                let mut guard = self.find_gate.lock().unwrap_or_else(|e| e.into_inner());
                if guard.as_ref().is_some_and(|current| Arc::ptr_eq(current, &self.held)) {
                    *guard = None;
                }
            }
        }
        let _ = self.held.release_tx.send(true);
    }
}

/// Memory storage with observable commits, held calls, and injected commit failures.
pub struct ControlledStorage {
    inner: MemoryStorage,
    /// Cloned batches admitted by Session (checked for admitted payload assertions).
    pub admitted_commits: Mutex<Vec<Vec<StorageWrite>>>,
    /// Detached batches for value assertions.
    pub commits: Mutex<Vec<Vec<StorageWrite>>>,
    pub mint_count: AtomicUsize,
    pub document_read_count: AtomicUsize,
    commit_gate: Arc<Mutex<Option<Arc<HeldGate>>>>,
    find_gate: Arc<Mutex<Option<Arc<HeldGate>>>>,
    commit_failure: Mutex<Option<pi_js::Error>>,
}

impl Default for ControlledStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlledStorage {
    pub fn new() -> Self {
        Self {
            inner: MemoryStorage::new(),
            admitted_commits: Mutex::new(Vec::new()),
            commits: Mutex::new(Vec::new()),
            mint_count: AtomicUsize::new(0),
            document_read_count: AtomicUsize::new(0),
            commit_gate: Arc::new(Mutex::new(None)),
            find_gate: Arc::new(Mutex::new(None)),
            commit_failure: Mutex::new(None),
        }
    }

    pub fn inner(&self) -> &MemoryStorage {
        &self.inner
    }

    pub fn hold_commits(&self) -> Gate {
        let held = HeldGate::new();
        *self.commit_gate.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&held));
        Gate {
            kind: GateKind::Commit,
            held,
            commit_gate: Arc::clone(&self.commit_gate),
            find_gate: Arc::clone(&self.find_gate),
        }
    }

    pub fn hold_find_document(&self) -> Gate {
        let held = HeldGate::new();
        *self.find_gate.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&held));
        Gate {
            kind: GateKind::Find,
            held,
            commit_gate: Arc::clone(&self.commit_gate),
            find_gate: Arc::clone(&self.find_gate),
        }
    }

    /// Simulate a crash during the held commit: it never reaches storage, and later commits proceed.
    pub fn crash(&self) {
        *self.commit_gate.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn fail_next_commit(&self, error: pi_js::Error) {
        *self.commit_failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
    }

    pub fn commits_snapshot(&self) -> Vec<Vec<StorageWrite>> {
        self.commits.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn last_commit(&self) -> Option<Vec<StorageWrite>> {
        self.commits.lock().unwrap_or_else(|e| e.into_inner()).last().cloned()
    }

    pub fn last_admitted_commit(&self) -> Option<Vec<StorageWrite>> {
        self.admitted_commits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last()
            .cloned()
    }

    pub fn commit_count(&self) -> usize {
        self.commits.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn mint_count(&self) -> usize {
        self.mint_count.load(Ordering::SeqCst)
    }

    pub fn document_read_count(&self) -> usize {
        self.document_read_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Storage for ControlledStorage {
    async fn commit(&self, writes: Vec<StorageWrite>, commit_context: Context) -> pi_js::Result<Seq> {
        {
            let cloned = writes.clone();
            self.admitted_commits
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(cloned.clone());
            self.commits.lock().unwrap_or_else(|e| e.into_inner()).push(cloned);
        }
        let held = self.commit_gate.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(held) = held {
            held.arrive_and_wait().await;
        }
        let failure = self.commit_failure.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(failure) = failure {
            return Err(failure);
        }
        self.inner.commit(writes, commit_context).await
    }

    async fn mint_id(&self) -> pi_js::Result<i64> {
        self.mint_count.fetch_add(1, Ordering::SeqCst);
        self.inner.mint_id().await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        call_context: Context,
    ) -> pi_js::Result<Option<ConversationRecord>> {
        self.inner.conversation(id, call_context).await
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        call_context: Context,
    ) -> pi_js::Result<Page<ConversationRecord, Cursor>> {
        self.inner.scan_conversations(query, limit, cursor, call_context).await
    }

    async fn entry(&self, id: EntryId, call_context: Context) -> pi_js::Result<Option<EntryLookup>> {
        self.inner.entry(id, call_context).await
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        call_context: Context,
    ) -> pi_js::Result<Option<EntryLookup>> {
        self.inner
            .entry_in_conversation(conversation_id, id, call_context)
            .await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        call_context: Context,
    ) -> pi_js::Result<Option<EntryRecord>> {
        self.inner
            .find_latest_head_marker(conversation_id, at_or_before_entry_id, call_context)
            .await
    }

    async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        call_context: Context,
    ) -> pi_js::Result<Page<EntryRecord, Cursor>> {
        self.inner.scan_entries(query, limit, cursor, call_context).await
    }

    async fn task(&self, id: TaskId, call_context: Context) -> pi_js::Result<Option<ErasedTaskRecord>> {
        self.inner.task(id, call_context).await
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        call_context: Context,
    ) -> pi_js::Result<Page<ErasedTaskRecord, Cursor>> {
        self.inner.scan_tasks(query, limit, cursor, call_context).await
    }

    async fn submission(&self, id: SubmissionId, call_context: Context) -> pi_js::Result<Option<SubmissionRecord>> {
        self.inner.submission(id, call_context).await
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        call_context: Context,
    ) -> pi_js::Result<Page<SubmissionRecord, Cursor>> {
        self.inner.scan_submissions(query, limit, cursor, call_context).await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        call_context: Context,
    ) -> pi_js::Result<Option<SubmissionRecord>> {
        self.inner
            .submission_by_request(conversation_id, request_id, call_context)
            .await
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        call_context: Context,
    ) -> pi_js::Result<Option<DocumentRecord>> {
        let held = self.find_gate.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(held) = held {
            held.arrive_and_wait().await;
        }
        self.inner.find_document(address, at, call_context).await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        call_context: Context,
    ) -> pi_js::Result<Option<StoredDocument>> {
        self.document_read_count.fetch_add(1, Ordering::SeqCst);
        self.inner.document(id, at, call_context).await
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        call_context: Context,
    ) -> pi_js::Result<Page<DocumentRecord, Cursor>> {
        self.inner.scan_documents(query, limit, cursor, call_context).await
    }

    async fn close(&self, call_context: Context) -> pi_js::Result<()> {
        self.inner.close(call_context).await
    }
}

pub struct TestSession {
    pub storage: Arc<ControlledStorage>,
    pub session: Arc<SessionImpl>,
    pub facade: Session,
    pub publications: Arc<Mutex<Vec<CommitPublication>>>,
    pub commit_subscription: pi_js::Unsubscribe,
}

impl TestSession {
    pub fn publications_snapshot(&self) -> Vec<CommitPublication> {
        self.publications.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn last_publication(&self) -> Option<CommitPublication> {
        self.publications
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last()
            .cloned()
    }
}

/// Session kernel plus its controlled storage and every committed publication.
pub fn open_test_session() -> TestSession {
    let storage = Arc::new(ControlledStorage::new());
    let storage_trait: Arc<dyn Storage> = Arc::clone(&storage) as Arc<dyn Storage>;
    let session = SessionImpl::new(storage_trait);
    let facade = Session::from_impl(Arc::clone(&session));
    let publications = Arc::new(Mutex::new(Vec::new()));
    let pub_sink = Arc::clone(&publications);
    let listener: CommitListener = Arc::new(move |publication, _ctx| {
        pub_sink
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(publication.clone());
    });
    let commit_subscription = session.subscribe_commits(listener).expect("subscribe commits");
    TestSession {
        storage,
        session,
        facade,
        publications,
        commit_subscription,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DocumentChangeView {
    pub record: DocumentRecord,
    pub conversation_id: Option<ConversationId>,
    pub version: Option<i64>,
    pub value: Option<pi_durable::types::JsonObject>,
    pub ops: Vec<pi_chord::delta::Op>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DocumentCopyChangeView {
    pub record: DocumentRecord,
    pub conversation_id: ConversationId,
    pub source: pi_durable::types::DocumentCopySource,
}

pub fn document_changes(publication: &CommitPublication) -> Vec<DocumentChangeView> {
    publication
        .changes
        .iter()
        .filter_map(|change| match change {
            CommitChange::Document(DocumentCommitChange::Document {
                record,
                conversation_id,
                version,
                value,
                ops,
            }) => Some(DocumentChangeView {
                record: record.clone(),
                conversation_id: *conversation_id,
                version: *version,
                value: value.clone(),
                ops: ops.clone(),
            }),
            _ => None,
        })
        .collect()
}

pub fn document_copy_changes(publication: &CommitPublication) -> Vec<DocumentCopyChangeView> {
    publication
        .changes
        .iter()
        .filter_map(|change| match change {
            CommitChange::Document(DocumentCommitChange::DocumentCopy {
                record,
                conversation_id,
                source,
            }) => Some(DocumentCopyChangeView {
                record: record.clone(),
                conversation_id: *conversation_id,
                source: source.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Create one conversation and return its ID.
pub async fn create_conversation(session: &Arc<SessionImpl>) -> ConversationId {
    session
        .commit(
            |tx| async move {
                let record = tx.create_conversation(ConversationOwnership::Ownerless).await?;
                Ok(record.id)
            },
            context(),
        )
        .await
        .expect("create conversation")
}

/// Resolve after pending microtasks and one macrotask turn.
pub async fn flush() {
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    pi_js::time::sleep(Duration::from_millis(0)).await;
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
}
