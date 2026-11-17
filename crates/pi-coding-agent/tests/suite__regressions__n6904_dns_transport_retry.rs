//! Port of packages/coding-agent/test/suite/regressions/6904-dns-transport-retry.test.ts

mod support;

use pi_ai::StopReason;
use pi_coding_agent::core::settings_manager::{RetrySettings, Settings};

use support::test_harness::{FauxResponse, FauxResponseInput, HarnessOptions, HarnessSessionEvent, create_harness};

const WRAPPED_DNS_LOOKUP_ERROR: &str =
    "The pending stream has been canceled (caused by: getaddrinfo ENOTFOUND bedrock-runtime.us-east-1.amazonaws.com)";

#[tokio::test(flavor = "current_thread")]
async fn retries_a_transient_dns_lookup_failure() {
    // WIRE(@codemode-w3): switches from HarnessSession adapter to `pi_coding_agent::core::agent_session::AgentSession`
    // once `core-session` lands; verifies retry policy handles DNS lookup failures.
    let harness = create_harness(Some(HarnessOptions {
        settings: Some(Settings {
            retry: Some(RetrySettings {
                enabled: Some(true),
                max_retries: Some(3),
                base_delay_ms: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }),
        responses: Some(vec![
            FauxResponseInput::Spec(FauxResponse {
                text: Some(String::new()),
                stop_reason: Some(StopReason::Error),
                error: Some(WRAPPED_DNS_LOOKUP_ERROR.to_owned()),
                ..Default::default()
            }),
            FauxResponseInput::from("recovered after DNS retry"),
        ]),
        ..Default::default()
    }))
    .await
    .expect("create harness");

    harness.session.prompt("test").await.expect("prompt");

    assert_eq!(harness.faux().call_count, 2);
    let events = harness.events_snapshot();
    let retry_starts: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            HarnessSessionEvent::AutoRetryStart { error_message, .. } => Some(error_message.clone()),
            _ => None,
        })
        .collect();
    let retry_ends: Vec<bool> = events
        .iter()
        .filter_map(|e| match e {
            HarnessSessionEvent::AutoRetryEnd { success, .. } => Some(*success),
            _ => None,
        })
        .collect();
    pretty_assertions::assert_eq!(retry_starts, vec![WRAPPED_DNS_LOOKUP_ERROR.to_owned()]);
    pretty_assertions::assert_eq!(retry_ends, vec![true]);

    harness.cleanup();
}
