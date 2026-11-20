//! Port of packages/coding-agent/test/suite/regressions/11227-recovery-omits-tool-results.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_agent_core::types::AgentMessage;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxModelDefinition, FauxResponseStep,
    FauxToolCallOptions, faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{
    Message, StopReason, TextContent, ToolResultMessage, Usage, UsageCost, UserContentBlock, UserMessage,
    UserMessageContent,
};
use pi_coding_agent::core::compaction::CompactionResult;
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, SessionBeforeCompactEvent, SessionBeforeCompactResult,
};
use pi_coding_agent::core::settings_manager::{CompactionSettings, Settings};
use serde_json::json;
use support::suite::harness::{HarnessOptions, create_harness};

#[tokio::test]
async fn pre_prompt_recovery_of_resumed_length_stop_omits_truncated_call_and_its_result() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<SessionBeforeCompactEvent>(|event, _ctx| {
            Box::pin(async move {
                Ok(Some(SessionBeforeCompactResult {
                    cancel: None,
                    compaction: Some(CompactionResult {
                        summary: "summary".to_string(),
                        first_kept_entry_id: event.preparation.first_kept_entry_id,
                        tokens_before: event.preparation.tokens_before,
                        estimated_tokens_after: None,
                        details: Some(json!({})),
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
            context_window: Some(100_000),
            max_tokens: Some(1_000),
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
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
            content: UserMessageContent::Text("check status".to_string()),
            timestamp: now - 1000,
        })))
        .unwrap();

    let mut args = IndexMap::new();
    args.insert("command".to_string(), json!("echo"));
    let mut truncated = faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![FauxContentBlock::ToolCall(faux_tool_call(
            "bash",
            args,
            Some(FauxToolCallOptions {
                id: Some("call_truncated".to_string()),
            }),
        ))]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Length),
            timestamp: Some(now - 500),
            ..Default::default()
        }),
    );
    truncated.api = model.api;
    truncated.provider = model.provider;
    truncated.model = model.id;
    truncated.usage = Usage {
        input: 100,
        output: 16,
        cache_read: 0,
        cache_write: 0,
        total_tokens: 116,
        cost: UsageCost::default(),
        ..Default::default()
    };
    harness
        .session_manager
        .append_message(AgentMessage::from(truncated))
        .unwrap();

    harness
        .session_manager
        .append_message(AgentMessage::from(Message::ToolResult(ToolResultMessage {
            tool_call_id: "call_truncated".to_string(),
            tool_name: "bash".to_string(),
            content: vec![UserContentBlock::Text(TextContent::new(
                "Not executed: output token limit reached.",
            ))],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: true,
            timestamp: now - 400,
            duration_ms: None,
        })))
        .unwrap();

    let msgs = harness.session_manager.build_session_context().messages;
    harness.session.agent().replace_messages(msgs);

    let request_messages = Arc::new(Mutex::new(Vec::<Message>::new()));
    let captured = Arc::clone(&request_messages);
    harness.set_responses(vec![FauxResponseStep::Factory(Arc::new(
        move |context, _opts, _st, _mdl| {
            let captured = Arc::clone(&captured);
            Box::pin(async move {
                *captured.lock().unwrap() = context.messages;
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("done".to_string()),
                    None,
                ))
            })
        },
    ))]);

    harness.session.prompt("continue", None).await.expect("prompt");

    let reqs = request_messages.lock().unwrap();
    let roles: Vec<&str> = reqs
        .iter()
        .map(|m| match m {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::ToolResult(_) => "toolResult",
            Message::System(_) => "system",
        })
        .collect();
    assert!(!roles.contains(&"toolResult"), "roles: {roles:?}");
    assert!(
        !reqs
            .iter()
            .any(|m| matches!(m, Message::Assistant(a) if a.stop_reason == StopReason::Length))
    );

    harness.cleanup();
}
