//! Port of packages/coding-agent/test/suite/regressions/1717-2113-agent-session-event-settlement.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{StopReason, TextContent, UserContentBlock};
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, MessageEndEvent, SessionEntry, ToolCallEvent,
};
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::suite::harness::{HarnessOptions, create_harness};

fn create_echo_tool() -> AgentTool {
    AgentTool {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "Echo text back".to_string(),
        parameters: t::object(&[t::prop("text", t::string())]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, params, _sig, _upd| {
            Box::pin(async move {
                let text = params.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(text.clone()))],
                    details: Some(json!({ "text": text })),
                    ..Default::default()
                })
            })
        }),
        replay: None,
    }
}

#[tokio::test]
async fn keeps_persisted_assistant_tool_result_message_order_when_extension_message_end_handlers_yield() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<MessageEndEvent>(|event, _ctx| {
            Box::pin(async move {
                if event.message.role() == Some("assistant") {
                    pi_js::time::sleep(20).await;
                }
                Ok(None)
            })
        });
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![create_echo_tool()]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![
                faux_tool_call("echo", json!({ "text": "one" })),
                faux_tool_call("echo", json!({ "text": "two" })),
            ]),
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

    harness.session.prompt("run tools", None).await.unwrap();

    let branch_roles: Vec<String> = harness
        .session_manager
        .get_branch(None)
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::Message(m) => m.message.role().map(str::to_string),
            _ => None,
        })
        .collect();
    assert_eq!(
        branch_roles,
        vec!["system", "user", "assistant", "toolResult", "toolResult", "assistant"]
    );
    let first_tool_result_index = branch_roles.iter().position(|r| r == "toolResult").unwrap();
    assert!(first_tool_result_index > 0);
    assert_eq!(branch_roles[first_tool_result_index - 1], "assistant");
    harness.cleanup();
}

#[tokio::test]
async fn runs_tool_call_handlers_after_the_assistant_tool_use_message_is_settled_in_the_session() {
    let branch_roles_at_tool_call = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let session_manager_slot = Arc::new(Mutex::new(
        Option::<pi_coding_agent::core::session_manager::SessionManager>::None,
    ));

    let roles_c = Arc::clone(&branch_roles_at_tool_call);
    let sm_c = Arc::clone(&session_manager_slot);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let roles = Arc::clone(&roles_c);
        let sm = Arc::clone(&sm_c);
        async move {
            pi.on::<ToolCallEvent>(move |_ev, _ctx| {
                let roles = Arc::clone(&roles);
                let sm = Arc::clone(&sm);
                Box::pin(async move {
                    if let Some(mgr) = sm.lock().unwrap_or_else(PoisonError::into_inner).clone() {
                        let branch: Vec<String> = mgr
                            .get_branch(None)
                            .into_iter()
                            .filter_map(|e| match e {
                                SessionEntry::Message(m) => m.message.role().map(str::to_string),
                                _ => None,
                            })
                            .collect();
                        roles.lock().unwrap_or_else(PoisonError::into_inner).push(branch);
                    }
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![create_echo_tool()]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    *session_manager_slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(harness.session_manager.clone());

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call("echo", json!({ "text": "hello" }))]),
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

    harness.session.prompt("run tool", None).await.unwrap();

    assert_eq!(
        *branch_roles_at_tool_call.lock().unwrap_or_else(PoisonError::into_inner),
        vec![vec![
            "system".to_string(),
            "user".to_string(),
            "assistant".to_string()
        ]]
    );
    harness.cleanup();
}
