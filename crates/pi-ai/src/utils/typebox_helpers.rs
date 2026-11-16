//! Port of packages/ai/src/utils/typebox-helpers.ts

#![allow(dead_code, unused_variables)]

use pi_js::vendor::typebox::Schema;

/// Named form of the optional options object `{ description?: string; default?: T[number] }`.
///
/// `None` passed to [`string_enum`] is the omitted argument. A `None` field is an omitted property.
/// A present string is still dropped from the schema when it is JS-falsy (`""`).
#[derive(Clone, Debug, Default)]
pub struct StringEnumOptions {
    pub description: Option<String>,
    /// TS `default`.
    ///
    /// PORT: `default` is a Rust keyword, so the field is `r#default`.
    pub r#default: Option<String>,
}

/// Creates a string enum schema compatible with Google's API and other providers
/// that don't support anyOf/const patterns.
///
/// @example
/// const OperationSchema = StringEnum(["add", "subtract", "multiply", "divide"], {
///   description: "The operation to perform"
/// });
///
/// type Operation = Static<typeof OperationSchema>; // "add" | "subtract" | "multiply" | "divide"
///
/// Returns `Type.Unsafe` (`t::unsafe_`) of `{ type: "string", enum: values }`, then
/// `description` and `default` in that order, each only when the option string is JS-truthy.
pub fn string_enum<I, S>(values: I, options: Option<StringEnumOptions>) -> Schema
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let enum_values: Vec<serde_json::Value> = values
        .into_iter()
        .map(|v| serde_json::Value::String(v.as_ref().to_string()))
        .collect();
    let mut obj = serde_json::Map::new();
    obj.insert("type".to_string(), serde_json::Value::String("string".to_string()));
    obj.insert("enum".to_string(), serde_json::Value::Array(enum_values));
    if let Some(opts) = options {
        if let Some(desc) = opts.description {
            if !desc.is_empty() {
                obj.insert("description".to_string(), serde_json::Value::String(desc));
            }
        }
        if let Some(default) = opts.r#default {
            if !default.is_empty() {
                obj.insert("default".to_string(), serde_json::Value::String(default));
            }
        }
    }
    pi_js::vendor::typebox::t::unsafe_(serde_json::Value::Object(obj))
}
