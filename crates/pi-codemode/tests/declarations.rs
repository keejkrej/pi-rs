//! Port of packages/codemode/test/declarations.test.ts

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::{Value, json};

use pi_codemode::{
    CodemodeJsonSchema, CodemodeTool, RenderDeclarationsOptions, SchemaToTypeOptions, mcp_structured_content_schema,
    render_declarations, render_tool_sample, render_tool_signature, schema_to_type, to_codemode_identifier,
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

fn mcp_result_schema(structured_content: Option<Value>) -> CodemodeJsonSchema {
    let mut props = serde_json::Map::new();
    props.insert(
        "content".into(),
        json!({ "type": "array", "items": { "type": "object" } }),
    );
    if let Some(sc) = structured_content {
        props.insert("structuredContent".into(), sc);
    }
    props.insert("isError".into(), json!({ "type": "boolean" }));
    props.insert("_meta".into(), json!({ "type": "object" }));
    schema_obj(json!({
        "type": "object",
        "properties": Value::Object(props),
        "required": ["content"],
    }))
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
        schema_to_type(
            &big,
            Some(SchemaToTypeOptions {
                max_chars: Some(100),
                ..Default::default()
            })
        ),
        "unknown"
    );
    assert!(schema_to_type(&big, None).contains("field49?: string;"));
}

#[test]
fn tool_declarations_render_signatures_mcp_and_sample() {
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

    let empty_input = schema_obj(json!({ "type": "object", "properties": {}, "additionalProperties": false }));
    let mut mcp = noop_tool("mcp__sample__search");
    mcp.input_schema = Some(empty_input.clone());
    mcp.output_schema = Some(mcp_result_schema(Some(json!({
        "type": "object",
        "properties": { "results": { "type": "array", "items": { "$ref": "#/definitions/Result~1item~0v1" } } },
        "required": ["results"],
        "additionalProperties": false,
        "definitions": {
            "Result/item~v1": {
                "type": "object",
                "properties": { "id": { "type": "string" }, "score": { "type": "number" } },
                "required": ["id", "score"],
                "additionalProperties": false
            }
        }
    }))));
    assert_eq!(
        render_tool_signature(&mcp, None),
        "mcp__sample__search(args: {}): Promise<CallToolResult<{ results: Array<{ id: string; score: number; }>; }>>;"
    );

    let mut plain_mcp = noop_tool("plain");
    plain_mcp.input_schema = Some(empty_input);
    plain_mcp.output_schema = Some(mcp_result_schema(None));
    assert_eq!(
        render_tool_signature(&plain_mcp, None),
        "plain(args: {}): Promise<CallToolResult>;"
    );

    assert!(
        mcp_structured_content_schema(Some(&schema_obj(json!({
            "type": "object",
            "properties": { "content": { "type": "array" } }
        }))))
        .is_none()
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
