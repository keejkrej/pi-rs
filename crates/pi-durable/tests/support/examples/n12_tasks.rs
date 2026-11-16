//! Port of packages/durable/test/examples/12-tasks.ts
#![allow(dead_code)]

// Run a durable task.
// Run via `spec_usage::examples_smoke`.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::create_models;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::tasks::{TaskDefinition, TypedPhaseHandler, define_task};
use pi_durable::types::{NextTaskState, TaskOptions, TaskOutcome, TaskOwnership, TaskState};
use pi_durable::{
    Extension, Harness, HarnessOptions, MemoryStorage, Registry, RegistryReader, create_registry, define_extension,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaymentInput {
    amount: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum PaymentState {
    Prepare,
    Charge { key: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaymentResult {
    receipt: i64,
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();

    // A task is a small state machine. Its state, the checkpoint, is saved after
    // every step, so after a crash the next open continues from the last saved
    // step. The usual pattern: save what you are about to do, do it, then save
    // the result. A crash between doing and saving reruns that step, so the step
    // must be safe to repeat; here the fake payment service ignores a repeated key.
    let payments: Arc<Mutex<IndexMap<String, i64>>> = Arc::new(Mutex::new(IndexMap::new()));
    let payments_charge = Arc::clone(&payments);

    let mut phases: IndexMap<String, TypedPhaseHandler<PaymentInput, PaymentState, PaymentResult, ()>> =
        IndexMap::new();
    phases.insert(
        "prepare".to_owned(),
        Arc::new(|task, runtime, task_context| {
            Box::pin(async move {
                let key = format!("payment-{}", task.id);
                runtime
                    .commit(
                        move |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Running {
                                    checkpoint: PaymentState::Charge { key },
                                }))
                            })
                        },
                        task_context,
                    )
                    .await
            })
        }),
    );
    phases.insert(
        "charge".to_owned(),
        Arc::new(move |task, runtime, task_context| {
            let payments = Arc::clone(&payments_charge);
            Box::pin(async move {
                let key = match &task.checkpoint {
                    PaymentState::Charge { key } => key.clone(),
                    PaymentState::Prepare => {
                        return Err(pi_js::Error::msg("charge phase reached without charge checkpoint"));
                    }
                };
                let receipt = {
                    let mut map = payments.lock().unwrap_or_else(|e| e.into_inner());
                    *map.entry(key).or_insert(task.input.amount * 100)
                };
                runtime
                    .commit(
                        move |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Completed {
                                        result: PaymentResult { receipt },
                                    },
                                }))
                            })
                        },
                        task_context,
                    )
                    .await
            })
        }),
    );

    let payment = define_task(TaskDefinition {
        name: "example.payment".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(PaymentState::Prepare)),
        // One handler per phase. Each must save progress through runtime.commit():
        // its callback returns the next checkpoint or the final outcome, and that
        // state is saved in the same commit as everything else the callback wrote.
        phases,
        // Runs instead of the phases after harness.abort_task(); it decides the outcome.
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

    // Extensions bring task code; the Harness finds it by name in the registry.
    // Nothing runs until resume() or a call that waits for progress, such as
    // wait_for_task().
    let registry = create_registry();
    registry.install(define_extension(Extension {
        name: "payments".to_owned(),
        tasks: vec![pi_durable::AnyTask::from(payment.clone())],
        ..Default::default()
    }))?;
    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            models: create_models(None),
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: None,
            env: None,
            conversation_created: None,
            now: None,
            on_report: None,
        },
        context.clone(),
    )
    .await?;
    let root = harness.root(context.clone(), None).await?;
    let root_id = root.id();

    // Every task names its owner. This one belongs to the conversation; a task can
    // also own child tasks and wait for them (24-child-tasks).
    let payment_c = payment.clone();
    let payment_id = root
        .commit(
            move |tx| async move {
                tx.create_task(
                    &payment_c,
                    &PaymentInput { amount: 5 },
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

    // The finished task record is the durable receipt; wait_for_task() knows its result type.
    let paid = harness.wait_for_task(payment_id, context.clone()).await?;
    let outcome = &paid.outcome;
    pi_js::console::log(&format!("payment outcome: {outcome:?}"));

    harness.close(context).await?;
    Ok(())
}
