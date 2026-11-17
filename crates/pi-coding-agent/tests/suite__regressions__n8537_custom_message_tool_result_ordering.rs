//! Port of packages/coding-agent/test/suite/regressions/8537-custom-message-tool-result-ordering.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use pi_agent_core::types::{AgentMessage, AgentTool, AgentToolResult};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{Message, StopReason, TextContent, UserContentBlock};
use pi_coding_agent::core::agent_session::{
    AgentSessionEvent, SendCustomMessageDraft, SendCustomMessageOptions,
};
use pi_coding_agent::core::extensions::types::{CustomMessageContent, SessionEntry};
use pi_coding_agent::core::messages::convert_to_llm;
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::suite::harness::{Harness, HarnessOptions, create_harness};
use tokio::sync::Notify;

fn roles(messages: &[AgentMessage]) -> Vec<&'static str> {
    messages
        .iter()
        .map(|m| match m.role() {
            Some("system") => "system",
            Some("user") => "user",
            Some("assistant") => "assistant",
            Some("toolResult") => "toolResult",
            Some("custom") => "custom",
            Some("compactionSummary") => "compactionSummary",
            Some("branchSummary") => "branchSummary",
            Some("bashExecution") => "bashExecution",
            _ => "unknown",
        })
        .collect()
}

fn build_wait_tool(started: Arc<Notify>, release: Arc<Notify>) -> AgentTool {
    AgentTool {
        name: "wait".to_string(),
        label: "Wait".to_string(),
        description: "Wait for a release signal".to_string(),
        parameters: t::object(&[]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, _sig, _upd| {
            let s = Arc::clone(&started);
            let r = Arc::clone(&release);
            Box::pin(async move {
                let wait = r.notified();
                s.notify_one();
                wait.await;
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("waited"))],
                    details: Some(json!({})),
                    structured_content: None,
                    usage: None,
                })
            })
        }),
        replay: None,
    }
}

async fn setup_active_tool_with_custom_message() -> Harness {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let harness = create_harness(HarnessOptions {
        tools: Some(vec![build_wait_tool(Arc::clone(&started), Arc::clone(&release))]),
        ..Default::default()
    })
    .await;

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call("wait", json!({}))]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("final".to_string()),
            None,
        )),
    ]);

    let started_wait = started.notified();
    let session = harness.session.clone();
    let prompt_handle = tokio::spawn(async move { session.prompt("start", None).await });
    started_wait.await;

    harness
        .session
        .send_custom_message(
            SendCustomMessageDraft {
                custom_type: "subagent-reply".to_string(),
                content: CustomMessageContent::Text("subagent replied".to_string()),
                display: true,
                details: None,
            },
            Some(SendCustomMessageOptions {
                trigger_turn: Some(false),
                deliver_as: None,
            }),
        )
        .await
        .unwrap();

    release.notify_one();
    prompt_handle.await.unwrap().unwrap();
    harness
}

#[tokio::test]
async fn delivers_pending_next_turn_custom_message_after_active_tool_result() {
    let harness = setup_active_tool_with_custom_message().await;

    assert_eq!(
        roles(&harness.session.messages()),
        vec!["system", "user", "assistant", "toolResult", "custom", "assistant"]
    );
    harness.cleanup();
}

#[tokio::test]
async fn records_branch_entries_and_events_in_matching_tool_result_then_custom_order() {
    let harness = setup_active_tool_with_custom_message().await;

    let branch_messages: Vec<AgentMessage> = harness
        .session_manager
        .get_branch(None)
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::Message(m) => Some(m.message),
            SessionEntry::CustomMessage(cm) => AgentMessage::try_custom_from(&json!({
                "role": "custom",
                "customType": cm.custom_type,
                "content": cm.content,
                "display": cm.display,
                "details": cm.details,
                "timestamp": cm.timestamp,
            }))
            .ok(),
            _ => None,
        })
        .collect();
    assert_eq!(
        roles(&branch_messages),
        vec!["system", "user", "assistant", "toolResult", "custom", "assistant"]
    );

    let event_messages: Vec<AgentMessage> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::MessageStart { message } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(
        roles(&event_messages),
        vec!["user", "assistant", "toolResult", "custom", "assistant"]
    );
    harness.cleanup();
}

#[tokio::test]
async fn keeps_llm_converted_tool_results_paired_immediately_after_assistant_tool_calls() {
    let seen_calls: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
    let harness = setup_active_tool_with_custom_message().await;
    let llm_messages = convert_to_llm(&harness.session.messages());

    for (idx, msg) in llm_messages.iter().enumerate() {
        if let Message::ToolResult(tr) = msg {
            assert!(idx > 0, "toolResult cannot appear at index 0");
            match &llm_messages[idx - 1] {
                Message::Assistant(assistant) => {
                    let has_call = assistant.content.iter().any(|b| match b {
                        pi_ai::types::AssistantContentBlock::ToolCall(tc) => tc.id == tr.tool_call_id,
                        _ => false,
                    });
                    assert!(has_call, "toolResult must follow assistant toolCall with matching id");
                }
                other => panic!("expected preceding assistant message, got {other:?}"),
            }
            seen_calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((tr.tool_name.clone(), json!(tr.tool_call_id)));
        }
    }

    assert_eq!(seen_calls.lock().unwrap_or_else(PoisonError::into_inner).len(), 1);
    harness.cleanup();
}
