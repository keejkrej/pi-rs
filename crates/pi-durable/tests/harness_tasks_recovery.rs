//! Port of packages/durable/test/harness-tasks-recovery.test.ts

mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::{TSchema, Type, create_models};
use pi_durable::AbortTaskResult;
use pi_durable::define_tool;
use pi_durable::documents::{DocDefinitionInput, TaskDocToken, define_task_doc};
use pi_durable::harness::harness::Harness;
use pi_durable::harness::registry::{RegistryImpl, create_registry};
use pi_durable::harness::types::{HarnessOptions, RegistryReader, ToolExecutionResult, ToolRegistration};
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::tasks::{Task, TaskDefinition, TypedMigrateFn, TypedPhaseHandler, define_task};
use pi_durable::types::{
    JsonValue, NextTaskState, Storage, StorageWrite, TaskId, TaskOptions, TaskOutcome, TaskOwnership, TaskQuery,
    TaskState, TaskStatus,
};
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use support::harness_support::{add_task, add_tool};
use support::session_support::{ControlledStorage, Gate, context, flush};
use support::task_support::{
    Deferred, OpenTasksOptions, OpenedTasks, aborted, aborted_with, completed, counting_reader, deferred,
    eventually_sync, open_tasks, track,
};
use tempfile::TempDir;

async fn sqlite_path() -> (TempDir, String) {
    let dir = TempDir::new().expect("create tempdir");
    let path = dir.path().join("session.sqlite").to_string_lossy().into_owned();
    (dir, path)
}

async fn open_sqlite(path: &str) -> Arc<dyn Storage> {
    Arc::new(
        open_node_sqlite_storage(path, NodeSqliteStorageOptions::default())
            .await
            .expect("open sqlite"),
    )
}

async fn create_in<S, R>(harness: &Harness, task: &Task<(), S, R, ()>) -> TaskId<R>
where
    S: Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    R: Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
{
    let root = harness.root(context(), None).await.expect("root");
    let task_c = task.clone();
    root.commit(
        move |tx| async move {
            tx.create_task(
                &task_c,
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
    .expect("create task")
}

#[derive(Default)]
struct TransferService {
    applied: Mutex<IndexMap<String, i64>>,
    calls: AtomicUsize,
}

impl TransferService {
    async fn apply(&self, key: &str, amount: i64) -> i64 {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut map = self.applied.lock().unwrap_or_else(|e| e.into_inner());
        *map.entry(key.to_owned()).or_insert(amount * 10)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TransferInput {
    amount: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum TransferState {
    Prepare,
    Apply { key: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TransferResult {
    receipt: i64,
}

fn transfer_task(
    service: Arc<TransferService>,
    interrupt_first: Arc<AtomicBool>,
) -> Task<TransferInput, TransferState, TransferResult, ()> {
    let mut phases: IndexMap<String, TypedPhaseHandler<TransferInput, TransferState, TransferResult, ()>> =
        IndexMap::new();
    phases.insert(
        "prepare".to_owned(),
        Arc::new(|task, rt, ctx| {
            Box::pin(async move {
                rt.memo_set("requested", &task.input.amount, ctx.clone()).await?;
                let tid = task.id;
                rt.commit(
                    move |_tx, _c| {
                        Box::pin(async move {
                            Ok(Some(NextTaskState::Running {
                                checkpoint: TransferState::Apply {
                                    key: format!("transfer-{tid}"),
                                },
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
        "apply".to_owned(),
        Arc::new(move |task, rt, ctx| {
            let service = Arc::clone(&service);
            let interrupt_first = Arc::clone(&interrupt_first);
            Box::pin(async move {
                let TransferState::Apply { key } = task.checkpoint else {
                    return Err(pi_js::Error::msg("expected apply checkpoint"));
                };
                let receipt = service.apply(&key, task.input.amount).await;
                if interrupt_first.swap(false, Ordering::AcqRel) {
                    let sig = rt.signal()?;
                    aborted::<()>(&sig).await?;
                }
                rt.commit(
                    move |_tx, _c| Box::pin(async move { Ok(Some(completed(TransferResult { receipt }))) }),
                    ctx,
                )
                .await
            })
        }),
    );

    define_task(TaskDefinition {
        name: "test.transfer".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(TransferState::Prepare)),
        phases,
        abort: Arc::new(|_task, rt, ctx| {
            Box::pin(async move {
                rt.commit(
                    |_tx, _c| {
                        Box::pin(async {
                            Ok(Some(NextTaskState::Terminal {
                                outcome: TaskOutcome::Aborted {
                                    reason: None,
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
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum StepCheckpoint {
    Run,
}

fn versioned(
    version: i64,
    result: &str,
    migrate: Option<TypedMigrateFn<(), StepCheckpoint>>,
) -> Task<(), StepCheckpoint, JsonValue, ()> {
    let res_r = result.to_owned();
    let res_a = result.to_owned();
    let mut phases: IndexMap<String, TypedPhaseHandler<(), StepCheckpoint, JsonValue, ()>> = IndexMap::new();
    phases.insert(
        "run".to_owned(),
        Arc::new(move |_task, rt, ctx| {
            let res_r = res_r.clone();
            Box::pin(async move {
                rt.commit(
                    move |_tx, _c| Box::pin(async move { Ok(Some(completed(JsonValue::String(res_r)))) }),
                    ctx,
                )
                .await
            })
        }),
    );
    define_task(TaskDefinition {
        name: "test.versioned".to_owned(),
        version,
        initial: Arc::new(|_| Ok(StepCheckpoint::Run)),
        phases,
        abort: Arc::new(move |_task, rt, ctx| {
            let res_a = res_a.clone();
            Box::pin(async move {
                rt.commit(
                    move |_tx, _c| Box::pin(async move { Ok(Some(aborted_with(res_a))) }),
                    ctx,
                )
                .await
            })
        }),
        migrate,
        _hooks: std::marker::PhantomData,
    })
}

// ---- describe("task recovery") ----

#[tokio::test]
async fn resumes_an_intent_effect_outcome_task_interrupted_after_its_intent_across_close_and_reopen() {
    let (_dir, path) = sqlite_path().await;
    let service = Arc::new(TransferService::default());
    let transfer = transfer_task(Arc::clone(&service), Arc::new(AtomicBool::new(true)));

    let first = open_tasks(
        open_sqlite(&path).await,
        vec![transfer.clone().into_any()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open 1");
    let root = first.harness.root(context(), None).await.expect("root");
    let t_c = transfer.clone();
    let id = root
        .commit(
            move |tx| async move {
                tx.create_task(
                    &t_c,
                    &TransferInput { amount: 7 },
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
    first.harness.resume().expect("resume");
    eventually_sync(|| service.calls.load(Ordering::SeqCst) == 1).await;
    first.harness.close(context()).await.expect("close 1");

    let second = open_tasks(
        open_sqlite(&path).await,
        vec![transfer.clone().into_any()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open 2");
    let rec = second
        .harness
        .get_task(id, context())
        .await
        .expect("get")
        .expect("present");
    assert!(!rec.abort_requested);
    let mut expected_memos = IndexMap::new();
    expected_memos.insert("requested".to_owned(), serde_json::json!(7));
    assert_eq!(rec.memos, Some(expected_memos));
    assert_eq!(
        rec.state,
        TaskState::Pending {
            checkpoint: serde_json::json!({ "phase": "apply", "key": format!("transfer-{id}") }),
        }
    );
    assert_eq!(service.calls.load(Ordering::SeqCst), 1);
    second.harness.resume().expect("resume 2");
    let receipt = second.harness.wait_for_task(id, context()).await.expect("wait");
    assert_eq!(
        receipt.outcome,
        TaskOutcome::Completed {
            result: TransferResult { receipt: 70 },
        }
    );
    assert_eq!(service.calls.load(Ordering::SeqCst), 2);
    assert_eq!(service.applied.lock().unwrap_or_else(|e| e.into_inner()).len(), 1);
    second.harness.close(context()).await.expect("close 2");

    let third = open_tasks(
        open_sqlite(&path).await,
        vec![transfer.into_any()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open 3");
    let rec3 = third
        .harness
        .get_task(id, context())
        .await
        .expect("get 3")
        .expect("present 3");
    assert_eq!(
        rec3.state,
        TaskState::Terminal {
            outcome: receipt.outcome
        }
    );
    third.harness.close(context()).await.expect("close 3");
}

#[tokio::test]
async fn resumes_abort_work_after_close_at_every_direct_task_abort_stage() {
    let (_dir, path) = sqlite_path().await;
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let block_abort = Arc::new(AtomicBool::new(true));
    let abort_reached = deferred::<()>();
    let run_release = deferred::<()>();

    let make_abortable = || {
        let log_r = Arc::clone(&log);
        let log_a = Arc::clone(&log);
        let r_rel = run_release.clone();
        let ba = Arc::clone(&block_abort);
        let ar = abort_reached.clone();

        let mut phases: IndexMap<String, TypedPhaseHandler<(), StepCheckpoint, (), ()>> = IndexMap::new();
        phases.insert(
            "run".to_owned(),
            Arc::new(move |_t, _rt, _ctx| {
                let log_r = Arc::clone(&log_r);
                let r_rel = r_rel.clone();
                Box::pin(async move {
                    log_r.lock().unwrap_or_else(|e| e.into_inner()).push("run".to_owned());
                    r_rel.wait().await
                })
            }),
        );
        define_task(TaskDefinition {
            name: "test.abortable".to_owned(),
            version: 1,
            initial: Arc::new(|_| Ok(StepCheckpoint::Run)),
            phases,
            abort: Arc::new(move |_t, rt, ctx| {
                let log_a = Arc::clone(&log_a);
                let ba = Arc::clone(&ba);
                let ar = ar.clone();
                Box::pin(async move {
                    log_a.lock().unwrap_or_else(|e| e.into_inner()).push("abort".to_owned());
                    if ba.load(Ordering::Acquire) {
                        ar.resolve(());
                        let sig = rt.signal()?;
                        aborted::<()>(&sig).await?;
                    }
                    rt.commit(|_tx, _c| Box::pin(async { Ok(Some(aborted_with("stop"))) }), ctx)
                        .await
                })
            }),
            migrate: None,
            _hooks: std::marker::PhantomData,
        })
    };

    let abortable = make_abortable();
    let mut opened = open_tasks(
        open_sqlite(&path).await,
        vec![abortable.clone().into_any()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open 1");
    let id = create_in(&opened.harness, &abortable).await;
    opened.harness.resume().expect("resume 1");
    eventually_sync(|| log.lock().unwrap_or_else(|e| e.into_inner()).len() == 1).await;
    let h_c = opened.harness.clone();
    let aborting = track(async move { h_c.abort_task(id.erased(), context()).await });
    while !opened
        .harness
        .get_task(id, context())
        .await
        .expect("get")
        .is_some_and(|t| t.abort_requested)
    {
        flush().await;
    }
    let h_close = opened.harness.clone();
    let closing = track(async move { h_close.close(context()).await });
    run_release.resolve(());
    closing.wait().await.expect("close 1");
    assert_eq!(aborting.wait().await.expect("abort"), AbortTaskResult::Marked);
    assert_eq!(*log.lock().unwrap_or_else(|e| e.into_inner()), vec!["run"]);

    opened = open_tasks(
        open_sqlite(&path).await,
        vec![make_abortable().into_any()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open 2");
    let rec2 = opened
        .harness
        .get_task(id, context())
        .await
        .expect("get 2")
        .expect("present 2");
    assert!(rec2.abort_requested);
    assert_eq!(rec2.state.status(), TaskStatus::Pending);
    opened.harness.resume().expect("resume 2");
    abort_reached.wait().await.expect("abort reached");
    opened.harness.close(context()).await.expect("close 2");
    assert_eq!(*log.lock().unwrap_or_else(|e| e.into_inner()), vec!["run", "abort"]);

    block_abort.store(false, Ordering::Release);
    opened = open_tasks(
        open_sqlite(&path).await,
        vec![make_abortable().into_any()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open 3");
    opened.harness.resume().expect("resume 3");
    assert_eq!(
        opened.harness.wait_for_task(id, context()).await.expect("wait").outcome,
        TaskOutcome::Aborted {
            reason: Some("stop".to_owned()),
            result: None,
        }
    );
    opened.harness.close(context()).await.expect("close 3");
    assert_eq!(
        *log.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["run", "abort", "abort"]
    );

    opened = open_tasks(
        open_sqlite(&path).await,
        vec![make_abortable().into_any()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open 4");
    opened.harness.resume().expect("resume 4");
    flush().await;
    assert_eq!(
        opened
            .harness
            .abort_task(id.erased(), context())
            .await
            .expect("abort 4"),
        AbortTaskResult::Terminal
    );
    opened.harness.close(context()).await.expect("close 4");
    assert_eq!(
        *log.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["run", "abort", "abort"]
    );
}

// ---- describe("task crash recovery") ----

fn crash_task(log: Arc<Mutex<Vec<String>>>, block_abort: bool) -> Task<(), StepCheckpoint, (), ()> {
    let log_r = Arc::clone(&log);
    let log_a = Arc::clone(&log);
    let mut phases: IndexMap<String, TypedPhaseHandler<(), StepCheckpoint, (), ()>> = IndexMap::new();
    phases.insert(
        "run".to_owned(),
        Arc::new(move |_t, _rt, _ctx| {
            let log_r = Arc::clone(&log_r);
            Box::pin(async move {
                log_r.lock().unwrap_or_else(|e| e.into_inner()).push("run".to_owned());
                std::future::pending::<pi_js::Result<()>>().await
            })
        }),
    );
    define_task(TaskDefinition {
        name: "test.crash".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(StepCheckpoint::Run)),
        phases,
        abort: Arc::new(move |_t, rt, ctx| {
            let log_a = Arc::clone(&log_a);
            Box::pin(async move {
                log_a.lock().unwrap_or_else(|e| e.into_inner()).push("abort".to_owned());
                if block_abort {
                    std::future::pending::<()>().await;
                }
                rt.commit(|_tx, _c| Box::pin(async { Ok(Some(aborted_with("recovered"))) }), ctx)
                    .await
            })
        }),
        migrate: None,
        _hooks: std::marker::PhantomData,
    })
}

async fn recover_crash(storage: Arc<ControlledStorage>, log: Arc<Mutex<Vec<String>>>) -> (Harness, TaskId<()>) {
    storage.crash();
    let opened = open_tasks(
        storage,
        vec![crash_task(log, false).into_any()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open recover");
    let page = opened
        .harness
        .commit(
            |tx| async move {
                tx.scan_tasks(
                    TaskQuery {
                        kind: Some("test.crash".to_owned()),
                        ..Default::default()
                    },
                    1,
                    None,
                )
                .await
            },
            context(),
        )
        .await
        .expect("scan");
    let id = page.items[0].id.cast::<()>();
    (opened.harness, id)
}

async fn crashed_run(log: Arc<Mutex<Vec<String>>>) -> (Arc<ControlledStorage>, Harness, TaskId<()>) {
    let storage = Arc::new(ControlledStorage::new());
    let task = crash_task(Arc::clone(&log), true);
    let opened = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![task.clone().into_any()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open");
    let id = create_in(&opened.harness, &task).await;
    opened.harness.resume().expect("resume");
    eventually_sync(|| log.lock().unwrap_or_else(|e| e.into_inner()).len() == 1).await;
    (storage, opened.harness, id)
}

#[tokio::test]
async fn crash_while_the_mark_commit_is_in_storage_the_run_resumes() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let (storage, harness, id) = crashed_run(Arc::clone(&log)).await;
    let held = storage.hold_commits();
    let h_c = harness.clone();
    tokio::spawn(async move {
        let _ = h_c.abort_task(id.erased(), context()).await;
    });
    held.entered().await;
    let (recovered, rec_id) = recover_crash(storage, Arc::clone(&log)).await;
    let rec = recovered
        .get_task(rec_id, context())
        .await
        .expect("get")
        .expect("present");
    assert!(!rec.abort_requested);
    assert_eq!(rec.state.status(), TaskStatus::Pending);
    recovered.resume().expect("resume");
    eventually_sync(|| log.lock().unwrap_or_else(|e| e.into_inner()).len() == 2).await;
    assert_eq!(*log.lock().unwrap_or_else(|e| e.into_inner()), vec!["run", "run"]);
    let _ = harness;
}

#[tokio::test]
async fn crash_after_the_mark_before_the_run_joins_only_the_abort_handler_runs() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let (storage, harness, id) = crashed_run(Arc::clone(&log)).await;
    let joined = Arc::new(AtomicBool::new(false));
    let joined_c = Arc::clone(&joined);
    let h_c = harness.clone();
    tokio::spawn(async move {
        let _ = h_c.abort_task(id.erased(), context()).await;
        joined_c.store(true, Ordering::Release);
    });
    while !harness
        .get_task(id, context())
        .await
        .expect("get")
        .is_some_and(|t| t.abort_requested)
    {
        flush().await;
    }
    flush().await;
    assert!(!joined.load(Ordering::Acquire));
    let (recovered, rec_id) = recover_crash(storage, Arc::clone(&log)).await;
    let rec = recovered
        .get_task(rec_id, context())
        .await
        .expect("get")
        .expect("present");
    assert!(rec.abort_requested);
    assert_eq!(rec.state.status(), TaskStatus::Pending);
    recovered.resume().expect("resume");
    assert_eq!(
        recovered.wait_for_task(rec_id, context()).await.expect("wait").outcome,
        TaskOutcome::Aborted {
            reason: Some("recovered".to_owned()),
            result: None,
        }
    );
    assert_eq!(*log.lock().unwrap_or_else(|e| e.into_inner()), vec!["run", "abort"]);
    recovered.close(context()).await.expect("close");
}

#[tokio::test]
async fn crash_while_the_abort_handler_runs_a_fresh_abort_invocation_settles_the_task() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let storage = Arc::new(ControlledStorage::new());
    let task = crash_task(Arc::clone(&log), true);
    let opened = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![task.clone().into_any()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open");
    let id = create_in(&opened.harness, &task).await;
    opened.harness.abort_task(id.erased(), context()).await.expect("abort");
    opened.harness.resume().expect("resume");
    eventually_sync(|| log.lock().unwrap_or_else(|e| e.into_inner()).len() == 1).await;
    assert_eq!(*log.lock().unwrap_or_else(|e| e.into_inner()), vec!["abort"]);
    assert_eq!(
        opened
            .harness
            .get_task(id, context())
            .await
            .expect("get")
            .expect("present")
            .state
            .status(),
        TaskStatus::Running
    );
    let (recovered, rec_id) = recover_crash(storage, Arc::clone(&log)).await;
    recovered.resume().expect("resume");
    assert_eq!(
        recovered.wait_for_task(rec_id, context()).await.expect("wait").outcome,
        TaskOutcome::Aborted {
            reason: Some("recovered".to_owned()),
            result: None,
        }
    );
    assert_eq!(*log.lock().unwrap_or_else(|e| e.into_inner()), vec!["abort", "abort"]);
    recovered.close(context()).await.expect("close");
}

#[tokio::test]
async fn crash_while_the_abort_outcome_is_in_storage_the_abort_handler_runs_again() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let storage = Arc::new(ControlledStorage::new());
    let reached = deferred::<()>();
    let proceed = deferred::<()>();

    let log_a = Arc::clone(&log);
    let r_a = reached.clone();
    let p_a = proceed.clone();
    let mut phases: IndexMap<String, TypedPhaseHandler<(), StepCheckpoint, (), ()>> = IndexMap::new();
    phases.insert("run".to_owned(), Arc::new(|_, _, _| Box::pin(async { Ok(()) })));
    let crash = define_task(TaskDefinition {
        name: "test.crash".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(StepCheckpoint::Run)),
        phases,
        abort: Arc::new(move |_t, rt, ctx| {
            let log_a = Arc::clone(&log_a);
            let r_a = r_a.clone();
            let p_a = p_a.clone();
            Box::pin(async move {
                log_a.lock().unwrap_or_else(|e| e.into_inner()).push("abort".to_owned());
                r_a.resolve(());
                p_a.wait().await?;
                rt.commit(|_tx, _c| Box::pin(async { Ok(Some(aborted_with("lost"))) }), ctx)
                    .await
            })
        }),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });

    let opened = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![crash.clone().into_any()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open");
    let id = create_in(&opened.harness, &crash).await;
    opened.harness.abort_task(id.erased(), context()).await.expect("abort");
    opened.harness.resume().expect("resume");
    reached.wait().await.expect("reached");
    let held = storage.hold_commits();
    proceed.resolve(());
    held.entered().await;
    let (recovered, rec_id) = recover_crash(storage, Arc::clone(&log)).await;
    recovered.resume().expect("resume");
    assert_eq!(
        recovered.wait_for_task(rec_id, context()).await.expect("wait").outcome,
        TaskOutcome::Aborted {
            reason: Some("recovered".to_owned()),
            result: None,
        }
    );
    assert_eq!(*log.lock().unwrap_or_else(|e| e.into_inner()), vec!["abort", "abort"]);
    recovered.close(context()).await.expect("close");
}

#[tokio::test]
async fn crash_after_the_terminal_outcome_nothing_runs_again() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let storage = Arc::new(ControlledStorage::new());
    let task = crash_task(Arc::clone(&log), false);
    let opened = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![task.clone().into_any()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open");
    let id = create_in(&opened.harness, &task).await;
    opened.harness.abort_task(id.erased(), context()).await.expect("abort");
    opened.harness.resume().expect("resume");
    opened.harness.wait_for_task(id, context()).await.expect("wait");
    let (recovered, rec_id) = recover_crash(storage, Arc::clone(&log)).await;
    recovered.resume().expect("resume");
    flush().await;
    assert_eq!(*log.lock().unwrap_or_else(|e| e.into_inner()), vec!["abort"]);
    assert_eq!(
        recovered
            .get_task(rec_id, context())
            .await
            .expect("get")
            .expect("present")
            .state,
        TaskState::Terminal {
            outcome: TaskOutcome::Aborted {
                reason: Some("recovered".to_owned()),
                result: None,
            },
        }
    );
    recovered.close(context()).await.expect("close");
}

#[tokio::test]
async fn crash_while_the_reservation_commit_is_in_storage_the_task_is_still_pending() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let storage = Arc::new(ControlledStorage::new());
    let task = crash_task(Arc::clone(&log), false);
    let opened = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![task.clone().into_any()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open");
    let _id = create_in(&opened.harness, &task).await;
    let held = storage.hold_commits();
    opened.harness.resume().expect("resume");
    held.entered().await;
    let (recovered, rec_id) = recover_crash(storage, Arc::clone(&log)).await;
    assert_eq!(
        recovered
            .get_task(rec_id, context())
            .await
            .expect("get")
            .expect("present")
            .state
            .status(),
        TaskStatus::Pending
    );
    assert!(log.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    recovered.abort_task(rec_id.erased(), context()).await.expect("abort");
    recovered.resume().expect("resume");
    recovered.wait_for_task(rec_id, context()).await.expect("wait");
    assert_eq!(*log.lock().unwrap_or_else(|e| e.into_inner()), vec!["abort"]);
    recovered.close(context()).await.expect("close");
}

// ---- describe("blocked tasks") ----

#[tokio::test]
async fn keeps_a_task_with_a_missing_definition_pending_and_live_for_idle_waits_until_registration() {
    let v1 = versioned(1, "v1", None);
    let opened = open_tasks(Arc::new(MemoryStorage::new()), vec![], OpenTasksOptions::default())
        .await
        .expect("open");
    let id = create_in(&opened.harness, &v1).await;
    opened.harness.resume().expect("resume");
    flush().await;
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
    let h_c = opened.harness.clone();
    let idle = track(async move { h_c.wait_for_idle(context()).await });
    flush().await;
    add_task(&opened.registry, v1.into_any(), None).expect("add v1");
    idle.wait().await.expect("idle");
    assert_eq!(
        opened
            .harness
            .get_task(id, context())
            .await
            .expect("get")
            .expect("present")
            .state,
        TaskState::Terminal {
            outcome: TaskOutcome::Completed {
                result: JsonValue::String("v1".to_owned())
            },
        }
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_a_task_stored_by_a_newer_version_pending_until_a_fitting_definition_is_registered() {
    let registry = create_registry();
    add_task(&registry, versioned(1, "old", None).into_any(), None).expect("add v1");
    let opened = open_tasks(
        Arc::new(MemoryStorage::new()),
        vec![],
        OpenTasksOptions {
            registry: Some(Arc::clone(&registry)),
            ..Default::default()
        },
    )
    .await
    .expect("open");
    let id = create_in(&opened.harness, &versioned(2, "new", None)).await;
    opened.harness.resume().expect("resume");
    flush().await;
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
    add_task(&registry, versioned(2, "new", None).into_any(), None).expect("add v2");
    assert_eq!(
        opened.harness.wait_for_task(id, context()).await.expect("wait").outcome,
        TaskOutcome::Completed {
            result: JsonValue::String("new".to_owned()),
        }
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn migrates_at_reservation_leaves_the_record_unchanged_when_migration_fails_and_retries_only_for_a_new_definition()
 {
    let (_dir, path) = sqlite_path().await;
    let first = open_tasks(open_sqlite(&path).await, vec![], OpenTasksOptions::default())
        .await
        .expect("open 1");
    let id = create_in(&first.harness, &versioned(1, "v1", None)).await;
    first.harness.close(context()).await.expect("close 1");

    let registry: Arc<RegistryImpl> = create_registry();
    let failures = Arc::new(AtomicUsize::new(0));
    let f_c = Arc::clone(&failures);
    add_task(
        &registry,
        versioned(
            2,
            "v2",
            Some(Arc::new(move |_input, _cp, _from| {
                f_c.fetch_add(1, Ordering::SeqCst);
                Err(pi_js::Error::msg("cannot migrate"))
            })),
        )
        .into_any(),
        None,
    )
    .expect("add broken v2");

    let opened = open_tasks(
        open_sqlite(&path).await,
        vec![],
        OpenTasksOptions {
            registry: Some(Arc::clone(&registry)),
            ..Default::default()
        },
    )
    .await
    .expect("open 2");
    opened.harness.resume().expect("resume");
    eventually_sync(|| opened.reports.lock().unwrap_or_else(|e| e.into_inner()).len() == 1).await;

    add_tool(
        &registry,
        define_tool(ToolRegistration {
            name: "unrelated".to_owned(),
            description: "unrelated".to_owned(),
            parameters: Type::object([] as [(&str, TSchema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(|_, _, _| Box::pin(async { Ok(ToolExecutionResult::default()) })),
        }),
        None,
    )
    .expect("add unrelated");
    flush().await;
    assert_eq!(failures.load(Ordering::SeqCst), 1);
    let reports: Vec<String> = opened
        .reports
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(reports.len(), 1);
    assert!(reports[0].contains("cannot migrate"));
    let rec = opened
        .harness
        .get_task(id, context())
        .await
        .expect("get")
        .expect("present");
    assert_eq!(rec.version, 1);
    assert_eq!(
        rec.state,
        TaskState::Pending {
            checkpoint: serde_json::json!({ "phase": "run" }),
        }
    );

    let migrations = Arc::new(Mutex::new(Vec::<i64>::new()));
    let m_c = Arc::clone(&migrations);
    add_task(
        &registry,
        versioned(
            2,
            "v2",
            Some(Arc::new(move |_input, cp, from| {
                m_c.lock().unwrap_or_else(|e| e.into_inner()).push(from);
                let step: StepCheckpoint =
                    serde_json::from_value(cp).map_err(|e| pi_js::Error::js("TypeError", e.to_string()))?;
                Ok(((), step))
            })),
        )
        .into_any(),
        None,
    )
    .expect("add working v2");

    let receipt = opened.harness.wait_for_task(id, context()).await.expect("wait");
    assert_eq!(receipt.version, 2);
    assert_eq!(
        receipt.outcome,
        TaskOutcome::Completed {
            result: JsonValue::String("v2".to_owned()),
        }
    );
    assert_eq!(*migrations.lock().unwrap_or_else(|e| e.into_inner()), vec![1]);
    opened.harness.close(context()).await.expect("close 2");
}

#[tokio::test]
async fn blocks_an_older_record_whose_newer_definition_has_no_migration() {
    let (_dir, path) = sqlite_path().await;
    let first = open_tasks(open_sqlite(&path).await, vec![], OpenTasksOptions::default())
        .await
        .expect("open 1");
    let id = create_in(&first.harness, &versioned(1, "v1", None)).await;
    first.harness.close(context()).await.expect("close 1");

    let opened = open_tasks(
        open_sqlite(&path).await,
        vec![versioned(2, "v2", None).into_any()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open 2");
    opened.harness.resume().expect("resume");
    eventually_sync(|| opened.reports.lock().unwrap_or_else(|e| e.into_inner()).len() == 1).await;
    let rep_msg = opened.reports.lock().unwrap_or_else(|e| e.into_inner())[0].to_string();
    assert!(rep_msg.contains("has no migration from 1"), "got: {rep_msg}");
    let rec = opened
        .harness
        .get_task(id, context())
        .await
        .expect("get")
        .expect("present");
    assert_eq!(rec.version, 1);
    assert_eq!(rec.state.status(), TaskStatus::Pending);
    opened.harness.close(context()).await.expect("close 2");
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ScratchDoc {
    n: i64,
}

#[tokio::test]
async fn settles_an_aborted_blocked_task_as_orphaned_and_retires_its_documents() {
    let scratch: TaskDocToken<ScratchDoc> = define_task_doc(DocDefinitionInput {
        kind: "test.orphan-scratch",
        version: 1,
        initial: ScratchDoc::default,
        migrate: None,
        checkpoint_when: None,
    })
    .expect("scratch doc");
    let opened = open_tasks(Arc::new(MemoryStorage::new()), vec![], OpenTasksOptions::default())
        .await
        .expect("open");
    let root = opened.harness.root(context(), None).await.expect("root");
    let scratch_c = scratch.clone();
    let v1 = versioned(1, "x", None);
    let id = root
        .commit(
            move |tx| async move {
                let created = tx
                    .create_task(
                        &v1,
                        &(),
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: None,
                            background: false,
                        },
                    )
                    .await?;
                tx.doc_task(&scratch_c, created.erased()).await?.update(|d| {
                    d.n = 1;
                })?;
                Ok(created)
            },
            context(),
        )
        .await
        .expect("create");
    assert_eq!(
        opened.harness.abort_task(id.erased(), context()).await.expect("abort"),
        AbortTaskResult::Marked
    );
    assert_eq!(
        opened
            .harness
            .get_task(id, context())
            .await
            .expect("get")
            .expect("present")
            .state,
        TaskState::Terminal {
            outcome: TaskOutcome::Orphaned {
                reason: "missing_task".to_owned(),
            },
        }
    );
    assert_eq!(
        opened
            .harness
            .snapshot_task(&scratch, id.erased(), context())
            .await
            .expect("snapshot"),
        None
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn orphans_a_marked_task_whose_definition_disappeared_while_its_run_was_active() {
    let reached = deferred::<()>();
    let r_c = reached.clone();
    let mut phases: IndexMap<String, TypedPhaseHandler<(), StepCheckpoint, (), ()>> = IndexMap::new();
    phases.insert(
        "run".to_owned(),
        Arc::new(move |_t, rt, _ctx| {
            let r_c = r_c.clone();
            Box::pin(async move {
                r_c.resolve(());
                let sig = rt.signal()?;
                aborted(&sig).await
            })
        }),
    );
    let running = define_task(TaskDefinition {
        name: "test.vanishing".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(StepCheckpoint::Run)),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Err(pi_js::Error::msg("must not run")) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });
    let registry = create_registry();
    let reg_inst = add_task(&registry, running.clone().into_any(), None).expect("add running");
    let opened = open_tasks(
        Arc::new(MemoryStorage::new()),
        vec![],
        OpenTasksOptions {
            registry: Some(Arc::clone(&registry)),
            ..Default::default()
        },
    )
    .await
    .expect("open");
    let id = create_in(&opened.harness, &running).await;
    opened.harness.resume().expect("resume");
    reached.wait().await.expect("reached");
    reg_inst.dispose();
    assert_eq!(
        opened.harness.abort_task(id.erased(), context()).await.expect("abort"),
        AbortTaskResult::Marked
    );
    assert_eq!(
        opened.harness.wait_for_task(id, context()).await.expect("wait").outcome,
        TaskOutcome::Orphaned {
            reason: "missing_task".to_owned(),
        }
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn orphans_a_reopened_abort_marked_task_without_a_definition_once_scheduling_resumes() {
    let (_dir, path) = sqlite_path().await;
    let first = open_tasks(
        open_sqlite(&path).await,
        vec![versioned(1, "x", None).into_any()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open 1");
    let id = create_in(&first.harness, &versioned(1, "x", None)).await;
    assert_eq!(
        first.harness.abort_task(id.erased(), context()).await.expect("abort"),
        AbortTaskResult::Marked
    );
    first.harness.close(context()).await.expect("close 1");

    let opened = open_tasks(open_sqlite(&path).await, vec![], OpenTasksOptions::default())
        .await
        .expect("open 2");
    let rec = opened
        .harness
        .get_task(id, context())
        .await
        .expect("get")
        .expect("present");
    assert!(rec.abort_requested);
    assert_eq!(rec.state.status(), TaskStatus::Pending);
    let h_c = opened.harness.clone();
    let idle = track(async move { h_c.wait_for_idle(context()).await });
    opened.harness.resume().expect("resume");
    idle.wait().await.expect("idle");
    assert_eq!(
        opened
            .harness
            .get_task(id, context())
            .await
            .expect("get")
            .expect("present")
            .state,
        TaskState::Terminal {
            outcome: TaskOutcome::Orphaned {
                reason: "missing_task".to_owned(),
            },
        }
    );
    opened.harness.close(context()).await.expect("close 2");
}

// ---- describe("definition handover") ----

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum HandoverCheckpoint {
    A,
    B,
    C,
}

#[derive(Clone, Default)]
struct HandoverGates {
    a: Option<Deferred<()>>,
    b: Option<Deferred<()>>,
    on_end: Option<Arc<dyn Fn() + Send + Sync>>,
}

fn handover_task(
    label: &str,
    version: i64,
    log: Arc<Mutex<Vec<String>>>,
    gates: HandoverGates,
    migrate: Option<TypedMigrateFn<(), HandoverCheckpoint>>,
) -> Task<(), HandoverCheckpoint, (), ()> {
    let make_advance = |phase_name: &'static str, next: HandoverCheckpoint, gate: Option<Deferred<()>>| {
        let label = label.to_owned();
        let log = Arc::clone(&log);
        let on_end = gates.on_end.clone();
        let handler: TypedPhaseHandler<(), HandoverCheckpoint, (), ()> = Arc::new(move |_t, rt, ctx| {
            let label = label.clone();
            let log = Arc::clone(&log);
            let gate = gate.clone();
            let on_end = on_end.clone();
            let next = next.clone();
            Box::pin(async move {
                log.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!("{label}:{phase_name} start"));
                if let Some(g) = gate {
                    let _ = g.wait().await;
                }
                rt.commit(
                    move |_tx, _c| Box::pin(async move { Ok(Some(NextTaskState::Running { checkpoint: next })) }),
                    ctx,
                )
                .await?;
                flush().await;
                if let Some(cb) = on_end {
                    cb();
                }
                log.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!("{label}:{phase_name} end"));
                Ok(())
            })
        });
        handler
    };

    let label_c = label.to_owned();
    let log_c = Arc::clone(&log);
    let label_a = label.to_owned();
    let log_a = Arc::clone(&log);

    let mut phases: IndexMap<String, TypedPhaseHandler<(), HandoverCheckpoint, (), ()>> = IndexMap::new();
    phases.insert("a".to_owned(), make_advance("a", HandoverCheckpoint::B, gates.a));
    phases.insert("b".to_owned(), make_advance("b", HandoverCheckpoint::C, gates.b));
    phases.insert(
        "c".to_owned(),
        Arc::new(move |_t, rt, ctx| {
            let label_c = label_c.clone();
            let log_c = Arc::clone(&log_c);
            Box::pin(async move {
                log_c
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!("{label_c}:c"));
                rt.commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                    .await
            })
        }),
    );

    define_task(TaskDefinition {
        name: "test.handover".to_owned(),
        version,
        initial: Arc::new(|_| Ok(HandoverCheckpoint::A)),
        phases,
        abort: Arc::new(move |_t, rt, ctx| {
            let label_a = label_a.clone();
            let log_a = Arc::clone(&log_a);
            Box::pin(async move {
                log_a
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!("{label_a}:abort"));
                rt.commit(|_tx, _c| Box::pin(async move { Ok(Some(aborted_with(label_a))) }), ctx)
                    .await
            })
        }),
        migrate,
        _hooks: std::marker::PhantomData,
    })
}

async fn start_handover(
    log: Arc<Mutex<Vec<String>>>,
    gates: HandoverGates,
    storage: Arc<dyn Storage>,
) -> (OpenedTasks, support::harness_support::Installed, TaskId<()>) {
    let registry = create_registry();
    let old_def = handover_task("old", 1, Arc::clone(&log), gates, None);
    let old = add_task(&registry, old_def.clone().into_any(), None).expect("add old");
    let opened = open_tasks(
        storage,
        vec![],
        OpenTasksOptions {
            registry: Some(registry),
            ..Default::default()
        },
    )
    .await
    .expect("open");
    let id = create_in(&opened.harness, &old_def).await;
    opened.harness.resume().expect("resume");
    eventually_sync(|| log.lock().unwrap_or_else(|e| e.into_inner()).len() == 1).await;
    (opened, old, id)
}

#[tokio::test]
async fn hands_over_at_the_next_phase_boundary_to_a_same_version_replacement_without_overlap() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let gate = deferred::<()>();
    let (opened, _old, id) = start_handover(
        Arc::clone(&log),
        HandoverGates {
            a: Some(gate.clone()),
            ..Default::default()
        },
        Arc::new(MemoryStorage::new()),
    )
    .await;
    add_task(
        &opened.registry,
        handover_task("new", 1, Arc::clone(&log), HandoverGates::default(), None).into_any(),
        None,
    )
    .expect("add new");
    gate.resolve(());
    opened.harness.wait_for_task(id, context()).await.expect("wait");
    assert_eq!(
        *log.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["old:a start", "old:a end", "new:b start", "new:b end", "new:c"]
    );
    opened.harness.close(context()).await.expect("close");
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum MemoedCheckpoint {
    A,
    B,
}

#[tokio::test]
async fn keeps_the_memos_across_a_handover() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let gate = deferred::<()>();
    let reached = deferred::<()>();

    let make_memo_task = |label: &'static str| {
        let log_b = Arc::clone(&log);
        let gate_a = gate.clone();
        let reached_a = reached.clone();
        let mut phases: IndexMap<String, TypedPhaseHandler<(), MemoedCheckpoint, String, ()>> = IndexMap::new();
        phases.insert(
            "a".to_owned(),
            Arc::new(move |_t, rt, ctx| {
                let gate_a = gate_a.clone();
                let reached_a = reached_a.clone();
                Box::pin(async move {
                    rt.memo_set("picked", &label.to_owned(), ctx.clone()).await?;
                    reached_a.resolve(());
                    gate_a.wait().await?;
                    rt.commit(
                        |_tx, _c| {
                            Box::pin(async {
                                Ok(Some(NextTaskState::Running {
                                    checkpoint: MemoedCheckpoint::B,
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
            Arc::new(move |_t, rt, ctx| {
                let log_b = Arc::clone(&log_b);
                Box::pin(async move {
                    let picked = rt.memo_set("picked", &label.to_owned(), ctx.clone()).await?;
                    log_b
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(format!("{label}:b {picked}"));
                    rt.commit(move |_tx, _c| Box::pin(async move { Ok(Some(completed(picked))) }), ctx)
                        .await
                })
            }),
        );
        define_task(TaskDefinition {
            name: "test.handover-memo".to_owned(),
            version: 1,
            initial: Arc::new(|_| Ok(MemoedCheckpoint::A)),
            phases,
            abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
            migrate: None,
            _hooks: std::marker::PhantomData,
        })
    };

    let registry = create_registry();
    let old_def = make_memo_task("old");
    add_task(&registry, old_def.clone().into_any(), None).expect("add old");
    let opened = open_tasks(
        Arc::new(MemoryStorage::new()),
        vec![],
        OpenTasksOptions {
            registry: Some(Arc::clone(&registry)),
            ..Default::default()
        },
    )
    .await
    .expect("open");
    let id = create_in(&opened.harness, &old_def).await;
    opened.harness.resume().expect("resume");
    reached.wait().await.expect("reached");
    add_task(&registry, make_memo_task("new").into_any(), None).expect("add new");
    gate.resolve(());
    let receipt = opened.harness.wait_for_task(id, context()).await.expect("wait");
    assert_eq!(
        receipt.outcome,
        TaskOutcome::Completed {
            result: "old".to_owned()
        }
    );
    assert_eq!(*log.lock().unwrap_or_else(|e| e.into_inner()), vec!["new:b old"]);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn hands_over_to_a_newer_version_with_a_migration() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let gate = deferred::<()>();
    let (opened, _old, id) = start_handover(
        Arc::clone(&log),
        HandoverGates {
            a: Some(gate.clone()),
            ..Default::default()
        },
        Arc::new(MemoryStorage::new()),
    )
    .await;
    add_task(
        &opened.registry,
        handover_task(
            "v2",
            2,
            Arc::clone(&log),
            HandoverGates::default(),
            Some(Arc::new(|_, _, _| Ok(((), HandoverCheckpoint::C)))),
        )
        .into_any(),
        None,
    )
    .expect("add v2");
    gate.resolve(());
    let receipt = opened.harness.wait_for_task(id, context()).await.expect("wait");
    assert_eq!(receipt.version, 2);
    assert_eq!(
        *log.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["old:a start", "old:a end", "v2:c"]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn hands_over_to_a_newer_version_whose_migration_fails_and_leaves_the_task_blocked() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let gate = deferred::<()>();
    let (opened, _old, id) = start_handover(
        Arc::clone(&log),
        HandoverGates {
            a: Some(gate.clone()),
            ..Default::default()
        },
        Arc::new(MemoryStorage::new()),
    )
    .await;
    add_task(
        &opened.registry,
        handover_task(
            "broken",
            2,
            Arc::clone(&log),
            HandoverGates::default(),
            Some(Arc::new(|_, _, _| Err(pi_js::Error::msg("broken migration")))),
        )
        .into_any(),
        None,
    )
    .expect("add broken");
    gate.resolve(());
    eventually_sync(|| opened.reports.lock().unwrap_or_else(|e| e.into_inner()).len() == 1).await;
    flush().await;
    let rec = opened
        .harness
        .get_task(id, context())
        .await
        .expect("get")
        .expect("present");
    assert_eq!(rec.version, 1);
    assert_eq!(
        rec.state,
        TaskState::Pending {
            checkpoint: serde_json::json!({ "phase": "b" }),
        }
    );
    assert_eq!(
        *log.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["old:a start", "old:a end"]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_running_under_the_old_definition_when_the_replacement_is_missing_or_cannot_take_the_task() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let gate_a = deferred::<()>();
    let gate_b = deferred::<()>();
    let (opened, old, id) = start_handover(
        Arc::clone(&log),
        HandoverGates {
            a: Some(gate_a.clone()),
            b: Some(gate_b.clone()),
            on_end: None,
        },
        Arc::new(MemoryStorage::new()),
    )
    .await;
    old.dispose();
    gate_a.resolve(());
    eventually_sync(|| {
        log.lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&"old:b start".to_owned())
    })
    .await;
    add_task(
        &opened.registry,
        handover_task("incompatible", 2, Arc::clone(&log), HandoverGates::default(), None).into_any(),
        None,
    )
    .expect("add incompatible");
    gate_b.resolve(());
    opened.harness.wait_for_task(id, context()).await.expect("wait");
    assert_eq!(
        *log.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["old:a start", "old:a end", "old:b start", "old:b end", "old:c"]
    );
    let report_msgs: Vec<String> = opened
        .reports
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(report_msgs.len(), 2);
    assert!(report_msgs[0].contains("(missing_task)"), "got: {}", report_msgs[0]);
    assert!(
        report_msgs[1].contains("(incompatible_task)"),
        "got: {}",
        report_msgs[1]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn rejects_a_runtime_commit_of_the_old_invocation_queued_behind_its_handover_commit() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let gate = deferred::<()>();
    let storage = Arc::new(ControlledStorage::new());
    let held_slot = Arc::new(Mutex::new(None::<Gate>));
    let old_runtime = Arc::new(Mutex::new(
        None::<pi_durable::tasks::TaskRuntimeHandle<(), HandoverCheckpoint, (), ()>>,
    ));
    let harness_slot = Arc::new(Mutex::new(None::<Harness>));

    let registry = create_registry();
    let g_a = gate.clone();
    let st_a = Arc::clone(&storage);
    let held_a = Arc::clone(&held_slot);
    let ort_a = Arc::clone(&old_runtime);
    let h_a = Arc::clone(&harness_slot);

    let mut phases: IndexMap<String, TypedPhaseHandler<(), HandoverCheckpoint, (), ()>> = IndexMap::new();
    phases.insert(
        "a".to_owned(),
        Arc::new(move |task, rt, ctx| {
            let g_a = g_a.clone();
            let st_a = Arc::clone(&st_a);
            let held_a = Arc::clone(&held_a);
            let ort_a = Arc::clone(&ort_a);
            let h_a = Arc::clone(&h_a);
            Box::pin(async move {
                *ort_a.lock().unwrap_or_else(|e| e.into_inner()) = Some(rt.clone());
                g_a.wait().await?;
                rt.commit(
                    |_tx, _c| {
                        Box::pin(async {
                            Ok(Some(NextTaskState::Running {
                                checkpoint: HandoverCheckpoint::B,
                            }))
                        })
                    },
                    ctx,
                )
                .await?;
                let held = st_a.hold_commits();
                *held_a.lock().unwrap_or_else(|e| e.into_inner()) = Some(held);
                let h = h_a.lock().unwrap_or_else(|e| e.into_inner()).clone().expect("h");
                let cid = task.conversation_id;
                tokio::spawn(async move {
                    let _ = h
                        .commit(
                            move |tx| async move {
                                tx.append_entry(
                                    cid,
                                    pi_durable::types::EntryDraft {
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
                Ok(())
            })
        }),
    );
    phases.insert("b".to_owned(), Arc::new(|_, _, _| Box::pin(async { Ok(()) })));
    phases.insert("c".to_owned(), Arc::new(|_, _, _| Box::pin(async { Ok(()) })));

    let old_def = define_task(TaskDefinition {
        name: "test.handover".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(HandoverCheckpoint::A)),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });
    add_task(&registry, old_def.clone().into_any(), None).expect("add old");
    let opened = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![],
        OpenTasksOptions {
            registry: Some(Arc::clone(&registry)),
            ..Default::default()
        },
    )
    .await
    .expect("open");
    *harness_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(opened.harness.clone());
    let id = create_in(&opened.harness, &old_def).await;
    opened.harness.resume().expect("resume");
    eventually_sync(|| old_runtime.lock().unwrap_or_else(|e| e.into_inner()).is_some()).await;
    add_task(
        &registry,
        handover_task("new", 1, Arc::clone(&log), HandoverGates::default(), None).into_any(),
        None,
    )
    .expect("add new");
    gate.resolve(());
    eventually_sync(|| held_slot.lock().unwrap_or_else(|e| e.into_inner()).is_some()).await;
    let held = held_slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("held");
    held.entered().await;
    flush().await;
    let rt_old = old_runtime
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("old rt");
    let late = track(async move {
        rt_old
            .commit(|_tx, _c| Box::pin(async { Ok(Some(completed(()))) }), context())
            .await
    });
    held.release();
    assert!(
        late.wait()
            .await
            .expect_err("late should fail")
            .to_string()
            .contains("invocation has ended")
    );
    opened.harness.wait_for_task(id, context()).await.expect("wait");
    assert_eq!(
        *log.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["new:b start", "new:b end", "new:c"]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn preserves_an_abort_mark_that_races_the_handover_commit_the_new_definition_aborts() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let gate = deferred::<()>();
    let storage = Arc::new(ControlledStorage::new());
    let held_slot = Arc::new(Mutex::new(None::<Gate>));

    let st_cb = Arc::clone(&storage);
    let held_cb = Arc::clone(&held_slot);
    let (opened, _old, id) = start_handover(
        Arc::clone(&log),
        HandoverGates {
            a: Some(gate.clone()),
            b: None,
            on_end: Some(Arc::new(move || {
                let mut guard = held_cb.lock().unwrap_or_else(|e| e.into_inner());
                if guard.is_none() {
                    *guard = Some(st_cb.hold_commits());
                }
            })),
        },
        Arc::clone(&storage) as Arc<dyn Storage>,
    )
    .await;
    add_task(
        &opened.registry,
        handover_task("new", 1, Arc::clone(&log), HandoverGates::default(), None).into_any(),
        None,
    )
    .expect("add new");
    gate.resolve(());
    eventually_sync(|| held_slot.lock().unwrap_or_else(|e| e.into_inner()).is_some()).await;
    let held = held_slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("held");
    held.entered().await;
    let h_c = opened.harness.clone();
    let aborting = track(async move { h_c.abort_task(id.erased(), context()).await });
    held.release();
    assert_eq!(aborting.wait().await.expect("abort"), AbortTaskResult::Marked);
    assert_eq!(
        opened.harness.wait_for_task(id, context()).await.expect("wait").outcome,
        TaskOutcome::Aborted {
            reason: Some("new".to_owned()),
            result: None,
        }
    );
    let states: Vec<String> = storage
        .commits_snapshot()
        .into_iter()
        .flat_map(|writes| writes.into_iter())
        .filter_map(|w| match w {
            StorageWrite::Task { value } if value.id == id.erased() => {
                let st = match value.state.status() {
                    TaskStatus::Pending => "pending",
                    TaskStatus::Running => "running",
                    TaskStatus::Waiting => "waiting",
                    TaskStatus::Completing => "completing",
                    TaskStatus::Terminal => "terminal",
                };
                Some(format!("{st}{}", if value.abort_requested { "+mark" } else { "" }))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        states,
        vec![
            "pending",
            "running",
            "running",
            "pending",
            "pending+mark",
            "running+mark",
            "terminal+mark"
        ]
    );
    assert_eq!(
        *log.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["old:a start", "old:a end", "new:abort"]
    );
    opened.harness.close(context()).await.expect("close");
}

// ---- describe("Harness open") ----

#[tokio::test]
async fn releases_its_registry_subscription_and_closes_the_session_when_open_fails() {
    let storage = Arc::new(ControlledStorage::new());
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let log_r = Arc::clone(&log);
    let mut phases: IndexMap<String, TypedPhaseHandler<(), StepCheckpoint, (), ()>> = IndexMap::new();
    phases.insert(
        "run".to_owned(),
        Arc::new(move |_t, _rt, _ctx| {
            let log_r = Arc::clone(&log_r);
            Box::pin(async move {
                log_r.lock().unwrap_or_else(|e| e.into_inner()).push("run".to_owned());
                std::future::pending::<pi_js::Result<()>>().await
            })
        }),
    );
    let stuck = define_task(TaskDefinition {
        name: "test.stuck".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(StepCheckpoint::Run)),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });
    let first = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![stuck.clone().into_any()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open 1");
    create_in(&first.harness, &stuck).await;
    first.harness.resume().expect("resume 1");
    eventually_sync(|| log.lock().unwrap_or_else(|e| e.into_inner()).len() == 1).await;

    let registry = create_registry();
    let reader = counting_reader(registry as Arc<dyn RegistryReader>);
    storage.fail_next_commit(pi_js::Error::msg("disk full"));
    let err = match Harness::open(
        Arc::clone(&storage) as Arc<dyn Storage>,
        HarnessOptions {
            models: create_models(None),
            registry: reader.clone(),
            settings: None,
            env: None,
            conversation_created: None,
            now: None,
            on_report: None,
        },
        context(),
    )
    .await
    {
        Ok(_) => panic!("open should fail"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("disk full"), "got: {err}");
    assert_eq!(reader.subscriptions(), 0);
    assert!(storage.task(TaskId::new_unchecked(1), context()).await.is_err());
}
