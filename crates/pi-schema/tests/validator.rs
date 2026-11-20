//! Port of packages/schema/test/validator.test.ts plus $schema dialect detection and check_result tests.

use indexmap::IndexMap;
use pi_schema::{
    CompileOptions, Draft, SchemaError, UnresolvedRefs, ValidationError, compile, compile_coercing, t as tb,
};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

fn simplify_errors(errors: &[ValidationError]) -> Vec<(String, String, String)> {
    errors
        .iter()
        .map(|e| (e.path.clone(), e.keyword.clone(), e.message.clone()))
        .collect()
}

fn read_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "offset": { "type": "integer", "minimum": 1 }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

fn block_schema() -> Value {
    json!({
        "anyOf": [
            {
                "type": "object",
                "properties": { "kind": { "const": "text" }, "text": { "type": "string" } },
                "required": ["kind", "text"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": { "kind": { "const": "image" }, "width": { "type": "integer", "minimum": 0 } },
                "required": ["kind", "width"],
                "additionalProperties": false
            }
        ]
    })
}

#[test]
fn errors_report_missing_minimum_and_unknown_field_at_field() {
    let v = compile(read_schema(), CompileOptions::default()).unwrap();
    let errs = v.errors(&json!({ "offset": 0, "extra": true }));
    assert_eq!(
        simplify_errors(&errs),
        vec![
            ("/path".into(), "required".into(), "missing".into()),
            ("/offset".into(), "minimum".into(), "expected >= 1".into()),
            ("/extra".into(), "additionalProperties".into(), "unknown field".into()),
        ]
    );
    assert_eq!(errs[0].schema_path, "#/required");
    assert_eq!(errs[1].schema_path, "#/properties/offset/minimum");
    assert_eq!(errs[2].schema_path, "#/additionalProperties");
    assert!(v.check_result(&json!({ "offset": 0, "extra": true })).is_err());
}

#[test]
fn errors_empty_and_check_result_ok_for_valid_value() {
    let v = compile(read_schema(), CompileOptions::default()).unwrap();
    let val = json!({ "path": "a.ts" });
    assert!(v.errors(&val).is_empty());
    assert_eq!(v.check_result(&val), Ok(()));
    assert_eq!(v.validate(&val), Ok(val));
}

#[test]
fn errors_report_branch_selected_by_tagged_union() {
    let v = compile(
        json!({ "type": "array", "items": block_schema() }),
        CompileOptions::default(),
    )
    .unwrap();
    let errs = v.errors(&json!([
        { "kind": "text", "text": "a" },
        { "kind": "image", "width": -1 }
    ]));
    assert_eq!(
        simplify_errors(&errs),
        vec![("/1/width".into(), "minimum".into(), "expected >= 0".into())]
    );
    let errs2 = v.errors(&json!([{ "kind": "video" }]));
    assert_eq!(
        simplify_errors(&errs2),
        vec![(
            "/0/kind".into(),
            "anyOf".into(),
            "expected one of [\"text\",\"image\"]".into()
        )]
    );
}

#[test]
fn errors_escape_tilde_and_slash_in_json_pointer_paths() {
    let v = compile(
        json!({ "type": "object", "properties": { "a/b~c": { "type": "string" } } }),
        CompileOptions::default(),
    )
    .unwrap();
    assert_eq!(
        simplify_errors(&v.errors(&json!({ "a/b~c": 1 }))),
        vec![("/a~1b~0c".into(), "type".into(), "expected string, got integer".into())]
    );
}

#[test]
fn errors_follow_ref_into_referenced_schema() {
    let v = compile(
        json!({
            "$defs": { "point": { "type": "object", "properties": { "x": { "type": "number" } }, "required": ["x"] } },
            "type": "array",
            "items": { "$ref": "#/$defs/point" }
        }),
        CompileOptions::default(),
    )
    .unwrap();
    assert_eq!(
        simplify_errors(&v.errors(&json!([{ "x": 1 }, {}]))),
        vec![("/1/x".into(), "required".into(), "missing".into())]
    );
}

#[test]
fn values_unknown_type_name_rejects_all_values() {
    let v = compile(json!({ "type": "constructor" }), CompileOptions::default()).unwrap();
    assert!(!v.check(&json!(1)));
    assert!(!v.check(&json!({})));
}

#[test]
fn values_bytes_bigint_undefined_rejected_in_json() {
    assert!(
        !compile(json!({ "type": "bytes" }), CompileOptions::default())
            .unwrap()
            .check(&json!([1, 2]))
    );
    assert!(
        !compile(json!({ "type": "bigint" }), CompileOptions::default())
            .unwrap()
            .check(&json!(3))
    );
    assert!(
        !compile(json!({ "type": "undefined" }), CompileOptions::default())
            .unwrap()
            .check(&Value::Null)
    );
}

#[test]
fn options_format_annotation_by_default_and_validated_with_formats() {
    let schema = json!({ "type": "string", "format": "email" });
    let plain = compile(&schema, CompileOptions::default()).unwrap();
    assert!(plain.check(&json!("not an email")));

    let checked = compile(&schema, CompileOptions::default().with_default_formats()).unwrap();
    assert!(!checked.check(&json!("not an email")));
    assert!(checked.check(&json!("a@example.com")));
}

#[test]
fn options_resolve_ref_to_registered_documents() {
    let point = json!({ "type": "object", "properties": { "x": { "type": "number" } }, "required": ["x"] });
    let mut schemas = IndexMap::new();
    schemas.insert("https://example.com/point.json".to_string(), point);
    let v = compile(
        json!({ "$ref": "https://example.com/point.json" }),
        CompileOptions::default().with_schemas(schemas),
    )
    .unwrap();
    assert!(v.check(&json!({ "x": 1 })));
    assert!(!v.check(&json!({})));
}

#[test]
fn options_throw_on_unresolved_ref_by_default() {
    assert!(matches!(
        compile(
            json!({ "$ref": "https://example.com/missing.json" }),
            CompileOptions::default()
        ),
        Err(SchemaError::UnresolvedRef { .. })
    ));
    assert!(matches!(
        compile(json!({ "$ref": "#/$defs/missing" }), CompileOptions::default()),
        Err(SchemaError::UnresolvedRef { .. })
    ));
}

#[test]
fn options_draft_7_semantics_and_schema_dialect_detection() {
    // Explicit draft option
    let schema = json!({ "definitions": { "s": { "type": "string" } }, "$ref": "#/definitions/s", "maxLength": 1 });
    assert!(
        compile(&schema, CompileOptions::default().draft(Draft::Draft7))
            .unwrap()
            .check(&json!("ab"))
    );
    assert!(!compile(&schema, CompileOptions::default()).unwrap().check(&json!("ab")));

    // Automatic $schema dialect detection when options.draft is None
    let auto_draft7 = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "definitions": { "s": { "type": "string" } },
        "$ref": "#/definitions/s",
        "maxLength": 1
    });
    assert!(
        compile(&auto_draft7, CompileOptions::default())
            .unwrap()
            .check(&json!("ab"))
    );
    assert!(
        !compile(&auto_draft7, CompileOptions::default().draft(Draft::Draft2020_12))
            .unwrap()
            .check(&json!("ab"))
    );
}

#[test]
fn schemas_typebox_compat_builders_compile() {
    let schema = tb::object([("name", tb::string()), ("tags", tb::array(tb::string()))]);
    let v = compile(&schema, CompileOptions::default()).unwrap();
    assert!(v.check(&json!({ "name": "a", "tags": [] })));
    assert_eq!(
        simplify_errors(&v.errors(&json!({ "name": "a", "tags": [1] }))),
        vec![("/tags/0".into(), "type".into(), "expected string, got integer".into())]
    );
}

#[test]
fn schemas_recursive_tree() {
    let tree = json!({
        "type": "object",
        "properties": { "value": { "type": "number" }, "children": { "type": "array", "items": { "$ref": "#" } } },
        "required": ["value"]
    });
    let v = compile(&tree, CompileOptions::default()).unwrap();
    assert!(v.check(&json!({ "value": 1, "children": [{ "value": 2, "children": [{ "value": 3 }] }] })));
    assert_eq!(
        simplify_errors(&v.errors(&json!({ "value": 1, "children": [{ "value": 2, "children": [{}] }] }))),
        vec![(
            "/children/0/children/0/value".into(),
            "required".into(),
            "missing".into()
        )]
    );
}

#[test]
fn issue_11697_relative_root_id_applied_once() {
    let schema = json!({
        "$id": "schemas/tool.json",
        "type": "object",
        "properties": { "count": { "$ref": "#/$defs/count" } },
        "required": ["count"],
        "$defs": { "count": { "type": "integer" } }
    });
    assert!(
        compile(&schema, CompileOptions::default())
            .unwrap()
            .check(&json!({ "count": 2 }))
    );
    let v = compile(
        &schema,
        CompileOptions::default().unresolved_refs(UnresolvedRefs::Reject),
    )
    .unwrap();
    assert!(v.check(&json!({ "count": 2 })));
    assert_eq!(
        simplify_errors(&v.errors(&json!({ "count": "a" }))),
        vec![("/count".into(), "type".into(), "expected integer, got string".into())]
    );
    let c = compile_coercing(&schema, CompileOptions::default()).unwrap();
    assert_eq!(c.parse(&json!({ "count": "2" })), Ok(json!({ "count": 2 })));
}

#[test]
fn issue_11697_relative_root_id_with_unevaluated_keywords() {
    let props = json!({
        "$id": "schemas/tool.json",
        "type": "object",
        "properties": { "count": { "$ref": "#/$defs/count" } },
        "$defs": { "count": { "type": "integer" } },
        "unevaluatedProperties": false
    });
    let v = compile(
        &props,
        CompileOptions::default().unresolved_refs(UnresolvedRefs::Reject),
    )
    .unwrap();
    assert!(v.check(&json!({ "count": 2 })));
    assert_eq!(
        simplify_errors(&v.errors(&json!({ "count": 2, "extra": 1 }))),
        vec![("/extra".into(), "unevaluatedProperties".into(), "unknown field".into())]
    );

    let items = json!({
        "$id": "schemas/list.json",
        "type": "array",
        "prefixItems": [{ "$ref": "#/$defs/count" }],
        "$defs": { "count": { "type": "integer" } },
        "unevaluatedItems": false
    });
    assert!(
        compile(
            &items,
            CompileOptions::default().unresolved_refs(UnresolvedRefs::Reject)
        )
        .unwrap()
        .check(&json!([2]))
    );
    assert!(
        !compile(&items, CompileOptions::default())
            .unwrap()
            .check(&json!([2, 3]))
    );
}

#[test]
fn explains_errors_under_nested_resources_with_relative_id() {
    let nested = json!({
        "$id": "https://example.com/root.json",
        "allOf": [{ "$ref": "#/$defs/child" }],
        "$defs": {
            "child": {
                "$id": "schemas/tool.json",
                "$ref": "#/$defs/props",
                "unevaluatedProperties": false,
                "$defs": { "props": { "properties": { "count": { "type": "integer" } } } }
            }
        }
    });
    assert_eq!(
        simplify_errors(
            &compile(&nested, CompileOptions::default())
                .unwrap()
                .errors(&json!({ "count": 2, "extra": 1 }))
        ),
        vec![("/extra".into(), "unevaluatedProperties".into(), "unknown field".into())]
    );

    let branch = json!({
        "$id": "https://example.com/root.json",
        "anyOf": [
            { "$id": "a/b.json", "$ref": "#/$defs/n", "$defs": { "n": { "type": "integer" } } },
            { "type": "boolean" }
        ]
    });
    assert_eq!(
        simplify_errors(&compile(&branch, CompileOptions::default()).unwrap().errors(&json!({}))),
        vec![("".into(), "type".into(), "expected integer, got object".into())]
    );
}

#[test]
fn malformed_schema_returns_invalid_schema_error() {
    assert!(matches!(
        compile(json!({ "properties": { "a": 1 } }), CompileOptions::default()),
        Err(SchemaError::InvalidSchema(_))
    ));
}

#[test]
fn unresolved_refs_reject_behavior() {
    let dangling = json!({ "type": "object", "properties": { "t": { "$ref": "#/definitions/Missing" } } });
    let external = json!({ "type": "object", "properties": { "t": { "$ref": "https://example.com/schemas/t.json" } } });
    for schema in [&dangling, &external] {
        assert!(compile(schema, CompileOptions::default()).is_err());
        let v = compile(
            schema,
            CompileOptions::default().unresolved_refs(UnresolvedRefs::Reject),
        )
        .unwrap();
        assert!(v.check(&json!({})));
        assert!(!v.check(&json!({ "t": 1 })));
    }
    let errs = compile(
        &dangling,
        CompileOptions::default().unresolved_refs(UnresolvedRefs::Reject),
    )
    .unwrap()
    .errors(&json!({ "t": 1 }));
    assert_eq!(
        simplify_errors(&errs),
        vec![(
            "/t".into(),
            "$ref".into(),
            "unresolved reference #/definitions/Missing".into()
        )]
    );

    let coerced = compile_coercing(
        &dangling,
        CompileOptions::default().unresolved_refs(UnresolvedRefs::Reject),
    )
    .unwrap();
    assert_eq!(
        coerced.parse(&json!({ "t": "1" })),
        Err(vec![ValidationError::new(
            "/t",
            "#/properties/t/$ref",
            "$ref",
            "unresolved reference #/definitions/Missing"
        )])
    );
}

#[test]
fn leaf_checks_agree_across_standalone_closed_open_and_array() {
    let cases: Vec<(Value, Vec<Value>)> = vec![
        (
            json!({ "const": "a" }),
            vec![json!("a"), json!("b"), json!(1), Value::Null],
        ),
        (
            json!({ "const": null }),
            vec![Value::Null, json!(0), json!(false), json!("null")],
        ),
        (
            json!({ "enum": ["a", 1, null] }),
            vec![json!("a"), json!(1), Value::Null, json!("b"), json!(2), json!(true)],
        ),
        (
            json!({ "type": ["string", "null"] }),
            vec![json!("a"), Value::Null, json!(1), json!({}), json!([])],
        ),
        (
            json!({ "type": ["integer", "boolean"] }),
            vec![json!(1), json!(1.5), json!(true), json!("1")],
        ),
        (
            json!({ "type": ["number", "array"] }),
            vec![json!(1.5), json!(2), json!([]), json!({})],
        ),
        (
            json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] }),
            vec![json!("a"), Value::Null, json!(1)],
        ),
        (
            json!({ "anyOf": [{ "type": "integer" }, { "type": "string", "minLength": 2 }] }),
            vec![json!(1), json!(1.5), json!("ab"), json!("a")],
        ),
        (
            json!({ "type": "integer", "minimum": 1, "exclusiveMaximum": 5 }),
            vec![json!(1), json!(4), json!(5), json!(0), json!(2.5), json!("2")],
        ),
        (
            json!({ "type": "number", "exclusiveMinimum": 0, "maximum": 1 }),
            vec![json!(0.5), json!(1), json!(0), json!(1.5), json!("0.5")],
        ),
        (
            json!({ "type": "integer", "multipleOf": 2 }),
            vec![json!(2), json!(3), json!(2.5)],
        ),
        (
            json!({ "type": "string", "minLength": 2, "maxLength": 3 }),
            vec![
                json!("ab"),
                json!("abc"),
                json!("a"),
                json!("abcd"),
                json!("😀😀"),
                json!(12),
            ],
        ),
        (
            json!({ "type": "string", "pattern": "^a+$" }),
            vec![json!("aa"), json!("ab"), json!(1)],
        ),
        (
            json!({ "minimum": 1 }),
            vec![json!(1), json!(0), json!("x"), Value::Null],
        ),
    ];

    for (schema, values) in cases {
        let alone = compile(&schema, CompileOptions::default()).unwrap();
        let closed = compile(
            json!({ "type": "object", "properties": { "p": schema }, "additionalProperties": false }),
            CompileOptions::default(),
        )
        .unwrap();
        let open = compile(
            json!({ "type": "object", "properties": { "p": schema }, "required": ["q"] }),
            CompileOptions::default(),
        )
        .unwrap();
        let items = compile(json!({ "type": "array", "items": schema }), CompileOptions::default()).unwrap();

        for v in values {
            let expected = alone.check(&v);
            assert_eq!(
                closed.check(&json!({ "p": v })),
                expected,
                "closed schema={schema} v={v}"
            );
            assert_eq!(
                open.check(&json!({ "p": v, "q": 1 })),
                expected,
                "open schema={schema} v={v}"
            );
            assert_eq!(items.check(&json!([v])), expected, "items schema={schema} v={v}");
        }
    }
}

#[test]
fn string_lengths_count_code_points() {
    assert!(
        compile(json!({ "type": "string", "maxLength": 1 }), CompileOptions::default())
            .unwrap()
            .check(&json!("😀"))
    );
    assert!(
        !compile(json!({ "type": "string", "minLength": 2 }), CompileOptions::default())
            .unwrap()
            .check(&json!("😀"))
    );
    assert!(
        compile(json!({ "type": "string", "minLength": 2 }), CompileOptions::default())
            .unwrap()
            .check(&json!("😀A"))
    );
    assert!(
        !compile(json!({ "type": "string", "maxLength": 1 }), CompileOptions::default())
            .unwrap()
            .check(&json!("😀A"))
    );
}

#[test]
fn error_messages_for_enum_and_contains_and_one_of_blaming() {
    let values: Vec<Value> = (0..5000).map(|i| Value::String(format!("v{i}"))).collect();
    let v = compile(
        json!({ "type": "object", "properties": { "e": { "type": "string", "enum": values } } }),
        CompileOptions::default(),
    )
    .unwrap();
    assert!(v.check(&json!({ "e": "v4999" })));
    let errs = v.errors(&json!({ "e": "v5000" }));
    assert_eq!(errs.len(), 1);
    let prefix = pi_js::json::stringify(&values[..20]);
    let expected_msg = format!("expected one of {}, ... and 4980 more]", &prefix[..prefix.len() - 1]);
    assert_eq!(errs[0].message, expected_msg);

    assert_eq!(
        compile(json!({ "enum": ["a", "b"] }), CompileOptions::default())
            .unwrap()
            .errors(&json!("c"))[0]
            .message,
        "expected one of [\"a\",\"b\"]"
    );

    let ten_k: Vec<Value> = (0..10_000).map(|i| Value::String(format!("member{i}"))).collect();
    assert!(
        compile(json!({ "enum": ten_k }), CompileOptions::default())
            .unwrap()
            .check(&json!("member9999"))
    );

    let contains_msg = |extra: Value| -> String {
        let mut s = json!({ "type": "array" });
        if let (Some(dst), Some(src)) = (s.as_object_mut(), extra.as_object()) {
            for (k, val) in src {
                dst.insert(k.clone(), val.clone());
            }
        }
        compile(s, CompileOptions::default()).unwrap().errors(&json!(["a"]))[0]
            .message
            .clone()
    };
    assert_eq!(
        contains_msg(json!({ "contains": { "type": "number" } })),
        "expected at least 1 matching item"
    );
    assert_eq!(
        contains_msg(json!({ "contains": { "type": "number" }, "minContains": 2 })),
        "expected at least 2 matching items"
    );
    assert_eq!(
        contains_msg(json!({ "contains": { "type": "string" }, "maxContains": 0, "minContains": 0 })),
        "expected at most 0 matching items"
    );
    assert_eq!(
        contains_msg(json!({ "contains": { "type": "number" }, "minContains": 1, "maxContains": 3 })),
        "expected between 1 and 3 matching items"
    );

    // Blame oneOf failure on branch declaring most properties
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
    let errs = compile(filter, CompileOptions::default())
        .unwrap()
        .errors(&json!({ "where": { "and": [{ "field": "a", "eq": 1, "x": 2 }] } }));
    assert_eq!(
        simplify_errors(&errs),
        vec![
            (
                "/where/and/0/eq".into(),
                "type".into(),
                "expected string, got integer".into()
            ),
            (
                "/where/and/0/x".into(),
                "additionalProperties".into(),
                "unknown field".into()
            ),
        ]
    );
}

#[test]
fn extra_formats_url_and_json_pointer_uri_fragment() {
    let url = compile(
        json!({ "type": "string", "format": "url" }),
        CompileOptions::default().with_default_formats(),
    )
    .unwrap();
    assert!(url.check(&json!("https://example.com/a?b=1")));
    assert!(!url.check(&json!("not a url")));

    let frag = compile(
        json!({ "type": "string", "format": "json-pointer-uri-fragment" }),
        CompileOptions::default().with_default_formats(),
    )
    .unwrap();
    assert!(frag.check(&json!("#/a/b~1c")));
    assert!(!frag.check(&json!("#/a b")));
    assert!(!frag.check(&json!("/a")));
}
