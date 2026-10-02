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
    todo!("port: string_enum")
}
