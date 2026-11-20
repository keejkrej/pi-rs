//! TypeBox schema shape inspection helpers corresponding to `typebox-static.ts`.

use serde_json::Value;

/// The TypeBox kind of a schema, either read from a `"~kind"` marker or inferred from JSON Schema keywords.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeBoxKind {
    String,
    Number,
    Integer,
    Boolean,
    Null,
    BigInt,
    Undefined,
    Unknown,
    Any,
    Literal,
    Array,
    Object,
    Union,
    Record,
    Enum,
    Tuple,
    Intersect,
    Unsafe,
    UnknownKind,
}

/// Infers the [`TypeBoxKind`] from a TypeBox-built schema or plain JSON Schema object.
pub fn infer_typebox_kind(schema: &Value) -> TypeBoxKind {
    let Some(obj) = schema.as_object() else {
        return TypeBoxKind::UnknownKind;
    };
    if let Some(kind) = obj.get("~kind").and_then(Value::as_str) {
        return match kind {
            "String" => TypeBoxKind::String,
            "Number" => TypeBoxKind::Number,
            "Integer" => TypeBoxKind::Integer,
            "Boolean" => TypeBoxKind::Boolean,
            "Null" => TypeBoxKind::Null,
            "BigInt" => TypeBoxKind::BigInt,
            "Undefined" => TypeBoxKind::Undefined,
            "Unknown" => TypeBoxKind::Unknown,
            "Any" => TypeBoxKind::Any,
            "Literal" => TypeBoxKind::Literal,
            "Array" => TypeBoxKind::Array,
            "Object" => TypeBoxKind::Object,
            "Union" => TypeBoxKind::Union,
            "Record" => TypeBoxKind::Record,
            "Enum" => TypeBoxKind::Enum,
            "Tuple" => TypeBoxKind::Tuple,
            "Intersect" => TypeBoxKind::Intersect,
            _ => TypeBoxKind::UnknownKind,
        };
    }
    if obj.contains_key("~unsafe") {
        return TypeBoxKind::Unsafe;
    }
    if obj.contains_key("const") {
        return TypeBoxKind::Literal;
    }
    if obj.contains_key("enum") {
        return TypeBoxKind::Enum;
    }
    if obj.contains_key("anyOf") {
        return TypeBoxKind::Union;
    }
    if obj.contains_key("allOf") {
        return TypeBoxKind::Intersect;
    }
    match obj.get("type").and_then(Value::as_str) {
        Some("string") => TypeBoxKind::String,
        Some("number") => TypeBoxKind::Number,
        Some("integer") => TypeBoxKind::Integer,
        Some("boolean") => TypeBoxKind::Boolean,
        Some("null") => TypeBoxKind::Null,
        Some("bigint") => TypeBoxKind::BigInt,
        Some("undefined") => TypeBoxKind::Undefined,
        Some("array") => {
            if obj.get("prefixItems").is_some_and(Value::is_array) || obj.get("items").is_some_and(Value::is_array) {
                TypeBoxKind::Tuple
            } else {
                TypeBoxKind::Array
            }
        }
        Some("object") => {
            if obj.contains_key("patternProperties") && !obj.contains_key("properties") {
                TypeBoxKind::Record
            } else {
                TypeBoxKind::Object
            }
        }
        _ => TypeBoxKind::Unknown,
    }
}

/// Returns `true` when `pattern` is one of TypeBox's numeric record key patterns (`integer` or `number`).
pub fn is_numeric_record_pattern(pattern: &str) -> bool {
    matches!(pattern, "^-?(?:0|[1-9][0-9]*)$" | "^-?(?:0|[1-9][0-9]*)(?:\\.[0-9]+)?$")
}
