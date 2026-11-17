//! Port of packages/coding-agent/test/suite/regressions/3982-message-end-cost-override.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use pi_agent_core::types::AgentMessage;
use pi_ai::providers::faux::{FauxAssistantMessageContent, FauxResponseStep, faux_assistant_message};
use pi_ai::types::Message;
use pi_coding_agent::core::agent_session::AgentSessionEvent;
use pi_coding_agent::core::extensions::types::{ExtensionApi, InlineExtension, MessageEndEvent, MessageEndEventResult};
use pretty_assertions::assert_eq;
use support::suite::harness::{HarnessOptions, create_harness};

#[tokio::test]
async fn allows_extensions_to_replace_finalized_assistant_usage_cost() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<MessageEndEvent>(|event, _ctx| {
            Box::pin(async move {
                let AgentMessage::Message(Message::Assistant(mut a)) = event.message else {
                    return Ok(None);
                };
                a.usage.cost.total = 0.123;
                Ok(Some(MessageEndEventResult {
                    message: Some(AgentMessage::from(Message::Assistant(a))),
                }))
            })
        });
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("hello".to_string()),
        None,
    ))]);

    harness.session.prompt("hi", None).await.unwrap();

    let assistant = harness
        .session
        .messages()
        .into_iter()
        .find_map(|m| match m {
            AgentMessage::Message(Message::Assistant(a)) => Some(a),
            _ => None,
        })
        .expect("missing assistant message");
    assert_eq!(assistant.usage.cost.total, 0.123);

    let event_assistant = harness
        .events()
        .into_iter()
        .find_map(|e| match e {
            AgentSessionEvent::MessageEnd {
                message: AgentMessage::Message(Message::Assistant(a)),
            } => Some(a),
            _ => None,
        })
        .expect("missing assistant message_end event");
    assert_eq!(event_assistant.usage.cost.total, 0.123);
    harness.cleanup();
}
