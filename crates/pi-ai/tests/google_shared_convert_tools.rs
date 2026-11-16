//! Port of packages/ai/test/google-shared-convert-tools.test.ts

use pi_ai::api::google_shared::{
    GoogleFunctionCallingMode, convert_tools, resolve_google_function_calling_mode,
    supports_google_strict_tool_sampling,
};
use pi_ai::{ConstrainedSamplingConfig, JsonSchemaStrict, TSchema, Tool};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

fn make_tool(parameters: Value) -> Tool {
    Tool {
        name: "test_tool".to_owned(),
        description: "A test tool".to_owned(),
        parameters: TSchema::from(parameters),
        constrained_sampling: None,
    }
}

#[test]
fn strips_json_schema_meta_keys_from_parameters_when_use_parameters_is_true() {
    let tools = [make_tool(json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "$id": "urn:bash-tool",
        "$comment": "A bash tool for demonstration",
        "$defs": {
            "commandDef": { "type": "string" }
        },
        "definitions": {
            "legacyDef": { "type": "number" }
        },
        "type": "object",
        "properties": {
            "command": { "type": "string" }
        },
        "required": ["command"]
    }))];

    let result = convert_tools(&tools, true, true)
        .expect("convert_tools should succeed")
        .expect("result should be Some");
    let decl = &result[0].function_declarations[0];
    let params = decl.parameters.as_ref().expect("parameters should be populated");

    assert_eq!(
        *params,
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" }
            },
            "required": ["command"]
        })
    );
    let obj = params.as_object().expect("params should be an object");
    assert!(!obj.contains_key("$schema"));
    assert!(!obj.contains_key("$id"));
    assert!(!obj.contains_key("$comment"));
    assert!(!obj.contains_key("$defs"));
    assert!(!obj.contains_key("definitions"));
}

#[test]
fn recursively_strips_nested_json_schema_meta_keys() {
    let tools = [make_tool(json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "deep": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "$id": "urn:nested",
                "type": "string"
            }
        }
    }))];

    let result = convert_tools(&tools, true, true)
        .expect("convert_tools should succeed")
        .expect("result should be Some");
    let decl = &result[0].function_declarations[0];

    assert_eq!(
        decl.parameters,
        Some(json!({
            "type": "object",
            "properties": {
                "deep": {
                    "type": "string"
                }
            }
        }))
    );
}

#[test]
fn preserves_ref_while_stripping_meta_keys() {
    let tools = [make_tool(json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "refProp": {
                "$ref": "#/$defs/someDef",
                "type": "string"
            }
        }
    }))];

    let result = convert_tools(&tools, true, true)
        .expect("convert_tools should succeed")
        .expect("result should be Some");
    let decl = &result[0].function_declarations[0];

    assert_eq!(
        decl.parameters,
        Some(json!({
            "type": "object",
            "properties": {
                "refProp": {
                    "$ref": "#/$defs/someDef",
                    "type": "string"
                }
            }
        }))
    );
}

#[test]
fn does_not_mutate_the_original_tool_parameters_object() {
    let original_parameters = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "command": { "type": "string" }
        },
        "required": ["command"]
    });
    let tools = [make_tool(original_parameters.clone())];

    let _ = convert_tools(&tools, true, true).expect("convert_tools should succeed");

    assert_eq!(tools[0].parameters.to_value(), original_parameters);
}

#[test]
fn preserves_schema_in_parameters_json_schema_when_use_parameters_is_false() {
    let tools = [make_tool(json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "command": { "type": "string" }
        },
        "required": ["command"]
    }))];

    let result = convert_tools(&tools, false, true)
        .expect("convert_tools should succeed")
        .expect("result should be Some");
    let decl = &result[0].function_declarations[0];

    assert_eq!(
        decl.parameters_json_schema,
        Some(json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {
                "command": { "type": "string" }
            },
            "required": ["command"]
        }))
    );
}

#[test]
fn handles_tools_without_schema_gracefully() {
    let tools = [make_tool(json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" }
        },
        "required": ["path"]
    }))];

    let result = convert_tools(&tools, true, true)
        .expect("convert_tools should succeed")
        .expect("result should be Some");
    let decl = &result[0].function_declarations[0];

    assert_eq!(
        decl.parameters,
        Some(json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            },
            "required": ["path"]
        }))
    );
}

#[test]
fn uses_validated_function_calling_for_strict_tools_on_gemini_3() {
    let mut tool = make_tool(json!({ "type": "object", "properties": {} }));
    tool.constrained_sampling = Some(
        ConstrainedSamplingConfig::JsonSchema {
            strict: JsonSchemaStrict::Require,
        }
        .into(),
    );

    assert!(supports_google_strict_tool_sampling("gemini-3.1-pro-preview"));
    assert!(!supports_google_strict_tool_sampling("gemini-2.5-pro"));
    assert_eq!(
        resolve_google_function_calling_mode(&[tool.clone()], None, true).expect("gemini 3 should allow strict mode"),
        Some(GoogleFunctionCallingMode::Validated)
    );
    let err = resolve_google_function_calling_mode(&[tool], None, false)
        .expect_err("non-strict model must reject strict tool");
    assert!(
        err.to_string()
            .contains("Tool \"test_tool\" requires JSON-schema constrained sampling"),
        "unexpected error: {err}"
    );
}

#[test]
fn returns_none_for_empty_tool_list() {
    assert_eq!(
        convert_tools(&[], false, true).expect("empty tools should succeed"),
        None
    );
    assert_eq!(
        convert_tools(&[], true, true).expect("empty tools should succeed"),
        None
    );
}
