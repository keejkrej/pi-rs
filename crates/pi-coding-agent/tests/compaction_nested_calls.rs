//! Port of `packages/coding-agent/test/compaction-nested-calls.test.ts`.

use pi_agent_core::AgentMessage;
use pi_ai::{JsonObject, Message, NestedToolCallRecord, NestedToolCallStatus, NestedToolCalls, ToolResultMessage};
use pi_coding_agent::core::compaction::utils::{
    FileLists, compute_file_lists, create_file_ops, extract_file_ops_from_message,
};
use serde_json::json;

#[test]
fn include_files_touched_by_nested_calls_recorded_on_tool_results() {
    let mut a_args = JsonObject::new();
    a_args.insert("path".to_owned(), json!("a.ts"));
    let mut b_args = JsonObject::new();
    b_args.insert("path".to_owned(), json!("b.ts"));
    b_args.insert("edits".to_owned(), json!([]));

    let result = AgentMessage::Message(Message::ToolResult(ToolResultMessage {
        tool_call_id: "codemode-1".to_owned(),
        tool_name: "codemode".to_owned(),
        content: Vec::new(),
        details: None,
        usage: None,
        nested_calls: Some(NestedToolCalls {
            calls: vec![
                NestedToolCallRecord {
                    id: "codemode-1/1".to_owned(),
                    name: "read".to_owned(),
                    arguments: Some(a_args),
                    arguments_bytes: None,
                    status: NestedToolCallStatus::Ok,
                    duration_ms: None,
                    error: None,
                },
                NestedToolCallRecord {
                    id: "codemode-1/2".to_owned(),
                    name: "edit".to_owned(),
                    arguments: Some(b_args),
                    arguments_bytes: None,
                    status: NestedToolCallStatus::Ok,
                    duration_ms: None,
                    error: None,
                },
                NestedToolCallRecord {
                    id: "codemode-1/3".to_owned(),
                    name: "write".to_owned(),
                    arguments: None,
                    arguments_bytes: Some(40_000),
                    status: NestedToolCallStatus::Ok,
                    duration_ms: None,
                    error: None,
                },
            ],
            complete: false,
        }),
        is_error: false,
        timestamp: 0,
        duration_ms: None,
    }));

    let mut file_ops = create_file_ops();
    extract_file_ops_from_message(&result, &mut file_ops);
    assert_eq!(
        compute_file_lists(&file_ops),
        FileLists {
            read_files: vec!["a.ts".to_owned()],
            modified_files: vec!["b.ts".to_owned()],
        }
    );
}
