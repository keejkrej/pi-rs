//! Port of packages/durable/test/examples/13-recovery.ts
#![allow(dead_code)]

// Close, reopen, and continue where a task stopped.
// Run via `spec_usage::examples_smoke`.

use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use indexmap::IndexMap;
use pi_ai::create_models;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::tasks::{TaskDefinition, TypedPhaseHandler, define_task};
use pi_durable::types::{NextTaskState, Storage, TaskOptions, TaskOutcome, TaskOwnership, TaskState};
use pi_durable::{Extension, Harness, HarnessOptions, Registry, RegistryReader, create_registry, define_extension};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TickerInput {
    to: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TickerState {
    phase: String,
    n: i64,
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();

    // Everything a task needs to continue is in storage, so a new Harness over the
    // same storage picks up where the last one stopped. This example keeps its
    // storage in a SQLite file so it survives closing.
    let directory = tempfile::TempDir::new().map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let database_path = directory.path().join("session.sqlite").to_string_lossy().into_owned();

    let notified_two = Arc::new(AtomicBool::new(false));
    let tick_two = Arc::new(Notify::new());
    let notified_c = Arc::clone(&notified_two);
    let tick_two_c = Arc::clone(&tick_two);

    let mut phases: IndexMap<String, TypedPhaseHandler<TickerInput, TickerState, String, ()>> = IndexMap::new();
    phases.insert(
        "tick".to_owned(),
        Arc::new(move |task, runtime, task_context| {
            let notified = Arc::clone(&notified_c);
            let tick_two = Arc::clone(&tick_two_c);
            Box::pin(async move {
                let n = task.checkpoint.n;
                let memo_key = format!("printed-{n}");
                // Save the intent before the effect. A memo keeps the first value
                // written under its name, so if the process dies after printing but
                // before the next checkpoint is saved, the rerun sees the memo and
                // does not print the same tick twice.
                if runtime
                    .memo_get::<bool>(&memo_key, task_context.clone())
                    .await?
                    .is_none()
                {
                    runtime.memo_set(&memo_key, &true, task_context.clone()).await?;
                    pi_js::console::log(&format!("tick {n}"));
                }
                if n == 2 && !notified.swap(true, Ordering::SeqCst) {
                    tick_two.notify_one();
                }
                // Save the outcome: the next tick, or the final result.
                let to = task.input.to;
                runtime
                    .commit(
                        move |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(if n == to {
                                    NextTaskState::Terminal {
                                        outcome: TaskOutcome::Completed {
                                            result: format!("counted to {n}"),
                                        },
                                    }
                                } else {
                                    NextTaskState::Running {
                                        checkpoint: TickerState {
                                            phase: "tick".to_owned(),
                                            n: n + 1,
                                        },
                                    }
                                }))
                            })
                        },
                        task_context.clone(),
                    )
                    .await?;
                // Wait a little between ticks. Closing the Harness cancels this wait;
                // the checkpoint saved above is where the next Harness continues.
                runtime.sleep(pi_js::time::now_ms() + 50, task_context).await?;
                Ok(())
            })
        }),
    );

    let ticker = define_task(TaskDefinition {
        name: "example.ticker".to_owned(),
        version: 1,
        initial: Arc::new(|_| {
            Ok(TickerState {
                phase: "tick".to_owned(),
                n: 1,
            })
        }),
        phases,
        abort: Arc::new(|_task, runtime, task_context| {
            Box::pin(async move {
                runtime
                    .commit(
                        |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Aborted {
                                        reason: None,
                                        result: None,
                                    },
                                }))
                            })
                        },
                        task_context,
                    )
                    .await
            })
        }),
        migrate: None,
        _hooks: PhantomData,
    });

    let registry = create_registry();
    registry.install(define_extension(Extension {
        name: "ticker".to_owned(),
        tasks: vec![ticker.clone().erase()],
        ..Default::default()
    }))?;

    let open = || {
        let database_path = database_path.clone();
        let registry = Arc::clone(&registry) as Arc<dyn RegistryReader>;
        let context = context.clone();
        async move {
            let storage = open_node_sqlite_storage(&database_path, NodeSqliteStorageOptions::default()).await?;
            Harness::open(
                Arc::new(storage) as Arc<dyn Storage>,
                HarnessOptions {
                    models: create_models(None),
                    registry,
                    settings: None,
                    env: None,
                    conversation_created: None,
                    now: None,
                    on_report: None,
                },
                context,
            )
            .await
        }
    };

    // First run: start counting to 5, and close the Harness right after tick 2 is
    // printed, before its next checkpoint is saved. That is the same situation as
    // a crash between the effect and saving its outcome.
    let first_run = open().await?;
    let root = first_run.root(context.clone(), None).await?;
    let root_id = root.id();
    let ticker_c = ticker.clone();
    let ticker_id = root
        .commit(
            move |tx| async move {
                tx.create_task(
                    &ticker_c,
                    &TickerInput { to: 5 },
                    TaskOptions {
                        ownership: TaskOwnership::Conversation,
                        conversation_id: Some(root_id),
                        background: false,
                    },
                )
                .await
            },
            context.clone(),
        )
        .await?;
    first_run.resume()?;
    tick_two.notified().await;
    first_run.close(context.clone()).await?;

    // Read the saved record through a Harness that never resumes, so nothing runs.
    let reader = open().await?;
    let saved = reader
        .get_task(ticker_id, context.clone())
        .await?
        .ok_or_else(|| pi_js::Error::msg("ticker task missing"))?;
    reader.close(context.clone()).await?;
    pi_js::console::log(&format!(
        "closed; saved checkpoint: {:?} memos: {:?}",
        saved.state, saved.memos
    ));

    // Second run: nothing to restart by hand. Waiting for the unfinished task
    // enables scheduling and continues it. Tick 2 runs again because its outcome
    // was never saved, but its memo says it was already printed.
    let second_run = open().await?;
    let counted = second_run.wait_for_task(ticker_id, context.clone()).await?;
    if let TaskState::Terminal { outcome } = &counted.state {
        pi_js::console::log(&format!("after reopen: {outcome:?}"));
    }
    second_run.close(context).await?;
    Ok(())
}
