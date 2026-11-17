//! Port of packages/durable/test/examples/28-reviewer.ts
#![allow(dead_code)]

// A reviewer agent next to the main one: a cheaper model, a review role and loop, read-only tools, and its own
// checkout of the project.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, UserContentBlock, UserMessageContent,
};
use pi_ai::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxModelDefinition, FauxResponseStep,
    RegisterFauxProviderOptions, create_models, faux_assistant_message, faux_provider, faux_tool_call,
};
use pi_chord::Context;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::env::ExecutionEnv;
use pi_durable::env::node::NodeExecutionEnv;
use pi_durable::harness::types::{EnvTarget, HarnessSettings};
use pi_durable::tools::{coding_tools, create_read_tool};
use pi_durable::types::{ConversationOwnership, JsonValue};
use pi_durable::{
    ASSISTANT_ENTRY, AgentChange, AgentExtensionsChange, AgentToolsChange, ConversationCreateOptions,
    ConversationEntriesQuery, Extension, FieldChange, GENERATION_TASK, GenerationHooks, Harness, HarnessOptions,
    HarnessSettingsSnapshot, MemoryStorage, ModelRef, Registry, RegistryReader, RootConversationOptions,
    SubmissionDraft, UserInput, create_registry, define_extension, hook, section,
};

const DONE: &str = "No further findings.";

fn text_of(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            AssistantContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect()
}

fn faux_model_def(id: &str) -> FauxModelDefinition {
    FauxModelDefinition {
        id: id.to_owned(),
        name: None,
        reasoning: None,
        input: None,
        input_limits: None,
        cost: None,
        context_window: None,
        max_tokens: None,
    }
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();

    // ─── Product code: the reviewer extension ───────────────────────────────────

    // A role and a review loop: every answer that still has findings gets a second pass.
    let reviewer_ext = define_extension(Extension {
        name: "reviewer".to_owned(),
        sections: vec![section(
            "role",
            Arc::new(|_input, _ctx| {
                Box::pin(async {
                    Ok(Some(
                        "You review diffs. Report problems as a list. Never edit files.".to_owned(),
                    ))
                })
            }),
        )],
        hooks: vec![hook(
            &*GENERATION_TASK,
            GenerationHooks {
                on_yield: Some(Arc::new(|answer, _api, _ctx| {
                    Box::pin(async move {
                        if text_of(&answer).contains(DONE) {
                            Ok(None)
                        } else {
                            Ok(Some(UserInput::Text(format!(
                                "Look again for anything you missed. Say \"{DONE}\" when there is nothing left."
                            ))))
                        }
                    })
                })),
                ..Default::default()
            },
        )],
        ..Default::default()
    });

    // ─── Host setup ─────────────────────────────────────────────────────────────

    // The reviewer works in its own checkout, in practice a `git worktree add`.
    let worktree = tempfile::TempDir::new().map_err(|err| pi_js::Error::msg(err.to_string()))?;
    tokio::fs::write(
        worktree.path().join("user.ts"),
        "export const name = (user) => user.name;\n",
    )
    .await
    .map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let worktree_str = worktree.path().to_string_lossy().into_owned();

    let faux = faux_provider(Some(RegisterFauxProviderOptions {
        models: Some(vec![faux_model_def("big"), faux_model_def("small")]),
        ..Default::default()
    }));
    let models = create_models(None);
    models.set_provider(faux.provider());
    faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "read",
                IndexMap::from([("path".to_owned(), JsonValue::String("user.ts".to_owned()))]),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("1. `name` does not handle a missing user.".to_owned()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text(format!("2. `user` has no type. {DONE}")),
            None,
        )),
    ]);

    let coding = coding_tools();
    let registry = create_registry();
    registry.install(coding.clone())?;
    registry.install(reviewer_ext.clone())?;

    // The main agent selects only CodingTools; the reviewer opts in.
    let settings: Arc<dyn HarnessSettings> = Arc::new(HarnessSettingsSnapshot {
        extensions: Some(vec![coding.clone()]),
        ..Default::default()
    });
    let env = Arc::new(|target: EnvTarget, _ctx: Context| {
        let cwd = target.cwd.unwrap_or_else(pi_js::env::cwd);
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

    harness
        .root(
            context.clone(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    model: FieldChange::Set(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "big".to_owned(),
                    }),
                    ..Default::default()
                }),
                init: None,
            }),
        )
        .await?;

    // Everything the reviewer is, stored on its conversation: the model, exactly these extensions in this order, only
    // the read tool, and its directory. A restart keeps all of it.
    let reviewer = harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: Some(AgentChange {
                    model: FieldChange::Set(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "small".to_owned(),
                    }),
                    extensions: FieldChange::Set(AgentExtensionsChange::replace(&[coding, reviewer_ext])),
                    tools: FieldChange::Set(AgentToolsChange::replace(&[create_read_tool()])),
                    cwd: FieldChange::Set(worktree_str.clone()),
                    ..Default::default()
                }),
                init: None,
            },
            context.clone(),
        )
        .await?;
    let agent = reviewer.agent(context.clone()).await?;
    let ext_names: Vec<&str> = agent.extensions.iter().map(|ext| ext.name.as_str()).collect();
    let tool_names: Vec<&str> = agent.tools.iter().map(|tool| tool.name.as_str()).collect();
    pi_js::console::log(&format!(
        "reviewer: {:?} {ext_names:?} {tool_names:?} {}",
        agent.model.as_ref().map(|m| m.model_id.as_str()),
        agent.cwd.as_deref() == Some(worktree_str.as_str())
    ));

    reviewer
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("Review user.ts.".to_owned()),
                when_busy: None,
            },
            context.clone(),
        )
        .await?
        .wait(context.clone())
        .await?;
    let page = reviewer
        .entries(ConversationEntriesQuery::default(), 20, None, context.clone())
        .await?;
    for entry in page.items.iter().rev() {
        match entry.model.as_ref().and_then(|m| m.first()) {
            Some(Message::User(u)) => {
                let text = match &u.content {
                    UserMessageContent::Text(s) => s.clone(),
                    UserMessageContent::Blocks(b) => b
                        .iter()
                        .filter_map(|part| match part {
                            UserContentBlock::Text(t) => Some(t.text.as_str()),
                            UserContentBlock::Image(_) => None,
                        })
                        .collect(),
                };
                pi_js::console::log(&format!("> {text}"));
            }
            Some(Message::Assistant(a)) if ASSISTANT_ENTRY.matches(Some(entry)) => {
                let txt = text_of(a);
                if !txt.is_empty() {
                    pi_js::console::log(&format!("reviewer: {txt}"));
                }
            }
            _ => {}
        }
    }

    harness.close(context).await?;
    Ok(())
}
