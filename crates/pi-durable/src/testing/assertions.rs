//! Port of packages/durable/src/testing/assertions.ts

use std::sync::Arc;

use serde_json::Value as JsonValue;

use crate::testing::types::StorageConformanceAssertions;

/// Default Rust assertion facade backing storage conformance suites.
#[derive(Clone, Copy, Debug, Default)]
pub struct StandardAssertions;

impl StorageConformanceAssertions for StandardAssertions {
    fn ok(&self, value: bool, message: &str) {
        assert!(value, "{message}");
    }

    fn strict_equal(&self, actual: &JsonValue, expected: &JsonValue) {
        assert_eq!(actual, expected);
    }

    fn deep_equal(&self, actual: &JsonValue, expected: &JsonValue) {
        assert_eq!(actual, expected);
    }

    fn partial_deep_equal(&self, actual: &JsonValue, expected: &JsonValue) {
        assert_partial_match(actual, expected, "$");
    }

    fn greater_than(&self, actual: i64, expected: i64) {
        assert!(actual > expected, "expected {actual} > {expected}");
    }

    fn rejects_containing(&self, result: Result<(), pi_js::Error>, message_includes: &str) {
        match result {
            Ok(()) => panic!("expected error containing '{message_includes}', but operation succeeded"),
            Err(err) => {
                let msg = err.to_string();
                assert!(
                    msg.contains(message_includes),
                    "expected error message '{msg}' to contain '{message_includes}'"
                );
            }
        }
    }

    fn rejects_error_name(&self, result: Result<(), pi_js::Error>, expected_name: &str) {
        match result {
            Ok(()) => panic!("expected error named '{expected_name}', but operation succeeded"),
            Err(err) => {
                assert_eq!(
                    err.name(),
                    expected_name,
                    "expected error name '{expected_name}', got '{}' (message: {err})",
                    err.name()
                );
            }
        }
    }
}

pub fn create_expect_assertions() -> Arc<dyn StorageConformanceAssertions> {
    Arc::new(StandardAssertions)
}

fn assert_partial_match(actual: &JsonValue, expected: &JsonValue, path: &str) {
    match (actual, expected) {
        (JsonValue::Object(a_map), JsonValue::Object(e_map)) => {
            for (k, e_val) in e_map {
                let next_path = format!("{path}.{k}");
                let a_val = a_map
                    .get(k)
                    .unwrap_or_else(|| panic!("missing expected field at {next_path} in {actual}"));
                assert_partial_match(a_val, e_val, &next_path);
            }
        }
        (JsonValue::Array(a_arr), JsonValue::Array(e_arr)) => {
            assert_eq!(
                a_arr.len(),
                e_arr.len(),
                "array length mismatch at {path}: {} vs {}",
                a_arr.len(),
                e_arr.len()
            );
            for (idx, (a_item, e_item)) in a_arr.iter().zip(e_arr.iter()).enumerate() {
                assert_partial_match(a_item, e_item, &format!("{path}[{idx}]"));
            }
        }
        _ => assert_eq!(actual, expected, "value mismatch at {path}"),
    }
}
