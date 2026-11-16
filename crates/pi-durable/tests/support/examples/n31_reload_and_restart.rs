//! Port of packages/durable/test/examples/31-reload-and-restart.ts
#![allow(dead_code)]

// Reload an extension while a call runs, then restart the process. Running work finishes on the code it started
// with; stored choices are names, so they survive a restart and bind to whatever code the new process installs.
// Run via `spec_usage::examples_smoke`.

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::types::{Message, StopReason, TextContent, UserContentBlock};
use pi_ai::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep, Type, create_models,
    faux_assistant_message, faux_provider, faux_tool_call,
};
use pi_chord::Context;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::harness::types::{ToolExecutionResult, ToolResultContentBlock};
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::types::Storage;
use pi_durable::{
    AgentChange, AgentExtensionsChange, Conversation, ConversationEntriesQuery, Extension, FieldChange, Harness,
    HarnessOptions, ModelRef, Registry, RegistryImpl, RegistryReader, RootConversationOptions, SubmissionDraft,
    TOOL_RESULT_ENTRY, ToolRegistration, UserInput, create_registry, define_extension, define_tool,
};
use tokio::sync::{Notify, watch};

// Stand-in for code loaded from disk: each call builds the extension as the current version.
fn load_versioned(version: &'static str, started: Arc<Notify>, gate: Arc<Mutex<watch::Receiver<bool>>>) -> Extension {
    define_extension(Extension {
        name: "versioned".to_owned(),
        tools: vec![define_tool(ToolRegistration {
            name: "version".to_owned(),
            description: "Report the tool's code version".to_owned(),
            parameters: Type::object::<_, &str>([]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(move |_args, _api, _ctx| {
                let started = Arc::clone(&started);
                let gate = Arc::clone(&gate);
                Box::pin(async move {
                    started.notify_one();
                    let mut rx = gate.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let _ = rx.wait_for(|open| *open).await;
                    Ok(ToolExecutionResult {
                        content: Some(vec![ToolResultContentBlock::Text(TextContent::new(version))]),
                        ..Default::default()
                    })
                })
            }),
        })],
        ..Default::default()
    })
}

async fn last_result(root: &Conversation, context: Context) -> pi_js::Result<String> {
    let page = root
        .entries(ConversationEntriesQuery::default(), 10, None, context)
        .await?;
    let Some(entry) = page
        .items
        .into_iter()
        .find(|entry| TOOL_RESULT_ENTRY.matches(Some(entry)))
    else {
        return Ok(String::new());
    };
    let Some(Message::ToolResult(result)) = entry.model.and_then(|mut m| m.drain(..).next()) else {
        return Ok(String::new());
    };
    Ok(result
        .content
        .iter()
        .filter_map(|part| match part {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect())
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();

    let started = Arc::new(Notify::new());
    let (gate_tx, gate_rx) = watch::channel(false);
    let gate = Arc::new(Mutex::new(gate_rx));

    let faux = faux_provider(None);
    let models = create_models(None);
    models.set_provider(faux.provider());
    let call_version = || {
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "version",
                IndexMap::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        ))
    };
    faux.set_responses(vec![
        call_version(),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Done.".to_owned()),
            None,
        )),
        call_version(),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Done.".to_owned()),
            None,
        )),
    ]);

    let directory = tempfile::TempDir::new().map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let db_path = directory.path().join("session.sqlite").to_string_lossy().into_owned();
    let open = |registry: &Arc<RegistryImpl>| {
        let db_path = db_path.clone();
        let models = models.clone();
        let registry = Arc::clone(registry) as Arc<dyn RegistryReader>;
        let context = context.clone();
        async move {
            let storage = open_node_sqlite_storage(&db_path, NodeSqliteStorageOptions::default()).await?;
            Harness::open(
                Arc::new(storage) as Arc<dyn Storage>,
                HarnessOptions {
                    models,
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

    // First process.
    let v1 = load_versioned("v1", Arc::clone(&started), Arc::clone(&gate));
    let registry = create_registry();
    registry.install(v1.clone())?;
    let harness = open(&registry).await?;
    let root = harness
        .root(
            context.clone(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    model: FieldChange::Set(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "faux-1".to_owned(),
                    }),
                    // Selected by name. The name is what is stored, never the code.
                    extensions: FieldChange::Set(AgentExtensionsChange::replace(&[v1])),
                    ..Default::default()
                }),
                init: None,
            }),
        )
        .await?;

    // The file changes while a call runs: the running call finishes on v1, the next call uses v2.
    let submission = root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("Which version?".to_owned()),
                when_busy: None,
            },
            context.clone(),
        )
        .await?;
    started.notified().await;
    registry.install(load_versioned("v2", Arc::clone(&started), Arc::clone(&gate)))?;
    let _ = gate_tx.send(true);
    submission.wait(context.clone()).await?;
    pi_js::console::log(&format!(
        "call running during the reload: {}",
        last_result(&root, context.clone()).await?
    ));

    root.submit(
        SubmissionDraft::Input {
            request_id: None,
            content: UserInput::Text("And now?".to_owned()),
            when_busy: None,
        },
        context.clone(),
    )
    .await?
    .wait(context.clone())
    .await?;
    pi_js::console::log(&format!("next call: {}", last_result(&root, context.clone()).await?));
    harness.close(context.clone()).await?;

    // Second process: the conversation still selects "versioned", but this process has not installed it yet.
    let registry = create_registry();
    let harness = open(&registry).await?;
    let root = harness.root(context.clone(), None).await?;
    let tool_names = |conv: &Conversation| {
        let conv = conv.clone();
        let context = context.clone();
        async move {
            Ok::<_, pi_js::Error>(
                conv.agent(context)
                    .await?
                    .tools
                    .into_iter()
                    .map(|tool| tool.name)
                    .collect::<Vec<_>>(),
            )
        }
    };
    pi_js::console::log(&format!(
        "after restart, before install: {:?}",
        tool_names(&root).await?
    ));
    registry.install(load_versioned("v3", started, gate))?;
    pi_js::console::log(&format!("after install: {:?}", tool_names(&root).await?));

    harness.close(context).await?;
    Ok(())
}
