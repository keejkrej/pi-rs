//! Port of packages/coding-agent/test/suite/regressions/6904-dns-transport-retry.test.ts

#[path = "support/mod.rs"]
mod support;

use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message,
};
use pi_ai::types::StopReason;
use pi_coding_agent::core::agent_session::AgentSessionEvent;
use pi_coding_agent::core::settings_manager::{RetrySettings, Settings};
use pretty_assertions::assert_eq;
use support::suite::harness::{HarnessOptions, create_harness};

const WRAPPED_DNS_LOOKUP_ERROR: &str =
    "The pending stream has been canceled (caused by: getaddrinfo ENOTFOUND bedrock-runtime.us-east-1.amazonaws.com)";

#[tokio::test(flavor = "current_thread")]
async fn retries_a_transient_dns_lookup_failure() {
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
                error_message: Some(WRAPPED_DNS_LOOKUP_ERROR.to_owned()),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("recovered after DNS retry".to_owned()),
            None,
        )),
    ]);

    harness.session.prompt("test", None).await.expect("prompt");

    assert_eq!(harness.faux.state().call_count(), 2);
    let events = harness.events();
    let retry_starts: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentSessionEvent::AutoRetryStart { error_message, .. } => Some(error_message.clone()),
            _ => None,
        })
        .collect();
    let retry_ends: Vec<bool> = events
        .iter()
        .filter_map(|e| match e {
            AgentSessionEvent::AutoRetryEnd { success, .. } => Some(*success),
            _ => None,
        })
        .collect();
    assert_eq!(retry_starts, vec![WRAPPED_DNS_LOOKUP_ERROR.to_owned()]);
    assert_eq!(retry_ends, vec![true]);

    harness.cleanup();
}
