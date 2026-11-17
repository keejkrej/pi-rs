//! Port of packages/coding-agent/test/suite/regressions/pre-prompt-compaction-no-continue.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use pi_agent_core::types::AgentMessage;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxModelDefinition, FauxResponseStep,
    faux_assistant_message,
};
use pi_ai::types::{
    Message, StopReason, TextContent, Usage, UsageCost, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_coding_agent::core::agent_session::{AgentSessionEvent, CompactionReason};
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, SessionBeforeCompactEvent, SessionBeforeCompactResult,
};
use pi_coding_agent::core::settings_manager::{CompactionSettings, Settings};
use pi_coding_agent::core::tools::mcp_wire::CompactionResult;
use pretty_assertions::assert_eq;
use support::suite::harness::{HarnessOptions, create_harness, get_user_texts};

#[tokio::test]
async fn compacts_length_stop_overflow_before_new_prompt_without_continuing_from_assistant_message() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<SessionBeforeCompactEvent>(|event, _ctx| {
            Box::pin(async move {
                Ok(Some(SessionBeforeCompactResult {
                    cancel: None,
                    compaction: Some(CompactionResult {
                        summary: "pre-prompt summary".to_string(),
                        first_kept_entry_id: event.preparation.first_kept_entry_id,
                        tokens_before: event.preparation.tokens_before,
                        details: Some(serde_json::json!({})),
                        usage: None,
                    }),
                }))
            })
        });
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            name: None,
            reasoning: None,
            input: None,
            input_limits: None,
            cost: None,
            context_window: Some(100),
            max_tokens: Some(100),
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
                keep_recent_tokens: Some(1),
                reserve_tokens: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        }),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let now = pi_js::time::now_ms();
    let model = harness.get_model();
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new("previous prompt"))]),
            timestamp: now - 1000,
        })))
        .unwrap();

    let mut length_stop_assistant = faux_assistant_message(
        FauxAssistantMessageContent::Text("length-stop assistant response".to_string()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Length),
            timestamp: Some(now - 500),
            ..Default::default()
        }),
    );
    length_stop_assistant.api = model.api;
    length_stop_assistant.provider = model.provider;
    length_stop_assistant.model = model.id;
    length_stop_assistant.usage = Usage {
        input: 100,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 100,
        cost: UsageCost::default(),
    };
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::Assistant(length_stop_assistant)))
        .unwrap();
    let ctx_msgs = harness.session_manager.build_session_context().messages;
    harness.session.agent().replace_messages(ctx_msgs);

    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("answered next prompt".to_string()),
        None,
    ))]);

    harness.session.prompt("next prompt", None).await.unwrap();

    let last_compaction = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::CompactionEnd {
                reason,
                aborted,
                will_retry,
                ..
            } => Some((reason, aborted, will_retry)),
            _ => None,
        })
        .last();
    assert_eq!(last_compaction, Some((CompactionReason::Overflow, false, true)));
    assert!(get_user_texts(&harness).contains(&"next prompt".to_string()));
    assert_eq!(harness.faux.state().call_count(), 1);
    harness.cleanup();
}
