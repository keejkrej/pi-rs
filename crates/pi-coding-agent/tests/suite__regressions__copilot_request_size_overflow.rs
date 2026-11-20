//! Port of packages/coding-agent/test/suite/regressions/copilot-request-size-overflow.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use pi_agent_core::types::AgentMessage;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxModelDefinition, FauxResponseStep,
    FauxToolCallOptions, faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{
    ImageContent, Message, StopReason, ToolResultMessage, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_coding_agent::core::agent_session::{AgentSessionEvent, CompactionReason};
use serde_json::json;
use support::suite::harness::{Harness, HarnessOptions, create_harness};

const COPILOT_413: &str = r#"github-copilot API error (413): {"message":"failed to parse request","code":""}"#;
const REQUEST_LIMIT: usize = 32 * 1024 * 1024;
const RECENT_PROMPT: &str = "Continue the task using the recent notes.";

fn seed_image_history(harness: &Harness, image_data: &str) {
    let timestamp = pi_js::time::now_ms() - 10_000;
    let model = harness.get_model();

    harness
        .session_manager
        .append_message(AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Text("Review the old images".to_string()),
            timestamp,
        })))
        .unwrap();

    let mut args = IndexMap::new();
    args.insert("path".to_string(), json!("old-images"));
    let mut read_call_msg = faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![FauxContentBlock::ToolCall(faux_tool_call(
            "read",
            args,
            Some(FauxToolCallOptions {
                id: Some("read-images".to_string()),
            }),
        ))]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            timestamp: Some(timestamp),
            ..Default::default()
        }),
    );
    read_call_msg.provider = model.provider.clone();
    read_call_msg.api = model.api.clone();
    read_call_msg.model = model.id.clone();
    harness
        .session_manager
        .append_message(AgentMessage::from(read_call_msg))
        .unwrap();

    let images: Vec<UserContentBlock> = (0..89)
        .map(|_| UserContentBlock::Image(ImageContent::new(image_data, "image/png")))
        .collect();
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::ToolResult(ToolResultMessage {
            tool_call_id: "read-images".to_string(),
            tool_name: "read".to_string(),
            content: images,
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp,
            duration_ms: None,
        })))
        .unwrap();

    let mut done_old = faux_assistant_message(
        FauxAssistantMessageContent::Text("Finished reviewing the old images".to_string()),
        Some(FauxAssistantMessageOptions {
            timestamp: Some(timestamp),
            ..Default::default()
        }),
    );
    done_old.provider = model.provider.clone();
    done_old.api = model.api.clone();
    done_old.model = model.id.clone();
    harness
        .session_manager
        .append_message(AgentMessage::from(done_old))
        .unwrap();

    harness
        .session_manager
        .append_message(AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Text(format!("Recent task notes: {}", "x".repeat(80_000))),
            timestamp,
        })))
        .unwrap();

    let mut ready = faux_assistant_message(
        FauxAssistantMessageContent::Text("Ready to continue".to_string()),
        Some(FauxAssistantMessageOptions {
            timestamp: Some(timestamp),
            ..Default::default()
        }),
    );
    ready.provider = model.provider;
    ready.api = model.api;
    ready.model = model.id;
    harness
        .session_manager
        .append_message(AgentMessage::from(ready))
        .unwrap();

    let msgs = harness.session_manager.build_session_context().messages;
    harness.session.agent().replace_messages(msgs);
}

#[tokio::test]
async fn compacts_an_image_heavy_history_and_retries_after_the_real_413() {
    let harness = create_harness(HarnessOptions {
        provider: Some("github-copilot".to_string()),
        models: Some(vec![FauxModelDefinition {
            id: "faux-copilot".to_string(),
            name: None,
            reasoning: None,
            input: None,
            input_limits: None,
            cost: None,
            context_window: Some(1_000_000),
            max_tokens: None,
        }]),
        ..Default::default()
    })
    .await;

    seed_image_history(&harness, &"A".repeat(384 * 1024));

    harness.set_responses(vec![
        FauxResponseStep::Factory(Arc::new(|context, _opts, _st, _mdl| {
            Box::pin(async move {
                let serialized = pi_js::json::stringify(&context);
                assert!(
                    serialized.len() > REQUEST_LIMIT,
                    "expected request > 32MiB, got {}",
                    serialized.len()
                );
                Err(pi_ai::Error::msg(COPILOT_413))
            })
        })),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text(
                "The old images have been reviewed. Continue with the recent task.".to_string(),
            ),
            None,
        )),
        FauxResponseStep::Factory(Arc::new(|context, _opts, _st, _mdl| {
            Box::pin(async move {
                let serialized = pi_js::json::stringify(&context);
                assert!(!serialized.contains(r#""type":"image""#));
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("Continued after compaction".to_string()),
                    None,
                ))
            })
        })),
    ]);

    harness.session.prompt(RECENT_PROMPT, None).await.expect("prompt");

    assert_eq!(harness.faux.state().call_count(), 3);

    let events = harness.events();
    let compaction_starts: Vec<CompactionReason> = events
        .iter()
        .filter_map(|e| match e {
            AgentSessionEvent::CompactionStart { reason } => Some(*reason),
            _ => None,
        })
        .collect();
    assert_eq!(compaction_starts, vec![CompactionReason::Overflow]);

    let last_end = events
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentSessionEvent::CompactionEnd {
                reason,
                aborted,
                will_retry,
                ..
            } => Some((*reason, *aborted, *will_retry)),
            _ => None,
        })
        .expect("compaction_end");
    assert_eq!(last_end, (CompactionReason::Overflow, false, true));

    assert_eq!(
        harness.session.get_last_assistant_text().as_deref(),
        Some("Continued after compaction")
    );

    harness.cleanup();
}
