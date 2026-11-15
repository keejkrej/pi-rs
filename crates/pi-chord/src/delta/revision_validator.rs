//! Port of packages/chord/src/delta/revision-validator.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexSet;

use crate::types::JsonValue;

/// Validates immutable replica revisions while skipping containers validated in earlier revisions.
#[derive(Clone)]
pub struct JsonRevisionValidator {
    inner: Arc<JsonRevisionValidatorInner>,
}

struct JsonRevisionValidatorInner {
    // PORT: JS `WeakSet<object>` tracks live object identity across revisions. In Rust, `JsonValue`
    // is moved by value and heap/stack addresses can be reused across calls after values are dropped,
    // so cross-call address caching would risk false hits; within-call DAG/cycle checks use `ancestors`
    // and `finished` while each revision value is borrowed live.
    _validated: Mutex<IndexSet<u64>>,
}

impl JsonRevisionValidator {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(JsonRevisionValidatorInner {
                _validated: Mutex::new(IndexSet::new()),
            }),
        }
    }

    pub fn validate<T: 'static>(&self, value: T) -> pi_js::Result<T> {
        let _lock = &self.inner._validated;
        if let Some(json) = crate::json::downcast_json_ref(&value) {
            let mut ancestors = IndexSet::new();
            let mut finished = IndexSet::new();
            validate_node(json, &mut ancestors, &mut finished)?;
            return Ok(value);
        }
        let cloned = crate::json::clone_as_json(&value)
            .map_err(|_| pi_js::Error::js("TypeError", "Replicated state values must be strict JSON"))?;
        let mut ancestors = IndexSet::new();
        let mut finished = IndexSet::new();
        validate_node(&cloned, &mut ancestors, &mut finished)?;
        Ok(value)
    }
}

impl Default for JsonRevisionValidator {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_node(value: &JsonValue, ancestors: &mut IndexSet<u64>, finished: &mut IndexSet<u64>) -> pi_js::Result<()> {
    if !is_container(value) {
        return assert_primitive(value);
    }
    let ptr = (value as *const JsonValue) as usize as u64;
    if finished.contains(&ptr) {
        return Ok(());
    }
    if !ancestors.insert(ptr) {
        return Err(pi_js::Error::js("TypeError", "Replicated state cannot contain cycles"));
    }
    let res = match value {
        JsonValue::Array(arr) => {
            assert_dense_array(value)?;
            for item in arr {
                validate_node(item, ancestors, finished)?;
            }
            Ok(())
        }
        JsonValue::Object(map) => {
            assert_plain_object(value)?;
            for entry in map.values() {
                validate_node(entry, ancestors, finished)?;
            }
            Ok(())
        }
        _ => unreachable!(),
    };
    ancestors.shift_remove(&ptr);
    if res.is_ok() {
        finished.insert(ptr);
    }
    res
}

fn is_container(value: &JsonValue) -> bool {
    matches!(value, JsonValue::Array(_) | JsonValue::Object(_))
}

fn assert_primitive(value: &JsonValue) -> pi_js::Result<()> {
    match value {
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::String(_) => Ok(()),
        JsonValue::Number(n) if n.as_f64().is_some_and(f64::is_finite) => Ok(()),
        _ => Err(pi_js::Error::js(
            "TypeError",
            "Replicated state values must be strict JSON",
        )),
    }
}

fn assert_plain_object(value: &JsonValue) -> pi_js::Result<()> {
    if matches!(value, JsonValue::Object(_)) {
        Ok(())
    } else {
        Err(pi_js::Error::js(
            "TypeError",
            "Replicated state containers must be plain objects or arrays",
        ))
    }
}

fn assert_dense_array(value: &JsonValue) -> pi_js::Result<()> {
    if matches!(value, JsonValue::Array(_)) {
        Ok(())
    } else {
        Err(pi_js::Error::js(
            "TypeError",
            "Replicated state arrays must be dense and contain only indexed entries",
        ))
    }
}
