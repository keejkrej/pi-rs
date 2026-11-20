//! Port of packages/agent/test/agent-tool-typebox.test.ts
//!
//! Verifies that [`AgentTool`] accepts schemas produced by `pi_schema` builders,
//! `pi_js::vendor::typebox::t`, and raw TypeBox 1.x JSON Schema objects (`"~kind"` markers),
//! validates calls with [`run_tool_call`], and converts cleanly via [`AgentTool::to_tool`].

mod support;

use std::sync::Arc;

use pi_agent_core::{AgentContext, AgentTool, AgentToolCall, AgentToolResult, RunToolCallOptions, run_tool_call};
use pi_ai::{FauxAssistantMessageContent, JsonObject, faux_assistant_message, validate_tool_arguments};
use pi_js::vendor::typebox::t;
use pi_schema::{Schema, lit, obj, opt, str, union};
use serde_json::{Value, json};

fn assistant_stub() -> pi_ai::AssistantMessage {
    faux_assistant_message(FauxAssistantMessageContent::Blocks(Vec::new()), None)
}

fn json_obj(val: Value) -> JsonObject {
    serde_json::from_value(val).expect("valid JsonObject")
}

fn make_bash_tool(parameters: Schema) -> AgentTool {
    AgentTool {
        name: "bash".to_string(),
        label: "Bash".to_string(),
        description: "Runs a command".to_string(),
        parameters,
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_tool_call_id, params, _signal, _on_update| {
            Box::pin(async move {
                let command = params.get("command").and_then(Value::as_str).unwrap_or("");
                let mode = params.get("mode").and_then(Value::as_str).unwrap_or("default");
                Ok(AgentToolResult::text(format!("{command}:{mode}")))
            })
        }),
        replay: None,
    }
}

#[tokio::test]
async fn agent_tool_accepts_typebox_1x_kind_schema_and_validates_execution() {
    let typebox_json = json!({
        "~kind": "Object",
        "type": "object",
        "properties": {
            "command": { "~kind": "String", "type": "string" },
            "mode": {
                "~kind": "Union",
                "anyOf": [
                    { "~kind": "Literal", "const": "fast" },
                    { "~kind": "Literal", "const": "deep" }
                ],
                "~optional": true
            }
        },
        "required": ["command"]
    });
    let tool = make_bash_tool(Schema::from_value(typebox_json));

    let valid_call = AgentToolCall {
        id: "call_1".to_string(),
        name: "bash".to_string(),
        arguments: json_obj(json!({ "command": "cargo check" })),
    };
    let options = RunToolCallOptions {
        tools: vec![tool.clone()],
        assistant_message: assistant_stub(),
        context: AgentContext::default(),
        before_tool_call: None,
        after_tool_call: None,
        signal: None,
        on_update: None,
    };

    let outcome_default = run_tool_call(valid_call, options.clone()).await;
    assert!(!outcome_default.is_error);
    assert_eq!(outcome_default.result, AgentToolResult::text("cargo check:default"));
    assert!(outcome_default.duration_ms.is_some());

    let deep_call = AgentToolCall {
        id: "call_2".to_string(),
        name: "bash".to_string(),
        arguments: json_obj(json!({ "command": "cargo test", "mode": "deep" })),
    };
    let outcome_deep = run_tool_call(deep_call, options.clone()).await;
    assert!(!outcome_deep.is_error);
    assert_eq!(outcome_deep.result, AgentToolResult::text("cargo test:deep"));

    let invalid_call = AgentToolCall {
        id: "call_3".to_string(),
        name: "bash".to_string(),
        arguments: json_obj(json!({ "command": "cargo test", "mode": "slow" })),
    };
    let outcome_invalid = run_tool_call(invalid_call, options).await;
    assert!(outcome_invalid.is_error);
    assert!(outcome_invalid.duration_ms.is_none());
}

#[tokio::test]
async fn agent_tool_accepts_both_pi_schema_and_vendor_typebox_builders() {
    let pi_schema_params = obj([("command", str()), ("mode", opt(union([lit("fast"), lit("deep")])))]);
    let typebox_builder_params: Schema = t::object([
        ("command", t::string()),
        (
            "mode",
            t::union(vec![t::literal("fast"), t::literal("deep")]).optional(),
        ),
    ])
    .into();

    assert_eq!(
        pi_schema_params.to_value(),
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" },
                "mode": { "anyOf": [{ "const": "fast" }, { "const": "deep" }] }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    );
    assert_eq!(
        typebox_builder_params.to_value().get("required"),
        Some(&json!(["command"]))
    );

    let closed_tool = make_bash_tool(pi_schema_params.clone());
    let extra_prop_call = AgentToolCall {
        id: "tc_extra".to_string(),
        name: "bash".to_string(),
        arguments: json_obj(json!({ "command": "pwd", "extra": true })),
    };
    let extra_outcome = run_tool_call(
        extra_prop_call.clone(),
        RunToolCallOptions {
            tools: vec![closed_tool],
            assistant_message: assistant_stub(),
            context: AgentContext::default(),
            before_tool_call: None,
            after_tool_call: None,
            signal: None,
            on_update: None,
        },
    )
    .await;
    assert!(extra_outcome.is_error);
    assert!(extra_outcome.duration_ms.is_none());

    let open_tool = make_bash_tool(typebox_builder_params.clone());
    let open_extra_outcome = run_tool_call(
        extra_prop_call,
        RunToolCallOptions {
            tools: vec![open_tool],
            assistant_message: assistant_stub(),
            context: AgentContext::default(),
            before_tool_call: None,
            after_tool_call: None,
            signal: None,
            on_update: None,
        },
    )
    .await;
    assert!(!open_extra_outcome.is_error);
    assert_eq!(open_extra_outcome.result, AgentToolResult::text("pwd:default"));

    for parameters in [pi_schema_params, typebox_builder_params] {
        let tool = make_bash_tool(parameters);
        let ai_tool = tool.to_tool();
        let ok_tc = pi_ai::ToolCall {
            id: "tc_ok".to_string(),
            name: "bash".to_string(),
            arguments: json_obj(json!({ "command": "pwd", "mode": "fast" })),
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        };
        let validated = validate_tool_arguments(&ai_tool, &ok_tc).expect("valid args validate");
        assert_eq!(validated, json!({ "command": "pwd", "mode": "fast" }));

        let missing_required = AgentToolCall {
            id: "tc_bad".to_string(),
            name: "bash".to_string(),
            arguments: json_obj(json!({})),
        };
        let outcome = run_tool_call(
            missing_required,
            RunToolCallOptions {
                tools: vec![tool],
                assistant_message: assistant_stub(),
                context: AgentContext::default(),
                before_tool_call: None,
                after_tool_call: None,
                signal: None,
                on_update: None,
            },
        )
        .await;
        assert!(outcome.is_error);
        assert!(outcome.duration_ms.is_none());
    }
}

#[tokio::test]
async fn open_utils_tools_accept_extra_args_and_get_current_time_formats_zones() {
    let calc = support::utils::calculate::calculate_tool();
    let time = support::utils::get_current_time::get_current_time_tool();

    let options = RunToolCallOptions {
        tools: vec![calc, time],
        assistant_message: assistant_stub(),
        context: AgentContext::default(),
        before_tool_call: None,
        after_tool_call: None,
        signal: None,
        on_update: None,
    };

    let calc_out = run_tool_call(
        AgentToolCall {
            id: "calc_open".to_string(),
            name: "calculate".to_string(),
            arguments: json_obj(json!({ "expression": "6 * 7", "ignoredExtra": true })),
        },
        options.clone(),
    )
    .await;
    assert!(!calc_out.is_error);
    assert_eq!(calc_out.result, AgentToolResult::text("6 * 7 = 42"));

    let utc_out = run_tool_call(
        AgentToolCall {
            id: "time_utc".to_string(),
            name: "get_current_time".to_string(),
            arguments: json_obj(json!({ "timezone": "UTC", "ignoredExtra": 1 })),
        },
        options.clone(),
    )
    .await;
    assert!(!utc_out.is_error);
    assert!(
        utc_out
            .result
            .details
            .and_then(|d| d.get("utcTimestamp").and_then(Value::as_i64))
            .is_some()
    );

    let bad_tz_out = run_tool_call(
        AgentToolCall {
            id: "time_bad".to_string(),
            name: "get_current_time".to_string(),
            arguments: json_obj(json!({ "timezone": "Mars/Olympus_Mons" })),
        },
        options,
    )
    .await;
    assert!(bad_tz_out.is_error);
}
