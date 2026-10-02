//! Port of packages/chord/src/delta/apply-immutable-trusted.ts

#![allow(dead_code, unused_variables)]

use indexmap::IndexSet;

use crate::delta::{Op, Path, Seg};
use crate::types::JsonValue;

// PORT: JS `DATA_DESCRIPTOR` is a reused property descriptor. Rust writes JSON entries directly.

struct CopiedPath {
    root: JsonValue,
    target: JsonValue,
}

/// Apply self-produced trusted operations while copying each touched container once.
pub fn apply_immutable_trusted<T>(target: T, operations: &[Op]) -> pi_js::Result<T> {
    todo!("port: apply_immutable_trusted")
}

fn copy_path(root: &JsonValue, path: &Path, length: i64, owned: &mut IndexSet<u64>) -> pi_js::Result<CopiedPath> {
    todo!("port: copy_path")
}

fn shallow_copy(value: &JsonValue) -> pi_js::Result<JsonValue> {
    todo!("port: shallow_copy")
}

fn read(target: &JsonValue, key: &Seg) -> JsonValue {
    todo!("port: read")
}

fn define_data(target: &mut JsonValue, key: &Seg, value: JsonValue) {
    todo!("port: define_data")
}

fn splice_trusted(target: &mut JsonValue, start: i64, remove: i64, items: &[JsonValue]) -> pi_js::Result<()> {
    todo!("port: splice_trusted")
}

fn permute_trusted(target: &mut JsonValue, permutation: &[i64]) -> pi_js::Result<()> {
    todo!("port: permute_trusted")
}

fn is_container(value: &JsonValue) -> bool {
    todo!("port: is_container")
}
