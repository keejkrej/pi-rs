//! Port of packages/durable/test/examples/18-print.ts
#![allow(dead_code)]

// Print mode: submit one prompt and print its answer, like `pi -p`. The host awaits its own Submission, not global idle.
// PORT: Offline adaptation omitting process.argv / OPENAI_API_KEY; uses deterministic prompt and faux provider.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::types::{AssistantContentBlock, Message, StopReason};
use pi_ai::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep, FauxToolCallOptions,
    create_models, faux_assistant_message, faux_provider, faux_text, faux_tool_call,
};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::env::ExecutionEnv;
use pi_durable::env::node::NodeExecutionEnv;
use pi_durable::harness::types::EnvTarget;
use pi_durable::tools::{create_bash_tool, create_read_tool};
use pi_durable::types::{InputSubmissionState, JsonValue, SubmissionState};
use pi_durable::{
    AgentChange, Extension, FieldChange, Harness, HarnessOptions, MemoryStorage, ModelRef, Registry, RegistryReader,
    RootConversationOptions, SectionOptions, SubmissionDraft, UserInput, create_registry, define_extension,
    section_with_options,
};

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();
    let prompt = "What is in this directory?";

    let models = create_models(None);
    let faux = faux_provider(None);
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

    let cwd = pi_js::env::cwd();
    let env_instance: Arc<dyn ExecutionEnv> = Arc::new(NodeExecutionEnv::new(cwd));
    let env = Arc::new(move |_target: EnvTarget, _ctx| {
        let env_c = Arc::clone(&env_instance);
        Box::pin(async move { Ok(Some(env_c)) }) as pi_js::BoxFuture<pi_js::Result<Option<Arc<dyn ExecutionEnv>>>>
    });
    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
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
    let settled = submission.wait(context.clone()).await?;
    match settled.record.state {
        SubmissionState::Input(InputSubmissionState::Done { answer, .. }) => {
            let entry = root
                .commit(move |tx| async move { tx.entry(answer).await }, context.clone())
                .await?;
            if let Some(Message::Assistant(msg)) = entry.and_then(|e| e.model).and_then(|mut m| m.drain(..).next()) {
                let text: String = msg
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect();
                pi_js::console::log(&text);
            }
        }
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) => {
            pi_js::console::error(&format!("unanswered: {reason}"));
        }
        other => {
            pi_js::console::error(&format!("unanswered: {:?}", other.status()));
        }
    }

    harness.close(context).await?;
    Ok(())
}
