//! Port of packages/coding-agent/test/suite/regressions/8989-fork-compaction-label-boundary.test.ts

#[path = "support/mod.rs"]
mod support;

use pi_agent_core::types::AgentMessage;
use pi_ai::types::{Message, TextContent, UserContentBlock, UserMessage, UserMessageContent};
use pi_coding_agent::core::extensions::types::SessionEntry;
use pi_coding_agent::core::session_manager::SessionManager;
use pretty_assertions::assert_eq;

fn user_message(text: &str) -> AgentMessage {
    AgentMessage::from(Message::User(UserMessage {
        content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
        timestamp: pi_js::time::now_ms(),
    }))
}

#[test]
fn resolves_compaction_first_kept_entry_id_to_next_kept_entry_when_boundary_is_stripped_label() {
    let sm = SessionManager::in_memory(None, None, None).unwrap();

    let old_id = sm.append_message(user_message("old")).unwrap();
    let label_id = sm.append_label_change(&old_id, Some("checkpoint")).unwrap();
    let kept_id = sm.append_message(user_message("kept")).unwrap();
    let compaction_id = sm
        .append_compaction("summary", Some(&label_id), 100, None, None, None)
        .unwrap();
    let leaf_id = sm.append_message(user_message("after")).unwrap();

    let branched = sm.create_branched_session(&leaf_id).unwrap();
    assert!(branched.is_none(), "in-memory branched session should return None file");

    match sm.get_entry(&compaction_id) {
        Some(SessionEntry::Compaction(c)) => assert_eq!(c.first_kept_entry_id, kept_id),
        other => panic!("expected compaction entry, got {other:?}"),
    }

    let ctx_msgs = sm.build_session_context().messages;
    let summary_and_users: Vec<String> = ctx_msgs
        .into_iter()
        .map(|m| match m {
            AgentMessage::Custom(v) if v.get("role").and_then(|r| r.as_str()) == Some("compactionSummary") => {
                format!(
                    "compactionSummary:{}",
                    v.get("summary").and_then(|s| s.as_str()).unwrap_or("")
                )
            }
            AgentMessage::Message(Message::User(u)) => match u.content {
                UserMessageContent::Text(t) => format!("user:{t}"),
                UserMessageContent::Blocks(bs) => {
                    let s: String = bs
                        .into_iter()
                        .filter_map(|b| match b {
                            UserContentBlock::Text(t) => Some(t.text),
                            _ => None,
                        })
                        .collect();
                    format!("user:{s}")
                }
            },
            other => format!("other:{other:?}"),
        })
        .collect();

    assert_eq!(
        summary_and_users,
        vec![
            "compactionSummary:summary".to_string(),
            "user:kept".to_string(),
            "user:after".to_string(),
        ]
    );
}
