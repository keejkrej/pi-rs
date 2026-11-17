//! Port of packages/coding-agent/test/codemode-renderer.test.ts

use std::sync::{Arc, Mutex};

use pi_agent_core::AgentToolResult;
use pi_ai::{TextContent, UserContentBlock};
use pi_coding_agent::core::extensions::types::{Theme, ToolRenderContext, ToolRenderResultOptions};
use pi_coding_agent::extensions::codemode::renderer::codemode_render_result;
use pi_coding_agent::extensions::codemode::tool::{CodemodeNestedCall, CodemodeNestedCallStatus, CodemodeToolDetails};
use pi_tui::utils::strip_terminal_sequences;
use serde_json::{Value, json};

fn render(result: AgentToolResult, is_error: bool, expanded: bool, width: usize) -> String {
    let context = ToolRenderContext {
        args: json!({ "code": "" }),
        tool_call_id: "call".to_string(),
        invalidate: Arc::new(|| {}),
        last_component: None,
        state: Arc::new(Mutex::new(None)),
        cwd: "/".to_string(),
        execution_started: true,
        args_complete: true,
        is_partial: false,
        expanded,
        show_images: false,
        is_error,
    };
    let render_fn = codemode_render_result();
    let component = render_fn(
        result,
        ToolRenderResultOptions {
            expanded,
            is_partial: false,
        },
        Theme::default(),
        context,
    );
    strip_terminal_sequences(&component.render(width).join("\n"))
        .split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn details_val(details: &CodemodeToolDetails) -> Option<Value> {
    pi_js::json::parse(&pi_js::json::stringify(details)).ok()
}

#[test]
fn hides_the_script_header_and_shows_the_output() {
    let details = CodemodeToolDetails {
        calls: vec![CodemodeNestedCall {
            id: "call/1".to_string(),
            name: "read".to_string(),
            args: r#"{"path":"a"}"#.to_string(),
            status: CodemodeNestedCallStatus::Ok,
            duration_ms: Some(5.0),
            cost: None,
            error: None,
        }],
        full_output_path: None,
    };
    let result = AgentToolResult {
        content: vec![
            UserContentBlock::Text(TextContent::new("Script completed\nWall time 0.1 seconds\nOutput:\n")),
            UserContentBlock::Text(TextContent::new("hello")),
        ],
        details: details_val(&details),
        ..AgentToolResult::default()
    };
    assert_eq!(render(result, false, true, 200), "✓ read {\"path\":\"a\"} 5ms\n\nhello");
}

#[test]
fn shows_cost_of_model_calls_and_their_total() {
    let details = CodemodeToolDetails {
        calls: vec![
            CodemodeNestedCall {
                id: "call/models.classify/1".to_string(),
                name: "models.classify".to_string(),
                args: "scorer/judge".to_string(),
                status: CodemodeNestedCallStatus::Ok,
                duration_ms: Some(5.0),
                cost: Some(0.000012936),
                error: None,
            },
            CodemodeNestedCall {
                id: "call/models.classify/2".to_string(),
                name: "models.classify".to_string(),
                args: "scorer/judge".to_string(),
                status: CodemodeNestedCallStatus::Ok,
                duration_ms: Some(5.0),
                cost: Some(0.02),
                error: None,
            },
            CodemodeNestedCall {
                id: "call/models.classify/3".to_string(),
                name: "models.classify".to_string(),
                args: "scorer/judge".to_string(),
                status: CodemodeNestedCallStatus::Ok,
                duration_ms: Some(5.0),
                cost: None,
                error: None,
            },
        ],
        full_output_path: None,
    };
    let result = AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent::new(
            "Script completed\nWall time 0.1 seconds\nOutput:\n",
        ))],
        details: details_val(&details),
        ..AgentToolResult::default()
    };
    assert_eq!(
        render(result, false, true, 200),
        [
            "✓ models.classify scorer/judge 5ms $0.000013",
            "✓ models.classify scorer/judge 5ms $0.02",
            "✓ models.classify scorer/judge 5ms",
            "Model calls: $0.02",
        ]
        .join("\n")
    );
}

#[test]
fn shows_results_without_a_header_such_as_rejected_options() {
    let result = AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent::new(
            "The @options line must be followed by JavaScript source",
        ))],
        details: None,
        ..AgentToolResult::default()
    };
    assert_eq!(
        render(result, true, true, 200),
        "The @options line must be followed by JavaScript source"
    );
}

#[test]
fn limits_collapsed_output_to_wrapped_lines_not_logical_lines() {
    let details = CodemodeToolDetails {
        calls: Vec::new(),
        full_output_path: Some("/tmp/out.txt".to_string()),
    };
    let result = AgentToolResult {
        content: vec![
            UserContentBlock::Text(TextContent::new("Script completed\nWall time 0.1 seconds\nOutput:\n")),
            UserContentBlock::Text(TextContent::new(&"x".repeat(1000))),
        ],
        details: details_val(&details),
        ..AgentToolResult::default()
    };
    let rendered = render(result, false, false, 50);
    let lines: Vec<&str> = rendered.split('\n').collect();
    assert_eq!(lines.len(), 7);
    assert_eq!(&lines[..5], &[&"x".repeat(50); 5]);
    assert!(lines[5].starts_with("... (15 more lines,"));
    assert_eq!(lines[6], "Full output: /tmp/out.txt");
}
