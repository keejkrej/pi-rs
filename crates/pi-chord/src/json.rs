//! Port of packages/chord/src/json.ts

use std::any::Any;

use indexmap::IndexSet;
use serde::{Deserialize, Serialize};

use crate::types::JsonValue;

// PORT: JS `DATA_DESCRIPTOR` is a reused property descriptor. Rust JSON values have no descriptors;
// `define_data` writes entries directly on maps/arrays.

/// Omit undefined object properties while preserving strict array semantics.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyJsonOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omit_undefined_properties: Option<bool>,
}

/// Copy a value into an alias-free strict-JSON tree owned by the caller.
pub fn copy_json(value: &serde_json::Value, options: Option<CopyJsonOptions>) -> pi_js::Result<JsonValue> {
    let omit_undefined_properties = options.and_then(|opts| opts.omit_undefined_properties).unwrap_or(false);
    let mut ancestors = IndexSet::new();
    copy(value, &mut ancestors, omit_undefined_properties)
}

fn copy(
    value: &serde_json::Value,
    ancestors: &mut IndexSet<u64>,
    omit_undefined_properties: bool,
) -> pi_js::Result<JsonValue> {
    match value {
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::String(_) => Ok(value.clone()),
        JsonValue::Number(n) => {
            if n.as_f64().is_some_and(f64::is_finite) {
                Ok(JsonValue::Number(n.clone()))
            } else {
                Err(pi_js::Error::js(
                    "TypeError",
                    "Value contains a non-finite number and is not strict JSON",
                ))
            }
        }
        JsonValue::Array(arr) => {
            let ptr = (value as *const JsonValue) as usize as u64;
            if !ancestors.insert(ptr) {
                return Err(pi_js::Error::js(
                    "TypeError",
                    "Value contains cycles and is not strict JSON",
                ));
            }
            let res = (|| {
                let mut result = JsonValue::Array(Vec::with_capacity(arr.len()));
                for (index, item) in arr.iter().enumerate() {
                    let copied = copy(item, ancestors, omit_undefined_properties)?;
                    define_data(&mut result, &index.to_string(), copied);
                }
                Ok(result)
            })();
            ancestors.shift_remove(&ptr);
            res
        }
        JsonValue::Object(map) => {
            let ptr = (value as *const JsonValue) as usize as u64;
            if !ancestors.insert(ptr) {
                return Err(pi_js::Error::js(
                    "TypeError",
                    "Value contains cycles and is not strict JSON",
                ));
            }
            let res = (|| {
                let mut result = JsonValue::Object(serde_json::Map::with_capacity(map.len()));
                for (key, entry) in map {
                    let copied = copy(entry, ancestors, omit_undefined_properties)?;
                    define_data(&mut result, key, copied);
                }
                Ok(result)
            })();
            ancestors.shift_remove(&ptr);
            res
        }
    }
}

fn define_data(target: &mut JsonValue, key: &str, value: JsonValue) {
    match target {
        JsonValue::Object(map) => {
            map.insert(key.to_owned(), value);
        }
        JsonValue::Array(arr) => {
            if let Ok(index) = key.parse::<usize>() {
                if index == arr.len() {
                    arr.push(value);
                } else if index < arr.len() {
                    arr[index] = value;
                }
            }
        }
        _ => {}
    }
}

/// Return whether a value is finite strict JSON with plain objects and no cycles.
pub fn is_json_value(value: &serde_json::Value) -> bool {
    let mut ancestors = IndexSet::new();
    check(value, &mut ancestors)
}

fn check(value: &serde_json::Value, ancestors: &mut IndexSet<u64>) -> bool {
    match value {
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::String(_) => true,
        JsonValue::Number(n) => n.as_f64().is_some_and(f64::is_finite),
        JsonValue::Array(arr) => {
            let ptr = (value as *const JsonValue) as usize as u64;
            if !ancestors.insert(ptr) {
                return false;
            }
            let ok = arr.iter().all(|item| check(item, ancestors));
            ancestors.shift_remove(&ptr);
            ok
        }
        JsonValue::Object(map) => {
            let ptr = (value as *const JsonValue) as usize as u64;
            if !ancestors.insert(ptr) {
                return false;
            }
            let ok = map.values().all(|entry| check(entry, ancestors));
            ancestors.shift_remove(&ptr);
            ok
        }
    }
}

/// Convert a supported `'static` strict-JSON representation (`JsonValue`, `Map`, `Vec<JsonValue>`) into `JsonValue`.
pub(crate) fn take_as_json<T: 'static>(value: T) -> pi_js::Result<JsonValue> {
    let boxed: Box<dyn Any> = Box::new(value);
    match boxed.downcast::<JsonValue>() {
        Ok(v) => Ok(*v),
        Err(boxed) => match boxed.downcast::<serde_json::Map<String, JsonValue>>() {
            Ok(map) => Ok(JsonValue::Object(*map)),
            Err(boxed) => match boxed.downcast::<Vec<JsonValue>>() {
                Ok(arr) => Ok(JsonValue::Array(*arr)),
                Err(_) => Err(pi_js::Error::js(
                    "TypeError",
                    "Value contains a non-JSON value; expected strict JSON",
                )),
            },
        },
    }
}

/// Borrow a `'static` representation as a `JsonValue` when it is already `JsonValue`.
pub(crate) fn downcast_json_ref<T: 'static>(value: &T) -> Option<&JsonValue> {
    (value as &dyn Any).downcast_ref::<JsonValue>()
}

/// Clone a `'static` strict-JSON representation (`JsonValue`, `Map`, `Vec<JsonValue>`) into an owned `JsonValue`.
pub(crate) fn clone_as_json<T: 'static>(value: &T) -> pi_js::Result<JsonValue> {
    let any = value as &dyn Any;
    if let Some(v) = any.downcast_ref::<JsonValue>() {
        return Ok(v.clone());
    }
    if let Some(map) = any.downcast_ref::<serde_json::Map<String, JsonValue>>() {
        return Ok(JsonValue::Object(map.clone()));
    }
    if let Some(arr) = any.downcast_ref::<Vec<JsonValue>>() {
        return Ok(JsonValue::Array(arr.clone()));
    }
    Err(pi_js::Error::js(
        "TypeError",
        "Value contains a non-JSON value; expected strict JSON",
    ))
}

/// Convert a `JsonValue` back into the caller's `'static` representation (`JsonValue`, `Map`, `Vec<JsonValue>`).
pub(crate) fn from_json<T: 'static>(value: JsonValue) -> pi_js::Result<T> {
    let type_id = std::any::TypeId::of::<T>();
    if type_id == std::any::TypeId::of::<JsonValue>() {
        let boxed: Box<dyn Any> = Box::new(value);
        return boxed
            .downcast::<T>()
            .map(|b| *b)
            .map_err(|_| pi_js::Error::js("TypeError", "Failed to convert JsonValue"));
    }
    if type_id == std::any::TypeId::of::<serde_json::Map<String, JsonValue>>() {
        if let JsonValue::Object(map) = value {
            let boxed: Box<dyn Any> = Box::new(map);
            return boxed
                .downcast::<T>()
                .map(|b| *b)
                .map_err(|_| pi_js::Error::js("TypeError", "Failed to convert Map"));
        }
        return Err(pi_js::Error::js(
            "TypeError",
            "Expected JSON object for Map representation",
        ));
    }
    if type_id == std::any::TypeId::of::<Vec<JsonValue>>() {
        if let JsonValue::Array(arr) = value {
            let boxed: Box<dyn Any> = Box::new(arr);
            return boxed
                .downcast::<T>()
                .map(|b| *b)
                .map_err(|_| pi_js::Error::js("TypeError", "Failed to convert Vec<JsonValue>"));
        }
        return Err(pi_js::Error::js(
            "TypeError",
            "Expected JSON array for Vec<JsonValue> representation",
        ));
    }
    Err(pi_js::Error::js(
        "TypeError",
        "Unsupported non-JSON state type; expected JsonValue",
    ))
}
