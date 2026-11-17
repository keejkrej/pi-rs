//! Port of packages/coding-agent/test/suite/regressions/8935-parallel-preflight-abort.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use pi_agent_core::types::{AgentTool, AgentToolResult, ToolExecutionMode};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{Message, StopReason, TextContent, UserContentBlock};
use pi_coding_agent::core::agent_session::AgentSessionEvent;
use pi_coding_agent::core::extensions::types::{ExtensionApi, InlineExtension, ToolCallEvent, ToolResultEvent};
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::suite::harness::{HarnessOptions, create_harness};

#[tokio::test]
async fn aborts_all_batched_parallel_tool_calls_without_execution_when_preflight_aborts() {
    let preflights: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let executions: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let result_hooks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let exec_c = Arc::clone(&executions);
    let external_write = AgentTool {
        name: "external_write".to_string(),
        label: "External Write".to_string(),
        description: "Test external write tool".to_string(),
        parameters: t::object(&[t::prop("value", t::string())]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: Some(ToolExecutionMode::Parallel),
        prepare_arguments: None,
        execute: Arc::new(move |_id, params, _sig, _upd| {
            let ex = Arc::clone(&exec_c);
            Box::pin(async move {
                let value = params
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                ex.lock().unwrap_or_else(PoisonError::into_inner).push(value.clone());
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(value))],
                    details: Some(json!({})),
                    structured_content: None,
                    usage: None,
                })
            })
        }),
        replay: None,
    };

    let pf_c = Arc::clone(&preflights);
    let rh_c = Arc::clone(&result_hooks);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let pf = Arc::clone(&pf_c);
        let rh = Arc::clone(&rh_c);
        async move {
            pi.on::<ToolCallEvent>(move |event, ctx| {
                let pf = Arc::clone(&pf);
                Box::pin(async move {
                    let value = event
                        .input_value()
                        .get("value")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    pf.lock().unwrap_or_else(PoisonError::into_inner).push(value.clone());
                    if value == "second" {
                        ctx.abort()?;
                    }
                    Ok(None)
                })
            });
            pi.on::<ToolResultEvent>(move |event, _ctx| {
                let rh = Arc::clone(&rh);
                Box::pin(async move {
                    rh.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(event.tool_call_id().to_string());
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![external_write]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![
            faux_tool_call("external_write", json!({ "value": "first" })),
            faux_tool_call("external_write", json!({ "value": "second" })),
        ]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    ))]);

    harness.session.prompt("run both writes", None).await.unwrap();

    assert_eq!(
        *preflights.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["first".to_string(), "second".to_string()]
    );
    assert!(executions.lock().unwrap_or_else(PoisonError::into_inner).is_empty());
    assert!(result_hooks.lock().unwrap_or_else(PoisonError::into_inner).is_empty());

    let starts: Vec<String> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::ToolExecutionStart { tool_call_id, .. } => Some(tool_call_id),
            _ => None,
        })
        .collect();
    let ends: Vec<(String, bool)> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::ToolExecutionEnd {
                tool_call_id, is_error, ..
            } => Some((tool_call_id, is_error)),
            _ => None,
        })
        .collect();

    assert_eq!(starts.len(), 2);
    assert_eq!(ends.len(), 2);
    assert_eq!(ends.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(), starts);
    assert!(ends.iter().all(|(_, is_err)| *is_err));

    let tool_result_texts: Vec<String> = harness
        .session
        .messages()
        .into_iter()
        .filter_map(|m| match m.into_message() {
            Some(Message::ToolResult(tr)) => Some(
                tr.content
                    .into_iter()
                    .filter_map(|b| match b {
                        UserContentBlock::Text(t) => Some(t.text),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect();
    assert_eq!(
        tool_result_texts,
        vec!["Operation aborted".to_string(), "Operation aborted".to_string()]
    );
    harness.cleanup();
}
