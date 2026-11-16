//! Port of packages/durable/test/examples/15-system-prompt.ts
#![allow(dead_code)]

// System prompt sections and per-conversation instructions.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use pi_ai::types::Message;
use pi_ai::{FauxAssistantMessageContent, FauxResponseStep, create_models, faux_assistant_message, faux_provider};
use pi_chord::Context;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::env::ExecutionEnv;
use pi_durable::env::node::NodeExecutionEnv;
use pi_durable::harness::types::{EnvTarget, PromptSection, SectionOptions};
use pi_durable::types::ConversationOwnership;
use pi_durable::{
    AgentChange, AgentExtensionsChange, Conversation, ConversationCreateOptions, ConversationEntriesQuery, Extension,
    FieldChange, Harness, HarnessOptions, MemoryStorage, ModelRef, Registry, RegistryReader, RootConversationOptions,
    SYSTEM_ENTRY, SubmissionDraft, UserInput, create_registry, define_extension, section, section_with_options,
    wrap_section,
};

async fn system_entries(conversation: &Conversation, context: Context) -> pi_js::Result<Vec<Message>> {
    let page = conversation
        .entries(ConversationEntriesQuery::default(), 20, None, context)
        .await?;
    Ok(page
        .items
        .into_iter()
        .rev()
        .flat_map(|entry| {
            if SYSTEM_ENTRY.matches(Some(&entry)) {
                entry.model.unwrap_or_default()
            } else {
                Vec::new()
            }
        })
        .collect())
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();
    let faux = faux_provider(None);
    let models = create_models(None);
    models.set_provider(faux.provider());
    faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Done.".to_owned()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Done.".to_owned()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Done.".to_owned()),
            None,
        )),
    ]);

    // Pico stores no prompt state. Before each model request, the sections of the
    // conversation's selected extensions render the desired prompt, and only the
    // difference to what the model already saw is appended to the transcript as a
    // `pi.system` entry.
    let coding = define_extension(Extension {
        name: "coding".to_owned(),
        sections: vec![
            // `tag: false` sends the text as is; by default it is wrapped in <key>...</key>.
            section_with_options(
                "preamble",
                Arc::new(|_input, _ctx| Box::pin(async { Ok(Some("You are a coding agent.".to_owned())) })),
                Some(SectionOptions { tag: Some(false) }),
            ),
            // Sections see the environment built for this request, here its working directory.
            section(
                "cwd",
                Arc::new(|input, _ctx| Box::pin(async move { Ok(input.env.as_ref().map(|env| env.cwd().to_owned())) })),
            ),
        ],
        ..Default::default()
    });
    let agents_md = define_extension(Extension {
        name: "agents-md".to_owned(),
        sections: vec![section(
            "agents_md",
            Arc::new(|_input, _ctx| Box::pin(async { Ok(Some("Run npm run check after changes.".to_owned())) })),
        )],
        ..Default::default()
    });
    // Another extension decorates a section by key without replacing it.
    let terse = define_extension(Extension {
        name: "terse".to_owned(),
        wraps: vec![wrap_section("preamble", |preamble| {
            let orig = Arc::clone(&preamble.render);
            PromptSection {
                render: Arc::new(move |input, render_context| {
                    let orig = Arc::clone(&orig);
                    Box::pin(async move {
                        let base = orig(input, render_context).await?.unwrap_or_default();
                        Ok(Some(format!("{base} Be terse.")))
                    })
                }),
                ..preamble
            }
        })],
        ..Default::default()
    });

    let registry = create_registry();
    registry.install(coding)?;
    registry.install(agents_md.clone())?;
    registry.install(terse)?;

    // The environment follows each conversation's agent `cwd`.
    let env = Arc::new(|target: EnvTarget, _ctx: Context| {
        Box::pin(async move {
            let cwd = target.cwd.unwrap_or_else(|| "/".to_owned());
            Ok(Some(Arc::new(NodeExecutionEnv::new(cwd)) as Arc<dyn ExecutionEnv>))
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

    let model = ModelRef {
        provider: "faux".to_owned(),
        model_id: "faux-1".to_owned(),
    };
    let root = harness
        .root(
            context.clone(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    model: FieldChange::Set(model.clone()),
                    cwd: FieldChange::Set("/repo".to_owned()),
                    ..Default::default()
                }),
                init: None,
            }),
        )
        .await?;

    // A subagent deselects AGENTS.md and gets its own instructions, rendered last
    // as the `instructions` section.
    let subagent = harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: Some(AgentChange {
                    model: FieldChange::Set(model),
                    cwd: FieldChange::Set("/repo".to_owned()),
                    extensions: FieldChange::Set(AgentExtensionsChange::Patch {
                        add: None,
                        remove: Some(vec![agents_md.name]),
                    }),
                    instructions: FieldChange::Set("Only read; never edit files.".to_owned()),
                    ..Default::default()
                }),
                init: None,
            },
            context.clone(),
        )
        .await?;

    root.submit(
        SubmissionDraft::Input {
            request_id: None,
            content: UserInput::Text("Fix the build.".to_owned()),
            when_busy: None,
        },
        context.clone(),
    )
    .await?
    .wait(context.clone())
    .await?;

    subagent
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("Read the logs.".to_owned()),
                when_busy: None,
            },
            context.clone(),
        )
        .await?
        .wait(context.clone())
        .await?;

    pi_js::console::log(&format!(
        "root system prompt: {:?}",
        system_entries(&root, context.clone()).await?
    ));
    pi_js::console::log(&format!(
        "subagent system prompt: {:?}",
        system_entries(&subagent, context.clone()).await?
    ));

    // When a section's output changes, the next request appends only the change.
    root.configure(
        AgentChange {
            cwd: FieldChange::Set("/repo/packages".to_owned()),
            ..Default::default()
        },
        context.clone(),
    )
    .await?;
    root.submit(
        SubmissionDraft::Input {
            request_id: None,
            content: UserInput::Text("Now the package.".to_owned()),
            when_busy: None,
        },
        context.clone(),
    )
    .await?
    .wait(context.clone())
    .await?;
    pi_js::console::log(&format!(
        "root system entries after cwd change: {:?}",
        system_entries(&root, context.clone()).await?
    ));

    harness.close(context).await?;
    Ok(())
}
