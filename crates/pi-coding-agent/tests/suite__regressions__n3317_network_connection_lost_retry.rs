//! Port of packages/coding-agent/test/suite/regressions/3317-network-connection-lost-retry.test.ts

#[path = "support/mod.rs"]
mod support;

use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message,
};
use pi_ai::types::StopReason;
use pi_coding_agent::core::agent_session::AgentSessionEvent;
use pi_coding_agent::core::settings_manager::{RetrySettings, Settings};
use pretty_assertions::assert_eq;
use support::suite::harness::{HarnessOptions, create_harness, get_assistant_texts};

#[tokio::test]
async fn retries_transient_network_connection_lost_failures() {
    let harness = create_harness(HarnessOptions {
        settings: Some(Settings {
            retry: Some(RetrySettings {
                enabled: Some(true),
                max_retries: Some(3),
                base_delay_ms: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text(String::new()),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("Network connection lost.".to_string()),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("recovered after reconnect".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("test", None).await.unwrap();

    assert_eq!(harness.faux.state().call_count(), 2);
    let start_errors: Vec<String> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::AutoRetryStart { error_message, .. } => Some(error_message),
            _ => None,
        })
        .collect();
    assert_eq!(start_errors, vec!["Network connection lost."]);
    let end_successes: Vec<bool> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::AutoRetryEnd { success, .. } => Some(success),
            _ => None,
        })
        .collect();
    assert_eq!(end_successes, vec![true]);
    assert!(get_assistant_texts(&harness).contains(&"recovered after reconnect".to_string()));
    harness.cleanup();
}
