//! Port of packages/ai/test/validation-probes.test.ts

use pi_ai::types::{JsonObject, Tool, ToolCall};
use pi_ai::utils::validation::validate_tool_arguments;
use pi_js::vendor::typebox::{Schema, t};
use serde_json::{Value, json};

fn to_args(val: Value) -> JsonObject {
    match val {
        Value::Object(map) => map.into_iter().collect(),
        _ => JsonObject::new(),
    }
}

fn outcome(name: &str, schema: Schema, args: Value) -> String {
    let tool = Tool {
        name: name.to_owned(),
        description: String::new(),
        parameters: schema,
        constrained_sampling: None,
    };
    let call = ToolCall::new("1", name, to_args(args));
    match validate_tool_arguments(&tool, &call) {
        Ok(coerced) => format!("ok {}", pi_js::json::stringify(&coerced)),
        Err(err) => {
            let msg = err.to_string();
            if msg.starts_with("Validation failed") {
                let first_line = msg.lines().nth(1).unwrap_or("").trim();
                format!("REJECT {first_line}")
            } else {
                let first_line = msg.lines().next().unwrap_or("");
                format!("THROW {first_line}")
            }
        }
    }
}

#[test]
fn reviewer_probes_plain_json_and_typebox_round_1() {
    let mut cases: Vec<(&str, Value, Value, &str)> = vec![
        // ref-with-$id
        (
            "ref-with-$id",
            json!({ "type": "object", "properties": { "a": { "$ref": "A" } } }),
            json!({ "a": { "x": "1" } }),
            "REJECT - a: unresolved reference A",
        ),
        // cyclic
        (
            "cyclic",
            json!({
                "type": "object",
                "properties": { "tree": { "$ref": "#/$defs/Node" } },
                "$defs": {
                    "Node": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "children": { "type": "array", "items": { "$ref": "#/$defs/Node" } }
                        },
                        "required": ["name", "children"]
                    }
                },
                "required": ["tree"]
            }),
            json!({ "tree": { "name": "a", "children": [{ "name": "b", "children": [] }] } }),
            r#"ok {"tree":{"name":"a","children":[{"name":"b","children":[]}]}}"#,
        ),
        (
            "cyclic",
            json!({
                "type": "object",
                "properties": { "tree": { "$ref": "#/$defs/Node" } },
                "$defs": {
                    "Node": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "children": { "type": "array", "items": { "$ref": "#/$defs/Node" } }
                        },
                        "required": ["name", "children"]
                    }
                },
                "required": ["tree"]
            }),
            json!({ "tree": { "name": 1, "children": [] } }),
            r#"ok {"tree":{"name":"1","children":[]}}"#,
        ),
        // record-string & record-literal-keys
        (
            "record-string",
            json!({ "type": "object", "properties": { "r": { "type": "object", "additionalProperties": { "type": "number" } } }, "required": ["r"] }),
            json!({ "r": { "a": "1" } }),
            r#"ok {"r":{"a":1}}"#,
        ),
        (
            "record-string",
            json!({ "type": "object", "properties": { "r": { "type": "object", "additionalProperties": { "type": "number" } } }, "required": ["r"] }),
            json!({ "r": { "a": 1, "b": "x" } }),
            "REJECT - r.b: expected number, got string",
        ),
        (
            "record-literal-keys",
            json!({ "type": "object", "properties": { "r": { "type": "object", "properties": { "a": { "type": "number" }, "b": { "type": "number" } }, "required": ["a", "b"] } }, "required": ["r"] }),
            json!({ "r": { "a": 1, "b": 2 } }),
            r#"ok {"r":{"a":1,"b":2}}"#,
        ),
        (
            "record-literal-keys",
            json!({ "type": "object", "properties": { "r": { "type": "object", "properties": { "a": { "type": "number" }, "b": { "type": "number" } }, "required": ["a", "b"] } }, "required": ["r"] }),
            json!({ "r": { "a": 1 } }),
            "REJECT - r.b: missing",
        ),
        (
            "record-literal-keys",
            json!({ "type": "object", "properties": { "r": { "type": "object", "properties": { "a": { "type": "number" }, "b": { "type": "number" } }, "required": ["a", "b"] } }, "required": ["r"] }),
            json!({ "r": { "a": 1, "b": 2, "c": 3 } }),
            r#"ok {"r":{"a":1,"b":2,"c":3}}"#,
        ),
        // intersect
        (
            "intersect",
            json!({ "allOf": [
                { "type": "object", "properties": { "a": { "type": "number" } }, "required": ["a"] },
                { "type": "object", "properties": { "b": { "type": "boolean" } }, "required": ["b"] }
            ] }),
            json!({ "a": "1", "b": "true" }),
            r#"ok {"a":1,"b":true}"#,
        ),
        (
            "intersect",
            json!({ "allOf": [
                { "type": "object", "properties": { "a": { "type": "number" } }, "required": ["a"] },
                { "type": "object", "properties": { "b": { "type": "boolean" } }, "required": ["b"] }
            ] }),
            json!({ "a": 1, "b": true, "c": 1 }),
            r#"ok {"a":1,"b":true,"c":1}"#,
        ),
        // union-objects
        (
            "union-objects",
            json!({
                "type": "object",
                "properties": {
                    "v": {
                        "anyOf": [
                            { "type": "object", "properties": { "k": { "const": "x" }, "n": { "type": "number" } }, "required": ["k", "n"] },
                            { "type": "object", "properties": { "k": { "const": "y" }, "s": { "type": "string" } }, "required": ["k", "s"] }
                        ]
                    }
                },
                "required": ["v"]
            }),
            json!({ "v": { "k": "x", "n": "2" } }),
            r#"ok {"v":{"k":"x","n":2}}"#,
        ),
        (
            "union-objects",
            json!({
                "type": "object",
                "properties": {
                    "v": {
                        "anyOf": [
                            { "type": "object", "properties": { "k": { "const": "x" }, "n": { "type": "number" } }, "required": ["k", "n"] },
                            { "type": "object", "properties": { "k": { "const": "y" }, "s": { "type": "string" } }, "required": ["k", "s"] }
                        ]
                    }
                },
                "required": ["v"]
            }),
            json!({ "v": { "k": "y", "s": 2 } }),
            r#"ok {"v":{"k":"y","s":"2"}}"#,
        ),
        // length & pattern
        (
            "minLength-astral",
            json!({ "type": "object", "properties": { "s": { "type": "string", "minLength": 2 } }, "required": ["s"] }),
            json!({ "s": "😀" }),
            "REJECT - s: expected at least 2 characters",
        ),
        (
            "maxLength-astral",
            json!({ "type": "object", "properties": { "s": { "type": "string", "maxLength": 1 } }, "required": ["s"] }),
            json!({ "s": "😀" }),
            r#"ok {"s":"😀"}"#,
        ),
        (
            "pattern-astral",
            json!({ "type": "object", "properties": { "s": { "type": "string", "pattern": "^.$" } }, "required": ["s"] }),
            json!({ "s": "😀" }),
            r#"ok {"s":"😀"}"#,
        ),
        (
            "pattern-class-dash",
            json!({ "type": "object", "properties": { "s": { "type": "string", "pattern": "^[\\w-]+$" } }, "required": ["s"] }),
            json!({ "s": "a-b" }),
            r#"ok {"s":"a-b"}"#,
        ),
        (
            "pattern-unicode-prop",
            json!({ "type": "object", "properties": { "s": { "type": "string", "pattern": "^\\p{L}+$" } }, "required": ["s"] }),
            json!({ "s": "äb" }),
            r#"ok {"s":"äb"}"#,
        ),
        (
            "pattern-unicode-prop",
            json!({ "type": "object", "properties": { "s": { "type": "string", "pattern": "^\\p{L}+$" } }, "required": ["s"] }),
            json!({ "s": "p{L}" }),
            "REJECT - s: expected to match ^\\p{L}+$",
        ),
        // multipleOf-float
        (
            "multipleOf-float",
            json!({ "type": "object", "properties": { "n": { "type": "number", "multipleOf": 0.01 } }, "required": ["n"] }),
            json!({ "n": 0.07 }),
            r#"ok {"n":0.07}"#,
        ),
        (
            "multipleOf-float",
            json!({ "type": "object", "properties": { "n": { "type": "number", "multipleOf": 0.01 } }, "required": ["n"] }),
            json!({ "n": 1.1 }),
            r#"ok {"n":1.1}"#,
        ),
        // uniqueItems-keyorder
        (
            "uniqueItems-keyorder",
            json!({ "type": "object", "properties": { "a": { "type": "array", "uniqueItems": true } }, "required": ["a"] }),
            json!({ "a": [{ "x": 1, "y": 2 }, { "y": 2, "x": 1 }] }),
            "REJECT - a: expected unique items",
        ),
        (
            "uniqueItems-keyorder",
            json!({ "type": "object", "properties": { "a": { "type": "array", "uniqueItems": true } }, "required": ["a"] }),
            json!({ "a": [1, 1.0] }),
            "REJECT - a: expected unique items",
        ),
        // formats
        (
            "formats",
            json!({
                "type": "object",
                "properties": {
                    "e": { "type": "string", "format": "email" },
                    "u": { "type": "string", "format": "uri" },
                    "d": { "type": "string", "format": "date-time" },
                    "x": { "type": "string", "format": "made-up" },
                    "h": { "type": "string", "format": "hostname" },
                    "r": { "type": "string", "format": "regex" },
                    "i": { "type": "string", "format": "uuid" },
                    "ip": { "type": "string", "format": "ipv4" }
                }
            }),
            json!({ "e": "a@b.co" }),
            r#"ok {"e":"a@b.co"}"#,
        ),
        (
            "formats",
            json!({ "type": "object", "properties": { "e": { "type": "string", "format": "email" } } }),
            json!({ "e": "not-an-email" }),
            "REJECT - e: expected format email",
        ),
        (
            "formats",
            json!({ "type": "object", "properties": { "u": { "type": "string", "format": "uri" } } }),
            json!({ "u": "relative/path" }),
            "REJECT - u: expected format uri",
        ),
        (
            "formats",
            json!({ "type": "object", "properties": { "u": { "type": "string", "format": "uri" } } }),
            json!({ "u": "https://x.y/z" }),
            r#"ok {"u":"https://x.y/z"}"#,
        ),
        (
            "formats",
            json!({ "type": "object", "properties": { "d": { "type": "string", "format": "date-time" } } }),
            json!({ "d": "2024-01-01T00:00:00Z" }),
            r#"ok {"d":"2024-01-01T00:00:00Z"}"#,
        ),
        (
            "formats",
            json!({ "type": "object", "properties": { "d": { "type": "string", "format": "date-time" } } }),
            json!({ "d": "2024-01-01" }),
            "REJECT - d: expected format date-time",
        ),
        (
            "formats",
            json!({ "type": "object", "properties": { "x": { "type": "string", "format": "made-up" } } }),
            json!({ "x": "anything" }),
            r#"ok {"x":"anything"}"#,
        ),
        (
            "formats",
            json!({ "type": "object", "properties": { "h": { "type": "string", "format": "hostname" } } }),
            json!({ "h": "under_score.example" }),
            "REJECT - h: expected format hostname",
        ),
        (
            "formats",
            json!({ "type": "object", "properties": { "r": { "type": "string", "format": "regex" } } }),
            json!({ "r": "[" }),
            "REJECT - r: expected format regex",
        ),
        (
            "formats",
            json!({ "type": "object", "properties": { "i": { "type": "string", "format": "uuid" } } }),
            json!({ "i": "00000000-0000-0000-0000-000000000000" }),
            r#"ok {"i":"00000000-0000-0000-0000-000000000000"}"#,
        ),
        (
            "formats",
            json!({ "type": "object", "properties": { "i": { "type": "string", "format": "uuid" } } }),
            json!({ "i": "not" }),
            "REJECT - i: expected format uuid",
        ),
        (
            "formats",
            json!({ "type": "object", "properties": { "ip": { "type": "string", "format": "ipv4" } } }),
            json!({ "ip": "999.1.1.1" }),
            "REJECT - ip: expected format ipv4",
        ),
        // integer-big & additionalProperties
        (
            "integer-big",
            json!({ "type": "object", "properties": { "n": { "type": "integer" } }, "required": ["n"] }),
            json!({ "n": 1e21 }),
            r#"ok {"n":1e+21}"#,
        ),
        (
            "integer-big",
            json!({ "type": "object", "properties": { "n": { "type": "integer" } }, "required": ["n"] }),
            json!({ "n": 9007199254740994i64 }),
            r#"ok {"n":9007199254740994}"#,
        ),
        (
            "additional-false",
            json!({ "type": "object", "properties": { "a": { "type": "string" } }, "required": ["a"], "additionalProperties": false }),
            json!({ "a": "x", "b": 1 }),
            "REJECT - b: unknown field",
        ),
        (
            "proto-key-value",
            json!({ "type": "object", "properties": {}, "additionalProperties": { "type": "array", "items": { "type": "string" } } }),
            json!({ "__proto__": "x", "a": "y" }),
            r#"ok {"__proto__":["x"],"a":["y"]}"#,
        ),
        // MCP plain JSON probes
        (
            "mcp-draft7-ref-definitions",
            json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": { "t": { "$ref": "#/definitions/T" } },
                "required": ["t"],
                "definitions": { "T": { "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] } }
            }),
            json!({ "t": { "path": "a" } }),
            r#"ok {"t":{"path":"a"}}"#,
        ),
        (
            "mcp-draft7-ref-definitions",
            json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": { "t": { "$ref": "#/definitions/T" } },
                "required": ["t"],
                "definitions": { "T": { "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] } }
            }),
            json!({ "t": { "path": 42 } }),
            r#"ok {"t":{"path":"42"}}"#,
        ),
        (
            "mcp-ref-unresolved",
            json!({ "type": "object", "properties": { "t": { "$ref": "#/definitions/Missing" } } }),
            json!({ "t": 1 }),
            "REJECT - t: unresolved reference #/definitions/Missing",
        ),
        (
            "mcp-ref-unresolved",
            json!({ "type": "object", "properties": { "t": { "$ref": "#/definitions/Missing" } } }),
            json!({}),
            "ok {}",
        ),
        (
            "mcp-ref-external",
            json!({ "type": "object", "properties": { "t": { "$ref": "https://example.com/schemas/t.json" } } }),
            json!({ "t": 1 }),
            "REJECT - t: unresolved reference https://example.com/schemas/t.json",
        ),
        (
            "mcp-nullable-openapi",
            json!({ "type": "object", "properties": { "s": { "type": "string", "nullable": true } }, "required": ["s"] }),
            json!({ "s": null }),
            r#"ok {"s":"null"}"#,
        ),
        (
            "mcp-nullable-openapi",
            json!({ "type": "object", "properties": { "s": { "type": "string", "nullable": true } }, "required": ["s"] }),
            json!({ "s": "x" }),
            r#"ok {"s":"x"}"#,
        ),
        (
            "mcp-draft7-tuple",
            json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": { "p": { "type": "array", "items": [{ "type": "number" }, { "type": "string" }], "additionalItems": false } }
            }),
            json!({ "p": [1, "a"] }),
            r#"ok {"p":[1,"a"]}"#,
        ),
        (
            "mcp-draft7-tuple",
            json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": { "p": { "type": "array", "items": [{ "type": "number" }, { "type": "string" }], "additionalItems": false } }
            }),
            json!({ "p": [1, "a", 3] }),
            "REJECT - p.2: no value is allowed here",
        ),
        (
            "mcp-draft7-tuple",
            json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": { "p": { "type": "array", "items": [{ "type": "number" }, { "type": "string" }], "additionalItems": false } }
            }),
            json!({ "p": ["1", 2] }),
            r#"ok {"p":[1,"2"]}"#,
        ),
        (
            // PORT: `crates/pi-schema` follows JSON Schema Draft-07 `$ref` sibling-ignoring semantics when
            // `$schema` is draft-07, so sibling `maxLength` is ignored alongside `$ref`.
            "mcp-draft7-ref-sibling",
            json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": { "s": { "$ref": "#/definitions/S", "maxLength": 1 } },
                "definitions": { "S": { "type": "string" } }
            }),
            json!({ "s": "abc" }),
            r#"ok {"s":"abc"}"#,
        ),
        (
            "mcp-draft4-exclusive-bool",
            json!({
                "$schema": "http://json-schema.org/draft-04/schema#",
                "type": "object",
                "properties": { "n": { "type": "number", "minimum": 0, "exclusiveMinimum": true } }
            }),
            json!({ "n": 0 }),
            r#"ok {"n":0}"#,
        ),
        (
            "mcp-draft4-exclusive-bool",
            json!({
                "$schema": "http://json-schema.org/draft-04/schema#",
                "type": "object",
                "properties": { "n": { "type": "number", "minimum": 0, "exclusiveMinimum": true } }
            }),
            json!({ "n": 1 }),
            r#"ok {"n":1}"#,
        ),
        (
            "mcp-id-draft4",
            json!({
                "$schema": "http://json-schema.org/draft-04/schema#",
                "id": "http://x/y",
                "type": "object",
                "properties": { "a": { "type": "string" } }
            }),
            json!({ "a": "x" }),
            r#"ok {"a":"x"}"#,
        ),
        (
            "mcp-bad-pattern",
            json!({ "type": "object", "properties": { "s": { "type": "string", "pattern": "(?<!x)y" } } }),
            json!({ "s": "y" }),
            r#"ok {"s":"y"}"#,
        ),
        (
            "mcp-type-integer-float-0",
            json!({ "type": "object", "properties": { "n": { "type": "integer" } } }),
            json!({ "n": 1.0 }),
            r#"ok {"n":1}"#,
        ),
        (
            "mcp-type-integer-float-0",
            json!({ "type": "object", "properties": { "n": { "type": "integer" } } }),
            json!({ "n": 1.5 }),
            r#"ok {"n":1}"#,
        ),
        (
            "mcp-format-unknown",
            json!({ "type": "object", "properties": { "s": { "type": "string", "format": "binary" } } }),
            json!({ "s": "xx" }),
            r#"ok {"s":"xx"}"#,
        ),
        (
            "mcp-required-null-string",
            json!({ "type": "object", "properties": { "q": { "type": "string" } }, "required": ["q"] }),
            json!({ "q": null }),
            r#"ok {"q":"null"}"#,
        ),
        (
            "mcp-array-json-string",
            json!({ "type": "object", "properties": { "p": { "type": "array", "items": { "type": "string" } } }, "required": ["p"] }),
            json!({ "p": "[\"a\",\"b\"]" }),
            r#"ok {"p":["[\"a\",\"b\"]"]}"#,
        ),
        (
            "mcp-integer-float",
            json!({ "type": "object", "properties": { "n": { "type": "integer" } }, "required": ["n"] }),
            json!({ "n": 3.7 }),
            r#"ok {"n":3}"#,
        ),
        (
            "mcp-integer-float",
            json!({ "type": "object", "properties": { "n": { "type": "integer" } }, "required": ["n"] }),
            json!({ "n": "3.7" }),
            r#"ok {"n":3}"#,
        ),
        ("mcp-empty-schema", json!({}), json!({ "a": 1 }), r#"ok {"a":1}"#),
        (
            "mcp-type-object-no-props",
            json!({ "type": "object" }),
            json!({ "a": 1 }),
            r#"ok {"a":1}"#,
        ),
        (
            "mcp-boolean-true-prop",
            json!({ "type": "object", "properties": { "a": true, "b": false } }),
            json!({ "a": 1 }),
            r#"ok {"a":1}"#,
        ),
        (
            "mcp-boolean-true-prop",
            json!({ "type": "object", "properties": { "a": true, "b": false } }),
            json!({ "b": 1 }),
            "REJECT - b: no value is allowed here",
        ),
        // Keyword cases
        (
            "oneOf-overlap",
            json!({ "type": "object", "properties": { "v": { "oneOf": [{ "type": "string" }, { "type": "string", "maxLength": 5 }] } } }),
            json!({ "v": "abc" }),
            "REJECT - v: matches more than one alternative",
        ),
        (
            "oneOf-overlap",
            json!({ "type": "object", "properties": { "v": { "oneOf": [{ "type": "string" }, { "type": "string", "maxLength": 5 }] } } }),
            json!({ "v": "abcdefgh" }),
            r#"ok {"v":"abcdefgh"}"#,
        ),
        (
            "oneOf-number-integer",
            json!({ "type": "object", "properties": { "v": { "oneOf": [{ "type": "number" }, { "type": "integer" }] } } }),
            json!({ "v": 2 }),
            "REJECT - v: matches more than one alternative",
        ),
        (
            "oneOf-number-integer",
            json!({ "type": "object", "properties": { "v": { "oneOf": [{ "type": "number" }, { "type": "integer" }] } } }),
            json!({ "v": 2.5 }),
            r#"ok {"v":2.5}"#,
        ),
        (
            "oneOf-objects-open",
            json!({
                "type": "object",
                "properties": {
                    "v": {
                        "oneOf": [
                            { "type": "object", "properties": { "a": { "type": "string" } } },
                            { "type": "object", "properties": { "b": { "type": "string" } } }
                        ]
                    }
                }
            }),
            json!({ "v": { "a": "x" } }),
            "REJECT - v: matches more than one alternative",
        ),
        (
            "if-then-else",
            json!({
                "type": "object",
                "properties": { "kind": { "type": "string" }, "n": { "type": "number" } },
                "if": { "properties": { "kind": { "const": "a" } } },
                "then": { "required": ["n"] }
            }),
            json!({ "kind": "a" }),
            "REJECT - n: missing",
        ),
        (
            "if-then-else",
            json!({
                "type": "object",
                "properties": { "kind": { "type": "string" }, "n": { "type": "number" } },
                "if": { "properties": { "kind": { "const": "a" } } },
                "then": { "required": ["n"] }
            }),
            json!({ "kind": "b" }),
            r#"ok {"kind":"b"}"#,
        ),
        (
            "dependentRequired",
            json!({
                "type": "object",
                "properties": { "a": { "type": "string" }, "b": { "type": "string" } },
                "dependentRequired": { "a": ["b"] }
            }),
            json!({ "a": "x" }),
            "REJECT - b: required when a is present",
        ),
        (
            "dependencies-d7",
            json!({
                "type": "object",
                "properties": { "a": { "type": "string" }, "b": { "type": "string" } },
                "dependencies": { "a": ["b"] }
            }),
            json!({ "a": "x" }),
            "REJECT - b: required when a is present",
        ),
        (
            "propertyNames",
            json!({ "type": "object", "properties": {}, "propertyNames": { "pattern": "^[a-z]+$" } }),
            json!({ "abc": 1 }),
            r#"ok {"abc":1}"#,
        ),
        (
            "propertyNames",
            json!({ "type": "object", "properties": {}, "propertyNames": { "pattern": "^[a-z]+$" } }),
            json!({ "ABC": 1 }),
            "REJECT - ABC: invalid property name",
        ),
        (
            "contains",
            json!({ "type": "object", "properties": { "l": { "type": "array", "contains": { "type": "number" } } } }),
            json!({ "l": ["a"] }),
            "REJECT - l: expected at least 1 matching item",
        ),
        (
            "contains",
            json!({ "type": "object", "properties": { "l": { "type": "array", "contains": { "type": "number" } } } }),
            json!({ "l": ["a", 1] }),
            r#"ok {"l":["a",1]}"#,
        ),
        (
            "not",
            json!({ "type": "object", "properties": { "s": { "not": { "type": "null" } } } }),
            json!({ "s": "x" }),
            r#"ok {"s":"x"}"#,
        ),
        (
            "not",
            json!({ "type": "object", "properties": { "s": { "not": { "type": "null" } } } }),
            json!({ "s": null }),
            "ok {}",
        ),
        (
            "minProperties",
            json!({ "type": "object", "properties": { "m": { "type": "object", "minProperties": 1 } } }),
            json!({ "m": {} }),
            "REJECT - m: expected at least 1 properties",
        ),
        (
            "unevaluatedProperties",
            json!({ "type": "object", "properties": { "a": { "type": "string" } }, "unevaluatedProperties": false }),
            json!({ "a": "x" }),
            r#"ok {"a":"x"}"#,
        ),
        (
            "unevaluatedProperties",
            json!({ "type": "object", "properties": { "a": { "type": "string" } }, "unevaluatedProperties": false }),
            json!({ "a": "x", "b": 1 }),
            "REJECT - b: unknown field",
        ),
        (
            "allOf-additional-false",
            json!({ "allOf": [
                { "type": "object", "properties": { "a": { "type": "string" } }, "additionalProperties": false },
                { "type": "object", "properties": { "b": { "type": "string" } } }
            ] }),
            json!({ "a": "x" }),
            r#"ok {"a":"x"}"#,
        ),
        (
            "allOf-additional-false",
            json!({ "allOf": [
                { "type": "object", "properties": { "a": { "type": "string" } }, "additionalProperties": false },
                { "type": "object", "properties": { "b": { "type": "string" } } }
            ] }),
            json!({ "a": "x", "b": "y" }),
            "REJECT - b: unknown field",
        ),
        (
            "type-array-multi",
            json!({ "type": "object", "properties": { "v": { "type": ["string", "number"] } } }),
            json!({ "v": true }),
            r#"ok {"v":"true"}"#,
        ),
        (
            "type-array-multi",
            json!({ "type": "object", "properties": { "v": { "type": ["string", "number"] } } }),
            json!({ "v": null }),
            "ok {}",
        ),
        (
            "nullable-type-array",
            json!({ "type": "object", "properties": { "v": { "type": ["string", "null"] } }, "required": ["v"] }),
            json!({ "v": null }),
            r#"ok {"v":null}"#,
        ),
        (
            "nullable-type-array",
            json!({ "type": "object", "properties": { "v": { "type": ["string", "null"] } }, "required": ["v"] }),
            json!({ "v": 5 }),
            r#"ok {"v":"5"}"#,
        ),
        (
            "exclusiveMinimum-number",
            json!({ "type": "object", "properties": { "n": { "type": "number", "exclusiveMinimum": 0 } } }),
            json!({ "n": 0 }),
            "REJECT - n: expected > 0",
        ),
        (
            "maxItems",
            json!({ "type": "object", "properties": { "l": { "type": "array", "maxItems": 1 } } }),
            json!({ "l": [1, 2] }),
            "REJECT - l: expected at most 1 items",
        ),
        (
            "maxItems",
            json!({ "type": "object", "properties": { "l": { "type": "array", "maxItems": 1 } } }),
            json!({ "l": "x" }),
            r#"ok {"l":["x"]}"#,
        ),
        (
            "discriminator-openapi",
            json!({
                "type": "object",
                "properties": {
                    "p": {
                        "oneOf": [
                            { "type": "object", "properties": { "kind": { "type": "string", "enum": ["a"] }, "x": { "type": "number" } }, "required": ["kind"] },
                            { "type": "object", "properties": { "kind": { "type": "string", "enum": ["b"] } }, "required": ["kind"] }
                        ],
                        "discriminator": { "propertyName": "kind" }
                    }
                }
            }),
            json!({ "p": { "kind": "a", "x": "1" } }),
            r#"ok {"p":{"kind":"a","x":1}}"#,
        ),
        (
            "ref-root-defs",
            json!({ "$ref": "#/$defs/Root", "$defs": { "Root": { "type": "object", "properties": { "a": { "type": "string" } }, "required": ["a"] } } }),
            json!({ "a": "x" }),
            r#"ok {"a":"x"}"#,
        ),
        (
            "ref-root-defs",
            json!({ "$ref": "#/$defs/Root", "$defs": { "Root": { "type": "object", "properties": { "a": { "type": "string" } }, "required": ["a"] } } }),
            json!({}),
            "REJECT - a: missing",
        ),
        (
            "ref-recursive",
            json!({
                "$defs": { "N": { "type": "object", "properties": { "c": { "type": "array", "items": { "$ref": "#/$defs/N" } }, "v": { "type": "integer" } } } },
                "type": "object",
                "properties": { "root": { "$ref": "#/$defs/N" } }
            }),
            json!({ "root": { "v": "2", "c": [{ "v": "3", "c": [] }] } }),
            r#"ok {"root":{"v":2,"c":[{"v":3,"c":[]}]}}"#,
        ),
        (
            "ref-percent",
            json!({ "type": "object", "properties": { "a": { "$ref": "#/$defs/a%20b" } }, "$defs": { "a b": { "type": "string" } } }),
            json!({ "a": 1 }),
            r#"ok {"a":"1"}"#,
        ),
        (
            "ref-tilde",
            json!({ "type": "object", "properties": { "a": { "$ref": "#/$defs/a~1b" } }, "$defs": { "a/b": { "type": "string" } } }),
            json!({ "a": "x" }),
            r#"ok {"a":"x"}"#,
        ),
        (
            "defs-with-$id",
            json!({ "type": "object", "properties": { "a": { "$ref": "item.json" } }, "$defs": { "item": { "$id": "item.json", "type": "string" } } }),
            json!({ "a": 2 }),
            r#"ok {"a":"2"}"#,
        ),
        (
            "anyOf-null-default",
            json!({ "type": "object", "properties": { "v": { "anyOf": [{ "type": "string" }, { "type": "null" }], "default": null } } }),
            json!({ "v": null }),
            r#"ok {"v":null}"#,
        ),
        (
            "anyOf-null-default",
            json!({ "type": "object", "properties": { "v": { "anyOf": [{ "type": "string" }, { "type": "null" }], "default": null } } }),
            json!({}),
            "ok {}",
        ),
        (
            "format-date-time-lower",
            json!({ "type": "object", "properties": { "d": { "type": "string", "format": "date-time" } } }),
            json!({ "d": "2024-01-01t00:00:00z" }),
            r#"ok {"d":"2024-01-01t00:00:00z"}"#,
        ),
        (
            "format-date-time-lower",
            json!({ "type": "object", "properties": { "d": { "type": "string", "format": "date-time" } } }),
            json!({ "d": "2024-01-01 00:00:00Z" }),
            "REJECT - d: expected format date-time",
        ),
        (
            "format-url",
            json!({ "type": "object", "properties": { "u": { "type": "string", "format": "url" } } }),
            json!({ "u": "not a url" }),
            "REJECT - u: expected format url",
        ),
        (
            "integer-as-float-1e0",
            json!({ "type": "object", "properties": { "n": { "type": "integer" } } }),
            json!({ "n": 1000 }),
            r#"ok {"n":1000}"#,
        ),
        (
            "required-missing-props",
            json!({ "type": "object", "required": ["a"] }),
            json!({}),
            "REJECT - a: missing",
        ),
        (
            "required-missing-props",
            json!({ "type": "object", "required": ["a"] }),
            json!({ "a": null }),
            r#"ok {"a":null}"#,
        ),
        (
            "string-to-number-trim",
            json!({ "type": "object", "properties": { "n": { "type": "number" } } }),
            json!({ "n": " 12 " }),
            r#"ok {"n":12}"#,
        ),
        (
            "string-to-number-trim",
            json!({ "type": "object", "properties": { "n": { "type": "number" } } }),
            json!({ "n": "0x10" }),
            r#"ok {"n":16}"#,
        ),
        (
            "string-to-number-trim",
            json!({ "type": "object", "properties": { "n": { "type": "number" } } }),
            json!({ "n": "1e3" }),
            r#"ok {"n":1000}"#,
        ),
        (
            "string-to-number-trim",
            json!({ "type": "object", "properties": { "n": { "type": "number" } } }),
            json!({ "n": "" }),
            r#"ok {"n":0}"#,
        ),
    ];

    // Recursion cases
    let tree = json!({
        "type": "object",
        "properties": { "t": { "$ref": "#/$defs/Tree" } },
        "$defs": { "Tree": { "anyOf": [{ "type": "string" }, { "type": "array", "items": { "$ref": "#/$defs/Tree" } }] } }
    });
    let json_schema = json!({
        "type": "object",
        "properties": { "data": { "$ref": "#/$defs/Json" } },
        "required": ["data"],
        "$defs": {
            "Json": {
                "anyOf": [
                    { "type": "string" },
                    { "type": "number" },
                    { "type": "boolean" },
                    { "type": "array", "items": { "$ref": "#/$defs/Json" } },
                    { "type": "object", "additionalProperties": { "$ref": "#/$defs/Json" } }
                ]
            }
        }
    });
    let filter = json!({
        "type": "object",
        "properties": { "where": { "$ref": "#/$defs/F" } },
        "$defs": {
            "F": {
                "oneOf": [
                    {
                        "type": "object",
                        "properties": { "field": { "type": "string" }, "eq": { "type": "string" } },
                        "required": ["field", "eq"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "properties": { "and": { "type": "array", "items": { "$ref": "#/$defs/F" } } },
                        "required": ["and"],
                        "additionalProperties": false
                    }
                ]
            }
        }
    });
    cases.extend([
        (
            "tree valid",
            tree.clone(),
            json!({ "t": ["a", ["b"]] }),
            r#"ok {"t":["a",["b"]]}"#,
        ),
        ("tree number", tree.clone(), json!({ "t": 5 }), r#"ok {"t":"5"}"#),
        ("tree [5]", tree.clone(), json!({ "t": [5] }), r#"ok {"t":["5"]}"#),
        (
            "tree {}",
            tree,
            json!({ "t": {} }),
            "REJECT - t: expected string, got object",
        ),
        (
            "json valid",
            json_schema.clone(),
            json!({ "data": { "a": [1, "x", { "b": true }] } }),
            r#"ok {"data":{"a":[1,"x",{"b":true}]}}"#,
        ),
        (
            "json null",
            json_schema.clone(),
            json!({ "data": null }),
            r#"ok {"data":"null"}"#,
        ),
        (
            "json nested null",
            json_schema,
            json!({ "data": { "a": [1, null] } }),
            r#"ok {"data":{"a":[1,"null"]}}"#,
        ),
        (
            "filter valid",
            filter.clone(),
            json!({ "where": { "and": [{ "field": "a", "eq": "b" }] } }),
            r#"ok {"where":{"and":[{"field":"a","eq":"b"}]}}"#,
        ),
        (
            "filter bad leaf",
            filter.clone(),
            json!({ "where": { "and": [{ "field": "a", "eq": 1, "x": 2 }] } }),
            "REJECT - where.and.0.eq: expected string, got integer",
        ),
        (
            "filter deep bad",
            filter,
            json!({ "where": { "and": [{ "and": [{ "and": [{ "and": [{ "and": [{ "field": 1 }] }] }] }] }] } }),
            "REJECT - where.and.0.and.0.and.0.and.0.and.0.eq: missing",
        ),
    ]);

    for (name, schema, args, expected) in cases {
        assert_eq!(outcome(name, Schema::from(schema), args), expected, "case: {name}");
    }

    // Invalid regex probe throws schema compilation error
    let bad_re = outcome(
        "mcp-invalid-regex",
        Schema::from(json!({ "type": "object", "properties": { "s": { "type": "string", "pattern": "[" } } })),
        json!({ "s": "y" }),
    );
    assert!(
        bad_re.starts_with("THROW Invalid schema for tool \"mcp-invalid-regex\":"),
        "{bad_re}"
    );

    // Huge 5000 & 10000 enums
    let huge_values: Vec<String> = (0..5000).map(|i| format!("v{i}")).collect();
    let huge_schema = Schema::from(json!({
        "type": "object",
        "properties": { "e": { "type": "string", "enum": huge_values } }
    }));
    assert_eq!(
        outcome("mcp-huge-enum", huge_schema.clone(), json!({ "e": "v4999" })),
        r#"ok {"e":"v4999"}"#
    );
    let rej_5k = outcome("mcp-huge-enum", huge_schema, json!({ "e": "v5000" }));
    assert!(rej_5k.contains("and 4980 more]"), "{rej_5k}");

    let values_10k: Vec<String> = (0..10_000).map(|i| format!("member{i}")).collect();
    let schema_10k = Schema::from(json!({
        "type": "object",
        "properties": { "choice": { "type": "string", "enum": values_10k } },
        "required": ["choice"]
    }));
    assert_eq!(
        outcome("enum", schema_10k.clone(), json!({ "choice": "member9999" })),
        r#"ok {"choice":"member9999"}"#
    );
    let rej_10k = outcome("enum", schema_10k, json!({ "choice": "x" }));
    assert!(rej_10k.contains("... and 9980 more]"), "{rej_10k}");

    // Integer coercion when float has fractional part or 1e3 exponent
    let sol_int = t::object([("n", t::integer())]);
    assert_eq!(
        outcome("integer from string", sol_int, json!({ "n": "42" })),
        r#"ok {"n":42}"#
    );
}

#[test]
fn reviewer_probes_sol_and_round_2() {
    // TypeBox additionalProperties converts only undeclared properties
    let add_schema =
        t::object([("amount", t::number()), ("label", t::string())]).with("additionalProperties", t::integer());
    assert_eq!(
        outcome(
            "additionalProperties",
            add_schema,
            json!({ "amount": 3.5, "label": "7" })
        ),
        r#"ok {"amount":3.5,"label":"7"}"#
    );

    // Property names with dots are not regex patterns
    let dot_schema = t::object([("a.b", t::integer()), ("axb", t::string())]);
    assert_eq!(
        outcome("property names", dot_schema, json!({ "a.b": "4", "axb": "5.8" })),
        r#"ok {"a.b":4,"axb":"5.8"}"#
    );

    // Scalar-or-array self ref
    let self_ref = Schema::from(json!({
        "type": "object",
        "properties": {
            "value": { "anyOf": [{ "type": "number" }, { "type": "array", "items": { "$ref": "#/properties/value" } }] }
        },
        "required": ["value"]
    }));
    assert_eq!(
        outcome("self-ref", self_ref, json!({ "value": "42" })),
        r#"ok {"value":42}"#
    );

    // StringEnum with empty member from null (Type.Object makes `mode` required)
    let empty_enum = Schema::from(json!({
        "type": "object",
        "properties": { "mode": { "type": "string", "enum": ["", "x"] } },
        "required": ["mode"]
    }));
    assert_eq!(
        outcome("empty-enum", empty_enum, json!({ "mode": null })),
        r#"ok {"mode":""}"#
    );

    // Round 2: drops optional null inside declared / additional / pattern property values
    let item = json!({ "type": "object", "properties": { "n": { "type": "integer", "minimum": 1 } } });
    for (name, schema) in [
        ("declared", json!({ "type": "object", "properties": { "entry": item } })),
        ("additional", json!({ "type": "object", "additionalProperties": item })),
        (
            "pattern",
            json!({ "type": "object", "patternProperties": { "^entry$": item }, "additionalProperties": false }),
        ),
    ] {
        assert_eq!(
            outcome(name, Schema::from(schema), json!({ "entry": { "n": null } })),
            r#"ok {"entry":{}}"#
        );
    }

    // Round 2: converts anyOf / oneOf branch together with node sibling properties
    for keyword in ["anyOf", "oneOf"] {
        let schema = Schema::from(json!({
            "type": "object",
            keyword: [
                { "type": "object", "properties": { "a": { "type": "array", "items": { "type": "integer" } } }, "required": ["a"] },
                { "type": "object", "properties": { "b": { "type": "boolean" } }, "required": ["b"] }
            ],
            "properties": { "c": { "type": "integer" } },
            "required": ["c"]
        }));
        assert_eq!(
            outcome(keyword, schema, json!({ "a": "3", "c": "2" })),
            r#"ok {"a":[3],"c":2}"#
        );
    }

    // Round 2: rejects deep value every branch of a recursive object union rejects without exponential work
    let s_def = json!({
        "anyOf": [
            { "type": "object", "additionalProperties": { "$ref": "#/$defs/S" }, "required": ["a"] },
            { "type": "object", "additionalProperties": { "$ref": "#/$defs/S" }, "required": ["b"] }
        ]
    });
    let deep_schema = Schema::from(json!({
        "type": "object",
        "properties": { "s": { "$ref": "#/$defs/S" } },
        "$defs": { "S": s_def }
    }));
    let mut val = json!({ "x": 1 });
    for _ in 0..40 {
        val = json!({ "c": val });
    }
    assert_eq!(
        outcome("deep", deep_schema, json!({ "s": val })),
        "REJECT - s.a: missing"
    );
}
