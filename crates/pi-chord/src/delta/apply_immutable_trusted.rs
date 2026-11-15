//! Port of packages/chord/src/delta/apply-immutable-trusted.ts

use crate::delta::{Op, OpAppend, OpDelete, OpMove, OpReplace, OpSet, OpSplice, OpTruncate, Path, Seg};
use crate::types::JsonValue;

// PORT: JS `DATA_DESCRIPTOR` is a reused property descriptor. Rust writes JSON entries directly.
// Because Rust `apply_immutable_trusted` takes unique ownership of `target`, container edits along
// each trusted path update the owned JSON tree in place without mutating any caller-held copy.

/// Apply self-produced trusted operations while copying each touched container once.
pub fn apply_immutable_trusted<T: 'static>(target: T, operations: &[Op]) -> pi_js::Result<T> {
    let mut root = crate::json::take_as_json(target)?;
    for operation in operations {
        match operation {
            Op::Replace(OpReplace(_, value)) => {
                root = value.clone();
            }
            Op::Splice(OpSplice(_, path, start, remove, items)) => {
                let container = resolve_container_mut(&mut root, path, path.len())?;
                if !matches!(container, JsonValue::Array(_)) {
                    return Err(pi_js::Error::js(
                        "TypeError",
                        "Trusted array operation target is not an array",
                    ));
                }
                splice_trusted(container, *start, *remove, items)?;
            }
            Op::Move(OpMove(_, path, permutation)) => {
                let container = resolve_container_mut(&mut root, path, path.len())?;
                if !matches!(container, JsonValue::Array(_)) {
                    return Err(pi_js::Error::js(
                        "TypeError",
                        "Trusted array operation target is not an array",
                    ));
                }
                permute_trusted(container, permutation)?;
            }
            Op::Set(OpSet(_, path, value)) => {
                let Some(key) = path.last() else {
                    return Err(pi_js::Error::js("TypeError", "Trusted operation path is empty"));
                };
                let container = resolve_container_mut(&mut root, path, path.len() - 1)?;
                define_data(container, key, value.clone())?;
            }
            Op::Delete(OpDelete(_, path)) => {
                let Some(key) = path.last() else {
                    return Err(pi_js::Error::js("TypeError", "Trusted operation path is empty"));
                };
                let container = resolve_container_mut(&mut root, path, path.len() - 1)?;
                match container {
                    JsonValue::Array(arr) => {
                        let Seg::Index(idx) = key else {
                            return Err(pi_js::Error::js(
                                "TypeError",
                                "Trusted array deletion key is not numeric",
                            ));
                        };
                        if *idx < 0 || (*idx as usize) >= arr.len() {
                            return Err(pi_js::Error::js(
                                "TypeError",
                                "Trusted array deletion index out of range",
                            ));
                        }
                        arr.drain(*idx as usize..=*idx as usize);
                    }
                    JsonValue::Object(map) => {
                        let k = seg_to_key(key);
                        map.shift_remove(k.as_ref());
                    }
                    _ => unreachable!(),
                }
            }
            Op::Append(OpAppend(_, path, suffix)) => {
                let Some(key) = path.last() else {
                    return Err(pi_js::Error::js("TypeError", "Trusted operation path is empty"));
                };
                let container = resolve_container_mut(&mut root, path, path.len() - 1)?;
                let current = read(container, key);
                let Some(JsonValue::String(existing)) = current else {
                    return Err(pi_js::Error::js("TypeError", "Trusted append target is not a string"));
                };
                let updated = format!("{existing}{suffix}");
                define_data(container, key, JsonValue::String(updated))?;
            }
            Op::Truncate(OpTruncate(_, path, count)) => {
                let Some(key) = path.last() else {
                    return Err(pi_js::Error::js("TypeError", "Trusted operation path is empty"));
                };
                let container = resolve_container_mut(&mut root, path, path.len() - 1)?;
                let current = read(container, key);
                let Some(JsonValue::String(existing)) = current else {
                    return Err(pi_js::Error::js("TypeError", "Trusted truncate target is not a string"));
                };
                if *count < 0 {
                    return Err(pi_js::Error::js("TypeError", "Trusted truncate count is negative"));
                }
                let updated = pi_js::str16::slice(existing, *count, None);
                define_data(container, key, JsonValue::String(updated))?;
            }
        }
    }
    crate::json::from_json(root)
}

fn resolve_container_mut<'a>(root: &'a mut JsonValue, path: &Path, length: usize) -> pi_js::Result<&'a mut JsonValue> {
    if !is_container(root) {
        return Err(pi_js::Error::js(
            "TypeError",
            "Trusted operation root is not a container",
        ));
    }
    let mut current = root;
    for segment in &path[..length] {
        let child = match (current, segment) {
            (JsonValue::Array(arr), Seg::Index(idx)) if *idx >= 0 => arr.get_mut(*idx as usize),
            (JsonValue::Array(_), Seg::Index(_)) | (JsonValue::Array(_), Seg::String(_)) => None,
            (JsonValue::Object(map), seg) => {
                let key = seg_to_key(seg);
                map.get_mut(key.as_ref())
            }
            _ => None,
        };
        let Some(next) = child else {
            return Err(pi_js::Error::js(
                "TypeError",
                "Trusted operation path is not a container",
            ));
        };
        if !is_container(next) {
            return Err(pi_js::Error::js(
                "TypeError",
                "Trusted operation path is not a container",
            ));
        }
        current = next;
    }
    Ok(current)
}

fn seg_to_key(seg: &Seg) -> std::borrow::Cow<'_, str> {
    match seg {
        Seg::String(s) => std::borrow::Cow::Borrowed(s.as_str()),
        Seg::Index(i) => std::borrow::Cow::Owned(i.to_string()),
    }
}

fn read<'a>(target: &'a JsonValue, key: &Seg) -> Option<&'a JsonValue> {
    match (target, key) {
        (JsonValue::Array(arr), Seg::Index(idx)) if *idx >= 0 => arr.get(*idx as usize),
        (JsonValue::Object(map), seg) => {
            let k = seg_to_key(seg);
            map.get(k.as_ref())
        }
        _ => None,
    }
}

fn define_data(target: &mut JsonValue, key: &Seg, value: JsonValue) -> pi_js::Result<()> {
    match target {
        JsonValue::Array(arr) => {
            let Seg::Index(idx) = key else {
                return Err(pi_js::Error::js("TypeError", "Trusted array key is not numeric"));
            };
            if *idx < 0 {
                return Err(pi_js::Error::js("TypeError", "Trusted array index is negative"));
            }
            let pos = *idx as usize;
            if pos < arr.len() {
                arr[pos] = value;
            } else if pos == arr.len() {
                arr.push(value);
            } else {
                return Err(pi_js::Error::js(
                    "TypeError",
                    "Trusted array write would create sparse hole",
                ));
            }
            Ok(())
        }
        JsonValue::Object(map) => {
            let k = seg_to_key(key).into_owned();
            map.insert(k, value);
            Ok(())
        }
        _ => Err(pi_js::Error::js(
            "TypeError",
            "Trusted operation target is not a container",
        )),
    }
}

fn splice_trusted(target: &mut JsonValue, start: i64, remove: i64, items: &[JsonValue]) -> pi_js::Result<()> {
    let JsonValue::Array(arr) = target else {
        return Err(pi_js::Error::js(
            "TypeError",
            "Trusted array operation target is not an array",
        ));
    };
    let len = arr.len();
    let idx = if start < 0 {
        let shifted = (len as i64) + start;
        if shifted < 0 { 0 } else { (shifted as usize).min(len) }
    } else {
        (start as usize).min(len)
    };
    let del = if remove <= 0 {
        0
    } else {
        (remove as usize).min(len.saturating_sub(idx))
    };
    arr.splice(idx..idx + del, items.iter().cloned());
    Ok(())
}

fn permute_trusted(target: &mut JsonValue, permutation: &[i64]) -> pi_js::Result<()> {
    let JsonValue::Array(arr) = target else {
        return Err(pi_js::Error::js(
            "TypeError",
            "Trusted array operation target is not an array",
        ));
    };
    if arr.len() != permutation.len() {
        return Err(pi_js::Error::js("TypeError", "Trusted permutation length mismatch"));
    }
    let previous = std::mem::take(arr);
    arr.reserve(permutation.len());
    for &idx in permutation {
        if idx < 0 || (idx as usize) >= previous.len() {
            return Err(pi_js::Error::js("TypeError", "Trusted permutation index out of range"));
        }
        arr.push(previous[idx as usize].clone());
    }
    Ok(())
}

fn is_container(value: &JsonValue) -> bool {
    matches!(value, JsonValue::Array(_) | JsonValue::Object(_))
}
