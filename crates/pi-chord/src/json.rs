//! Port of packages/chord/src/json.ts

#![allow(dead_code, unused_variables)]

use serde::{Deserialize, Serialize};

use crate::types::JsonValue;

// PORT: JS `DATA_DESCRIPTOR` is a reused property descriptor. Rust JSON values have no descriptors;
// `define_data` writes entries. Codecs call `pi_js::json` when the bodies are filled.

/// Omit undefined object properties while preserving strict array semantics.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyJsonOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omit_undefined_properties: Option<bool>,
}

/// Copy a value into an alias-free strict-JSON tree owned by the caller.
pub fn copy_json(value: &serde_json::Value, options: Option<CopyJsonOptions>) -> pi_js::Result<JsonValue> {
    todo!("port: copy_json")
}

fn copy(
    value: &serde_json::Value,
    ancestors: &mut indexmap::IndexSet<u64>,
    omit_undefined_properties: bool,
) -> pi_js::Result<JsonValue> {
    todo!("port: copy")
}

fn define_data(target: &mut JsonValue, key: &str, value: JsonValue) {
    todo!("port: define_data")
}

/// Return whether a value is finite strict JSON with plain objects and no cycles.
pub fn is_json_value(value: &serde_json::Value) -> bool {
    todo!("port: is_json_value")
}

fn check(value: &serde_json::Value, ancestors: &mut indexmap::IndexSet<u64>) -> bool {
    todo!("port: check")
}
