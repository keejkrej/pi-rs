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

// Regression coverage for #11308.
#[test]
fn preserves_literal_data_under_non_schema_keywords() {
    for key in ["default", "examples", "enum", "const", "x-custom"] {
        let literal = json!({ "const": "literal-data", "properties": { "nested": { "const": 1 } } });
        let value = if key == "enum" || key == "examples" {
            json!([literal])
        } else {
            literal
        };
        let mut prop = serde_json::Map::new();
        prop.insert("type".to_owned(), json!("object"));
        prop.insert(key.to_owned(), value);
        let parameters = json!({ "type": "object", "properties": { "value": Value::Object(prop) } });
        let original = parameters.clone();

        let result = convert_tools(&[make_tool(parameters)], false, true)
            .expect("convert_tools")
            .expect("some tools");
        assert_eq!(
            result[0].function_declarations[0].parameters_json_schema,
            Some(original)
        );
    }
}

#[test]
fn preserves_names_while_rewriting_schemas_in_nested_maps() {
    for key in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
        "dependencies",
    ] {
        let map_val = json!({
            "__proto__": { "const": "x" },
            "const": { "const": "y" },
            "properties": { "const": "z" }
        });
        let expected_map = json!({
            "__proto__": { "enum": ["x"] },
            "const": { "enum": ["y"] },
            "properties": { "enum": ["z"] }
        });
        let mut parameters = serde_json::Map::new();
        parameters.insert("type".to_owned(), json!("object"));
        parameters.insert(key.to_owned(), map_val);
        let mut expected = serde_json::Map::new();
        expected.insert("type".to_owned(), json!("object"));
        expected.insert(key.to_owned(), expected_map);

        let result = convert_tools(&[make_tool(Value::Object(parameters))], false, true)
            .expect("convert_tools")
            .expect("some tools");
        let schema = result[0].function_declarations[0]
            .parameters_json_schema
            .as_ref()
            .unwrap();
        let expected_val = Value::Object(expected);
        assert_eq!(schema, &expected_val);
        assert_eq!(pi_js::json::stringify(schema), pi_js::json::stringify(&expected_val));
    }
}

#[test]
fn rewrites_only_schema_bearing_keywords() {
    let parameters = json!({
        "type": "object",
        "additionalProperties": { "const": "x" },
        "unevaluatedProperties": false,
        "propertyNames": { "const": "name" },
        "allOf": [{ "if": { "const": "x" }, "then": { "const": "y" }, "else": { "not": { "const": "z" } } }],
        "properties": {
            "list": {
                "type": "array",
                "items": [{ "const": 1 }],
                "prefixItems": [{ "const": 2 }],
                "additionalItems": { "const": 3 },
                "contains": { "const": 4 },
                "unevaluatedItems": { "const": 5 }
            }
        },
        "dependencies": { "list": ["other"] }
    });
    let expected = json!({
        "type": "object",
        "additionalProperties": { "enum": ["x"] },
        "unevaluatedProperties": false,
        "propertyNames": { "enum": ["name"] },
        "allOf": [{ "if": { "enum": ["x"] }, "then": { "enum": ["y"] }, "else": { "not": { "enum": ["z"] } } }],
        "properties": {
            "list": {
                "type": "array",
                "items": [{ "enum": [1] }],
                "prefixItems": [{ "enum": [2] }],
                "additionalItems": { "enum": [3] },
                "contains": { "enum": [4] },
                "unevaluatedItems": { "enum": [5] }
            }
        },
        "dependencies": { "list": ["other"] }
    });

    let result = convert_tools(&[make_tool(parameters)], false, true)
        .expect("convert_tools")
        .expect("some tools");
    assert_eq!(
        result[0].function_declarations[0].parameters_json_schema,
        Some(expected)
    );
}

#[test]
fn rewrites_string_and_number_const_values_to_single_value_enums_in_parameters_json_schema() {
    let parameters = json!({
        "type": "object",
        "properties": {
            "origin": { "type": "string", "const": "zq-src-7" },
            "mode": {
                "anyOf": [
                    { "type": "string", "const": "a" },
                    { "type": "string", "const": "b" }
                ]
            },
            "level": { "oneOf": [{ "const": 1 }, { "const": 2 }] },
            "flag": { "type": "boolean", "const": true },
            "list": { "type": "array", "items": { "const": "x" } },
            "const": { "type": "string" }
        },
        "required": ["origin"]
    });
    let expected = json!({
        "type": "object",
        "properties": {
            "origin": { "type": "string", "enum": ["zq-src-7"] },
            "mode": {
                "anyOf": [
                    { "type": "string", "enum": ["a"] },
                    { "type": "string", "enum": ["b"] }
                ]
            },
            "level": { "oneOf": [{ "enum": [1] }, { "enum": [2] }] },
            "flag": { "type": "boolean", "const": true },
            "list": { "type": "array", "items": { "enum": ["x"] } },
            "const": { "type": "string" }
        },
        "required": ["origin"]
    });

    let result = convert_tools(&[make_tool(parameters)], false, true)
        .expect("convert_tools")
        .expect("some tools");
    assert_eq!(
        result[0].function_declarations[0].parameters_json_schema,
        Some(expected)
    );
}
