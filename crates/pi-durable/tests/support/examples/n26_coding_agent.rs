//! Port of packages/durable/test/examples/26-coding-agent.ts
#![allow(dead_code)]

// A small coding agent: the built-in coding tools, a prompt that knows the working directory, settings backed by the
// app's settings file, and an environment that follows the conversation's directory.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use indexmap::IndexMap;
use pi_ai::types::{Message, StopReason, UserContentBlock};
use pi_ai::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep, create_models,
    faux_assistant_message, faux_provider, faux_tool_call,
};
use pi_chord::Context;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::env::ExecutionEnv;
use pi_durable::env::node::NodeExecutionEnv;
use pi_durable::harness::types::{EnvTarget, HarnessSettings, ToolExecutionMode};
use pi_durable::tools::coding_tools;
use pi_durable::types::JsonValue;
use pi_durable::{
    AgentChange, Conversation, ConversationEntriesQuery, Extension, FieldChange, Harness, HarnessOptions,
    HarnessSettingsSnapshot, MemoryStorage, ModelRef, PartialRetryPolicy, Registry, RegistryReader,
    RootConversationOptions, SectionOptions, SubmissionDraft, TOOL_RESULT_ENTRY, UserInput, create_registry,
    define_extension, section, section_with_options,
};

struct UserSettings {
    parallel_tools: Arc<AtomicBool>,
    max_retries: Arc<AtomicU32>,
}

impl HarnessSettings for UserSettings {
    fn get(&self) -> HarnessSettingsSnapshot {
        HarnessSettingsSnapshot {
            tool_execution: Some(if self.parallel_tools.load(Ordering::Relaxed) {
                ToolExecutionMode::Parallel
            } else {
                ToolExecutionMode::Sequential
            }),
            retry: Some(PartialRetryPolicy {
                max_retries: Some(self.max_retries.load(Ordering::Relaxed)),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

async fn last_bash_output(root: &Conversation, context: Context) -> pi_js::Result<String> {
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
            UserContentBlock::Text(t) => Some(t.text.trim()),
            UserContentBlock::Image(_) => None,
        })
        .collect())
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();
    let workspace = tempfile::TempDir::new().map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let app_dir = workspace.path().join("app");
    tokio::fs::create_dir_all(&app_dir)
        .await
        .map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let workspace_str = workspace.path().to_string_lossy().into_owned();
    let app_str = app_dir.to_string_lossy().into_owned();

    // The app's own prompt, next to the coding tools.
    let coding = define_extension(Extension {
        name: "coding".to_owned(),
        sections: vec![
            section_with_options(
                "preamble",
                Arc::new(|_input, _ctx| {
                    Box::pin(async {
                        Ok(Some(
                            "You are a coding agent. Use the tools to inspect the project.".to_owned(),
                        ))
                    })
                }),
                Some(SectionOptions { tag: Some(false) }),
            ),
            section(
                "cwd",
                Arc::new(|input, _ctx| Box::pin(async move { Ok(input.env.as_ref().map(|env| env.cwd().to_owned())) })),
            ),
        ],
        ..Default::default()
    });
    let registry = create_registry();
    registry.install(coding_tools())?;
    registry.install(coding)?;

    // Settings the user edits while the agent runs, like a settings.json the app watches. The getters make every
    // Harness read see the current file; nothing is copied or stored.
    let parallel_tools = Arc::new(AtomicBool::new(true));
    let max_retries = Arc::new(AtomicU32::new(3));
    let settings: Arc<dyn HarnessSettings> = Arc::new(UserSettings {
        parallel_tools: Arc::clone(&parallel_tools),
        max_retries,
    });

    // The Harness calls this for every tool call and request with the conversation's `cwd`, so changing a conversation's
    // directory needs no restart.
    let default_cwd = workspace_str.clone();
    let env = Arc::new(move |target: EnvTarget, _ctx: Context| {
        let cwd = target.cwd.unwrap_or_else(|| default_cwd.clone());
        Box::pin(async move { Ok(Some(Arc::new(NodeExecutionEnv::new(cwd)) as Arc<dyn ExecutionEnv>)) })
            as pi_js::BoxFuture<pi_js::Result<Option<Arc<dyn ExecutionEnv>>>>
    });

    let faux = faux_provider(None);
    let models = create_models(None);
    models.set_provider(faux.provider());
    let pwd = || {
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "bash",
                IndexMap::from([("command".to_owned(), JsonValue::String("pwd".to_owned()))]),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        ))
    };
    faux.set_responses(vec![
        pwd(),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Done.".to_owned()),
            None,
        )),
        pwd(),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Done.".to_owned()),
            None,
        )),
    ]);

    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            models,
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: Some(settings),
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
                    model: FieldChange::Set(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "faux-1".to_owned(),
                    }),
                    cwd: FieldChange::Set(workspace_str),
                    ..Default::default()
                }),
                init: None,
            }),
        )
        .await?;

    root.submit(
        SubmissionDraft::Input {
            request_id: None,
            content: UserInput::Text("Where are we?".to_owned()),
            when_busy: None,
        },
        context.clone(),
    )
    .await?
    .wait(context.clone())
    .await?;
    pi_js::console::log(&format!(
        "bash ran in: {}",
        last_bash_output(&root, context.clone()).await?
    ));

    // The user switches the project directory and turns off parallel tools. Both apply from the next use.
    root.configure(
        AgentChange {
            cwd: FieldChange::Set(app_str),
            ..Default::default()
        },
        context.clone(),
    )
    .await?;
    parallel_tools.store(false, Ordering::Relaxed);
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
    pi_js::console::log(&format!(
        "bash ran in: {}",
        last_bash_output(&root, context.clone()).await?
    ));

    harness.close(context).await?;
    Ok(())
}
