//! Arena representation of immutable JSON Schema documents and exact JSON numeric/equality operations.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

use indexmap::IndexMap;
use serde_json::{Map, Number, Value};

/// Identifier of an interned JSON node within a [`SchemaArena`].
pub(crate) type NodeId = usize;

/// One JSON node stored inside a [`SchemaArena`].
#[derive(Clone, Debug)]
pub(crate) enum NodeData {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<NodeId>),
    Object(IndexMap<String, NodeId>),
}

/// Arena storing one root schema and any lazily loaded external schema documents.
#[derive(Clone, Debug, Default)]
pub(crate) struct SchemaArena {
    nodes: Vec<NodeData>,
}

impl SchemaArena {
    pub(crate) fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    /// Inserts `value` and all of its descendants into the arena, returning the root [`NodeId`].
    pub(crate) fn insert(&mut self, value: &Value) -> NodeId {
        let data = match value {
            Value::Null => NodeData::Null,
            Value::Bool(b) => NodeData::Bool(*b),
            Value::Number(n) => NodeData::Number(n.clone()),
            Value::String(s) => NodeData::String(s.clone()),
            Value::Array(items) => {
                let ids = items.iter().map(|item| self.insert(item)).collect();
                NodeData::Array(ids)
            }
            Value::Object(map) => {
                let mut entries = IndexMap::with_capacity(map.len());
                for (k, v) in map {
                    entries.insert(k.clone(), self.insert(v));
                }
                NodeData::Object(entries)
            }
        };
        let id = self.nodes.len();
        self.nodes.push(data);
        id
    }

    pub(crate) fn get(&self, id: NodeId) -> &NodeData {
        &self.nodes[id]
    }

    pub(crate) fn is_schema_object(&self, id: NodeId) -> bool {
        matches!(self.get(id), NodeData::Object(_))
    }

    pub(crate) fn as_bool(&self, id: NodeId) -> Option<bool> {
        match self.get(id) {
            NodeData::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self, id: NodeId) -> Option<&str> {
        match self.get(id) {
            NodeData::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub(crate) fn as_number(&self, id: NodeId) -> Option<&Number> {
        match self.get(id) {
            NodeData::Number(n) => Some(n),
            _ => None,
        }
    }

    pub(crate) fn as_array(&self, id: NodeId) -> Option<&[NodeId]> {
        match self.get(id) {
            NodeData::Array(items) => Some(items.as_slice()),
            _ => None,
        }
    }

    pub(crate) fn as_object(&self, id: NodeId) -> Option<&IndexMap<String, NodeId>> {
        match self.get(id) {
            NodeData::Object(map) => Some(map),
            _ => None,
        }
    }

    pub(crate) fn prop(&self, id: NodeId, key: &str) -> Option<NodeId> {
        self.as_object(id)?.get(key).copied()
    }

    pub(crate) fn str_prop(&self, id: NodeId, key: &str) -> Option<&str> {
        self.as_str(self.prop(id, key)?)
    }

    pub(crate) fn array_prop(&self, id: NodeId, key: &str) -> Option<&[NodeId]> {
        self.as_array(self.prop(id, key)?)
    }

    pub(crate) fn obj_prop(&self, id: NodeId, key: &str) -> Option<&IndexMap<String, NodeId>> {
        self.as_object(self.prop(id, key)?)
    }

    /// Reconstructs a `serde_json::Value` from `id`.
    pub(crate) fn to_value(&self, id: NodeId) -> Value {
        match self.get(id) {
            NodeData::Null => Value::Null,
            NodeData::Bool(b) => Value::Bool(*b),
            NodeData::Number(n) => Value::Number(n.clone()),
            NodeData::String(s) => Value::String(s.clone()),
            NodeData::Array(items) => Value::Array(items.iter().map(|&c| self.to_value(c)).collect()),
            NodeData::Object(map) => {
                let mut out = Map::with_capacity(map.len());
                for (k, &v) in map {
                    out.insert(k.clone(), self.to_value(v));
                }
                Value::Object(out)
            }
        }
    }
}

/// Whether `n` represents a finite JSON integer (`1`, `1.0`, `1e5`).
pub(crate) fn is_integer_number(n: &Number) -> bool {
    n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.is_finite() && f.fract() == 0.0)
}

/// Whether `n` represents a finite JSON number.
pub(crate) fn is_finite_number(n: &Number) -> bool {
    n.is_i64() || n.is_u64() || n.as_f64().is_some_and(f64::is_finite)
}

/// Converts a JSON `Number` to `i128` if it is represented as an integer (`i64` or `u64`).
pub(crate) fn num_as_exact_i128(n: &Number) -> Option<i128> {
    if let Some(i) = n.as_i64() {
        Some(i128::from(i))
    } else {
        n.as_u64().map(i128::from)
    }
}

/// Compares two JSON numbers with full exactness across `i64`, `u64`, and `f64` (`1 == 1.0`, `-0.0 == 0.0`).
pub(crate) fn number_cmp(a: &Number, b: &Number) -> Ordering {
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        return x.cmp(&y);
    }
    if let (Some(x), Some(y)) = (a.as_u64(), b.as_u64()) {
        return x.cmp(&y);
    }
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_u64()) {
        return if x < 0 { Ordering::Less } else { (x as u64).cmp(&y) };
    }
    if let (Some(x), Some(y)) = (a.as_u64(), b.as_i64()) {
        return if y < 0 { Ordering::Greater } else { x.cmp(&(y as u64)) };
    }
    if let (Some(u), Some(f)) = (a.as_u64(), b.as_f64()) {
        return cmp_u64_f64(u, f);
    }
    if let (Some(f), Some(u)) = (a.as_f64(), b.as_u64()) {
        return cmp_u64_f64(u, f).reverse();
    }
    if let (Some(i), Some(f)) = (a.as_i64(), b.as_f64()) {
        return cmp_i64_f64(i, f);
    }
    if let (Some(f), Some(i)) = (a.as_f64(), b.as_i64()) {
        return cmp_i64_f64(i, f).reverse();
    }
    match (a.as_f64(), b.as_f64()) {
        (Some(fa), Some(fb)) => {
            if fa == fb {
                Ordering::Equal
            } else if fa < fb {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        _ => Ordering::Equal,
    }
}

fn cmp_u64_f64(u: u64, f: f64) -> Ordering {
    if f < 0.0 {
        return Ordering::Greater;
    }
    // 2^64 is exactly representable in f64 as 18446744073709551616.0.
    if f >= 18_446_744_073_709_551_616.0 {
        return Ordering::Less;
    }
    let trunc = f.trunc() as u64;
    match u.cmp(&trunc) {
        Ordering::Equal => {
            if f.fract() > 0.0 {
                Ordering::Less
            } else {
                Ordering::Equal
            }
        }
        ord => ord,
    }
}

fn cmp_i64_f64(i: i64, f: f64) -> Ordering {
    // -2^63 is exactly representable as i64::MIN as f64.
    if f < -9_223_372_036_854_775_808.0 {
        return Ordering::Greater;
    }
    // +2^63 is exactly representable in f64.
    if f >= 9_223_372_036_854_775_808.0 {
        return Ordering::Less;
    }
    let trunc = f.trunc() as i64;
    match i.cmp(&trunc) {
        Ordering::Equal => {
            let frac = f.fract();
            if frac > 0.0 {
                Ordering::Less
            } else if frac < 0.0 {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        }
        ord => ord,
    }
}

/// Exact numeric equality for JSON numbers (`1 == 1.0`, `-0.0 == 0.0`).
pub(crate) fn number_eq(a: &Number, b: &Number) -> bool {
    number_cmp(a, b) == Ordering::Equal
}

/// Formats a JSON `Number` using JS number stringification (`ryu_js` for floats, exact decimal for large integers).
pub(crate) fn format_number(n: &Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    if let Some(f) = n.as_f64() {
        return pi_js::num::to_js_string(f);
    }
    n.to_string()
}

/// JSON equality as `const`, `enum`, and `uniqueItems` compare (key order does not matter, `1 == 1.0`).
pub(crate) fn deep_equal_values(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => number_eq(x, y),
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| deep_equal_values(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| deep_equal_values(v, w)))
        }
        _ => false,
    }
}

/// Structural hash consistent with [`deep_equal_values`] (used for coercion memoization).
pub(crate) fn hash_value<H: Hasher>(value: &Value, state: &mut H) {
    match value {
        Value::Null => 0u8.hash(state),
        Value::Bool(b) => {
            1u8.hash(state);
            b.hash(state);
        }
        Value::Number(n) => {
            2u8.hash(state);
            if let Some(i) = n.as_i64() {
                0u8.hash(state);
                i.hash(state);
            } else if let Some(u) = n.as_u64() {
                if let Ok(i) = i64::try_from(u) {
                    0u8.hash(state);
                    i.hash(state);
                } else {
                    1u8.hash(state);
                    u.hash(state);
                }
            } else if let Some(f) = n.as_f64() {
                let norm = if f == 0.0 { 0.0 } else { f };
                if norm.is_finite()
                    && norm.fract() == 0.0
                    && norm >= i64::MIN as f64
                    && norm < 9_223_372_036_854_775_808.0
                {
                    0u8.hash(state);
                    (norm as i64).hash(state);
                } else if norm.is_finite() && norm.fract() == 0.0 && (0.0..18_446_744_073_709_551_616.0).contains(&norm)
                {
                    1u8.hash(state);
                    (norm as u64).hash(state);
                } else {
                    2u8.hash(state);
                    norm.to_bits().hash(state);
                }
            }
        }
        Value::String(s) => {
            3u8.hash(state);
            s.hash(state);
        }
        Value::Array(items) => {
            4u8.hash(state);
            items.len().hash(state);
            for item in items {
                hash_value(item, state);
            }
        }
        Value::Object(map) => {
            5u8.hash(state);
            map.len().hash(state);
            // XOR/sum order-independent combination so key insertion order matches deep_equal_values.
            let mut acc: u64 = 0;
            for (k, v) in map {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                k.hash(&mut h);
                hash_value(v, &mut h);
                acc = acc.wrapping_add(h.finish());
            }
            acc.hash(state);
        }
    }
}
