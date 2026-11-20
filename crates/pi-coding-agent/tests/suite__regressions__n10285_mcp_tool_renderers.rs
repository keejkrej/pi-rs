//! Port of packages/coding-agent/test/suite/regressions/10285-mcp-tool-renderers.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use pi_agent_core::types::AgentMessage;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxToolCallOptions,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{Message, StopReason, UserMessage, UserMessageContent};
use pi_coding_agent::core::extensions::types::{InlineExtension, Theme, ToolRenderContext};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::extensions::mcp::config::LoadedMcpConfig;
use pi_coding_agent::extensions::mcp::{McpExtensionOptions, create_mcp_extension};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_coding_agent::utils::ansi::strip_ansi;
use serde_json::json;
use support::suite::harness::{HarnessOptions, create_harness};

fn empty_mcp_extension() -> InlineExtension {
    InlineExtension::Factory(create_mcp_extension(McpExtensionOptions {
        load_config: Some(Arc::new(|_| LoadedMcpConfig {
            servers: Vec::new(),
            auto_enable_codemode: None,
            errors: Vec::new(),
            project_config: None,
        })),
        ..Default::default()
    }))
}

// Regression #10285: a resumed session renders MCP tool calls before their server connected.
#[tokio::test]
async fn renders_calls_to_mcp_tools_that_are_not_registered() {
    init_theme(Some("dark"), None);
    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![empty_mcp_extension()]),
        ..Default::default()
    })
    .await;

    let runner = harness.session.extension_runner().expect("extension runner");
    let resolve = |tool_name: &str| {
        let session = harness.session.clone();
        let name_owned = tool_name.to_string();
        runner.resolve_tool_renderers(tool_name, move || {
            session.get_tool_definition(&name_owned).map(|d| d.to_renderers())
        })
    };

    let renderers = resolve("mcp__my_docs__search").expect("mcp renderer");
    let render_call = renderers.render_call.expect("render_call");
    let comp = render_call(json!({ "query": "pi" }), Theme::default(), ToolRenderContext::default());
    let rendered = strip_ansi(&comp.render(100).join("\n"));
    assert!(
        rendered.contains(r#"my_docs/search query="pi""#),
        "rendered: {rendered}"
    );

    assert!(resolve("not_mcp").is_none());
    assert!(resolve("read").and_then(|r| r.render_call).is_some());

    harness.cleanup();
}

#[tokio::test]
async fn renders_unregistered_mcp_tool_calls_in_html_exports_too() {
    init_theme(Some("dark"), None);
    let dir = tempfile::tempdir().expect("tempdir");
    let dir_str = dir.path().to_str().expect("utf8").to_string();
    let sessions_dir = dir.path().join("sessions");
    let session_manager =
        SessionManager::create(&dir_str, Some(sessions_dir.to_str().expect("utf8")), None).expect("session_manager");

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![empty_mcp_extension()]),
        session_manager: Some(session_manager.clone()),
        ..Default::default()
    })
    .await;

    session_manager
        .append_message(AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Text("search".to_string()),
            timestamp: 1,
        })))
        .unwrap();

    let mut args = IndexMap::new();
    args.insert("query".to_string(), json!("pi"));
    let mut msg = faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![FauxContentBlock::ToolCall(faux_tool_call(
            "mcp__my_docs__search",
            args,
            Some(FauxToolCallOptions {
                id: Some("call-1".to_string()),
            }),
        ))]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            timestamp: Some(2),
            ..Default::default()
        }),
    );
    msg.api = "anthropic-messages".to_string();
    msg.provider = "anthropic".to_string();
    msg.model = "test".to_string();
    session_manager.append_message(AgentMessage::from(msg)).unwrap();

    let out_html = dir.path().join("export.html");
    let exported = harness
        .session
        .export_to_html(Some(out_html.to_str().expect("utf8")), None)
        .await
        .expect("export_to_html");
    let html = std::fs::read_to_string(exported).expect("read html");
    let data_re = regex::Regex::new(r#"<script id="session-data" type="application/json">([^<]*)</script>"#).unwrap();
    let b64 = data_re
        .captures(&html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
        .unwrap_or("");
    let session_json = String::from_utf8(pi_js::b64::decode(b64)).expect("utf8");
    let session_val = pi_js::json::parse(&session_json).expect("json");
    let call_html = session_val
        .get("renderedTools")
        .and_then(|rt| rt.get("call-1"))
        .and_then(|c| c.get("callHtml"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    assert!(
        strip_ansi(call_html).contains("my_docs/search"),
        "callHtml: {call_html}"
    );

    harness.cleanup();
}
