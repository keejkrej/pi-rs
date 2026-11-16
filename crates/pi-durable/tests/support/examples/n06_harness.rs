//! Port of packages/durable/test/examples/06-harness.ts
#![allow(dead_code)]

// Open a Harness with a registry of extensions.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use pi_ai::types::{ModelThinkingLevel, TextContent};
use pi_ai::{Type, create_models};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::documents::{DocDefinitionInput, define_rewindable_conversation_doc};
use pi_durable::harness::types::{ToolExecutionResult, ToolResultContentBlock};
use pi_durable::types::RewindableFork;
use pi_durable::{
    AGENT_DOC, AgentChange, Extension, FieldChange, Harness, HarnessOptions, MemoryStorage, Registry, RegistryReader,
    RootConversationOptions, ToolRegistration, create_registry, define_extension, define_tool,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Notes {
    text: String,
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();

    // A Harness is a Session plus conversation handles and durable tasks. Extension
    // code (tools, prompt sections, hooks, tasks) comes in named extensions
    // installed in a registry the application owns. Nothing in the registry is
    // saved; it is this process's code.
    let read = define_tool(ToolRegistration {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: Type::object([("path", Type::string())]).to_value(),
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        output_limits: None,
        execute: Arc::new(|args, _api, _ctx| {
            Box::pin(async move {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                Ok(ToolExecutionResult {
                    content: Some(vec![ToolResultContentBlock::Text(TextContent::new(format!(
                        "contents of {path}"
                    )))]),
                    ..Default::default()
                })
            })
        }),
    });
    let files = define_extension(Extension {
        name: "files".to_owned(),
        tools: vec![read],
        ..Default::default()
    });
    let registry = create_registry();
    registry.install(files)?;

    // `models` is pi-ai's model access; generation calls models through it.
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

    let notes = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "example.notes",
            version: 1,
            initial: || Notes { text: String::new() },
            migrate: None,
            checkpoint_when: None,
        },
    )?;

    // The root conversation always has ID 1. The first root() call creates it,
    // its built-in documents, the `agent` choices, and whatever `init` writes, all
    // in one commit. Later calls, including after a restart, return it and ignore
    // both options.
    let notes_c = notes.clone();
    let root = harness
        .root(
            context.clone(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    thinking_level: FieldChange::Set(ModelThinkingLevel::Low),
                    ..Default::default()
                }),
                init: Some(Arc::new(move |tx, root_id| {
                    let notes_c = notes_c.clone();
                    Box::pin(async move {
                        tx.doc_conversation(&notes_c, root_id)
                            .await?
                            .update(|doc| doc.text = "root notes".to_owned())?;
                        Ok(())
                    })
                })),
            }),
        )
        .await?;

    let root_notes = harness
        .snapshot_conversation(&notes, root.id(), context.clone())
        .await?;
    pi_js::console::log(&format!("root: {} {root_notes:?}", root.id()));
    // The stored choices are names; agent() resolves them against the registry.
    let stored_agent = harness
        .snapshot_conversation(&*AGENT_DOC, root.id(), context.clone())
        .await?;
    pi_js::console::log(&format!("stored agent: {stored_agent:?}"));
    let agent = root.agent(context.clone()).await?;
    let ext_names: Vec<&str> = agent.extensions.iter().map(|ext| ext.name.as_str()).collect();
    let tool_names: Vec<&str> = agent.tools.iter().map(|tool| tool.name.as_str()).collect();
    pi_js::console::log(&format!(
        "resolved: {:?} {ext_names:?} {tool_names:?}",
        agent.thinking_level
    ));

    harness.close(context).await?;
    Ok(())
}
