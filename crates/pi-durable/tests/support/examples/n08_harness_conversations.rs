//! Port of packages/durable/test/examples/08-harness-conversations.ts
#![allow(dead_code)]

// Conversation handles, typed entries, created conversations, and forks.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use pi_ai::create_models;
use pi_ai::types::{Message, ModelThinkingLevel, UserMessage, UserMessageContent};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::entries::define_entry;
use pi_durable::types::{ConversationOwnership, TypedEntryDraft};
use pi_durable::{
    AgentChange, ConversationCreateOptions, FieldChange, Harness, HarnessOptions, MemoryStorage, RegistryReader,
    create_registry,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ExampleMessageData {
    from: String,
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();
    let registry = create_registry();
    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            models: create_models(None),
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

    let root = harness.root(context.clone(), None).await?;
    root.configure(
        AgentChange {
            thinking_level: FieldChange::Set(ModelThinkingLevel::High),
            ..Default::default()
        },
        context.clone(),
    )
    .await?;

    // Conversation handles are stateless; compare them by id. They bind commits
    // to their conversation. An entry token types an entry kind's `data`.
    let message_entry = define_entry::<ExampleMessageData>("message")?;
    let root_id = root.id();
    let msg_tok = message_entry.clone();
    let hello = root
        .commit(
            move |tx| async move {
                tx.append_typed_entry(
                    &msg_tok,
                    root_id,
                    TypedEntryDraft {
                        data: ExampleMessageData {
                            from: "example".to_owned(),
                        },
                        model: Some(vec![Message::User(UserMessage {
                            content: UserMessageContent::Text("hello".to_owned()),
                            timestamp: 1,
                        })]),
                        head: None,
                        edits: None,
                    },
                )
                .await
            },
            context.clone(),
        )
        .await?;
    pi_js::console::log(&format!(
        "typed entry: {} {}",
        message_entry.matches(Some(&hello.record)),
        hello.data.from
    ));

    // create_conversation() and fork() apply `agent` and run `init` in the
    // creating commit. A fork starts with the agent the parent had at the fork
    // entry.
    let helper = harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: Some(AgentChange {
                    thinking_level: FieldChange::Set(ModelThinkingLevel::Minimal),
                    ..Default::default()
                }),
                init: None,
            },
            context.clone(),
        )
        .await?;
    let retry = root
        .fork(
            hello.record.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context.clone(),
        )
        .await?;

    pi_js::console::log(&format!(
        "helper thinking: {:?}",
        helper.agent(context.clone()).await?.thinking_level
    ));
    pi_js::console::log(&format!(
        "fork thinking: {:?}",
        retry.agent(context.clone()).await?.thinking_level
    ));
    let found = harness.conversation(retry.id(), context.clone()).await?;
    pi_js::console::log(&format!("lookup: {}", found.map(|c| c.id()) == Some(retry.id())));

    harness.close(context).await?;
    Ok(())
}
