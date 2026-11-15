//! Port of packages/chord/test/json.test.ts

use pi_chord::json::{CopyJsonOptions, copy_json, is_json_value};
use serde_json::{Value, json};

#[test]
fn checks_strict_json_without_normalizing_it() {
    assert!(is_json_value(&json!({ "nested": [1, true, null] })));
    // PORT: `undefined`, typed arrays (`Uint8Array`), `Infinity`, and cyclic graphs cannot exist
    // inside Rust's `serde_json::Value` representation and are rejected statically by the type system.
    assert!(is_json_value(&json!(null)));
    assert!(is_json_value(&json!(42.5)));
    assert!(is_json_value(&json!("hello")));
}

#[test]
fn copies_strict_json_without_retaining_aliases() {
    let shared = json!({ "value": 1 });
    let input = json!({ "left": shared, "right": shared });
    let copied = copy_json(&input, None).expect("strict JSON copy should succeed");
    assert_eq!(copied, input);
    assert_eq!(copied["left"]["value"], 1);
    assert_eq!(copied["right"]["value"], 1);
}

#[test]
fn copies_json_with_omit_undefined_properties_option() {
    // PORT: Rust `JsonValue` has no `undefined` property value, so object copy preserves keys
    // while `CopyJsonOptions { omit_undefined_properties: true }` remains accepted for API parity.
    let input = json!({ "kept": 1, "nested": { "kept": true } });
    let copied = copy_json(
        &input,
        Some(CopyJsonOptions {
            omit_undefined_properties: Some(true),
        }),
    )
    .expect("copy with options should succeed");
    assert_eq!(copied, json!({ "kept": 1, "nested": { "kept": true } }));
}

#[test]
fn preserves_own_proto_data_properties() {
    // PORT: `serde_json::Value::Object` stores keys in a map without prototype inheritance.
    let mut map = serde_json::Map::new();
    map.insert("__proto__".to_owned(), json!({ "safe": true }));
    let input = Value::Object(map);
    let copied = copy_json(&input, None).expect("copying own __proto__ data property should succeed");
    let obj = copied.as_object().expect("expected object");
    assert!(obj.contains_key("__proto__"));
    assert_eq!(obj["__proto__"], json!({ "safe": true }));
}
