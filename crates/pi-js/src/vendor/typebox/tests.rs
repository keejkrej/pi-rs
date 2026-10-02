use serde_json::{Value, json};

use super::t;
use super::{Schema, compile, value};

fn js(v: &impl serde::Serialize) -> String {
    crate::json::stringify(v)
}

fn unsafe_of(v: Value) -> Schema {
    t::unsafe_(Schema::from(v))
}

fn schema_by_id(id: &str) -> Schema {
    match id {
        "object_basic" => t::object([
            ("path", t::string().with("description", "Path to the file")),
            ("limit", t::number().with("minimum", 1).optional()),
        ]),
        "object_all_optional" => t::object([("offset", t::number().optional()), ("enabled", t::boolean().optional())]),
        "object_nested" => t::object([
            ("path", t::string()),
            ("offset", t::number().optional()),
            ("nullable", t::union([t::string(), t::null()]).optional()),
            ("metadata", t::object([("enabled", t::boolean().optional())])),
        ]),
        "literal_string" => t::literal("a"),
        "literal_number" => t::literal(1),
        "literal_bool" => t::literal(true),
        "record_string" => t::record(t::string(), t::number()),
        "record_integer" => t::record(t::integer(), t::string()),
        "record_number" => t::record(t::number(), t::boolean()),
        "record_literal" => t::record(t::literal("id"), t::string()),
        "record_bool_key" => t::record(t::boolean(), t::number()),
        "union" => t::union([t::string(), t::null()]).with("description", "nullable"),
        "intersect" => t::intersect([t::object([("a", t::string())]), t::object([("b", t::number())])]),
        "array" => t::array(t::string()).with("minItems", 1),
        "tuple" => t::tuple([t::string(), t::number()]),
        "enum_str" => t::enum_(["add", "subtract"]),
        "enum_num" => t::enum_([1, 2, 3]),
        "any" => t::any(),
        "unknown" => t::unknown(),
        "string" => t::string(),
        "number" => t::number(),
        "integer" => t::integer().with("minimum", 0),
        "boolean" => t::boolean(),
        "null" => t::null(),
        "string_constraints" => t::string()
            .with("minLength", 1)
            .with("maxLength", 4)
            .with("pattern", "^[a-z]+$"),
        "unsafe_enum" => unsafe_of(json!({
            "type": "string",
            "enum": ["add", "subtract"],
            "description": "The operation to perform"
        })),
        "ref_defs" => unsafe_of(json!({
            "type": "object",
            "properties": { "value": { "$ref": "#/$defs/value" } },
            "$defs": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } }
        })),
        "defs_keyword" => unsafe_of(json!({
            "type": "object",
            "properties": { "value": { "$ref": "#/definitions/value" } },
            "definitions": { "value": { "type": "string" } }
        })),
        "number_bounds" => t::number()
            .with("minimum", 1)
            .with("maximum", 10)
            .with("exclusiveMinimum", 0)
            .with("exclusiveMaximum", 11)
            .with("multipleOf", 1),
        "array_keywords" => t::array(t::number())
            .with("minItems", 1)
            .with("maxItems", 3)
            .with("uniqueItems", true),
        "prefix" => unsafe_of(json!({
            "type": "array",
            "prefixItems": [{ "type": "string" }, { "type": "number" }],
            "items": { "type": "boolean" }
        })),
        "additional" => t::object([("a", t::string())]).with("additionalProperties", false),
        "additional_schema" => t::object([("a", t::string())]).with("additionalProperties", t::number()),
        "pattern_props" => unsafe_of(json!({
            "type": "object",
            "patternProperties": { "^x-": { "type": "number" } },
            "additionalProperties": false
        })),
        "not_schema" => unsafe_of(json!({ "not": { "type": "string" } })),
        "one_of" => unsafe_of(json!({ "oneOf": [{ "type": "string" }, { "type": "number" }] })),
        "all_of" => unsafe_of(json!({
            "allOf": [
                { "type": "object" },
                { "required": ["a"], "properties": { "a": { "type": "string" } } }
            ]
        })),
        "format_email" => t::string().with("format", "email"),
        "format_uuid" => t::string().with("format", "uuid"),
        "format_date" => t::string().with("format", "date"),
        "format_date_time" => t::string().with("format", "date-time"),
        "format_uri" => t::string().with("format", "uri"),
        "format_ipv4" => t::string().with("format", "ipv4"),
        "type_array" => unsafe_of(json!({ "type": ["number", "string"] })),
        "with_default" => t::object([
            ("a", t::string().with("default", "hi")),
            ("b", t::number().with("default", 1).optional()),
        ]),
        "union_obj" => t::union([t::object([("nested", t::string())]), t::null()]),
        "enum_as_union_input" => t::enum_(["a", "b"]),
        "length_emoji" | "length_ascii" | "length_combining" => t::string().with("minLength", 2).with("maxLength", 2),
        other => panic!("unknown schema id {other}"),
    }
}

#[test]
fn builders_match_typebox_1_3_27() {
    let root: Value = serde_json::from_str(include_str!("testdata/vectors.json")).unwrap();
    let built = root["built"].as_object().unwrap();
    assert_eq!(built.len(), 47);
    for (id, expected) in built {
        let got = js(&schema_by_id(id));
        assert_eq!(got, expected.as_str().unwrap(), "{id}");
    }
}

#[test]
fn value_ops_match_node() {
    let root: Value = serde_json::from_str(include_str!("testdata/vectors.json")).unwrap();
    let cases = root["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 142);
    for (index, case) in cases.iter().enumerate() {
        let id = case["id"].as_str().unwrap();
        let input = &case["value"];
        let schema = schema_by_id(id);
        let label = format!("{index}:{id} {}", js(input));
        let utf16_len = id == "length_emoji" || id == "length_combining";
        let got_check = value::check(&schema, input);
        let compiled = compile(&schema);
        if utf16_len {
            // TypeBox 1.3.27 counts graphemes. This port counts UTF-16 code units.
            assert!(got_check, "{label}");
            assert!(compiled.check(input), "{label}");
            assert!(value::errors(&schema, input).is_empty(), "{label}");
            assert!(compiled.errors(input).is_empty(), "{label}");
        } else {
            assert_eq!(got_check, case["check"].as_bool().unwrap(), "check {label}");
            assert_eq!(
                compiled.check(input),
                case["compileCheck"].as_bool().unwrap(),
                "compile {label}"
            );
            assert_errors(&value::errors(&schema, input), &case["errors"], &label);
            assert_errors(&compiled.errors(input), &case["compileErrors"], &label);
        }
        assert!(case["convertThrew"].is_null(), "{label}");
        assert!(case["cleanThrew"].is_null(), "{label}");
        assert!(case["defaultThrew"].is_null(), "{label}");
        assert_eq!(
            js(&value::convert(&schema, input.clone())),
            js(&case["converted"]),
            "convert {label}"
        );
        assert_eq!(
            js(&value::clean(&schema, input.clone())),
            js(&case["cleaned"]),
            "clean {label}"
        );
        assert_eq!(
            js(&value::default(&schema, input.clone())),
            js(&case["defaulted"]),
            "default {label}"
        );
    }
}

fn assert_errors(got: &[super::ValueError], expected: &Value, label: &str) {
    let expected = expected.as_array().unwrap();
    assert_eq!(got.len(), expected.len(), "error count {label}\n got {got:?}");
    for (err, exp) in got.iter().zip(expected) {
        assert_eq!(err.keyword, exp["keyword"].as_str().unwrap(), "{label}");
        assert_eq!(err.schema_path, exp["schemaPath"].as_str().unwrap(), "{label}");
        assert_eq!(err.instance_path, exp["instancePath"].as_str().unwrap(), "{label}");
        assert_eq!(err.message, exp["message"].as_str().unwrap(), "{label}");
        assert_eq!(js(&err.params), js(&exp["params"]), "params {label}");
    }
}

#[test]
fn from_json_has_no_kind() {
    let schema = Schema::from(json!({
        "type": "object",
        "required": ["path"],
        "properties": { "path": { "type": "string", "minLength": 1 } }
    }));
    assert!(!schema.is_typebox());
    let ok = json!({ "path": "a" });
    let bad = json!({ "path": 1 });
    assert!(value::check(&schema, &ok));
    assert!(!value::check(&schema, &bad));
    assert_eq!(value::errors(&schema, &bad)[0].keyword, "type");
    assert_eq!(js(&value::convert(&schema, bad.clone())), js(&bad));
    assert_eq!(js(&value::clean(&schema, bad.clone())), js(&bad));
}

#[test]
fn optional_is_not_a_keyword() {
    let params = t::object([
        ("path", t::string().with("description", "Path to the file")),
        ("limit", t::number().with("minimum", 1).optional()),
    ]);
    let text = js(&params);
    assert!(params.is_typebox());
    assert!(!text.contains("optional"));
    assert!(text.contains("\"required\":[\"path\"]"));
}
