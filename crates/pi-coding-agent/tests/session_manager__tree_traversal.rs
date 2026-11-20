//! Port of `packages/coding-agent/test/session-manager/tree-traversal.test.ts`.

use std::path::Path;

use pi_agent_core::AgentMessage;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, TextContent, ToolResultMessage, Usage, UsageCost,
    UserContentBlock, UserMessage, UserMessageContent, faux_text,
};
use pi_coding_agent::core::session_manager::{SessionEntry, SessionManager};
use serde_json::{Value, json};
use tempfile::TempDir;

fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_owned()),
        timestamp: 1,
    }))
}

fn assistant_msg(text: &str) -> AgentMessage {
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
        timestamp: 2,
        duration_ms: None,
    }))
}

fn read_session_file_roles(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = pi_js::json::parse(l).unwrap();
            let t = v.get("type").and_then(Value::as_str).unwrap_or("");
            if t == "message" {
                v.get("message")
                    .and_then(|m| m.get("role"))
                    .and_then(Value::as_str)
                    .unwrap_or("message")
                    .to_owned()
            } else {
                t.to_owned()
            }
        })
        .collect()
}

fn sample_usage() -> Usage {
    Usage {
        input: 10,
        output: 20,
        cache_read: 30,
        cache_write: 40,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 100,
        cost: UsageCost {
            input: 0.1,
            output: 0.2,
            cache_read: 0.3,
            cache_write: 0.4,
            total: 1.0,
            total_from_provider: None,
        },
    }
}

#[test]
fn append_operations_and_leaf_pointer() {
    let sm = SessionManager::in_memory(None, None, None).unwrap();
    assert_eq!(sm.get_leaf_id(), None);
    let id1 = sm.append_message(user_msg("1")).unwrap();
    assert_eq!(sm.get_leaf_id().as_deref(), Some(id1.as_str()));
    let id2 = sm.append_message(assistant_msg("2")).unwrap();
    assert_eq!(sm.get_leaf_id().as_deref(), Some(id2.as_str()));
    let id3 = sm.append_message(user_msg("3")).unwrap();
    assert_eq!(sm.get_leaf_id().as_deref(), Some(id3.as_str()));

    let entries = sm.get_entries();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].parent_id(), None);
    assert_eq!(entries[1].parent_id(), Some(id1.as_str()));
    assert_eq!(entries[2].parent_id(), Some(id2.as_str()));

    // Thinking level change, model change, compaction, custom
    let sm2 = SessionManager::in_memory(None, None, None).unwrap();
    let m1 = sm2.append_message(user_msg("hello")).unwrap();
    let t1 = sm2.append_thinking_level_change("high").unwrap();
    let mc = sm2.append_model_change("openai", "gpt-4").unwrap();
    let c1 = sm2
        .append_custom_entry("my_data", Some(json!({ "key": "value" })))
        .unwrap();
    let comp = sm2
        .append_compaction("summary", Some(&m1), 1000, None, Some(false), Some(sample_usage()))
        .unwrap();
    let m2 = sm2.append_message(assistant_msg("resp")).unwrap();
    assert_eq!(sm2.get_entry(&t1).unwrap().parent_id(), Some(m1.as_str()));
    assert_eq!(sm2.get_entry(&mc).unwrap().parent_id(), Some(t1.as_str()));
    assert_eq!(sm2.get_entry(&c1).unwrap().parent_id(), Some(mc.as_str()));
    assert_eq!(sm2.get_entry(&comp).unwrap().parent_id(), Some(c1.as_str()));
    assert_eq!(sm2.get_entry(&m2).unwrap().parent_id(), Some(comp.as_str()));
    match sm2.get_entry(&comp).unwrap() {
        SessionEntry::Compaction(c) => {
            assert_eq!(c.summary, "summary");
            assert_eq!(c.first_kept_entry_id, m1);
            assert_eq!(c.usage, Some(sample_usage()));
        }
        other => panic!("expected compaction: {other:?}"),
    }
}

#[test]
fn get_branch_get_tree_and_branch_navigation() {
    let empty = SessionManager::in_memory(None, None, None).unwrap();
    assert!(empty.get_branch(None).is_empty());
    assert!(empty.get_tree().is_empty());

    let sm = SessionManager::in_memory(None, None, None).unwrap();
    let id1 = sm.append_message(user_msg("1")).unwrap();
    let id2 = sm.append_message(assistant_msg("2")).unwrap();
    let id3 = sm.append_message(user_msg("3")).unwrap();
    let _id4 = sm.append_message(assistant_msg("4")).unwrap();

    assert_eq!(
        sm.get_branch(Some(&id2)).iter().map(|e| e.id()).collect::<Vec<_>>(),
        vec![id1.as_str(), id2.as_str()]
    );

    sm.branch(&id2).unwrap();
    let id5 = sm.append_message(user_msg("5")).unwrap();
    let _id6 = sm.append_message(assistant_msg("6")).unwrap();
    sm.branch(&id5).unwrap();
    let _id7 = sm.append_message(user_msg("7")).unwrap();

    let tree = sm.get_tree();
    assert_eq!(tree.len(), 1);
    let node2 = &tree[0].children[0];
    assert_eq!(node2.children.len(), 2);
    let node5 = node2.children.iter().find(|c| c.entry.id() == id5).unwrap();
    assert_eq!(node5.children.len(), 2);
    let node3 = node2.children.iter().find(|c| c.entry.id() == id3).unwrap();
    assert_eq!(node3.children.len(), 1);

    assert!(sm.branch("nonexistent").unwrap_err().to_string().contains("not found"));

    // branch_with_summary
    let bs_sm = SessionManager::in_memory(None, None, None).unwrap();
    let b1 = bs_sm.append_message(user_msg("1")).unwrap();
    let _b2 = bs_sm.append_message(assistant_msg("2")).unwrap();
    let b3 = bs_sm.append_message(user_msg("3")).unwrap();
    let sum_id = bs_sm
        .branch_with_summary(
            Some(&b1),
            "Summary of abandoned work",
            None,
            Some(false),
            Some(sample_usage()),
        )
        .unwrap();
    assert_eq!(bs_sm.get_leaf_id().as_deref(), Some(sum_id.as_str()));
    match bs_sm.get_entry(&sum_id).unwrap() {
        SessionEntry::BranchSummary(bs) => {
            assert_eq!(bs.parent_id.as_deref(), Some(b1.as_str()));
            assert_eq!(bs.from_id, b3);
            assert_eq!(bs.usage, Some(sample_usage()));
        }
        other => panic!("expected branch summary: {other:?}"),
    }
    assert!(
        bs_sm
            .branch_with_summary(Some("nonexistent"), "s", None, None, None)
            .is_err()
    );

    // get_leaf_entry and get_entry
    assert_eq!(empty.get_leaf_entry(), None);
    assert_eq!(empty.get_entry("missing"), None);
    assert_eq!(bs_sm.get_leaf_entry().unwrap().id(), sum_id);
}

#[test]
fn create_branched_session_cases_and_usage_roundtrip() {
    let sm = SessionManager::in_memory(None, None, None).unwrap();
    sm.append_message(user_msg("hello")).unwrap();
    assert!(sm.create_branched_session("nonexistent").is_err());

    // In-memory extraction
    let mem = SessionManager::in_memory(None, None, None).unwrap();
    let id1 = mem.append_message(user_msg("1")).unwrap();
    let id2 = mem.append_message(assistant_msg("2")).unwrap();
    let id3 = mem.append_message(user_msg("3")).unwrap();
    mem.append_message(assistant_msg("4")).unwrap();
    mem.branch(&id3).unwrap();
    mem.append_message(user_msg("5")).unwrap();
    assert_eq!(mem.create_branched_session(&id2).unwrap(), None);
    assert_eq!(
        mem.get_entries().iter().map(|e| e.id()).collect::<Vec<_>>(),
        vec![id1.as_str(), id2.as_str()]
    );

    // Persisted dedup before first user message
    let tmp = TempDir::new().unwrap();
    let dir_str = tmp.path().to_str().unwrap();
    let dedup = SessionManager::create(dir_str, Some(dir_str), None).unwrap();
    let mc = dedup.append_model_change("anthropic", "claude-sonnet-4-5").unwrap();
    dedup.append_message(user_msg("first question")).unwrap();
    dedup.append_message(assistant_msg("first answer")).unwrap();
    let new_file = dedup.create_branched_session(&mc).unwrap().unwrap();
    assert!(!Path::new(&new_file).exists());
    dedup.append_message(user_msg("new question")).unwrap();
    assert!(Path::new(&new_file).exists());
    dedup
        .append_custom_entry("preset-state", Some(json!({ "name": "plan" })))
        .unwrap();
    dedup.append_message(assistant_msg("new answer")).unwrap();
    assert_eq!(
        read_session_file_roles(Path::new(&new_file)),
        vec!["session", "model_change", "user", "custom", "assistant"]
    );

    // Usage roundtrip across reload
    let usage = sample_usage();
    let roundtrip = SessionManager::create(dir_str, Some(dir_str), None).unwrap();
    let root_id = roundtrip.append_message(user_msg("question")).unwrap();
    roundtrip.append_message(assistant_msg("answer")).unwrap();
    roundtrip
        .append_message(AgentMessage::Message(Message::ToolResult(ToolResultMessage {
            tool_call_id: "call-1".to_owned(),
            tool_name: "nested-model".to_owned(),
            content: vec![UserContentBlock::Text(TextContent {
                text: "result".to_owned(),
                text_signature: None,
            })],
            details: None,
            is_error: false,
            nested_calls: None,
            usage: Some(usage.clone()),
            timestamp: 3,
            duration_ms: None,
        })))
        .unwrap();
    roundtrip
        .append_compaction("summary", Some(&root_id), 100, None, Some(false), Some(usage.clone()))
        .unwrap();
    roundtrip
        .branch_with_summary(Some(&root_id), "branch summary", None, Some(false), Some(usage.clone()))
        .unwrap();
    let rt_file = roundtrip.get_session_file().unwrap();
    let reopened = SessionManager::open(&rt_file, Some(dir_str), None).unwrap();
    let entries = reopened.get_entries();
    assert!(
        entries
            .iter()
            .any(|e| matches!(e, SessionEntry::Compaction(c) if c.usage.as_ref() == Some(&usage)))
    );
    assert!(
        entries
            .iter()
            .any(|e| matches!(e, SessionEntry::BranchSummary(b) if b.usage.as_ref() == Some(&usage)))
    );
    assert!(entries.iter().any(|e| matches!(
        e,
        SessionEntry::Message(m) if matches!(&m.message, AgentMessage::Message(Message::ToolResult(tr)) if tr.usage.as_ref() == Some(&usage))
    )));

    // Writes immediately when forking at user message
    let immediate = SessionManager::create(dir_str, Some(dir_str), None).unwrap();
    let u1 = immediate.append_message(user_msg("first question")).unwrap();
    immediate.append_message(assistant_msg("first answer")).unwrap();
    let forked_file = immediate.create_branched_session(&u1).unwrap().unwrap();
    assert!(Path::new(&forked_file).exists());
    immediate.append_message(assistant_msg("new answer")).unwrap();
    assert_eq!(
        read_session_file_roles(Path::new(&forked_file)),
        vec!["session", "user", "assistant"]
    );
}
