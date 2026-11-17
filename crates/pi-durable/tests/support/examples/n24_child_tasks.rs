//! Port of packages/durable/test/examples/24-child-tasks.ts
#![allow(dead_code)]

// A task that owns child tasks: a checkout charges four payments at once and waits for them.
// The task graph view prints the checkout's tree while its payments run.
// Run via `spec_usage::examples_smoke`.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use indexmap::{IndexMap, IndexSet};
use pi_ai::create_models;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::tasks::{AnyTask, TaskDefinition, TypedPhaseHandler, define_task};
use pi_durable::types::{
    JoinPolicy, NextTaskState, Storage, TaskId, TaskOptions, TaskOutcome, TaskOutcomeError, TaskOwnership, TaskState,
};
use pi_durable::{
    Conversation, Extension, Harness, HarnessOptions, Registry, RegistryReader, TaskGraph, TaskGraphNode,
    TaskGraphState, create_registry, define_extension,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PaymentInput {
    card: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PaymentState {
    phase: String,
    at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PaymentResult {
    card: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CheckoutInput {
    cards: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum CheckoutState {
    Pay,
    Decide { payments: Vec<TaskId<PaymentResult>> },
}

fn outcome_status<R>(outcome: &TaskOutcome<R>) -> &'static str {
    match outcome {
        TaskOutcome::Completed { .. } => "completed",
        TaskOutcome::Failed { .. } => "failed",
        TaskOutcome::Aborted { .. } => "aborted",
        TaskOutcome::Orphaned { .. } => "orphaned",
        TaskOutcome::Faulted { .. } => "faulted",
    }
}

/// Print the live tasks as a tree along their owner edges.
fn print_graph(graph: &TaskGraph) {
    fn print_node(nodes: &[&TaskGraphNode], node: &TaskGraphNode, depth: usize) {
        let status = match &node.state {
            TaskGraphState::Pending { .. } => "pending".to_owned(),
            TaskGraphState::Running { .. } => "running".to_owned(),
            TaskGraphState::Waiting { on, .. } => {
                let ids: Vec<String> = on.iter().map(ToString::to_string).collect();
                format!("waiting on {}", ids.join(", "))
            }
            TaskGraphState::Completing { outcome } => format!("completing ({outcome})"),
        };
        let indent = "  ".repeat(depth);
        pi_js::console::log(&format!("  {indent}{} {}: {status}", node.kind, node.id));
        for child in nodes.iter().copied().filter(|c| c.owner == Some(node.id)) {
            print_node(nodes, child, depth + 1);
        }
    }

    let nodes: Vec<&TaskGraphNode> = graph.tasks.values().collect();
    for node in nodes.iter().copied().filter(|c| c.owner.is_none()) {
        print_node(&nodes, node, 0);
    }
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();

    // A fake bank: it declines an expired card at once, and confirms other charges after a moment.
    let charged: Arc<Mutex<IndexSet<String>>> = Arc::new(Mutex::new(IndexSet::new()));
    let charged_run = Arc::clone(&charged);
    let charged_abort = Arc::clone(&charged);

    let mut payment_phases: IndexMap<String, TypedPhaseHandler<PaymentInput, PaymentState, PaymentResult, ()>> =
        IndexMap::new();
    payment_phases.insert(
        "charge".to_owned(),
        Arc::new(move |task, runtime, task_context| {
            let charged = Arc::clone(&charged_run);
            Box::pin(async move {
                let card = task.input.card.clone();
                if card.starts_with("expired") {
                    let error = TaskOutcomeError {
                        message: format!("{card} declined"),
                        detail: None,
                    };
                    return runtime
                        .commit(
                            move |_tx, _current| {
                                Box::pin(async move {
                                    Ok(Some(NextTaskState::Terminal {
                                        outcome: TaskOutcome::Failed { error, result: None },
                                    }))
                                })
                            },
                            task_context,
                        )
                        .await;
                }
                charged.lock().unwrap_or_else(|e| e.into_inner()).insert(card.clone());
                runtime.sleep(task.checkpoint.at, task_context.clone()).await?;
                runtime
                    .commit(
                        move |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Completed {
                                        result: PaymentResult { card },
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
        initial: Arc::new(|_| {
            Ok(PaymentState {
                phase: "charge".to_owned(),
                at: pi_js::time::now_ms() + 100,
            })
        }),
        phases: payment_phases,
        // Each payment undoes its own effect when aborted.
        abort: Arc::new(move |task, runtime, task_context| {
            let charged = Arc::clone(&charged_abort);
            Box::pin(async move {
                let refunded = charged
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .shift_remove(&task.input.card);
                pi_js::console::log(&format!(
                    "  payment {} aborted{}",
                    task.input.card,
                    if refunded { ", refunded" } else { "" }
                ));
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

    // The checkout creates its payments as child tasks and waits for all of them.
    // With `failFast`, the first payment that fails aborts the others; the checkout
    // itself is not aborted and decides its outcome once every payment is done.
    let payment_child = payment.clone();
    let mut checkout_phases: IndexMap<String, TypedPhaseHandler<CheckoutInput, CheckoutState, String, ()>> =
        IndexMap::new();
    checkout_phases.insert(
        "pay".to_owned(),
        Arc::new(move |task, runtime, task_context| {
            let payment_tok = payment_child.clone();
            Box::pin(async move {
                let cards = task.input.cards.clone();
                let parent_id = task.id.erased();
                let conv_id = task.conversation_id;
                runtime
                    .commit(
                        move |tx, _current| {
                            let payment_tok = payment_tok.clone();
                            Box::pin(async move {
                                let mut payments = Vec::new();
                                for card in cards {
                                    let pid = tx
                                        .create_task(
                                            &payment_tok,
                                            &PaymentInput { card },
                                            TaskOptions {
                                                ownership: TaskOwnership::Task { task_id: parent_id },
                                                conversation_id: Some(conv_id),
                                                background: false,
                                            },
                                        )
                                        .await?;
                                    payments.push(pid);
                                }
                                let on: Vec<TaskId> = payments.iter().map(|id| id.erased()).collect();
                                Ok(Some(NextTaskState::Waiting {
                                    checkpoint: CheckoutState::Decide { payments },
                                    on,
                                    policy: JoinPolicy::FailFast,
                                }))
                            })
                        },
                        task_context,
                    )
                    .await
            })
        }),
    );
    checkout_phases.insert(
        "decide".to_owned(),
        Arc::new(|task, runtime, task_context| {
            Box::pin(async move {
                let payment_ids = match &task.checkpoint {
                    CheckoutState::Decide { payments } => payments.clone(),
                    CheckoutState::Pay => Vec::new(),
                };
                let erased_ids: Vec<TaskId> = payment_ids.iter().map(|id| id.erased()).collect();
                let outcomes = runtime.outcomes(&erased_ids, task_context.clone()).await?;
                let statuses: Vec<&str> = outcomes.iter().map(outcome_status).collect();
                pi_js::console::log(&format!("  payments: {}", statuses.join(", ")));
                let paid = outcomes
                    .iter()
                    .all(|outcome| matches!(outcome, TaskOutcome::Completed { .. }));
                runtime
                    .commit(
                        move |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: if paid {
                                        TaskOutcome::Completed {
                                            result: "order placed".to_owned(),
                                        }
                                    } else {
                                        TaskOutcome::Failed {
                                            error: TaskOutcomeError {
                                                message: "payment failed".to_owned(),
                                                detail: None,
                                            },
                                            result: None,
                                        }
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

    let checkout_task = define_task(TaskDefinition {
        name: "example.checkout".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(CheckoutState::Pay)),
        phases: checkout_phases,
        // Runs only after every payment is done, so the refunds have already happened.
        abort: Arc::new(|_task, runtime, task_context| {
            Box::pin(async move {
                pi_js::console::log("  checkout aborted");
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
        name: "checkout".to_owned(),
        tasks: vec![AnyTask::from(payment), AnyTask::from(checkout_task.clone())],
        ..Default::default()
    }))?;

    let directory = tempfile::TempDir::new().map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let database_path = directory.path().join("session.sqlite").to_string_lossy().into_owned();
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

    let start_checkout = |root: &Conversation, cards: Vec<&str>| {
        let root_id = root.id();
        let checkout_tok = checkout_task.clone();
        let cards: Vec<String> = cards.into_iter().map(str::to_owned).collect();
        let context = context.clone();
        let root = root.clone();
        async move {
            root.commit(
                move |tx| async move {
                    tx.create_task(
                        &checkout_tok,
                        &CheckoutInput { cards },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(root_id),
                            background: false,
                        },
                    )
                    .await
                },
                context,
            )
            .await
        }
    };

    let harness = open().await?;
    let root = harness.root(context.clone(), None).await?;

    pi_js::console::log("One card is declined:");
    let id = start_checkout(&root, vec!["visa-1", "expired-2", "visa-3", "visa-4"]).await?;
    let settled = harness.wait_for_task(id, context.clone()).await?;
    pi_js::console::log(&format!("  checkout: {}", outcome_status(&settled.outcome)));

    pi_js::console::log("The customer cancels:");
    let id = start_checkout(&root, vec!["visa-5", "visa-6", "visa-7", "visa-8"]).await?;
    harness.resume()?;
    pi_js::time::sleep(20, None).await?;
    let graph = harness.task_graph(context.clone()).await?;
    print_graph(&graph.value());
    graph.dispose()?;
    harness.abort_task(id.erased(), context.clone()).await?;
    let settled = harness.wait_for_task(id, context.clone()).await?;
    pi_js::console::log(&format!("  checkout: {}", outcome_status(&settled.outcome)));

    pi_js::console::log("The process stops while the payments run, and a new one continues:");
    let id = start_checkout(&root, vec!["visa-9", "visa-10", "visa-11", "visa-12"]).await?;
    pi_js::time::sleep(20, None).await?;
    harness.close(context.clone()).await?;

    let harness = open().await?;
    let _root = harness.root(context.clone(), None).await?;
    let settled = harness.wait_for_task(id, context.clone()).await?;
    pi_js::console::log(&format!("  checkout: {}", outcome_status(&settled.outcome)));
    harness.close(context).await?;
    Ok(())
}
