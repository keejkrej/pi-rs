//! Port of packages/coding-agent/test/suite/regressions/7290-json-stream-linear.test.ts

// #7290

use std::sync::{Arc, Mutex};

use pi_agent_core::{Agent, AgentEvent, AgentInitialState, AgentOptions, ThinkingLevel};
use pi_ai::compat::register_faux_provider;
use pi_ai::{FauxAssistantMessageContent, FauxProviderRegistration, FauxResponseStep, faux_assistant_message};
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
async fn measure_update_bytes(text: String) -> usize {
    let faux = FauxGuard(register_faux_provider(None));
    faux.0
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text(text),
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

    let recorded = session_updates.lock().unwrap().clone();
    for update in &recorded {
        let raw = pi_js::json::parse(&pi_js::json::stringify(update)).expect("valid raw update json");
        assert!(raw.get("message").is_some());
        assert!(
            raw.get("assistantMessageEvent")
                .and_then(|v| v.get("partial"))
                .is_some()
        );
    }

    let wire_updates: Vec<Value> = recorded
        .iter()
        .map(|event| {
            let wire = to_json_event(event).expect("to_json_event succeeds");
            pi_js::json::parse(&pi_js::json::stringify(&wire)).expect("valid wire json")
        })
        .collect();
    assert!(!wire_updates.is_empty());

    for wire in &wire_updates {
        assert!(wire.get("message").is_none());
        assert!(
            wire.get("assistantMessageEvent")
                .and_then(|v| v.get("partial"))
                .is_none()
        );
    }

    wire_updates
        .iter()
        .map(|event| pi_js::json::stringify(event).len())
        .sum()
}

mod regression_7290_json_event_streams_stay_linear {
    use super::*;

    #[tokio::test]
    async fn emits_delta_only_message_updates_whose_size_scales_linearly() {
        let small_bytes = measure_update_bytes("x".repeat(2_000)).await;
        let large_bytes = measure_update_bytes("x".repeat(4_000)).await;

        assert!(large_bytes > small_bytes);
        assert!((large_bytes as f64) / (small_bytes as f64) < 2.2);
    }
}
