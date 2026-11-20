//! Port of `packages/coding-agent/test/session-manager/save-entry.test.ts`.

use pi_agent_core::AgentMessage;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, Usage, UserMessage, UserMessageContent, faux_text,
};
use pi_coding_agent::core::session_manager::{SessionEntry, SessionManager};
use serde_json::json;

#[test]
fn saves_custom_entries_and_includes_them_in_tree_traversal() {
    let session = SessionManager::in_memory(None, None, None).unwrap();

    let msg_id = session
        .append_message(AgentMessage::Message(Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_owned()),
            timestamp: 1,
        })))
        .unwrap();
    let custom_id = session
        .append_custom_entry("my_data", Some(json!({ "foo": "bar" })))
        .unwrap();
    let msg2_id = session
        .append_message(AgentMessage::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantContentBlock::Text(faux_text("hi"))],
            api: "anthropic-messages".to_owned(),
            provider: "anthropic".to_owned(),
            model: "test".to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: 2,
                cost: Default::default(),
            },
            stop_reason: StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 2,
            duration_ms: None,
        })))
        .unwrap();

    let entries = session.get_entries();
    assert_eq!(entries.len(), 3);
    let custom = entries
        .iter()
        .find_map(|e| match e {
            SessionEntry::Custom(c) => Some(c),
            _ => None,
        })
        .unwrap();
    assert_eq!(custom.custom_type, "my_data");
    assert_eq!(custom.data, Some(json!({ "foo": "bar" })));
    assert_eq!(custom.id, custom_id);
    assert_eq!(custom.parent_id.as_deref(), Some(msg_id.as_str()));

    let path = session.get_branch(None);
    assert_eq!(
        path.iter().map(|e| e.id()).collect::<Vec<_>>(),
        vec![msg_id.as_str(), custom_id.as_str(), msg2_id.as_str()]
    );
    assert_eq!(session.build_session_context().messages.len(), 2);
}
