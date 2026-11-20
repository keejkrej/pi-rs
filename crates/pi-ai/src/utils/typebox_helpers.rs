//! Compatibility re-export of `string_enum` returning legacy `TSchema`.
//!
//! PORT: `packages/ai/src/utils/typebox-helpers.ts` moved to `string-enum.ts` using `@earendil-works/pi-schema`.
//! Kept as a thin shim for downstream callers migrating from `TSchema`.

use pi_js::vendor::typebox::Schema as TSchema;

/// Legacy TypeBox option struct preserved for existing callers.
#[derive(Clone, Debug, Default)]
pub struct StringEnumOptions {
    pub description: Option<String>,
    /// TS `default`.
    ///
    /// PORT: `default` is a Rust keyword, so the field is `r#default`.
    pub r#default: Option<String>,
}

/// Builds `{ "type": "string", "enum": values, ... }` via `crate::utils::string_enum` and converts to `TSchema`.
pub fn string_enum<I, S>(values: I, options: Option<StringEnumOptions>) -> TSchema
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let schema_opts = options.map(|o| pi_schema::StringEnumOptions {
        description: o.description,
        default_value: o.r#default,
    });
    TSchema::from(crate::utils::string_enum::string_enum(values, schema_opts))
}
