//! Port of packages/durable/src/harness/view.ts

use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

use indexmap::{IndexMap, IndexSet};
use pi_chord::context::without_abort_signal;
use pi_chord::delta::{
    DeleteVerb, Op, OpAppend, OpDelete, OpMove, OpSet, OpSplice, OpTruncate, Path, Seg, SetVerb, SpliceVerb,
    apply_immutable,
};
use pi_chord::services::state::attach_replicated_state_source;
use pi_chord::{AttachedReplicatedState, Context, JsonValue, ReplicatedStateSource, ReplicatedStateSourceAttachment};
use pi_js::abort::AbortReason;

use crate::harness::agent::AGENT_DOC;
use crate::harness::context::{active_entries, capture_context_bounds};
use crate::harness::inbox::INBOX_DOC;
use crate::harness::live::LIVE_DOC;
pub use crate::harness::types::ConversationView;
use crate::harness::usage::USAGE_DOC;
use crate::harness::util::closed_error;
use crate::session::observation::{CommittedStateSource, CommittedWatch};
use crate::session::session::SessionImpl;
use crate::types::{
    CommitChange, CommitPublication, ConversationId, DocumentCommitChange, DocumentId, EntryRecord, JsonObject,
    Storage, TableCommitChange, WatchHandle,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn abort_error(signal: &pi_js::abort::AbortSignal) -> pi_js::Error {
    match signal.reason() {
        Some(reason) if reason.value.is_none() => pi_js::Error::Abort(reason),
        _ => pi_js::Error::Abort(AbortReason::error("AbortError", "The operation was aborted")),
    }
}

static MOUNTED_KINDS: LazyLock<IndexSet<&'static str>> = LazyLock::new(|| {
    IndexSet::from([
        AGENT_DOC.definition.kind.as_str(),
        LIVE_DOC.definition.kind.as_str(),
        INBOX_DOC.definition.kind.as_str(),
        USAGE_DOC.definition.kind.as_str(),
    ])
});

/// Receives each next revision of a mount and the Session's close.
pub trait ViewObserver: Send + Sync {
    fn advance(&self, _value: &ConversationView, _ops: &[Op], _context: &Context) {}

    /// Every publication, after the mount took it; `ops` are the mount's, possibly none.
    fn publication(
        &self,
        _before: &ConversationView,
        _after: &ConversationView,
        _ops: &[Op],
        _publication: &CommitPublication,
        _context: &Context,
    ) {
    }

    fn close_session(&self);
}

struct MountedDoc {
    id: DocumentId,
    version: i64,
}

struct Mount {
    value: ConversationView,
    docs: IndexMap<String, MountedDoc>,
    observers: IndexMap<u64, Arc<dyn ViewObserver>>,
}

struct ViewsState {
    next_observer_id: u64,
    mounts: IndexMap<ConversationId, Mount>,
    closed: bool,
}

struct ArcSource<T>(Arc<CommittedStateSource<T>>);

impl<T: Clone + Send + Sync + 'static> ReplicatedStateSource<T> for ArcSource<T> {
    fn attach(&self) -> pi_js::Result<Box<dyn ReplicatedStateSourceAttachment<T> + Send + Sync>> {
        self.0.attach()
    }
}

struct SourceObserver(Arc<CommittedStateSource<ConversationView>>);

impl ViewObserver for SourceObserver {
    fn advance(&self, value: &ConversationView, ops: &[Op], context: &Context) {
        self.0.advance(value.clone(), ops.to_vec(), context.clone());
    }

    fn close_session(&self) {
        self.0.close_session();
    }
}

struct WatchObserver(Arc<CommittedWatch<ConversationView>>);

impl ViewObserver for WatchObserver {
    fn advance(&self, value: &ConversationView, ops: &[Op], context: &Context) {
        self.0.advance(value.clone(), ops.to_vec(), context.clone());
    }

    fn close_session(&self) {
        self.0.close_session();
    }
}

/// The Harness's conversation view mounts: at most one per conversation, built on the Session line
/// by its first observer and dropped with its last. Each mount advances from the Session's commit
/// publications, which are durable.
pub struct ConversationViews {
    session: Arc<SessionImpl>,
    storage: Arc<dyn Storage>,
    state: Arc<Mutex<ViewsState>>,
    _sub_commits: pi_js::Unsubscribe,
    _sub_close: pi_js::Unsubscribe,
}

impl ConversationViews {
    pub fn new(session: Arc<SessionImpl>, storage: Arc<dyn Storage>) -> pi_js::Result<Arc<Self>> {
        let state = Arc::new(Mutex::new(ViewsState {
            next_observer_id: 1,
            mounts: IndexMap::new(),
            closed: false,
        }));

        let state_for_commits = Arc::clone(&state);
        let sub_commits = session.subscribe_commits(Arc::new(move |publication, context| {
            let observers = {
                let mut guard = lock(&state_for_commits);
                let mut all = Vec::new();
                for (&id, mount) in &mut guard.mounts {
                    let (before, after, ops, list) = advance(id, mount, publication, context);
                    all.push((before, after, ops, list));
                }
                all
            };
            let frame_context = without_abort_signal(context.clone());
            for (before, after, ops, list) in observers {
                for observer in &list {
                    if !ops.is_empty() {
                        observer.advance(&after, &ops, &frame_context);
                    }
                    observer.publication(&before, &after, &ops, publication, &frame_context);
                }
            }
        }))?;

        let state_for_close = Arc::clone(&state);
        let sub_close = session.subscribe_close(Arc::new(move || {
            let observers: Vec<Arc<dyn ViewObserver>> = {
                let mut guard = lock(&state_for_close);
                guard.closed = true;
                let mut all = Vec::new();
                for mount in guard.mounts.values() {
                    all.extend(mount.observers.values().cloned());
                }
                guard.mounts.clear();
                all
            };
            for observer in observers {
                observer.close_session();
            }
        }))?;

        Ok(Arc::new(Self {
            session,
            storage,
            state,
            _sub_commits: sub_commits,
            _sub_close: sub_close,
        }))
    }

    /// A disposable read-only Chord state of the view.
    pub async fn state(
        &self,
        id: ConversationId,
        context: Context,
    ) -> pi_js::Result<AttachedReplicatedState<ConversationView>> {
        let (source, detach) = self
            .attach(
                id,
                |value, release, _storage| async move {
                    let source = Arc::new(CommittedStateSource::new(value, |_| false, release));
                    let observer: Arc<dyn ViewObserver> = Arc::new(SourceObserver(Arc::clone(&source)));
                    Ok((observer, source))
                },
                context,
            )
            .await?;

        match attach_replicated_state_source(ArcSource(source), None) {
            Ok(impl_) => Ok(AttachedReplicatedState::from_impl(impl_)),
            Err(err) => {
                detach();
                Err(err)
            }
        }
    }

    /// A serialized exact-frame watch of the view; cancelling `context` stops it.
    pub async fn watch(&self, id: ConversationId, context: Context) -> pi_js::Result<WatchHandle<ConversationView>> {
        let (watch, _detach) = self
            .attach(
                id,
                |value, release, _storage| async move {
                    let watch = CommittedWatch::new_arc(
                        value,
                        |_| false,
                        |v| serde_json::to_value(v).unwrap_or(JsonValue::Null),
                        release,
                        None,
                    );
                    let observer: Arc<dyn ViewObserver> = Arc::new(WatchObserver(Arc::clone(&watch)));
                    Ok((observer, watch))
                },
                context.clone(),
            )
            .await?;

        if let Some(signal) = context.abort_signal() {
            if signal.aborted() {
                watch.cancel();
                return Err(abort_error(&signal));
            }
            watch.observe_cancellation(&signal)?;
        }
        Ok(watch.into_handle())
    }

    /// Register an observer created from the current revision, atomically on the Session line:
    /// it sees every later publication and nothing earlier. `create` may read committed Storage,
    /// still on the line. `release` drops it, and the mount with its last observer.
    pub async fn attach<R, F, Fut>(
        &self,
        id: ConversationId,
        create: F,
        context: Context,
    ) -> pi_js::Result<(R, Box<dyn FnOnce() + Send + Sync>)>
    where
        F: FnOnce(ConversationView, Box<dyn FnOnce() + Send + Sync>, Arc<dyn Storage>) -> Fut + Send,
        Fut: Future<Output = pi_js::Result<(Arc<dyn ViewObserver>, R)>> + Send,
        R: Send,
    {
        let session = Arc::clone(&self.session);
        let storage = Arc::clone(&self.storage);
        let state = Arc::clone(&self.state);
        let ctx_for_line = context;

        session
            .clone()
            .read_on_line(move || async move {
                let (existing_value, observer_id) = {
                    let mut guard = lock(&state);
                    if guard.closed {
                        return Err(closed_error());
                    }
                    if let Some(signal) = ctx_for_line.abort_signal()
                        && signal.aborted()
                    {
                        return Err(abort_error(&signal));
                    }
                    let obs_id = guard.next_observer_id;
                    guard.next_observer_id += 1;
                    let val = guard.mounts.get(&id).map(|m| m.value.clone());
                    (val, obs_id)
                };

                let built_mount = match existing_value {
                    Some(_) => None,
                    None => Some(build_mount(&session, &storage, id, &ctx_for_line).await?),
                };
                let initial_value = match (&existing_value, &built_mount) {
                    (Some(v), _) => v.clone(),
                    (None, Some(m)) => m.value.clone(),
                    (None, None) => return Err(closed_error()),
                };

                let state_for_detach = Arc::clone(&state);
                let detach_fn: Box<dyn FnOnce() + Send + Sync> = Box::new(move || {
                    let mut guard = lock(&state_for_detach);
                    if let Some(mount) = guard.mounts.get_mut(&id) {
                        mount.observers.shift_remove(&observer_id);
                        if mount.observers.is_empty() {
                            guard.mounts.shift_remove(&id);
                        }
                    }
                });

                let state_for_return_detach = Arc::clone(&state);
                let return_detach: Box<dyn FnOnce() + Send + Sync> = Box::new(move || {
                    let mut guard = lock(&state_for_return_detach);
                    if let Some(mount) = guard.mounts.get_mut(&id) {
                        mount.observers.shift_remove(&observer_id);
                        if mount.observers.is_empty() {
                            guard.mounts.shift_remove(&id);
                        }
                    }
                });

                let (observer, result) = create(initial_value, detach_fn, storage).await?;

                {
                    let mut guard = lock(&state);
                    if guard.closed {
                        return Err(closed_error());
                    }
                    if let Some(signal) = ctx_for_line.abort_signal()
                        && signal.aborted()
                    {
                        return Err(abort_error(&signal));
                    }
                    let mount = if let Some(existing) = guard.mounts.get_mut(&id) {
                        existing
                    } else if let Some(built) = built_mount {
                        guard.mounts.entry(id).or_insert(built)
                    } else {
                        return Err(closed_error());
                    };
                    mount.observers.insert(observer_id, observer);
                }

                Ok((result, return_detach))
            })
            .await
    }
}

async fn build_mount(
    session: &Arc<SessionImpl>,
    storage: &Arc<dyn Storage>,
    id: ConversationId,
    context: &Context,
) -> pi_js::Result<Mount> {
    let conversation = storage
        .conversation(id, context.clone())
        .await?
        .ok_or_else(|| pi_js::Error::msg(format!("Conversation {id} does not exist")))?;
    let bounds = capture_context_bounds(storage.as_ref(), id, context, None).await?;
    let entries = active_entries(storage.as_ref(), id, bounds, context).await?;

    let mut docs: IndexMap<String, JsonObject> = IndexMap::new();
    let mut incarnations: IndexMap<String, MountedDoc> = IndexMap::new();
    let tokens = [
        &AGENT_DOC.definition,
        &LIVE_DOC.definition,
        &INBOX_DOC.definition,
        &USAGE_DOC.definition,
    ];
    for def in tokens {
        let Some((record, version, value)) = session.conversation_document_on_line(def, id, context).await? else {
            continue;
        };
        docs.insert(def.kind.clone(), value);
        incarnations.insert(def.kind.clone(), MountedDoc { id: record.id, version });
    }

    Ok(Mount {
        value: ConversationView {
            conversation,
            entries,
            docs,
        },
        docs: incarnations,
        observers: IndexMap::new(),
    })
}

/// Derive the mount's operations from one publication, apply them, and return `(before, after, ops, observers)`.
fn advance(
    id: ConversationId,
    mount: &mut Mount,
    publication: &CommitPublication,
    _context: &Context,
) -> (ConversationView, ConversationView, Vec<Op>, Vec<Arc<dyn ViewObserver>>) {
    let before = mount.value.clone();
    let mut doc_ops: Vec<Op> = Vec::new();
    let mut entry_ops: Vec<Op> = Vec::new();
    let mut entries: Vec<EntryRecord> = mount.value.entries.clone();
    let mut docs = mount.value.docs.clone();

    for change in &publication.changes {
        match change {
            CommitChange::Table(TableCommitChange::Entry(entry)) if entry.conversation_id == id => {
                if let Ok(entry_value) = serde_json::to_value(entry) {
                    if let Some(target) = entry.head {
                        let kept = entries
                            .iter()
                            .position(|candidate| candidate.head.is_none() && candidate.id >= target)
                            .unwrap_or(entries.len());
                        entry_ops.push(Op::Splice(OpSplice(
                            SpliceVerb::P,
                            vec![Seg::String("entries".to_owned())],
                            0,
                            kept as i64,
                            vec![entry_value],
                        )));
                        let mut next_entries = Vec::with_capacity(1 + entries.len() - kept);
                        next_entries.push(entry.clone());
                        next_entries.extend_from_slice(&entries[kept..]);
                        entries = next_entries;
                    } else {
                        entry_ops.push(Op::Splice(OpSplice(
                            SpliceVerb::P,
                            vec![Seg::String("entries".to_owned())],
                            entries.len() as i64,
                            0,
                            vec![entry_value],
                        )));
                        entries.push(entry.clone());
                    }
                }
            }
            CommitChange::Document(DocumentCommitChange::Document {
                record,
                conversation_id: Some(cid),
                version,
                value,
                ops,
            }) if *cid == id => {
                let kind = &record.kind;
                if !MOUNTED_KINDS.contains(kind.as_str()) || record.key.is_some() {
                    continue;
                }
                let path = vec![Seg::String("docs".to_owned()), Seg::String(kind.clone())];
                let mounted = mount.docs.get(kind);
                match value {
                    None => {
                        if mounted.is_some_and(|m| m.id == record.id) {
                            mount.docs.shift_remove(kind);
                            doc_ops.push(Op::Delete(OpDelete(DeleteVerb::D, path)));
                            docs.shift_remove(kind);
                        }
                    }
                    Some(val) => {
                        if mounted.is_some_and(|m| m.id == record.id && Some(m.version) == *version) {
                            for op in ops {
                                doc_ops.push(prefixed(op, &path));
                            }
                            let updated = docs
                                .get(kind)
                                .cloned()
                                .and_then(|current| apply_immutable(Some(JsonValue::Object(current)), ops).ok())
                                .and_then(|v| match v {
                                    JsonValue::Object(map) => Some(map),
                                    _ => None,
                                })
                                .unwrap_or_else(|| val.clone());
                            docs.insert(kind.clone(), updated);
                        } else if let Some(ver) = *version {
                            mount.docs.insert(
                                kind.clone(),
                                MountedDoc {
                                    id: record.id,
                                    version: ver,
                                },
                            );
                            doc_ops.push(Op::Set(OpSet(SetVerb::S, path, JsonValue::Object(val.clone()))));
                            docs.insert(kind.clone(), val.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let mut ops = doc_ops;
    ops.extend(entry_ops);
    if !ops.is_empty() {
        mount.value = ConversationView {
            conversation: mount.value.conversation.clone(),
            entries,
            docs,
        };
    }
    let after = mount.value.clone();
    let observers = mount.observers.values().cloned().collect();
    (before, after, ops, observers)
}

/// `op` moved under `prefix`; a root replacement becomes a set of the prefix.
fn prefixed(op: &Op, prefix: &[Seg]) -> Op {
    let at = |path: &Path| -> Path {
        let mut out = Vec::with_capacity(prefix.len() + path.len());
        out.extend_from_slice(prefix);
        out.extend_from_slice(path);
        out
    };
    match op {
        Op::Replace(op_r) => Op::Set(OpSet(SetVerb::S, prefix.to_vec(), op_r.1.clone())),
        Op::Splice(op_p) => Op::Splice(OpSplice(op_p.0, at(&op_p.1), op_p.2, op_p.3, op_p.4.clone())),
        Op::Move(op_m) => Op::Move(OpMove(op_m.0, at(&op_m.1), op_m.2.clone())),
        Op::Set(op_s) => Op::Set(OpSet(op_s.0, at(&op_s.1), op_s.2.clone())),
        Op::Delete(op_d) => Op::Delete(OpDelete(op_d.0, at(&op_d.1))),
        Op::Append(op_a) => Op::Append(OpAppend(op_a.0, at(&op_a.1), op_a.2.clone())),
        Op::Truncate(op_t) => Op::Truncate(OpTruncate(op_t.0, at(&op_t.1), op_t.2)),
    }
}
