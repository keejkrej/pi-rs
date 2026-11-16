//! Port of packages/durable/src/harness/task-graph.ts

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use indexmap::IndexMap;
use pi_chord::context::without_abort_signal;
use pi_chord::delta::{DeleteVerb, Op, OpDelete, OpSet, Seg, SetVerb};
use pi_chord::services::state::attach_replicated_state_source;
use pi_chord::{AttachedReplicatedState, Context, JsonValue, ReplicatedStateSource, ReplicatedStateSourceAttachment};
use pi_js::abort::AbortReason;

pub use crate::harness::types::{TaskGraph, TaskGraphNode, TaskGraphState, TaskGraphWatch};
use crate::harness::util::{closed_error, scan_all};
use crate::session::observation::{CommittedStateSource, CommittedWatch};
use crate::session::session::SessionImpl;
use crate::types::{
    CommitChange, CommitPublication, ConversationId, ConversationQuery, ErasedTaskRecord, Storage, TableCommitChange,
    TaskQuery, TaskState, TaskStatus,
};

const LIVE_STATUSES: [TaskStatus; 4] = [
    TaskStatus::Pending,
    TaskStatus::Running,
    TaskStatus::Waiting,
    TaskStatus::Completing,
];
const SCAN_PAGE_SIZE: usize = 256;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn abort_error(signal: &pi_js::abort::AbortSignal) -> pi_js::Error {
    match signal.reason() {
        Some(reason) if reason.value.is_none() => pi_js::Error::Abort(reason),
        _ => pi_js::Error::Abort(AbortReason::error("AbortError", "The operation was aborted")),
    }
}

enum GraphObserver {
    Source(Arc<CommittedStateSource<TaskGraph>>),
    Watch(Arc<CommittedWatch<TaskGraph>>),
}

impl GraphObserver {
    fn advance(&self, value: &TaskGraph, ops: &[Op], context: &Context) {
        match self {
            Self::Source(s) => s.advance(value.clone(), ops.to_vec(), context.clone()),
            Self::Watch(w) => w.advance(value.clone(), ops.to_vec(), context.clone()),
        }
    }

    fn close_session(&self) {
        match self {
            Self::Source(s) => s.close_session(),
            Self::Watch(w) => w.close_session(),
        }
    }
}

struct Mount {
    value: TaskGraph,
    observers: IndexMap<u64, GraphObserver>,
}

struct GraphState {
    next_observer_id: u64,
    mount: Option<Mount>,
    closed: bool,
}

struct ArcSource<T>(Arc<CommittedStateSource<T>>);

impl<T: Clone + Send + Sync + 'static> ReplicatedStateSource<T> for ArcSource<T> {
    fn attach(&self) -> pi_js::Result<Box<dyn ReplicatedStateSourceAttachment<T> + Send + Sync>> {
        self.0.attach()
    }
}

/// The Harness's task graph mount: built on the Session line by its first observer and dropped
/// with its last. It advances from the Session's commit publications, which are durable.
pub struct TaskGraphView {
    session: Arc<SessionImpl>,
    storage: Arc<dyn Storage>,
    state: Arc<Mutex<GraphState>>,
    _sub_commits: pi_js::Unsubscribe,
    _sub_close: pi_js::Unsubscribe,
}

impl TaskGraphView {
    pub fn new(session: Arc<SessionImpl>, storage: Arc<dyn Storage>) -> pi_js::Result<Arc<Self>> {
        let state = Arc::new(Mutex::new(GraphState {
            next_observer_id: 1,
            mount: None,
            closed: false,
        }));

        let state_for_commits = Arc::clone(&state);
        let sub_commits = session.subscribe_commits(Arc::new(move |publication, context| {
            let delivery = {
                let mut guard = lock(&state_for_commits);
                let Some(mount) = guard.mount.as_mut() else {
                    return;
                };
                advance(mount, publication)
            };
            if let Some((value, ops, observers)) = delivery {
                let frame_context = without_abort_signal(context.clone());
                for observer in observers {
                    observer.advance(&value, &ops, &frame_context);
                }
            }
        }))?;

        let state_for_close = Arc::clone(&state);
        let sub_close = session.subscribe_close(Arc::new(move || {
            let observers: Vec<GraphObserver> = {
                let mut guard = lock(&state_for_close);
                guard.closed = true;
                let list = guard
                    .mount
                    .as_ref()
                    .map(|m| {
                        m.observers
                            .values()
                            .map(|o| match o {
                                GraphObserver::Source(s) => GraphObserver::Source(Arc::clone(s)),
                                GraphObserver::Watch(w) => GraphObserver::Watch(Arc::clone(w)),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                guard.mount = None;
                list
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

    /// A disposable read-only Chord state of the graph.
    pub async fn state(&self, context: Context) -> pi_js::Result<AttachedReplicatedState<TaskGraph>> {
        let (source, detach) = self
            .attach(
                |value, release| {
                    let source = Arc::new(CommittedStateSource::new(value, |_| false, release));
                    let obs = GraphObserver::Source(Arc::clone(&source));
                    (obs, source)
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

    /// A serialized exact-frame watch of the graph; cancelling `context` stops it.
    pub async fn watch(&self, context: Context) -> pi_js::Result<TaskGraphWatch> {
        let (watch, _detach) = self
            .attach(
                |value, release| {
                    let watch = CommittedWatch::new_arc(
                        value,
                        |_| false,
                        |v| serde_json::to_value(v).unwrap_or(JsonValue::Null),
                        release,
                        None,
                    );
                    let obs = GraphObserver::Watch(Arc::clone(&watch));
                    (obs, watch)
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

    async fn attach<R, F>(&self, create: F, context: Context) -> pi_js::Result<(R, Box<dyn FnOnce() + Send + Sync>)>
    where
        F: FnOnce(TaskGraph, Box<dyn FnOnce() + Send + Sync>) -> (GraphObserver, R) + Send,
        R: Send,
    {
        let storage = Arc::clone(&self.storage);
        let state = Arc::clone(&self.state);
        self.session
            .read_on_line(move || async move {
                let (existing_value, observer_id) = {
                    let mut guard = lock(&state);
                    if guard.closed {
                        return Err(closed_error());
                    }
                    if let Some(signal) = context.abort_signal()
                        && signal.aborted()
                    {
                        return Err(abort_error(&signal));
                    }
                    let obs_id = guard.next_observer_id;
                    guard.next_observer_id += 1;
                    (guard.mount.as_ref().map(|m| m.value.clone()), obs_id)
                };

                let built = match existing_value {
                    Some(v) => v,
                    None => build_graph(storage.as_ref(), &context).await?,
                };

                let state_for_detach = Arc::clone(&state);
                let detach_fn: Box<dyn FnOnce() + Send + Sync> = Box::new(move || {
                    let mut guard = lock(&state_for_detach);
                    if let Some(mount) = guard.mount.as_mut() {
                        mount.observers.shift_remove(&observer_id);
                        if mount.observers.is_empty() {
                            guard.mount = None;
                        }
                    }
                });

                let state_for_return_detach = Arc::clone(&state);
                let return_detach: Box<dyn FnOnce() + Send + Sync> = Box::new(move || {
                    let mut guard = lock(&state_for_return_detach);
                    if let Some(mount) = guard.mount.as_mut() {
                        mount.observers.shift_remove(&observer_id);
                        if mount.observers.is_empty() {
                            guard.mount = None;
                        }
                    }
                });

                let (observer, result) = create(built.clone(), detach_fn);

                {
                    let mut guard = lock(&state);
                    if guard.closed {
                        return Err(closed_error());
                    }
                    if let Some(signal) = context.abort_signal()
                        && signal.aborted()
                    {
                        return Err(abort_error(&signal));
                    }
                    let mount = guard.mount.get_or_insert_with(|| Mount {
                        value: built,
                        observers: IndexMap::new(),
                    });
                    mount.observers.insert(observer_id, observer);
                }

                Ok((result, return_detach))
            })
            .await
    }
}

async fn build_graph(storage: &dyn Storage, context: &Context) -> pi_js::Result<TaskGraph> {
    let mut records: Vec<ErasedTaskRecord> = Vec::new();
    for status in LIVE_STATUSES {
        let batch = scan_all(|cursor| {
            let ctx = context.clone();
            async move {
                storage
                    .scan_tasks(
                        TaskQuery {
                            status: Some(status),
                            ..Default::default()
                        },
                        SCAN_PAGE_SIZE,
                        cursor,
                        ctx,
                    )
                    .await
            }
        })
        .await?;
        records.extend(batch);
    }
    records.sort_by_key(|record| record.id);

    let mut tasks = IndexMap::new();
    for record in records {
        let task_id = record.id;
        let owned = scan_all(|cursor| {
            let ctx = context.clone();
            async move {
                storage
                    .scan_conversations(
                        ConversationQuery {
                            owner_conversation_id: None,
                            owner_task_id: Some(task_id),
                        },
                        SCAN_PAGE_SIZE,
                        cursor,
                        ctx,
                    )
                    .await
            }
        })
        .await?;
        let mut conversations: Vec<ConversationId> = owned.into_iter().map(|c| c.id).collect();
        conversations.sort_unstable();
        tasks.insert(record.id.raw().to_string(), node_of(&record, conversations));
    }

    Ok(TaskGraph { tasks })
}

/// Derive the mount's operations from one publication, apply them, and return observers if non-empty.
fn advance(mount: &mut Mount, publication: &CommitPublication) -> Option<(TaskGraph, Vec<Op>, Vec<GraphObserver>)> {
    let mut ops: Vec<Op> = Vec::new();
    let mut changed: IndexMap<String, Option<TaskGraphNode>> = IndexMap::new();

    let node_of_key =
        |key: &str, changed: &IndexMap<String, Option<TaskGraphNode>>, mount: &Mount| -> Option<TaskGraphNode> {
            if let Some(entry) = changed.get(key) {
                entry.clone()
            } else {
                mount.value.tasks.get(key).cloned()
            }
        };

    for change in &publication.changes {
        let CommitChange::Table(TableCommitChange::Task(record)) = change else {
            continue;
        };
        let key = record.id.raw().to_string();
        let previous = node_of_key(&key, &changed, mount);
        if record.state.is_terminal() {
            if previous.is_none() {
                continue;
            }
            ops.push(Op::Delete(OpDelete(
                DeleteVerb::D,
                vec![Seg::String("tasks".to_owned()), Seg::String(key.clone())],
            )));
            changed.insert(key, None);
            continue;
        }
        let next = node_of(
            record,
            previous.as_ref().map(|n| n.conversations.clone()).unwrap_or_default(),
        );
        if previous.as_ref() == Some(&next) {
            continue;
        }
        if let Ok(next_val) = serde_json::to_value(&next) {
            ops.push(Op::Set(OpSet(
                SetVerb::S,
                vec![Seg::String("tasks".to_owned()), Seg::String(key.clone())],
                next_val,
            )));
            changed.insert(key, Some(next));
        }
    }

    // After the tasks, so a conversation created with its owner task in one commit finds the owner's node.
    // Change order within a publication is unspecified, so each owner's list is sorted again.
    let mut created: IndexMap<String, Vec<ConversationId>> = IndexMap::new();
    for change in &publication.changes {
        let CommitChange::Table(TableCommitChange::Conversation(conversation)) = change else {
            continue;
        };
        let Some(owner) = conversation.owner else {
            continue;
        };
        let key = owner.task_id.raw().to_string();
        if node_of_key(&key, &changed, mount).is_some() {
            created.entry(key).or_default().push(conversation.id);
        }
    }

    for (key, ids) in created {
        if let Some(mut owner_node) = node_of_key(&key, &changed, mount) {
            let mut conversations = owner_node.conversations.clone();
            conversations.extend(ids);
            conversations.sort_unstable();
            if let Ok(val) = serde_json::to_value(&conversations) {
                ops.push(Op::Set(OpSet(
                    SetVerb::S,
                    vec![
                        Seg::String("tasks".to_owned()),
                        Seg::String(key.clone()),
                        Seg::String("conversations".to_owned()),
                    ],
                    val,
                )));
                owner_node.conversations = conversations;
                changed.insert(key, Some(owner_node));
            }
        }
    }

    if ops.is_empty() {
        return None;
    }

    for (key, value) in changed {
        match value {
            Some(node) => {
                mount.value.tasks.insert(key, node);
            }
            None => {
                mount.value.tasks.shift_remove(&key);
            }
        }
    }

    let observers = mount
        .observers
        .values()
        .map(|o| match o {
            GraphObserver::Source(s) => GraphObserver::Source(Arc::clone(s)),
            GraphObserver::Watch(w) => GraphObserver::Watch(Arc::clone(w)),
        })
        .collect();

    Some((mount.value.clone(), ops, observers))
}

fn node_of(record: &ErasedTaskRecord, conversations: Vec<ConversationId>) -> TaskGraphNode {
    TaskGraphNode {
        id: record.id,
        kind: record.kind.clone(),
        conversation_id: record.conversation_id,
        owner: record.owner,
        background: record.background,
        abort_requested: record.abort_requested,
        state: state_of(record),
        conversations,
    }
}

fn state_of(record: &ErasedTaskRecord) -> TaskGraphState {
    match &record.state {
        TaskState::Pending { checkpoint } => TaskGraphState::Pending {
            phase: phase_of(checkpoint),
        },
        TaskState::Running { checkpoint } => TaskGraphState::Running {
            phase: phase_of(checkpoint),
        },
        TaskState::Waiting { checkpoint, on, policy } => TaskGraphState::Waiting {
            phase: phase_of(checkpoint),
            on: on.clone(),
            policy: *policy,
        },
        // Terminal records never reach here: they leave the graph.
        TaskState::Completing { outcome } | TaskState::Terminal { outcome } => TaskGraphState::Completing {
            outcome: outcome.status_str().to_owned(),
        },
    }
}

fn phase_of(checkpoint: &JsonValue) -> String {
    checkpoint
        .get("phase")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned()
}
