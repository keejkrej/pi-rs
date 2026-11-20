//! Port of packages/schema/test/builders.test.ts plus TypeBox-compatible `t::*` & `string_enum` tests.

use pi_schema::{
    Annotations, ArrOptions, BytesOptions, CompileOptions, NumOptions, ObjOptions, Schema, StrOptions,
    StringEnumOptions, arr, arr_with, bool, bytes_with, compile, int, int_with, lit, map, nul, nullable, num, obj,
    obj_with, one_of, opt, recursive, str, str_with, string_enum, t as tb, tuple, union, unknown, unsafe_schema,
};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

#[test]
fn builders_produce_plain_json_schema() {
    let schema = obj([
        (
            "path",
            str_with(StrOptions {
                annotations: Annotations {
                    description: Some("File path".into()),
                    ..Default::default()
                },
                min_length: Some(1),
                ..Default::default()
            }),
        ),
        (
            "offset",
            opt(int_with(NumOptions {
                minimum: Some(1.0),
                ..Default::default()
            })),
        ),
        ("mode", one_of(["read", "write"])),
    ]);

    let serialized: Value = serde_json::from_str(&pi_js::json::stringify(&schema)).unwrap();
    assert_eq!(
        serialized,
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path", "minLength": 1 },
                "offset": { "type": "integer", "minimum": 1 },
                "mode": { "enum": ["read", "write"] }
            },
            "required": ["path", "mode"],
            "additionalProperties": false
        })
    );
}

#[test]
fn builders_produce_every_leaf() {
    assert_eq!(
        vec![
            str().into_value(),
            num().into_value(),
            int().into_value(),
            bool().into_value(),
            nul().into_value(),
            unknown().into_value(),
            lit(3).into_value(),
            bytes_with(BytesOptions {
                max_length: Some(8),
                ..Default::default()
            })
            .into_value(),
        ],
        vec![
            json!({ "type": "string" }),
            json!({ "type": "number" }),
            json!({ "type": "integer" }),
            json!({ "type": "boolean" }),
            json!({ "type": "null" }),
            json!({}),
            json!({ "const": 3 }),
            json!({ "type": "bytes", "maxLength": 8 }),
        ]
    );

    assert_eq!(
        arr_with(
            str(),
            ArrOptions {
                max_items: Some(2),
                ..Default::default()
            }
        )
        .into_value(),
        json!({ "type": "array", "items": { "type": "string" }, "maxItems": 2 })
    );
    assert_eq!(
        map(num()).into_value(),
        json!({ "type": "object", "additionalProperties": { "type": "number" } })
    );
    assert_eq!(
        tuple([str(), num()]).into_value(),
        json!({
            "type": "array",
            "prefixItems": [{ "type": "string" }, { "type": "number" }],
            "items": false,
            "minItems": 2
        })
    );
    assert_eq!(
        nullable(str()).into_value(),
        json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] })
    );
    assert_eq!(
        obj_with(
            [("a", str())],
            ObjOptions {
                open: true,
                ..Default::default()
            }
        )
        .into_value(),
        json!({
            "type": "object",
            "properties": { "a": { "type": "string" } },
            "required": ["a"]
        })
    );
}

#[test]
fn builders_unsafe_schema_preserves_shape() {
    let usage = unsafe_schema(Schema::from(json!({})));
    let level = unsafe_schema(json!({ "type": "string", "enum": ["low", "high"] }));
    assert_eq!(usage.as_value(), &json!({}));
    assert_eq!(level.as_value(), &json!({ "type": "string", "enum": ["low", "high"] }));
    assert!(
        compile(&usage, CompileOptions::default())
            .unwrap()
            .check(&json!("not usage"))
    );
    assert!(
        !compile(&level, CompileOptions::default())
            .unwrap()
            .check(&json!("medium"))
    );
}

#[test]
fn builders_recursive_checks_every_level() {
    let tree = recursive("Tree", |self_ref| obj([("name", str()), ("children", arr(self_ref))]));
    assert_eq!(
        tree.as_value(),
        &json!({
            "type": "object",
            "properties": { "name": { "type": "string" }, "children": { "type": "array", "items": { "$ref": "Tree" } } },
            "required": ["name", "children"],
            "additionalProperties": false,
            "$id": "Tree"
        })
    );
    let v = compile(&tree, CompileOptions::default()).unwrap();
    assert!(v.check(&json!({ "name": "a", "children": [{ "name": "b", "children": [] }] })));
    let errs =
        v.errors(&json!({ "name": "a", "children": [{ "name": "b", "children": [{ "name": 1, "children": [] }] }] }));
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].path, "/children/0/children/0/name");
    assert_eq!(errs[0].keyword, "type");
    assert_eq!(errs[0].message, "expected string, got integer");
}

#[test]
fn builders_nest_distinct_recursive_schemas() {
    let tree = recursive("Tree", |s| obj([("name", str()), ("children", arr(s))]));
    let list = recursive("List", |s| obj([("value", num()), ("next", nullable(s))]));
    let both = obj([("tree", tree), ("list", opt(list))]);
    let v = compile(&both, CompileOptions::default()).unwrap();

    let root = json!({ "name": "a", "children": [{ "name": "b", "children": [] }] });
    assert!(v.check(&json!({ "tree": root, "list": { "value": 1, "next": { "value": 2, "next": null } } })));
    assert!(!v.check(&json!({ "tree": root, "list": { "value": 1, "next": { "value": "2", "next": null } } })));
    assert!(!v.check(&json!({ "tree": { "name": "a", "children": [{ "name": "b", "children": [{ "name": 3 }] }] } })));
}

#[test]
fn builders_compile_into_checks_with_declared_semantics() {
    let schema = obj([
        ("name", str().min_length(1)),
        ("tags", opt(arr(str()).unique_items(true))),
    ]);
    let v = compile(&schema, CompileOptions::default()).unwrap();
    assert!(v.check(&json!({ "name": "a" })));
    assert!(v.check(&json!({ "name": "a", "tags": ["x", "y"] })));
    let errs = v.errors(&json!({ "name": "", "tags": ["x", "x"], "other": 1 }));
    let simplified: Vec<_> = errs
        .iter()
        .map(|e| (e.path.as_str(), e.keyword.as_str(), e.message.as_str()))
        .collect();
    assert_eq!(
        simplified,
        vec![
            ("/name", "minLength", "expected at least 1 characters"),
            ("/tags", "uniqueItems", "expected unique items"),
            ("/other", "additionalProperties", "unknown field"),
        ]
    );
}

#[test]
fn builders_tagged_union_and_typebox_compat() {
    let block = union([
        obj([("kind", lit("text")), ("text", str())]),
        obj([("kind", lit("image")), ("width", int().minimum(0))]),
    ]);
    let errs = compile(&block, CompileOptions::default())
        .unwrap()
        .errors(&json!({ "kind": "video" }));
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].path, "/kind");
    assert_eq!(errs[0].keyword, "anyOf");

    let se = string_enum(
        ["add", "sub"],
        Some(StringEnumOptions {
            description: Some("Op".into()),
            default_value: Some("add".into()),
        }),
    );
    assert_eq!(
        se.as_value(),
        &json!({ "type": "string", "enum": ["add", "sub"], "description": "Op", "default": "add" })
    );

    let tb_obj = tb::object([
        ("a", tb::string()),
        ("b", tb::integer().optional()),
        ("c", tb::tuple([tb::string(), tb::number()])),
    ]);
    let tv = compile(&tb_obj, CompileOptions::default()).unwrap();
    assert!(tv.check(&json!({ "a": "hi", "c": ["x", 2], "extra": true })));
    assert!(!tv.check(&json!({ "a": "hi", "c": ["x", 2, 3] })));
}
