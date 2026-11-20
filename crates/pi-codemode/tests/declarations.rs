//! Port of packages/codemode/test/declarations.test.ts

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::{Value, json};

use pi_codemode::{
    CodemodeJsonSchema, CodemodeTool, RenderDeclarationsOptions, SchemaToTypeOptions, render_declarations,
    render_tool_sample, render_tool_signature, resolve_schema_ref, schema_to_type, to_codemode_identifier,
};

fn noop_tool(name: &str) -> CodemodeTool {
    CodemodeTool {
        name: name.to_string(),
        description: None,
        input_schema: None,
        output_schema: None,
        spread: None,
        signature: None,
        execute: Arc::new(|_, _| Box::pin(async { Ok(None) })),
    }
}

fn schema_obj(value: Value) -> CodemodeJsonSchema {
    let map: IndexMap<String, Value> = value
        .as_object()
        .expect("object schema")
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    CodemodeJsonSchema::Object(map)
}

#[test]
fn identifier_normalizes_invalid_characters() {
    assert_eq!(to_codemode_identifier("read"), "read");
    assert_eq!(to_codemode_identifier("fs-read"), "fs_read");
    assert_eq!(to_codemode_identifier("123tool"), "_23tool");
    assert_eq!(to_codemode_identifier("a.b/c:d"), "a_b_c_d");
    assert_eq!(to_codemode_identifier("_$ok1"), "_$ok1");
    assert_eq!(to_codemode_identifier(""), "_");
}

#[test]
fn schema_to_type_renders_primitives_literals_and_unions() {
    assert_eq!(schema_to_type(&schema_obj(json!({ "type": "string" })), None), "string");
    assert_eq!(
        schema_to_type(&schema_obj(json!({ "type": "integer" })), None),
        "number"
    );
    assert_eq!(
        schema_to_type(&schema_obj(json!({ "type": ["string", "null"] })), None),
        "string | null"
    );
    assert_eq!(schema_to_type(&schema_obj(json!({ "const": "a" })), None), "\"a\"");
    assert_eq!(
        schema_to_type(&schema_obj(json!({ "enum": ["a", 1, null] })), None),
        "\"a\" | 1 | null"
    );
    assert_eq!(
        schema_to_type(
            &schema_obj(json!({ "anyOf": [{ "type": "string" }, { "type": "number" }] })),
            None
        ),
        "string | number"
    );
    assert_eq!(
        schema_to_type(&schema_obj(json!({ "anyOf": [{ "type": "string" }, {}] })), None),
        "unknown"
    );
    assert_eq!(
        schema_to_type(
            &schema_obj(json!({
                "allOf": [
                    { "anyOf": [{ "type": "string" }, { "type": "number" }] },
                    { "const": 1 }
                ]
            })),
            None
        ),
        "(string | number) & 1"
    );
    assert_eq!(
        schema_to_type(&schema_obj(json!({ "$ref": "#/defs/x" })), None),
        "unknown"
    );
    assert_eq!(schema_to_type(&CodemodeJsonSchema::Boolean(true), None), "unknown");
    assert_eq!(schema_to_type(&CodemodeJsonSchema::Boolean(false), None), "never");
}

#[test]
fn schema_to_type_renders_objects_and_descriptions() {
    assert_eq!(
        schema_to_type(
            &schema_obj(json!({
                "type": "object",
                "properties": { "city": { "type": "string" }, "max-lines": { "type": "number" } },
                "required": ["city"],
                "additionalProperties": false
            })),
            None
        ),
        "{ city: string; \"max-lines\"?: number; }"
    );
    assert_eq!(
        schema_to_type(
            &schema_obj(json!({ "type": "object", "additionalProperties": { "type": "number" } })),
            None
        ),
        "{ [key: string]: number; }"
    );
    assert_eq!(
        schema_to_type(&schema_obj(json!({ "type": "object" })), None),
        "{ [key: string]: unknown; }"
    );
    assert_eq!(
        schema_to_type(
            &schema_obj(json!({ "type": "object", "properties": {}, "additionalProperties": false })),
            None
        ),
        "{}"
    );

    assert_eq!(
        schema_to_type(
            &schema_obj(json!({
                "type": "object",
                "properties": {
                    "weather": {
                        "type": "array",
                        "description": "look up weather for a given list of locations",
                        "items": {
                            "type": "object",
                            "properties": { "location": { "type": "string" } },
                            "required": ["location"]
                        }
                    }
                },
                "required": ["weather"]
            })),
            None
        ),
        "{\n  // look up weather for a given list of locations\n  weather: Array<{ location: string; }>;\n}"
    );
    assert_eq!(
        schema_to_type(
            &schema_obj(json!({
                "type": "object",
                "properties": {
                    "outer": {
                        "type": "object",
                        "description": "Outer",
                        "properties": { "inner": { "type": "string", "description": "Inner" } }
                    }
                }
            })),
            None
        ),
        "{\n  // Outer\n  outer?: {\n    // Inner\n    inner?: string;\n  };\n}"
    );
}

// https://github.com/earendil-works/pi/issues/10707
#[test]
fn puts_constraints_the_type_cannot_express_on_property_comment_lines() {
    let schema = schema_obj(json!({
        "type": "object",
        "properties": {
            "timeoutMs": {
                "type": "integer",
                "minimum": 0,
                "maximum": 30000,
                "default": 0,
                "description": "Wait in milliseconds."
            },
            "id": { "type": "string", "pattern": "^[a-z]+$", "maxLength": 8 },
            "tags": { "type": "array", "items": { "type": "string" }, "maxItems": 5, "uniqueItems": true, "deprecated": true },
            "name": { "type": "string" }
        },
        "required": ["id"]
    }));
    assert_eq!(
        schema_to_type(&schema, None),
        [
            "{",
            "  // @maxLength 8 @pattern ^[a-z]+$",
            "  id: string;",
            "  name?: string;",
            "  // @maxItems 5 @uniqueItems @deprecated",
            "  tags?: Array<string>;",
            "  // Wait in milliseconds.",
            "  // @integer @minimum 0 @maximum 30000 @default 0",
            "  timeoutMs?: number;",
            "}",
        ]
        .join("\n")
    );
}

#[test]
fn reads_constraints_of_optional_fields_from_the_non_null_branch() {
    let limit = schema_obj(json!({
        "type": "object",
        "properties": {
            "limit": {
                "anyOf": [{ "type": "integer", "minimum": 1, "maximum": 100 }, { "type": "null" }],
                "default": null,
                "title": "Limit"
            }
        }
    }));
    assert_eq!(
        schema_to_type(&limit, None),
        "{\n  // @integer @minimum 1 @maximum 100\n  limit?: number | null;\n}"
    );

    let n = schema_obj(json!({
        "type": "object",
        "properties": {
            "n": {
                "anyOf": [{ "type": "integer", "minimum": 10, "maximum": 50, "format": "int32" }, { "type": "null" }],
                "minimum": 0,
                "maximum": 20,
                "format": "port"
            }
        }
    }));
    assert_eq!(
        schema_to_type(&n, None),
        "{\n  // @integer @minimum 10 @maximum 20 @format port @format int32\n  n?: number | null;\n}"
    );
}

#[test]
fn reads_draft_04_boolean_exclusive_bounds() {
    let schema = schema_obj(json!({
        "type": "object",
        "properties": {
            "n": { "type": "integer", "minimum": 0, "exclusiveMinimum": true, "maximum": 10, "exclusiveMaximum": false },
            "m": { "anyOf": [{ "type": "number", "maximum": 5, "exclusiveMaximum": true }, { "type": "null" }], "maximum": 8 }
        }
    }));
    assert_eq!(
        schema_to_type(&schema, None),
        [
            "{",
            "  // @maximum 8 @exclusiveMaximum 5",
            "  m?: number | null;",
            "  // @integer @exclusiveMinimum 0 @maximum 10",
            "  n?: number;",
            "}",
        ]
        .join("\n")
    );
}

#[test]
fn leaves_out_long_tags_and_escapes_line_terminators() {
    let long_default = "x".repeat(100);
    let long_schema = schema_obj(json!({
        "type": "object",
        "properties": { "config": { "type": "object", "default": { "text": long_default } } }
    }));
    assert_eq!(
        schema_to_type(&long_schema, None),
        "{ config?: { [key: string]: unknown; }; }"
    );

    let term_schema = schema_obj(json!({
        "type": "object",
        "properties": {
            "sep": {
                "type": "string",
                "pattern": "^[\r\n\u{2028}\u{2029}]$",
                "default": "\u{2028}",
                "description": "a\u{2028}b"
            }
        }
    }));
    assert_eq!(
        schema_to_type(&term_schema, None),
        [
            "{",
            "  // a",
            "  // b",
            r#"  // @pattern ^[\u000d\u000a\u2028\u2029]$ @default "\u2028""#,
            "  sep?: string;",
            "}",
        ]
        .join("\n")
    );
}

#[test]
fn drops_constraint_tags_before_rendering_over_budget_as_unknown() {
    let schema = schema_obj(json!({
        "type": "object",
        "properties": { "n": { "type": "integer", "minimum": 1 } }
    }));
    assert_eq!(
        schema_to_type(&schema, None),
        "{\n  // @integer @minimum 1\n  n?: number;\n}"
    );
    assert_eq!(
        schema_to_type(&schema, Some(SchemaToTypeOptions { max_chars: Some(20) })),
        "{ n?: number; }"
    );
    assert_eq!(
        schema_to_type(&schema, Some(SchemaToTypeOptions { max_chars: Some(10) })),
        "unknown"
    );
}

#[test]
fn schema_to_type_resolves_refs_arrays_and_truncates_over_budget() {
    let schema = schema_obj(json!({
        "type": "object",
        "properties": {
            "item": { "$ref": "#/$defs/Item" },
            "legacy": { "$ref": "#/definitions/Legacy" },
            "remote": { "$ref": "https://example.com/schema.json" }
        },
        "required": ["item"],
        "$defs": {
            "Item": {
                "type": "object",
                "properties": { "id": { "type": "string" }, "parent": { "$ref": "#/$defs/Item" } },
                "required": ["id"]
            }
        },
        "definitions": { "Legacy": { "enum": ["a", "b"] } }
    }));
    assert_eq!(
        schema_to_type(&schema, None),
        "{ item: { id: string; parent?: unknown; }; legacy?: \"a\" | \"b\"; remote?: unknown; }"
    );

    assert_eq!(
        schema_to_type(
            &schema_obj(json!({ "type": "array", "items": { "type": "string" } })),
            None
        ),
        "Array<string>"
    );
    assert_eq!(
        schema_to_type(
            &schema_obj(json!({ "type": "array", "prefixItems": [{ "type": "string" }, { "type": "number" }] })),
            None
        ),
        "[string, number]"
    );
    assert_eq!(
        schema_to_type(&schema_obj(json!({ "type": "array" })), None),
        "unknown[]"
    );

    let mut props = serde_json::Map::new();
    for i in 0..50 {
        props.insert(format!("field{i}"), json!({ "type": "string" }));
    }
    let big = schema_obj(json!({ "type": "object", "properties": Value::Object(props) }));
    assert_eq!(
        schema_to_type(&big, Some(SchemaToTypeOptions { max_chars: Some(100) })),
        "unknown"
    );
    assert!(schema_to_type(&big, None).contains("field49?: string;"));
}

// #11803
#[test]
fn resolve_schema_ref_handles_escapes_and_malformed_percent_sequences() {
    let item = json!({ "type": "string" });
    let root = json!({
        "$defs": {
            "item": item,
            "a/b": item,
            "c~d": item,
            "e f": item
        }
    });
    let expected_item = schema_obj(item.clone());
    assert_eq!(resolve_schema_ref("#", &root), Some(schema_obj(root.clone())));
    assert_eq!(resolve_schema_ref("#/$defs/item", &root), Some(expected_item.clone()));
    assert_eq!(resolve_schema_ref("#/$defs/a~1b", &root), Some(expected_item.clone()));
    assert_eq!(resolve_schema_ref("#/$defs/c~0d", &root), Some(expected_item.clone()));
    assert_eq!(resolve_schema_ref("#/$defs/e%20f", &root), Some(expected_item));

    assert_eq!(resolve_schema_ref("other.json#/$defs/item", &root), None);
    assert_eq!(resolve_schema_ref("#/$defs/missing", &root), None);
    assert_eq!(resolve_schema_ref("#/$defs/%E0%A4%A", &root), None);
}

#[test]
fn tool_declarations_render_signatures_plain_types_and_sample() {
    let mut tool = noop_tool("hidden-dynamic-tool");
    tool.input_schema = Some(schema_obj(json!({
        "type": "object",
        "properties": { "city": { "type": "string" } },
        "required": ["city"],
        "additionalProperties": false
    })));
    tool.output_schema = Some(schema_obj(json!({
        "type": "object",
        "properties": { "ok": { "type": "boolean" } },
        "required": ["ok"]
    })));
    assert_eq!(
        render_tool_signature(&tool, None),
        "hidden_dynamic_tool(args: { city: string; }): Promise<{ ok: boolean; }>;"
    );
    assert_eq!(
        render_tool_signature(&noop_tool("free"), None),
        "free(args: unknown): Promise<unknown>;"
    );

    let mut plain = noop_tool("plain");
    plain.input_schema = Some(schema_obj(json!({ "type": "object", "properties": {} })));
    plain.output_schema = Some(schema_obj(json!({
        "type": "object",
        "properties": {
            "content": { "type": "array", "items": { "type": "object" } },
            "isError": { "type": "boolean" },
            "_meta": { "type": "object" }
        },
        "required": ["content"]
    })));
    assert_eq!(
        render_tool_signature(&plain, None),
        "plain(args: { [key: string]: unknown; }): Promise<{ _meta?: { [key: string]: unknown; }; content: Array<{ [key: string]: unknown; }>; isError?: boolean; }>;"
    );

    let mut sample_tool = noop_tool("foo");
    sample_tool.description = Some("bar".to_string());
    sample_tool.input_schema = Some(schema_obj(json!({ "type": "string" })));
    assert_eq!(
        render_tool_sample(&sample_tool, None),
        "bar\n\ncodemode tool declaration:\n```ts\ndeclare const tools: { foo(args: string): Promise<unknown>; };\n```"
    );
}

#[test]
fn render_declarations_renders_tools_globals_namespaces_and_escapes() {
    let mut read = noop_tool("read");
    read.description = Some("Read a file.\nSecond line.".to_string());
    read.input_schema = Some(schema_obj(json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path"]
    })));
    read.output_schema = Some(schema_obj(json!({ "type": "string" })));

    let mut attach = noop_tool("attach");
    attach.description = Some("Attach it.".to_string());
    attach.input_schema = Some(schema_obj(json!({ "type": "string" })));

    let text = render_declarations(RenderDeclarationsOptions {
        tools: Some(vec![read, noop_tool("remote-api")]),
        globals: Some(vec![attach]),
    });
    assert_eq!(
        text,
        [
            "declare const tools: {",
            "  /**",
            "   * Read a file.",
            "   * Second line.",
            "   */",
            "  read(args: { path: string; }): Promise<string>;",
            "  remote_api(args: unknown): Promise<unknown>;",
            "};",
            "",
            "/** Attach it. */",
            "declare function attach(args: string): Promise<unknown>;",
        ]
        .join("\n")
    );

    let mut models_list = noop_tool("models.list");
    models_list.description = Some("List models.".to_string());
    models_list.signature = Some("(type: string): Promise<string[]>".to_string());
    let mut models_get = noop_tool("models.get");
    models_get.input_schema = Some(schema_obj(json!({ "type": "string" })));
    let mut plain = noop_tool("plain");
    plain.signature = Some("(): void".to_string());

    let ns_text = render_declarations(RenderDeclarationsOptions {
        tools: None,
        globals: Some(vec![models_list, models_get, plain]),
    });
    assert_eq!(
        ns_text,
        [
            "declare function plain(): void;",
            "",
            "declare const models: {",
            "  /** List models. */",
            "  list(type: string): Promise<string[]>;",
            "  get(args: string): Promise<unknown>;",
            "};",
        ]
        .join("\n")
    );

    let mut comment_tool = noop_tool("x");
    comment_tool.description = Some("a */ b".to_string());
    let escaped = render_declarations(RenderDeclarationsOptions {
        tools: Some(vec![comment_tool]),
        globals: None,
    });
    assert!(escaped.contains("/** a *\\/ b */"));
}
