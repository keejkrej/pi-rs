//! Port of packages/coding-agent/test/suite/regressions/7911-json-stream-usage.test.ts

// #7911

use std::sync::{Arc, Mutex};

use pi_agent_core::{Agent, AgentEvent, AgentInitialState, AgentMessage, AgentOptions, ThinkingLevel};
use pi_ai::compat::register_faux_provider;
use pi_ai::{FauxAssistantMessageContent, FauxProviderRegistration, FauxResponseStep, Message, faux_assistant_message};
use pi_coding_agent::modes::json_event::{AgentSessionEvent, to_json_event};
use serde_json::Value;

struct FauxGuard(FauxProviderRegistration);

impl Drop for FauxGuard {
    fn drop(&mut self) {
        self.0.unregister();
    }
}

// PORT: `AgentSession` / `tests/support/suite/harness.rs` (Stage 3A) have not merged yet on this branch,
// so streaming `message_update` events are driven directly through `pi_agent_core::Agent` + `register_faux_provider`
// and transformed with `pi_coding_agent::modes::json_event::to_json_event`.
mod regression_7911_json_message_updates_retain_usage {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn includes_cumulative_usage_without_cumulative_message_snapshots() {
        let faux = FauxGuard(register_faux_provider(None));
        faux.0
            .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
                FauxAssistantMessageContent::Text("hello".to_string()),
                None,
            ))]);

        let agent = Agent::new(AgentOptions {
            initial_state: Some(AgentInitialState {
                model: Some(faux.0.get_model()),
                thinking_level: Some(ThinkingLevel::Off),
                tools: Some(Vec::new()),
                ..Default::default()
            }),
            ..Default::default()
        });

        let session_updates = Arc::new(Mutex::new(Vec::<AgentSessionEvent>::new()));
        let updates_ref = Arc::clone(&session_updates);
        let _unsub = agent.subscribe(move |event| {
            if let AgentEvent::MessageUpdate {
                message,
                assistant_message_event,
            } = event
            {
                updates_ref.lock().unwrap().push(AgentSessionEvent::MessageUpdate {
                    message: message.clone(),
                    assistant_message_event: assistant_message_event.clone(),
                });
            }
        });

        agent.prompt("respond").await.expect("prompt succeeds");

        // #7290's delta-only wire projection dropped this fixed-size metadata with the snapshots.
        let updates = session_updates.lock().unwrap().clone();
        let update = updates
            .into_iter()
            .find(|event| match event {
                AgentSessionEvent::MessageUpdate {
                    message: AgentMessage::Message(Message::Assistant(a)),
                    ..
                } => a.usage.total_tokens > 0,
                _ => false,
            })
            .expect("Expected an assistant update with populated usage");

        let expected_usage = match &update {
            AgentSessionEvent::MessageUpdate {
                message: AgentMessage::Message(Message::Assistant(a)),
                ..
            } => pi_js::json::parse(&pi_js::json::stringify(&a.usage)).expect("valid usage json"),
            _ => unreachable!(),
        };

        let wire_update = to_json_event(&update).expect("to_json_event succeeds");
        let wire_json: Value = pi_js::json::parse(&pi_js::json::stringify(&wire_update)).expect("valid wire json");

        assert_eq!(wire_json.get("usage"), Some(&expected_usage));
        assert!(wire_json.get("message").is_none());
        assert!(
            wire_json
                .get("assistantMessageEvent")
                .and_then(|v| v.get("partial"))
                .is_none()
        );
    }
}
