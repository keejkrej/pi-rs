//! Port of packages/ai/test/json-parse.test.ts

use indexmap::IndexMap;
use pi_ai::utils::json_parse::{parse_streaming_json, parse_tool_call_arguments};
use serde_json::{Map, Value, json};

fn as_value(map: IndexMap<String, Value>) -> Value {
    let out: Map<String, Value> = map.into_iter().collect();
    Value::Object(out)
}

#[test]
fn parse_streaming_json_keeps_invalid_escapes_decodes_valid_ones_and_drops_cutoff_u_escapes() {
    // #10868
    assert_eq!(
        as_value(parse_streaming_json(Some(r#"{"text":"\url{x}"}"#))),
        json!({ "text": r"\url{x}" })
    );
    assert_eq!(
        as_value(parse_streaming_json(Some(
            r#"{"path":"a.tex","content":"\usepackage{x}","mode":"w"}"#
        ))),
        json!({
            "path": "a.tex",
            "content": r"\usepackage{x}",
            "mode": "w",
        })
    );
    assert_eq!(
        as_value(parse_streaming_json(Some(r#"{"text":"caf\u00e9 \url"}"#))),
        json!({ "text": "café \\url" })
    );
    assert_eq!(
        as_value(parse_streaming_json(Some(r#"{"text":"ab\u00"#))),
        json!({ "text": "ab" })
    );
}

// #10897
#[test]
fn parse_tool_call_arguments_handles_valid_repaired_empty_malformed_cutoff_and_non_objects() {
    let valid = parse_tool_call_arguments(Some(r#"{"kind":"seek","note":"\url"}"#));
    assert_eq!(as_value(valid.arguments), json!({ "kind": "seek", "note": r"\url" }));
    assert!(valid.arguments_error.is_none());

    let empty = parse_tool_call_arguments(Some(""));
    assert_eq!(as_value(empty.arguments), json!({}));
    assert!(empty.arguments_error.is_none());

    let raw_malformed = r#"{"action":"apply","actions":[{"kind":"seek","time":.5}]}"#;
    let malformed = parse_tool_call_arguments(Some(raw_malformed));
    assert_eq!(as_value(malformed.arguments), json!({}));
    let err = malformed.arguments_error.as_deref().unwrap();
    assert!(err.contains("\n\nReceived arguments:\n"), "{err}");
    assert!(err.ends_with(raw_malformed), "{err}");

    let raw_cutoff = r#"{"kind":"seek","clipId":"clip-fl"#;
    let cutoff = parse_tool_call_arguments(Some(raw_cutoff));
    assert_eq!(as_value(cutoff.arguments), json!({}));
    let cutoff_err = cutoff.arguments_error.as_deref().unwrap();
    assert!(cutoff_err.ends_with(raw_cutoff), "{cutoff_err}");

    let non_object = parse_tool_call_arguments(Some("[1,2]"));
    assert_eq!(as_value(non_object.arguments), json!({}));
    assert_eq!(
        non_object.arguments_error.as_deref(),
        Some("Tool arguments must be a JSON object\n\nReceived arguments:\n[1,2]")
    );
}
