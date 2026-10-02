//! Port of packages/chord/src/delta/revision-validator.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use indexmap::IndexSet;

use crate::types::JsonValue;

/// Validates immutable replica revisions while skipping containers validated in earlier revisions.
#[derive(Clone)]
pub struct JsonRevisionValidator {
    inner: Arc<JsonRevisionValidatorInner>,
}

struct JsonRevisionValidatorInner {
    // PORT: JS `WeakSet<object>` has no Rust equivalent. `validate` records container identity here.
    validated: Mutex<IndexSet<u64>>,
}

impl JsonRevisionValidator {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(JsonRevisionValidatorInner {
                validated: Mutex::new(IndexSet::new()),
            }),
        }
    }

    pub fn validate<T>(&self, value: T) -> pi_js::Result<T> {
        todo!("port: JsonRevisionValidator::validate")
    }
}

impl Default for JsonRevisionValidator {
    fn default() -> Self {
        Self::new()
    }
}

fn is_container(value: &JsonValue) -> bool {
    todo!("port: is_container")
}

fn assert_primitive(value: &JsonValue) -> pi_js::Result<()> {
    todo!("port: assert_primitive")
}

fn assert_plain_object(value: &JsonValue) -> pi_js::Result<()> {
    todo!("port: assert_plain_object")
}

fn assert_dense_array(value: &JsonValue) -> pi_js::Result<()> {
    todo!("port: assert_dense_array")
}
