//! Port of packages/coding-agent/src/core/extensions/pi-ai-extension-view.ts
//!
//! The `@earendil-works/pi-ai` surface exposed to extensions: `pi-ai`'s compat
//! entrypoint plus TypeBox's `Type` (`t`) and `TSchema`, and `StringEnum` so
//! extensions using it inside `t::object` continue to work unchanged.

use serde_json::{Map, Value};

pub use pi_ai::compat::*;
pub use pi_js::vendor::typebox::{Schema as TSchema, t as Type};

/// Options for [`string_enum`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StringEnumOptions {
    pub description: Option<String>,
    pub default: Option<String>,
}

/// Creates a string enum schema compatible with Google's API and other providers
/// that do not support `anyOf`/`const` patterns: `{ "type": "string", "enum": [...] }`.
pub fn string_enum(values: &[&str], options: Option<StringEnumOptions>) -> TSchema {
    let mut obj = Map::new();
    obj.insert("type".to_string(), Value::String("string".to_string()));
    obj.insert(
        "enum".to_string(),
        Value::Array(values.iter().map(|v| Value::String((*v).to_string())).collect()),
    );
    if let Some(opts) = options {
        if let Some(desc) = opts.description
            && !desc.is_empty()
        {
            obj.insert("description".to_string(), Value::String(desc));
        }
        if let Some(def) = opts.default
            && !def.is_empty()
        {
            obj.insert("default".to_string(), Value::String(def));
        }
    }
    Type::unsafe_(Value::Object(obj))
}
