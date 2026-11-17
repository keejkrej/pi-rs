//! Port of packages/chord/src/delta/index.ts

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
        match self {
            Op::Replace(_) => "r",
            Op::Set(_) => "s",
            Op::Delete(_) => "d",
            Op::Append(_) => "a",
            Op::Truncate(_) => "t",
            Op::Splice(_) => "p",
            Op::Move(_) => "m",
        }
    }
}

impl OpVerb for WireOp {
    fn verb(&self) -> &'static str {
        match self {
            WireOp::Replace(_) => "r",
            WireOp::Set(_) | WireOp::SetShort(_) => "s",
            WireOp::Delete(_) | WireOp::DeleteShort(_) => "d",
            WireOp::Append(_) | WireOp::AppendShort(_) => "a",
            WireOp::Truncate(_) | WireOp::TruncateShort(_) => "t",
            WireOp::Splice(_) | WireOp::SpliceShort(_) => "p",
            WireOp::Move(_) | WireOp::MoveShort(_) => "m",
            WireOp::Define(_) => "#",
        }
    }
}

pub fn is_replace(op: &impl OpVerb) -> bool {
    op.verb() == "r"
}

/// A batch begins with a replacement. Flush guarantees `r` is at index 0 or absent.
pub fn is_base<T: OpVerb>(ops: &[T]) -> bool {
    ops.first().is_some_and(|op| op.verb() == "r")
}

/// Longest suffix of `a` that is a prefix of `b`.
///
/// `probe` defaults to 64 and `max_candidates` defaults to 8 inside the body.
/// Giving up returns 0, which emits a set: larger, never wrong.
pub fn overlap(a: &str, b: &str, scan: i64, probe: Option<i64>, max_candidates: Option<i64>) -> i64 {
    if a.is_empty() || b.is_empty() || scan == 0 {
        return 0;
    }
    let probe = probe.unwrap_or(64);
    let max_candidates = max_candidates.unwrap_or(8);

    if a.is_ascii() && b.is_ascii() {
        if scan < 0 {
            return 0;
        }
        let tail_start = if (a.len() as i64) > scan {
            a.len() - (scan as usize)
        } else {
            0
        };
        let tail = &a.as_bytes()[tail_start..];
        let b_bytes = b.as_bytes();
        let b_len = b_bytes.len();
        let h_first = probe.min(b_len as i64);
        for h in [h_first, 1] {
            if h <= 0 {
                if h == 1 {
                    break;
                }
                continue;
            }
            let h_usize = h as usize;
            let head = &b_bytes[..h_usize];
            let mut tried = 0i64;
            let mut pos = 0usize;
            while pos + h_usize <= tail.len() {
                let Some(rel) = tail[pos..].windows(h_usize).position(|w| w == head) else {
                    break;
                };
                let k = pos + rel;
                tried += 1;
                if tried > max_candidates {
                    break;
                }
                let n = tail.len() - k;
                if n <= b_len && &tail[k..] == &b_bytes[..n] {
                    return n as i64;
                }
                pos = k + 1;
            }
            if h == 1 {
                break;
            }
        }
        return 0;
    }

    if scan < 0 {
        return 0;
    }
    let a_len = pi_js::str16::len(a) as i64;
    let b_len = pi_js::str16::len(b) as i64;
    if a_len == 0 || b_len == 0 {
        return 0;
    }
    let tail_owned;
    let tail: &str = if a_len > scan {
        tail_owned = pi_js::str16::slice(a, a_len - scan, None);
        &tail_owned
    } else {
        a
    };
    let tail_len = pi_js::str16::len(tail) as i64;
    let h_first = probe.min(b_len);
    for h in [h_first, 1] {
        if h <= 0 {
            if h == 1 {
                break;
            }
            continue;
        }
        let head = pi_js::str16::slice(b, 0, Some(h));
        let mut tried = 0i64;
        let mut from = 0usize;
        while let Some(k_usize) = pi_js::str16::index_of(tail, &head, from) {
            tried += 1;
            if tried > max_candidates {
                break;
            }
            let k = k_usize as i64;
            let n = tail_len - k;
            if n <= b_len && pi_js::str16::slice(tail, k, None) == pi_js::str16::slice(b, 0, Some(n)) {
                return n;
            }
            from = k_usize + 1;
        }
        if h == 1 {
            break;
        }
    }
    0
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
        let label = match &segment {
            Seg::String(s) => s.clone(),
            Seg::Index(i) => i.to_string(),
        };
        Self {
            segment,
            message: format!("unsafe path segment: {label}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct PathError {
    pub path: PathRef,
    message: String,
}

impl PathError {
    pub fn new(path: PathRef) -> Self {
        let formatted = pi_js::json::stringify(&path);
        Self {
            path,
            message: format!("unresolvable path: {formatted}"),
        }
    }
}

fn non_neg_int_i64(value: &serde_json::Value) -> Option<i64> {
    let n = value.as_number()?;
    if let Some(i) = n.as_i64() {
        return (i >= 0).then_some(i);
    }
    if let Some(u) = n.as_u64() {
        return i64::try_from(u).ok();
    }
    if let Some(f) = n.as_f64() {
        if f.is_finite() && f >= 0.0 && f.fract() == 0.0 && f <= i64::MAX as f64 {
            return Some(f as i64);
        }
    }
    None
}

fn js_value_label(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_owned(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                pi_js::json::number_to_string(f)
            } else {
                n.to_string()
            }
        }
        serde_json::Value::String(s) => s.clone(),
        _ => pi_js::json::stringify(value),
    }
}

fn ok_wire_ref(r: &serde_json::Value) -> pi_js::Result<()> {
    if r.is_number() {
        if non_neg_int_i64(r).is_none() {
            return Err(pi_js::Error::js("TypeError", "bad path id"));
        }
        return Ok(());
    }
    if !r.is_array() {
        return Err(pi_js::Error::js("TypeError", "path is not an array"));
    }
    assert_json_path_segments(r)
}

fn assert_json_path_segments(path: &serde_json::Value) -> pi_js::Result<()> {
    let Some(items) = path.as_array() else {
        return Err(pi_js::Error::js("TypeError", "path is not an array"));
    };
    let mut parsed = Vec::with_capacity(items.len());
    for item in items {
        match item {
            serde_json::Value::String(s) => parsed.push(Seg::String(s.clone())),
            serde_json::Value::Number(_) => {
                if let Some(idx) = non_neg_int_i64(item) {
                    parsed.push(Seg::Index(idx));
                } else {
                    return Err(pi_js::Error::typed(
                        "UnsafePathError",
                        UnsafePathError::new(Seg::String(js_value_label(item))),
                    ));
                }
            }
            _ => {
                return Err(pi_js::Error::typed(
                    "UnsafePathError",
                    UnsafePathError::new(Seg::String(js_value_label(item))),
                ));
            }
        }
    }
    assert_safe_path(&parsed)
}

/// Verb, arity, and payload shape for a decoded op.
pub fn assert_valid_op(op: &serde_json::Value) -> pi_js::Result<()> {
    let Some(items) = op.as_array() else {
        return Err(pi_js::Error::js("TypeError", "op is not a tuple"));
    };
    if items.is_empty() {
        return Err(pi_js::Error::js("TypeError", "op is not a tuple"));
    }
    match items[0].as_str() {
        Some("r") => {
            if items.len() != 2 {
                return Err(pi_js::Error::js("TypeError", "r arity"));
            }
            Ok(())
        }
        Some("s") => {
            if items.len() != 3 {
                return Err(pi_js::Error::js("TypeError", "s arity"));
            }
            assert_path_arg(&items[1], true)
        }
        Some("d") => {
            if items.len() != 2 {
                return Err(pi_js::Error::js("TypeError", "d arity"));
            }
            assert_path_arg(&items[1], true)
        }
        Some("a") => {
            if items.len() != 3 || !items[2].is_string() {
                return Err(pi_js::Error::js("TypeError", "a shape"));
            }
            assert_path_arg(&items[1], true)
        }
        Some("t") => {
            if items.len() != 3 || non_neg_int_i64(&items[2]).is_none() {
                return Err(pi_js::Error::js("TypeError", "t shape"));
            }
            assert_path_arg(&items[1], true)
        }
        Some("p") => {
            if items.len() != 5 {
                return Err(pi_js::Error::js("TypeError", "p arity"));
            }
            assert_path_arg(&items[1], false)?;
            if non_neg_int_i64(&items[2]).is_none() {
                return Err(pi_js::Error::js("TypeError", "p index"));
            }
            if non_neg_int_i64(&items[3]).is_none() {
                return Err(pi_js::Error::js("TypeError", "p remove"));
            }
            if !items[4].is_array() {
                return Err(pi_js::Error::js("TypeError", "p items"));
            }
            Ok(())
        }
        Some("m") => {
            if items.len() != 3 {
                return Err(pi_js::Error::js("TypeError", "m arity"));
            }
            assert_path_arg(&items[1], false)?;
            assert_permutation(&items[2])
        }
        _ => Err(pi_js::Error::js(
            "TypeError",
            format!("unknown op verb: {}", js_value_label(&items[0])),
        )),
    }
}

/// The same, for the wire grammar: ids and short forms are legal here.
pub fn assert_valid_wire_op(op: &serde_json::Value) -> pi_js::Result<()> {
    let Some(items) = op.as_array() else {
        return Err(pi_js::Error::js("TypeError", "op is not a tuple"));
    };
    if items.is_empty() {
        return Err(pi_js::Error::js("TypeError", "op is not a tuple"));
    }
    match items[0].as_str() {
        Some("r") => {
            if items.len() != 2 {
                return Err(pi_js::Error::js("TypeError", "r arity"));
            }
            Ok(())
        }
        Some("s") => {
            if items.len() == 3 {
                ok_wire_ref(&items[1])
            } else if items.len() == 2 {
                Ok(())
            } else {
                Err(pi_js::Error::js("TypeError", "s arity"))
            }
        }
        Some("d") => {
            if items.len() == 2 {
                ok_wire_ref(&items[1])
            } else if items.len() == 1 {
                Ok(())
            } else {
                Err(pi_js::Error::js("TypeError", "d arity"))
            }
        }
        Some("a") => {
            if items.len() == 3 {
                ok_wire_ref(&items[1])?;
                if !items[2].is_string() {
                    return Err(pi_js::Error::js("TypeError", "a value"));
                }
                Ok(())
            } else if items.len() == 2 {
                if !items[1].is_string() {
                    return Err(pi_js::Error::js("TypeError", "a value"));
                }
                Ok(())
            } else {
                Err(pi_js::Error::js("TypeError", "a arity"))
            }
        }
        Some("t") => {
            if items.len() == 3 {
                ok_wire_ref(&items[1])?;
                if non_neg_int_i64(&items[2]).is_none() {
                    return Err(pi_js::Error::js("TypeError", "t count"));
                }
                Ok(())
            } else if items.len() == 2 {
                if non_neg_int_i64(&items[1]).is_none() {
                    return Err(pi_js::Error::js("TypeError", "t count"));
                }
                Ok(())
            } else {
                Err(pi_js::Error::js("TypeError", "t arity"))
            }
        }
        Some("p") => {
            let (i_val, r_val, items_val) = if items.len() == 5 {
                (&items[2], &items[3], &items[4])
            } else if items.len() == 4 {
                (&items[1], &items[2], &items[3])
            } else {
                return Err(pi_js::Error::js("TypeError", "p arity"));
            };
            if items.len() == 5 {
                ok_wire_ref(&items[1])?;
            }
            if non_neg_int_i64(i_val).is_none() {
                return Err(pi_js::Error::js("TypeError", "p index"));
            }
            if non_neg_int_i64(r_val).is_none() {
                return Err(pi_js::Error::js("TypeError", "p remove"));
            }
            if !items_val.is_array() {
                return Err(pi_js::Error::js("TypeError", "p items"));
            }
            Ok(())
        }
        Some("m") => {
            if items.len() == 3 {
                ok_wire_ref(&items[1])?;
            } else if items.len() != 2 {
                return Err(pi_js::Error::js("TypeError", "m arity"));
            }
            assert_permutation(items.last().expect("non-empty tuple"))
        }
        Some("#") => {
            if items.len() != 3 || non_neg_int_i64(&items[1]).is_none() || !items[2].is_array() {
                return Err(pi_js::Error::js("TypeError", "# shape"));
            }
            assert_json_path_segments(&items[2])
        }
        _ => Err(pi_js::Error::js(
            "TypeError",
            format!("unknown op verb: {}", js_value_label(&items[0])),
        )),
    }
}

pub fn assert_safe_path(path: &Path) -> pi_js::Result<()> {
    for seg in path {
        match seg {
            Seg::String(s) => {
                if RESERVED_SEGMENTS.contains(s.as_str()) {
                    return Err(pi_js::Error::typed(
                        "UnsafePathError",
                        UnsafePathError::new(Seg::String(s.clone())),
                    ));
                }
            }
            Seg::Index(i) => {
                if *i < 0 {
                    return Err(pi_js::Error::typed(
                        "UnsafePathError",
                        UnsafePathError::new(Seg::Index(*i)),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Apply ops to a plain mutable value. Returns the value, because `r` replaces it outright.
pub fn apply<T: 'static>(target: Option<T>, ops: &[Op]) -> pi_js::Result<T> {
    apply_ops(target, ops)
}

/// Apply one decoded operation batch without mutating the previous immutable value.
pub fn apply_immutable<T: 'static>(target: Option<T>, ops: &[Op]) -> pi_js::Result<T> {
    apply_immutable_batches(target, [ops])
}

/// Apply decoded operation batches as one final-result-only replay.
pub fn apply_immutable_batches<T, I, B>(target: Option<T>, batches: I) -> pi_js::Result<T>
where
    T: 'static,
    I: IntoIterator<Item = B>,
    B: AsRef<[Op]>,
{
    let mut root: Option<JsonValue> = match target {
        Some(value) => Some(crate::json::take_as_json(value)?),
        None => None,
    };
    let mut owned = IndexSet::new();
    for batch in batches {
        for op in batch.as_ref() {
            validate_typed_op(op)?;
            if let Op::Replace(OpReplace(_, value)) = op {
                root = Some(value.clone());
                continue;
            }
            let prefix = match op {
                Op::Splice(OpSplice(_, path, _, _, _)) | Op::Move(OpMove(_, path, _)) => path.as_slice(),
                Op::Set(OpSet(_, path, _))
                | Op::Delete(OpDelete(_, path))
                | Op::Append(OpAppend(_, path, _))
                | Op::Truncate(OpTruncate(_, path, _)) => &path[..path.len() - 1],
                Op::Replace(_) => unreachable!(),
            };
            let current_root = root.take().ok_or_else(|| {
                let err_path = match op {
                    Op::Splice(OpSplice(_, p, _, _, _))
                    | Op::Move(OpMove(_, p, _))
                    | Op::Set(OpSet(_, p, _))
                    | Op::Delete(OpDelete(_, p))
                    | Op::Append(OpAppend(_, p, _))
                    | Op::Truncate(OpTruncate(_, p, _)) => p.clone(),
                    Op::Replace(_) => Vec::new(),
                };
                pi_js::Error::typed("PathError", PathError::new(PathRef::Path(err_path)))
            })?;
            let prefix_vec = prefix.to_vec();
            let copied_root = copy_containers(current_root, &prefix_vec, &mut owned)?;
            let next_root = apply_single_json_op(copied_root, op)?;
            root = Some(next_root);
        }
    }
    let final_root = root.ok_or_else(|| {
        pi_js::Error::js(
            "TypeError",
            "Cannot apply immutable batches without initial target or replacement op",
        )
    })?;
    crate::json::from_json(final_root)
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
        let mut seen = self.inner.seen.lock().unwrap_or_else(|e| e.into_inner());
        let mut ids = self.inner.ids.lock().unwrap_or_else(|e| e.into_inner());
        let mut next_id = self.inner.next_id.lock().unwrap_or_else(|e| e.into_inner());
        let mut previous = self.inner.previous.lock().unwrap_or_else(|e| e.into_inner());

        *previous = None;
        let mut out = Vec::with_capacity(ops.len());
        for op in ops {
            if let Op::Replace(OpReplace(_, value)) = op {
                out.push(WireOp::Replace(OpReplace(ReplaceVerb::R, value.clone())));
                seen.clear();
                ids.clear();
                *next_id = 0;
                *previous = None;
                continue;
            }
            let path: &Path = match op {
                Op::Set(OpSet(_, p, _))
                | Op::Delete(OpDelete(_, p))
                | Op::Append(OpAppend(_, p, _))
                | Op::Truncate(OpTruncate(_, p, _))
                | Op::Splice(OpSplice(_, p, _, _, _))
                | Op::Move(OpMove(_, p, _)) => p,
                Op::Replace(_) => unreachable!(),
            };
            let key = path_key(path);

            if previous.as_deref() == Some(key.as_str()) {
                match op {
                    Op::Set(OpSet(_, _, value)) => {
                        out.push(WireOp::SetShort(WireOpSetShort(SetVerb::S, value.clone())));
                    }
                    Op::Delete(_) => {
                        out.push(WireOp::DeleteShort(WireOpDeleteShort(DeleteVerb::D)));
                    }
                    Op::Append(OpAppend(_, _, text)) => {
                        out.push(WireOp::AppendShort(WireOpAppendShort(AppendVerb::A, text.clone())));
                    }
                    Op::Truncate(OpTruncate(_, _, count)) => {
                        out.push(WireOp::TruncateShort(WireOpTruncateShort(TruncateVerb::T, *count)));
                    }
                    Op::Splice(OpSplice(_, _, index, remove, items)) => {
                        out.push(WireOp::SpliceShort(WireOpSpliceShort(
                            SpliceVerb::P,
                            *index,
                            *remove,
                            items.clone(),
                        )));
                    }
                    Op::Move(OpMove(_, _, permutation)) => {
                        out.push(WireOp::MoveShort(WireOpMoveShort(MoveVerb::M, permutation.clone())));
                    }
                    Op::Replace(_) => unreachable!(),
                }
                continue;
            }

            let path_ref = if let Some(&existing) = ids.get(&key) {
                PathRef::Id(existing)
            } else if seen.contains(&key) {
                let id = *next_id;
                *next_id += 1;
                ids.insert(key.clone(), id);
                out.push(WireOp::Define(WireOpDefine(DefineVerb::Define, id, path.clone())));
                PathRef::Id(id)
            } else {
                seen.insert(key.clone());
                PathRef::Path(path.clone())
            };

            match op {
                Op::Set(OpSet(_, _, value)) => {
                    out.push(WireOp::Set(WireOpSet(SetVerb::S, path_ref, value.clone())));
                }
                Op::Delete(_) => {
                    out.push(WireOp::Delete(WireOpDelete(DeleteVerb::D, path_ref)));
                }
                Op::Append(OpAppend(_, _, text)) => {
                    out.push(WireOp::Append(WireOpAppend(AppendVerb::A, path_ref, text.clone())));
                }
                Op::Truncate(OpTruncate(_, _, count)) => {
                    out.push(WireOp::Truncate(WireOpTruncate(TruncateVerb::T, path_ref, *count)));
                }
                Op::Splice(OpSplice(_, _, index, remove, items)) => {
                    out.push(WireOp::Splice(WireOpSplice(
                        SpliceVerb::P,
                        path_ref,
                        *index,
                        *remove,
                        items.clone(),
                    )));
                }
                Op::Move(OpMove(_, _, permutation)) => {
                    out.push(WireOp::Move(WireOpMove(MoveVerb::M, path_ref, permutation.clone())));
                }
                Op::Replace(_) => unreachable!(),
            }
            *previous = Some(key);
        }
        out
    }
}

/// Intern on second use. A definition costs more than the path it replaces.
pub fn encoder() -> Encoder {
    Encoder {
        inner: Arc::new(EncoderState {
            seen: Mutex::new(IndexSet::new()),
            ids: Mutex::new(IndexMap::new()),
            next_id: Mutex::new(0),
            previous: Mutex::new(None),
        }),
    }
}

pub struct Decoder {
    inner: Arc<DecoderState>,
}

struct DecoderState {
    paths: Mutex<IndexMap<i64, Path>>,
}

impl Decoder {
    pub fn decode(&self, wire: &[WireOp]) -> pi_js::Result<Vec<Op>> {
        let mut paths = self.inner.paths.lock().unwrap_or_else(|e| e.into_inner());
        let mut previous: Option<Path> = None;
        let mut out = Vec::with_capacity(wire.len());
        for wop in wire {
            validate_typed_wire_op(wop)?;
            match wop {
                WireOp::Define(WireOpDefine(_, id, path)) => {
                    assert_safe_path(path)?;
                    paths.insert(*id, path.clone());
                }
                WireOp::Replace(OpReplace(_, value)) => {
                    out.push(Op::Replace(OpReplace(ReplaceVerb::R, value.clone())));
                    paths.clear();
                    previous = None;
                }
                WireOp::SetShort(WireOpSetShort(_, value)) => {
                    let path = require_previous_path(&previous, true)?;
                    out.push(Op::Set(OpSet(SetVerb::S, path, value.clone())));
                }
                WireOp::DeleteShort(_) => {
                    let path = require_previous_path(&previous, true)?;
                    out.push(Op::Delete(OpDelete(DeleteVerb::D, path)));
                }
                WireOp::AppendShort(WireOpAppendShort(_, text)) => {
                    let path = require_previous_path(&previous, true)?;
                    out.push(Op::Append(OpAppend(AppendVerb::A, path, text.clone())));
                }
                WireOp::TruncateShort(WireOpTruncateShort(_, count)) => {
                    let path = require_previous_path(&previous, true)?;
                    out.push(Op::Truncate(OpTruncate(TruncateVerb::T, path, *count)));
                }
                WireOp::SpliceShort(WireOpSpliceShort(_, index, remove, items)) => {
                    let path = require_previous_path(&previous, false)?;
                    out.push(Op::Splice(OpSplice(
                        SpliceVerb::P,
                        path,
                        *index,
                        *remove,
                        items.clone(),
                    )));
                }
                WireOp::MoveShort(WireOpMoveShort(_, permutation)) => {
                    let path = require_previous_path(&previous, false)?;
                    out.push(Op::Move(OpMove(MoveVerb::M, path, permutation.clone())));
                }
                WireOp::Set(WireOpSet(_, path_ref, value)) => {
                    let path = resolve_wire_path(&paths, path_ref, true)?;
                    previous = Some(path.clone());
                    out.push(Op::Set(OpSet(SetVerb::S, path, value.clone())));
                }
                WireOp::Delete(WireOpDelete(_, path_ref)) => {
                    let path = resolve_wire_path(&paths, path_ref, true)?;
                    previous = Some(path.clone());
                    out.push(Op::Delete(OpDelete(DeleteVerb::D, path)));
                }
                WireOp::Append(WireOpAppend(_, path_ref, text)) => {
                    let path = resolve_wire_path(&paths, path_ref, true)?;
                    previous = Some(path.clone());
                    out.push(Op::Append(OpAppend(AppendVerb::A, path, text.clone())));
                }
                WireOp::Truncate(WireOpTruncate(_, path_ref, count)) => {
                    let path = resolve_wire_path(&paths, path_ref, true)?;
                    previous = Some(path.clone());
                    out.push(Op::Truncate(OpTruncate(TruncateVerb::T, path, *count)));
                }
                WireOp::Splice(WireOpSplice(_, path_ref, index, remove, items)) => {
                    let path = resolve_wire_path(&paths, path_ref, false)?;
                    previous = Some(path.clone());
                    out.push(Op::Splice(OpSplice(
                        SpliceVerb::P,
                        path,
                        *index,
                        *remove,
                        items.clone(),
                    )));
                }
                WireOp::Move(WireOpMove(_, path_ref, permutation)) => {
                    let path = resolve_wire_path(&paths, path_ref, false)?;
                    previous = Some(path.clone());
                    out.push(Op::Move(OpMove(MoveVerb::M, path, permutation.clone())));
                }
            }
        }
        Ok(out)
    }
}

pub fn decoder() -> Decoder {
    Decoder {
        inner: Arc::new(DecoderState {
            paths: Mutex::new(IndexMap::new()),
        }),
    }
}

fn require_previous_path(previous: &Option<Path>, require_non_empty: bool) -> pi_js::Result<Path> {
    let Some(path) = previous.as_ref() else {
        return Err(pi_js::Error::typed(
            "PathError",
            PathError::new(PathRef::Path(Vec::new())),
        ));
    };
    if require_non_empty && path.is_empty() {
        return Err(pi_js::Error::typed(
            "PathError",
            PathError::new(PathRef::Path(path.clone())),
        ));
    }
    Ok(path.clone())
}

fn resolve_wire_path(paths: &IndexMap<i64, Path>, path_ref: &PathRef, require_non_empty: bool) -> pi_js::Result<Path> {
    let path = match path_ref {
        PathRef::Id(id) => paths
            .get(id)
            .cloned()
            .ok_or_else(|| pi_js::Error::typed("PathError", PathError::new(PathRef::Id(*id))))?,
        PathRef::Path(path) => path.clone(),
    };
    if require_non_empty && path.is_empty() {
        return Err(pi_js::Error::typed("PathError", PathError::new(PathRef::Path(path))));
    }
    Ok(path)
}

fn validate_typed_op(op: &Op) -> pi_js::Result<()> {
    match op {
        Op::Replace(_) => Ok(()),
        Op::Set(OpSet(_, path, _)) | Op::Delete(OpDelete(_, path)) | Op::Append(OpAppend(_, path, _)) => {
            if path.is_empty() {
                return Err(pi_js::Error::js("TypeError", "path is empty"));
            }
            assert_safe_path(path)
        }
        Op::Truncate(OpTruncate(_, path, count)) => {
            if *count < 0 {
                return Err(pi_js::Error::js("TypeError", "t shape"));
            }
            if path.is_empty() {
                return Err(pi_js::Error::js("TypeError", "path is empty"));
            }
            assert_safe_path(path)
        }
        Op::Splice(OpSplice(_, path, index, remove, _)) => {
            assert_safe_path(path)?;
            if *index < 0 {
                return Err(pi_js::Error::js("TypeError", "p index"));
            }
            if *remove < 0 {
                return Err(pi_js::Error::js("TypeError", "p remove"));
            }
            Ok(())
        }
        Op::Move(OpMove(_, path, perm)) => {
            assert_safe_path(path)?;
            assert_typed_permutation(perm)
        }
    }
}

fn validate_typed_wire_op(wop: &WireOp) -> pi_js::Result<()> {
    let ok_typed_ref = |r: &PathRef| -> pi_js::Result<()> {
        match r {
            PathRef::Id(id) => {
                if *id < 0 {
                    return Err(pi_js::Error::js("TypeError", "bad path id"));
                }
                Ok(())
            }
            PathRef::Path(path) => assert_safe_path(path),
        }
    };
    match wop {
        WireOp::Replace(_) | WireOp::SetShort(_) | WireOp::DeleteShort(_) | WireOp::AppendShort(_) => Ok(()),
        WireOp::TruncateShort(WireOpTruncateShort(_, count)) => {
            if *count < 0 {
                return Err(pi_js::Error::js("TypeError", "t count"));
            }
            Ok(())
        }
        WireOp::SpliceShort(WireOpSpliceShort(_, index, remove, _)) => {
            if *index < 0 {
                return Err(pi_js::Error::js("TypeError", "p index"));
            }
            if *remove < 0 {
                return Err(pi_js::Error::js("TypeError", "p remove"));
            }
            Ok(())
        }
        WireOp::MoveShort(WireOpMoveShort(_, perm)) => assert_typed_permutation(perm),
        WireOp::Set(WireOpSet(_, r, _))
        | WireOp::Delete(WireOpDelete(_, r))
        | WireOp::Append(WireOpAppend(_, r, _)) => ok_typed_ref(r),
        WireOp::Truncate(WireOpTruncate(_, r, count)) => {
            ok_typed_ref(r)?;
            if *count < 0 {
                return Err(pi_js::Error::js("TypeError", "t count"));
            }
            Ok(())
        }
        WireOp::Splice(WireOpSplice(_, r, index, remove, _)) => {
            ok_typed_ref(r)?;
            if *index < 0 {
                return Err(pi_js::Error::js("TypeError", "p index"));
            }
            if *remove < 0 {
                return Err(pi_js::Error::js("TypeError", "p remove"));
            }
            Ok(())
        }
        WireOp::Move(WireOpMove(_, r, perm)) => {
            ok_typed_ref(r)?;
            assert_typed_permutation(perm)
        }
        WireOp::Define(WireOpDefine(_, id, path)) => {
            if *id < 0 {
                return Err(pi_js::Error::js("TypeError", "# shape"));
            }
            assert_safe_path(path)
        }
    }
}

fn assert_typed_permutation(perm: &[i64]) -> pi_js::Result<()> {
    let mut seen = vec![0u8; perm.len()];
    for &idx in perm {
        if idx < 0 || (idx as usize) >= perm.len() || seen[idx as usize] != 0 {
            return Err(pi_js::Error::js("TypeError", "m permutation is not a bijection"));
        }
        seen[idx as usize] = 1;
    }
    Ok(())
}

fn is_obj(value: &JsonValue) -> bool {
    matches!(value, JsonValue::Array(_) | JsonValue::Object(_))
}

fn assert_path_arg(path: &serde_json::Value, non_empty: bool) -> pi_js::Result<()> {
    let Some(arr) = path.as_array() else {
        return Err(pi_js::Error::js("TypeError", "path is not an array"));
    };
    if non_empty && arr.is_empty() {
        return Err(pi_js::Error::js("TypeError", "path is empty"));
    }
    assert_json_path_segments(path)
}

fn assert_permutation(value: &serde_json::Value) -> pi_js::Result<()> {
    let Some(items) = value.as_array() else {
        return Err(pi_js::Error::js("TypeError", "m permutation is not an array"));
    };
    let mut seen = vec![0u8; items.len()];
    for item in items {
        let Some(index) = non_neg_int_i64(item) else {
            return Err(pi_js::Error::js("TypeError", "m permutation is not a bijection"));
        };
        if (index as usize) >= items.len() || seen[index as usize] != 0 {
            return Err(pi_js::Error::js("TypeError", "m permutation is not a bijection"));
        }
        seen[index as usize] = 1;
    }
    Ok(())
}

fn assert_index_in_range(parent_len: i64, index: i64) -> pi_js::Result<()> {
    if index > parent_len {
        return Err(pi_js::Error::typed(
            "UnsafePathError",
            UnsafePathError::new(Seg::Index(index)),
        ));
    }
    Ok(())
}

fn parse_canonical_array_index(s: &str, len: usize) -> Option<usize> {
    if s == "0" {
        return (0 < len).then_some(0);
    }
    if s.is_empty() || s.starts_with('0') || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let idx: usize = s.parse().ok()?;
    (idx < len).then_some(idx)
}

fn seg_to_key(seg: &Seg) -> std::borrow::Cow<'_, str> {
    match seg {
        Seg::String(s) => std::borrow::Cow::Borrowed(s.as_str()),
        Seg::Index(i) => std::borrow::Cow::Owned(i.to_string()),
    }
}

fn resolve_mut<'a>(root: &'a mut JsonValue, path: &Path) -> pi_js::Result<&'a mut JsonValue> {
    let mut node = root;
    for seg in path {
        if !is_obj(node) {
            return Err(pi_js::Error::typed(
                "PathError",
                PathError::new(PathRef::Path(path.clone())),
            ));
        }
        match node {
            JsonValue::Array(arr) => match seg {
                Seg::Index(idx) => {
                    if *idx < 0 || (*idx as usize) >= arr.len() {
                        return Err(pi_js::Error::typed(
                            "PathError",
                            PathError::new(PathRef::Path(path.clone())),
                        ));
                    }
                    node = &mut arr[*idx as usize];
                }
                Seg::String(s) => {
                    if parse_canonical_array_index(s, arr.len()).is_some() {
                        return Err(pi_js::Error::typed(
                            "UnsafePathError",
                            UnsafePathError::new(Seg::String(s.clone())),
                        ));
                    }
                    return Err(pi_js::Error::typed(
                        "PathError",
                        PathError::new(PathRef::Path(path.clone())),
                    ));
                }
            },
            JsonValue::Object(map) => {
                let k = seg_to_key(seg);
                let Some(next) = map.get_mut(k.as_ref()) else {
                    return Err(pi_js::Error::typed(
                        "PathError",
                        PathError::new(PathRef::Path(path.clone())),
                    ));
                };
                node = next;
            }
            _ => unreachable!(),
        }
    }
    if !is_obj(node) {
        return Err(pi_js::Error::typed(
            "PathError",
            PathError::new(PathRef::Path(path.clone())),
        ));
    }
    Ok(node)
}

fn apply_single_json_op(mut root: JsonValue, op: &Op) -> pi_js::Result<JsonValue> {
    validate_typed_op(op)?;
    match op {
        Op::Replace(OpReplace(_, value)) => return Ok(value.clone()),
        Op::Splice(OpSplice(_, path, index, remove, items)) => {
            assert_safe_path(path)?;
            let target = if path.is_empty() {
                if !is_obj(&root) {
                    return Err(pi_js::Error::typed(
                        "PathError",
                        PathError::new(PathRef::Path(path.clone())),
                    ));
                }
                &mut root
            } else {
                resolve_mut(&mut root, path)?
            };
            let JsonValue::Array(arr) = target else {
                return Err(pi_js::Error::typed(
                    "PathError",
                    PathError::new(PathRef::Path(path.clone())),
                ));
            };
            let start = (*index as usize).min(arr.len());
            let del = (*remove as usize).min(arr.len().saturating_sub(start));
            arr.splice(start..start + del, items.iter().cloned());
        }
        Op::Move(OpMove(_, path, perm)) => {
            assert_safe_path(path)?;
            let target = if path.is_empty() {
                if !is_obj(&root) {
                    return Err(pi_js::Error::typed(
                        "PathError",
                        PathError::new(PathRef::Path(path.clone())),
                    ));
                }
                &mut root
            } else {
                resolve_mut(&mut root, path)?
            };
            let JsonValue::Array(arr) = target else {
                return Err(pi_js::Error::typed(
                    "PathError",
                    PathError::new(PathRef::Path(path.clone())),
                ));
            };
            if arr.len() != perm.len() {
                return Err(pi_js::Error::typed(
                    "PathError",
                    PathError::new(PathRef::Path(path.clone())),
                ));
            }
            let previous = std::mem::take(arr);
            arr.reserve(perm.len());
            for &idx in perm {
                arr.push(previous[idx as usize].clone());
            }
        }
        Op::Set(OpSet(_, path, _))
        | Op::Delete(OpDelete(_, path))
        | Op::Append(OpAppend(_, path, _))
        | Op::Truncate(OpTruncate(_, path, _)) => {
            assert_safe_path(path)?;
            let parent_path = path[..path.len() - 1].to_vec();
            let parent = resolve_mut(&mut root, &parent_path)?;
            let key = path.last().expect("validated non-empty path");
            match parent {
                JsonValue::Array(arr) => {
                    let Seg::Index(idx) = key else {
                        return Err(pi_js::Error::typed(
                            "UnsafePathError",
                            UnsafePathError::new(key.clone()),
                        ));
                    };
                    assert_index_in_range(arr.len() as i64, *idx)?;
                    let pos = *idx as usize;
                    match op {
                        Op::Set(OpSet(_, _, value)) => {
                            if pos < arr.len() {
                                arr[pos] = value.clone();
                            } else {
                                arr.push(value.clone());
                            }
                        }
                        Op::Delete(_) => {
                            if pos >= arr.len() {
                                return Err(pi_js::Error::typed(
                                    "PathError",
                                    PathError::new(PathRef::Path(path.clone())),
                                ));
                            }
                            arr.drain(pos..=pos);
                        }
                        Op::Append(OpAppend(_, _, suffix)) => {
                            let Some(JsonValue::String(existing)) = arr.get(pos) else {
                                return Err(pi_js::Error::typed(
                                    "PathError",
                                    PathError::new(PathRef::Path(path.clone())),
                                ));
                            };
                            let updated = format!("{existing}{suffix}");
                            arr[pos] = JsonValue::String(updated);
                        }
                        Op::Truncate(OpTruncate(_, _, count)) => {
                            let Some(JsonValue::String(existing)) = arr.get(pos) else {
                                return Err(pi_js::Error::typed(
                                    "PathError",
                                    PathError::new(PathRef::Path(path.clone())),
                                ));
                            };
                            let updated = pi_js::str16::slice(existing, *count, None);
                            arr[pos] = JsonValue::String(updated);
                        }
                        _ => unreachable!(),
                    }
                }
                JsonValue::Object(map) => {
                    let k = seg_to_key(key);
                    match op {
                        Op::Set(OpSet(_, _, value)) => {
                            map.insert(k.into_owned(), value.clone());
                        }
                        Op::Delete(_) => {
                            map.shift_remove(k.as_ref());
                        }
                        Op::Append(OpAppend(_, _, suffix)) => {
                            let Some(JsonValue::String(existing)) = map.get(k.as_ref()) else {
                                return Err(pi_js::Error::typed(
                                    "PathError",
                                    PathError::new(PathRef::Path(path.clone())),
                                ));
                            };
                            let updated = format!("{existing}{suffix}");
                            map.insert(k.into_owned(), JsonValue::String(updated));
                        }
                        Op::Truncate(OpTruncate(_, _, count)) => {
                            let Some(JsonValue::String(existing)) = map.get(k.as_ref()) else {
                                return Err(pi_js::Error::typed(
                                    "PathError",
                                    PathError::new(PathRef::Path(path.clone())),
                                ));
                            };
                            let updated = pi_js::str16::slice(existing, *count, None);
                            map.insert(k.into_owned(), JsonValue::String(updated));
                        }
                        _ => unreachable!(),
                    }
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(root)
}

fn apply_ops<T: 'static>(target: Option<T>, ops: &[Op]) -> pi_js::Result<T> {
    let mut root: Option<JsonValue> = match target {
        Some(value) => Some(crate::json::take_as_json(value)?),
        None => None,
    };
    for op in ops {
        validate_typed_op(op)?;
        if let Op::Replace(OpReplace(_, value)) = op {
            root = Some(value.clone());
            continue;
        }
        let current = root.take().ok_or_else(|| {
            let err_path = match op {
                Op::Splice(OpSplice(_, p, _, _, _))
                | Op::Move(OpMove(_, p, _))
                | Op::Set(OpSet(_, p, _))
                | Op::Delete(OpDelete(_, p))
                | Op::Append(OpAppend(_, p, _))
                | Op::Truncate(OpTruncate(_, p, _)) => p.clone(),
                Op::Replace(_) => Vec::new(),
            };
            pi_js::Error::typed("PathError", PathError::new(PathRef::Path(err_path)))
        })?;
        root = Some(apply_single_json_op(current, op)?);
    }
    let final_root = root.ok_or_else(|| {
        pi_js::Error::js(
            "TypeError",
            "Cannot apply delta operations without initial target or replacement op",
        )
    })?;
    crate::json::from_json(final_root)
}

fn copy_containers(root: JsonValue, path: &Path, owned: &mut IndexSet<u64>) -> pi_js::Result<JsonValue> {
    if !is_obj(&root) {
        return Err(pi_js::Error::typed(
            "PathError",
            PathError::new(PathRef::Path(path.clone())),
        ));
    }
    let _ = owned;
    let mut node: &JsonValue = &root;
    for segment in path {
        match node {
            JsonValue::Array(arr) => match segment {
                Seg::Index(idx) => {
                    if *idx < 0 || (*idx as usize) >= arr.len() {
                        return Err(pi_js::Error::typed(
                            "PathError",
                            PathError::new(PathRef::Path(path.clone())),
                        ));
                    }
                    let child = &arr[*idx as usize];
                    if !is_obj(child) {
                        return Err(pi_js::Error::typed(
                            "PathError",
                            PathError::new(PathRef::Path(path.clone())),
                        ));
                    }
                    node = child;
                }
                Seg::String(s) => {
                    if parse_canonical_array_index(s, arr.len()).is_some() {
                        return Err(pi_js::Error::typed(
                            "UnsafePathError",
                            UnsafePathError::new(Seg::String(s.clone())),
                        ));
                    }
                    return Err(pi_js::Error::typed(
                        "PathError",
                        PathError::new(PathRef::Path(path.clone())),
                    ));
                }
            },
            JsonValue::Object(map) => {
                let k = seg_to_key(segment);
                let Some(child) = map.get(k.as_ref()) else {
                    return Err(pi_js::Error::typed(
                        "PathError",
                        PathError::new(PathRef::Path(path.clone())),
                    ));
                };
                if !is_obj(child) {
                    return Err(pi_js::Error::typed(
                        "PathError",
                        PathError::new(PathRef::Path(path.clone())),
                    ));
                }
                node = child;
            }
            _ => unreachable!(),
        }
    }
    Ok(root)
}

fn path_key(path: &Path) -> String {
    pi_js::json::stringify(path)
}
