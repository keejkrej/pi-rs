//! Tests for TypeBox schema inspection helpers (`typebox_static.rs`).

use pi_schema::typebox_static::{TypeBoxKind, infer_typebox_kind, is_numeric_record_pattern};
use pi_schema::{CompileOptions, compile_coercing, t as tb};
use serde_json::json;

#[test]
fn infers_typebox_kinds_from_markers_and_json_schema() {
    assert_eq!(infer_typebox_kind(&json!({ "~kind": "String" })), TypeBoxKind::String);
    assert_eq!(infer_typebox_kind(&json!({ "~kind": "BigInt" })), TypeBoxKind::BigInt);
    assert_eq!(
        infer_typebox_kind(&json!({ "~kind": "Undefined" })),
        TypeBoxKind::Undefined
    );
    assert_eq!(infer_typebox_kind(&json!({ "~unsafe": true })), TypeBoxKind::Unsafe);

    assert_eq!(infer_typebox_kind(tb::string().as_value()), TypeBoxKind::String);
    assert_eq!(infer_typebox_kind(tb::number().as_value()), TypeBoxKind::Number);
    assert_eq!(infer_typebox_kind(tb::integer().as_value()), TypeBoxKind::Integer);
    assert_eq!(infer_typebox_kind(tb::boolean().as_value()), TypeBoxKind::Boolean);
    assert_eq!(infer_typebox_kind(tb::null().as_value()), TypeBoxKind::Null);
    assert_eq!(infer_typebox_kind(tb::literal("add").as_value()), TypeBoxKind::Literal);
    assert_eq!(infer_typebox_kind(tb::enum_(["a", "b"]).as_value()), TypeBoxKind::Enum);
    assert_eq!(
        infer_typebox_kind(tb::array(tb::string()).as_value()),
        TypeBoxKind::Array
    );
    assert_eq!(
        infer_typebox_kind(tb::tuple([tb::string(), tb::number()]).as_value()),
        TypeBoxKind::Tuple
    );
    assert_eq!(
        infer_typebox_kind(tb::object([("a", tb::string())]).as_value()),
        TypeBoxKind::Object
    );
    assert_eq!(
        infer_typebox_kind(tb::union([tb::string(), tb::null()]).as_value()),
        TypeBoxKind::Union
    );
    assert_eq!(
        infer_typebox_kind(tb::record(tb::integer(), tb::boolean()).as_value()),
        TypeBoxKind::Record
    );
    assert_eq!(
        infer_typebox_kind(tb::intersect([tb::object([("a", tb::string())])]).as_value()),
        TypeBoxKind::Intersect
    );
}

#[test]
fn numeric_record_pattern_and_coercing_compile() {
    assert!(is_numeric_record_pattern("^-?(?:0|[1-9][0-9]*)$"));
    assert!(is_numeric_record_pattern("^-?(?:0|[1-9][0-9]*)(?:\\.[0-9]+)?$"));
    assert!(!is_numeric_record_pattern("^.*$"));

    let schema = tb::object([("n", tb::number()), ("s", tb::string().optional())]);
    let v = compile_coercing(&schema, CompileOptions::default()).unwrap();
    assert_eq!(v.parse(&json!({ "n": "1" })), Ok(json!({ "n": 1 })));
}
