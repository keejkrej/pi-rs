//! Port of packages/durable/test/examples/14-chat.ts
#![allow(dead_code)]

// A chat turn.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use pi_ai::types::Message;
use pi_ai::{FauxAssistantMessageContent, FauxResponseStep, create_models, faux_assistant_message, faux_provider};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::types::{InputSubmissionState, SubmissionState};
use pi_durable::{
    AGENT_DOC, AgentChange, ConversationEntriesQuery, Extension, FieldChange, Harness, HarnessOptions, MemoryStorage,
    ModelRef, Registry, RegistryReader, RootConversationOptions, SectionOptions, SubmissionDraft, UserInput,
    create_registry, define_extension, section_with_options,
};

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();

    // The faux provider stands in for a real model (16-real-model is omitted offline).
    let faux = faux_provider(None);
    let models = create_models(None);
    models.set_provider(faux.provider());
    faux.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("Paris.".to_owned()),
        None,
    ))]);

    let registry = create_registry();
    registry.install(define_extension(Extension {
        name: "terse".to_owned(),
        sections: vec![section_with_options(
            "preamble",
            Arc::new(|_input, _ctx| Box::pin(async { Ok(Some("You answer in one word.".to_owned())) })),
            Some(SectionOptions { tag: Some(false) }),
        )],
        ..Default::default()
    }))?;
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

    let _agent_kind = AGENT_DOC.definition.kind.as_str();

    // submit() durably admits user input and returns a SubmissionHandle. The built-in
    // pi.generation task prepares the system prompt from the agent's sections,
    // calls the model, and appends the answer; wait() resolves once the input is
    // answered or has failed.
    let capital = root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("Capital of France?".to_owned()),
                when_busy: None,
            },
            context.clone(),
        )
        .await?;
    let answered = capital.wait(context.clone()).await?;
    if let SubmissionState::Input(InputSubmissionState::Done { answer, .. }) = settled_state(&answered.record.state) {
        let entry = root
            .commit(move |tx| async move { tx.entry(*answer).await }, context.clone())
            .await?;
        if let Some(Message::Assistant(msg)) = entry.and_then(|e| e.model).and_then(|mut m| m.drain(..).next()) {
            pi_js::console::log(&format!("answer: {:?}", msg.content));
        }
    }

    // The transcript holds the user input, the positional system prompt, and the answer.
    let transcript = root
        .entries(ConversationEntriesQuery::default(), 10, None, context.clone())
        .await?;
    let kinds: Vec<&str> = transcript.items.iter().rev().map(|entry| entry.kind.as_str()).collect();
    pi_js::console::log(&format!("transcript: {kinds:?}"));
    harness.close(context).await?;
    Ok(())
}

fn settled_state(state: &SubmissionState) -> &SubmissionState {
    state
}
