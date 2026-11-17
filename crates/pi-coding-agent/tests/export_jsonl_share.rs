//! Port of `packages/coding-agent/test/export-jsonl-share.test.ts`.
// WIRE(@ca-cli-w3): full `createAgentSession` / `session.exportToJsonl` orchestration requires `core::sdk` / `core::agent_session`.
// WIRE(@tui-w3): `exportSessionForShare` UI helper resides in `modes::interactive::session_share`.

use pi_agent_core::AgentMessage;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, JsonObject, Message, StopReason, ToolResultMessage, Usage,
    UserContentBlock, UserMessage, UserMessageContent, faux_text, faux_tool_call,
};
use pi_coding_agent::core::session_export::export_session_to_jsonl;
use pi_coding_agent::core::session_manager::{
    SessionEntry, SessionManager, build_session_context, parse_session_entries,
};
use serde_json::{Value, json};
use tempfile::TempDir;

#[test]
fn adds_presentation_data_without_changing_conversation_ids_or_links() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();
    let sm = SessionManager::in_memory(Some(cwd), None, None).unwrap();

    let user_id = sm
        .append_message(AgentMessage::Message(Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_owned()),
            timestamp: 1,
        })))
        .unwrap();

    let mut args = JsonObject::new();
    args.insert("value".to_owned(), json!("example"));
    let assistant_id = sm
        .append_message(AgentMessage::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantContentBlock::ToolCall(faux_tool_call(
                "share_tool",
                args,
                None,
            ))],
            api: "anthropic-messages".to_owned(),
            provider: "anthropic".to_owned(),
            model: "claude-sonnet-4-5".to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 2,
        })))
        .unwrap();

    let result_id = sm
        .append_message(AgentMessage::Message(Message::ToolResult(ToolResultMessage {
            tool_call_id: "call-1".to_owned(),
            tool_name: "share_tool".to_owned(),
            content: vec![UserContentBlock::Text(faux_text("done"))],
            details: Some(json!({})),
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 3,
        })))
        .unwrap();

    let original_entry_ids: Vec<String> = sm.get_branch(None).into_iter().map(|e| e.id().to_owned()).collect();

    let normal_path = tmp.path().join("normal.jsonl");
    export_session_to_jsonl(&sm, Some(normal_path.to_str().unwrap()), None).unwrap();
    let normal_content = std::fs::read_to_string(&normal_path).unwrap();
    let normal_records: Vec<Value> = normal_content
        .trim()
        .lines()
        .map(|l| pi_js::json::parse(l).unwrap())
        .collect();
    assert!(
        !normal_records
            .iter()
            .any(|r| r.get("type").and_then(Value::as_str) == Some("custom")
                && r.get("customType").and_then(Value::as_str) == Some("pi.share"))
    );

    let share_path = tmp.path().join("share.jsonl");
    let expected_system_prompt = "share test prompt";
    let expected_tools = json!([{
        "name": "share_tool",
        "description": "Render a value for sharing",
        "parameters": { "type": "object", "properties": { "value": { "type": "string", "description": "Value to render" } } }
    }]);
    let trailing = |leaf_id: Option<&str>, _timestamp: &str| -> Vec<Value> {
        vec![json!({
            "type": "custom",
            "id": "share-entry-1",
            "parentId": leaf_id,
            "timestamp": pi_js::time::iso_now(),
            "customType": "pi.share",
            "data": {
                "systemPrompt": expected_system_prompt,
                "tools": expected_tools,
            },
        })]
    };
    export_session_to_jsonl(&sm, Some(share_path.to_str().unwrap()), Some(&trailing)).unwrap();

    let share_content = std::fs::read_to_string(&share_path).unwrap();
    let records: Vec<Value> = share_content
        .trim()
        .lines()
        .map(|l| pi_js::json::parse(l).unwrap())
        .collect();
    let conversation_records = &records[1..records.len() - 1];
    let exported_ids: Vec<&str> = conversation_records
        .iter()
        .map(|r| r.get("id").and_then(Value::as_str).unwrap())
        .collect();
    assert_eq!(
        exported_ids,
        original_entry_ids.iter().map(String::as_str).collect::<Vec<_>>()
    );
    let parent_ids: Vec<Option<&str>> = conversation_records
        .iter()
        .map(|r| r.get("parentId").and_then(Value::as_str))
        .collect();
    assert_eq!(
        parent_ids,
        vec![None, Some(user_id.as_str()), Some(assistant_id.as_str())]
    );
    assert_eq!(
        &exported_ids[exported_ids.len() - 3..],
        &[user_id.as_str(), assistant_id.as_str(), result_id.as_str()]
    );

    let share_record = records.last().unwrap();
    assert_eq!(share_record.get("type").and_then(Value::as_str), Some("custom"));
    assert_eq!(share_record.get("customType").and_then(Value::as_str), Some("pi.share"));
    assert_eq!(
        share_record.get("parentId").and_then(Value::as_str),
        Some(result_id.as_str())
    );
    let data = share_record.get("data").unwrap();
    assert_eq!(
        data.get("systemPrompt").and_then(Value::as_str),
        Some(expected_system_prompt)
    );
    assert!(data.get("renderedTools").is_none());
    assert!(data.get("theme").is_none());
    assert!(data.get("version").is_none());

    let parsed: Vec<SessionEntry> = parse_session_entries(&share_content)
        .into_iter()
        .filter_map(|fe| fe.into_session_entry())
        .collect();
    let imported = SessionManager::open(share_path.to_str().unwrap(), None, None).unwrap();
    assert_eq!(imported.get_leaf_id().as_deref(), Some("share-entry-1"));
    let ctx = build_session_context(&parsed, None, None);
    let roles: Vec<Option<&str>> = ctx.messages.iter().map(|m| m.role()).collect();
    assert_eq!(roles, vec![Some("user"), Some("assistant"), Some("toolResult")]);
}
