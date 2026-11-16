//! Port of packages/durable/test/harness-task-graph.test.ts

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_chord::Context;
use pi_chord::context::with_abort_signal;
use pi_chord::delta::{DeleteVerb, Op, OpDelete, OpSet, Seg, SetVerb, apply_immutable};
use pi_chord::types::ReplicatedStateDeliveryKind;
use pi_durable::AbortTaskResult;
use pi_durable::harness::types::{TaskGraph, TaskGraphState, TaskInspectionState};
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::tasks::{Task, TaskDefinition, define_task};
use pi_durable::types::{
    ConversationId, ConversationOwnership, ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentId,
    DocumentPoint, DocumentQuery, DocumentRecord, EntryDraft, EntryId, EntryLookup, EntryQuery, EntryRecord,
    ErasedTaskRecord, JoinPolicy, JsonValue, NextTaskState, Page, Seq, Storage, StorageWrite, StoredDocument,
    SubmissionQuery, SubmissionRecord, TaskId, TaskOptions, TaskOutcome, TaskOwnership, TaskQuery, TaskStatus,
};
use pi_js::abort::AbortController;
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use support::session_support::{ControlledStorage, context, flush};
use support::task_support::{OpenTasksOptions, aborted, completed, deferred, eventually, eventually_sync, open_tasks};

struct CountingScanStorage {
    inner: ControlledStorage,
    scan_tasks_calls: AtomicUsize,
}

impl CountingScanStorage {
    fn new() -> Self {
        Self {
            inner: ControlledStorage::new(),
            scan_tasks_calls: AtomicUsize::new(0),
        }
    }

    fn scan_tasks_count(&self) -> usize {
        self.scan_tasks_calls.load(Ordering::Acquire)
    }
}

#[async_trait]
impl Storage for CountingScanStorage {
    async fn commit(&self, writes: Vec<StorageWrite>, context: Context) -> pi_js::Result<Seq> {
        self.inner.commit(writes, context).await
    }

    async fn mint_id(&self) -> pi_js::Result<i64> {
        self.inner.mint_id().await
    }

    async fn conversation(&self, id: ConversationId, context: Context) -> pi_js::Result<Option<ConversationRecord>> {
        self.inner.conversation(id, context).await
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<ConversationRecord, Cursor>> {
        self.inner.scan_conversations(query, limit, cursor, context).await
    }

    async fn entry(&self, id: EntryId, context: Context) -> pi_js::Result<Option<EntryLookup>> {
        self.inner.entry(id, context).await
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<EntryLookup>> {
        self.inner.entry_in_conversation(conversation_id, id, context).await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: Context,
    ) -> pi_js::Result<Option<EntryRecord>> {
        self.inner
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
        self.inner.scan_entries(query, limit, cursor, context).await
    }

    async fn task(&self, id: TaskId, context: Context) -> pi_js::Result<Option<ErasedTaskRecord>> {
        self.inner.task(id, context).await
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<ErasedTaskRecord, Cursor>> {
        self.scan_tasks_calls.fetch_add(1, Ordering::AcqRel);
        self.inner.scan_tasks(query, limit, cursor, context).await
    }

    async fn submission(
        &self,
        id: pi_durable::types::SubmissionId,
        context: Context,
    ) -> pi_js::Result<Option<SubmissionRecord>> {
        self.inner.submission(id, context).await
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<SubmissionRecord, Cursor>> {
        self.inner.scan_submissions(query, limit, cursor, context).await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: Context,
    ) -> pi_js::Result<Option<SubmissionRecord>> {
        self.inner
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<DocumentRecord>> {
        self.inner.find_document(address, at, context).await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<StoredDocument>> {
        self.inner.document(id, at, context).await
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<DocumentRecord, Cursor>> {
        self.inner.scan_documents(query, limit, cursor, context).await
    }

    async fn close(&self, context: Context) -> pi_js::Result<()> {
        self.inner.close(context).await
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ChildInput {
    late: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum ChildState {
    Work,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum ParentState {
    Spawn,
    Join { child: TaskId },
    Finish,
}

fn family(
    child_gate: support::task_support::Deferred<()>,
    late_gate: support::task_support::Deferred<()>,
) -> (Task<(), ParentState, (), ()>, Task<ChildInput, ChildState, (), ()>) {
    let mut child_phases = indexmap::IndexMap::new();
    child_phases.insert(
        "work".to_owned(),
        Arc::new(
            move |task: pi_durable::types::RunningTask<ChildInput, ChildState, ()>,
                  runtime: pi_durable::tasks::TaskRuntimeHandle<ChildInput, ChildState, (), ()>,
                  ctx: Context| {
                let child_gate = child_gate.clone();
                let late_gate = late_gate.clone();
                Box::pin(async move {
                    if task.input.late {
                        let _ = late_gate.wait().await;
                    } else {
                        let _ = child_gate.wait().await;
                    }
                    runtime
                        .commit(|_tx, _current| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                        .await
                }) as pi_js::BoxFuture<pi_js::Result<()>>
            },
        ) as _,
    );
    let child_task = define_task(TaskDefinition {
        name: "test.graph-child".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(ChildState::Work)),
        phases: child_phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });
    let child_for_parent = child_task.clone();
    let child_for_join = child_task.clone();

    let mut parent_phases = indexmap::IndexMap::new();
    parent_phases.insert(
        "spawn".to_owned(),
        Arc::new(
            move |task: pi_durable::types::RunningTask<(), ParentState, ()>,
                  runtime: pi_durable::tasks::TaskRuntimeHandle<(), ParentState, (), ()>,
                  ctx: Context| {
                let child_def = child_for_parent.clone();
                Box::pin(async move {
                    let ownership = ConversationOwnership::Task {
                        task_id: task.id.erased(),
                    };
                    runtime
                        .commit(
                            move |tx, _current| {
                                Box::pin(async move {
                                    tx.create_conversation(ownership).await?;
                                    tx.create_conversation(ownership).await?;
                                    Ok(None)
                                })
                            },
                            ctx.clone(),
                        )
                        .await?;
                    let task_ownership = TaskOwnership::Task {
                        task_id: task.id.erased(),
                    };
                    runtime
                        .commit(
                            move |tx, _current| {
                                let child_def = child_def.clone();
                                Box::pin(async move {
                                    let child = tx
                                        .create_task(
                                            &child_def,
                                            &ChildInput { late: false },
                                            TaskOptions {
                                                ownership: task_ownership,
                                                conversation_id: None,
                                                background: false,
                                            },
                                        )
                                        .await?
                                        .erased();
                                    Ok(Some(NextTaskState::Waiting {
                                        checkpoint: ParentState::Join { child },
                                        on: vec![child],
                                        policy: JoinPolicy::AllSettled,
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                }) as pi_js::BoxFuture<pi_js::Result<()>>
            },
        ) as _,
    );
    parent_phases.insert(
        "join".to_owned(),
        Arc::new(
            move |task: pi_durable::types::RunningTask<(), ParentState, ()>,
                  runtime: pi_durable::tasks::TaskRuntimeHandle<(), ParentState, (), ()>,
                  ctx: Context| {
                let child_def = child_for_join.clone();
                Box::pin(async move {
                    let ownership = TaskOwnership::Task {
                        task_id: task.id.erased(),
                    };
                    runtime
                        .commit(
                            move |tx, _current| {
                                let child_def = child_def.clone();
                                Box::pin(async move {
                                    tx.create_task(
                                        &child_def,
                                        &ChildInput { late: true },
                                        TaskOptions {
                                            ownership,
                                            conversation_id: None,
                                            background: false,
                                        },
                                    )
                                    .await?;
                                    Ok(Some(NextTaskState::Running {
                                        checkpoint: ParentState::Finish,
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                }) as pi_js::BoxFuture<pi_js::Result<()>>
            },
        ) as _,
    );
    parent_phases.insert(
        "finish".to_owned(),
        Arc::new(
            |_task: pi_durable::types::RunningTask<(), ParentState, ()>,
             runtime: pi_durable::tasks::TaskRuntimeHandle<(), ParentState, (), ()>,
             ctx: Context| {
                Box::pin(async move {
                    runtime
                        .commit(|_tx, _current| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                        .await
                }) as pi_js::BoxFuture<pi_js::Result<()>>
            },
        ) as _,
    );
    let parent_task = define_task(TaskDefinition {
        name: "test.graph-parent".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(ParentState::Spawn)),
        phases: parent_phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });

    (parent_task, child_task)
}

fn statuses(graph: &TaskGraph) -> indexmap::IndexMap<String, &'static str> {
    let mut map = indexmap::IndexMap::new();
    for node in graph.tasks.values() {
        let status = match &node.state {
            TaskGraphState::Pending { .. } => "pending",
            TaskGraphState::Running { .. } => "running",
            TaskGraphState::Waiting { .. } => "waiting",
            TaskGraphState::Completing { .. } => "completing",
        };
        map.insert(format!("{}#{}", node.kind, node.id), status);
    }
    map
}

#[tokio::test]
async fn follows_every_live_task_through_its_statuses_owner_edges_and_owned_conversations() {
    let child_gate = deferred::<()>();
    let late_gate = deferred::<()>();
    let (parent_def, child_def) = family(child_gate.clone(), late_gate.clone());
    let storage = Arc::new(CountingScanStorage::new());
    let opened = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![parent_def.as_any().clone(), child_def.as_any().clone()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open tasks");
    let harness = opened.harness;
    let root = harness.root(context(), None).await.expect("root");
    let seen = Arc::new(Mutex::new(Vec::<TaskGraph>::new()));

    let observe = |harness: &pi_durable::harness::harness::Harness, seen: &Arc<Mutex<Vec<TaskGraph>>>| {
        let harness = harness.clone();
        let seen = Arc::clone(seen);
        async move {
            let opened = harness.task_graph(context()).await.expect("taskGraph");
            let seen_cb = Arc::clone(&seen);
            let _unsub = opened.subscribe(Arc::new(move |value, _ctx, delivery| {
                let seen_cb = Arc::clone(&seen_cb);
                Box::pin(async move {
                    if delivery.kind == ReplicatedStateDeliveryKind::Update {
                        seen_cb.lock().expect("lock seen").push(value);
                    }
                    Ok(())
                })
            }));
            opened
        }
    };

    let mut graph = observe(&harness, &seen).await;
    assert_eq!(graph.value(), TaskGraph::default());
    let mut scans_after_initial = storage.scan_tasks_count();

    let parent = root
        .commit(
            {
                let parent_def = parent_def.clone();
                move |tx| async move {
                    tx.create_task(
                        &parent_def,
                        &(),
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: None,
                            background: false,
                        },
                    )
                    .await
                }
            },
            context(),
        )
        .await
        .expect("create parent");
    flush().await;

    let parent_key = parent.to_string();
    let parent_node = graph.value().tasks.get(&parent_key).cloned().expect("parent node");
    assert_eq!(
        parent_node,
        pi_durable::harness::types::TaskGraphNode {
            id: parent.erased(),
            kind: "test.graph-parent".to_owned(),
            conversation_id: root.id(),
            owner: None,
            background: false,
            abort_requested: false,
            state: TaskGraphState::Pending {
                phase: "spawn".to_owned()
            },
            conversations: Vec::new(),
        }
    );

    harness.resume().expect("resume");
    eventually_sync(|| {
        let current = graph.value();
        current.tasks.len() == 2
            && current
                .tasks
                .values()
                .find(|node| node.kind == "test.graph-child")
                .is_some_and(|node| matches!(node.state, TaskGraphState::Running { .. }))
    })
    .await;

    let current = graph.value();
    let first = current
        .tasks
        .values()
        .find(|node| node.kind == "test.graph-child")
        .expect("child node")
        .id;
    let parent_node = current.tasks.get(&parent_key).expect("parent node");
    assert_eq!(
        parent_node.state,
        TaskGraphState::Waiting {
            phase: "join".to_owned(),
            on: vec![first],
            policy: JoinPolicy::AllSettled,
        }
    );
    assert_eq!(parent_node.conversations.len(), 2);
    let mut sorted_convs = parent_node.conversations.clone();
    sorted_convs.sort();
    assert_eq!(sorted_convs, parent_node.conversations);
    let owned = parent_node.conversations[0];
    let first_node = current.tasks.get(&first.to_string()).expect("first node");
    assert_eq!(first_node.owner, Some(parent.erased()));
    assert_eq!(first_node.conversation_id, root.id());
    assert!(harness.conversation(owned, context()).await.expect("conv").is_some());

    // Rebuild checks mount disposal: acquiring again after disposing scans storage again and matches value.
    let advanced = graph.value();
    graph.dispose().expect("dispose");
    graph = observe(&harness, &seen).await;
    let next_scans = storage.scan_tasks_count();
    assert!(next_scans > scans_after_initial);
    scans_after_initial = next_scans;
    assert_eq!(graph.value(), advanced);

    child_gate.resolve(());
    eventually_sync(|| {
        matches!(
            graph.value().tasks.get(&parent_key).map(|node| &node.state),
            Some(TaskGraphState::Completing { .. })
        )
    })
    .await;
    let current = graph.value();
    let parent_after_child = current.tasks.get(&parent_key).expect("parent node");
    assert_eq!(
        parent_after_child.state,
        TaskGraphState::Completing {
            outcome: "completed".to_owned(),
        }
    );
    assert!(current.tasks.get(&first.to_string()).is_none());
    assert_eq!(parent_after_child.conversations, parent_node.conversations);

    let advanced2 = graph.value();
    graph.dispose().expect("dispose 2");
    graph = observe(&harness, &seen).await;
    assert!(storage.scan_tasks_count() > scans_after_initial);
    assert_eq!(graph.value(), advanced2);

    late_gate.resolve(());
    harness.wait_for_task(parent, context()).await.expect("wait parent");
    flush().await;
    assert_eq!(graph.value(), TaskGraph::default());

    let recorded = seen.lock().expect("lock seen").clone();
    for window in recorded.windows(2) {
        assert_ne!(window[0], window[1]);
    }
    assert!(
        recorded
            .iter()
            .any(|value| value.tasks.get(&parent_key).is_some_and(|n| n.conversations.len() == 2))
    );

    graph.dispose().expect("dispose final");
    harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn builds_from_committed_tasks_shows_surviving_tasks_as_pending_after_reopen_and_marks_aborts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("session.sqlite");
    let gate = deferred::<()>();

    let mut work_phases = indexmap::IndexMap::new();
    let gate_run = gate.clone();
    work_phases.insert(
        "work".to_owned(),
        Arc::new(
            move |_task: pi_durable::types::RunningTask<(), JsonValue, ()>,
                  runtime: pi_durable::tasks::TaskRuntimeHandle<(), JsonValue, (), ()>,
                  ctx: Context| {
                let gate_run = gate_run.clone();
                Box::pin(async move {
                    let signal = runtime.signal()?;
                    tokio::select! {
                        _ = gate_run.wait() => {}
                        _ = aborted::<()>(&signal) => {}
                    }
                    runtime
                        .commit(|_tx, _current| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                        .await
                }) as pi_js::BoxFuture<pi_js::Result<()>>
            },
        ) as _,
    );
    let work_task: Task<(), JsonValue, (), ()> = define_task(TaskDefinition {
        name: "test.graph-work".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(serde_json::json!({ "phase": "work" }))),
        phases: work_phases,
        abort: Arc::new(|_, runtime, ctx| {
            Box::pin(async move {
                runtime
                    .commit(
                        |_tx, _current| {
                            Box::pin(async {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Aborted {
                                        reason: Some("test".to_owned()),
                                        result: None,
                                    },
                                }))
                            })
                        },
                        ctx,
                    )
                    .await
            })
        }),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });

    let sqlite1 = open_node_sqlite_storage(&path.to_string_lossy(), NodeSqliteStorageOptions::default())
        .await
        .expect("sqlite1");
    let first = open_tasks(
        Arc::new(sqlite1),
        vec![work_task.as_any().clone()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open1");
    let first_root = first.harness.root(context(), None).await.expect("root1");
    let work_c = work_task.clone();
    let (foreground, background, owned) = first_root
        .commit(
            move |tx| async move {
                let foreground = tx
                    .create_task(
                        &work_c,
                        &(),
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: None,
                            background: false,
                        },
                    )
                    .await?;
                let background = tx
                    .create_task(
                        &work_c,
                        &(),
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: None,
                            background: true,
                        },
                    )
                    .await?;
                let owned = tx
                    .create_conversation(ConversationOwnership::Task {
                        task_id: foreground.erased(),
                    })
                    .await?;
                Ok((foreground, background, owned.id))
            },
            context(),
        )
        .await
        .expect("seed tasks");
    first.harness.resume().expect("resume1");
    eventually(|| {
        let harness = first.harness.clone();
        async move {
            harness
                .inspect(context())
                .await
                .expect("inspect")
                .tasks
                .iter()
                .all(|task| matches!(task.state, TaskInspectionState::Running))
        }
    })
    .await;
    let running = first.harness.task_graph(context()).await.expect("graph1");
    let mut expected_running = indexmap::IndexMap::new();
    expected_running.insert(format!("test.graph-work#{foreground}"), "running");
    expected_running.insert(format!("test.graph-work#{background}"), "running");
    assert_eq!(statuses(&running.value()), expected_running);
    running.dispose().expect("dispose1");
    first.harness.close(context()).await.expect("close1");

    let sqlite2 = open_node_sqlite_storage(&path.to_string_lossy(), NodeSqliteStorageOptions::default())
        .await
        .expect("sqlite2");
    let second = open_tasks(
        Arc::new(sqlite2),
        vec![work_task.as_any().clone()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open2");
    let watch = second.harness.watch_task_graph(context()).await.expect("watch");
    let mut expected_pending = indexmap::IndexMap::new();
    expected_pending.insert(format!("test.graph-work#{foreground}"), "pending");
    expected_pending.insert(format!("test.graph-work#{background}"), "pending");
    assert_eq!(statuses(&watch.value()), expected_pending);
    assert_eq!(
        watch
            .value()
            .tasks
            .get(&foreground.to_string())
            .expect("fg")
            .conversations,
        vec![owned]
    );
    assert!(watch.value().tasks.get(&background.to_string()).expect("bg").background);

    let replica = Arc::new(Mutex::new(serde_json::to_value(watch.value()).expect("json")));
    let frames = Arc::new(Mutex::new(Vec::<Vec<Op>>::new()));
    let replica_cb = Arc::clone(&replica);
    let frames_cb = Arc::clone(&frames);
    watch
        .start(Arc::new(move |value, ops, _| {
            let replica_cb = Arc::clone(&replica_cb);
            let frames_cb = Arc::clone(&frames_cb);
            Box::pin(async move {
                let next = {
                    let guard = replica_cb.lock().expect("lock replica");
                    apply_immutable(Some(guard.clone()), &ops)?
                };
                assert_eq!(next, serde_json::to_value(&value).expect("json"));
                *replica_cb.lock().expect("lock replica") = next;
                frames_cb.lock().expect("lock frames").push(ops);
                Ok(())
            })
        }))
        .expect("start watch");

    second.harness.resume().expect("resume2");
    eventually_sync(|| {
        matches!(
            watch.value().tasks.get(&background.to_string()).map(|n| &n.state),
            Some(TaskGraphState::Running { .. })
        )
    })
    .await;
    assert_eq!(
        second
            .harness
            .abort_task(background.erased(), context())
            .await
            .expect("abort bg"),
        AbortTaskResult::Marked
    );
    second
        .harness
        .wait_for_task(background, context())
        .await
        .expect("wait bg");
    flush().await;

    let recorded_frames = frames.lock().expect("lock frames").clone();
    let bg_key = background.to_string();
    assert!(recorded_frames.iter().any(|ops| {
        ops.iter().any(|op| match op {
            Op::Set(OpSet(SetVerb::S, path, value))
                if path == &vec![Seg::String("tasks".to_owned()), Seg::String(bg_key.clone())] =>
            {
                value.get("abortRequested").and_then(JsonValue::as_bool) == Some(true)
            }
            _ => false,
        })
    }));
    assert_eq!(
        recorded_frames.last().cloned(),
        Some(vec![Op::Delete(OpDelete(
            DeleteVerb::D,
            vec![Seg::String("tasks".to_owned()), Seg::String(bg_key)],
        ))])
    );
    let final_graph: TaskGraph = serde_json::from_value(replica.lock().expect("lock replica").clone()).expect("graph");
    assert_eq!(
        final_graph.tasks.keys().cloned().collect::<Vec<_>>(),
        vec![foreground.to_string()]
    );

    watch.stop().await;
    gate.resolve(());
    second
        .harness
        .wait_for_task(foreground, context())
        .await
        .expect("wait fg");
    second.harness.close(context()).await.expect("close2");
}

#[tokio::test]
async fn lists_owned_conversations_in_id_order_whatever_order_one_commit_creates_them_in() {
    #[derive(Clone, Debug, Serialize, Deserialize)]
    struct SpawnerInput {
        at: EntryId,
    }

    let gate = deferred::<()>();
    let created = Arc::new(Mutex::new(Vec::<ConversationId>::new()));
    let created_cb = Arc::clone(&created);
    let gate_cb = gate.clone();

    let mut phases = indexmap::IndexMap::new();
    phases.insert(
        "spawn".to_owned(),
        Arc::new(
            move |task: pi_durable::types::RunningTask<SpawnerInput, JsonValue, ()>,
                  runtime: pi_durable::tasks::TaskRuntimeHandle<SpawnerInput, JsonValue, (), ()>,
                  ctx: Context| {
                let created_cb = Arc::clone(&created_cb);
                let gate_cb = gate_cb.clone();
                Box::pin(async move {
                    let conv_id = runtime.conversation_id()?;
                    let at = task.input.at;
                    let ownership = ConversationOwnership::Task {
                        task_id: task.id.erased(),
                    };
                    let created_inner = Arc::clone(&created_cb);
                    runtime
                        .commit(
                            move |tx, _current| {
                                Box::pin(async move {
                                    let (forked, fresh) = tokio::join!(
                                        tx.fork_conversation(conv_id, at, ownership),
                                        tx.create_conversation(ownership),
                                    );
                                    let forked = forked?;
                                    let fresh = fresh?;
                                    let mut guard = created_inner.lock().expect("lock created");
                                    guard.push(forked.id);
                                    guard.push(fresh.id);
                                    Ok(None)
                                })
                            },
                            ctx.clone(),
                        )
                        .await?;
                    let _ = gate_cb.wait().await;
                    runtime
                        .commit(|_tx, _current| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                        .await
                }) as pi_js::BoxFuture<pi_js::Result<()>>
            },
        ) as _,
    );
    let spawner: Task<SpawnerInput, JsonValue, (), ()> = define_task(TaskDefinition {
        name: "test.graph-spawner".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(serde_json::json!({ "phase": "spawn" }))),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });

    let opened = open_tasks(
        Arc::new(MemoryStorage::new()),
        vec![spawner.as_any().clone()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open");
    let harness = opened.harness;
    let root = harness.root(context(), None).await.expect("root");
    let graph = harness.task_graph(context()).await.expect("graph");
    let root_id = root.id();
    let spawner_c = spawner.clone();
    let id = root
        .commit(
            move |tx| async move {
                let entry = tx
                    .append_entry(
                        root_id,
                        EntryDraft {
                            kind: "note".to_owned(),
                            model: None,
                            data: None,
                            head: None,
                            edits: None,
                        },
                    )
                    .await?;
                tx.create_task(
                    &spawner_c,
                    &SpawnerInput { at: entry.id },
                    TaskOptions {
                        ownership: TaskOwnership::Conversation,
                        conversation_id: None,
                        background: false,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("seed");
    harness.resume().expect("resume");
    let key = id.to_string();
    eventually_sync(|| {
        graph
            .value()
            .tasks
            .get(&key)
            .is_some_and(|node| node.conversations.len() == 2)
    })
    .await;
    let advanced = graph.value();
    let mut expected_ids = created.lock().expect("lock created").clone();
    expected_ids.sort();
    assert_eq!(advanced.tasks.get(&key).expect("node").conversations, expected_ids);
    graph.dispose().expect("dispose");
    let rebuilt = harness.task_graph(context()).await.expect("rebuilt");
    assert_eq!(rebuilt.value(), advanced);
    rebuilt.dispose().expect("dispose rebuilt");
    gate.resolve(());
    harness.wait_for_task(id, context()).await.expect("wait");
    harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn registers_nothing_for_an_acquisition_cancelled_while_it_waits_for_the_line() {
    let storage = Arc::new(CountingScanStorage::new());
    let opened = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Vec::new(),
        OpenTasksOptions::default(),
    )
    .await
    .expect("open");
    let harness = opened.harness;
    let root = harness.root(context(), None).await.expect("root");
    let child_gate = deferred::<()>();
    let late_gate = deferred::<()>();
    let (_parent, child_task) = family(child_gate, late_gate);
    let root_id = root.id();
    root.commit(
        move |tx| async move {
            tx.create_task(
                &child_task,
                &ChildInput { late: false },
                TaskOptions {
                    ownership: TaskOwnership::Conversation,
                    conversation_id: None,
                    background: false,
                },
            )
            .await
        },
        context(),
    )
    .await
    .expect("create task");

    let held = storage.inner.hold_commits();
    let harness_blk = harness.clone();
    let blocking = tokio::spawn(async move {
        harness_blk
            .commit(
                move |tx| async move {
                    tx.append_entry(
                        root_id,
                        EntryDraft {
                            kind: "blocker".to_owned(),
                            model: None,
                            data: None,
                            head: None,
                            edits: None,
                        },
                    )
                    .await
                },
                context(),
            )
            .await
    });
    held.entered().await;
    let controller = AbortController::new();
    let cancel_ctx = with_abort_signal(controller.signal(), context());
    let harness_watch = harness.clone();
    let cancelled = tokio::spawn(async move { harness_watch.watch_task_graph(cancel_ctx).await });
    controller.abort(Some(pi_js::abort::AbortReason::from_error(&pi_js::Error::msg(
        "cancelled",
    ))));
    held.release();
    blocking.await.expect("join blocking").expect("blocking ok");
    let err = match cancelled.await.expect("join cancelled") {
        Ok(_) => panic!("expected cancelled error"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("cancelled"));

    // No observer kept the mount: first acquisition builds one, disposing drops it, second builds again.
    let scans_before = storage.scan_tasks_count();
    let first = harness.task_graph(context()).await.expect("first");
    let scans_after_first = storage.scan_tasks_count();
    assert!(scans_after_first > scans_before);
    let value = first.value();
    first.dispose().expect("dispose first");
    let second = harness.task_graph(context()).await.expect("second");
    assert!(storage.scan_tasks_count() > scans_after_first);
    assert_eq!(second.value(), value);
    second.dispose().expect("dispose second");
    harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn publishes_no_revision_for_a_commit_that_changes_no_node_and_shares_one_mount_between_observers() {
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    let gate_run = gate.clone();
    let reached_run = reached.clone();

    let mut phases = indexmap::IndexMap::new();
    phases.insert(
        "work".to_owned(),
        Arc::new(
            move |_task: pi_durable::types::RunningTask<(), JsonValue, ()>,
                  runtime: pi_durable::tasks::TaskRuntimeHandle<(), JsonValue, (), ()>,
                  ctx: Context| {
                let gate_run = gate_run.clone();
                let reached_run = reached_run.clone();
                Box::pin(async move {
                    runtime.memo_set("seen", &true, ctx.clone()).await?;
                    reached_run.resolve(());
                    let _ = gate_run.wait().await;
                    runtime
                        .commit(|_tx, _current| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                        .await
                }) as pi_js::BoxFuture<pi_js::Result<()>>
            },
        ) as _,
    );
    let memo_task: Task<(), JsonValue, (), ()> = define_task(TaskDefinition {
        name: "test.graph-memo".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(serde_json::json!({ "phase": "work" }))),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });

    let storage = Arc::new(CountingScanStorage::new());
    let opened = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![memo_task.as_any().clone()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open");
    let harness = opened.harness;
    let root = harness.root(context(), None).await.expect("root");
    let memo_c = memo_task.clone();
    let id = root
        .commit(
            move |tx| async move {
                tx.create_task(
                    &memo_c,
                    &(),
                    TaskOptions {
                        ownership: TaskOwnership::Conversation,
                        conversation_id: None,
                        background: false,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("create memo");

    let scans_before = storage.scan_tasks_count();
    let first = harness.task_graph(context()).await.expect("first");
    let scans_after_first = storage.scan_tasks_count();
    assert!(scans_after_first > scans_before);
    let second = harness.task_graph(context()).await.expect("second");
    assert_eq!(storage.scan_tasks_count(), scans_after_first);
    assert_eq!(second.value(), first.value());

    let updates = Arc::new(Mutex::new(Vec::<String>::new()));
    let updates_cb = Arc::clone(&updates);
    let key = id.to_string();
    let key_cb = key.clone();
    let _unsub = first.subscribe(Arc::new(move |value, _ctx, delivery| {
        let updates_cb = Arc::clone(&updates_cb);
        let key_cb = key_cb.clone();
        Box::pin(async move {
            if delivery.kind == ReplicatedStateDeliveryKind::Update {
                let status = match value.tasks.get(&key_cb).map(|node| &node.state) {
                    Some(TaskGraphState::Pending { .. }) => "pending",
                    Some(TaskGraphState::Running { .. }) => "running",
                    Some(TaskGraphState::Waiting { .. }) => "waiting",
                    Some(TaskGraphState::Completing { .. }) => "completing",
                    None => "gone",
                };
                updates_cb.lock().expect("lock updates").push(status.to_owned());
            }
            Ok(())
        })
    }));

    harness.resume().expect("resume");
    reached.wait().await.expect("reached");
    flush().await;
    // Reservation changed the node; the memo commit did not.
    assert_eq!(updates.lock().expect("lock updates").clone(), vec!["running"]);

    gate.resolve(());
    harness.wait_for_task(id, context()).await.expect("wait");
    flush().await;
    assert_eq!(updates.lock().expect("lock updates").clone(), vec!["running", "gone"]);

    let last = first.value();
    let scans_before_rebuilt = storage.scan_tasks_count();
    first.dispose().expect("dispose1");
    second.dispose().expect("dispose2");
    let rebuilt = harness.task_graph(context()).await.expect("rebuilt");
    assert!(storage.scan_tasks_count() > scans_before_rebuilt);
    assert_eq!(rebuilt.value(), last);
    assert_eq!(last.tasks.get(&key), None);
    let _ = TaskStatus::Pending;
    rebuilt.dispose().expect("dispose rebuilt");
    harness.close(context()).await.expect("close");
}
