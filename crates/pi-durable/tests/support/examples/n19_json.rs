//! Port of packages/durable/test/examples/19-json.ts
#![allow(dead_code)]

// JSON mode: stream one prompt's run as JSON lines, like `pi --mode json`.
// PORT: Offline adaptation using TempDir for SQLite storage and scripted faux provider (omits process.argv / OPENAI_API_KEY).
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::types::StopReason;
use pi_ai::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep, FauxToolCallOptions,
    RegisterFauxProviderOptions, create_models, faux_assistant_message, faux_provider, faux_text, faux_tool_call,
};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::delta::Op;
use pi_durable::env::ExecutionEnv;
use pi_durable::env::node::NodeExecutionEnv;
use pi_durable::harness::types::{ConversationView, EnvTarget};
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::tools::{create_bash_tool, create_read_tool};
use pi_durable::types::{JsonValue, Storage};
use pi_durable::{
    AgentChange, Extension, FieldChange, Harness, HarnessOptions, ModelRef, Registry, RegistryReader,
    RootConversationOptions, SectionOptions, SubmissionDraft, UserInput, create_registry, define_extension,
    section_with_options, watch_events,
};
use serde::Serialize;

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Events,
    Ops,
}

#[derive(Serialize)]
struct ViewLine {
    view: ConversationView,
}

#[derive(Serialize)]
struct OpsLine {
    ops: Vec<Op>,
}

fn print_json_line<T: Serialize>(value: &T) {
    pi_js::console::stdout_write(&format!("{}\n", pi_js::json::stringify(value)));
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();
    let mode = OutputMode::Events;
    let prompt = "What is in this directory?";

    let temp_dir = tempfile::TempDir::new().map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let location = temp_dir
        .path()
        .join("pi-durable-json.sqlite")
        .to_string_lossy()
        .into_owned();
    let storage: Arc<dyn Storage> =
        Arc::new(open_node_sqlite_storage(&location, NodeSqliteStorageOptions::default()).await?);

    let models = create_models(None);
    let faux = faux_provider(Some(RegisterFauxProviderOptions {
        tokens_per_second: Some(200.0),
        ..Default::default()
    }));
    models.set_provider(faux.provider());
    let model = ModelRef {
        provider: "faux".to_owned(),
        model_id: "faux-1".to_owned(),
    };
    faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "bash",
                IndexMap::from([("command".to_owned(), JsonValue::String("ls".to_owned()))]),
                Some(FauxToolCallOptions {
                    id: Some("call-1".to_owned()),
                }),
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text(
                "This directory holds the durable package sources, tests, and docs.",
            ))),
            None,
        )),
    ]);

    let registry = create_registry();
    registry.install(define_extension(Extension {
        name: "coding".to_owned(),
        tools: vec![create_read_tool(), create_bash_tool(None)],
        sections: vec![section_with_options(
            "preamble",
            Arc::new(|_input, _ctx| Box::pin(async { Ok(Some("You are a concise coding assistant.".to_owned())) })),
            Some(SectionOptions { tag: Some(false) }),
        )],
        ..Default::default()
    }))?;

    let env_instance: Arc<dyn ExecutionEnv> = Arc::new(NodeExecutionEnv::new(pi_js::env::cwd()));
    let env = Arc::new(move |_target: EnvTarget, _ctx| {
        let env_c = Arc::clone(&env_instance);
        Box::pin(async move { Ok(Some(env_c)) }) as pi_js::BoxFuture<pi_js::Result<Option<Arc<dyn ExecutionEnv>>>>
    });
    let harness = Harness::open(
        storage,
        HarnessOptions {
            models,
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: None,
            env: Some(env),
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
                    model: FieldChange::Set(model),
                    ..Default::default()
                }),
                init: None,
            }),
        )
        .await?;

    // Attach before submitting, so the stream covers the whole run.
    let event_stream;
    let view_watch;
    match mode {
        OutputMode::Events => {
            let stream = watch_events(&root, context.clone()).await?;
            print_json_line(&stream.snapshot);
            stream.start(|events, _ctx| {
                Box::pin(async move {
                    for event in &events {
                        print_json_line(event);
                    }
                    Ok(())
                })
            })?;
            event_stream = Some(stream);
            view_watch = None;
        }
        OutputMode::Ops => {
            let watch = root.watch(context.clone()).await?;
            print_json_line(&ViewLine { view: watch.value() });
            watch.start(Arc::new(|_value, ops, _ctx| {
                Box::pin(async move {
                    print_json_line(&OpsLine { ops });
                    Ok(())
                })
            }))?;
            event_stream = None;
            view_watch = Some(watch);
        }
    }

    let submission = root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(prompt.to_owned()),
                when_busy: None,
            },
            context.clone(),
        )
        .await?;
    submission.wait(context.clone()).await?;
    harness.wait_for_idle(context.clone()).await?;
    // Let the last batch reach the listener before stopping.
    pi_js::time::sleep(0, None).await?;
    if let Some(stream) = event_stream {
        stream.stop().await;
    }
    if let Some(watch) = view_watch {
        watch.stop().await;
    }
    harness.close(context).await?;
    pi_js::console::error(&format!("sqlite storage: {location}"));
    Ok(())
}
