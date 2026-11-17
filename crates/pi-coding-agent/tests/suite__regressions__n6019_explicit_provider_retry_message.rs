//! Port of packages/coding-agent/test/suite/regressions/6019-explicit-provider-retry-message.test.ts

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

const OPENAI_EXPLICIT_RETRY_MESSAGE: &str = "An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID req_******** in your message.";
const BEDROCK_EXPLICIT_RETRY_MESSAGE: &str =
    r#"{"message":"The system encountered an unexpected error during processing. Try your request again."}"#;

async fn assert_retries_explicit_guidance(error_message: &str) {
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
                error_message: Some(error_message.to_string()),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("recovered".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("test", None).await.unwrap();

    assert_eq!(harness.faux.state().call_count(), 2);
    let started: Vec<String> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::AutoRetryStart { error_message, .. } => Some(error_message),
            _ => None,
        })
        .collect();
    assert_eq!(started, vec![error_message.to_string()]);
    let ended: Vec<bool> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::AutoRetryEnd { success, .. } => Some(success),
            _ => None,
        })
        .collect();
    assert_eq!(ended, vec![true]);
    harness.cleanup();
}

#[tokio::test]
async fn retries_openai_explicit_retry_guidance() {
    assert_retries_explicit_guidance(OPENAI_EXPLICIT_RETRY_MESSAGE).await;
}

#[tokio::test]
async fn retries_bedrock_explicit_retry_guidance() {
    assert_retries_explicit_guidance(BEDROCK_EXPLICIT_RETRY_MESSAGE).await;
}
