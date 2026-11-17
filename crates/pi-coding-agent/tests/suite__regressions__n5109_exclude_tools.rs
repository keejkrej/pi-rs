//! Port of packages/coding-agent/test/suite/regressions/5109-exclude-tools.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use pi_agent_core::types::AgentToolResult;
use pi_ai::types::{TextContent, UserContentBlock};
use pi_coding_agent::core::extensions::types::{ExtensionApi, InlineExtension, SessionStartEvent, ToolDefinition};
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::json;
use support::suite::harness::{Harness, HarnessOptions, create_harness};

fn tool_names(harness: &Harness) -> Vec<String> {
    let mut names: Vec<String> = harness
        .session
        .get_all_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    names.sort();
    names
}

fn extension_factories() -> Vec<InlineExtension> {
    vec![InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        let api = pi.clone();
        pi.on::<SessionStartEvent>(move |_event, _ctx| {
            let pi = api.clone();
            Box::pin(async move {
                pi.register_tool(ToolDefinition {
                    name: "ask_question".to_string(),
                    label: "Ask Question".to_string(),
                    description: "Ask a question".to_string(),
                    prompt_snippet: Some("Ask a question".to_string()),
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
                                content: vec![UserContentBlock::Text(TextContent::new("ok"))],
                                details: Some(json!({})),
                                ..Default::default()
                            })
                        })
                    }),
                    render_call: None,
                    render_result: None,
                })?;
                pi.register_tool(ToolDefinition {
                    name: "dynamic_tool".to_string(),
                    label: "Dynamic Tool".to_string(),
                    description: "Dynamic test tool".to_string(),
                    prompt_snippet: Some("Run dynamic test behavior".to_string()),
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
                                content: vec![UserContentBlock::Text(TextContent::new("ok"))],
                                details: Some(json!({})),
                                ..Default::default()
                            })
                        })
                    }),
                    render_call: None,
                    render_result: None,
                })?;
                Ok(None)
            })
        });
        Ok(())
    }))]
}

#[tokio::test]
async fn filters_built_in_and_extension_tools_from_available_and_active_tools() {
    let harness = create_harness(HarnessOptions {
        excluded_tool_names: Some(vec!["read".to_string(), "ask_question".to_string()]),
        extension_factories: Some(extension_factories()),
        ..Default::default()
    })
    .await;
    harness.session.bind_extensions(Default::default()).await.unwrap();

    let all_tool_names = tool_names(&harness);
    assert!(!all_tool_names.contains(&"read".to_string()));
    assert!(!all_tool_names.contains(&"ask_question".to_string()));
    assert!(all_tool_names.contains(&"bash".to_string()));
    assert!(all_tool_names.contains(&"dynamic_tool".to_string()));

    let mut active = harness.session.get_active_tool_names();
    active.sort();
    assert_eq!(active, vec!["bash", "dynamic_tool", "edit", "write"]);
    let prompt = harness.session.system_prompt();
    assert!(!prompt.contains("- read:"));
    assert!(!prompt.contains("ask_question"));
    assert!(prompt.contains("- dynamic_tool: Run dynamic test behavior"));
    harness.cleanup();
}

#[tokio::test]
async fn lets_excluded_tools_override_the_allowlist() {
    let harness = create_harness(HarnessOptions {
        allowed_tool_names: Some(vec!["read".to_string(), "bash".to_string(), "ask_question".to_string()]),
        excluded_tool_names: Some(vec!["read".to_string(), "ask_question".to_string()]),
        initial_active_tool_names: Some(vec!["read".to_string(), "bash".to_string(), "ask_question".to_string()]),
        extension_factories: Some(extension_factories()),
        ..Default::default()
    })
    .await;
    harness.session.bind_extensions(Default::default()).await.unwrap();

    assert_eq!(tool_names(&harness), vec!["bash"]);
    assert_eq!(harness.session.get_active_tool_names(), vec!["bash"]);
    let prompt = harness.session.system_prompt();
    assert!(prompt.contains("- bash:"));
    assert!(!prompt.contains("- read:"));
    assert!(!prompt.contains("ask_question"));
    harness.cleanup();
}

#[tokio::test]
async fn matches_allowlist_and_denylist_patterns() {
    let harness = create_harness(HarnessOptions {
        allowed_tool_names: Some(vec!["*_tool".to_string(), "ask_*".to_string(), "re*".to_string()]),
        excluded_tool_names: Some(vec!["ask*".to_string()]),
        initial_active_tool_names: Some(vec!["*_tool".to_string(), "ask_*".to_string(), "re*".to_string()]),
        extension_factories: Some(extension_factories()),
        ..Default::default()
    })
    .await;
    harness.session.bind_extensions(Default::default()).await.unwrap();

    assert_eq!(tool_names(&harness), vec!["dynamic_tool", "read"]);
    let mut active = harness.session.get_active_tool_names();
    active.sort();
    assert_eq!(active, vec!["dynamic_tool", "read"]);
    harness.cleanup();
}
