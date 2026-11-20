//! Port of TypeBox 1.3.27 Value.Convert conformance cases (packages/schema/test/coerce-typebox.test.ts).

#![allow(clippy::approx_constant)]

use pi_schema::{CompileOptions, compile_coercing, t as tb};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

fn convert(schema: &pi_schema::Schema, value: Value) -> Value {
    compile_coercing(schema, CompileOptions::default().with_default_formats())
        .unwrap()
        .convert_only(&value)
}

#[test]
fn convert_any_and_unknown() {
    for schema in [tb::any(), tb::unknown()] {
        for val in [
            Value::Null,
            json!("hello"),
            json!(42),
            json!(true),
            json!({ "x": 1 }),
            json!([1, 2, 3]),
        ] {
            assert_eq!(convert(&schema, val.clone()), val);
        }
    }
}

#[test]
fn convert_array_and_wrapping() {
    let arr_num = tb::array(tb::number());
    assert_eq!(
        convert(
            &arr_num,
            json!([1, 3.14, "1", "3.14", true, false, "true", "false", "hello"])
        ),
        json!([1, 3.14, 1, 3.14, 1, 0, 1, 0, "hello"])
    );
    assert_eq!(convert(&arr_num, json!("1")), json!([1]));
    assert_eq!(convert(&arr_num, json!([])), json!([]));
    assert_eq!(convert(&arr_num, json!(42)), json!([42]));
    assert_eq!(convert(&arr_num, json!("hello")), json!(["hello"]));

    let arr_bool = tb::array(tb::boolean());
    assert_eq!(
        convert(
            &arr_bool,
            json!([1, 3.14, "1", "3.14", true, false, "true", "false", "hello"])
        ),
        json!([true, 3.14, true, "3.14", true, false, true, false, "hello"])
    );
    assert_eq!(convert(&arr_bool, json!(true)), json!([true]));
    assert_eq!(convert(&arr_bool, json!(0)), json!([false]));

    let arr_str = tb::array(tb::string());
    assert_eq!(
        convert(
            &arr_str,
            json!([1, 3.14, "1", "3.14", true, false, "true", "false", "hello"])
        ),
        json!(["1", "3.14", "1", "3.14", "true", "false", "true", "false", "hello"])
    );

    let nested_intersect = tb::array(tb::intersect([
        tb::object([("x", tb::number())]),
        tb::object([("y", tb::number())]),
    ]));
    let v = compile_coercing(&nested_intersect, CompileOptions::default()).unwrap();
    let res = v.convert_only(&json!([{ "x": "1", "y": "2" }, { "x": "3", "y": "4" }]));
    assert!(v.check(&res));
}

#[test]
fn convert_boolean_null_number_integer_string() {
    let b = tb::boolean();
    assert_eq!(convert(&b, json!("1")), json!(true));
    assert_eq!(convert(&b, json!("3.14")), json!("3.14"));
    assert_eq!(convert(&b, json!("TRUE")), json!(true));
    assert_eq!(convert(&b, json!("false")), json!(false));
    assert_eq!(convert(&b, json!("0")), json!(false));
    assert_eq!(convert(&b, json!(1)), json!(true));
    assert_eq!(convert(&b, json!(0)), json!(false));
    assert_eq!(convert(&b, json!(2)), json!(2));
    assert_eq!(convert(&b, Value::Null), json!(false));

    let n = tb::null();
    for v in [
        json!("null"),
        json!("NULL"),
        json!("undefined"),
        json!(""),
        json!("0"),
        json!(0),
        json!(false),
        Value::Null,
    ] {
        assert_eq!(convert(&n, v), Value::Null);
    }
    assert_eq!(convert(&n, json!("nil")), json!("nil"));
    assert_eq!(convert(&n, json!(true)), json!(true));
    assert_eq!(convert(&n, json!(1)), json!(1));

    let num = tb::number();
    assert_eq!(convert(&num, json!("3.14")), json!(3.14));
    assert_eq!(convert(&num, json!("-100")), json!(-100));
    assert_eq!(convert(&num, json!(true)), json!(1));
    assert_eq!(convert(&num, json!(false)), json!(0));
    assert_eq!(convert(&num, json!("123n")), json!(123));
    assert_eq!(convert(&num, json!("90071992547409910n")), json!("90071992547409910n"));

    let int = tb::integer();
    assert_eq!(convert(&int, json!("3.14")), json!(3));
    assert_eq!(convert(&int, json!("1738213389080.555")), json!(1_738_213_389_080_u64));
    assert_eq!(
        convert(&int, json!(1_738_213_389_080.555_5_f64)),
        json!(1_738_213_389_080_u64)
    );

    let s = tb::string();
    assert_eq!(convert(&s, json!(3.14)), json!("3.14"));
    assert_eq!(convert(&s, json!(3)), json!("3"));
    assert_eq!(convert(&s, json!(true)), json!("true"));
    assert_eq!(convert(&s, json!(false)), json!("false"));
    assert_eq!(convert(&s, Value::Null), json!("null"));
}

#[test]
fn convert_const_enum_object_record_tuple_union() {
    assert_eq!(convert(&tb::literal("1"), json!(1)), json!("1"));
    assert_eq!(convert(&tb::literal("1"), json!(2)), json!(2));
    assert_eq!(convert(&tb::literal(3.14), json!("3.14")), json!(3.14));
    assert_eq!(convert(&tb::literal(true), json!(1)), json!(true));
    assert_eq!(convert(&tb::literal(false), json!("0")), json!(false));

    let enum_obj = tb::object([("x", tb::enum_([json!(1), json!(2), json!("hello")]))]);
    assert_eq!(convert(&enum_obj, json!({ "x": "1" })), json!({ "x": 1 }));
    assert_eq!(convert(&enum_obj, json!({ "x": "2" })), json!({ "x": 2 }));
    assert_eq!(convert(&enum_obj, json!({ "x": "world" })), json!({ "x": "world" }));

    let obj_add = tb::object([("x", tb::number())]).with("additionalProperties", tb::null());
    assert_eq!(
        convert(&obj_add, json!({ "x": "1", "y": 0 })),
        json!({ "x": 1, "y": null })
    );

    let rec = tb::record(tb::string(), tb::number());
    assert_eq!(
        convert(&rec, json!({ "x": "42", "y": "24", "z": "hello" })),
        json!({ "x": 42, "y": 24, "z": "hello" })
    );

    let tup = tb::tuple([tb::number(), tb::number()]);
    assert_eq!(convert(&tup, json!(["1", "true"])), json!([1, 1]));
    assert_eq!(convert(&tup, json!(["1", "2", "3"])), json!([1, 2, "3"]));
    assert_eq!(convert(&tup, json!("not an array")), json!("not an array"));

    // Issue #787 intersected union
    let inter = tb::intersect([
        tb::union([tb::object([("a", tb::number())]), tb::object([("b", tb::number())])]),
        tb::object([("c", tb::number())]),
    ]);
    assert_eq!(
        convert(&inter, json!({ "a": "1", "c": "2" })),
        json!({ "a": 1, "c": 2 })
    );
    assert_eq!(
        convert(&inter, json!({ "b": "1", "c": "2" })),
        json!({ "b": 1, "c": 2 })
    );
    assert_eq!(
        convert(&inter, json!({ "a": "1", "b": "2", "c": "3" })),
        json!({ "a": 1, "b": "2", "c": 3 })
    );

    // Issue #1295 array conversion guard inside object/record union
    let guard_obj = tb::union([
        tb::object([
            ("type", tb::literal("A")),
            ("values", tb::union([tb::string(), tb::number()])),
        ]),
        tb::object([("type", tb::literal("B")), ("values", tb::string())]),
    ]);
    assert_eq!(
        convert(&guard_obj, json!([{ "type": "A", "values": 1 }])),
        json!([{ "type": "A", "values": 1 }])
    );

    let guard_rec = tb::union([
        tb::record(
            tb::string().pattern("^values$"),
            tb::union([tb::string(), tb::number()]),
        ),
        tb::record(tb::string().pattern("^type$"), tb::union([tb::string(), tb::number()])),
    ]);
    assert_eq!(
        convert(&guard_rec, json!([{ "type": "A", "values": 1 }])),
        json!([{ "type": "A", "values": 1 }])
    );
}
