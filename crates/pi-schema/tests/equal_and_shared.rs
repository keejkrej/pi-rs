//! Port of packages/schema/test/equal.test.ts and packages/schema/test/shared.test.ts.

use pi_schema::{CompileOptions, compile, equal, shared_regex_count};
use serde_json::{Value, json};

#[test]
fn equal_compares_json_values_structurally() {
    let cases = [
        (json!(1), json!(1), true),
        (json!(1), json!(1.0), true),
        (json!("a"), json!("b"), false),
        (Value::Null, Value::Null, true),
        (
            json!({ "a": 1, "b": [1, { "c": 2 }] }),
            json!({ "b": [1, { "c": 2 }], "a": 1 }),
            true,
        ),
        (json!({ "a": 1 }), json!({ "a": 1, "b": 2 }), false),
        (json!([1, 2]), json!([2, 1]), false),
        (json!([1]), json!({ "0": 1 }), false),
        (json!({}), json!([]), false),
    ];
    for (a, b, expected) in cases {
        assert_eq!(equal(&a, &b), expected, "a={a} b={b}");
    }
}

#[test]
fn shared_regex_drops_entries_when_validators_are_dropped() {
    let before = shared_regex_count();
    let mut validators: Vec<_> = (0..50)
        .map(|i| {
            compile(
                json!({ "type": "string", "pattern": format!("^shared-drop-test-{i}$") }),
                CompileOptions::default(),
            )
            .unwrap()
        })
        .collect();
    let again = compile(
        json!({ "type": "string", "pattern": "^shared-drop-test-0$" }),
        CompileOptions::default(),
    )
    .unwrap();
    assert_eq!(shared_regex_count(), before + 50);
    assert!(again.check(&json!("shared-drop-test-0")));

    validators.clear();
    // `again` still holds `^shared-drop-test-0$`; the other 49 weak entries drop immediately.
    assert_eq!(shared_regex_count(), before + 1);
}
