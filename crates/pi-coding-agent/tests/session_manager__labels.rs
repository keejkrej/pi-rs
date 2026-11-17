//! Port of `packages/coding-agent/test/session-manager/labels.test.ts`.

use pi_agent_core::AgentMessage;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, Usage, UserMessage, UserMessageContent, faux_text,
};
use pi_coding_agent::core::session_manager::{LabelEntry, SessionEntry, SessionManager};

fn user_msg(text: &str, ts: i64) -> AgentMessage {
    AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_owned()),
        timestamp: ts,
    }))
}

fn assistant_msg(text: &str, ts: i64) -> AgentMessage {
    AgentMessage::Message(Message::Assistant(AssistantMessage {
        content: vec![AssistantContentBlock::Text(faux_text(text))],
        api: "anthropic-messages".to_owned(),
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
        timestamp: ts,
    }))
}

#[test]
fn sets_clears_and_resolves_latest_label() {
    let session = SessionManager::in_memory(None, None, None).unwrap();
    let msg_id = session.append_message(user_msg("hello", 1)).unwrap();
    assert_eq!(session.get_label(&msg_id), None);

    let label_id = session.append_label_change(&msg_id, Some("checkpoint")).unwrap();
    assert_eq!(session.get_label(&msg_id).as_deref(), Some("checkpoint"));

    let label_entry = session
        .get_entries()
        .into_iter()
        .find_map(|e| match e {
            SessionEntry::Label(l) => Some(l),
            _ => None,
        })
        .unwrap();
    assert_eq!(label_entry.id, label_id);
    assert_eq!(label_entry.target_id, msg_id);
    assert_eq!(label_entry.label.as_deref(), Some("checkpoint"));

    session.append_label_change(&msg_id, None).unwrap();
    assert_eq!(session.get_label(&msg_id), None);

    session.append_label_change(&msg_id, Some("first")).unwrap();
    session.append_label_change(&msg_id, Some("second")).unwrap();
    let last_id = session.append_label_change(&msg_id, Some("third")).unwrap();
    assert_eq!(session.get_label(&msg_id).as_deref(), Some("third"));

    let last_ts = match session.get_entry(&last_id).unwrap() {
        SessionEntry::Label(l) => l.timestamp,
        other => panic!("expected label: {other:?}"),
    };
    let tree = session.get_tree();
    let node = tree.iter().find(|n| n.entry.id() == msg_id).unwrap();
    assert_eq!(node.label_timestamp.as_deref(), Some(last_ts.as_str()));
}

#[test]
fn tree_nodes_and_create_branched_session_preserve_only_path_labels() {
    let session = SessionManager::in_memory(None, None, None).unwrap();
    let msg1_id = session.append_message(user_msg("hello", 1)).unwrap();
    let msg2_id = session.append_message(assistant_msg("hi", 2)).unwrap();
    let msg3_id = session.append_message(user_msg("followup", 3)).unwrap();

    let l1_id = session.append_label_change(&msg1_id, Some("start")).unwrap();
    let l2_id = session.append_label_change(&msg2_id, Some("response")).unwrap();
    session.append_label_change(&msg3_id, Some("third")).unwrap();

    let l1_ts = session.get_entry(&l1_id).unwrap().timestamp().to_owned();
    let l2_ts = session.get_entry(&l2_id).unwrap().timestamp().to_owned();

    let tree = session.get_tree();
    let n1 = tree.iter().find(|n| n.entry.id() == msg1_id).unwrap();
    assert_eq!(n1.label.as_deref(), Some("start"));
    assert_eq!(n1.label_timestamp.as_deref(), Some(l1_ts.as_str()));
    let n2 = n1.children.iter().find(|n| n.entry.id() == msg2_id).unwrap();
    assert_eq!(n2.label.as_deref(), Some("response"));
    assert_eq!(n2.label_timestamp.as_deref(), Some(l2_ts.as_str()));

    session.create_branched_session(&msg2_id).unwrap();
    assert_eq!(session.get_label(&msg1_id).as_deref(), Some("start"));
    assert_eq!(session.get_label(&msg2_id).as_deref(), Some("response"));
    assert_eq!(session.get_label(&msg3_id), None);

    let label_entries: Vec<LabelEntry> = session
        .get_entries()
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::Label(l) => Some(l),
            _ => None,
        })
        .collect();
    assert_eq!(label_entries.len(), 2);

    // Rewires children of removed labels when forking
    let rewire = SessionManager::in_memory(None, None, None).unwrap();
    let m1 = rewire.append_message(user_msg("hello", 1)).unwrap();
    rewire.append_label_change(&m1, Some("checkpoint")).unwrap();
    let mc = rewire.append_model_change("anthropic", "claude-test").unwrap();
    let m2 = rewire.append_message(user_msg("followup", 2)).unwrap();
    rewire.create_branched_session(&m2).unwrap();
    assert_eq!(rewire.get_entry(&mc).unwrap().parent_id(), Some(m1.as_str()));

    // Labels are excluded from build_session_context
    let ctx_sm = SessionManager::in_memory(None, None, None).unwrap();
    let c1 = ctx_sm.append_message(user_msg("hello", 1)).unwrap();
    ctx_sm.append_label_change(&c1, Some("checkpoint")).unwrap();
    assert_eq!(ctx_sm.build_session_context().messages.len(), 1);

    let err = ctx_sm.append_label_change("non-existent", Some("label")).unwrap_err();
    assert!(err.to_string().contains("Entry non-existent not found"));
}
