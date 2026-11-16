//! Port of packages/durable/src/session/session.ts

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_chord::context::{await_with_context, without_abort_signal};
use pi_chord::delta::{Op, OpReplace, ReplaceVerb, track};
use pi_chord::{Context, JsonValue, replicated_state_source};
use pi_js::abort::AbortReason;
use serde::de::DeserializeOwned;
use tokio::sync::{Mutex as AsyncMutex, watch};

use crate::documents::{
    ConversationDoc, ConversationDocFamily, ErasedDocDefinition, RewindableConversationDocFamilyToken,
    RewindableConversationDocToken, SessionDocFamilyToken, SessionDocToken, TaskDocFamilyToken, TaskDocToken,
    check_record_scope, check_record_version, json_object_to_value, materialize_document, resolve_address,
};
use crate::errors::is_storage_rejected;
use crate::harness::types::{DocumentObserverBackend, DocumentReaderBackend};
use crate::session::observation::{CommittedStateSource, CommittedWatch, ObservedDocumentValue, RETIREMENT_OPERATIONS};
use crate::session::transaction::{LoadedDocument, Transaction, TransactionHost, TransactionScope, Tx, clone_pi_error};
use crate::types::{
    CommitChange, CommitPublication, ConversationId, ConversationRecord, DocumentAddress, DocumentCommitChange,
    DocumentPoint, DocumentScope, DocumentState, DocumentWatch, EntryId, JsonObject, Seq, Storage, StorageWrite,
    TableCommitChange, TaskId,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub type CommitListener = Arc<dyn Fn(&CommitPublication, &Context) + Send + Sync>;
pub type CloseListener = Arc<dyn Fn() + Send + Sync>;

/// Optional subclassing hooks implemented by higher layers (such as `Harness`) without inheritance.
#[async_trait]
pub trait SessionHooks: Send + Sync {
    async fn conversation_created(&self, _tx: &Arc<Transaction>, _record: &ConversationRecord) -> pi_js::Result<()> {
        Ok(())
    }

    async fn before_close(&self) {}
}

struct NoopHooks;

#[async_trait]
impl SessionHooks for NoopHooks {}

struct SessionHost {
    session: std::sync::Weak<SessionImpl>,
    storage: Arc<dyn Storage>,
    hooks: Arc<dyn SessionHooks>,
}

#[async_trait]
impl TransactionHost for SessionHost {
    fn storage(&self) -> &Arc<dyn Storage> {
        &self.storage
    }

    fn cached(&self, address_id: &str) -> Option<Arc<LoadedDocument>> {
        self.session
            .upgrade()
            .and_then(|s| lock(&s.documents).get(address_id).cloned())
    }

    async fn load(
        &self,
        definition: &ErasedDocDefinition,
        address_id: &str,
        address: &DocumentAddress,
        context: &Context,
    ) -> pi_js::Result<Option<Arc<LoadedDocument>>> {
        let Some(session) = self.session.upgrade() else {
            return Err(pi_js::Error::msg("Session is closed"));
        };
        session.load_document(definition, address_id, address, context).await
    }

    fn install(&self, document: Arc<LoadedDocument>) {
        if let Some(session) = self.session.upgrade() {
            lock(&session.documents).insert(document.address_id.clone(), document);
        }
    }

    fn evict(&self, address_id: &str, record_id: crate::types::DocumentId) {
        if let Some(session) = self.session.upgrade() {
            let mut docs = lock(&session.documents);
            if docs.get(address_id).is_some_and(|d| d.record.id == record_id) {
                docs.shift_remove(address_id);
            }
        }
    }

    async fn conversation_created(&self, tx: &Arc<Transaction>, record: &ConversationRecord) -> pi_js::Result<()> {
        self.hooks.conversation_created(tx, record).await
    }
}

struct ListenersState {
    next_id: u64,
    commit_listeners: IndexMap<u64, CommitListener>,
    close_listeners: IndexMap<u64, CloseListener>,
}

/// Public clonable handle to an open durable [`SessionImpl`].
#[derive(Clone)]
pub struct Session {
    inner: Arc<SessionImpl>,
}

/// Open a Session kernel over one storage backend.
pub fn create_session(storage: Arc<dyn Storage>) -> Session {
    Session::new(storage)
}

impl Session {
    pub fn new(storage: Arc<dyn Storage>) -> Self {
        Self {
            inner: SessionImpl::new(storage),
        }
    }

    pub fn with_hooks(storage: Arc<dyn Storage>, hooks: Arc<dyn SessionHooks>) -> Self {
        Self {
            inner: SessionImpl::with_hooks(storage, hooks),
        }
    }

    pub fn from_impl(inner: Arc<SessionImpl>) -> Self {
        Self { inner }
    }

    pub fn impl_arc(&self) -> Arc<SessionImpl> {
        Arc::clone(&self.inner)
    }

    pub fn commit<T, F, Fut>(&self, change: F, context: Context) -> impl Future<Output = pi_js::Result<T>> + Send
    where
        F: FnOnce(Tx) -> Fut + Send,
        Fut: Future<Output = pi_js::Result<T>> + Send,
        T: Send,
    {
        self.inner.commit(change, context)
    }

    pub async fn close(&self, context: Context) -> pi_js::Result<()> {
        self.inner.close(context).await
    }

    pub fn subscribe_commits<F>(&self, listener: F) -> pi_js::Result<pi_js::Unsubscribe>
    where
        F: Fn(&CommitPublication, &Context) + Send + Sync + 'static,
    {
        self.inner.subscribe_commits(Arc::new(listener))
    }

    pub fn subscribe_close<F>(&self, listener: F) -> pi_js::Result<pi_js::Unsubscribe>
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.inner.subscribe_close(Arc::new(listener))
    }

    pub async fn snapshot_session<T: DeserializeOwned>(
        &self,
        token: &SessionDocToken<T>,
        context: Context,
    ) -> pi_js::Result<Option<T>> {
        self.inner
            .snapshot_erased_value(&token.definition, None, None, None, &context)
            .await
    }

    pub async fn snapshot_conversation<T: DeserializeOwned>(
        &self,
        token: &impl ConversationDoc<T>,
        conversation_id: ConversationId,
        context: Context,
    ) -> pi_js::Result<Option<T>> {
        self.inner
            .snapshot_erased_value(token.definition(), Some(conversation_id), None, None, &context)
            .await
    }

    pub async fn snapshot_task<T: DeserializeOwned>(
        &self,
        token: &TaskDocToken<T>,
        task_id: TaskId,
        context: Context,
    ) -> pi_js::Result<Option<T>> {
        self.inner
            .snapshot_erased_value(&token.definition, None, Some(task_id), None, &context)
            .await
    }

    pub async fn snapshot_session_family<T: DeserializeOwned, I>(
        &self,
        token: &SessionDocFamilyToken<T, I>,
        key: &str,
        context: Context,
    ) -> pi_js::Result<Option<T>> {
        self.inner
            .snapshot_erased_value(&token.definition, None, None, Some(key), &context)
            .await
    }

    pub async fn snapshot_conversation_family<T: DeserializeOwned, I>(
        &self,
        token: &impl ConversationDocFamily<T, I>,
        conversation_id: ConversationId,
        key: &str,
        context: Context,
    ) -> pi_js::Result<Option<T>> {
        self.inner
            .snapshot_erased_value(token.definition(), Some(conversation_id), None, Some(key), &context)
            .await
    }

    pub async fn snapshot_task_family<T: DeserializeOwned, I>(
        &self,
        token: &TaskDocFamilyToken<T, I>,
        task_id: TaskId,
        key: &str,
        context: Context,
    ) -> pi_js::Result<Option<T>> {
        self.inner
            .snapshot_erased_value(&token.definition, None, Some(task_id), Some(key), &context)
            .await
    }

    pub async fn snapshot_as_of<T: DeserializeOwned>(
        &self,
        token: &RewindableConversationDocToken<T>,
        conversation_id: ConversationId,
        at: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<T>> {
        self.inner
            .snapshot_as_of_typed(&token.definition, conversation_id, None, at, &context)
            .await
    }

    pub async fn snapshot_family_as_of<T: DeserializeOwned, I>(
        &self,
        token: &RewindableConversationDocFamilyToken<T, I>,
        conversation_id: ConversationId,
        key: &str,
        at: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<T>> {
        self.inner
            .snapshot_as_of_typed(&token.definition, conversation_id, Some(key), at, &context)
            .await
    }

    pub async fn document_state_session<T: DeserializeOwned>(
        &self,
        token: &SessionDocToken<T>,
        context: Context,
    ) -> pi_js::Result<Option<DocumentState<T>>> {
        self.inner
            .document_state_typed(&token.definition, None, None, None, &context)
            .await
    }

    pub async fn document_state_conversation<T: DeserializeOwned>(
        &self,
        token: &impl ConversationDoc<T>,
        conversation_id: ConversationId,
        context: Context,
    ) -> pi_js::Result<Option<DocumentState<T>>> {
        self.inner
            .document_state_typed(token.definition(), Some(conversation_id), None, None, &context)
            .await
    }

    pub async fn document_state_task<T: DeserializeOwned>(
        &self,
        token: &TaskDocToken<T>,
        task_id: TaskId,
        context: Context,
    ) -> pi_js::Result<Option<DocumentState<T>>> {
        self.inner
            .document_state_typed(&token.definition, None, Some(task_id), None, &context)
            .await
    }

    pub async fn document_state_session_family<T: DeserializeOwned, I>(
        &self,
        token: &SessionDocFamilyToken<T, I>,
        key: &str,
        context: Context,
    ) -> pi_js::Result<Option<DocumentState<T>>> {
        self.inner
            .document_state_typed(&token.definition, None, None, Some(key), &context)
            .await
    }

    pub async fn document_state_conversation_family<T: DeserializeOwned, I>(
        &self,
        token: &impl ConversationDocFamily<T, I>,
        conversation_id: ConversationId,
        key: &str,
        context: Context,
    ) -> pi_js::Result<Option<DocumentState<T>>> {
        self.inner
            .document_state_typed(token.definition(), Some(conversation_id), None, Some(key), &context)
            .await
    }

    pub async fn document_state_task_family<T: DeserializeOwned, I>(
        &self,
        token: &TaskDocFamilyToken<T, I>,
        task_id: TaskId,
        key: &str,
        context: Context,
    ) -> pi_js::Result<Option<DocumentState<T>>> {
        self.inner
            .document_state_typed(&token.definition, None, Some(task_id), Some(key), &context)
            .await
    }

    pub async fn watch_doc_session<T: DeserializeOwned + Send + Sync + 'static>(
        &self,
        token: &SessionDocToken<T>,
        context: Context,
    ) -> pi_js::Result<Option<DocumentWatch<T>>> {
        let raw = self
            .inner
            .watch_erased(&token.definition, None, None, None, context)
            .await?;
        Ok(raw.map(crate::harness::types::wrap_typed_watch))
    }

    pub async fn watch_doc_conversation<T: DeserializeOwned + Send + Sync + 'static>(
        &self,
        token: &impl ConversationDoc<T>,
        conversation_id: ConversationId,
        context: Context,
    ) -> pi_js::Result<Option<DocumentWatch<T>>> {
        let raw = self
            .inner
            .watch_erased(token.definition(), Some(conversation_id), None, None, context)
            .await?;
        Ok(raw.map(crate::harness::types::wrap_typed_watch))
    }

    pub async fn watch_doc_task<T: DeserializeOwned + Send + Sync + 'static>(
        &self,
        token: &TaskDocToken<T>,
        task_id: TaskId,
        context: Context,
    ) -> pi_js::Result<Option<DocumentWatch<T>>> {
        let raw = self
            .inner
            .watch_erased(&token.definition, None, Some(task_id), None, context)
            .await?;
        Ok(raw.map(crate::harness::types::wrap_typed_watch))
    }

    pub async fn watch_doc_session_family<T: DeserializeOwned + Send + Sync + 'static, I>(
        &self,
        token: &SessionDocFamilyToken<T, I>,
        key: &str,
        context: Context,
    ) -> pi_js::Result<Option<DocumentWatch<T>>> {
        let raw = self
            .inner
            .watch_erased(&token.definition, None, None, Some(key), context)
            .await?;
        Ok(raw.map(crate::harness::types::wrap_typed_watch))
    }

    pub async fn watch_doc_conversation_family<T: DeserializeOwned + Send + Sync + 'static, I>(
        &self,
        token: &impl ConversationDocFamily<T, I>,
        conversation_id: ConversationId,
        key: &str,
        context: Context,
    ) -> pi_js::Result<Option<DocumentWatch<T>>> {
        let raw = self
            .inner
            .watch_erased(token.definition(), Some(conversation_id), None, Some(key), context)
            .await?;
        Ok(raw.map(crate::harness::types::wrap_typed_watch))
    }

    pub async fn watch_doc_task_family<T: DeserializeOwned + Send + Sync + 'static, I>(
        &self,
        token: &TaskDocFamilyToken<T, I>,
        task_id: TaskId,
        key: &str,
        context: Context,
    ) -> pi_js::Result<Option<DocumentWatch<T>>> {
        let raw = self
            .inner
            .watch_erased(&token.definition, None, Some(task_id), Some(key), context)
            .await?;
        Ok(raw.map(crate::harness::types::wrap_typed_watch))
    }

    pub async fn unload_documents(&self) -> pi_js::Result<()> {
        self.inner.unload_documents().await
    }
}

/// Session kernel: one mutation line, the loaded document tracker cache, and committed publication.
pub struct SessionImpl {
    storage: Arc<dyn Storage>,
    hooks: Arc<dyn SessionHooks>,
    documents: Mutex<IndexMap<String, Arc<LoadedDocument>>>,
    listeners: Mutex<ListenersState>,
    line_lock: AsyncMutex<()>,
    closing_started: AtomicBool,
    storage_closed: AtomicBool,
    close_result: Mutex<Option<watch::Receiver<Option<Result<(), Arc<pi_js::Error>>>>>>,
    poison: Mutex<Option<Arc<pi_js::Error>>>,
    host: Arc<SessionHost>,
}

impl SessionImpl {
    pub fn new(storage: Arc<dyn Storage>) -> Arc<Self> {
        Self::with_hooks(storage, Arc::new(NoopHooks))
    }

    pub fn with_hooks(storage: Arc<dyn Storage>, hooks: Arc<dyn SessionHooks>) -> Arc<Self> {
        Arc::new_cyclic(|weak| {
            let host = Arc::new(SessionHost {
                session: weak.clone(),
                storage: Arc::clone(&storage),
                hooks: Arc::clone(&hooks),
            });
            Self {
                storage,
                hooks,
                documents: Mutex::new(IndexMap::new()),
                listeners: Mutex::new(ListenersState {
                    next_id: 1,
                    commit_listeners: IndexMap::new(),
                    close_listeners: IndexMap::new(),
                }),
                line_lock: AsyncMutex::new(()),
                closing_started: AtomicBool::new(false),
                storage_closed: AtomicBool::new(false),
                close_result: Mutex::new(None),
                poison: Mutex::new(None),
                host,
            }
        })
    }

    pub fn commit<T, F, Fut>(
        self: &Arc<Self>,
        change: F,
        context: Context,
    ) -> impl Future<Output = pi_js::Result<T>> + Send
    where
        F: FnOnce(Tx) -> Fut + Send,
        Fut: Future<Output = pi_js::Result<T>> + Send,
        T: Send,
    {
        self.commit_with(move |tx| change(tx.as_tx()), context, TransactionScope::default())
    }

    /// Internal commit exposing the concrete transaction and its internal operations.
    pub fn commit_with<T, F, Fut>(
        self: &Arc<Self>,
        change: F,
        context: Context,
        scope: TransactionScope,
    ) -> impl Future<Output = pi_js::Result<T>> + Send
    where
        F: FnOnce(Arc<Transaction>) -> Fut + Send,
        Fut: Future<Output = pi_js::Result<T>> + Send,
        T: Send,
    {
        let usable = self.assert_usable();
        let this = Arc::clone(self);
        async move {
            usable?;
            let _permit = this.line_lock.lock().await;
            if this.storage_closed.load(Ordering::Acquire) {
                return Err(pi_js::Error::msg("Session is closed"));
            }
            this.run_commit(change, context, scope).await
        }
    }

    /// Internal: run a read-only job on the mutation line so multi-read derivations observe one committed state.
    pub fn read_on_line<T, F, Fut>(self: &Arc<Self>, job: F) -> impl Future<Output = pi_js::Result<T>> + Send
    where
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = pi_js::Result<T>> + Send,
        T: Send,
    {
        let usable = self.assert_usable();
        let this = Arc::clone(self);
        async move {
            usable?;
            let _permit = this.line_lock.lock().await;
            if this.storage_closed.load(Ordering::Acquire) {
                return Err(pi_js::Error::msg("Session is closed"));
            }
            this.assert_healthy()?;
            job().await
        }
    }

    /// Internal: a conversation document's current incarnation and value, for a job already running on the line.
    pub async fn conversation_document_on_line(
        &self,
        definition: &ErasedDocDefinition,
        conversation_id: ConversationId,
        context: &Context,
    ) -> pi_js::Result<Option<(DocumentRecord, i64, JsonObject)>> {
        let resolved = resolve_address(definition, Some(conversation_id), None, None)?;
        let Some(loaded) = self
            .load_document(definition, &resolved.id, &resolved.address, context)
            .await?
        else {
            return Ok(None);
        };
        check_record_scope(definition, &loaded.record)?;
        check_record_version(
            definition,
            loaded.record.id,
            &loaded.record.kind,
            loaded.stored_version(),
        )?;
        Ok(Some((
            loaded.record.clone(),
            loaded.value_version,
            loaded.tracker.value(),
        )))
    }

    async fn snapshot_erased_value<T: DeserializeOwned>(
        self: &Arc<Self>,
        definition: &ErasedDocDefinition,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<&str>,
        context: &Context,
    ) -> pi_js::Result<Option<T>> {
        let Some(obj) = self
            .snapshot_erased(
                &Arc::new(definition.clone()),
                conversation_id,
                task_id,
                key,
                context.clone(),
            )
            .await?
        else {
            return Ok(None);
        };
        json_object_to_value(&obj).map(Some)
    }

    async fn snapshot_as_of_typed<T: DeserializeOwned>(
        self: &Arc<Self>,
        definition: &ErasedDocDefinition,
        conversation_id: ConversationId,
        key: Option<&str>,
        at: EntryId,
        context: &Context,
    ) -> pi_js::Result<Option<T>> {
        let Some(obj) = self
            .snapshot_as_of_erased(&Arc::new(definition.clone()), conversation_id, key, at, context.clone())
            .await?
        else {
            return Ok(None);
        };
        json_object_to_value(&obj).map(Some)
    }

    async fn document_state_typed<T>(
        self: &Arc<Self>,
        definition: &ErasedDocDefinition,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<&str>,
        context: &Context,
    ) -> pi_js::Result<Option<DocumentState<T>>> {
        self.assert_usable()?;
        let resolved = resolve_address(definition, conversation_id, task_id, key.map(str::to_owned))?;
        let def_clone = definition.clone();
        let ctx_clone = context.clone();
        let this = Arc::clone(self);
        let source_opt = {
            let _permit = this.line_lock.lock().await;
            if this.storage_closed.load(Ordering::Acquire) {
                return Err(pi_js::Error::msg("Session is closed"));
            }
            this.assert_healthy()?;
            let Some(loaded) = this
                .load_document(&def_clone, &resolved.id, &resolved.address, &ctx_clone)
                .await?
            else {
                return Ok(None);
            };
            let detach_cell: Arc<Mutex<Option<Box<dyn FnOnce() + Send + Sync>>>> = Arc::new(Mutex::new(None));
            let detach_cell_cb = Arc::clone(&detach_cell);
            let (source, detach) = this.attach_document(
                &def_clone,
                &loaded,
                false,
                move |initial| {
                    CommittedStateSource::for_json_value(JsonValue::Object(initial), move || {
                        if let Some(d) = lock(&detach_cell_cb).take() {
                            d();
                        }
                    })
                },
                |src, val, ops, frame_ctx| {
                    let json_val = match val {
                        None => JsonValue::Null,
                        Some(obj) => JsonValue::Object(obj),
                    };
                    src.advance(json_val, ops, frame_ctx);
                },
                |src| src.close_session(),
            )?;
            *lock(&detach_cell) = Some(detach);
            Some(source)
        };
        let Some(source) = source_opt else {
            return Ok(None);
        };
        let attached = match replicated_state_source(source.clone(), None) {
            Ok(state) => state,
            Err(err) => {
                source.close_session();
                return Err(err);
            }
        };
        Ok(Some(DocumentState::from_raw(attached)))
    }

    pub async fn close(self: &Arc<Self>, context: Context) -> pi_js::Result<()> {
        let rx = {
            let mut slot = lock(&self.close_result);
            if let Some(rx) = slot.as_ref() {
                rx.clone()
            } else {
                self.closing_started.store(true, Ordering::Release);
                let (tx, rx) = watch::channel(None);
                *slot = Some(rx.clone());
                drop(slot);

                let cleanup = without_abort_signal(context.clone());
                let this = Arc::clone(self);
                let run_close = async move {
                    this.hooks.before_close().await;
                    let _permit = this.line_lock.lock().await;
                    this.storage_closed.store(true, Ordering::Release);
                    {
                        lock(&this.listeners).commit_listeners.clear();
                    }
                    lock(&this.documents).clear();
                    let res = this.storage.close(cleanup).await.map_err(Arc::new);
                    let _ = tx.send(Some(res));
                };
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(run_close);
                } else {
                    run_close.await;
                }

                let listeners: Vec<CloseListener> = {
                    let mut guard = lock(&self.listeners);
                    let list: Vec<CloseListener> = guard.close_listeners.values().cloned().collect();
                    guard.close_listeners.clear();
                    list
                };
                for listener in listeners {
                    listener();
                }
                rx
            }
        };

        await_with_context(
            async move {
                let mut rx = rx;
                loop {
                    if let Some(res) = rx.borrow_and_update().clone() {
                        return match res {
                            Ok(()) => Ok(()),
                            Err(err) => Err(clone_pi_error(&err)),
                        };
                    }
                    if rx.changed().await.is_err() {
                        return Ok(());
                    }
                }
            },
            context,
        )
        .await
    }

    pub fn subscribe_commits(self: &Arc<Self>, listener: CommitListener) -> pi_js::Result<pi_js::Unsubscribe> {
        self.assert_usable()?;
        let id = {
            let mut guard = lock(&self.listeners);
            let id = guard.next_id;
            guard.next_id += 1;
            guard.commit_listeners.insert(id, listener);
            id
        };
        let weak = Arc::downgrade(self);
        Ok(Arc::new(move || {
            if let Some(session) = weak.upgrade() {
                lock(&session.listeners).commit_listeners.shift_remove(&id);
            }
        }))
    }

    pub fn subscribe_close(self: &Arc<Self>, listener: CloseListener) -> pi_js::Result<pi_js::Unsubscribe> {
        self.assert_usable()?;
        let id = {
            let mut guard = lock(&self.listeners);
            let id = guard.next_id;
            guard.next_id += 1;
            guard.close_listeners.insert(id, listener);
            id
        };
        let weak = Arc::downgrade(self);
        Ok(Arc::new(move || {
            if let Some(session) = weak.upgrade() {
                lock(&session.listeners).close_listeners.shift_remove(&id);
            }
        }))
    }

    pub async fn unload_documents(self: &Arc<Self>) -> pi_js::Result<()> {
        let _permit = self.line_lock.lock().await;
        lock(&self.documents).clear();
        Ok(())
    }

    async fn run_commit<T, F, Fut>(
        self: &Arc<Self>,
        change: F,
        context: Context,
        scope: TransactionScope,
    ) -> pi_js::Result<T>
    where
        F: FnOnce(Arc<Transaction>) -> Fut + Send,
        Fut: Future<Output = pi_js::Result<T>> + Send,
    {
        self.assert_healthy()?;
        if let Some(signal) = context.abort_signal() {
            signal.throw_if_aborted()?;
        }
        let tx = Transaction::new(
            Arc::clone(&self.host) as Arc<dyn TransactionHost>,
            context.clone(),
            scope,
        );
        let result = match change(Arc::clone(&tx)).await {
            Ok(res) => res,
            Err(err) => {
                tx.settle_failure().await;
                return Err(err);
            }
        };
        let writes = tx.settle_success().await?;
        if writes.is_empty() {
            tx.discard();
            return Ok(result);
        }
        let seq = match self
            .storage
            .commit(writes.clone(), without_abort_signal(context.clone()))
            .await
        {
            Ok(seq) => seq,
            Err(err) => {
                tx.discard();
                if !is_storage_rejected(&err) {
                    *lock(&self.poison) = Some(Arc::new(clone_pi_error(&err)));
                }
                return Err(err);
            }
        };
        let documents = match tx.adopt(seq) {
            Ok(docs) => docs,
            Err(err) => {
                *lock(&self.poison) = Some(Arc::new(clone_pi_error(&err)));
                return Err(err);
            }
        };
        self.publish(seq, writes, documents, &context);
        Ok(result)
    }

    fn publish(&self, seq: Seq, writes: Vec<StorageWrite>, documents: Vec<DocumentCommitChange>, context: &Context) {
        let listeners: Vec<CommitListener> = {
            let guard = lock(&self.listeners);
            if guard.commit_listeners.is_empty() {
                return;
            }
            guard.commit_listeners.values().cloned().collect()
        };
        let mut changes = Vec::new();
        for write in writes {
            match write {
                StorageWrite::Conversation { value } => {
                    changes.push(CommitChange::Table(TableCommitChange::Conversation(value)));
                }
                StorageWrite::Entry { value } => {
                    changes.push(CommitChange::Table(TableCommitChange::Entry(value)));
                }
                StorageWrite::Task { value } => {
                    changes.push(CommitChange::Table(TableCommitChange::Task(value)));
                }
                StorageWrite::Submission { value } => {
                    changes.push(CommitChange::Table(TableCommitChange::Submission(value)));
                }
                StorageWrite::DocumentCreate { .. }
                | StorageWrite::DocumentCopy { .. }
                | StorageWrite::DocumentChange { .. }
                | StorageWrite::DocumentRetire { .. } => {}
            }
        }
        for doc in documents {
            changes.push(CommitChange::Document(doc));
        }
        let publication = CommitPublication { seq, changes };
        for listener in listeners {
            listener(&publication, context);
        }
    }

    #[allow(clippy::type_complexity)]
    fn attach_document<O: Send + Sync + 'static>(
        self: &Arc<Self>,
        definition: &ErasedDocDefinition,
        loaded: &Arc<LoadedDocument>,
        keep_caller_signal: bool,
        create: impl FnOnce(JsonObject) -> Arc<O>,
        advance: fn(&Arc<O>, ObservedDocumentValue, Vec<Op>, Context),
        close_observer: fn(&Arc<O>),
    ) -> pi_js::Result<(Arc<O>, Box<dyn FnOnce() + Send + Sync>)> {
        check_record_scope(definition, &loaded.record)?;
        check_record_version(
            definition,
            loaded.record.id,
            &loaded.record.kind,
            loaded.stored_version(),
        )?;
        let unsub_commit: Arc<Mutex<Option<pi_js::Unsubscribe>>> = Arc::new(Mutex::new(None));
        let unsub_close: Arc<Mutex<Option<pi_js::Unsubscribe>>> = Arc::new(Mutex::new(None));
        let unsub_commit_detach = Arc::clone(&unsub_commit);
        let unsub_close_detach = Arc::clone(&unsub_close);
        let detach: Box<dyn FnOnce() + Send + Sync> = Box::new(move || {
            if let Some(u) = lock(&unsub_commit_detach).take() {
                u();
            }
            if let Some(u) = lock(&unsub_close_detach).take() {
                u();
            }
        });
        let observer = create(loaded.tracker.value());
        let observed_version = Arc::new(Mutex::new(loaded.value_version));
        let record_id = loaded.record.id;

        let observer_commit = Arc::clone(&observer);
        let uc = self.subscribe_commits(Arc::new(move |publication, ctx| {
            for change in &publication.changes {
                let CommitChange::Document(doc_change) = change else {
                    continue;
                };
                let doc_id = match doc_change {
                    DocumentCommitChange::Document { record, .. }
                    | DocumentCommitChange::DocumentCopy { record, .. } => record.id,
                };
                if doc_id != record_id {
                    continue;
                }
                let DocumentCommitChange::Document {
                    version, value, ops, ..
                } = doc_change
                else {
                    continue;
                };
                let frame_context = if keep_caller_signal {
                    ctx.clone()
                } else {
                    without_abort_signal(ctx.clone())
                };
                let ops = observed_operations(&observed_version, *version, value.as_ref(), ops);
                if ops.is_empty() {
                    continue;
                }
                advance(&observer_commit, value.clone(), ops, frame_context);
            }
        }))?;
        *lock(&unsub_commit) = Some(uc);

        let observer_close = Arc::clone(&observer);
        let ucl = self.subscribe_close(Arc::new(move || {
            close_observer(&observer_close);
        }))?;
        *lock(&unsub_close) = Some(ucl);

        Ok((observer, detach))
    }

    async fn load_document(
        &self,
        definition: &ErasedDocDefinition,
        address_id: &str,
        address: &DocumentAddress,
        context: &Context,
    ) -> pi_js::Result<Option<Arc<LoadedDocument>>> {
        if let Some(cached) = lock(&self.documents).get(address_id).cloned() {
            if cached.value_version == definition.version {
                return Ok(Some(cached));
            }
            lock(&self.documents).shift_remove(address_id);
        }
        let Some(record) = self
            .storage
            .find_document(address.clone(), DocumentPoint::Current, context.clone())
            .await?
        else {
            return Ok(None);
        };
        let Some(stored) = self
            .storage
            .document(record.id, DocumentPoint::Current, context.clone())
            .await?
        else {
            return Err(pi_js::Error::msg(format!(
                "Current document {} ({}) cannot be read",
                record.id, record.kind
            )));
        };
        let value = materialize_document(definition, &stored)?;
        let loaded = Arc::new(LoadedDocument::new(
            address_id.to_owned(),
            stored.record,
            stored.version,
            definition.version,
            stored.deltas_since_base,
            track(value),
        ));
        lock(&self.documents).insert(address_id.to_owned(), Arc::clone(&loaded));
        Ok(Some(loaded))
    }

    fn assert_usable(&self) -> pi_js::Result<()> {
        if self.closing_started.load(Ordering::Acquire) {
            return Err(pi_js::Error::msg("Session is closed"));
        }
        self.assert_healthy()
    }

    fn assert_healthy(&self) -> pi_js::Result<()> {
        if let Some(poison) = lock(&self.poison).as_ref() {
            return Err(pi_js::Error::js_with_cause(
                "Error",
                "Session is poisoned by a failed commit after storage admission; reopen it",
                clone_pi_error(poison),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl DocumentReaderBackend for SessionImpl {
    async fn snapshot_erased(
        &self,
        def: &Arc<ErasedDocDefinition>,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<&str>,
        context: Context,
    ) -> pi_js::Result<Option<JsonObject>> {
        self.assert_usable()?;
        let resolved = resolve_address(def, conversation_id, task_id, key.map(str::to_owned))?;
        let cached = lock(&self.documents).get(&resolved.id).cloned();
        let loaded = match cached {
            Some(c) if c.value_version == def.version => Some(c),
            _ => {
                let _permit = self.line_lock.lock().await;
                if self.storage_closed.load(Ordering::Acquire) {
                    return Err(pi_js::Error::msg("Session is closed"));
                }
                self.assert_healthy()?;
                self.load_document(def, &resolved.id, &resolved.address, &context)
                    .await?
            }
        };
        let Some(loaded) = loaded else {
            return Ok(None);
        };
        check_record_scope(def, &loaded.record)?;
        check_record_version(def, loaded.record.id, &loaded.record.kind, loaded.stored_version())?;
        Ok(Some(loaded.tracker.value()))
    }

    async fn snapshot_as_of_erased(
        &self,
        def: &Arc<ErasedDocDefinition>,
        conversation_id: ConversationId,
        key: Option<&str>,
        at: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<JsonObject>> {
        self.assert_usable()?;
        let resolved = resolve_address(def, Some(conversation_id), None, key.map(str::to_owned))?;
        let DocumentScope::Conversation { conversation_id } = resolved.address.scope else {
            return Err(pi_js::Error::js(
                "TypeError",
                "Session.snapshotAsOf() requires a conversation document",
            ));
        };
        let _permit = self.line_lock.lock().await;
        if self.storage_closed.load(Ordering::Acquire) {
            return Err(pi_js::Error::msg("Session is closed"));
        }
        self.assert_healthy()?;
        let Some(stored_entry) = self
            .storage
            .entry_in_conversation(conversation_id, at, context.clone())
            .await?
        else {
            return Err(pi_js::Error::msg(format!(
                "Entry {at} is not visible from conversation {conversation_id}"
            )));
        };
        let address = DocumentAddress {
            kind: resolved.address.kind,
            scope: DocumentScope::Conversation {
                conversation_id: stored_entry.entry.conversation_id,
            },
            key: resolved.address.key,
        };
        let Some(record) = self
            .storage
            .find_document(address, DocumentPoint::At(stored_entry.commit_seq), context.clone())
            .await?
        else {
            return Ok(None);
        };
        let Some(stored) = self
            .storage
            .document(record.id, DocumentPoint::At(stored_entry.commit_seq), context)
            .await?
        else {
            return Err(pi_js::Error::msg(format!(
                "Historical document {} ({}) cannot be read",
                record.id, record.kind
            )));
        };
        materialize_document(def, &stored).map(Some)
    }
}

#[async_trait]
impl DocumentObserverBackend for SessionImpl {
    async fn watch_erased(
        &self,
        def: &Arc<ErasedDocDefinition>,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<&str>,
        context: Context,
    ) -> pi_js::Result<Option<DocumentWatch<JsonObject>>> {
        // NOTE: requires Arc<SessionImpl>; callers should go through `Session` or `Arc<SessionImpl>::watch_erased`.
        // Fall back to returning Err if invoked without Arc? Wait: Session implements DocumentObserverBackend via Arc<SessionImpl>!
        let _ = (def, conversation_id, task_id, key, context);
        Err(pi_js::Error::msg(
            "Use Session or Arc<SessionImpl>::watch_doc_erased to attach a watch",
        ))
    }
}

impl SessionImpl {
    pub async fn watch_doc_erased(
        self: &Arc<Self>,
        def: &Arc<ErasedDocDefinition>,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<&str>,
        context: Context,
    ) -> pi_js::Result<Option<DocumentWatch<JsonObject>>> {
        self.assert_usable()?;
        let resolved = resolve_address(def, conversation_id, task_id, key.map(str::to_owned))?;
        let signal = context.abort_signal();
        let cancelled = Arc::new(AtomicBool::new(signal.as_ref().is_some_and(|s| s.aborted())));
        let _guard = signal.as_ref().map(|sig| {
            let flag = Arc::clone(&cancelled);
            sig.on_abort(move |_| flag.store(true, Ordering::Release))
        });

        let watch_opt = {
            let _permit = self.line_lock.lock().await;
            if self.storage_closed.load(Ordering::Acquire) {
                return Err(pi_js::Error::msg("Session is closed"));
            }
            self.assert_healthy()?;
            if cancelled.load(Ordering::Acquire) {
                return Err(cancellation_error(signal.as_ref()));
            }
            let loaded = self
                .load_document(def, &resolved.id, &resolved.address, &context)
                .await?;
            if cancelled.load(Ordering::Acquire) {
                return Err(cancellation_error(signal.as_ref()));
            }
            let Some(loaded) = loaded else {
                return Ok(None);
            };
            let detach_cell: Arc<Mutex<Option<Box<dyn FnOnce() + Send + Sync>>>> = Arc::new(Mutex::new(None));
            let detach_cell_cb = Arc::clone(&detach_cell);
            let (watch, detach) = self.attach_document(
                def,
                &loaded,
                true,
                move |initial| {
                    CommittedWatch::for_document(
                        Some(initial),
                        move || {
                            if let Some(d) = lock(&detach_cell_cb).take() {
                                d();
                            }
                        },
                        None,
                    )
                },
                |w, val, ops, frame_ctx| {
                    w.advance(val, ops, frame_ctx);
                },
                |w| w.close_session(),
            )?;
            *lock(&detach_cell) = Some(detach);
            Some(watch)
        };
        drop(_guard);

        let Some(watch) = watch_opt else {
            return Ok(None);
        };
        if cancelled.load(Ordering::Acquire) {
            watch.cancel();
            return Err(cancellation_error(signal.as_ref()));
        }
        if let Some(sig) = &signal {
            watch.observe_cancellation(sig)?;
        }
        Ok(Some(watch.into_handle()))
    }
}

#[async_trait]
impl DocumentReaderBackend for Session {
    async fn snapshot_erased(
        &self,
        def: &Arc<ErasedDocDefinition>,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<&str>,
        context: Context,
    ) -> pi_js::Result<Option<JsonObject>> {
        self.inner
            .snapshot_erased(def, conversation_id, task_id, key, context)
            .await
    }

    async fn snapshot_as_of_erased(
        &self,
        def: &Arc<ErasedDocDefinition>,
        conversation_id: ConversationId,
        key: Option<&str>,
        at: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<JsonObject>> {
        self.inner
            .snapshot_as_of_erased(def, conversation_id, key, at, context)
            .await
    }
}

#[async_trait]
impl DocumentObserverBackend for Session {
    async fn watch_erased(
        &self,
        def: &Arc<ErasedDocDefinition>,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<&str>,
        context: Context,
    ) -> pi_js::Result<Option<DocumentWatch<JsonObject>>> {
        self.inner
            .watch_doc_erased(def, conversation_id, task_id, key, context)
            .await
    }
}

fn observed_operations(
    observed_version: &Arc<Mutex<i64>>,
    change_version: Option<i64>,
    change_value: Option<&JsonObject>,
    change_ops: &[Op],
) -> Vec<Op> {
    let Some(value) = change_value else {
        return RETIREMENT_OPERATIONS.clone();
    };
    let Some(version) = change_version else {
        return change_ops.to_vec();
    };
    let mut guard = lock(observed_version);
    if version == *guard {
        change_ops.to_vec()
    } else {
        *guard = version;
        vec![Op::Replace(OpReplace(ReplaceVerb::R, JsonValue::Object(value.clone())))]
    }
}

fn cancellation_error(signal: Option<&pi_js::abort::AbortSignal>) -> pi_js::Error {
    match signal.and_then(|s| s.reason()) {
        Some(reason) => pi_js::Error::Abort(reason),
        None => pi_js::Error::Abort(AbortReason::error("AbortError", "The operation was aborted")),
    }
}
