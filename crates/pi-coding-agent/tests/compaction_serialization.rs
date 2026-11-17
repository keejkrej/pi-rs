//! Port of `packages/coding-agent/test/compaction-serialization.test.ts`.

use pi_ai::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, ToolResultMessage, Usage, UserContentBlock,
    UserMessage, UserMessageContent, faux_text,
};
use pi_coding_agent::core::compaction::utils::serialize_conversation;

#[test]
fn should_truncate_long_tool_results() {
    let long_content = "x".repeat(5000);
    let messages = vec![Message::ToolResult(ToolResultMessage {
        tool_call_id: "tc1".to_owned(),
        tool_name: "read".to_owned(),
        content: vec![UserContentBlock::Text(faux_text(&long_content))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
    })];

    let result = serialize_conversation(&messages);

    assert!(result.contains("[Tool result]:"));
    assert!(result.contains("[... 3000 more characters truncated]"));
    assert!(!result.contains(&"x".repeat(3000)));
    assert!(result.contains(&"x".repeat(2000)));
}

#[test]
fn should_not_truncate_short_tool_results() {
    let short_content = "x".repeat(1500);
    let messages = vec![Message::ToolResult(ToolResultMessage {
        tool_call_id: "tc1".to_owned(),
        tool_name: "read".to_owned(),
        content: vec![UserContentBlock::Text(faux_text(&short_content))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
    })];

    let result = serialize_conversation(&messages);

    assert_eq!(result, format!("[Tool result]: {short_content}"));
    assert!(!result.contains("truncated"));
}

#[test]
fn should_not_truncate_assistant_or_user_messages() {
    let long_text = "y".repeat(5000);
    let messages = vec![
        Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(faux_text(&long_text))]),
            timestamp: 0,
        }),
        Message::Assistant(AssistantMessage {
            content: vec![AssistantContentBlock::Text(faux_text(&long_text))],
            api: "anthropic".to_owned(),
            provider: "anthropic".to_owned(),
            model: "test".to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
        }),
    ];

    let result = serialize_conversation(&messages);

    assert!(!result.contains("truncated"));
    assert!(result.contains(&long_text));
}
