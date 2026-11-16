//! Port of packages/ai/test/validation.test.ts (unit ai-utils-messages)

use indexmap::IndexMap;
use pi_ai::types::{Tool, ToolCall};
use pi_ai::utils::validation::validate_tool_arguments;
use pi_js::vendor::typebox::{Schema, t};
use serde_json::{Value, json};

fn create_tool_call_with_plain_schema(schema: Value, value: Value) -> (Tool, ToolCall) {
    let tool = Tool {
        name: "echo".to_string(),
        description: "Echo tool".to_string(),
        parameters: Schema::from(json!({
            "type": "object",
            "properties": { "value": schema },
            "required": ["value"]
        })),
        constrained_sampling: None,
    };
    let mut args = IndexMap::new();
    args.insert("value".to_string(), value);
    let tc = ToolCall {
        id: "tool-1".to_string(),
        name: "echo".to_string(),
        namespace: None,
        arguments: args,
        thought_signature: None,
    };
    (tool, tc)
}

#[test]
fn coerces_serialized_plain_json_schemas_with_ajv_compatible_primitive_rules() {
    let cases = [
        (json!({ "type": "number" }), json!("42"), json!(42)),
        (json!({ "type": "number" }), json!(true), json!(1)),
        (json!({ "type": "number" }), Value::Null, json!(0)),
        (json!({ "type": "integer" }), json!("42"), json!(42)),
        (json!({ "type": "boolean" }), json!("true"), json!(true)),
        (json!({ "type": "boolean" }), json!("false"), json!(false)),
        (json!({ "type": "boolean" }), json!(1), json!(true)),
        (json!({ "type": "boolean" }), json!(0), json!(false)),
        (json!({ "type": "string" }), Value::Null, json!("")),
        (json!({ "type": "string" }), json!(true), json!("true")),
        (json!({ "type": "null" }), json!(""), Value::Null),
        (json!({ "type": "null" }), json!(0), Value::Null),
        (json!({ "type": "null" }), json!(false), Value::Null),
        (json!({ "type": ["number", "string"] }), json!("1"), json!("1")),
        (json!({ "type": ["boolean", "number"] }), json!("1"), json!(1)),
    ];

    for (schema, input, expected) in cases {
        let (tool, tc) = create_tool_call_with_plain_schema(schema, input);
        assert_eq!(
            validate_tool_arguments(&tool, &tc).unwrap(),
            json!({ "value": expected })
        );
    }
}

#[test]
fn treats_null_as_omission_and_preserves_nullable_schemas() {
    let tool = Tool {
        name: "echo".to_string(),
        description: "Echo tool".to_string(),
        parameters: t::object([
            ("path", t::string()),
            ("offset", t::number().optional()),
            ("nullable", t::union([t::string(), t::null()]).optional()),
            ("metadata", t::object([("enabled", t::boolean().optional())])),
        ]),
        constrained_sampling: None,
    };
    let mut args = IndexMap::new();
    args.insert("path".to_string(), json!("file.txt"));
    args.insert("offset".to_string(), Value::Null);
    args.insert("nullable".to_string(), Value::Null);
    args.insert("metadata".to_string(), json!({ "enabled": null }));
    let tc = ToolCall {
        id: "tool-1".to_string(),
        name: "echo".to_string(),
        namespace: None,
        arguments: args,
        thought_signature: None,
    };
    assert_eq!(
        validate_tool_arguments(&tool, &tc).unwrap(),
        json!({ "path": "file.txt", "nullable": null, "metadata": {} })
    );

    let ref_tool = Tool {
        name: "echo".to_string(),
        description: "Echo tool".to_string(),
        parameters: Schema::from(json!({
            "type": "object",
            "properties": { "value": { "$ref": "#/$defs/value" } },
            "$defs": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } }
        })),
        constrained_sampling: None,
    };
    let mut ref_args = IndexMap::new();
    ref_args.insert("value".to_string(), Value::Null);
    let ref_tc = ToolCall {
        id: "tool-1".to_string(),
        name: "echo".to_string(),
        namespace: None,
        arguments: ref_args,
        thought_signature: None,
    };
    assert_eq!(
        validate_tool_arguments(&ref_tool, &ref_tc).unwrap(),
        json!({ "value": null })
    );

    let union_tool = Tool {
        name: "echo".to_string(),
        description: "Echo tool".to_string(),
        parameters: t::object([("value", t::union([t::number(), t::null()]))]),
        constrained_sampling: None,
    };
    assert_eq!(
        validate_tool_arguments(&union_tool, &ref_tc).unwrap(),
        json!({ "value": null })
    );

    let (oneof_tool, oneof_tc) = create_tool_call_with_plain_schema(
        json!({ "oneOf": [{ "type": "number" }, { "type": "null" }] }),
        Value::Null,
    );
    assert_eq!(
        validate_tool_arguments(&oneof_tool, &oneof_tc).unwrap(),
        json!({ "value": null })
    );

    let (anyof_tool, anyof_tc) = create_tool_call_with_plain_schema(
        json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] }),
        json!("42"),
    );
    assert_eq!(
        validate_tool_arguments(&anyof_tool, &anyof_tc).unwrap(),
        json!({ "value": 42 })
    );

    let (arr_tool, arr_tc) = create_tool_call_with_plain_schema(
        json!({ "type": ["array", "null"], "items": { "type": "string" } }),
        Value::Null,
    );
    assert_eq!(
        validate_tool_arguments(&arr_tool, &arr_tc).unwrap(),
        json!({ "value": null })
    );
}

#[test]
fn rejects_invalid_coercions_for_serialized_plain_json_schemas() {
    let failing = [
        (json!({ "type": "boolean" }), json!("1")),
        (json!({ "type": "boolean" }), json!("0")),
        (json!({ "type": "null" }), json!("null")),
        (json!({ "type": "integer" }), json!("42.1")),
    ];

    for (schema, input) in failing {
        let (tool, tc) = create_tool_call_with_plain_schema(schema, input);
        let err = validate_tool_arguments(&tool, &tc).unwrap_err();
        assert!(err.message().contains("Validation failed"), "msg={}", err.message());
    }
}
