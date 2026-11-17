//! Port of packages/coding-agent/test/suite/lax-message-content.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use pi_agent_core::types::{AgentMessage, AgentToolResult};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{Message, StopReason};
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, MessageEndEvent, MessageEndEventResult, ToolDefinition,
};
use pi_coding_agent::core::session_manager::{SessionEntry, SessionMessageEntry, session_entry_to_context_messages};
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::suite::harness::{HarnessOptions, create_harness};

fn raw_message_entry(raw_msg: Value) -> SessionEntry {
    let msg: AgentMessage = serde_json::from_value(raw_msg).unwrap();
    SessionEntry::Message(SessionMessageEntry {
        id: "entry-1".to_string(),
        parent_id: None,
        timestamp: pi_js::time::iso_now(),
        message: msg,
    })
}

#[tokio::test]
async fn normalizes_tool_results_from_untyped_tools_that_omit_content() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.register_tool(ToolDefinition {
            name: "web_search".to_string(),
            label: "Web Search".to_string(),
            description: "Custom tool that returns a result without content".to_string(),
            prompt_snippet: None,
            prompt_guidelines: None,
            parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]),
            constrained_sampling: None,
            render_shell: None,
            prepare_arguments: None,
            output_schema: None,
            exposure: None,
            namespace: None,
            annotations: None,
            default_active: None,
            prepare_loadout: None,
            execution_mode: None,
            execute: Arc::new(|_id, _params, _sig, _upd, _ctx| {
                Box::pin(async move {
                    Ok(AgentToolResult {
                        content: Vec::new(),
                        details: Some(json!({})),
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: None,
                        control: None,
                    })
                })
            }),
            render_call: None,
            render_result: None,
        })?;
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "web_search",
                indexmap::IndexMap::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("done".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("search something", None).await.unwrap();

    let tool_results: Vec<_> = harness
        .session
        .messages()
        .into_iter()
        .filter_map(|m| match m {
            AgentMessage::Message(Message::ToolResult(tr)) => Some(tr),
            _ => None,
        })
        .collect();
    assert_eq!(tool_results.len(), 1);
    assert!(tool_results[0].content.is_empty());
    assert_eq!(harness.get_pending_response_count(), 0);

    harness.cleanup();
}

#[tokio::test]
async fn normalizes_null_content_in_message_end_extension_replacements() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<MessageEndEvent>(|event, _ctx| {
            Box::pin(async move {
                if event.message.role() != Some("assistant") {
                    return Ok(None);
                }
                let mut val = pi_js::json::parse(&pi_js::json::try_stringify(&event.message)?).unwrap();
                if let Some(obj) = val.as_object_mut() {
                    obj.insert("content".to_string(), Value::Null);
                }
                let replaced = AgentMessage::Custom(val);
                Ok(Some(MessageEndEventResult {
                    message: Some(replaced),
                }))
            })
        });
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("hello".to_string()),
        None,
    ))]);

    harness.session.prompt("hi", None).await.unwrap();

    let assistants: Vec<AgentMessage> = harness
        .session
        .messages()
        .into_iter()
        .filter(|m| m.role() == Some("assistant"))
        .collect();
    assert_eq!(assistants.len(), 1);
    let content_empty = match &assistants[0] {
        AgentMessage::Message(Message::Assistant(a)) => a.content.is_empty(),
        AgentMessage::Custom(v) => v.get("content").and_then(Value::as_array).is_some_and(Vec::is_empty),
        _ => false,
    };
    assert!(content_empty);

    harness.cleanup();
}

#[tokio::test]
async fn normalizes_null_content_in_custom_messages_from_extensions() {
    let harness = create_harness(HarnessOptions::default()).await;
    harness
        .session
        .send_custom_message(
            pi_coding_agent::core::agent_session::SendCustomMessageDraft {
                custom_type: "test".to_string(),
                content: pi_coding_agent::core::extensions::types::CustomMessageContent::Blocks(Vec::new()),
                display: false,
                details: None,
            },
            None,
        )
        .await
        .unwrap();

    let custom_msgs: Vec<_> = harness
        .session
        .messages()
        .into_iter()
        .filter(|m| m.role() == Some("custom"))
        .collect();
    assert_eq!(custom_msgs.len(), 1);

    harness.cleanup();
}

#[test]
fn normalizes_null_or_missing_content_when_loading_session_message_entries() {
    let ts = pi_js::time::now_ms();
    let bad_messages = [
        json!({ "role": "user", "content": null, "timestamp": ts }),
        json!({
            "role": "assistant",
            "content": null,
            "api": "openai-completions",
            "provider": "openai",
            "model": "test-model",
            "usage": {
                "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
            },
            "stopReason": "stop",
            "timestamp": ts
        }),
        json!({
            "role": "toolResult",
            "toolCallId": "call_1",
            "toolName": "web_search",
            "isError": false,
            "timestamp": ts
        }),
    ];

    for bad in &bad_messages {
        let msgs = session_entry_to_context_messages(&raw_message_entry(bad.clone()));
        assert_eq!(msgs.len(), 1);
        let expected_role = bad.get("role").and_then(Value::as_str);
        assert_eq!(msgs[0].role(), expected_role);
        let empty = match &msgs[0] {
            AgentMessage::Message(Message::User(u)) => {
                matches!(&u.content, pi_ai::types::UserMessageContent::Blocks(b) if b.is_empty())
            }
            AgentMessage::Message(Message::Assistant(a)) => a.content.is_empty(),
            AgentMessage::Message(Message::ToolResult(t)) => t.content.is_empty(),
            AgentMessage::Custom(v) => v.get("content").and_then(Value::as_array).is_some_and(Vec::is_empty),
            _ => false,
        };
        assert!(empty);
    }
}

#[test]
fn normalizes_null_content_when_loading_custom_message_entries() {
    let entry: SessionEntry = serde_json::from_value(json!({
        "type": "custom_message",
        "id": "entry-1",
        "parentId": null,
        "timestamp": pi_js::time::iso_now(),
        "customType": "test",
        "content": null,
        "display": false
    }))
    .unwrap();
    let msgs = session_entry_to_context_messages(&entry);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].role(), Some("custom"));
}

#[test]
fn keeps_valid_message_content_untouched_when_loading_session_entries() {
    let entry = raw_message_entry(json!({
        "role": "user",
        "content": "hello",
        "timestamp": pi_js::time::now_ms(),
    }));
    let msgs = session_entry_to_context_messages(&entry);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].role(), Some("user"));
    match &msgs[0] {
        AgentMessage::Message(Message::User(u)) => {
            assert_eq!(u.content, pi_ai::types::UserMessageContent::Text("hello".to_string()));
        }
        other => panic!("expected User message, got {other:?}"),
    }
}
