//! Port of packages/durable/test/examples/03-owned-conversations.ts
#![allow(dead_code)]

// A background task that owns a child conversation.
// Run via `spec_usage::examples_smoke`.

use std::marker::PhantomData;
use std::sync::Arc;

use indexmap::IndexMap;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::documents::{DocDefinitionInput, define_conversation_doc};
use pi_durable::tasks::{TaskDefinition, TypedPhaseHandler, define_task};
use pi_durable::types::{ConversationId, ConversationOwnership, LatestFork, TaskOptions, TaskOwnership};
use pi_durable::{MemoryStorage, create_session};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SupervisorState {
    phase: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisteredAgent {
    conversation_id: ConversationId,
    request_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct AgentRegistry {
    agents: IndexMap<String, RegisteredAgent>,
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();
    let session = create_session(Arc::new(MemoryStorage::new()));

    // A typical agent setup: a background task supervises a helper conversation,
    // and the main conversation keeps a registry that maps agent names to their
    // conversations. All three are created in one commit, so after a crash either
    // all of them exist or none do.

    // A task definition needs a name, a version, the task's starting state, a
    // handler for every phase, and an abort handler (12-tasks runs a task).
    // This example only creates the task record; a plain Session never runs it.
    let mut phases = IndexMap::new();
    phases.insert(
        "ready".to_owned(),
        Arc::new(|_, _, _| Box::pin(async { Ok(()) })) as TypedPhaseHandler<(), SupervisorState, (), ()>,
    );
    let supervisor = define_task(TaskDefinition {
        name: "example.supervisor".to_owned(),
        version: 1,
        initial: Arc::new(|_| {
            Ok(SupervisorState {
                phase: "ready".to_owned(),
            })
        }),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: PhantomData,
    });

    // "latest" keeps only the current value. "initial" means forks of this
    // conversation start without a registry, so a child doesn't inherit its
    // parent's list of agents.
    let agent_registry = define_conversation_doc(
        LatestFork::Initial,
        DocDefinitionInput {
            kind: "example.agent-registry",
            version: 1,
            initial: AgentRegistry::default,
            migrate: None,
            checkpoint_when: None,
        },
    )?;

    let main = session
        .commit(
            |tx| async move { tx.create_conversation(ConversationOwnership::Ownerless).await },
            context.clone(),
        )
        .await?;

    let supervisor_c = supervisor.clone();
    let registry_c = agent_registry.clone();
    let (supervisor_id, child) = session
        .commit(
            move |tx| async move {
                // `background: true` means the task is side work: waiting for the main
                // conversation to finish does not wait for it.
                let supervisor_id = tx
                    .create_task(
                        &supervisor_c,
                        &(),
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(main.id),
                            background: true,
                        },
                    )
                    .await?;

                // The child records that it belongs to the supervisor task. The task was
                // created a few lines above in this same commit, which is allowed.
                let child = tx
                    .create_conversation(ConversationOwnership::Task {
                        task_id: supervisor_id.erased(),
                    })
                    .await?;

                // requestId is a fixed name for the child's first message. Later code sends
                // that message using this requestId, so a retry after a crash cannot
                // deliver it twice.
                tx.doc_conversation(&registry_c, main.id).await?.update(|doc| {
                    doc.agents.insert(
                        "researcher".to_owned(),
                        RegisteredAgent {
                            conversation_id: child.id,
                            request_id: format!("researcher:first-message:{supervisor_id}"),
                        },
                    );
                })?;
                Ok((supervisor_id, child))
            },
            context.clone(),
        )
        .await?;

    pi_js::console::log(&format!("supervisor task: {supervisor_id}"));
    pi_js::console::log(&format!("child conversation: {child:?}"));
    let snapshot = session
        .snapshot_conversation(&agent_registry, main.id, context.clone())
        .await?;
    pi_js::console::log(&format!("registry: {snapshot:?}"));

    session.close(context).await?;
    Ok(())
}
