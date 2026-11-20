//! Port of packages/schema/test/coerce.test.ts.

use indexmap::IndexMap;
use pi_schema::{
    AsSchema, CompileOptions, arr, compile, compile_coercing, int, map, nullable, num, obj, open_obj, opt, str, tuple,
};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

fn coerce(schema: impl AsSchema, value: Value) -> Value {
    compile_coercing(schema, CompileOptions::default().with_default_formats())
        .unwrap()
        .coerce(&value)
}

fn coerce_value(schema: Value, value: Value) -> Value {
    let root = json!({
        "type": "object",
        "properties": { "value": schema },
        "required": ["value"]
    });
    let res = coerce(root, json!({ "value": value }));
    res.get("value").cloned().unwrap_or(Value::Null)
}

#[test]
fn compile_options_coerce_flag() {
    let schema = obj([("n", num())]);
    let plain = compile(&schema, CompileOptions::default()).unwrap();
    assert!(!plain.is_coercing());
    assert!(plain.try_coerce(&json!({ "n": "1" })).is_none());
    assert!(plain.try_parse(&json!({ "n": "1" })).is_none());

    let coercing = compile(&schema, CompileOptions::default().coerce(true)).unwrap();
    assert!(coercing.is_coercing());
    assert!(coercing.check(&json!({ "n": 1 })));
    assert_eq!(coercing.try_parse(&json!({ "n": "1" })), Some(Ok(json!({ "n": 1 }))));
}

#[test]
fn coerce_never_modifies_input_and_leaves_uncoercible_values() {
    let value = json!({ "count": "42", "tags": "a" });
    let snapshot = value.clone();
    let schema = obj([("count", num()), ("tags", arr(str()))]);
    assert_eq!(coerce(&schema, value.clone()), json!({ "count": 42, "tags": ["a"] }));
    assert_eq!(value, snapshot);

    let validator = compile_coercing(obj([("count", num())]), CompileOptions::default()).unwrap();
    let coerced = validator.coerce(&json!({ "count": "many" }));
    assert_eq!(coerced, json!({ "count": "many" }));
    assert!(!validator.check(&coerced));
}

#[test]
fn parse_and_validate() {
    let v = compile_coercing(obj([("count", num()), ("tags", arr(str()))]), CompileOptions::default()).unwrap();
    assert_eq!(
        v.parse(&json!({ "count": "42", "tags": "a" })),
        Ok(json!({ "count": 42, "tags": ["a"] }))
    );

    let v2 = compile_coercing(obj([("count", num()), ("name", str())]), CompileOptions::default()).unwrap();
    let errs = v2.parse(&json!({ "count": "many", "name": 7 })).unwrap_err();
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].path, "/count");
    assert_eq!(errs[0].keyword, "type");
    assert_eq!(errs[0].message, "expected number, got string");

    // validate checks without coercing
    let val = json!({ "count": 42, "tags": ["a"] });
    for plain_or_coercing in [
        compile(obj([("count", num()), ("tags", arr(str()))]), CompileOptions::default()).unwrap(),
        compile(
            obj([("count", num()), ("tags", arr(str()))]),
            CompileOptions::default().coerce(true),
        )
        .unwrap(),
    ] {
        assert_eq!(plain_or_coercing.validate(&val), Ok(val.clone()));
        let err_res = plain_or_coercing
            .validate(&json!({ "count": "42", "tags": "a" }))
            .unwrap_err();
        assert_eq!(err_res.len(), 2);
        assert_eq!(err_res[0].path, "/count");
        assert_eq!(err_res[1].path, "/tags");
    }
}

#[test]
fn pass_1_drops_optional_nulls_nested_and_array_items() {
    let schema = open_obj([
        ("path", str()),
        ("offset", opt(num())),
        ("nullable", opt(nullable(str()))),
        ("metadata", obj([("enabled", opt(pi_schema::bool()))])),
    ]);
    assert_eq!(
        coerce(
            &schema,
            json!({ "path": "file.txt", "offset": null, "nullable": null, "metadata": { "enabled": null } })
        ),
        json!({ "path": "file.txt", "nullable": null, "metadata": {} })
    );

    // Keeps null when property is a $ref
    let ref_schema = json!({
        "type": "object",
        "properties": { "value": { "$ref": "#/$defs/value" } },
        "$defs": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } }
    });
    assert_eq!(coerce(&ref_schema, json!({ "value": null })), json!({ "value": null }));

    // Walks array items
    let arr_schema = json!({ "type": "array", "items": obj([("a", str()), ("b", opt(int()))]).into_value() });
    assert_eq!(
        coerce(&arr_schema, json!([{ "a": "x", "b": null }])),
        json!([{ "a": "x" }])
    );
}

#[test]
fn pass_2_typebox_conversion_rules() {
    assert_eq!(coerce_value(json!({ "type": "number" }), json!("42")), json!(42));
    assert_eq!(coerce_value(json!({ "type": "number" }), json!("")), json!(0));
    assert_eq!(coerce_value(json!({ "type": "number" }), json!("true")), json!(1));
    assert_eq!(coerce_value(json!({ "type": "integer" }), json!("3.5")), json!(3));
    assert_eq!(coerce_value(json!({ "type": "integer" }), json!(1.5)), json!(1));
    assert_eq!(coerce_value(json!({ "type": "boolean" }), json!("1")), json!(true));
    assert_eq!(coerce_value(json!({ "type": "boolean" }), json!("FALSE")), json!(false));
    assert_eq!(coerce_value(json!({ "type": "null" }), json!("null")), Value::Null);
    assert_eq!(coerce_value(json!({ "type": "string" }), json!(42)), json!("42"));

    // Wrap single value into array
    assert_eq!(
        coerce_value(json!({ "type": "array", "items": { "type": "number" } }), json!("1")),
        json!([1])
    );
    assert_eq!(
        coerce_value(json!({ "type": "array", "items": { "type": "string" } }), Value::Null),
        json!(["null"])
    );

    // Tuple items convert in place, never wrap
    let tup = json!({ "type": "array", "prefixItems": [{ "type": "number" }, { "type": "boolean" }], "items": false });
    assert_eq!(coerce_value(tup.clone(), json!(["1", "true"])), json!([1, true]));
    assert_eq!(coerce_value(tup, json!("1")), json!("1"));

    // const & enum
    assert_eq!(coerce_value(json!({ "const": 1 }), json!("1")), json!(1));
    assert_eq!(coerce_value(json!({ "const": 1 }), json!("2")), json!("2"));
    assert_eq!(coerce_value(json!({ "enum": [1, 2, "hello"] }), json!("2")), json!(2));
    assert_eq!(
        coerce_value(json!({ "enum": [1, 2, "hello"] }), json!("world")),
        json!("world")
    );

    // Union
    let u = json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] });
    assert_eq!(coerce_value(u.clone(), Value::Null), Value::Null);
    assert_eq!(coerce_value(u.clone(), json!("42")), json!(42));
    assert_eq!(coerce_value(u, json!("null")), Value::Null);
    assert_eq!(
        coerce_value(
            json!({ "oneOf": [{ "type": "boolean" }, { "type": "number" }] }),
            json!("1")
        ),
        json!(true)
    );

    // allOf
    let all_of = json!({
        "allOf": [
            open_obj([("a", num())]).into_value(),
            open_obj([("b", pi_schema::bool())]).into_value()
        ]
    });
    assert_eq!(
        coerce(&all_of, json!({ "a": "1", "b": "true" })),
        json!({ "a": 1, "b": true })
    );

    // $ref
    let ref_s = json!({
        "type": "object",
        "properties": { "item": { "$ref": "#/$defs/item" } },
        "$defs": { "item": { "type": "object", "properties": { "id": { "type": "integer" } } } }
    });
    assert_eq!(
        coerce(&ref_s, json!({ "item": { "id": "7" } })),
        json!({ "item": { "id": 7 } })
    );

    // Relative $ref with root $id and $dynamicRef
    let dyn_schema = json!({
        "$id": "https://example.com/root.json",
        "$dynamicAnchor": "node",
        "type": "object",
        "properties": { "count": { "$ref": "defs/n.json" }, "children": { "type": "array", "items": { "$dynamicRef": "#node" } } }
    });
    let mut schemas = IndexMap::new();
    schemas.insert(
        "https://example.com/defs/n.json".to_string(),
        json!({ "type": "integer" }),
    );
    let dyn_v = compile_coercing(&dyn_schema, CompileOptions::default().with_schemas(schemas)).unwrap();
    assert_eq!(dyn_v.coerce(&json!({ "count": "2" })), json!({ "count": 2 }));
    assert_eq!(dyn_v.parse(&json!({ "count": "2" })), Ok(json!({ "count": 2 })));

    // patternProperties & additionalProperties
    let pat_schema = json!({
        "type": "object",
        "properties": { "a": { "type": "number" } },
        "patternProperties": { "^n_": { "type": "number" } },
        "additionalProperties": { "type": "boolean" }
    });
    assert_eq!(
        coerce(&pat_schema, json!({ "a": "1", "n_x": "2", "other": "true" })),
        json!({ "a": 1, "n_x": 2, "other": true })
    );
}

#[test]
fn pass_3_primitive_coercion_and_conflicts() {
    let cases = [
        (json!({ "type": "number" }), json!(true), json!(1)),
        (json!({ "type": "number" }), Value::Null, json!(0)),
        (json!({ "type": "integer" }), json!("42"), json!(42)),
        (json!({ "type": "boolean" }), json!("true"), json!(true)),
        (json!({ "type": "boolean" }), json!(0), json!(false)),
        (json!({ "type": "string" }), json!(true), json!("true")),
        (json!({ "type": "null" }), json!(""), Value::Null),
        (json!({ "type": "null" }), json!(0), Value::Null),
        (json!({ "type": "null" }), json!(false), Value::Null),
    ];
    for (s, inp, exp) in cases {
        assert_eq!(coerce_value(s, inp), exp);
    }

    assert_eq!(
        coerce_value(json!({ "type": ["number", "string"] }), json!("1")),
        json!("1")
    );
    assert_eq!(
        coerce_value(json!({ "type": ["boolean", "number"] }), json!("1")),
        json!(1)
    );
    assert_eq!(
        coerce_value(json!({ "type": ["integer", "null"] }), json!("5")),
        json!(5)
    );
    assert_eq!(
        coerce_value(
            json!({ "type": ["array", "null"], "items": { "type": "string" } }),
            Value::Null
        ),
        Value::Null
    );

    // Conflicts: Pass 2 conversion rules win over Pass 3, Pass 1 optional null wins over Pass 2
    assert_eq!(coerce_value(json!({ "type": "string" }), Value::Null), json!("null"));
    assert_eq!(coerce_value(json!({ "type": "number" }), json!("")), json!(0));
    assert_eq!(coerce_value(json!({ "type": "boolean" }), json!("0")), json!(false));
    assert_eq!(coerce_value(json!({ "type": "integer" }), json!("42.1")), json!(42));
    assert_eq!(coerce(obj([("name", opt(str()))]), json!({ "name": null })), json!({}));
}

#[test]
fn recursive_schemas_coercion_and_linear_time() {
    let tree = json!({
        "type": "object",
        "properties": { "t": { "$ref": "#/$defs/Tree" } },
        "$defs": { "Tree": { "anyOf": [{ "type": "string" }, { "type": "array", "items": { "$ref": "#/$defs/Tree" } }] } }
    });
    let v = compile_coercing(&tree, CompileOptions::default()).unwrap();
    assert_eq!(v.parse(&json!({ "t": ["a", ["b"]] })), Ok(json!({ "t": ["a", ["b"]] })));
    assert_eq!(v.parse(&json!({ "t": 5 })), Ok(json!({ "t": "5" })));
    assert_eq!(v.parse(&json!({ "t": [5] })), Ok(json!({ "t": ["5"] })));
    assert!(v.parse(&json!({ "t": {} })).is_err());
    assert_eq!(v.coerce(&json!({ "t": { "a": 1 } })), json!({ "t": { "a": 1 } }));

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
    assert_eq!(
        compile_coercing(&json_schema, CompileOptions::default())
            .unwrap()
            .parse(&json!({ "data": { "a": [1, null] } })),
        Ok(json!({ "data": { "a": [1, "null"] } }))
    );

    // 40-deep recursive union must finish in linear time (O(depth), not O(2^depth))
    let s_def = json!({
        "anyOf": [
            { "type": "object", "additionalProperties": { "$ref": "#/$defs/S" }, "required": ["a"] },
            { "type": "object", "additionalProperties": { "$ref": "#/$defs/S" }, "required": ["b"] }
        ]
    });
    let deep_v = compile_coercing(
        json!({ "type": "object", "properties": { "s": { "$ref": "#/$defs/S" } }, "$defs": { "S": s_def } }),
        CompileOptions::default(),
    )
    .unwrap();
    let mut value = json!({ "x": 1 });
    for _ in 0..40 {
        value = json!({ "c": value });
    }
    let errs = deep_v.parse(&json!({ "s": value })).unwrap_err();
    assert_eq!(errs.len(), 42);
    assert_eq!(errs[0].path, "/s/a");
    assert_eq!(errs[41].keyword, "type");
}

#[test]
fn member_keywords_across_passes() {
    let plain = json!({
        "type": "object",
        "patternProperties": { "^n_": { "type": ["integer", "null"] } },
        "additionalProperties": { "type": "string" }
    });
    assert_eq!(
        coerce(&plain, json!({ "n_a": "5", "other": 7 })),
        json!({ "n_a": 5, "other": "7" })
    );

    // Drop optional nulls inside declared, pattern, and additional properties
    let item = obj([("n", opt(int().minimum(1)))]);
    for s in [
        obj([("entry", item.clone())]).into_value(),
        map(item.clone()).into_value(),
        json!({ "type": "object", "patternProperties": { "^entry$": item.as_value() }, "additionalProperties": false }),
    ] {
        let v = compile_coercing(&s, CompileOptions::default()).unwrap();
        assert_eq!(v.parse(&json!({ "entry": { "n": null } })), Ok(json!({ "entry": {} })));
    }

    // Tuple items in pass 1 and pass 3
    let item_opt = obj([("n", opt(int()))]);
    assert_eq!(
        compile_coercing(arr(item_opt.clone()), CompileOptions::default())
            .unwrap()
            .coerce(&json!([{ "n": null }])),
        json!([{}])
    );
    assert_eq!(
        compile_coercing(tuple([item_opt]), CompileOptions::default())
            .unwrap()
            .coerce(&json!([{ "n": null }])),
        json!([{}])
    );

    let tup_pass3 =
        json!({ "type": "array", "prefixItems": [{ "type": ["integer", "null"] }, { "type": ["boolean", "null"] }] });
    assert_eq!(coerce(&tup_pass3, json!(["5", "true"])), json!([5, true]));

    // Union node with sibling properties
    let union_with_props = json!({
        "type": "object",
        "properties": { "a": { "type": "integer" }, "b": { "type": "integer" } },
        "anyOf": [{ "required": ["a"] }, { "required": ["b"] }]
    });
    assert_eq!(
        compile_coercing(&union_with_props, CompileOptions::default())
            .unwrap()
            .parse(&json!({ "a": "3.5" })),
        Ok(json!({ "a": 3 }))
    );

    for kw in ["anyOf", "oneOf"] {
        let schema = json!({
            "type": "object",
            kw: [
                { "type": "object", "properties": { "a": { "type": "array", "items": { "type": "integer" } } }, "required": ["a"] },
                { "type": "object", "properties": { "b": { "type": "boolean" } }, "required": ["b"] }
            ],
            "properties": { "c": { "type": "integer" } },
            "required": ["c"]
        });
        assert_eq!(
            compile_coercing(&schema, CompileOptions::default())
                .unwrap()
                .parse(&json!({ "a": "3", "c": "2" })),
            Ok(json!({ "a": [3], "c": 2 }))
        );
    }
}
