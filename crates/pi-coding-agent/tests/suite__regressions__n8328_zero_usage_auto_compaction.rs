//! Port of packages/coding-agent/test/suite/regressions/8328-zero-usage-auto-compaction.test.ts

#[path = "support/mod.rs"]
mod support;

use pi_agent_core::types::AgentMessage;
use pi_ai::providers::faux::{FauxAssistantMessageContent, FauxModelDefinition, faux_assistant_message};
use pi_ai::types::{Message, TextContent, Usage, UserContentBlock, UserMessage, UserMessageContent};
use pi_coding_agent::core::agent_session::{AgentSessionEvent, CompactionReason};
use pi_coding_agent::core::settings_manager::{CompactionSettings, Settings};
use pretty_assertions::assert_eq;
use support::suite::harness::{HarnessOptions, create_harness};

fn build_user_message(text: &str, timestamp: i64) -> AgentMessage {
    AgentMessage::from(Message::User(UserMessage {
        content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
        timestamp,
    }))
}

#[tokio::test]
async fn falls_back_to_context_token_estimation_when_provider_reports_zero_usage() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            name: None,
            reasoning: None,
            input: None,
            input_limits: None,
            cost: None,
            context_window: Some(100),
            max_tokens: Some(20),
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
                reserve_tokens: Some(10),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;

    let user = build_user_message(&"x".repeat(400), pi_js::time::now_ms() - 1000);
    let mut assistant = faux_assistant_message(FauxAssistantMessageContent::Text("response".to_string()), None);
    assistant.usage = Usage::default();

    harness
        .session
        .agent()
        .replace_messages(vec![user, AgentMessage::from(Message::Assistant(assistant.clone()))]);

    harness.session.check_compaction_for_test(&assistant, true).await;

    let started = harness.events().into_iter().any(|e| {
        matches!(
            e,
            AgentSessionEvent::CompactionStart {
                reason: CompactionReason::Threshold
            }
        )
    });
    assert_eq!(started, true);
    harness.cleanup();
}

#[tokio::test]
async fn does_not_trigger_threshold_compaction_for_short_messages_with_zero_usage() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            name: None,
            reasoning: None,
            input: None,
            input_limits: None,
            cost: None,
            context_window: Some(100),
            max_tokens: Some(20),
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
                reserve_tokens: Some(10),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;

    let user = build_user_message("short", pi_js::time::now_ms() - 1000);
    let mut assistant = faux_assistant_message(FauxAssistantMessageContent::Text("ok".to_string()), None);
    assistant.usage = Usage::default();

    harness
        .session
        .agent()
        .replace_messages(vec![user, AgentMessage::from(Message::Assistant(assistant.clone()))]);

    harness.session.check_compaction_for_test(&assistant, true).await;

    let started = harness
        .events()
        .into_iter()
        .any(|e| matches!(e, AgentSessionEvent::CompactionStart { .. }));
    assert_eq!(started, false);
    harness.cleanup();
}
