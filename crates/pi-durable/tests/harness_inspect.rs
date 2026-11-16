//! Port of packages/durable/test/harness-inspect.test.ts

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use indexmap::IndexMap;
use pi_durable::harness::types::{
    ConversationCreateOptions, SchedulingStatus, SubmissionDraft, TaskBlockedReason, TaskInspection,
    TaskInspectionState, UserInput,
};
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::tasks::{Task, TaskDefinition, TypedMigrateFn, define_task};
use pi_durable::types::{ConversationOwnership, EntryDraft, JsonValue, TaskId, TaskOptions, TaskOwnership};
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use support::chat_support::{chat_setup, open_chat, unanswered, wait_for};
use support::harness_support::add_task;
use support::session_support::context;
use support::task_support::{OpenTasksOptions, completed, deferred, eventually, open_tasks};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum RunStep {
    Run,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum DepStep {
    Wait,
    Done,
}

fn make_task(
    name: &str,
    version: i64,
    gate: Option<support::task_support::Deferred<()>>,
    migrate: Option<Arc<dyn Fn() -> pi_js::Result<()> + Send + Sync>>,
) -> Task<JsonValue, RunStep, JsonValue, ()> {
    let mut phases = IndexMap::new();
    let gate_cb = gate;
    phases.insert(
        "run".to_owned(),
        Arc::new(
            move |_task, runtime: pi_durable::tasks::TaskRuntimeHandle<JsonValue, RunStep, JsonValue, ()>, ctx| {
                let gate_cb = gate_cb.clone();
                Box::pin(async move {
                    if let Some(g) = gate_cb {
                        g.wait().await?;
                    }
                    runtime
                        .commit(
                            |_tx, _cur| Box::pin(async { Ok(Some(completed(JsonValue::Null))) }),
                            ctx,
                        )
                        .await
                }) as pi_js::BoxFuture<pi_js::Result<()>>
            },
        ) as pi_durable::tasks::TypedPhaseHandler<JsonValue, RunStep, JsonValue, ()>,
    );

    let migrate_fn: Option<TypedMigrateFn<JsonValue, RunStep>> = migrate.map(|cb| {
        Arc::new(move |_raw_in: JsonValue, _raw_chk: JsonValue, _from: i64| {
            cb()?;
            Ok((JsonValue::Null, RunStep::Run))
        }) as TypedMigrateFn<JsonValue, RunStep>
    });

    define_task(TaskDefinition {
        name: name.to_owned(),
        version,
        initial: Arc::new(|_| Ok(RunStep::Run)),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: migrate_fn,
        _hooks: std::marker::PhantomData,
    })
}

fn state_of(tasks: &[TaskInspection], id: TaskId) -> Option<&TaskInspectionState> {
    tasks
        .iter()
        .find(|entry| entry.record.id == id)
        .map(|entry| &entry.state)
}

#[tokio::test]
async fn derives_every_live_tasks_state_without_running_task_code() {
    let gate = deferred();
    let gate_task = make_task("test.gate", 1, Some(gate.clone()), None);
    let gate_id_slot = Arc::new(std::sync::Mutex::new(None::<TaskId>));
    let gate_id_cb = Arc::clone(&gate_id_slot);

    let mut dep_phases = IndexMap::new();
    dep_phases.insert(
        "wait".to_owned(),
        Arc::new(
            move |_task, runtime: pi_durable::tasks::TaskRuntimeHandle<JsonValue, DepStep, JsonValue, ()>, ctx| {
                let gid = gate_id_cb.lock().unwrap().expect("gate id");
                Box::pin(async move {
                    runtime
                        .commit(
                            move |_tx, _cur| {
                                Box::pin(async move {
                                    Ok(Some(pi_durable::types::NextTaskState::Waiting {
                                        checkpoint: DepStep::Done,
                                        on: vec![gid],
                                        policy: pi_durable::types::JoinPolicy::AllSettled,
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                }) as pi_js::BoxFuture<pi_js::Result<()>>
            },
        ) as pi_durable::tasks::TypedPhaseHandler<JsonValue, DepStep, JsonValue, ()>,
    );
    dep_phases.insert(
        "done".to_owned(),
        Arc::new(
            |_task, runtime: pi_durable::tasks::TaskRuntimeHandle<JsonValue, DepStep, JsonValue, ()>, ctx| {
                Box::pin(async move {
                    runtime
                        .commit(
                            |_tx, _cur| Box::pin(async { Ok(Some(completed(JsonValue::Null))) }),
                            ctx,
                        )
                        .await
                }) as pi_js::BoxFuture<pi_js::Result<()>>
            },
        ) as pi_durable::tasks::TypedPhaseHandler<JsonValue, DepStep, JsonValue, ()>,
    );
    let dependent = define_task(TaskDefinition {
        name: "test.dependent".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(DepStep::Wait)),
        phases: dep_phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });

    let migrations = Arc::new(AtomicUsize::new(0));
    let mig_cb = Arc::clone(&migrations);

    let registered = vec![
        gate_task.as_any().clone(),
        dependent.as_any().clone(),
        make_task(
            "test.migrating",
            2,
            None,
            Some(Arc::new(move || {
                mig_cb.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })),
        )
        .as_any()
        .clone(),
        make_task("test.no-migration", 2, None, None).as_any().clone(),
        make_task(
            "test.failing",
            2,
            None,
            Some(Arc::new(|| Err(pi_js::Error::msg("cannot migrate")))),
        )
        .as_any()
        .clone(),
        make_task("test.too-old", 1, None, None).as_any().clone(),
    ];

    let opened = open_tasks(
        Arc::new(MemoryStorage::new()),
        registered.clone(),
        OpenTasksOptions::default(),
    )
    .await
    .expect("open tasks");
    let root = opened.harness.root(context(), None).await.expect("root");

    let migrating_v1 = make_task("test.migrating", 1, None, None);
    let no_mig_v1 = make_task("test.no-migration", 1, None, None);
    let failing_v1 = make_task("test.failing", 1, None, None);
    let too_old_v2 = make_task("test.too-old", 2, None, None);
    let missing_v1 = make_task("test.missing", 1, None, None);

    let gt_c = gate_task.clone();
    let dep_c = dependent.clone();
    let gate_slot_tx = Arc::clone(&gate_id_slot);

    let ids = root
        .commit(
            move |tx| async move {
                let opts = TaskOptions {
                    ownership: TaskOwnership::Conversation,
                    conversation_id: None,
                    background: false,
                };
                let gate_id = tx.create_task(&gt_c, &JsonValue::Null, opts.clone()).await?.erased();
                *gate_slot_tx.lock().unwrap() = Some(gate_id);
                let dependent_id = tx.create_task(&dep_c, &JsonValue::Null, opts.clone()).await?.erased();
                let migrating_id = tx
                    .create_task(&migrating_v1, &JsonValue::Null, opts.clone())
                    .await?
                    .erased();
                let no_mig_id = tx
                    .create_task(&no_mig_v1, &JsonValue::Null, opts.clone())
                    .await?
                    .erased();
                let failing_id = tx
                    .create_task(&failing_v1, &JsonValue::Null, opts.clone())
                    .await?
                    .erased();
                let too_old_id = tx
                    .create_task(&too_old_v2, &JsonValue::Null, opts.clone())
                    .await?
                    .erased();
                let missing_id = tx.create_task(&missing_v1, &JsonValue::Null, opts).await?.erased();
                Ok((
                    gate_id,
                    dependent_id,
                    migrating_id,
                    no_mig_id,
                    failing_id,
                    too_old_id,
                    missing_id,
                ))
            },
            context(),
        )
        .await
        .expect("create tasks");
    let (gate_id, dependent_id, migrating_id, no_mig_id, failing_id, too_old_id, missing_id) = ids;

    let paused = opened.harness.inspect(context()).await.expect("inspect paused");
    assert_eq!(paused.scheduling, SchedulingStatus::Paused);
    let actual_ids: Vec<TaskId> = paused.tasks.iter().map(|e| e.record.id).collect();
    assert_eq!(
        actual_ids,
        vec![
            gate_id,
            dependent_id,
            migrating_id,
            no_mig_id,
            failing_id,
            too_old_id,
            missing_id
        ]
    );
    assert!(matches!(
        state_of(&paused.tasks, gate_id),
        Some(TaskInspectionState::Ready { migrates: false })
    ));
    assert!(matches!(
        state_of(&paused.tasks, dependent_id),
        Some(TaskInspectionState::Ready { migrates: false })
    ));
    assert!(matches!(
        state_of(&paused.tasks, migrating_id),
        Some(TaskInspectionState::Ready { migrates: true })
    ));
    assert!(matches!(
        state_of(&paused.tasks, failing_id),
        Some(TaskInspectionState::Ready { migrates: true })
    ));
    match state_of(&paused.tasks, no_mig_id) {
        Some(TaskInspectionState::Blocked {
            reason: TaskBlockedReason::MigrationFailed,
            error: Some(err),
        }) => assert_eq!(
            err.message(),
            "Task test.no-migration version 2 has no migration from 1"
        ),
        other => panic!("unexpected no_mig state: {other:?}"),
    }
    assert!(matches!(
        state_of(&paused.tasks, too_old_id),
        Some(TaskInspectionState::Blocked {
            reason: TaskBlockedReason::TaskTooOld,
            ..
        })
    ));
    assert!(matches!(
        state_of(&paused.tasks, missing_id),
        Some(TaskInspectionState::Blocked {
            reason: TaskBlockedReason::MissingTask,
            ..
        })
    ));
    assert_eq!(migrations.load(Ordering::SeqCst), 0);
    assert_eq!(
        opened.harness.inspect(context()).await.expect("inspect 2").scheduling,
        SchedulingStatus::Paused
    );

    opened.harness.resume().expect("resume");
    let mig_check = Arc::clone(&migrations);
    eventually(move || {
        let m = mig_check.load(Ordering::SeqCst) == 1;
        async move { m }
    })
    .await;
    opened
        .harness
        .wait_for_task(migrating_id.cast::<JsonValue>(), context())
        .await
        .expect("wait migrating");

    let harness_c = opened.harness.clone();
    wait_for(
        move || {
            let harness = harness_c.clone();
            async move {
                let running = harness.inspect(context()).await.expect("inspect running");
                matches!(state_of(&running.tasks, gate_id), Some(TaskInspectionState::Running))
                    && matches!(
                        state_of(&running.tasks, dependent_id),
                        Some(TaskInspectionState::Waiting { .. })
                    )
            }
        },
        5_000,
    )
    .await;

    let running = opened.harness.inspect(context()).await.expect("inspect running");
    match state_of(&running.tasks, dependent_id) {
        Some(TaskInspectionState::Waiting { on }) => assert_eq!(on, &vec![gate_id]),
        other => panic!("unexpected dependent state: {other:?}"),
    }
    assert_eq!(running.scheduling, SchedulingStatus::Running);
    assert!(matches!(
        state_of(&running.tasks, gate_id),
        Some(TaskInspectionState::Running)
    ));
    assert!(state_of(&running.tasks, migrating_id).is_none());
    match state_of(&running.tasks, failing_id) {
        Some(TaskInspectionState::Blocked {
            reason: TaskBlockedReason::MigrationFailed,
            error: Some(err),
        }) => assert_eq!(err.message(), "cannot migrate"),
        other => panic!("unexpected failing state: {other:?}"),
    }

    gate.resolve(());
    opened
        .harness
        .wait_for_task(dependent_id.cast::<JsonValue>(), context())
        .await
        .expect("wait dependent");
    let settled = opened.harness.inspect(context()).await.expect("inspect settled");
    let remaining: Vec<TaskId> = settled.tasks.iter().map(|e| e.record.id).collect();
    assert_eq!(remaining, vec![no_mig_id, failing_id, too_old_id, missing_id]);
    opened.harness.close(context()).await.expect("close");
    let _ = add_task;
}

#[tokio::test]
async fn lists_unsettled_submissions() {
    let setup = chat_setup(None);
    let busy = unanswered();
    setup.faux.set_responses(vec![busy.step]);
    let opened = open_chat(
        Arc::new(MemoryStorage::new()),
        &setup,
        support::chat_support::OpenChatEnv::None,
    )
    .await
    .expect("open");
    opened.harness.resume().expect("resume");

    let other = opened
        .harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("other");
    other
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "note".to_owned(),
                    ..Default::default()
                },
            },
            context(),
        )
        .await
        .expect("other write");
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("input");
    busy.reached.wait().await.expect("busy reached");

    let inspection = opened.harness.inspect(context()).await.expect("inspect");
    assert_eq!(
        inspection.submissions,
        vec![input.status(context()).await.expect("status")]
    );
    assert_eq!(inspection.tasks.len(), 1);
    assert_eq!(inspection.tasks[0].record.kind, "pi.generation");
    assert!(matches!(inspection.tasks[0].state, TaskInspectionState::Running));
    opened.harness.close(context()).await.expect("close");
}
