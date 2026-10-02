//! Port of packages/chord/src/delta/index.ts
#![allow(dead_code, unused_variables)]

// @generated-mods begin (scaffold-owned, do not edit)
pub mod apply_immutable_trusted;
pub mod diff;
pub mod draft;
pub mod revision_validator;
pub mod tracker;
// @generated-mods end

use std::sync::{Arc, Mutex};

use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};

// chord/delta — immutable revision tracking and operations over plain JSON.
//
// Depends on nothing else in the harness. Session storage, the runtime and the
// facet host consume it; keep the arrows pointing that way.

pub use crate::delta::diff::diff_revisions;
pub use crate::delta::draft::Draft;
pub use crate::delta::tracker::{Change, Prepared, Tracker, track};
pub use crate::types::JsonValue;

pub type Path = Vec<Seg>;
pub type NonEmptyPath = Vec<Seg>;

/// Path segment: an object key or a non-negative array index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Seg {
    String(String),
    Index(i64),
}

/// A path inline, or an id assigned by the encoder on second use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PathRef<P = Path> {
    Id(i64),
    Path(P),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplaceVerb {
    #[serde(rename = "r")]
    R,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetVerb {
    #[serde(rename = "s")]
    S,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeleteVerb {
    #[serde(rename = "d")]
    D,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppendVerb {
    #[serde(rename = "a")]
    A,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TruncateVerb {
    #[serde(rename = "t")]
    T,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpliceVerb {
    #[serde(rename = "p")]
    P,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MoveVerb {
    #[serde(rename = "m")]
    M,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DefineVerb {
    #[serde(rename = "#")]
    Define,
}

/// `["r", value]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpReplace(pub ReplaceVerb, pub JsonValue);

/// `["s", path, value]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpSet(pub SetVerb, pub NonEmptyPath, pub JsonValue);

/// `["d", path]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpDelete(pub DeleteVerb, pub NonEmptyPath);

/// `["a", path, text]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpAppend(pub AppendVerb, pub NonEmptyPath, pub String);

/// `["t", path, count]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpTruncate(pub TruncateVerb, pub NonEmptyPath, pub i64);

/// `["p", path, index, remove, items]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpSplice(pub SpliceVerb, pub Path, pub i64, pub i64, pub Vec<JsonValue>);

/// `["m", path, permutation]` where `new[i] = old[permutation[i]]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpMove(pub MoveVerb, pub Path, pub Vec<i64>);

/// Decoded operation. Tuples are the form in memory, on the wire, and on disk.
///
/// `r` is the only op that replaces a whole value. `s` / `d` / `a` / `t` cannot target the root.
/// `p` and `m` may, because a tracked value can itself be an array.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Op {
    Replace(OpReplace),
    Set(OpSet),
    Delete(OpDelete),
    Append(OpAppend),
    Truncate(OpTruncate),
    Splice(OpSplice),
    Move(OpMove),
}

/// `["s", value]` reuses the previous op's path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireOpSetShort(pub SetVerb, pub JsonValue);

/// `["d"]` reuses the previous op's path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireOpDeleteShort(pub DeleteVerb);

/// `["a", text]` reuses the previous op's path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireOpAppendShort(pub AppendVerb, pub String);

/// `["t", count]` reuses the previous op's path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireOpTruncateShort(pub TruncateVerb, pub i64);

/// `["p", index, remove, items]` reuses the previous op's path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireOpSpliceShort(pub SpliceVerb, pub i64, pub i64, pub Vec<JsonValue>);

/// `["m", permutation]` reuses the previous op's path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireOpMoveShort(pub MoveVerb, pub Vec<i64>);

/// `["s", pathRef, value]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireOpSet(pub SetVerb, pub PathRef<NonEmptyPath>, pub JsonValue);

/// `["d", pathRef]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireOpDelete(pub DeleteVerb, pub PathRef<NonEmptyPath>);

/// `["a", pathRef, text]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireOpAppend(pub AppendVerb, pub PathRef<NonEmptyPath>, pub String);

/// `["t", pathRef, count]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireOpTruncate(pub TruncateVerb, pub PathRef<NonEmptyPath>, pub i64);

/// `["p", pathRef, index, remove, items]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireOpSplice(pub SpliceVerb, pub PathRef, pub i64, pub i64, pub Vec<JsonValue>);

/// `["m", pathRef, permutation]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireOpMove(pub MoveVerb, pub PathRef, pub Vec<i64>);

/// `["#", id, path]` defines an id, emitted on a path's second use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireOpDefine(pub DefineVerb, pub i64, pub Path);

/// What crosses a boundary. Path interning and omitted paths exist only between encode and decode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireOp {
    Replace(OpReplace),
    Set(WireOpSet),
    SetShort(WireOpSetShort),
    Delete(WireOpDelete),
    DeleteShort(WireOpDeleteShort),
    Append(WireOpAppend),
    AppendShort(WireOpAppendShort),
    Truncate(WireOpTruncate),
    TruncateShort(WireOpTruncateShort),
    Splice(WireOpSplice),
    SpliceShort(WireOpSpliceShort),
    Move(WireOpMove),
    MoveShort(WireOpMoveShort),
    Define(WireOpDefine),
}

/// Verb of a decoded or wire op. Lets [`is_replace`] and [`is_base`] accept either vocabulary.
pub trait OpVerb {
    fn verb(&self) -> &'static str;
}

impl OpVerb for Op {
    fn verb(&self) -> &'static str {
        todo!("port: Op::verb")
    }
}

impl OpVerb for WireOp {
    fn verb(&self) -> &'static str {
        todo!("port: WireOp::verb")
    }
}

pub fn is_replace(op: &impl OpVerb) -> bool {
    todo!("port: is_replace")
}

/// A batch begins with a replacement. Flush guarantees `r` is at index 0 or absent.
pub fn is_base<T: OpVerb>(ops: &[T]) -> bool {
    todo!("port: is_base")
}

/// Longest suffix of `a` that is a prefix of `b`.
///
/// `probe` defaults to 64 and `max_candidates` defaults to 8 inside the body.
/// Giving up returns 0, which emits a set: larger, never wrong.
pub fn overlap(a: &str, b: &str, scan: i64, probe: Option<i64>, max_candidates: Option<i64>) -> i64 {
    todo!("port: overlap")
}

/// Segments that reach the prototype chain.
pub static RESERVED_SEGMENTS: std::sync::LazyLock<IndexSet<String>> = std::sync::LazyLock::new(|| {
    let mut segments = IndexSet::new();
    segments.insert("__proto__".to_owned());
    segments.insert("constructor".to_owned());
    segments.insert("prototype".to_owned());
    segments
});

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct UnsafePathError {
    pub segment: Seg,
    message: String,
}

impl UnsafePathError {
    pub fn new(segment: Seg) -> Self {
        todo!("port: UnsafePathError::new")
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct PathError {
    pub path: PathRef,
    message: String,
}

impl PathError {
    pub fn new(path: PathRef) -> Self {
        todo!("port: PathError::new")
    }
}

/// Verb, arity, and payload shape for a decoded op.
pub fn assert_valid_op(op: &serde_json::Value) -> pi_js::Result<()> {
    todo!("port: assert_valid_op")
}

/// The same, for the wire grammar: ids and short forms are legal here.
pub fn assert_valid_wire_op(op: &serde_json::Value) -> pi_js::Result<()> {
    todo!("port: assert_valid_wire_op")
}

pub fn assert_safe_path(path: &Path) -> pi_js::Result<()> {
    todo!("port: assert_safe_path")
}

/// Apply ops to a plain mutable value. Returns the value, because `r` replaces it outright.
pub fn apply<T>(target: Option<T>, ops: &[Op]) -> pi_js::Result<T> {
    todo!("port: apply")
}

/// Apply one decoded operation batch without mutating the previous immutable value.
pub fn apply_immutable<T>(target: Option<T>, ops: &[Op]) -> pi_js::Result<T> {
    todo!("port: apply_immutable")
}

/// Apply decoded operation batches as one final-result-only replay.
pub fn apply_immutable_batches<T, I, B>(target: Option<T>, batches: I) -> pi_js::Result<T>
where
    I: IntoIterator<Item = B>,
    B: AsRef<[Op]>,
{
    todo!("port: apply_immutable_batches")
}

pub struct Encoder {
    inner: Arc<EncoderState>,
}

struct EncoderState {
    seen: Mutex<IndexSet<String>>,
    ids: Mutex<IndexMap<String, i64>>,
    next_id: Mutex<i64>,
    previous: Mutex<Option<String>>,
}

impl Encoder {
    pub fn encode(&self, ops: &[Op]) -> Vec<WireOp> {
        todo!("port: Encoder::encode")
    }
}

/// Intern on second use. A definition costs more than the path it replaces.
pub fn encoder() -> Encoder {
    todo!("port: encoder")
}

pub struct Decoder {
    inner: Arc<DecoderState>,
}

struct DecoderState {
    paths: Mutex<IndexMap<i64, Path>>,
}

impl Decoder {
    pub fn decode(&self, wire: &[WireOp]) -> pi_js::Result<Vec<Op>> {
        todo!("port: Decoder::decode")
    }
}

pub fn decoder() -> Decoder {
    todo!("port: decoder")
}

fn is_obj(value: &JsonValue) -> bool {
    todo!("port: is_obj")
}

fn assert_path_arg(path: &serde_json::Value, non_empty: bool) -> pi_js::Result<()> {
    todo!("port: assert_path_arg")
}

fn assert_permutation(value: &serde_json::Value) -> pi_js::Result<()> {
    todo!("port: assert_permutation")
}

fn assert_index_in_range(parent_len: i64, index: i64) -> pi_js::Result<()> {
    todo!("port: assert_index_in_range")
}

fn apply_ops<T>(target: Option<T>, ops: &[Op]) -> pi_js::Result<T> {
    todo!("port: apply_ops")
}

fn copy_containers(root: JsonValue, path: &Path, owned: &mut IndexSet<u64>) -> pi_js::Result<JsonValue> {
    todo!("port: copy_containers")
}

fn resolve_value(root: &JsonValue, path: &Path) -> pi_js::Result<JsonValue> {
    todo!("port: resolve_value")
}

fn resolve(root: &JsonValue, path: &Path) -> pi_js::Result<JsonValue> {
    todo!("port: resolve")
}

fn path_key(path: &Path) -> String {
    todo!("port: path_key")
}
