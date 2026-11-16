//! Port of packages/durable/test/examples/29-sandbox-per-conversation.ts
#![allow(dead_code)]

// One sandbox per conversation: the app records each conversation's sandbox in its own document, and the Harness
// environment function looks it up for every tool call. Here a sandbox is a directory; a hosted product would return
// an ExecutionEnv that runs inside the conversation's container.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::types::StopReason;
use pi_ai::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep, create_models,
    faux_assistant_message, faux_provider, faux_tool_call,
};
use pi_chord::Context;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::documents::{DocDefinitionInput, define_conversation_doc};
use pi_durable::env::ExecutionEnv;
use pi_durable::env::node::NodeExecutionEnv;
use pi_durable::harness::types::EnvTarget;
use pi_durable::tools::coding_tools;
use pi_durable::types::{ConversationOwnership, JsonValue, LatestFork};
use pi_durable::{
    AgentChange, ConversationCreateOptions, FieldChange, Harness, HarnessOptions, MemoryStorage, ModelRef, Registry,
    RegistryReader, SubmissionDraft, UserInput, create_registry,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct SandboxDoc {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();

    // Which sandbox a conversation runs in. `fork: "initial"`: a fork gets no sandbox until the app assigns one.
    let sandbox_doc = define_conversation_doc(
        LatestFork::Initial,
        DocDefinitionInput {
            kind: "app.sandbox",
            version: 1,
            initial: SandboxDoc::default,
            migrate: None,
            checkpoint_when: None,
        },
    )?;

    let faux = faux_provider(None);
    let models = create_models(None);
    models.set_provider(faux.provider());
    let note = |text: &str| {
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "write",
                IndexMap::from([
                    ("path".to_owned(), JsonValue::String("note.txt".to_owned())),
                    ("content".to_owned(), JsonValue::String(text.to_owned())),
                ]),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        ))
    };
    faux.set_responses(vec![
        note("from alice"),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Saved.".to_owned()),
            None,
        )),
        note("from bob"),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Saved.".to_owned()),
            None,
        )),
    ]);

    let registry = create_registry();
    registry.install(coding_tools())?;

    let sandbox_for_env = sandbox_doc.clone();
    // Committed reads only; a conversation without a sandbox gets no environment, so its tools fail cleanly.
    let env = Arc::new(move |target: EnvTarget, env_context: Context| {
        let sandbox_tok = sandbox_for_env.clone();
        Box::pin(async move {
            let sandbox = target
                .read
                .snapshot_conversation(&sandbox_tok, target.conversation_id, env_context)
                .await?;
            Ok(sandbox
                .and_then(|s| s.path)
                .map(|path| Arc::new(NodeExecutionEnv::new(path)) as Arc<dyn ExecutionEnv>))
        }) as pi_js::BoxFuture<pi_js::Result<Option<Arc<dyn ExecutionEnv>>>>
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

    // Each user's conversation gets a fresh sandbox in the creating commit.
    let model = ModelRef {
        provider: "faux".to_owned(),
        model_id: "faux-1".to_owned(),
    };
    let conversation_for = |prefix: &str| {
        let harness = harness.clone();
        let model = model.clone();
        let sandbox_tok = sandbox_doc.clone();
        let context = context.clone();
        let prefix = prefix.to_owned();
        async move {
            let dir = tempfile::Builder::new()
                .prefix(&format!("pi-durable-sandbox-{prefix}-"))
                .tempdir()
                .map_err(|err| pi_js::Error::msg(err.to_string()))?;
            let path = dir.path().to_string_lossy().into_owned();
            let path_for_init = path.clone();
            let conversation = harness
                .create_conversation(
                    ConversationCreateOptions {
                        ownership: ConversationOwnership::Ownerless,
                        agent: Some(AgentChange {
                            model: FieldChange::Set(model),
                            ..Default::default()
                        }),
                        init: Some(Arc::new(move |tx, id| {
                            let sandbox_tok = sandbox_tok.clone();
                            let path_c = path_for_init.clone();
                            Box::pin(async move {
                                tx.doc_conversation(&sandbox_tok, id)
                                    .await?
                                    .update(|doc| doc.path = Some(path_c))?;
                                Ok(())
                            })
                        })),
                    },
                    context,
                )
                .await?;
            Ok::<_, pi_js::Error>((conversation, dir))
        }
    };

    let (alice, alice_dir) = conversation_for("alice").await?;
    let (bob, bob_dir) = conversation_for("bob").await?;

    alice
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("Leave a note.".to_owned()),
                when_busy: None,
            },
            context.clone(),
        )
        .await?
        .wait(context.clone())
        .await?;
    bob.submit(
        SubmissionDraft::Input {
            request_id: None,
            content: UserInput::Text("Leave a note.".to_owned()),
            when_busy: None,
        },
        context.clone(),
    )
    .await?
    .wait(context.clone())
    .await?;

    let alice_note = tokio::fs::read_to_string(alice_dir.path().join("note.txt"))
        .await
        .map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let bob_note = tokio::fs::read_to_string(bob_dir.path().join("note.txt"))
        .await
        .map_err(|err| pi_js::Error::msg(err.to_string()))?;
    pi_js::console::log(&format!("alice's sandbox: {alice_note}"));
    pi_js::console::log(&format!("bob's sandbox: {bob_note}"));

    harness.close(context).await?;
    Ok(())
}
