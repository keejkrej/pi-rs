//! Port of packages/coding-agent/test/suite/agent-session-tool-orchestration.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_agent_core::types::AgentToolResult;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{Message, StopReason, TextContent, UserContentBlock};
use pi_ai::utils::transcript::get_current_tools;
use pi_coding_agent::core::agent_session::ExtensionBindings;
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, ToolCallEvent, ToolDefinition, ToolExposure, ToolLoadoutChanges,
};
use pi_coding_agent::extensions::codemode::{CodemodeExtensionOptions, create_codemode_extension};
use pi_coding_agent::extensions::tool_search::create_tool_search_extension;
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::suite::harness::{HarnessOptions, create_harness};

fn orchestrator_extension() -> InlineExtension {
    InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.register_tool(ToolDefinition {
            name: "echo".to_string(),
            label: "echo".to_string(),
            description: "Echo text.".to_string(),
            prompt_snippet: None,
            prompt_guidelines: None,
            parameters: t::object(&[t::prop("text", t::string())]),
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
            execute: Arc::new(|_id, params, _sig, _upd, _ctx| {
                Box::pin(async move {
                    let text = params.get("text").and_then(Value::as_str).unwrap_or("");
                    Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new(format!("echo: {text}")))],
                        details: Some(json!({})),
                        structured_content: None,
                        usage: None,
                    })
                })
            }),
            render_call: None,
            render_result: None,
        })?;

        pi.register_tool(ToolDefinition {
            name: "helper".to_string(),
            label: "helper".to_string(),
            description: "Only reachable from other tools.".to_string(),
            prompt_snippet: None,
            prompt_guidelines: None,
            parameters: t::object(&[]),
            constrained_sampling: None,
            render_shell: None,
            prepare_arguments: None,
            output_schema: None,
            exposure: Some(ToolExposure::Codemode),
            namespace: None,
            annotations: None,
            default_active: None,
            prepare_loadout: None,
            execution_mode: None,
            execute: Arc::new(|_id, _params, _sig, _upd, _ctx| {
                Box::pin(async move {
                    Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new("helped"))],
                        details: Some(json!({})),
                        structured_content: None,
                        usage: None,
                    })
                })
            }),
            render_call: None,
            render_result: None,
        })?;

        pi.register_tool(ToolDefinition {
            name: "run_tools".to_string(),
            label: "run_tools".to_string(),
            description: "Runs tools.".to_string(),
            prompt_snippet: None,
            prompt_guidelines: None,
            parameters: t::object(&[]),
            constrained_sampling: None,
            render_shell: None,
            prepare_arguments: None,
            output_schema: None,
            exposure: Some(ToolExposure::ModelOnly),
            namespace: None,
            annotations: None,
            default_active: None,
            prepare_loadout: Some(Arc::new(|loadout| {
                let callable_names: Vec<&str> = loadout.callable().iter().map(|t| t.name.as_str()).collect();
                let mut descriptions = IndexMap::new();
                descriptions.insert(
                    "run_tools".to_string(),
                    format!("Runs tools: {}", callable_names.join(", ")),
                );
                descriptions.insert(
                    "echo".to_string(),
                    "Echo text (also callable from run_tools).".to_string(),
                );
                Ok(Some(ToolLoadoutChanges {
                    descriptions: Some(descriptions),
                    hidden_declarations: Some(vec!["echo".to_string()]),
                }))
            })),
            execution_mode: None,
            execute: Arc::new(|_id, _params, _sig, _upd, ctx| {
                Box::pin(async move {
                    let helper = ctx.execute_tool("helper", json!({}), None).await?;
                    let echo = ctx.execute_tool("echo", json!({ "text": "hi" }), None).await?;
                    let self_call = ctx.execute_tool("run_tools", json!({}), None).await?;
                    let text: String = [helper, echo, self_call]
                        .into_iter()
                        .filter_map(|outcome| match outcome.result.content.first() {
                            Some(UserContentBlock::Text(t)) => Some(t.text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" | ");
                    Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new(text))],
                        details: Some(json!({})),
                        structured_content: None,
                        usage: None,
                    })
                })
            }),
            render_call: None,
            render_result: None,
        })?;
        Ok(())
    }))
}

#[tokio::test]
async fn supports_tools_that_call_other_tools_under_any_name_through_the_extension_api() {
    let tool_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let tc_ref = Arc::clone(&tool_calls);
    let recorder = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let tc = Arc::clone(&tc_ref);
        async move {
            pi.on::<ToolCallEvent>(move |ev, _ctx| {
                let tc_inner = Arc::clone(&tc);
                Box::pin(async move {
                    let name = ev.tool_name().to_string();
                    let parent = ev.parent_tool_call_id().unwrap_or("top").to_string();
                    tc_inner
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(format!("{name}:{parent}"));
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        initial_active_tool_names: Some(vec![]),
        extension_factories: Some(vec![orchestrator_extension(), recorder]),
        ..Default::default()
    })
    .await;
    harness
        .session
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();

    assert_eq!(harness.session.get_active_tool_names(), vec!["echo", "run_tools"]);
    assert_eq!(harness.session.get_callable_tool_names(), vec!["echo", "helper"]);
    let tools = harness.session.agent().state().tools;
    assert_eq!(
        tools.iter().find(|t| t.name == "run_tools").map(|t| t.description.as_str()),
        Some("Runs tools: echo, helper")
    );
    assert_eq!(
        tools.iter().find(|t| t.name == "echo").map(|t| t.description.as_str()),
        Some("Echo text (also callable from run_tools).")
    );

    let request_tools: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let rt_ref = Arc::clone(&request_tools);
    harness.set_responses(vec![
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let rt = Arc::clone(&rt_ref);
            Box::pin(async move {
                let names: Vec<String> = get_current_tools(&ctx.messages).into_iter().map(|t| t.name).collect();
                rt.lock().unwrap_or_else(PoisonError::into_inner).push(names);
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                        "run_tools",
                        serde_json::Map::new(),
                        None,
                    ))),
                    Some(FauxAssistantMessageOptions {
                        stop_reason: Some(StopReason::ToolUse),
                        ..Default::default()
                    }),
                ))
            })
        })),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("done".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("go", None).await.unwrap();

    assert_eq!(
        request_tools.lock().unwrap_or_else(PoisonError::into_inner)[0],
        vec!["run_tools".to_string()]
    );
    let result = harness
        .session
        .messages()
        .into_iter()
        .find_map(|m| match m {
            pi_agent_core::types::AgentMessage::Message(Message::ToolResult(tr)) => Some(tr),
            _ => None,
        })
        .unwrap();
    let content_text = match result.content.first() {
        Some(UserContentBlock::Text(t)) => t.text.as_str(),
        _ => "",
    };
    assert_eq!(content_text, "helped | echo: hi | Tool run_tools not found");
    let parent = result.tool_call_id.clone();
    assert_eq!(
        *tool_calls.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            "run_tools:top".to_string(),
            format!("helper:{parent}"),
            format!("echo:{parent}"),
        ]
    );
    let nested = result.nested_calls.clone().unwrap();
    let summary: Vec<(String, String, &'static str)> = nested
        .calls
        .iter()
        .map(|c| {
            (
                c.id.clone(),
                c.name.clone(),
                if c.status == pi_ai::types::NestedToolCallStatus::Ok {
                    "ok"
                } else {
                    "error"
                },
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (format!("{parent}/1"), "helper".to_string(), "ok"),
            (format!("{parent}/2"), "echo".to_string(), "ok"),
            (format!("{parent}/3"), "run_tools".to_string(), "error"),
        ]
    );
    let persisted_nested = harness
        .session_manager
        .get_branch(None)
        .into_iter()
        .find_map(|e| match e {
            pi_coding_agent::core::session_manager::SessionEntry::Message(me) => match me.message {
                pi_agent_core::types::AgentMessage::Message(Message::ToolResult(tr)) => tr.nested_calls,
                _ => None,
            },
            _ => None,
        });
    assert_eq!(persisted_nested, Some(nested));

    harness.cleanup();
}

#[tokio::test]
async fn registers_codemode_and_tool_search_inactive_until_they_are_named() {
    let factories = vec![
        InlineExtension::Factory(create_codemode_extension(CodemodeExtensionOptions::default())),
        InlineExtension::Factory(create_tool_search_extension()),
    ];
    let plain = create_harness(HarnessOptions {
        extension_factories: Some(factories.clone()),
        ..Default::default()
    })
    .await;
    let all_names: Vec<String> = plain.session.get_all_tools().into_iter().map(|t| t.name).collect();
    assert!(all_names.iter().any(|n| n == "codemode"));
    assert!(all_names.iter().any(|n| n == "tool_search"));
    assert_eq!(plain.session.get_active_tool_names(), vec!["read", "bash", "edit", "write"]);
    plain.cleanup();

    let allowed = create_harness(HarnessOptions {
        allowed_tool_names: Some(vec!["read".to_string(), "codemode".to_string()]),
        extension_factories: Some(factories.clone()),
        ..Default::default()
    })
    .await;
    assert_eq!(allowed.session.get_active_tool_names(), vec!["read", "codemode"]);
    allowed.cleanup();

    let initial = create_harness(HarnessOptions {
        initial_active_tool_names: Some(vec!["tool_search".to_string()]),
        extension_factories: Some(factories),
        ..Default::default()
    })
    .await;
    assert_eq!(initial.session.get_active_tool_names(), vec!["tool_search"]);
    initial.cleanup();
}

#[tokio::test]
async fn leaves_results_without_nested_calls_unchanged() {
    let harness = create_harness(HarnessOptions {
        initial_active_tool_names: Some(vec![]),
        extension_factories: Some(vec![orchestrator_extension()]),
        ..Default::default()
    })
    .await;
    harness
        .session
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();
    let mut args = serde_json::Map::new();
    args.insert("text".to_string(), Value::String("x".to_string()));
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call("echo", args, None))),
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

    harness.session.prompt("go", None).await.unwrap();
    let result = harness
        .session
        .messages()
        .into_iter()
        .find_map(|m| match m {
            pi_agent_core::types::AgentMessage::Message(Message::ToolResult(tr)) => Some(tr),
            _ => None,
        })
        .unwrap();
    assert!(result.nested_calls.is_none());
    harness.cleanup();
}
