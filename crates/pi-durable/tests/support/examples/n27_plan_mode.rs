//! Port of packages/durable/test/examples/27-plan-mode.ts
#![allow(dead_code)]

// Plan mode: the user switches a conversation into a read-only planning mode, the agent writes a plan into the
// extension's own document, and switching back restores the full tool set.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::types::{Message, StopReason, TextContent};
use pi_ai::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep, Type, create_models,
    faux_assistant_message, faux_provider, faux_tool_call,
};
use pi_chord::Context;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::documents::{DocDefinitionInput, define_rewindable_conversation_doc};
use pi_durable::env::ExecutionEnv;
use pi_durable::env::node::NodeExecutionEnv;
use pi_durable::harness::types::{EnvTarget, HarnessSettings, ToolExecutionResult, ToolResultContentBlock};
use pi_durable::tools::{coding_tools, create_read_tool};
use pi_durable::types::{JsonValue, RewindableFork};
use pi_durable::{
    AgentChange, AgentExtensionsChange, AgentToolsChange, Conversation, Extension, FieldChange, Harness,
    HarnessOptions, HarnessSettingsSnapshot, MemoryStorage, ModelRef, Registry, RegistryReader,
    RootConversationOptions, SubmissionDraft, ToolControl, ToolRegistration, UserInput, create_registry,
    define_extension, define_tool, section,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct PlanDoc {
    steps: Vec<String>,
}

async fn tool_names(conv: &Conversation, context: Context) -> pi_js::Result<Vec<String>> {
    Ok(conv
        .agent(context)
        .await?
        .tools
        .into_iter()
        .map(|tool| tool.name)
        .collect())
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();

    // ─── Product code: the plan extension ───────────────────────────────────────

    // The current plan of a conversation. A fork keeps the plan it had at the fork entry.
    let plan_doc = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "app.plan",
            version: 1,
            initial: PlanDoc::default,
            migrate: None,
            checkpoint_when: None,
        },
    )?;

    let plan_doc_for_tool = plan_doc.clone();
    let submit_plan = define_tool(ToolRegistration {
        name: "submit_plan".to_owned(),
        description: "Submit the plan as a list of steps.".to_owned(),
        parameters: Type::object([("steps", Type::array(Type::string()))]).to_value(),
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        output_limits: None,
        execute: Arc::new(move |args, api, call_context| {
            let plan_tok = plan_doc_for_tool.clone();
            Box::pin(async move {
                let steps: Vec<String> = args
                    .get("steps")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default();
                let conv_id = api.conversation_id();
                api.commit(
                    move |tx| async move {
                        tx.doc_conversation(&plan_tok, conv_id)
                            .await?
                            .update(|doc| doc.steps = steps)?;
                        Ok(())
                    },
                    call_context,
                )
                .await?;
                Ok(ToolExecutionResult {
                    content: Some(vec![ToolResultContentBlock::Text(TextContent::new("Plan submitted."))]),
                    control: Some(ToolControl {
                        terminate: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
            })
        }),
    });

    let plan_ext = define_extension(Extension {
        name: "plan".to_owned(),
        tools: vec![submit_plan.clone()],
        sections: vec![section(
            "plan_mode",
            Arc::new(|_input, _ctx| {
                Box::pin(async {
                    Ok(Some(
                        "You are in plan mode. Read the code, then call submit_plan. Change nothing.".to_owned(),
                    ))
                })
            }),
        )],
        ..Default::default()
    });

    // Plan mode is a change to the conversation's agent: select the plan extension and offer only reading and
    // submitting. Clearing both returns to the host's default selection and every tool.
    let read = create_read_tool();
    let enter_plan_mode = AgentChange {
        extensions: FieldChange::Set(AgentExtensionsChange::Patch {
            add: Some(vec![plan_ext.name.clone()]),
            remove: None,
        }),
        tools: FieldChange::Set(AgentToolsChange::replace(&[read, submit_plan])),
        ..Default::default()
    };
    let leave_plan_mode = AgentChange {
        extensions: FieldChange::Clear,
        tools: FieldChange::Clear,
        ..Default::default()
    };

    // ─── Host setup ─────────────────────────────────────────────────────────────

    let directory = tempfile::TempDir::new().map_err(|err| pi_js::Error::msg(err.to_string()))?;
    tokio::fs::write(directory.path().join("server.ts"), "app.listen(3000);\n")
        .await
        .map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let dir_str = directory.path().to_string_lossy().into_owned();

    let faux = faux_provider(None);
    let models = create_models(None);
    models.set_provider(faux.provider());
    faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "read",
                IndexMap::from([("path".to_owned(), JsonValue::String("server.ts".to_owned()))]),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "submit_plan",
                IndexMap::from([(
                    "steps".to_owned(),
                    serde_json::json!(["Read PORT from the environment", "Default to 3000"]),
                )]),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Implementing step 1.".to_owned()),
            None,
        )),
    ]);

    let coding = coding_tools();
    let registry = create_registry();
    registry.install(coding.clone())?;
    registry.install(plan_ext)?;

    // Installed, but only CodingTools is selected by default: conversations opt into plan mode.
    let settings: Arc<dyn HarnessSettings> = Arc::new(HarnessSettingsSnapshot {
        extensions: Some(vec![coding]),
        ..Default::default()
    });
    let env_cwd = dir_str;
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
    pi_js::console::log(&format!("tools: {:?}", tool_names(&root, context.clone()).await?));

    root.configure(enter_plan_mode, context.clone()).await?;
    pi_js::console::log(&format!(
        "plan mode tools: {:?}",
        tool_names(&root, context.clone()).await?
    ));
    root.submit(
        SubmissionDraft::Input {
            request_id: None,
            content: UserInput::Text("Make the port configurable.".to_owned()),
            when_busy: None,
        },
        context.clone(),
    )
    .await?
    .wait(context.clone())
    .await?;
    let plan = harness
        .snapshot_conversation(&plan_doc, root.id(), context.clone())
        .await?
        .map(|doc| doc.steps);
    pi_js::console::log(&format!("plan: {plan:?}"));

    root.configure(leave_plan_mode, context.clone()).await?;
    pi_js::console::log(&format!("tools again: {:?}", tool_names(&root, context.clone()).await?));
    root.submit(
        SubmissionDraft::Input {
            request_id: None,
            content: UserInput::Text("Go ahead.".to_owned()),
            when_busy: None,
        },
        context.clone(),
    )
    .await?
    .wait(context.clone())
    .await?;

    // The model saw each switch as a system prompt change in its transcript.
    let ctx_view = root.context(context.clone()).await?;
    for message in &ctx_view.messages {
        if let Message::System(sys) = message {
            let added = sys
                .tools_added
                .as_ref()
                .map(|tools| tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>());
            let removed = sys
                .tools_removed
                .as_ref()
                .map(|tools| tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>());
            // A None section value removes it.
            pi_js::console::log(&format!(
                "system: sections={:?} added={added:?} removed={removed:?}",
                sys.sections
            ));
        }
    }

    harness.close(context).await?;
    Ok(())
}
