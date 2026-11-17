//! Port of packages/durable/test/examples/11-extension-state.ts
#![allow(dead_code)]

// An extension that keeps its own per-conversation document.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::types::{Message, StopReason, TextContent};
use pi_ai::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep, Type, create_models,
    faux_assistant_message, faux_provider, faux_tool_call,
};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::documents::{DocDefinitionInput, define_rewindable_conversation_doc};
use pi_durable::harness::types::{ToolExecutionResult, ToolResultContentBlock};
use pi_durable::types::{InputSubmissionState, JsonValue, RewindableFork, SubmissionState};
use pi_durable::{
    AgentChange, Extension, FieldChange, Harness, HarnessOptions, MemoryStorage, ModelRef, Registry, RegistryReader,
    RootConversationOptions, SubmissionDraft, ToolRegistration, UserInput, create_registry, define_extension,
    define_tool, section,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Todos {
    items: Vec<String>,
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();

    // A todo list per conversation. Rewindable with `asOf`, so a fork starts with
    // the todos its parent had at the fork entry.
    let todos = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "example.todos",
            version: 1,
            initial: Todos::default,
            migrate: None,
            checkpoint_when: None,
        },
    )?;

    // The tool writes the document; the section shows it to the model before every
    // request. Nothing needs to create it up front: tx.doc_conversation() creates it on first
    // write, and the section treats a missing document as an empty list.
    let todos_for_tool = todos.clone();
    let todos_for_section = todos.clone();
    let todo_ext = define_extension(Extension {
        name: "todo".to_owned(),
        tools: vec![define_tool(ToolRegistration {
            name: "todo".to_owned(),
            description: "Add an item to your todo list".to_owned(),
            parameters: Type::object([("item", Type::string())]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(move |args, api, call_context| {
                let todos_tok = todos_for_tool.clone();
                Box::pin(async move {
                    let item = args.get("item").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
                    let conv_id = api.conversation_id();
                    let item_c = item.clone();
                    api.commit(
                        move |tx| async move {
                            tx.doc_conversation(&todos_tok, conv_id)
                                .await?
                                .update(|doc| doc.items.push(item_c))?;
                            Ok(())
                        },
                        call_context,
                    )
                    .await?;
                    Ok(ToolExecutionResult {
                        content: Some(vec![ToolResultContentBlock::Text(TextContent::new(format!(
                            "added {item}"
                        )))]),
                        ..Default::default()
                    })
                })
            }),
        })],
        sections: vec![section(
            "todos",
            Arc::new(move |input, render_context| {
                let todos_tok = todos_for_section.clone();
                Box::pin(async move {
                    let snapshot = input
                        .read
                        .snapshot_conversation(&todos_tok, input.conversation_id, render_context)
                        .await?;
                    Ok(snapshot.filter(|t| !t.items.is_empty()).map(|t| t.items.join("\n")))
                })
            }),
        )],
        ..Default::default()
    });

    let faux = faux_provider(None);
    let models = create_models(None);
    models.set_provider(faux.provider());
    let registry = create_registry();
    registry.install(todo_ext)?;

    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            models,
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

    let root = harness
        .root(
            context.clone(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    model: FieldChange::Set(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "faux-1".to_owned(),
                    }),
                    ..Default::default()
                }),
                init: None,
            }),
        )
        .await?;

    faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "todo",
                IndexMap::from([("item".to_owned(), JsonValue::String("fix the build".to_owned()))]),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Noted.".to_owned()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Working on it.".to_owned()),
            None,
        )),
    ]);

    let sub1 = root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("Remember to fix the build.".to_owned()),
                when_busy: None,
            },
            context.clone(),
        )
        .await?;
    let settled1 = sub1.wait(context.clone()).await?;
    debug_assert!(matches!(
        settled1.record.state,
        SubmissionState::Input(InputSubmissionState::Done { .. })
    ));
    let saved = harness
        .snapshot_conversation(&todos, root.id(), context.clone())
        .await?;
    pi_js::console::log(&format!("todos: {saved:?}"));

    // The next request's system prompt carries the list. The first system message
    // only announced the todo tool; no section had text yet.
    root.submit(
        SubmissionDraft::Input {
            request_id: None,
            content: UserInput::Text("What is next?".to_owned()),
            when_busy: None,
        },
        context.clone(),
    )
    .await?
    .wait(context.clone())
    .await?;

    let ctx_view = root.context(context.clone()).await?;
    let sys_messages: Vec<_> = ctx_view
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::System(sys) => {
                let tools_added = sys
                    .tools_added
                    .as_ref()
                    .map(|tools| tools.iter().map(|tool| tool.name.clone()).collect::<Vec<_>>());
                Some((sys.sections.clone(), tools_added))
            }
            _ => None,
        })
        .collect();
    pi_js::console::log(&format!("system messages: {sys_messages:?}"));

    harness.close(context).await?;
    Ok(())
}
