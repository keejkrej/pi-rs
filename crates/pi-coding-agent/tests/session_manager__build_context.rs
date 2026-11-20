//! Port of `packages/coding-agent/test/session-manager/build-context.test.ts`.

use pi_agent_core::AgentMessage;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, Usage, UserContentBlock, UserMessage,
    UserMessageContent, faux_text,
};
use pi_coding_agent::core::session_manager::{
    BranchSummaryEntry, CompactionEntry, CustomEntry, ModelChangeEntry, SessionEntry, SessionMessageEntry,
    SessionModelRef, ThinkingLevelChangeEntry, build_context_entries, build_session_context,
};
use serde_json::{Value, json};

fn msg_user(id: &str, parent_id: Option<&str>, text: &str) -> SessionEntry {
    SessionEntry::Message(SessionMessageEntry {
        id: id.to_owned(),
        parent_id: parent_id.map(ToOwned::to_owned),
        timestamp: "2025-01-01T00:00:00Z".to_owned(),
        message: AgentMessage::Message(Message::User(UserMessage {
            content: UserMessageContent::Text(text.to_owned()),
            timestamp: 1,
        })),
    })
}

fn msg_assistant(id: &str, parent_id: Option<&str>, text: &str) -> SessionEntry {
    SessionEntry::Message(SessionMessageEntry {
        id: id.to_owned(),
        parent_id: parent_id.map(ToOwned::to_owned),
        timestamp: "2025-01-01T00:00:00Z".to_owned(),
        message: AgentMessage::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantContentBlock::Text(faux_text(text))],
            api: "anthropic-messages".to_owned(),
            provider: "anthropic".to_owned(),
            model: "claude-test".to_owned(),
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
            timestamp: 1,
            duration_ms: None,
        })),
    })
}

fn compaction(id: &str, parent_id: Option<&str>, summary: &str, first_kept: &str) -> SessionEntry {
    SessionEntry::Compaction(CompactionEntry {
        id: id.to_owned(),
        parent_id: parent_id.map(ToOwned::to_owned),
        timestamp: "2025-01-01T00:00:00Z".to_owned(),
        summary: summary.to_owned(),
        first_kept_entry_id: first_kept.to_owned(),
        first_kept_entry_index: None,
        tokens_before: 1000,
        details: None,
        from_hook: None,
        usage: None,
        system_message: None,
    })
}

fn branch_summary(id: &str, parent_id: Option<&str>, summary: &str, from_id: &str) -> SessionEntry {
    SessionEntry::BranchSummary(BranchSummaryEntry {
        id: id.to_owned(),
        parent_id: parent_id.map(ToOwned::to_owned),
        timestamp: "2025-01-01T00:00:00Z".to_owned(),
        from_id: from_id.to_owned(),
        summary: summary.to_owned(),
        details: None,
        usage: None,
        from_hook: None,
    })
}

fn custom(id: &str, parent_id: Option<&str>, custom_type: &str, data: Option<Value>) -> SessionEntry {
    SessionEntry::Custom(CustomEntry {
        id: id.to_owned(),
        parent_id: parent_id.map(ToOwned::to_owned),
        timestamp: "2025-01-01T00:00:00Z".to_owned(),
        custom_type: custom_type.to_owned(),
        data,
    })
}

fn thinking_level(id: &str, parent_id: Option<&str>, level: &str) -> SessionEntry {
    SessionEntry::ThinkingLevelChange(ThinkingLevelChangeEntry {
        id: id.to_owned(),
        parent_id: parent_id.map(ToOwned::to_owned),
        timestamp: "2025-01-01T00:00:00Z".to_owned(),
        thinking_level: level.to_owned(),
    })
}

fn model_change(id: &str, parent_id: Option<&str>, provider: &str, model_id: &str) -> SessionEntry {
    SessionEntry::ModelChange(ModelChangeEntry {
        id: id.to_owned(),
        parent_id: parent_id.map(ToOwned::to_owned),
        timestamp: "2025-01-01T00:00:00Z".to_owned(),
        provider: provider.to_owned(),
        model_id: model_id.to_owned(),
    })
}

fn msg_text(m: &AgentMessage) -> String {
    match m {
        AgentMessage::Message(Message::User(u)) => match &u.content {
            UserMessageContent::Text(s) => s.clone(),
            UserMessageContent::Blocks(b) => b
                .iter()
                .filter_map(|p| match p {
                    UserContentBlock::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect(),
        },
        AgentMessage::Message(Message::Assistant(a)) => a
            .content
            .iter()
            .filter_map(|p| match p {
                AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect(),
        AgentMessage::Custom(v) => v.get("summary").and_then(Value::as_str).unwrap_or_default().to_owned(),
        _ => String::new(),
    }
}

#[test]
fn trivial_cases() {
    let empty = build_session_context(&[], None, None);
    assert!(empty.messages.is_empty());
    assert_eq!(empty.thinking_level, "off");
    assert_eq!(empty.model, None);

    let single = build_session_context(&[msg_user("1", None, "hello")], None, None);
    assert_eq!(single.messages.len(), 1);
    assert_eq!(single.messages[0].role(), Some("user"));

    let conv = build_session_context(
        &[
            msg_user("1", None, "hello"),
            msg_assistant("2", Some("1"), "hi there"),
            msg_user("3", Some("2"), "how are you"),
            msg_assistant("4", Some("3"), "great"),
        ],
        None,
        None,
    );
    assert_eq!(
        conv.messages.iter().map(|m| m.role()).collect::<Vec<_>>(),
        vec![Some("user"), Some("assistant"), Some("user"), Some("assistant")]
    );

    let thinking = build_session_context(
        &[
            msg_user("1", None, "hello"),
            thinking_level("2", Some("1"), "high"),
            msg_assistant("3", Some("2"), "thinking hard"),
        ],
        None,
        None,
    );
    assert_eq!(thinking.thinking_level, "high");
    assert_eq!(thinking.messages.len(), 2);

    let from_asst = build_session_context(
        &[msg_user("1", None, "hello"), msg_assistant("2", Some("1"), "hi")],
        None,
        None,
    );
    assert_eq!(
        from_asst.model,
        Some(SessionModelRef {
            provider: "anthropic".to_owned(),
            model_id: "claude-test".to_owned(),
        })
    );

    let from_change = build_session_context(
        &[
            msg_user("1", None, "hello"),
            model_change("2", Some("1"), "openai", "gpt-4"),
            msg_assistant("3", Some("2"), "hi"),
        ],
        None,
        None,
    );
    assert_eq!(
        from_change.model,
        Some(SessionModelRef {
            provider: "anthropic".to_owned(),
            model_id: "claude-test".to_owned(),
        })
    );
}

#[test]
fn with_compaction() {
    let entries = vec![
        msg_user("1", None, "first"),
        msg_assistant("2", Some("1"), "response1"),
        msg_user("3", Some("2"), "second"),
        msg_assistant("4", Some("3"), "response2"),
        compaction("5", Some("4"), "Summary of first two turns", "3"),
        msg_user("6", Some("5"), "third"),
        msg_assistant("7", Some("6"), "response3"),
    ];
    let ctx = build_session_context(&entries, None, None);
    assert_eq!(ctx.messages.len(), 5);
    assert!(msg_text(&ctx.messages[0]).contains("Summary of first two turns"));
    assert_eq!(msg_text(&ctx.messages[1]), "second");
    assert_eq!(msg_text(&ctx.messages[2]), "response2");
    assert_eq!(msg_text(&ctx.messages[3]), "third");
    assert_eq!(msg_text(&ctx.messages[4]), "response3");

    let keep_first = build_session_context(
        &[
            msg_user("1", None, "first"),
            msg_assistant("2", Some("1"), "response"),
            compaction("3", Some("2"), "Empty summary", "1"),
            msg_user("4", Some("3"), "second"),
        ],
        None,
        None,
    );
    assert_eq!(keep_first.messages.len(), 4);
    assert!(msg_text(&keep_first.messages[0]).contains("Empty summary"));

    let multi = build_session_context(
        &[
            msg_user("1", None, "a"),
            msg_assistant("2", Some("1"), "b"),
            compaction("3", Some("2"), "First summary", "1"),
            msg_user("4", Some("3"), "c"),
            msg_assistant("5", Some("4"), "d"),
            compaction("6", Some("5"), "Second summary", "4"),
            msg_user("7", Some("6"), "e"),
        ],
        None,
        None,
    );
    assert_eq!(multi.messages.len(), 4);
    assert!(msg_text(&multi.messages[0]).contains("Second summary"));

    let with_custom = vec![
        msg_user("1", None, "first"),
        custom("2", Some("1"), "old-state", Some(json!({ "hidden": true }))),
        msg_assistant("3", Some("2"), "response1"),
        custom("4", Some("3"), "kept-card", Some(json!({ "title": "Kept" }))),
        msg_user("5", Some("4"), "second"),
        compaction("6", Some("5"), "Summary", "4"),
        custom("7", Some("6"), "after-card", Some(json!({ "title": "After" }))),
        msg_assistant("8", Some("7"), "response2"),
    ];
    let kept_entries = build_context_entries(&with_custom, None, None);
    let kept_ids: Vec<&str> = kept_entries.iter().map(|e| e.id()).collect();
    assert_eq!(kept_ids, vec!["6", "4", "5", "7", "8"]);
    let ctx_custom = build_session_context(&with_custom, None, None);
    assert_eq!(
        ctx_custom.messages.iter().map(|m| m.role()).collect::<Vec<_>>(),
        vec![Some("compactionSummary"), Some("user"), Some("assistant")]
    );

    let full_settings = build_session_context(
        &[
            msg_user("1", None, "first"),
            thinking_level("2", Some("1"), "high"),
            msg_assistant("3", Some("2"), "response1"),
            msg_user("4", Some("3"), "second"),
            compaction("5", Some("4"), "Summary", "4"),
        ],
        None,
        None,
    );
    assert_eq!(full_settings.thinking_level, "high");
    assert_eq!(
        full_settings.messages.iter().map(|m| m.role()).collect::<Vec<_>>(),
        vec![Some("compactionSummary"), Some("user")]
    );
}

#[test]
fn with_branches_and_edge_cases() {
    let entries = vec![
        msg_user("1", None, "start"),
        msg_assistant("2", Some("1"), "response"),
        msg_user("3", Some("2"), "branch A"),
        msg_user("4", Some("2"), "branch B"),
    ];
    let ctx_a = build_session_context(&entries, Some(Some("3")), None);
    assert_eq!(msg_text(&ctx_a.messages[2]), "branch A");
    let ctx_b = build_session_context(&entries, Some(Some("4")), None);
    assert_eq!(msg_text(&ctx_b.messages[2]), "branch B");

    let with_bs = build_session_context(
        &[
            msg_user("1", None, "start"),
            msg_assistant("2", Some("1"), "response"),
            msg_user("3", Some("2"), "abandoned path"),
            branch_summary("4", Some("2"), "Summary of abandoned work", "3"),
            msg_user("5", Some("4"), "new direction"),
        ],
        Some(Some("5")),
        None,
    );
    assert_eq!(with_bs.messages.len(), 4);
    assert!(msg_text(&with_bs.messages[2]).contains("Summary of abandoned work"));
    assert_eq!(msg_text(&with_bs.messages[3]), "new direction");

    let complex = vec![
        msg_user("1", None, "start"),
        msg_assistant("2", Some("1"), "r1"),
        msg_user("3", Some("2"), "q2"),
        msg_assistant("4", Some("3"), "r2"),
        compaction("5", Some("4"), "Compacted history", "3"),
        msg_user("6", Some("5"), "q3"),
        msg_assistant("7", Some("6"), "r3"),
        msg_user("8", Some("3"), "wrong path"),
        msg_assistant("9", Some("8"), "wrong response"),
        branch_summary("10", Some("3"), "Tried wrong approach", "9"),
        msg_user("11", Some("10"), "better approach"),
    ];
    let ctx_main = build_session_context(&complex, Some(Some("7")), None);
    assert_eq!(ctx_main.messages.len(), 5);
    assert!(msg_text(&ctx_main.messages[0]).contains("Compacted history"));
    assert_eq!(msg_text(&ctx_main.messages[4]), "r3");

    let ctx_branch = build_session_context(&complex, Some(Some("11")), None);
    assert_eq!(ctx_branch.messages.len(), 5);
    assert!(msg_text(&ctx_branch.messages[3]).contains("Tried wrong approach"));
    assert_eq!(msg_text(&ctx_branch.messages[4]), "better approach");

    // Not found -> fallback to last entry
    let fallback = build_session_context(
        &[msg_user("1", None, "hello"), msg_assistant("2", Some("1"), "hi")],
        Some(Some("nonexistent")),
        None,
    );
    assert_eq!(fallback.messages.len(), 2);

    // Orphaned entry -> only orphan returned
    let orphan = build_session_context(
        &[
            msg_user("1", None, "hello"),
            msg_assistant("2", Some("missing"), "orphan"),
        ],
        Some(Some("2")),
        None,
    );
    assert_eq!(orphan.messages.len(), 1);
}
