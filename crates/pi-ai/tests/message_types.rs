//! Port of packages/ai/test/message-types.test.ts (unit ai-utils-messages)

use pi_ai::types::{Context, Message, ToolResultMessage};
use serde_json::json;

#[test]
fn preserves_json_compatible_detail_types() {
    let base = |details: Option<serde_json::Value>| ToolResultMessage {
        tool_call_id: "call-1".to_string(),
        tool_name: "read".to_string(),
        content: Vec::new(),
        details,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 1,
    };

    let object_msg = base(Some(json!({
        "path": "a.ts",
        "items": ["first"],
        "summary": { "lines": 3 }
    })));
    let array_msg = base(Some(json!(["a", "b"])));
    let tuple_msg = base(Some(json!(["a", { "count": 1 }])));
    let primitive_msg = base(Some(json!("diagnostic")));
    let null_msg = base(Some(serde_json::Value::Null));

    assert_eq!(object_msg.details.as_ref().unwrap()["summary"]["lines"], 3);
    assert_eq!(array_msg.details, Some(json!(["a", "b"])));
    assert_eq!(tuple_msg.details, Some(json!(["a", { "count": 1 }])));
    assert_eq!(primitive_msg.details, Some(json!("diagnostic")));
    assert_eq!(null_msg.details, Some(serde_json::Value::Null));

    let context = Context {
        system_prompt: None,
        messages: vec![Message::ToolResult(object_msg), Message::ToolResult(tuple_msg)],
        tools: None,
    };
    assert_eq!(context.messages.len(), 2);

    // Ensure serialization/deserialization preserves explicit null details vs omitted details.
    let roundtrip: ToolResultMessage =
        serde_json::from_str(&pi_js::json::stringify(&serde_json::to_value(&null_msg).unwrap())).unwrap();
    assert_eq!(roundtrip.details, Some(serde_json::Value::Null));
    let omitted = base(None);
    let roundtrip_omitted: ToolResultMessage =
        serde_json::from_str(&pi_js::json::stringify(&serde_json::to_value(&omitted).unwrap())).unwrap();
    assert_eq!(roundtrip_omitted.details, None);
}
