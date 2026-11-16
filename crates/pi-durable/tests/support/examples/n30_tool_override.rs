//! Port of packages/durable/test/examples/30-tool-override.ts
#![allow(dead_code)]

// Replace a tool for some conversations, and decorate whichever tool wins: a bash that runs inside a Python
// virtualenv, and a wrapper that times every bash call.
// Run via `spec_usage::examples_smoke`.

use std::sync::{Arc, Mutex};

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
use pi_durable::harness::types::{EnvTarget, HarnessSettings};
use pi_durable::tools::{BashToolOptions, coding_tools, create_bash_tool};
use pi_durable::types::{ConversationOwnership, JsonValue};
use pi_durable::{
    AgentChange, AgentExtensionsChange, Conversation, ConversationCreateOptions, ConversationEntriesQuery, Extension,
    FieldChange, Harness, HarnessOptions, HarnessSettingsSnapshot, MemoryStorage, ModelRef, Registry, RegistryReader,
    RootConversationOptions, SubmissionDraft, TOOL_RESULT_ENTRY, ToolRegistration, UserInput, create_registry,
    define_extension, wrap_tool,
};

async fn probe_bash(conversation: &Conversation, context: Context) -> pi_js::Result<String> {
    conversation
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("Which venv?".to_owned()),
                when_busy: None,
            },
            context.clone(),
        )
        .await?
        .wait(context.clone())
        .await?;
    let page = conversation
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

    // ─── Product code ───────────────────────────────────────────────────────────

    // A bash with the same name: where selected after CodingTools, it replaces CodingTools' bash in place.
    let venv = define_extension(Extension {
        name: "venv".to_owned(),
        tools: vec![create_bash_tool(Some(BashToolOptions {
            command_prefix: Some("source .venv/bin/activate".to_owned()),
            prepare: None,
        }))],
        ..Default::default()
    });

    // Wraps the bash that won, whichever it is. Wrappers never capture a base tool, so reloading either bash keeps it.
    let timings: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let timings_wrap = Arc::clone(&timings);
    let timing = define_extension(Extension {
        name: "timing".to_owned(),
        wraps: vec![wrap_tool(&create_bash_tool(None), move |bash| {
            let orig = Arc::clone(&bash.execute);
            let timings = Arc::clone(&timings_wrap);
            ToolRegistration {
                execute: Arc::new(move |args, api, call_context| {
                    let orig = Arc::clone(&orig);
                    let timings = Arc::clone(&timings);
                    Box::pin(async move {
                        let start = pi_js::time::now_ms();
                        let res = orig(args, api, call_context).await;
                        let elapsed = pi_js::time::now_ms() - start;
                        timings.lock().unwrap_or_else(|e| e.into_inner()).push(elapsed);
                        res
                    })
                }),
                ..bash
            }
        })],
        ..Default::default()
    });

    // ─── Host setup ─────────────────────────────────────────────────────────────

    let project = tempfile::TempDir::new().map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let venv_bin = project.path().join(".venv/bin");
    tokio::fs::create_dir_all(&venv_bin)
        .await
        .map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let project_str = project.path().to_string_lossy().into_owned();
    tokio::fs::write(
        venv_bin.join("activate"),
        format!("export VIRTUAL_ENV=\"{project_str}/.venv\"\n"),
    )
    .await
    .map_err(|err| pi_js::Error::msg(err.to_string()))?;

    let faux = faux_provider(None);
    let models = create_models(None);
    models.set_provider(faux.provider());
    let probe = || {
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "bash",
                IndexMap::from([(
                    "command".to_owned(),
                    JsonValue::String("echo venv: $VIRTUAL_ENV".to_owned()),
                )]),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        ))
    };
    faux.set_responses(vec![
        probe(),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Done.".to_owned()),
            None,
        )),
        probe(),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Done.".to_owned()),
            None,
        )),
    ]);

    let coding = coding_tools();
    let registry = create_registry();
    registry.install(coding.clone())?;
    registry.install(timing.clone())?;
    registry.install(venv.clone())?;

    // Venv is installed but not selected by default.
    let settings: Arc<dyn HarnessSettings> = Arc::new(HarnessSettingsSnapshot {
        extensions: Some(vec![coding, timing]),
        ..Default::default()
    });
    let env_cwd = project_str.clone();
    let env = Arc::new(move |_target: EnvTarget, _ctx: Context| {
        let cwd = env_cwd.clone();
        Box::pin(async move { Ok(Some(Arc::new(NodeExecutionEnv::new(cwd)) as Arc<dyn ExecutionEnv>)) })
            as pi_js::BoxFuture<pi_js::Result<Option<Arc<dyn ExecutionEnv>>>>
    });
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

    let model = ModelRef {
        provider: "faux".to_owned(),
        model_id: "faux-1".to_owned(),
    };
    let plain = harness
        .root(
            context.clone(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    model: FieldChange::Set(model.clone()),
                    ..Default::default()
                }),
                init: None,
            }),
        )
        .await?;
    let python = harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: Some(AgentChange {
                    model: FieldChange::Set(model),
                    extensions: FieldChange::Set(AgentExtensionsChange::Patch {
                        add: Some(vec![venv.name]),
                        remove: None,
                    }),
                    ..Default::default()
                }),
                init: None,
            },
            context.clone(),
        )
        .await?;

    pi_js::console::log(&format!(
        "plain conversation: {}",
        probe_bash(&plain, context.clone()).await?
    ));
    pi_js::console::log(&format!(
        "python conversation: {}",
        probe_bash(&python, context.clone())
            .await?
            .replace(&project_str, "<project>")
    ));
    let call_count = timings.lock().unwrap_or_else(|e| e.into_inner()).len();
    pi_js::console::log(&format!("timed bash calls: {call_count}"));

    harness.close(context).await?;
    Ok(())
}
