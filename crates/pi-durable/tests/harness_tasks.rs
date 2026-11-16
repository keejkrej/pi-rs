//! Port of packages/durable/test/harness-tasks.test.ts

mod support;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::types::ModelThinkingLevel;
use pi_ai::{TSchema, Type, create_models};
use pi_chord::Context;
use pi_chord::context::with_abort_signal;
use pi_durable::documents::{
    DocDefinitionInput, RewindableConversationDocToken, SessionDocToken, TaskDocToken,
    define_rewindable_conversation_doc, define_session_doc, define_task_doc,
};
use pi_durable::entries::Entry;
use pi_durable::errors::StorageRejected;
use pi_durable::harness::harness::{Conversation, Harness};
use pi_durable::harness::registry::{RegistryImpl, create_registry};
use pi_durable::harness::types::{
    AgentChange, FieldChange, HarnessOptions, RegistryReader, RegistrySnapshot, ToolExecutionResult, ToolRegistration,
};
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::tasks::{Task, TaskDefinition, TaskRuntimeHandle, TypedAbortHandler, TypedPhaseHandler, define_task};
use pi_durable::types::{
    ConversationId, ConversationOwnership, ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentId,
    DocumentPoint, DocumentQuery, DocumentRecord, EntryDraft, EntryId, EntryLookup, EntryQuery, EntryRecord,
    ErasedTaskRecord, JoinPolicy, JsonValue, NextTaskState, Page, RewindableFork, Seq, Storage, StorageWrite,
    StoredDocument, SubmissionId, SubmissionQuery, SubmissionRecord, TaskId, TaskOptions, TaskOutcome,
    TaskOutcomeError, TaskOwnership, TaskQuery, TaskState, TaskStatus, WatchEnd,
};
use pi_durable::{AbortTaskResult, define_tool};
use pi_js::abort::{AbortController, AbortReason};
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use support::harness_support::{add_hooks, add_task, add_tool, user};
use support::session_support::{ControlledStorage, context, flush};
use support::task_support::{
    Deferred, OpenTasksOptions, OpenedTasks, Tracked, aborted, aborted_with, completed, deferred, eventually,
    eventually_sync, open_tasks, settled, track,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum StepCheckpoint {
    Run,
}

type StepRuntime<R> = TaskRuntimeHandle<(), StepCheckpoint, R, ()>;

fn one_step<R, FRun, FutRun>(name: &str, run: FRun) -> Task<(), StepCheckpoint, R, ()>
where
    R: Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    FRun: Fn(pi_durable::types::RunningTask<(), StepCheckpoint, R>, StepRuntime<R>, Context) -> FutRun
        + Send
        + Sync
        + 'static,
    FutRun: std::future::Future<Output = pi_js::Result<()>> + Send + 'static,
{
    one_step_with_abort(name, run, |_task, runtime, ctx| async move {
        runtime
            .commit(|_tx, _current| Box::pin(async { Ok(Some(aborted_with("test"))) }), ctx)
            .await
    })
}

fn one_step_with_abort<R, FRun, FutRun, FAbort, FutAbort>(
    name: &str,
    run: FRun,
    abort: FAbort,
) -> Task<(), StepCheckpoint, R, ()>
where
    R: Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    FRun: Fn(pi_durable::types::RunningTask<(), StepCheckpoint, R>, StepRuntime<R>, Context) -> FutRun
        + Send
        + Sync
        + 'static,
    FutRun: std::future::Future<Output = pi_js::Result<()>> + Send + 'static,
    FAbort: Fn(pi_durable::types::RunningTask<(), StepCheckpoint, R>, StepRuntime<R>, Context) -> FutAbort
        + Send
        + Sync
        + 'static,
    FutAbort: std::future::Future<Output = pi_js::Result<()>> + Send + 'static,
{
    let run = Arc::new(run);
    let abort = Arc::new(abort);
    let mut phases: IndexMap<String, TypedPhaseHandler<(), StepCheckpoint, R, ()>> = IndexMap::new();
    phases.insert(
        "run".to_owned(),
        Arc::new(move |task, runtime, ctx| {
            let run = Arc::clone(&run);
            Box::pin(async move { run(task, runtime, ctx).await })
        }),
    );
    let abort_fn: TypedAbortHandler<(), StepCheckpoint, R, ()> = Arc::new(move |task, runtime, ctx| {
        let abort = Arc::clone(&abort);
        Box::pin(async move { abort(task, runtime, ctx).await })
    });
    define_task(TaskDefinition {
        name: name.to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(StepCheckpoint::Run)),
        phases,
        abort: abort_fn,
        migrate: None,
        _hooks: std::marker::PhantomData,
    })
}

fn gated_task(name: &str, gate: Deferred<()>) -> Task<(), StepCheckpoint, (), ()> {
    one_step(name, move |_task, runtime, ctx| {
        let gate = gate.clone();
        async move {
            gate.wait().await?;
            runtime
                .commit(|_tx, _current| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                .await
        }
    })
}

async fn start<R>(conversation: &Conversation, task: &Task<(), StepCheckpoint, R, ()>, background: bool) -> TaskId<R>
where
    R: Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
{
    let task_c = task.clone();
    conversation
        .commit(
            move |tx| async move {
                tx.create_task(
                    &task_c,
                    &(),
                    TaskOptions {
                        ownership: TaskOwnership::Conversation,
                        conversation_id: None,
                        background,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("create task")
}

struct OpenRoot {
    harness: Harness,
    registry: Arc<RegistryImpl>,
    reports: Arc<Mutex<Vec<pi_js::Error>>>,
    root: Conversation,
}

async fn open_root(
    tasks: Vec<pi_durable::tasks::AnyTask>,
    options: OpenTasksOptions,
    storage: Option<Arc<dyn Storage>>,
) -> OpenRoot {
    let storage = storage.unwrap_or_else(|| Arc::new(MemoryStorage::new()));
    let opened: OpenedTasks = open_tasks(storage, tasks, options).await.expect("open tasks");
    let root = opened.harness.root(context(), None).await.expect("root");
    OpenRoot {
        harness: opened.harness,
        registry: opened.registry,
        reports: opened.reports,
        root,
    }
}

async fn mark_durably(harness: &Harness, id: TaskId) -> Tracked<pi_js::Result<AbortTaskResult>> {
    let harness_c = harness.clone();
    let aborting = track(async move { harness_c.abort_task(id, context()).await });
    loop {
        if harness
            .get_erased_task(id, context())
            .await
            .expect("get_erased_task")
            .is_some_and(|t| t.abort_requested)
        {
            break;
        }
        flush().await;
    }
    aborting
}

fn with_controller_abort(controller: &AbortController) -> Context {
    with_abort_signal(controller.signal(), context())
}

/// Storage wrapper that can fail `find_document` lookups for `"pi.agent"` on demand.
struct FailingAgentDocStorage {
    inner: ControlledStorage,
    fail: AtomicBool,
}

impl FailingAgentDocStorage {
    fn new() -> Self {
        Self {
            inner: ControlledStorage::new(),
            fail: AtomicBool::new(false),
        }
    }

    fn set_fail(&self, value: bool) {
        self.fail.store(value, Ordering::Release);
    }
}

#[async_trait]
impl Storage for FailingAgentDocStorage {
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
        self.inner.scan_tasks(query, limit, cursor, context).await
    }

    async fn submission(&self, id: SubmissionId, context: Context) -> pi_js::Result<Option<SubmissionRecord>> {
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
        if self.fail.load(Ordering::Acquire) && address.kind == "pi.agent" {
            return Err(pi_js::Error::msg("agent doc read broke"));
        }
        self.inner.find_document(address, at, context).await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<StoredDocument>> {
        if self.fail.load(Ordering::Acquire) {
            return Err(pi_js::Error::msg("agent doc read broke"));
        }
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

// ---- describe("task phases") ----

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CounterInput {
    to: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum CounterCheckpoint {
    Count { n: i64 },
}

#[tokio::test]
async fn continues_one_invocation_through_checkpoint_progress_and_completes_with_a_typed_result() {
    let seen = Arc::new(Mutex::new(Vec::<i64>::new()));
    let runtime_ids = Arc::new(Mutex::new(BTreeSet::<usize>::new()));

    let seen_c = Arc::clone(&seen);
    let ids_c = Arc::clone(&runtime_ids);
    let mut phases: IndexMap<String, TypedPhaseHandler<CounterInput, CounterCheckpoint, i64, ()>> = IndexMap::new();
    phases.insert(
        "count".to_owned(),
        Arc::new(move |task, runtime, ctx| {
            let seen_c = Arc::clone(&seen_c);
            let ids_c = Arc::clone(&ids_c);
            Box::pin(async move {
                let CounterCheckpoint::Count { n } = task.checkpoint;
                seen_c.lock().unwrap_or_else(|e| e.into_inner()).push(n);
                let ptr = Arc::as_ptr(&runtime.erased) as *const () as usize;
                ids_c.lock().unwrap_or_else(|e| e.into_inner()).insert(ptr);
                let to = task.input.to;
                runtime
                    .commit(
                        move |_tx, current| {
                            Box::pin(async move {
                                let CounterCheckpoint::Count { n } = current.checkpoint;
                                let next = n + 1;
                                if next == to {
                                    Ok(Some(completed(next)))
                                } else {
                                    Ok(Some(NextTaskState::Running {
                                        checkpoint: CounterCheckpoint::Count { n: next },
                                    }))
                                }
                            })
                        },
                        ctx,
                    )
                    .await
            })
        }),
    );
    let counter = define_task(TaskDefinition {
        name: "test.counter".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(CounterCheckpoint::Count { n: 0 })),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });

    let opened = open_root(vec![counter.clone().into_any()], OpenTasksOptions::default(), None).await;
    let id = opened
        .root
        .commit(
            move |tx| async move {
                tx.create_task(
                    &counter,
                    &CounterInput { to: 3 },
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
        .expect("create counter");
    opened.harness.resume().expect("resume");
    let receipt = opened.harness.wait_for_task(id, context()).await.expect("wait");
    let result = match receipt.outcome {
        TaskOutcome::Completed { result } => Some(result),
        _ => None,
    };
    assert_eq!(result, Some(3));
    assert_eq!(*seen.lock().unwrap_or_else(|e| e.into_inner()), vec![0, 1, 2]);
    assert_eq!(runtime_ids.lock().unwrap_or_else(|e| e.into_inner()).len(), 1);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn faults_a_phase_without_durable_progress_and_a_throwing_phase() {
    let idle = one_step::<(), _, _>("test.idle", |_t, _rt, _ctx| async { Ok(()) });
    let doc_only = one_step::<(), _, _>("test.document-only", |_t, rt, ctx| async move {
        rt.commit(|_tx, _current| Box::pin(async { Ok(None) }), ctx).await
    });
    let throws = one_step::<(), _, _>("test.throws", |_t, _rt, _ctx| async { Err(pi_js::Error::msg("boom")) });

    let opened = open_root(
        vec![
            idle.clone().into_any(),
            doc_only.clone().into_any(),
            throws.clone().into_any(),
        ],
        OpenTasksOptions::default(),
        None,
    )
    .await;
    let ids = vec![
        start(&opened.root, &idle, false).await,
        start(&opened.root, &doc_only, false).await,
        start(&opened.root, &throws, false).await,
    ];
    opened.harness.resume().expect("resume");
    let mut outcomes = Vec::new();
    for id in ids {
        outcomes.push(opened.harness.wait_for_task(id, context()).await.expect("wait").outcome);
    }
    assert_eq!(
        outcomes,
        vec![
            TaskOutcome::Faulted {
                error: TaskOutcomeError {
                    message: "Task test.idle phase run returned without durable progress".to_owned(),
                    detail: None,
                },
            },
            TaskOutcome::Faulted {
                error: TaskOutcomeError {
                    message: "Task test.document-only phase run returned without durable progress".to_owned(),
                    detail: None,
                },
            },
            TaskOutcome::Faulted {
                error: TaskOutcomeError {
                    message: "boom".to_owned(),
                    detail: None,
                },
            },
        ]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_a_committed_terminal_outcome_when_the_handler_throws_afterwards() {
    let late_commit = Arc::new(Mutex::new(None::<String>));
    let late_c = Arc::clone(&late_commit);
    let done = one_step::<String, _, _>("test.done", move |_t, rt, ctx| {
        let late_c = Arc::clone(&late_c);
        async move {
            rt.commit(
                |_tx, _current| Box::pin(async { Ok(Some(completed("ok".to_owned()))) }),
                ctx.clone(),
            )
            .await?;
            let err = rt
                .commit(|_tx, _current| Box::pin(async { Ok(None) }), ctx)
                .await
                .expect_err("late commit should reject");
            *late_c.lock().unwrap_or_else(|e| e.into_inner()) = Some(err.to_string());
            Err(pi_js::Error::msg("after terminal"))
        }
    });

    let opened = open_root(vec![done.clone().into_any()], OpenTasksOptions::default(), None).await;
    let id = start(&opened.root, &done, false).await;
    opened.harness.resume().expect("resume");
    let settled = opened.harness.wait_for_task(id, context()).await.expect("wait");
    assert_eq!(
        settled.outcome,
        TaskOutcome::Completed {
            result: "ok".to_owned()
        }
    );
    eventually_sync(|| late_commit.lock().unwrap_or_else(|e| e.into_inner()).is_some()).await;
    let msg = late_commit.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap();
    assert!(msg.contains(&format!("Task {id} is terminal")), "got: {msg}");
    opened.harness.close(context()).await.expect("close");
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum CollectCheckpoint {
    Collect { items: Vec<String> },
}

#[tokio::test]
async fn compares_checkpoints_by_value_including_arrays() {
    let mut phases: IndexMap<String, TypedPhaseHandler<(), CollectCheckpoint, (), ()>> = IndexMap::new();
    phases.insert(
        "collect".to_owned(),
        Arc::new(|task, runtime, ctx| {
            Box::pin(async move {
                let CollectCheckpoint::Collect { items } = task.checkpoint;
                let mut next = items.clone();
                if items.len() < 2 {
                    next.push(format!("item{}", items.len()));
                }
                runtime
                    .commit(
                        move |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Running {
                                    checkpoint: CollectCheckpoint::Collect { items: next },
                                }))
                            })
                        },
                        ctx,
                    )
                    .await
            })
        }),
    );
    let collect = define_task(TaskDefinition {
        name: "test.collect".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(CollectCheckpoint::Collect { items: Vec::new() })),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });
    let opened = open_root(vec![collect.clone().into_any()], OpenTasksOptions::default(), None).await;
    let id = opened
        .root
        .commit(
            move |tx| async move {
                tx.create_task(
                    &collect,
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
        .expect("create collect");
    opened.harness.resume().expect("resume");
    let settled = opened.harness.wait_for_task(id, context()).await.expect("wait");
    assert_eq!(
        settled.outcome,
        TaskOutcome::Faulted {
            error: TaskOutcomeError {
                message: "Task test.collect phase collect returned without durable progress".to_owned(),
                detail: None,
            },
        }
    );
    opened.harness.close(context()).await.expect("close");
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct TaskProgressDoc {
    lines: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum WriterCheckpoint {
    Write,
    Answer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriterResult {
    entry_id: EntryId,
}

#[tokio::test]
async fn commits_results_with_entries_atomically_keeps_memos_until_terminal_and_retires_task_documents() {
    let progress: TaskDocToken<TaskProgressDoc> = define_task_doc(DocDefinitionInput {
        kind: "test.task-progress",
        version: 1,
        initial: TaskProgressDoc::default,
        migrate: None,
        checkpoint_when: None,
    })
    .expect("progress doc");
    let answer_entry: Entry<()> = Entry::builtin("answer");
    let child = one_step::<(), _, _>("test.child", |_t, rt, ctx| async move {
        rt.commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), ctx)
            .await
    });
    let child_id_slot = Arc::new(Mutex::new(None::<TaskId<()>>));
    let progress_seen = Arc::new(Mutex::new(None::<Vec<String>>));

    let progress_w = progress.clone();
    let child_w = child.clone();
    let child_slot_w = Arc::clone(&child_id_slot);
    let progress_a = progress.clone();
    let seen_a = Arc::clone(&progress_seen);

    let mut phases: IndexMap<String, TypedPhaseHandler<(), WriterCheckpoint, WriterResult, ()>> = IndexMap::new();
    phases.insert(
        "write".to_owned(),
        Arc::new(move |task, runtime, ctx| {
            let progress_w = progress_w.clone();
            let child_w = child_w.clone();
            let child_slot_w = Arc::clone(&child_slot_w);
            Box::pin(async move {
                assert_eq!(runtime.memo_get::<String>("choice", ctx.clone()).await?, None);
                let (choice_a, choice_b) = ("a".to_owned(), "b".to_owned());
                let (w1, w2) = tokio::join!(
                    runtime.memo_set("choice", &choice_a, ctx.clone()),
                    runtime.memo_set("choice", &choice_b, ctx.clone()),
                );
                assert_eq!(vec![w1?, w2?], vec!["a".to_owned(), "a".to_owned()]);
                assert_eq!(
                    runtime.memo_get::<String>("choice", ctx.clone()).await?,
                    Some("a".to_owned())
                );
                // Memo names never clash with object prototype properties.
                assert_eq!(runtime.memo_get::<String>("toString", ctx.clone()).await?, None);
                assert_eq!(
                    runtime.memo_set("toString", &"own".to_owned(), ctx.clone()).await?,
                    "own".to_owned()
                );
                let task_id = task.id.erased();
                runtime
                    .commit(
                        move |tx, _current| {
                            let progress_w = progress_w.clone();
                            let child_w = child_w.clone();
                            let child_slot_w = Arc::clone(&child_slot_w);
                            Box::pin(async move {
                                tx.doc_task(&progress_w, task_id).await?.update(|d| {
                                    d.lines.push("wrote".to_owned());
                                })?;
                                let created = tx
                                    .create_task(
                                        &child_w,
                                        &(),
                                        TaskOptions {
                                            ownership: TaskOwnership::Conversation,
                                            conversation_id: None,
                                            background: false,
                                        },
                                    )
                                    .await?;
                                *child_slot_w.lock().unwrap_or_else(|e| e.into_inner()) = Some(created);
                                Ok(Some(NextTaskState::Running {
                                    checkpoint: WriterCheckpoint::Answer,
                                }))
                            })
                        },
                        ctx,
                    )
                    .await
            })
        }),
    );
    phases.insert(
        "answer".to_owned(),
        Arc::new(move |task, runtime, ctx| {
            let progress_a = progress_a.clone();
            let seen_a = Arc::clone(&seen_a);
            Box::pin(async move {
                let mut expected_memos = IndexMap::new();
                expected_memos.insert("choice".to_owned(), serde_json::json!("a"));
                expected_memos.insert("toString".to_owned(), serde_json::json!("own"));
                assert_eq!(task.memos, Some(expected_memos));
                let task_id = task.id.erased();
                runtime
                    .commit(
                        move |tx, _current| {
                            let progress_a = progress_a.clone();
                            let seen_a = Arc::clone(&seen_a);
                            Box::pin(async move {
                                let lines = tx.doc_task(&progress_a, task_id).await?.get()?.lines;
                                *seen_a.lock().unwrap_or_else(|e| e.into_inner()) = Some(lines);
                                Ok(None)
                            })
                        },
                        ctx.clone(),
                    )
                    .await?;
                runtime
                    .commit(
                        |tx, current| {
                            Box::pin(async move {
                                let entry = tx
                                    .append_entry(
                                        current.conversation_id,
                                        EntryDraft {
                                            kind: "answer".to_owned(),
                                            model: Some(vec![pi_ai::types::Message::User(user("done"))]),
                                            data: None,
                                            head: None,
                                            edits: None,
                                        },
                                    )
                                    .await?;
                                Ok(Some(completed(WriterResult { entry_id: entry.id })))
                            })
                        },
                        ctx,
                    )
                    .await
            })
        }),
    );

    let writer = define_task(TaskDefinition {
        name: "test.writer".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(WriterCheckpoint::Write)),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });

    let opened = open_root(
        vec![writer.clone().into_any(), child.into_any()],
        OpenTasksOptions::default(),
        None,
    )
    .await;
    let conversation = opened
        .harness
        .create_conversation(
            pi_durable::harness::types::ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("create conv");
    let writer_c = writer.clone();
    let id = conversation
        .commit(
            move |tx| async move {
                tx.create_task(
                    &writer_c,
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
        .expect("create writer");
    opened.harness.resume().expect("resume");
    let receipt = opened.harness.wait_for_task(id, context()).await.expect("wait");
    let raw_rec = opened
        .harness
        .get_task(id, context())
        .await
        .expect("get_task")
        .expect("present");
    assert!(matches!(raw_rec.state, TaskState::Terminal { .. }));
    assert_eq!(raw_rec.memos, None);
    let outcome_entry_id = match receipt.outcome.clone() {
        TaskOutcome::Completed { result } => result.entry_id,
        other => panic!("expected completion, got {other:?}"),
    };
    let entry = opened
        .harness
        .commit(move |tx| async move { tx.entry(outcome_entry_id).await }, context())
        .await
        .expect("get entry")
        .expect("present");
    assert!(answer_entry.matches(Some(&entry)));
    assert_eq!(entry.conversation_id, conversation.id());
    assert_eq!(
        *progress_seen.lock().unwrap_or_else(|e| e.into_inner()),
        Some(vec!["wrote".to_owned()])
    );
    assert_eq!(
        opened
            .harness
            .snapshot_task(&progress, id.erased(), context())
            .await
            .expect("snapshot"),
        None
    );
    assert_eq!(
        raw_rec.state,
        TaskState::Terminal {
            outcome: receipt.outcome.clone()
        }
    );
    let cid = child_id_slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .expect("child_id");
    assert_eq!(
        opened
            .harness
            .wait_for_task(cid, context())
            .await
            .expect("wait child")
            .conversation_id,
        conversation.id()
    );
    assert_ne!(opened.root.id(), conversation.id());
    opened.harness.close(context()).await.expect("close");
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum WaiterResumeCheckpoint {
    Wait,
    Resume,
}

#[tokio::test]
async fn resumes_a_waiting_task_once_every_task_in_on_is_terminal_whatever_the_outcome() {
    let gate = deferred::<()>();
    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let on_slot = Arc::new(Mutex::new(Vec::<TaskId>::new()));
    let outcomes_seen = Arc::new(Mutex::new(Vec::<String>::new()));

    let order_w = Arc::clone(&order);
    let on_w = Arc::clone(&on_slot);
    let order_r = Arc::clone(&order);
    let on_r = Arc::clone(&on_slot);
    let outcomes_r = Arc::clone(&outcomes_seen);

    let mut phases: IndexMap<String, TypedPhaseHandler<(), WaiterResumeCheckpoint, (), ()>> = IndexMap::new();
    phases.insert(
        "wait".to_owned(),
        Arc::new(move |_task, runtime, ctx| {
            let order_w = Arc::clone(&order_w);
            let on_w = Arc::clone(&on_w);
            Box::pin(async move {
                order_w
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push("wait".to_owned());
                let on = on_w.lock().unwrap_or_else(|e| e.into_inner()).clone();
                runtime
                    .commit(
                        move |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Waiting {
                                    checkpoint: WaiterResumeCheckpoint::Resume,
                                    on,
                                    policy: JoinPolicy::AllSettled,
                                }))
                            })
                        },
                        ctx,
                    )
                    .await
            })
        }),
    );
    phases.insert(
        "resume".to_owned(),
        Arc::new(move |_task, runtime, ctx| {
            let order_r = Arc::clone(&order_r);
            let on_r = Arc::clone(&on_r);
            let outcomes_r = Arc::clone(&outcomes_r);
            Box::pin(async move {
                order_r
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push("resume".to_owned());
                let on = on_r.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let res = runtime.outcomes(&on, ctx.clone()).await?;
                let labels: Vec<String> = res.iter().map(|o| o.status_str().to_owned()).collect();
                *outcomes_r.lock().unwrap_or_else(|e| e.into_inner()) = labels;
                runtime
                    .commit(|_tx, _current| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                    .await
            })
        }),
    );

    let waiter = define_task(TaskDefinition {
        name: "test.waiter".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(WaiterResumeCheckpoint::Wait)),
        phases,
        abort: Arc::new(|_task, runtime, ctx| {
            Box::pin(async move {
                runtime
                    .commit(|_tx, _c| Box::pin(async { Ok(Some(aborted_with("test"))) }), ctx)
                    .await
            })
        }),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });

    let order_f = Arc::clone(&order);
    let gate_f = gate.clone();
    let first = one_step::<(), _, _>("test.first", move |_t, rt, ctx| {
        let order_f = Arc::clone(&order_f);
        let gate_f = gate_f.clone();
        async move {
            order_f
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push("first".to_owned());
            gate_f.wait().await?;
            rt.commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                .await
        }
    });

    let order_err = Arc::clone(&order);
    let faulting = one_step::<(), _, _>("test.faulting", move |_t, _rt, _ctx| {
        let order_err = Arc::clone(&order_err);
        async move {
            order_err
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push("faulting".to_owned());
            Err(pi_js::Error::msg("fails"))
        }
    });

    let opened = open_root(
        vec![
            first.clone().into_any(),
            faulting.clone().into_any(),
            waiter.clone().into_any(),
        ],
        OpenTasksOptions::default(),
        None,
    )
    .await;
    let first_id = start(&opened.root, &first, false).await;
    let faulting_id = start(&opened.root, &faulting, false).await;
    let on = vec![first_id.erased(), faulting_id.erased()];
    *on_slot.lock().unwrap_or_else(|e| e.into_inner()) = on.clone();
    let waiter_c = waiter.clone();
    let waiter_id = opened
        .root
        .commit(
            move |tx| async move {
                tx.create_task(
                    &waiter_c,
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
        .expect("create waiter");
    opened.harness.resume().expect("resume");
    eventually_sync(|| order.lock().unwrap_or_else(|e| e.into_inner()).len() == 3).await;
    flush().await;
    let mut sorted = order.lock().unwrap_or_else(|e| e.into_inner()).clone();
    sorted.sort();
    assert_eq!(sorted, vec!["faulting", "first", "wait"]);
    let waiting_rec = opened
        .harness
        .get_task(waiter_id, context())
        .await
        .expect("get")
        .expect("present");
    assert!(matches!(
        waiting_rec.state,
        TaskState::Waiting { on: ref w_on, .. } if *w_on == on
    ));
    gate.resolve(());
    opened.harness.wait_for_task(waiter_id, context()).await.expect("wait");
    assert_eq!(
        order
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last()
            .map(String::as_str),
        Some("resume")
    );
    assert_eq!(
        *outcomes_seen.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["completed", "faulted"]
    );
    opened.harness.close(context()).await.expect("close");
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum AbCheckpoint {
    A,
    B,
}

#[tokio::test]
async fn keeps_one_registry_snapshot_per_phase_and_refreshes_it_at_the_phase_boundary() {
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct SeenSnap {
        phase: &'static str,
        tools: Vec<String>,
        same: bool,
    }
    let seen = Arc::new(Mutex::new(Vec::<SeenSnap>::new()));
    let phase_gate = deferred::<()>();
    let entered = deferred::<()>();

    let tools_of = |snapshot: &Arc<dyn RegistrySnapshot>| -> Vec<String> {
        snapshot.tools().into_iter().map(|t| t.tool.name).collect()
    };

    let seen_a = Arc::clone(&seen);
    let gate_a = phase_gate.clone();
    let entered_a = entered.clone();
    let seen_b = Arc::clone(&seen);

    let mut phases: IndexMap<String, TypedPhaseHandler<(), AbCheckpoint, (), ()>> = IndexMap::new();
    phases.insert(
        "a".to_owned(),
        Arc::new(move |_task, runtime, ctx| {
            let seen_a = Arc::clone(&seen_a);
            let gate_a = gate_a.clone();
            let entered_a = entered_a.clone();
            Box::pin(async move {
                let before = runtime.registry()?;
                entered_a.resolve(());
                gate_a.wait().await?;
                let after = runtime.registry()?;
                seen_a.lock().unwrap_or_else(|e| e.into_inner()).push(SeenSnap {
                    phase: "a",
                    tools: tools_of(&after),
                    same: Arc::ptr_eq(&after, &before),
                });
                runtime
                    .commit(
                        |_tx, _c| {
                            Box::pin(async {
                                Ok(Some(NextTaskState::Running {
                                    checkpoint: AbCheckpoint::B,
                                }))
                            })
                        },
                        ctx,
                    )
                    .await
            })
        }),
    );
    phases.insert(
        "b".to_owned(),
        Arc::new(move |_task, runtime, ctx| {
            let seen_b = Arc::clone(&seen_b);
            Box::pin(async move {
                let snap = runtime.registry()?;
                seen_b.lock().unwrap_or_else(|e| e.into_inner()).push(SeenSnap {
                    phase: "b",
                    tools: tools_of(&snap),
                    same: true,
                });
                runtime
                    .commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                    .await
            })
        }),
    );

    let snapshots = define_task(TaskDefinition {
        name: "test.snapshots".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(AbCheckpoint::A)),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });

    let opened = open_root(vec![snapshots.clone().into_any()], OpenTasksOptions::default(), None).await;
    let id = opened
        .root
        .commit(
            move |tx| async move {
                tx.create_task(
                    &snapshots,
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
        .expect("create snapshots");
    opened.harness.resume().expect("resume");
    entered.wait().await.expect("entered");
    add_tool(
        &opened.registry,
        define_tool(ToolRegistration {
            name: "late".to_owned(),
            description: "late".to_owned(),
            parameters: Type::object([] as [(&str, TSchema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(|_, _, _| Box::pin(async { Ok(ToolExecutionResult::default()) })),
        }),
        None,
    )
    .expect("add late");
    phase_gate.resolve(());
    opened.harness.wait_for_task(id, context()).await.expect("wait");
    assert_eq!(
        *seen.lock().unwrap_or_else(|e| e.into_inner()),
        vec![
            SeenSnap {
                phase: "a",
                tools: vec![],
                same: true,
            },
            SeenSnap {
                phase: "b",
                tools: vec!["late".to_owned()],
                same: true,
            },
        ]
    );
    opened.harness.close(context()).await.expect("close");
}

#[derive(Clone)]
struct PingHooks {
    ping: Arc<dyn Fn() + Send + Sync>,
}

#[tokio::test]
async fn resolves_the_agent_at_first_use_in_a_phase_from_its_snapshot_keeps_it_for_the_phase_and_anew_at_the_next() {
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct SeenPhase {
        phase: &'static str,
        pings: Vec<String>,
        thinking: ModelThinkingLevel,
        same: Option<bool>,
    }
    let pings = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = Arc::new(Mutex::new(Vec::<SeenPhase>::new()));
    let before_use = deferred::<()>();
    let after_use = deferred::<()>();
    let waiting_before_use = deferred::<()>();
    let waiting_after_use = deferred::<()>();

    let run_ping = |pings: &Arc<Mutex<Vec<String>>>, runtime: &TaskRuntimeHandle<(), AbCheckpoint, (), PingHooks>| {
        let pings = Arc::clone(pings);
        let runtime = runtime.clone();
        async move {
            pings.lock().unwrap_or_else(|e| e.into_inner()).clear();
            let hooks = runtime.hooks("test.agent-phases")?;
            hooks
                .each(|h: &PingHooks, _api| {
                    (h.ping)();
                    Box::pin(async { Ok(()) })
                })
                .await?;
            Ok::<Vec<String>, pi_js::Error>(pings.lock().unwrap_or_else(|e| e.into_inner()).clone())
        }
    };

    let pings_a = Arc::clone(&pings);
    let seen_a = Arc::clone(&seen);
    let before_a = before_use.clone();
    let after_a = after_use.clone();
    let wait_before_a = waiting_before_use.clone();
    let wait_after_a = waiting_after_use.clone();

    let pings_b = Arc::clone(&pings);
    let seen_b = Arc::clone(&seen);

    let mut phases: IndexMap<String, TypedPhaseHandler<(), AbCheckpoint, (), PingHooks>> = IndexMap::new();
    phases.insert(
        "a".to_owned(),
        Arc::new(move |_task, runtime, ctx| {
            let pings_a = Arc::clone(&pings_a);
            let seen_a = Arc::clone(&seen_a);
            let before_a = before_a.clone();
            let after_a = after_a.clone();
            let wait_before_a = wait_before_a.clone();
            let wait_after_a = wait_after_a.clone();
            Box::pin(async move {
                wait_before_a.resolve(());
                before_a.wait().await?;
                let first = runtime.agent(ctx.clone()).await?;
                let first_pings = run_ping(&pings_a, &runtime).await?;
                seen_a.lock().unwrap_or_else(|e| e.into_inner()).push(SeenPhase {
                    phase: "a",
                    pings: first_pings,
                    thinking: first.thinking_level,
                    same: None,
                });
                wait_after_a.resolve(());
                after_a.wait().await?;
                let second = runtime.agent(ctx.clone()).await?;
                let same = second.thinking_level == first.thinking_level
                    && second.extensions.iter().map(|e| &e.name).collect::<Vec<_>>()
                        == first.extensions.iter().map(|e| &e.name).collect::<Vec<_>>();
                let second_pings = run_ping(&pings_a, &runtime).await?;
                seen_a.lock().unwrap_or_else(|e| e.into_inner()).push(SeenPhase {
                    phase: "a",
                    pings: second_pings,
                    thinking: second.thinking_level,
                    same: Some(same),
                });
                runtime
                    .commit(
                        |_tx, _c| {
                            Box::pin(async {
                                Ok(Some(NextTaskState::Running {
                                    checkpoint: AbCheckpoint::B,
                                }))
                            })
                        },
                        ctx,
                    )
                    .await
            })
        }),
    );
    phases.insert(
        "b".to_owned(),
        Arc::new(move |_task, runtime, ctx| {
            let pings_b = Arc::clone(&pings_b);
            let seen_b = Arc::clone(&seen_b);
            Box::pin(async move {
                let agent = runtime.agent(ctx.clone()).await?;
                let pings = run_ping(&pings_b, &runtime).await?;
                seen_b.lock().unwrap_or_else(|e| e.into_inner()).push(SeenPhase {
                    phase: "b",
                    pings,
                    thinking: agent.thinking_level,
                    same: None,
                });
                runtime
                    .commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                    .await
            })
        }),
    );

    let phases_task = define_task(TaskDefinition {
        name: "test.agent-phases".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(AbCheckpoint::A)),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });

    let opened = open_root(vec![phases_task.clone().into_any()], OpenTasksOptions::default(), None).await;
    let p_before = Arc::clone(&pings);
    add_hooks(
        &opened.registry,
        &phases_task,
        PingHooks {
            ping: Arc::new(move || {
                p_before
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push("before".to_owned())
            }),
        },
        None,
    )
    .expect("add before hook");

    let phases_c = phases_task.clone();
    let id = opened
        .root
        .commit(
            move |tx| async move {
                tx.create_task(
                    &phases_c,
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
        .expect("create");
    opened.harness.resume().expect("resume");

    waiting_before_use.wait().await.expect("waiting before use");
    opened
        .root
        .configure(
            AgentChange {
                thinking_level: FieldChange::Set(ModelThinkingLevel::Low),
                ..Default::default()
            },
            context(),
        )
        .await
        .expect("configure low");
    before_use.resolve(());

    waiting_after_use.wait().await.expect("waiting after use");
    let p_late = Arc::clone(&pings);
    add_hooks(
        &opened.registry,
        &phases_task,
        PingHooks {
            ping: Arc::new(move || p_late.lock().unwrap_or_else(|e| e.into_inner()).push("late".to_owned())),
        },
        None,
    )
    .expect("add late hook");
    opened
        .root
        .configure(
            AgentChange {
                thinking_level: FieldChange::Set(ModelThinkingLevel::High),
                ..Default::default()
            },
            context(),
        )
        .await
        .expect("configure high");
    after_use.resolve(());

    opened.harness.wait_for_task(id, context()).await.expect("wait");
    assert_eq!(
        *seen.lock().unwrap_or_else(|e| e.into_inner()),
        vec![
            SeenPhase {
                phase: "a",
                pings: vec!["before".to_owned()],
                thinking: ModelThinkingLevel::Low,
                same: None,
            },
            SeenPhase {
                phase: "a",
                pings: vec!["before".to_owned()],
                thinking: ModelThinkingLevel::Low,
                same: Some(true),
            },
            SeenPhase {
                phase: "b",
                pings: vec!["before".to_owned(), "late".to_owned()],
                thinking: ModelThinkingLevel::High,
                same: None,
            },
        ]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn rejects_an_agent_wait_whose_caller_context_is_cancelled_without_affecting_the_phases_resolution() {
    let cancelled = Arc::new(Mutex::new(None::<String>));
    let resolved = Arc::new(Mutex::new(None::<ModelThinkingLevel>));
    let cancelled_c = Arc::clone(&cancelled);
    let resolved_c = Arc::clone(&resolved);

    let agent_task = one_step::<(), _, _>("test.agent-cancel", move |_t, rt, ctx| {
        let cancelled_c = Arc::clone(&cancelled_c);
        let resolved_c = Arc::clone(&resolved_c);
        async move {
            let caller = AbortController::new();
            caller.abort(Some(AbortReason::error("Error", "caller gone")));
            let err = match rt.agent(with_controller_abort(&caller)).await {
                Ok(_) => panic!("cancelled agent()"),
                Err(err) => err,
            };
            *cancelled_c.lock().unwrap_or_else(|e| e.into_inner()) = Some(err.to_string());
            let agent = rt.agent(ctx.clone()).await?;
            *resolved_c.lock().unwrap_or_else(|e| e.into_inner()) = Some(agent.thinking_level);
            rt.commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                .await
        }
    });

    let opened = open_root(vec![agent_task.clone().into_any()], OpenTasksOptions::default(), None).await;
    let id = start(&opened.root, &agent_task, false).await;
    opened.harness.resume().expect("resume");
    opened.harness.wait_for_task(id, context()).await.expect("wait");
    assert!(
        cancelled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_deref()
            .is_some_and(|s| s.contains("caller gone"))
    );
    assert_eq!(
        *resolved.lock().unwrap_or_else(|e| e.into_inner()),
        Some(ModelThinkingLevel::Off)
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn observes_a_failed_agent_resolution_whose_only_caller_stopped_waiting() {
    let storage = Arc::new(FailingAgentDocStorage::new());
    let harness_slot = Arc::new(Mutex::new(None::<Harness>));
    let resolved = Arc::new(Mutex::new(None::<String>));

    let storage_c = Arc::clone(&storage);
    let harness_slot_c = Arc::clone(&harness_slot);
    let resolved_c = Arc::clone(&resolved);

    let agent_task = one_step::<(), _, _>("test.agent-failure", move |_t, rt, ctx| {
        let storage_c = Arc::clone(&storage_c);
        let harness_slot_c = Arc::clone(&harness_slot_c);
        let resolved_c = Arc::clone(&resolved_c);
        async move {
            let harness_ref = harness_slot_c
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .expect("harness stored");
            harness_ref.unload_documents().await.expect("unload documents");
            let caller = AbortController::new();
            caller.abort(Some(AbortReason::error("Error", "caller gone")));
            storage_c.set_fail(true);
            let _ = rt.agent(with_controller_abort(&caller)).await;
            flush().await;
            let err = match rt.agent(ctx.clone()).await {
                Ok(_) => panic!("should observe failed resolution"),
                Err(err) => err,
            };
            *resolved_c.lock().unwrap_or_else(|e| e.into_inner()) = Some(err.to_string());
            rt.commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                .await
        }
    });

    let opened = open_root(
        vec![agent_task.clone().into_any()],
        OpenTasksOptions::default(),
        Some(storage.clone() as Arc<dyn Storage>),
    )
    .await;
    *harness_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(opened.harness.clone());
    let id = start(&opened.root, &agent_task, false).await;
    opened.harness.resume().expect("resume");
    opened.harness.wait_for_task(id, context()).await.expect("wait");
    assert!(
        resolved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_deref()
            .is_some_and(|s| s.contains("agent doc read broke"))
    );
    opened.harness.close(context()).await.expect("close");
}

// ---- describe("task runtime") ----

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct TextDoc {
    text: String,
}

#[tokio::test]
async fn rejects_runtime_operations_after_the_invocation_ends_and_stops_its_watches() {
    let notes: SessionDocToken<TextDoc> = define_session_doc(DocDefinitionInput {
        kind: "test.task-notes",
        version: 1,
        initial: TextDoc::default,
        migrate: None,
        checkpoint_when: None,
    })
    .expect("notes");
    let absent_doc: SessionDocToken<TextDoc> = define_session_doc(DocDefinitionInput {
        kind: "test.task-absent",
        version: 1,
        initial: TextDoc::default,
        migrate: None,
        checkpoint_when: None,
    })
    .expect("absent");

    let captured = Arc::new(Mutex::new(None::<StepRuntime<()>>));
    let watch_holder = Arc::new(Mutex::new(None::<pi_durable::types::DocumentWatch<TextDoc>>));
    let delivered = Arc::new(Mutex::new(Vec::<String>::new()));
    let absent_was_none = Arc::new(Mutex::new(false));
    let handler_context = Arc::new(Mutex::new(None::<Context>));

    let notes_w = notes.clone();
    let absent_w = absent_doc.clone();
    let cap_w = Arc::clone(&captured);
    let wh_w = Arc::clone(&watch_holder);
    let del_w = Arc::clone(&delivered);
    let abs_w = Arc::clone(&absent_was_none);
    let hctx_w = Arc::clone(&handler_context);

    let watcher = one_step::<(), _, _>("test.watcher", move |_t, rt, ctx| {
        let notes_w = notes_w.clone();
        let absent_w = absent_w.clone();
        let cap_w = Arc::clone(&cap_w);
        let wh_w = Arc::clone(&wh_w);
        let del_w = Arc::clone(&del_w);
        let abs_w = Arc::clone(&abs_w);
        let hctx_w = Arc::clone(&hctx_w);
        async move {
            *cap_w.lock().unwrap_or_else(|e| e.into_inner()) = Some(rt.clone());
            *hctx_w.lock().unwrap_or_else(|e| e.into_inner()) = Some(ctx.clone());
            let absent_watch = rt.watch_doc_session(&absent_w, ctx.clone()).await?;
            *abs_w.lock().unwrap_or_else(|e| e.into_inner()) = absent_watch.is_none();
            let watch = rt.watch_doc_session(&notes_w, ctx.clone()).await?.expect("notes watch");
            let del_cb = Arc::clone(&del_w);
            watch.start(Arc::new(move |val, _ops, _ctx| {
                let text = val.map_or_else(|| "retired".to_owned(), |v| v.text);
                del_cb.lock().unwrap_or_else(|e| e.into_inner()).push(text);
                Box::pin(async { Ok(()) })
            }))?;
            *wh_w.lock().unwrap_or_else(|e| e.into_inner()) = Some(watch);
            rt.commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                .await
        }
    });

    let opened = open_root(vec![watcher.clone().into_any()], OpenTasksOptions::default(), None).await;
    let notes_init = notes.clone();
    opened
        .harness
        .commit(
            move |tx| async move {
                tx.doc_session(&notes_init).await?.update(|d| {
                    d.text = "hello".to_owned();
                })
            },
            context(),
        )
        .await
        .expect("init notes");
    let id = start(&opened.root, &watcher, false).await;
    opened.harness.resume().expect("resume");
    opened.harness.wait_for_task(id, context()).await.expect("wait");
    assert!(*absent_was_none.lock().unwrap_or_else(|e| e.into_inner()));
    let watch = watch_holder
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("watch");
    assert_eq!(watch.closed().await, WatchEnd::Stopped);

    let notes_after = notes.clone();
    opened
        .harness
        .commit(
            move |tx| async move {
                tx.doc_session(&notes_after).await?.update(|d| {
                    d.text = "after".to_owned();
                })
            },
            context(),
        )
        .await
        .expect("commit after");
    flush().await;
    assert!(delivered.lock().unwrap_or_else(|e| e.into_inner()).is_empty());

    let rt = captured.lock().unwrap_or_else(|e| e.into_inner()).take().expect("rt");
    let hctx = handler_context
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("hctx");
    assert!(
        rt.commit(|_tx, _c| Box::pin(async { Ok(None) }), context())
            .await
            .expect_err("commit")
            .to_string()
            .contains("invocation has ended")
    );
    assert!(
        rt.memo_get::<JsonValue>("x", context())
            .await
            .expect_err("memo_get")
            .to_string()
            .contains("invocation has ended")
    );
    assert!(
        rt.memo_set("x", &1_i64, context())
            .await
            .expect_err("memo_set")
            .to_string()
            .contains("invocation has ended")
    );
    assert!(
        rt.sleep(0, context())
            .await
            .expect_err("sleep")
            .to_string()
            .contains("invocation has ended")
    );
    assert!(
        match rt.watch_doc_session(&notes, context()).await {
            Ok(_) => panic!("watchDoc"),
            Err(err) => err,
        }
        .to_string()
        .contains("invocation has ended")
    );
    assert!(
        match rt.agent(hctx).await {
            Ok(_) => panic!("agent"),
            Err(err) => err,
        }
        .to_string()
        .contains("invocation has ended")
    );
    assert!(rt.now().expect_err("now").to_string().contains("invocation has ended"));
    rt.report(pi_js::Error::msg("late"));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reads_committed_documents_and_context_through_the_runtime_and_forwards_the_clock_and_reports() {
    let notes: RewindableConversationDocToken<TextDoc> = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "test.runtime-notes",
            version: 1,
            initial: TextDoc::default,
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("notes");
    let captured = Arc::new(Mutex::new(None::<StepRuntime<()>>));
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));

    let notes_r = notes.clone();
    let cap_r = Arc::clone(&captured);
    let seen_r = Arc::clone(&seen);
    let reader = one_step::<(), _, _>("test.reader", move |_t, rt, ctx| {
        let notes_r = notes_r.clone();
        let cap_r = Arc::clone(&cap_r);
        let seen_r = Arc::clone(&seen_r);
        async move {
            *cap_r.lock().unwrap_or_else(|e| e.into_inner()) = Some(rt.clone());
            let cid = rt.conversation_id()?;
            let view = rt.context(cid, ctx.clone(), None).await?;
            let first = view.entries[0].id;
            let second = view.entries[1].id;
            let doc_reader = rt.read()?;
            let snap_now = doc_reader
                .snapshot_conversation(&notes_r, cid, ctx.clone())
                .await?
                .map(|v| v.text)
                .unwrap_or_default();
            let snap_as_of = doc_reader
                .snapshot_as_of(&notes_r, cid, first, ctx.clone())
                .await?
                .map(|v| v.text)
                .unwrap_or_default();
            let len1 = rt.context(cid, ctx.clone(), Some(first)).await?.entries.len();
            let len2 = rt.context(cid, ctx.clone(), Some(second)).await?.messages.len();
            let now = rt.now()?;
            rt.report(pi_js::Error::msg("reported"));
            {
                let mut out = seen_r.lock().unwrap_or_else(|e| e.into_inner());
                out.push(snap_now);
                out.push(snap_as_of);
                out.push(len1.to_string());
                out.push(len2.to_string());
                out.push(now.to_string());
            }
            rt.commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                .await
        }
    });

    let opened = open_root(
        vec![reader.clone().into_any()],
        OpenTasksOptions {
            now: Some(Arc::new(|| 1234)),
            ..Default::default()
        },
        None,
    )
    .await;
    for text in ["one", "two"] {
        let notes_c = notes.clone();
        let root_id = opened.root.id();
        opened
            .root
            .commit(
                move |tx| async move {
                    tx.doc_conversation(&notes_c, root_id).await?.update(|d| {
                        d.text = text.to_owned();
                    })?;
                    tx.append_entry(
                        root_id,
                        EntryDraft {
                            kind: "message".to_owned(),
                            model: Some(vec![pi_ai::types::Message::User(user(text))]),
                            data: None,
                            head: None,
                            edits: None,
                        },
                    )
                    .await?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("seed");
    }
    let id = start(&opened.root, &reader, false).await;
    opened.harness.resume().expect("resume");
    opened.harness.wait_for_task(id, context()).await.expect("wait");
    assert_eq!(
        *seen.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["two", "one", "1", "2", "1234"]
    );
    let report_msgs: Vec<String> = opened
        .reports
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(report_msgs, vec!["reported"]);
    flush().await;
    let rt = captured.lock().unwrap_or_else(|e| e.into_inner()).take().expect("rt");
    assert!(
        rt.read()
            .expect("read")
            .snapshot_conversation(&notes, opened.root.id(), context())
            .await
            .expect_err("snapshot after end")
            .to_string()
            .contains("invocation has ended")
    );
    assert!(
        rt.context(opened.root.id(), context(), None)
            .await
            .expect_err("context after end")
            .to_string()
            .contains("invocation has ended")
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn orders_runtime_commits_against_the_step_one_queued_before_it_lands_one_after_it_rejects() {
    let storage = Arc::new(ControlledStorage::new());
    let before = Arc::new(Mutex::new(None::<Tracked<pi_js::Result<()>>>));
    let after = Arc::new(Mutex::new(None::<Tracked<pi_js::Result<()>>>));
    let held_slot = Arc::new(Mutex::new(None::<support::session_support::Gate>));
    let harness_slot = Arc::new(Mutex::new(None::<Harness>));

    let st_d = Arc::clone(&storage);
    let bef_d = Arc::clone(&before);
    let aft_d = Arc::clone(&after);
    let held_d = Arc::clone(&held_slot);
    let h_d = Arc::clone(&harness_slot);

    let detached = one_step::<String, _, _>("test.detached", move |_t, rt, _ctx| {
        let st_d = Arc::clone(&st_d);
        let bef_d = Arc::clone(&bef_d);
        let aft_d = Arc::clone(&aft_d);
        let held_d = Arc::clone(&held_d);
        let h_d = Arc::clone(&h_d);
        async move {
            let held = st_d.hold_commits();
            *held_d.lock().unwrap_or_else(|e| e.into_inner()) = Some(held);
            let h = h_d.lock().unwrap_or_else(|e| e.into_inner()).clone().expect("h");
            let cid = rt.conversation_id()?;
            tokio::spawn(async move {
                let _ = h
                    .commit(
                        move |tx| async move {
                            tx.append_entry(
                                cid,
                                EntryDraft {
                                    kind: "blocker".to_owned(),
                                    model: None,
                                    data: None,
                                    head: None,
                                    edits: None,
                                },
                            )
                            .await?;
                            Ok(())
                        },
                        context(),
                    )
                    .await;
            });
            tokio::task::yield_now().await;
            let rt_before = rt.clone();
            *bef_d.lock().unwrap_or_else(|e| e.into_inner()) = Some(track(async move {
                rt_before
                    .commit(
                        |_tx, _c| Box::pin(async { Ok(Some(completed("before".to_owned()))) }),
                        context(),
                    )
                    .await
            }));
            tokio::task::yield_now().await;
            let rt_after = rt.clone();
            let aft_cb = Arc::clone(&aft_d);
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                let fut = track(async move {
                    rt_after
                        .commit(
                            |_tx, _c| Box::pin(async { Ok(Some(completed("after".to_owned()))) }),
                            context(),
                        )
                        .await
                });
                *aft_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(fut);
            });
            Ok(())
        }
    });

    let opened = open_root(
        vec![detached.clone().into_any()],
        OpenTasksOptions::default(),
        Some(Arc::clone(&storage) as Arc<dyn Storage>),
    )
    .await;
    *harness_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(opened.harness.clone());
    let id = start(&opened.root, &detached, false).await;
    opened.harness.resume().expect("resume");
    eventually_sync(|| held_slot.lock().unwrap_or_else(|e| e.into_inner()).is_some()).await;
    let held = held_slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("held");
    held.entered().await;
    eventually_sync(|| after.lock().unwrap_or_else(|e| e.into_inner()).is_some()).await;
    held.release();
    before
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("before")
        .wait()
        .await
        .expect("before ok");
    assert_eq!(
        opened.harness.wait_for_task(id, context()).await.expect("wait").outcome,
        TaskOutcome::Completed {
            result: "before".to_owned()
        }
    );
    let after_err = after
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("after")
        .wait()
        .await
        .expect_err("after rejects");
    let msg = after_err.to_string();
    assert!(
        msg.contains("invocation has ended") || msg.contains("is terminal"),
        "unexpected error: {msg}"
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn stops_a_watch_whose_acquisition_finishes_after_the_invocation_ended() {
    let notes: SessionDocToken<TextDoc> = define_session_doc(DocDefinitionInput {
        kind: "test.late-watch",
        version: 1,
        initial: TextDoc::default,
        migrate: None,
        checkpoint_when: None,
    })
    .expect("notes");
    let storage = Arc::new(ControlledStorage::new());
    let watching = Arc::new(Mutex::new(None::<Tracked<String>>));
    let held_slot = Arc::new(Mutex::new(None::<support::session_support::Gate>));
    let harness_slot = Arc::new(Mutex::new(None::<Harness>));

    let notes_l = notes.clone();
    let st_l = Arc::clone(&storage);
    let watch_l = Arc::clone(&watching);
    let held_l = Arc::clone(&held_slot);
    let h_l = Arc::clone(&harness_slot);

    let late = one_step::<(), _, _>("test.late-watch", move |_t, rt, _ctx| {
        let notes_l = notes_l.clone();
        let st_l = Arc::clone(&st_l);
        let watch_l = Arc::clone(&watch_l);
        let held_l = Arc::clone(&held_l);
        let h_l = Arc::clone(&h_l);
        async move {
            let held = st_l.hold_commits();
            *held_l.lock().unwrap_or_else(|e| e.into_inner()) = Some(held);
            let h = h_l.lock().unwrap_or_else(|e| e.into_inner()).clone().expect("h");
            let cid = rt.conversation_id()?;
            tokio::spawn(async move {
                let _ = h
                    .commit(
                        move |tx| async move {
                            tx.append_entry(
                                cid,
                                EntryDraft {
                                    kind: "blocker".to_owned(),
                                    model: None,
                                    data: None,
                                    head: None,
                                    edits: None,
                                },
                            )
                            .await?;
                            Ok(())
                        },
                        context(),
                    )
                    .await;
            });
            let rt_l = rt.clone();
            let notes_sub = notes_l.clone();
            let watch_cb = Arc::clone(&watch_l);
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                let fut = track(async move {
                    match rt_l.watch_doc_session(&notes_sub, context()).await {
                        Ok(_) => "ok".to_owned(),
                        Err(e) => e.to_string(),
                    }
                });
                *watch_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(fut);
            });
            Ok(())
        }
    });

    let opened = open_root(
        vec![late.clone().into_any()],
        OpenTasksOptions::default(),
        Some(Arc::clone(&storage) as Arc<dyn Storage>),
    )
    .await;
    *harness_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(opened.harness.clone());
    let notes_init = notes.clone();
    opened
        .harness
        .commit(
            move |tx| async move {
                tx.doc_session(&notes_init).await?.update(|d| {
                    d.text = "x".to_owned();
                })
            },
            context(),
        )
        .await
        .expect("init");
    let id = start(&opened.root, &late, false).await;
    opened.harness.resume().expect("resume");
    eventually_sync(|| held_slot.lock().unwrap_or_else(|e| e.into_inner()).is_some()).await;
    let held = held_slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("held");
    held.entered().await;
    eventually_sync(|| watching.lock().unwrap_or_else(|e| e.into_inner()).is_some()).await;
    held.release();
    let msg = watching
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("watching")
        .wait()
        .await;
    assert!(msg.contains("invocation has ended"), "got: {msg}");
    assert!(matches!(
        opened.harness.wait_for_task(id, context()).await.expect("wait").outcome,
        TaskOutcome::Faulted { .. }
    ));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn sleeps_until_the_harness_clock_reaches_the_deadline_rechecking_after_each_timer() {
    let clock = Arc::new(AtomicI64::new(1_000));
    let woke = deferred::<()>();

    let woke_c = woke.clone();
    let sleeper = one_step::<(), _, _>("test.sleeper", move |_t, rt, ctx| {
        let woke_c = woke_c.clone();
        async move {
            rt.sleep(900, ctx.clone()).await?;
            rt.sleep(1_005, ctx.clone()).await?;
            woke_c.resolve(());
            rt.commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                .await
        }
    });

    let clock_c = Arc::clone(&clock);
    let opened = open_root(
        vec![sleeper.clone().into_any()],
        OpenTasksOptions {
            now: Some(Arc::new(move || clock_c.load(Ordering::Acquire))),
            ..Default::default()
        },
        None,
    )
    .await;
    let id = start(&opened.root, &sleeper, false).await;
    opened.harness.resume().expect("resume");
    tokio::time::sleep(Duration::from_millis(30)).await;
    let woke_fut = track(woke.promise());
    assert!(!settled(&woke_fut).await);
    clock.store(1_005, Ordering::Release);
    opened.harness.wait_for_task(id, context()).await.expect("wait");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn rejects_a_sleep_when_the_invocation_is_signalled_or_the_sleeps_own_context_is_cancelled() {
    let results = Arc::new(Mutex::new(Vec::<String>::new()));
    let sleeping = deferred::<()>();

    let res_s = Arc::clone(&results);
    let slp_s = sleeping.clone();
    let signalled = one_step::<(), _, _>("test.sleep-signalled", move |_t, rt, ctx| {
        let res_s = Arc::clone(&res_s);
        let slp_s = slp_s.clone();
        async move {
            slp_s.resolve(());
            let until = pi_js::time::now_ms() + 60_000;
            if let Err(err) = rt.sleep(until, ctx).await {
                let kind = if err.is_abort() { "AbortError" } else { "Other" };
                res_s
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!("signalled:{kind}"));
                return Err(err);
            }
            Ok(())
        }
    });

    let res_c = Arc::clone(&results);
    let cancelled = one_step::<(), _, _>("test.sleep-cancelled", move |_t, rt, ctx| {
        let res_c = Arc::clone(&res_c);
        async move {
            let controller = AbortController::new();
            let until = pi_js::time::now_ms() + 60_000;
            let rt_c = rt.clone();
            let sig = controller.signal();
            let sleep_fut = rt_c.sleep(until, with_abort_signal(sig, ctx.clone()));
            controller.abort(Some(AbortReason::error("Error", "stop sleeping")));
            if let Err(err) = sleep_fut.await {
                res_c
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!("cancelled:{}", err.message()));
            }
            rt.commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                .await
        }
    });

    let opened = open_root(
        vec![signalled.clone().into_any(), cancelled.clone().into_any()],
        OpenTasksOptions::default(),
        None,
    )
    .await;
    let signalled_id = start(&opened.root, &signalled, false).await;
    let cancelled_id = start(&opened.root, &cancelled, false).await;
    opened.harness.resume().expect("resume");
    opened
        .harness
        .wait_for_task(cancelled_id, context())
        .await
        .expect("wait cancelled");
    sleeping.wait().await.expect("sleeping");
    opened
        .harness
        .abort_task(signalled_id.erased(), context())
        .await
        .expect("abort signalled");
    assert_eq!(
        opened
            .harness
            .wait_for_task(signalled_id, context())
            .await
            .expect("wait signalled")
            .outcome,
        TaskOutcome::Aborted {
            reason: Some("test".to_owned()),
            result: None,
        }
    );
    assert_eq!(
        *results.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["cancelled:stop sleeping", "signalled:AbortError"]
    );
    opened.harness.close(context()).await.expect("close");
}

// ---- describe("task scheduling") ----

#[tokio::test]
async fn retries_reservation_on_the_next_wakeup_after_a_rejected_reservation_commit() {
    let runs = Arc::new(AtomicUsize::new(0));
    let runs_c = Arc::clone(&runs);
    let once = one_step::<(), _, _>("test.once", move |_t, rt, ctx| {
        let runs_c = Arc::clone(&runs_c);
        async move {
            runs_c.fetch_add(1, Ordering::SeqCst);
            rt.commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                .await
        }
    });
    let storage = Arc::new(ControlledStorage::new());
    let opened = open_root(
        vec![once.clone().into_any()],
        OpenTasksOptions::default(),
        Some(Arc::clone(&storage) as Arc<dyn Storage>),
    )
    .await;
    let id = start(&opened.root, &once, false).await;
    storage.fail_next_commit(StorageRejected::new("busy").into_error());
    opened.harness.resume().expect("resume");
    eventually_sync(|| opened.reports.lock().unwrap_or_else(|e| e.into_inner()).len() == 1).await;
    assert_eq!(
        opened
            .harness
            .get_task(id, context())
            .await
            .expect("get")
            .expect("present")
            .state
            .status(),
        TaskStatus::Pending
    );
    add_tool(
        &opened.registry,
        define_tool(ToolRegistration {
            name: "wake".to_owned(),
            description: "wake".to_owned(),
            parameters: Type::object([] as [(&str, TSchema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(|_, _, _| Box::pin(async { Ok(ToolExecutionResult::default()) })),
        }),
        None,
    )
    .expect("add wake");
    opened.harness.wait_for_task(id, context()).await.expect("wait");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_a_wakeup_that_arrives_while_a_rejected_reservation_commit_is_in_storage() {
    let first = one_step::<(), _, _>("test.wake-first", |_t, rt, ctx| async move {
        rt.commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), ctx)
            .await
    });
    let late = one_step::<(), _, _>("test.wake-late", |_t, rt, ctx| async move {
        rt.commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), ctx)
            .await
    });
    let storage = Arc::new(ControlledStorage::new());
    let opened = open_root(
        vec![first.clone().into_any()],
        OpenTasksOptions::default(),
        Some(Arc::clone(&storage) as Arc<dyn Storage>),
    )
    .await;
    let first_id = start(&opened.root, &first, false).await;
    let late_id = start(&opened.root, &late, false).await;
    let held = storage.hold_commits();
    storage.fail_next_commit(StorageRejected::new("busy").into_error());
    opened.harness.resume().expect("resume");
    held.entered().await;
    add_task(&opened.registry, late.into_any(), None).expect("add late");
    held.release();
    opened.harness.wait_for_task(first_id, context()).await.expect("wait 1");
    opened.harness.wait_for_task(late_id, context()).await.expect("wait 2");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reruns_a_task_whose_fault_write_was_rejected() {
    let runs = Arc::new(AtomicUsize::new(0));
    let storage = Arc::new(ControlledStorage::new());
    let runs_c = Arc::clone(&runs);
    let st_c = Arc::clone(&storage);
    let throws = one_step::<(), _, _>("test.rejected-fault", move |_t, _rt, _ctx| {
        let runs_c = Arc::clone(&runs_c);
        let st_c = Arc::clone(&st_c);
        async move {
            let n = runs_c.fetch_add(1, Ordering::SeqCst) + 1;
            if n == 1 {
                st_c.fail_next_commit(StorageRejected::new("busy").into_error());
            }
            Err(pi_js::Error::msg("boom"))
        }
    });
    let opened = open_root(
        vec![throws.clone().into_any()],
        OpenTasksOptions::default(),
        Some(Arc::clone(&storage) as Arc<dyn Storage>),
    )
    .await;
    let id = start(&opened.root, &throws, false).await;
    opened.harness.resume().expect("resume");
    assert_eq!(
        opened.harness.wait_for_task(id, context()).await.expect("wait").outcome,
        TaskOutcome::Faulted {
            error: TaskOutcomeError {
                message: "boom".to_owned(),
                detail: None,
            },
        }
    );
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    let report_strs: Vec<String> = opened
        .reports
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(report_strs, vec!["busy"]);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn waits_for_harness_and_conversation_idleness_counting_blocked_work_and_ignoring_background_tasks() {
    let gates = Arc::new(Mutex::new(IndexMap::<TaskId<()>, Deferred<()>>::new()));
    let gates_c = Arc::clone(&gates);
    let gated = one_step::<(), _, _>("test.gated", move |task, rt, ctx| {
        let gates_c = Arc::clone(&gates_c);
        async move {
            let d = deferred::<()>();
            gates_c
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(task.id, d.clone());
            d.wait().await?;
            rt.commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                .await
        }
    });

    let opened = open_root(vec![gated.clone().into_any()], OpenTasksOptions::default(), None).await;
    opened.harness.wait_for_idle(context()).await.expect("initial idle");
    let other = opened
        .harness
        .create_conversation(
            pi_durable::harness::types::ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("create other");
    let foreground = start(&opened.root, &gated, false).await;
    let background = start(&opened.root, &gated, true).await;
    let elsewhere = start(&other, &gated, false).await;

    let cancelled = AbortController::new();
    let h_c = opened.harness.clone();
    let cancel_sig = cancelled.signal();
    let cancelled_wait = track(async move { h_c.wait_for_idle(with_abort_signal(cancel_sig, context())).await });
    cancelled.abort(Some(AbortReason::error("Error", "stop waiting")));
    assert!(
        cancelled_wait
            .wait()
            .await
            .expect_err("cancelled")
            .to_string()
            .contains("stop waiting")
    );
    let aborted_ctrl = AbortController::new();
    aborted_ctrl.abort(Some(AbortReason::error("Error", "already cancelled")));
    assert!(
        opened
            .root
            .wait_for_idle(with_controller_abort(&aborted_ctrl))
            .await
            .expect_err("already aborted")
            .to_string()
            .contains("already cancelled")
    );

    opened.harness.resume().expect("resume");
    eventually_sync(|| gates.lock().unwrap_or_else(|e| e.into_inner()).len() == 3).await;
    let root_c = opened.root.clone();
    let root_idle = track(async move { root_c.wait_for_idle(context()).await });
    let harness_c = opened.harness.clone();
    let harness_idle = track(async move { harness_c.wait_for_idle(context()).await });

    gates
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&foreground)
        .expect("fg gate")
        .resolve(());
    root_idle.wait().await.expect("root idle");
    assert!(!settled(&harness_idle).await);
    gates
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&elsewhere)
        .expect("elsewhere gate")
        .resolve(());
    harness_idle.wait().await.expect("harness idle");
    assert_eq!(
        opened
            .harness
            .get_task(background, context())
            .await
            .expect("get bg")
            .expect("present")
            .state
            .status(),
        TaskStatus::Running
    );
    gates
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&background)
        .expect("bg gate")
        .resolve(());
    opened
        .harness
        .wait_for_task(background, context())
        .await
        .expect("wait bg");
    opened.harness.close(context()).await.expect("close");
    assert!(
        opened
            .harness
            .wait_for_idle(context())
            .await
            .expect_err("closed")
            .to_string()
            .contains("closed")
    );
    assert!(
        opened
            .root
            .wait_for_idle(context())
            .await
            .expect_err("closed")
            .to_string()
            .contains("closed")
    );
}

#[tokio::test]
async fn rejects_unknown_tasks_and_reports_terminal_tasks() {
    let done = one_step::<(), _, _>("test.quick", |_t, rt, ctx| async move {
        rt.commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), ctx)
            .await
    });
    let opened = open_root(vec![done.clone().into_any()], OpenTasksOptions::default(), None).await;
    let unknown = TaskId::<()>::new_unchecked(999_999);
    assert_eq!(opened.harness.get_task(unknown, context()).await.expect("get"), None);
    assert!(
        opened
            .harness
            .wait_for_task(unknown, context())
            .await
            .expect_err("unknown wait")
            .to_string()
            .contains("does not exist")
    );
    assert!(
        opened
            .harness
            .abort_task(unknown.erased(), context())
            .await
            .expect_err("unknown abort")
            .to_string()
            .contains("does not exist")
    );
    let id = start(&opened.root, &done, false).await;
    opened.harness.resume().expect("resume");
    opened.harness.wait_for_task(id, context()).await.expect("wait");
    let rec = opened
        .harness
        .get_task(id, context())
        .await
        .expect("get")
        .expect("present");
    assert_eq!(rec.state.status(), TaskStatus::Terminal);
    assert_eq!(
        opened
            .harness
            .abort_task(id.erased(), context())
            .await
            .expect("abort terminal"),
        AbortTaskResult::Terminal
    );
    opened.harness.close(context()).await.expect("close");
    assert!(
        opened
            .harness
            .resume()
            .expect_err("resume after close")
            .to_string()
            .contains("closed")
    );
}

#[tokio::test]
async fn rejects_task_waits_cancelled_or_closed_while_queued_on_the_line_and_pending_waits_on_close() {
    let gate = deferred::<()>();
    let blocking = gated_task("test.wait-close", gate.clone());
    let storage = Arc::new(ControlledStorage::new());
    let opened = open_root(
        vec![blocking.clone().into_any()],
        OpenTasksOptions::default(),
        Some(Arc::clone(&storage) as Arc<dyn Storage>),
    )
    .await;
    let id = start(&opened.root, &blocking, false).await;

    let held = storage.hold_commits();
    let h_c = opened.harness.clone();
    let root_id = opened.root.id();
    let blocker = track(async move {
        h_c.commit(
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
                .await?;
                Ok(())
            },
            context(),
        )
        .await
    });
    held.entered().await;
    let controller = AbortController::new();
    let h_w = opened.harness.clone();
    let sig = controller.signal();
    let cancelled_while_queued = track(async move { h_w.wait_for_task(id, with_abort_signal(sig, context())).await });
    controller.abort(Some(AbortReason::error("Error", "wait cancelled")));
    held.release();
    blocker.wait().await.expect("blocker");
    assert!(
        cancelled_while_queued
            .wait()
            .await
            .expect_err("wait cancelled")
            .to_string()
            .contains("wait cancelled")
    );

    let h1 = opened.harness.clone();
    let pending = track(async move { h1.wait_for_task(id, context()).await });
    let h2 = opened.harness.clone();
    let idle = track(async move { h2.wait_for_idle(context()).await });
    let r1 = opened.root.clone();
    let conv_idle = track(async move { r1.wait_for_idle(context()).await });
    flush().await;

    let held_again = storage.hold_commits();
    let h_c2 = opened.harness.clone();
    let blocker_again = track(async move {
        h_c2.commit(
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
                .await?;
                Ok(())
            },
            context(),
        )
        .await
    });
    held_again.entered().await;
    let h3 = opened.harness.clone();
    let closed_while_queued = track(async move { h3.wait_for_task(id, context()).await });
    let h4 = opened.harness.clone();
    let closing = track(async move { h4.close(context()).await });
    held_again.release();
    blocker_again.wait().await.expect("blocker again");
    assert!(
        closed_while_queued
            .wait()
            .await
            .expect_err("closed")
            .to_string()
            .contains("closed")
    );
    assert!(
        pending
            .wait()
            .await
            .expect_err("pending closed")
            .to_string()
            .contains("closed")
    );
    assert!(
        idle.wait()
            .await
            .expect_err("idle closed")
            .to_string()
            .contains("closed")
    );
    assert!(
        conv_idle
            .wait()
            .await
            .expect_err("conv idle closed")
            .to_string()
            .contains("closed")
    );
    gate.resolve(());
    closing.wait().await.expect("closing");
}

// ---- describe("task abort") ----

#[tokio::test]
async fn rejects_the_runs_commits_and_memo_writes_after_the_mark_and_settles_through_a_fresh_abort_invocation() {
    let reached = deferred::<()>();
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let memo_read = Arc::new(Mutex::new(None::<i64>));
    let abort_ptrs = Arc::new(Mutex::new(Vec::<usize>::new()));
    let run_ptr = Arc::new(Mutex::new(None::<usize>));

    let reached_r = reached.clone();
    let err_r = Arc::clone(&errors);
    let mread_r = Arc::clone(&memo_read);
    let rptr_r = Arc::clone(&run_ptr);
    let aptr_a = Arc::clone(&abort_ptrs);

    let marked = one_step_with_abort::<(), _, _, _, _>(
        "test.marked",
        move |_t, rt, ctx| {
            let reached_r = reached_r.clone();
            let err_r = Arc::clone(&err_r);
            let mread_r = Arc::clone(&mread_r);
            let rptr_r = Arc::clone(&rptr_r);
            async move {
                *rptr_r.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::as_ptr(&rt.erased) as *const () as usize);
                rt.memo_set("kept", &1_i64, ctx.clone()).await?;
                reached_r.resolve(());
                let sig = rt.signal()?;
                let _ = aborted::<()>(&sig).await;
                *mread_r.lock().unwrap_or_else(|e| e.into_inner()) = rt.memo_get::<i64>("kept", context()).await?;
                if let Err(err) = rt.memo_set("late", &2_i64, context()).await {
                    err_r.lock().unwrap_or_else(|e| e.into_inner()).push(err.to_string());
                }
                if let Err(err) = rt
                    .commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), context())
                    .await
                {
                    err_r.lock().unwrap_or_else(|e| e.into_inner()).push(err.to_string());
                }
                Ok(())
            }
        },
        move |_t, rt, ctx| {
            let aptr_a = Arc::clone(&aptr_a);
            async move {
                aptr_a
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(Arc::as_ptr(&rt.erased) as *const () as usize);
                rt.commit(
                    |_tx, current| {
                        Box::pin(async move {
                            assert!(current.abort_requested);
                            let mut expected = IndexMap::new();
                            expected.insert("kept".to_owned(), serde_json::json!(1));
                            assert_eq!(current.memos, Some(expected));
                            Ok(Some(aborted_with("mark")))
                        })
                    },
                    ctx,
                )
                .await
            }
        },
    );

    let opened = open_root(vec![marked.clone().into_any()], OpenTasksOptions::default(), None).await;
    let id = start(&opened.root, &marked, false).await;
    opened.harness.resume().expect("resume");
    reached.wait().await.expect("reached");
    assert_eq!(
        opened.harness.abort_task(id.erased(), context()).await.expect("abort"),
        AbortTaskResult::Marked
    );
    assert_eq!(
        opened.harness.wait_for_task(id, context()).await.expect("wait").outcome,
        TaskOutcome::Aborted {
            reason: Some("mark".to_owned()),
            result: None,
        }
    );
    assert_eq!(*memo_read.lock().unwrap_or_else(|e| e.into_inner()), Some(1));
    assert_eq!(
        *errors.lock().unwrap_or_else(|e| e.into_inner()),
        vec![
            format!("Task {id} has a durable abort mark"),
            format!("Task {id} has a durable abort mark"),
        ]
    );
    let ptrs = abort_ptrs.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(ptrs.len(), 1);
    assert_ne!(Some(ptrs[0]), *run_ptr.lock().unwrap_or_else(|e| e.into_inner()));
    opened.harness.close(context()).await.expect("close");
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum OneTwoCheckpoint {
    One,
    Two,
}

#[tokio::test]
async fn starts_no_further_phase_after_a_mark_that_lands_during_a_phase_with_progress() {
    let reached = deferred::<()>();
    let proceed = deferred::<()>();
    let phases_seen = Arc::new(Mutex::new(Vec::<String>::new()));

    let r_c = reached.clone();
    let p_c = proceed.clone();
    let ph1 = Arc::clone(&phases_seen);
    let ph2 = Arc::clone(&phases_seen);
    let pha = Arc::clone(&phases_seen);

    let mut phases: IndexMap<String, TypedPhaseHandler<(), OneTwoCheckpoint, (), ()>> = IndexMap::new();
    phases.insert(
        "one".to_owned(),
        Arc::new(move |_task, rt, ctx| {
            let r_c = r_c.clone();
            let p_c = p_c.clone();
            let ph1 = Arc::clone(&ph1);
            Box::pin(async move {
                ph1.lock().unwrap_or_else(|e| e.into_inner()).push("one".to_owned());
                rt.commit(
                    |_tx, _c| {
                        Box::pin(async {
                            Ok(Some(NextTaskState::Running {
                                checkpoint: OneTwoCheckpoint::Two,
                            }))
                        })
                    },
                    ctx,
                )
                .await?;
                r_c.resolve(());
                let _ = p_c.wait().await;
                Ok(())
            })
        }),
    );
    phases.insert(
        "two".to_owned(),
        Arc::new(move |_task, _rt, _ctx| {
            let ph2 = Arc::clone(&ph2);
            Box::pin(async move {
                ph2.lock().unwrap_or_else(|e| e.into_inner()).push("two".to_owned());
                Ok(())
            })
        }),
    );

    let two = define_task(TaskDefinition {
        name: "test.mark-boundary".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(OneTwoCheckpoint::One)),
        phases,
        abort: Arc::new(move |_task, rt, ctx| {
            let pha = Arc::clone(&pha);
            Box::pin(async move {
                pha.lock().unwrap_or_else(|e| e.into_inner()).push("abort".to_owned());
                rt.commit(|_tx, _c| Box::pin(async { Ok(Some(aborted_with("boundary"))) }), ctx)
                    .await
            })
        }),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });

    let opened = open_root(vec![two.clone().into_any()], OpenTasksOptions::default(), None).await;
    let id = opened
        .root
        .commit(
            move |tx| async move {
                tx.create_task(
                    &two,
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
        .expect("create");
    opened.harness.resume().expect("resume");
    reached.wait().await.expect("reached");
    let aborting = mark_durably(&opened.harness, id.erased()).await;
    proceed.resolve(());
    assert_eq!(aborting.wait().await.expect("abort"), AbortTaskResult::Marked);
    assert_eq!(
        opened.harness.wait_for_task(id, context()).await.expect("wait").outcome,
        TaskOutcome::Aborted {
            reason: Some("boundary".to_owned()),
            result: None,
        }
    );
    assert_eq!(
        *phases_seen.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["one", "abort"]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn signals_and_joins_the_run_before_returning_then_runs_the_abort_handler() {
    let reached = deferred::<()>();
    let run_ended = Arc::new(AtomicBool::new(false));
    let r_c = reached.clone();
    let end_c = Arc::clone(&run_ended);
    let signalled = one_step::<(), _, _>("test.signalled", move |_t, rt, _ctx| {
        let r_c = r_c.clone();
        let end_c = Arc::clone(&end_c);
        async move {
            r_c.resolve(());
            let sig = rt.signal()?;
            let res = aborted::<()>(&sig).await;
            end_c.store(true, Ordering::Release);
            res
        }
    });

    let opened = open_root(vec![signalled.clone().into_any()], OpenTasksOptions::default(), None).await;
    let id = start(&opened.root, &signalled, false).await;
    opened.harness.resume().expect("resume");
    reached.wait().await.expect("reached");
    assert_eq!(
        opened.harness.abort_task(id.erased(), context()).await.expect("abort"),
        AbortTaskResult::Marked
    );
    assert!(run_ended.load(Ordering::Acquire));
    assert_eq!(
        opened
            .harness
            .abort_task(id.erased(), context())
            .await
            .expect("abort again"),
        AbortTaskResult::Marked
    );
    assert_eq!(
        opened.harness.wait_for_task(id, context()).await.expect("wait").outcome,
        TaskOutcome::Aborted {
            reason: Some("test".to_owned()),
            result: None,
        }
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn aborts_waiting_work_before_its_wait_ends_and_faults_abort_handlers_that_throw_or_settle_nothing() {
    let gate = deferred::<()>();
    let first = gated_task("test.dependency", gate.clone());
    let first_slot = Arc::new(Mutex::new(None::<TaskId>));

    let f_l = Arc::clone(&first_slot);
    let lazy = one_step_with_abort::<(), _, _, _, _>(
        "test.lazy-abort",
        move |_t, rt, ctx| {
            let f_l = Arc::clone(&f_l);
            async move {
                let dep = f_l.lock().unwrap_or_else(|e| e.into_inner()).expect("first");
                rt.commit(
                    move |_tx, _c| {
                        Box::pin(async move {
                            Ok(Some(NextTaskState::Waiting {
                                checkpoint: StepCheckpoint::Run,
                                on: vec![dep],
                                policy: JoinPolicy::AllSettled,
                            }))
                        })
                    },
                    ctx,
                )
                .await
            }
        },
        |_t, _rt, _ctx| async { Ok(()) },
    );

    let f_t = Arc::clone(&first_slot);
    let throwing = one_step_with_abort::<(), _, _, _, _>(
        "test.throwing-abort",
        move |_t, rt, ctx| {
            let f_t = Arc::clone(&f_t);
            async move {
                let dep = f_t.lock().unwrap_or_else(|e| e.into_inner()).expect("first");
                rt.commit(
                    move |_tx, _c| {
                        Box::pin(async move {
                            Ok(Some(NextTaskState::Waiting {
                                checkpoint: StepCheckpoint::Run,
                                on: vec![dep],
                                policy: JoinPolicy::AllSettled,
                            }))
                        })
                    },
                    ctx,
                )
                .await
            }
        },
        |_t, _rt, _ctx| async { Err(pi_js::Error::msg("abort failed")) },
    );

    let opened = open_root(
        vec![
            first.clone().into_any(),
            lazy.clone().into_any(),
            throwing.clone().into_any(),
        ],
        OpenTasksOptions::default(),
        None,
    )
    .await;
    let first_id = start(&opened.root, &first, false).await;
    *first_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(first_id.erased());
    let lazy_id = start(&opened.root, &lazy, false).await;
    let throwing_id = start(&opened.root, &throwing, false).await;
    opened.harness.resume().expect("resume");

    eventually(|| async {
        opened
            .harness
            .get_task(throwing_id, context())
            .await
            .expect("get")
            .is_some_and(|t| t.state.status() == TaskStatus::Waiting)
    })
    .await;
    eventually(|| async {
        opened
            .harness
            .get_task(lazy_id, context())
            .await
            .expect("get")
            .is_some_and(|t| t.state.status() == TaskStatus::Waiting)
    })
    .await;

    opened
        .harness
        .abort_task(lazy_id.erased(), context())
        .await
        .expect("abort lazy");
    opened
        .harness
        .abort_task(throwing_id.erased(), context())
        .await
        .expect("abort throwing");

    assert_eq!(
        opened
            .harness
            .wait_for_task(lazy_id, context())
            .await
            .expect("wait lazy")
            .outcome,
        TaskOutcome::Faulted {
            error: TaskOutcomeError {
                message: format!("Abort handler of task {lazy_id} returned without a terminal outcome"),
                detail: None,
            },
        }
    );
    assert_eq!(
        opened
            .harness
            .wait_for_task(throwing_id, context())
            .await
            .expect("wait throwing")
            .outcome,
        TaskOutcome::Faulted {
            error: TaskOutcomeError {
                message: "abort failed".to_owned(),
                detail: None,
            },
        }
    );
    assert_eq!(
        opened
            .harness
            .get_task(first_id, context())
            .await
            .expect("get first")
            .expect("present")
            .state
            .status(),
        TaskStatus::Running
    );
    gate.resolve(());
    opened
        .harness
        .wait_for_task(first_id, context())
        .await
        .expect("wait first");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn does_not_signal_a_running_abort_handler_when_aborted_again_and_a_cancelled_caller_leaves_the_mark_durable() {
    let reached = deferred::<()>();
    let proceed = deferred::<()>();
    let run_release = deferred::<()>();
    let abort_signalled = Arc::new(Mutex::new(None::<bool>));
    let aborts = Arc::new(AtomicUsize::new(0));

    let r_rel = run_release.clone();
    let r_ab = reached.clone();
    let p_ab = proceed.clone();
    let sig_ab = Arc::clone(&abort_signalled);
    let cnt_ab = Arc::clone(&aborts);

    let run = one_step_with_abort::<(), _, _, _, _>(
        "test.abort-again",
        move |_t, _rt, _ctx| {
            let r_rel = r_rel.clone();
            async move { r_rel.wait().await }
        },
        move |_t, rt, ctx| {
            let r_ab = r_ab.clone();
            let p_ab = p_ab.clone();
            let sig_ab = Arc::clone(&sig_ab);
            let cnt_ab = Arc::clone(&cnt_ab);
            async move {
                cnt_ab.fetch_add(1, Ordering::SeqCst);
                r_ab.resolve(());
                p_ab.wait().await?;
                *sig_ab.lock().unwrap_or_else(|e| e.into_inner()) = Some(rt.signal()?.aborted());
                rt.commit(|_tx, _c| Box::pin(async { Ok(Some(aborted_with("once"))) }), ctx)
                    .await
            }
        },
    );

    let opened = open_root(vec![run.clone().into_any()], OpenTasksOptions::default(), None).await;
    let id = start(&opened.root, &run, false).await;
    opened.harness.resume().expect("resume");
    flush().await;

    let controller = AbortController::new();
    let h_c = opened.harness.clone();
    let sig = controller.signal();
    let cancelled = track(async move { h_c.abort_task(id.erased(), with_abort_signal(sig, context())).await });
    while !opened
        .harness
        .get_task(id, context())
        .await
        .expect("get")
        .is_some_and(|t| t.abort_requested)
    {
        flush().await;
    }
    controller.abort(Some(AbortReason::error("Error", "caller gave up")));
    assert!(
        cancelled
            .wait()
            .await
            .expect_err("caller gave up")
            .to_string()
            .contains("caller gave up")
    );
    run_release.resolve(());
    reached.wait().await.expect("reached");
    assert_eq!(
        opened
            .harness
            .abort_task(id.erased(), context())
            .await
            .expect("abort again"),
        AbortTaskResult::Marked
    );
    proceed.resolve(());
    assert_eq!(
        opened.harness.wait_for_task(id, context()).await.expect("wait").outcome,
        TaskOutcome::Aborted {
            reason: Some("once".to_owned()),
            result: None,
        }
    );
    assert_eq!(*abort_signalled.lock().unwrap_or_else(|e| e.into_inner()), Some(false));
    assert_eq!(aborts.load(Ordering::SeqCst), 1);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_a_terminal_outcome_committed_by_an_abort_handler_that_throws_afterwards() {
    let run = one_step_with_abort::<(), _, _, _, _>(
        "test.abort-then-throw",
        |_t, rt, _ctx| async move {
            let sig = rt.signal()?;
            aborted(&sig).await
        },
        |_t, rt, ctx| async move {
            rt.commit(|_tx, _c| Box::pin(async { Ok(Some(aborted_with("done"))) }), ctx)
                .await?;
            Err(pi_js::Error::msg("after terminal"))
        },
    );
    let opened = open_root(vec![run.clone().into_any()], OpenTasksOptions::default(), None).await;
    let id = start(&opened.root, &run, false).await;
    opened.harness.resume().expect("resume");
    flush().await;
    opened.harness.abort_task(id.erased(), context()).await.expect("abort");
    assert_eq!(
        opened.harness.wait_for_task(id, context()).await.expect("wait").outcome,
        TaskOutcome::Aborted {
            reason: Some("done".to_owned()),
            result: None,
        }
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn never_starts_phase_one_when_the_abort_lands_while_the_reservation_settles() {
    let ran = Arc::new(AtomicBool::new(false));
    let ran_c = Arc::clone(&ran);
    let reserved = one_step::<(), _, _>("test.reserved", move |_t, rt, _ctx| {
        let ran_c = Arc::clone(&ran_c);
        async move {
            ran_c.store(true, Ordering::Release);
            let sig = rt.signal()?;
            aborted(&sig).await
        }
    });
    let storage = Arc::new(ControlledStorage::new());
    let opened = open_root(
        vec![reserved.clone().into_any()],
        OpenTasksOptions::default(),
        Some(Arc::clone(&storage) as Arc<dyn Storage>),
    )
    .await;
    let id = start(&opened.root, &reserved, false).await;
    let held = storage.hold_commits();
    opened.harness.resume().expect("resume");
    held.entered().await;
    let h_c = opened.harness.clone();
    let aborting = track(async move { h_c.abort_task(id.erased(), context()).await });
    held.release();
    assert_eq!(aborting.wait().await.expect("abort"), AbortTaskResult::Marked);
    assert_eq!(
        opened.harness.wait_for_task(id, context()).await.expect("wait").outcome,
        TaskOutcome::Aborted {
            reason: Some("test".to_owned()),
            result: None,
        }
    );
    assert!(!ran.load(Ordering::Acquire));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn lets_an_abort_mark_win_over_a_fault_that_races_it() {
    let storage = Arc::new(ControlledStorage::new());
    let marking = Arc::new(Mutex::new(None::<Tracked<pi_js::Result<AbortTaskResult>>>));
    let held_slot = Arc::new(Mutex::new(None::<support::session_support::Gate>));
    let harness_slot = Arc::new(Mutex::new(None::<Harness>));

    let st_c = Arc::clone(&storage);
    let mark_c = Arc::clone(&marking);
    let held_c = Arc::clone(&held_slot);
    let h_c = Arc::clone(&harness_slot);

    let racing = one_step::<(), _, _>("test.racing-fault", move |task, _rt, _ctx| {
        let st_c = Arc::clone(&st_c);
        let mark_c = Arc::clone(&mark_c);
        let held_c = Arc::clone(&held_c);
        let h_c = Arc::clone(&h_c);
        async move {
            let held = st_c.hold_commits();
            let held_enter = held.clone();
            let entered = tokio::spawn(async move { held_enter.entered().await });
            *held_c.lock().unwrap_or_else(|e| e.into_inner()) = Some(held);
            let h = h_c.lock().unwrap_or_else(|e| e.into_inner()).clone().expect("h");
            let tid = task.id.erased();
            *mark_c.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(track(async move { h.abort_task(tid, context()).await }));
            let _ = entered.await;
            Err(pi_js::Error::msg("would fault"))
        }
    });

    let opened = open_root(
        vec![racing.clone().into_any()],
        OpenTasksOptions::default(),
        Some(Arc::clone(&storage) as Arc<dyn Storage>),
    )
    .await;
    *harness_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(opened.harness.clone());
    let id = start(&opened.root, &racing, false).await;
    opened.harness.resume().expect("resume");
    eventually_sync(|| held_slot.lock().unwrap_or_else(|e| e.into_inner()).is_some()).await;
    let held = held_slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("held");
    held.entered().await;
    flush().await;
    held.release();
    assert_eq!(
        opened.harness.wait_for_task(id, context()).await.expect("wait").outcome,
        TaskOutcome::Aborted {
            reason: Some("test".to_owned()),
            result: None,
        }
    );
    assert_eq!(
        marking
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .expect("marking")
            .wait()
            .await
            .expect("abort"),
        AbortTaskResult::Marked
    );
    opened.harness.close(context()).await.expect("close");
}

// ---- describe("task close") ----

#[tokio::test]
async fn stops_without_outcomes_and_starts_no_fresh_phase_or_abort_invocation_while_closing() {
    let reached = deferred::<()>();
    let proceed = deferred::<()>();
    let phases_seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let abort_ran = Arc::new(AtomicBool::new(false));

    let r_c = reached.clone();
    let p_c = proceed.clone();
    let ph1 = Arc::clone(&phases_seen);
    let ph2 = Arc::clone(&phases_seen);
    let ab_c = Arc::clone(&abort_ran);

    let mut phases: IndexMap<String, TypedPhaseHandler<(), OneTwoCheckpoint, (), ()>> = IndexMap::new();
    phases.insert(
        "one".to_owned(),
        Arc::new(move |_task, rt, ctx| {
            let r_c = r_c.clone();
            let p_c = p_c.clone();
            let ph1 = Arc::clone(&ph1);
            Box::pin(async move {
                ph1.lock().unwrap_or_else(|e| e.into_inner()).push("one".to_owned());
                rt.commit(
                    |_tx, _c| {
                        Box::pin(async {
                            Ok(Some(NextTaskState::Running {
                                checkpoint: OneTwoCheckpoint::Two,
                            }))
                        })
                    },
                    ctx,
                )
                .await?;
                r_c.resolve(());
                let _ = p_c.wait().await;
                Ok(())
            })
        }),
    );
    phases.insert(
        "two".to_owned(),
        Arc::new(move |_task, _rt, _ctx| {
            let ph2 = Arc::clone(&ph2);
            Box::pin(async move {
                ph2.lock().unwrap_or_else(|e| e.into_inner()).push("two".to_owned());
                Ok(())
            })
        }),
    );

    let two = define_task(TaskDefinition {
        name: "test.two".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(OneTwoCheckpoint::One)),
        phases,
        abort: Arc::new(move |_task, _rt, _ctx| {
            let ab_c = Arc::clone(&ab_c);
            Box::pin(async move {
                ab_c.store(true, Ordering::Release);
                Ok(())
            })
        }),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });

    let storage = Arc::new(ControlledStorage::new());
    let opened = open_root(
        vec![two.clone().into_any()],
        OpenTasksOptions::default(),
        Some(Arc::clone(&storage) as Arc<dyn Storage>),
    )
    .await;
    let two_c = two.clone();
    let id = opened
        .root
        .commit(
            move |tx| async move {
                tx.create_task(
                    &two_c,
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
        .expect("create");
    opened.harness.resume().expect("resume");
    reached.wait().await.expect("reached");
    let aborting = mark_durably(&opened.harness, id.erased()).await;
    let record = opened
        .harness
        .get_erased_task(id.erased(), context())
        .await
        .expect("get")
        .expect("present");
    let commits = storage.commits_snapshot().len();
    let h_c = opened.harness.clone();
    let closing = track(h_c.close(context()));
    proceed.resolve(());
    closing.wait().await.expect("close");
    assert_eq!(aborting.wait().await.expect("abort"), AbortTaskResult::Marked);
    assert_eq!(storage.commits_snapshot().len(), commits);
    assert_eq!(*phases_seen.lock().unwrap_or_else(|e| e.into_inner()), vec!["one"]);
    assert!(!abort_ran.load(Ordering::Acquire));
    assert!(record.abort_requested);
    assert_eq!(
        record.state,
        TaskState::Running {
            checkpoint: serde_json::json!({ "phase": "two" }),
        }
    );
    assert!(
        opened
            .harness
            .commit(|_tx| async { Ok(()) }, context())
            .await
            .expect_err("commit after close")
            .to_string()
            .contains("closed")
    );
}

#[tokio::test]
async fn seals_admission_before_signalling_handlers_and_stops_watches_before_joining_them() {
    let notes: SessionDocToken<TextDoc> = define_session_doc(DocDefinitionInput {
        kind: "test.close-notes",
        version: 1,
        initial: TextDoc::default,
        migrate: None,
        checkpoint_when: None,
    })
    .expect("notes");
    let reached = deferred::<()>();
    let from_listener = Arc::new(Mutex::new(None::<Tracked<pi_js::Result<()>>>));
    let harness_slot = Arc::new(Mutex::new(None::<Harness>));
    let watch_end = Arc::new(Mutex::new(None::<WatchEnd>));

    let notes_s = notes.clone();
    let r_s = reached.clone();
    let fl_s = Arc::clone(&from_listener);
    let hs_s = Arc::clone(&harness_slot);
    let we_s = Arc::clone(&watch_end);

    let stubborn = one_step::<(), _, _>("test.stubborn", move |_t, rt, _ctx| {
        let notes_s = notes_s.clone();
        let r_s = r_s.clone();
        let fl_s = Arc::clone(&fl_s);
        let hs_s = Arc::clone(&hs_s);
        let we_s = Arc::clone(&we_s);
        async move {
            let watch = rt.watch_doc_session(&notes_s, context()).await?.expect("watch");
            let sig = rt.signal()?;
            let _guard = sig.on_abort(move |_reason| {
                let h = hs_s.lock().unwrap_or_else(|e| e.into_inner()).clone().expect("h");
                *fl_s.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(track(async move { h.commit(|_tx| async { Ok(()) }, context()).await }));
            });
            r_s.resolve(());
            let end = watch.closed().await;
            *we_s.lock().unwrap_or_else(|e| e.into_inner()) = Some(end);
            Ok(())
        }
    });

    let opened = open_root(vec![stubborn.clone().into_any()], OpenTasksOptions::default(), None).await;
    *harness_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(opened.harness.clone());
    let notes_c = notes.clone();
    opened
        .harness
        .commit(
            move |tx| async move {
                tx.doc_session(&notes_c).await?.update(|d| {
                    d.text = "x".to_owned();
                })
            },
            context(),
        )
        .await
        .expect("init");
    start(&opened.root, &stubborn, false).await;
    opened.harness.resume().expect("resume");
    reached.wait().await.expect("reached");
    opened.harness.close(context()).await.expect("close");
    assert!(
        from_listener
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .expect("from_listener")
            .wait()
            .await
            .expect_err("commit should fail")
            .to_string()
            .contains("closed")
    );
    assert_eq!(
        *watch_end.lock().unwrap_or_else(|e| e.into_inner()),
        Some(WatchEnd::SessionClosed)
    );
}

#[tokio::test]
async fn writes_no_fault_when_close_seals_while_the_step_after_a_failed_phase_is_queued() {
    let storage = Arc::new(ControlledStorage::new());
    let held_slot = Arc::new(Mutex::new(None::<support::session_support::Gate>));
    let harness_slot = Arc::new(Mutex::new(None::<Harness>));

    let st_t = Arc::clone(&storage);
    let held_t = Arc::clone(&held_slot);
    let h_t = Arc::clone(&harness_slot);

    let throws = one_step::<(), _, _>("test.close-fault", move |_t, rt, _ctx| {
        let st_t = Arc::clone(&st_t);
        let held_t = Arc::clone(&held_t);
        let h_t = Arc::clone(&h_t);
        async move {
            let held = st_t.hold_commits();
            *held_t.lock().unwrap_or_else(|e| e.into_inner()) = Some(held);
            let h = h_t.lock().unwrap_or_else(|e| e.into_inner()).clone().expect("h");
            let cid = rt.conversation_id()?;
            tokio::spawn(async move {
                let _ = h
                    .commit(
                        move |tx| async move {
                            tx.append_entry(
                                cid,
                                EntryDraft {
                                    kind: "blocker".to_owned(),
                                    model: None,
                                    data: None,
                                    head: None,
                                    edits: None,
                                },
                            )
                            .await?;
                            Ok(())
                        },
                        context(),
                    )
                    .await;
            });
            tokio::task::yield_now().await;
            Err(pi_js::Error::msg("would fault"))
        }
    });

    let opened = open_root(
        vec![throws.clone().into_any()],
        OpenTasksOptions::default(),
        Some(Arc::clone(&storage) as Arc<dyn Storage>),
    )
    .await;
    *harness_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(opened.harness.clone());
    let id = start(&opened.root, &throws, false).await;
    opened.harness.resume().expect("resume");
    eventually_sync(|| held_slot.lock().unwrap_or_else(|e| e.into_inner()).is_some()).await;
    let held = held_slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("held");
    held.entered().await;
    flush().await;
    let h_c = opened.harness.clone();
    let closing = track(h_c.close(context()));
    held.release();
    closing.wait().await.expect("closing");
    let statuses: Vec<TaskStatus> = storage
        .commits_snapshot()
        .into_iter()
        .flat_map(|writes| writes.into_iter())
        .filter_map(|w| match w {
            StorageWrite::Task { value } if value.id == id.erased() => Some(value.state.status()),
            _ => None,
        })
        .collect();
    assert_eq!(statuses, vec![TaskStatus::Pending, TaskStatus::Running]);
}

#[tokio::test]
async fn starts_no_abort_handler_whose_reservation_settles_while_closing() {
    let ran = Arc::new(AtomicBool::new(false));
    let ran_c = Arc::clone(&ran);
    let marked = one_step_with_abort::<(), _, _, _, _>(
        "test.close-abort-reservation",
        |_t, _rt, _ctx| async { Ok(()) },
        move |_t, _rt, _ctx| {
            let ran_c = Arc::clone(&ran_c);
            async move {
                ran_c.store(true, Ordering::Release);
                Ok(())
            }
        },
    );
    let storage = Arc::new(ControlledStorage::new());
    let opened = open_root(
        vec![marked.clone().into_any()],
        OpenTasksOptions::default(),
        Some(Arc::clone(&storage) as Arc<dyn Storage>),
    )
    .await;
    let id = start(&opened.root, &marked, false).await;
    assert_eq!(
        opened.harness.abort_task(id.erased(), context()).await.expect("abort"),
        AbortTaskResult::Marked
    );
    let held = storage.hold_commits();
    opened.harness.resume().expect("resume");
    held.entered().await;
    let h_c = opened.harness.clone();
    let closing = track(h_c.close(context()));
    held.release();
    closing.wait().await.expect("closing");
    assert!(!ran.load(Ordering::Acquire));
    let last_commit = storage.commits_snapshot().pop().expect("last commit");
    let task_writes: Vec<_> = last_commit
        .into_iter()
        .filter_map(|w| match w {
            StorageWrite::Task { value } => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(task_writes.len(), 1);
    assert_eq!(task_writes[0].id, id.erased());
    assert!(task_writes[0].abort_requested);
    assert_eq!(task_writes[0].state.status(), TaskStatus::Running);
}

#[tokio::test]
async fn rejects_a_runtime_commit_that_was_queued_on_the_line_when_close_sealed_it() {
    let storage = Arc::new(ControlledStorage::new());
    let queued = Arc::new(Mutex::new(None::<Tracked<pi_js::Result<()>>>));
    let held_slot = Arc::new(Mutex::new(None::<support::session_support::Gate>));
    let harness_slot = Arc::new(Mutex::new(None::<Harness>));
    let proceed = deferred::<()>();

    let st_q = Arc::clone(&storage);
    let q_q = Arc::clone(&queued);
    let held_q = Arc::clone(&held_slot);
    let h_q = Arc::clone(&harness_slot);
    let p_q = proceed.clone();

    let queued_task = one_step::<(), _, _>("test.close-queued-commit", move |_t, rt, _ctx| {
        let st_q = Arc::clone(&st_q);
        let q_q = Arc::clone(&q_q);
        let held_q = Arc::clone(&held_q);
        let h_q = Arc::clone(&h_q);
        let p_q = p_q.clone();
        async move {
            let held = st_q.hold_commits();
            *held_q.lock().unwrap_or_else(|e| e.into_inner()) = Some(held);
            let h = h_q.lock().unwrap_or_else(|e| e.into_inner()).clone().expect("h");
            let cid = rt.conversation_id()?;
            tokio::spawn(async move {
                let _ = h
                    .commit(
                        move |tx| async move {
                            tx.append_entry(
                                cid,
                                EntryDraft {
                                    kind: "blocker".to_owned(),
                                    model: None,
                                    data: None,
                                    head: None,
                                    edits: None,
                                },
                            )
                            .await?;
                            Ok(())
                        },
                        context(),
                    )
                    .await;
            });
            let rt_c = rt.clone();
            *q_q.lock().unwrap_or_else(|e| e.into_inner()) = Some(track(async move {
                rt_c.commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), context())
                    .await
            }));
            let _ = p_q.wait().await;
            Ok(())
        }
    });

    let opened = open_root(
        vec![queued_task.clone().into_any()],
        OpenTasksOptions::default(),
        Some(Arc::clone(&storage) as Arc<dyn Storage>),
    )
    .await;
    *harness_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(opened.harness.clone());
    let id = start(&opened.root, &queued_task, false).await;
    opened.harness.resume().expect("resume");
    eventually_sync(|| held_slot.lock().unwrap_or_else(|e| e.into_inner()).is_some()).await;
    let held = held_slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("held");
    held.entered().await;
    flush().await;
    let h_c = opened.harness.clone();
    let closing = track(h_c.close(context()));
    held.release();
    let err = queued
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("queued")
        .wait()
        .await
        .expect_err("queued commit should fail");
    assert!(err.to_string().contains("Harness is closed"), "got: {err}");
    proceed.resolve(());
    closing.wait().await.expect("closing");
    let task_write_count = storage
        .commits_snapshot()
        .into_iter()
        .flat_map(|writes| writes.into_iter())
        .filter(|w| matches!(w, StorageWrite::Task { value } if value.id == id.erased()))
        .count();
    assert_eq!(task_write_count, 2);
}

struct CloseOnSnapshotReader {
    registry: Arc<RegistryImpl>,
    close_on_snapshot: Arc<AtomicBool>,
    harness: Arc<Mutex<Option<Harness>>>,
    closing: Arc<Mutex<Option<Tracked<pi_js::Result<()>>>>>,
}

impl RegistryReader for CloseOnSnapshotReader {
    fn snapshot(&self) -> Arc<dyn RegistrySnapshot> {
        if self.close_on_snapshot.swap(false, Ordering::AcqRel) {
            if let Some(h) = self.harness.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                let fut = track(h.close(context()));
                *self.closing.lock().unwrap_or_else(|e| e.into_inner()) = Some(fut);
            }
        }
        self.registry.snapshot()
    }

    fn subscribe(&self, listener: Arc<dyn Fn() + Send + Sync>) -> pi_js::Unsubscribe {
        self.registry.subscribe(listener)
    }
}

#[tokio::test]
async fn starts_no_next_phase_when_close_seals_during_a_step_that_decided_to_continue() {
    let phases_seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let registry = create_registry();
    let harness_slot = Arc::new(Mutex::new(None::<Harness>));
    let closing_slot = Arc::new(Mutex::new(None::<Tracked<pi_js::Result<()>>>));
    let close_on_snapshot = Arc::new(AtomicBool::new(false));

    let reader = Arc::new(CloseOnSnapshotReader {
        registry: Arc::clone(&registry),
        close_on_snapshot: Arc::clone(&close_on_snapshot),
        harness: Arc::clone(&harness_slot),
        closing: Arc::clone(&closing_slot),
    });

    let ph1 = Arc::clone(&phases_seen);
    let ph2 = Arc::clone(&phases_seen);
    let cos1 = Arc::clone(&close_on_snapshot);

    let mut phases: IndexMap<String, TypedPhaseHandler<(), OneTwoCheckpoint, (), ()>> = IndexMap::new();
    phases.insert(
        "one".to_owned(),
        Arc::new(move |_task, rt, ctx| {
            let ph1 = Arc::clone(&ph1);
            let cos1 = Arc::clone(&cos1);
            Box::pin(async move {
                ph1.lock().unwrap_or_else(|e| e.into_inner()).push("one".to_owned());
                rt.commit(
                    |_tx, _c| {
                        Box::pin(async {
                            Ok(Some(NextTaskState::Running {
                                checkpoint: OneTwoCheckpoint::Two,
                            }))
                        })
                    },
                    ctx,
                )
                .await?;
                cos1.store(true, Ordering::Release);
                Ok(())
            })
        }),
    );
    phases.insert(
        "two".to_owned(),
        Arc::new(move |_task, _rt, _ctx| {
            let ph2 = Arc::clone(&ph2);
            Box::pin(async move {
                ph2.lock().unwrap_or_else(|e| e.into_inner()).push("two".to_owned());
                Ok(())
            })
        }),
    );

    let two = define_task(TaskDefinition {
        name: "test.close-in-step".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(OneTwoCheckpoint::One)),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });
    add_task(&registry, two.clone().into_any(), None).expect("add two");

    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            models: create_models(None),
            registry: reader,
            settings: None,
            env: None,
            conversation_created: None,
            now: None,
            on_report: None,
        },
        context(),
    )
    .await
    .expect("open");
    *harness_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(harness.clone());
    let root = harness.root(context(), None).await.expect("root");
    let two_c = two.clone();
    root.commit(
        move |tx| async move {
            tx.create_task(
                &two_c,
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
    .expect("create");
    harness.resume().expect("resume");
    eventually_sync(|| closing_slot.lock().unwrap_or_else(|e| e.into_inner()).is_some()).await;
    closing_slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("closing")
        .wait()
        .await
        .expect("close");
    assert_eq!(*phases_seen.lock().unwrap_or_else(|e| e.into_inner()), vec!["one"]);
}

#[tokio::test]
async fn joins_a_reservation_that_settles_while_closing_without_starting_its_handler() {
    let ran = Arc::new(AtomicBool::new(false));
    let ran_c = Arc::clone(&ran);
    let never = one_step::<(), _, _>("test.close-reservation", move |_t, _rt, _ctx| {
        let ran_c = Arc::clone(&ran_c);
        async move {
            ran_c.store(true, Ordering::Release);
            Ok(())
        }
    });
    let storage = Arc::new(ControlledStorage::new());
    let opened = open_root(
        vec![never.clone().into_any()],
        OpenTasksOptions::default(),
        Some(Arc::clone(&storage) as Arc<dyn Storage>),
    )
    .await;
    let id = start(&opened.root, &never, false).await;
    let held = storage.hold_commits();
    opened.harness.resume().expect("resume");
    held.entered().await;
    let h_c = opened.harness.clone();
    let closing = track(h_c.close(context()));
    held.release();
    closing.wait().await.expect("close");
    assert!(!ran.load(Ordering::Acquire));
    let last_commit = storage.commits_snapshot().pop().expect("last commit");
    let task_writes: Vec<_> = last_commit
        .into_iter()
        .filter_map(|w| match w {
            StorageWrite::Task { value } => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(task_writes.len(), 1);
    assert_eq!(task_writes[0].id, id.erased());
    assert_eq!(task_writes[0].state.status(), TaskStatus::Running);
}
