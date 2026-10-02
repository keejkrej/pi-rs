//! pi_js::testing (Rust-only; API contract: PORTING.md Appendix A).
//!
//! Test helpers shared by every crate: [`assert_json_matches`] (vitest `toMatchObject`) and
//! [`Recorder`] (`vi.fn()` call recording).

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::Value;

/// `expect(actual).toMatchObject(expected)` (vitest 4 `equals` with `iterableEquality` and
/// `subsetEquality`) on JSON values:
///
/// - an expected object matches when every expected key is a property of `actual` and its value
///   matches recursively (extra actual keys are ignored; an empty expected object matches any
///   value, even `null`). On an actual array, the properties are the indices and `length`.
/// - an expected array matches only an array of the same length whose elements match
///   element-wise (each element by these same rules).
/// - other values compare with `Object.is` (so `0` and `-0` differ; `1` and `1.0` are equal).
///
/// Panics with a vitest-style message and the first mismatching path when it does not match.
#[track_caller]
pub fn assert_json_matches(actual: &Value, expected: &Value) {
    if let Err(path) = matches(actual, expected, "$") {
        panic!(
            "expected {} to match object {}\nfirst mismatch at {path}",
            pretty(actual),
            pretty(expected)
        );
    }
}

/// The `toMatchObject` predicate behind [`assert_json_matches`].
pub fn json_matches(actual: &Value, expected: &Value) -> bool {
    matches(actual, expected, "$").is_ok()
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

/// `hasPropertyInObject(object, key) ? object[key] : <missing>` for JSON values.
fn property(actual: &Value, key: &str) -> Option<Value> {
    match actual {
        Value::Object(map) => map.get(key).cloned(),
        Value::Array(items) => {
            if key == "length" {
                return Some(Value::from(items.len()));
            }
            let index: usize = key.parse().ok()?;
            // Only canonical index strings ("0", "12"; not "01" or "+1") are array indices.
            if index.to_string() != key {
                return None;
            }
            items.get(index).cloned()
        }
        _ => None,
    }
}

fn matches(actual: &Value, expected: &Value, path: &str) -> Result<(), String> {
    match expected {
        Value::Object(subset) => {
            for (key, ev) in subset {
                let child = format!("{path}.{key}");
                match property(actual, key) {
                    Some(av) => matches(&av, ev, &child)?,
                    None => return Err(child),
                }
            }
            Ok(())
        }
        Value::Array(expected_items) => match actual {
            Value::Array(actual_items) if actual_items.len() == expected_items.len() => {
                for (i, (av, ev)) in actual_items.iter().zip(expected_items).enumerate() {
                    matches(av, ev, &format!("{path}[{i}]"))?;
                }
                Ok(())
            }
            _ => Err(path.to_string()),
        },
        Value::Number(en) => match actual {
            Value::Number(an) if number_is(an, en) => Ok(()),
            _ => Err(path.to_string()),
        },
        _ => {
            if actual == expected {
                Ok(())
            } else {
                Err(path.to_string())
            }
        }
    }
}

/// `Object.is(a, b)` for two JSON numbers.
fn number_is(a: &serde_json::Number, b: &serde_json::Number) -> bool {
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        return x == y;
    }
    if let (Some(x), Some(y)) = (a.as_u64(), b.as_u64()) {
        return x == y;
    }
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x == y && x.is_sign_negative() == y.is_sign_negative()) || (x.is_nan() && y.is_nan()),
        _ => false,
    }
}

/// Records the calls of a `vi.fn()` replacement. Clones share the same record.
pub struct Recorder<T> {
    calls: Arc<Mutex<Vec<T>>>,
}

impl<T> Recorder<T> {
    pub fn new() -> Self {
        Recorder {
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<T>> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records one call.
    pub fn push(&self, v: T) {
        self.lock().push(v);
    }

    /// `mock.calls.length`.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// `mock.mockClear()`.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Removes and returns every recorded call.
    pub fn take(&self) -> Vec<T> {
        std::mem::take(&mut *self.lock())
    }

    /// Runs `f` on the recorded calls without cloning them.
    pub fn with<R>(&self, f: impl FnOnce(&[T]) -> R) -> R {
        f(&self.lock())
    }
}

impl<T: Clone> Recorder<T> {
    /// `mock.calls`.
    pub fn calls(&self) -> Vec<T> {
        self.lock().clone()
    }

    /// `mock.lastCall`.
    pub fn last(&self) -> Option<T> {
        self.lock().last().cloned()
    }
}

impl<T> Clone for Recorder<T> {
    fn clone(&self) -> Self {
        Recorder {
            calls: self.calls.clone(),
        }
    }
}

impl<T> Default for Recorder<T> {
    fn default() -> Self {
        Recorder::new()
    }
}

impl<T: fmt::Debug> fmt::Debug for Recorder<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Recorder").field(&*self.lock()).finish()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Number, Value};

    use super::*;

    fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
        if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_string()
        } else {
            "<non-string panic>".to_string()
        }
    }

    #[test]
    fn subset_ignores_extra_keys_and_matches_nested_values() {
        let actual = serde_json::json!({
            "a": 1,
            "b": {"c": 2, "d": 3},
            "e": 4
        });
        let expected = serde_json::json!({"b": {"d": 3}, "a": 1});
        assert_json_matches(&actual, &expected);
        assert!(json_matches(&actual, &expected));
        assert!(!json_matches(&actual, &serde_json::json!({"a": 1, "missing": 0})));
        assert!(!json_matches(&actual, &serde_json::json!({"a": 2})));
    }

    #[test]
    fn subset_rejects_mismatch_with_the_first_path() {
        let payload = std::panic::catch_unwind(|| {
            assert_json_matches(&serde_json::json!({"a": {"b": 1}}), &serde_json::json!({"a": {"b": 2}}));
        })
        .expect_err("mismatch must panic");
        let msg = panic_message(payload);
        assert!(msg.contains("$.a.b"), "{msg}");

        let payload = std::panic::catch_unwind(|| {
            assert_json_matches(&serde_json::json!([{"id": 1}]), &serde_json::json!([{"id": 2}]));
        })
        .expect_err("mismatch must panic");
        let msg = panic_message(payload);
        assert!(msg.contains("$[0].id"), "{msg}");
    }

    #[test]
    fn arrays_match_elementwise_and_allow_object_subsets() {
        let actual = serde_json::json!([{"id": 1, "name": "a", "extra": true}, 2]);
        let expected = serde_json::json!([{"id": 1, "name": "a"}, 2]);
        assert_json_matches(&actual, &expected);
        assert!(!json_matches(&serde_json::json!([1, 2, 3]), &serde_json::json!([1, 2])));
        assert!(!json_matches(
            &serde_json::json!({"a": 1}),
            &serde_json::json!([{"a": 1}])
        ));
        assert_json_matches(
            &serde_json::json!(["x", "y"]),
            &serde_json::json!({"0": "x", "length": 2}),
        );
        assert!(!json_matches(
            &serde_json::json!(["x"]),
            &serde_json::json!({"length": 2})
        ));
    }

    #[test]
    fn numbers_use_object_is_and_empty_object_matches_anything() {
        assert!(json_matches(&serde_json::json!(1), &serde_json::json!(1.0)));
        assert!(json_matches(&serde_json::json!(1.0), &serde_json::json!(1)));
        assert!(!json_matches(&serde_json::json!(1), &serde_json::json!(2)));
        let neg = Value::Number(Number::from_f64(-0.0).unwrap());
        let pos = Value::Number(Number::from_f64(0.0).unwrap());
        assert!(json_matches(&serde_json::json!(0), &pos));
        assert!(!json_matches(&serde_json::json!(0), &neg));
        assert!(!json_matches(&pos, &neg));

        assert!(json_matches(&Value::Null, &serde_json::json!({})));
        assert!(json_matches(&serde_json::json!(1), &serde_json::json!({})));
        assert!(json_matches(&serde_json::json!("x"), &serde_json::json!("x")));
        assert!(!json_matches(&serde_json::json!("x"), &serde_json::json!("y")));
        assert!(json_matches(&Value::Null, &Value::Null));
        assert!(!json_matches(&Value::Null, &serde_json::json!(0)));
    }

    #[test]
    fn recorder_shares_calls_across_clones() {
        let rec = Recorder::new();
        rec.push(1);
        let clone = rec.clone();
        clone.push(2);
        assert_eq!(rec.calls(), vec![1, 2]);
        assert_eq!(clone.len(), 2);
        assert_eq!(rec.last(), Some(2));
        assert_eq!(rec.take(), vec![1, 2]);
        assert!(clone.is_empty());
    }
}
