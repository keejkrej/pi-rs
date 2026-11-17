//! Port of `packages/coding-agent/test/compaction.test.ts`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_agent_core::AgentMessage;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, JsonObject, Message, StopReason, SystemMessage, SystemMessageContent,
    ToolResultMessage, Usage, UserContentBlock, UserMessage, UserMessageContent, faux_text, faux_tool_call,
};
use pi_coding_agent::core::compaction::{
    CompactionSettings, DEFAULT_COMPACTION_SETTINGS, calculate_context_tokens, estimate_context_tokens, find_cut_point,
    get_last_assistant_usage, prepare_compaction, should_compact,
};
use pi_coding_agent::core::messages::CustomMessageContent;
use pi_coding_agent::core::session_manager::{
    CompactionEntry, CustomMessageEntry, ModelChangeEntry, SessionEntry, SessionMessageEntry, ThinkingLevelChangeEntry,
    build_session_context, migrate_session_entries, parse_session_entries,
};
use serde_json::json;

fn load_large_session_entries() -> Vec<SessionEntry> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let session_path = manifest_dir.join("tests/fixtures/large-session.jsonl");
    let content = std::fs::read_to_string(&session_path).expect("read large-session.jsonl");
    let mut entries = parse_session_entries(&content);
    migrate_session_entries(&mut entries);
    entries.into_iter().filter_map(|e| e.into_session_entry()).collect()
}

fn mock_usage(input: i64, output: i64, cache_read: i64, cache_write: i64) -> Usage {
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: input + output + cache_read + cache_write,
        cost: Default::default(),
    }
}

fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_owned()),
        timestamp: 0,
    }))
}

fn assistant_msg(text: &str, usage: Option<Usage>) -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContentBlock::Text(faux_text(text))],
        api: "anthropic-messages".to_owned(),
        provider: "anthropic".to_owned(),
        model: "claude-sonnet-4-5".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: usage.unwrap_or_else(|| mock_usage(100, 50, 0, 0)),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
    }
}

struct EntryBuilder {
    counter: AtomicUsize,
    last_id: Option<String>,
}

impl EntryBuilder {
    fn new() -> Self {
        Self {
            counter: AtomicUsize::new(0),
            last_id: None,
        }
    }

    fn next_id(&mut self) -> (String, Option<String>) {
        let id = format!("test-id-{}", self.counter.fetch_add(1, Ordering::Relaxed));
        let parent = self.last_id.replace(id.clone());
        (id, parent)
    }

    fn message(&mut self, message: AgentMessage) -> SessionEntry {
        let (id, parent_id) = self.next_id();
        SessionEntry::Message(SessionMessageEntry {
            id,
            parent_id,
            timestamp: pi_js::time::iso_now(),
            message,
        })
    }

    fn compaction(&mut self, summary: &str, first_kept_entry_id: &str) -> SessionEntry {
        let (id, parent_id) = self.next_id();
        SessionEntry::Compaction(CompactionEntry {
            id,
            parent_id,
            timestamp: pi_js::time::iso_now(),
            summary: summary.to_owned(),
            first_kept_entry_id: first_kept_entry_id.to_owned(),
            tokens_before: 10_000,
            system_message: None,
            usage: None,
            details: None,
            from_hook: None,
            first_kept_entry_index: None,
        })
    }

    fn model_change(&mut self, provider: &str, model_id: &str) -> SessionEntry {
        let (id, parent_id) = self.next_id();
        SessionEntry::ModelChange(ModelChangeEntry {
            id,
            parent_id,
            timestamp: pi_js::time::iso_now(),
            provider: provider.to_owned(),
            model_id: model_id.to_owned(),
        })
    }

    fn thinking_level(&mut self, level: &str) -> SessionEntry {
        let (id, parent_id) = self.next_id();
        SessionEntry::ThinkingLevelChange(ThinkingLevelChangeEntry {
            id,
            parent_id,
            timestamp: pi_js::time::iso_now(),
            thinking_level: level.to_owned(),
        })
    }

    fn custom_message(&mut self, content: &str) -> SessionEntry {
        let (id, parent_id) = self.next_id();
        SessionEntry::CustomMessage(CustomMessageEntry {
            id,
            parent_id,
            timestamp: pi_js::time::iso_now(),
            custom_type: "test".to_owned(),
            content: CustomMessageContent::Text(content.to_owned()),
            display: true,
            details: None,
        })
    }
}

fn extract_text(messages: &[AgentMessage]) -> String {
    messages
        .iter()
        .map(|msg| match msg {
            AgentMessage::Message(Message::User(u)) => match &u.content {
                UserMessageContent::Text(s) => s.clone(),
                UserMessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| match b {
                        UserContentBlock::Text(t) => Some(t.text.as_str()),
                        UserContentBlock::Image(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            },
            AgentMessage::Message(Message::Assistant(a)) => pi_ai::content_text(&a.content, Some(" ")),
            AgentMessage::Custom(val) => val
                .get("summary")
                .and_then(|s| s.as_str())
                .or_else(|| val.get("content").and_then(|c| c.as_str()))
                .unwrap_or("")
                .to_owned(),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn token_calculation() {
    let usage = mock_usage(1000, 500, 200, 100);
    assert_eq!(calculate_context_tokens(&usage), 1800);
    let zero = mock_usage(0, 0, 0, 0);
    assert_eq!(calculate_context_tokens(&zero), 0);
}

#[test]
fn get_last_assistant_usage_behavior() {
    let mut b = EntryBuilder::new();
    let entries = vec![
        b.message(user_msg("Hello")),
        b.message(AgentMessage::Message(Message::Assistant(assistant_msg(
            "Hi",
            Some(mock_usage(100, 50, 0, 0)),
        )))),
        b.message(user_msg("How are you?")),
        b.message(AgentMessage::Message(Message::Assistant(assistant_msg(
            "Good",
            Some(mock_usage(200, 100, 0, 0)),
        )))),
    ];
    let usage = get_last_assistant_usage(&entries).unwrap();
    assert_eq!(usage.input, 200);

    let mut aborted = assistant_msg("Aborted", Some(mock_usage(300, 150, 0, 0)));
    aborted.stop_reason = StopReason::Aborted;
    let mut b2 = EntryBuilder::new();
    let entries2 = vec![
        b2.message(user_msg("Hello")),
        b2.message(AgentMessage::Message(Message::Assistant(assistant_msg(
            "Hi",
            Some(mock_usage(100, 50, 0, 0)),
        )))),
        b2.message(user_msg("How are you?")),
        b2.message(AgentMessage::Message(Message::Assistant(aborted))),
    ];
    assert_eq!(get_last_assistant_usage(&entries2).unwrap().input, 100);

    let mut b3 = EntryBuilder::new();
    let entries3 = vec![
        b3.message(user_msg("Hello")),
        b3.message(AgentMessage::Message(Message::Assistant(assistant_msg(
            "Hi",
            Some(mock_usage(100, 50, 0, 0)),
        )))),
        b3.message(user_msg("continue")),
        b3.message(AgentMessage::Message(Message::Assistant(assistant_msg(
            "Partial",
            Some(mock_usage(0, 0, 0, 0)),
        )))),
    ];
    assert_eq!(get_last_assistant_usage(&entries3).unwrap().input, 100);

    let mut b4 = EntryBuilder::new();
    let entries4 = vec![b4.message(user_msg("Hello"))];
    assert!(get_last_assistant_usage(&entries4).is_none());
}

#[test]
fn estimate_context_tokens_uses_last_non_zero_usage_anchor() {
    let messages = vec![
        user_msg("Hello"),
        AgentMessage::Message(Message::Assistant(assistant_msg("Hi", Some(mock_usage(100, 50, 0, 0))))),
        user_msg("continue"),
        AgentMessage::Message(Message::Assistant(assistant_msg(
            "Partial thinking",
            Some(mock_usage(0, 0, 0, 0)),
        ))),
    ];
    let estimate = estimate_context_tokens(&messages);
    assert_eq!(estimate.usage_tokens, 150);
    assert_eq!(estimate.last_usage_index, Some(1));
    assert!(estimate.trailing_tokens > 0);
    assert_eq!(estimate.tokens, 150 + estimate.trailing_tokens);
}

#[test]
fn should_compact_checks_threshold_and_enabled() {
    let enabled = CompactionSettings {
        enabled: true,
        reserve_tokens: 10_000,
        keep_recent_tokens: 20_000,
    };
    assert!(should_compact(95_000, 100_000, &enabled));
    assert!(!should_compact(89_000, 100_000, &enabled));

    let disabled = CompactionSettings {
        enabled: false,
        ..enabled
    };
    assert!(!should_compact(95_000, 100_000, &disabled));
}

#[test]
fn find_cut_point_scenarios() {
    let mut b = EntryBuilder::new();
    let mut entries = Vec::new();
    for i in 0..10 {
        entries.push(b.message(user_msg(&format!("User {i}"))));
        entries.push(b.message(AgentMessage::Message(Message::Assistant(assistant_msg(
            &format!("Assistant {i}"),
            Some(mock_usage(0, 100, (i + 1) * 1000, 0)),
        )))));
    }
    let result = find_cut_point(&entries, 0, entries.len(), 2500);
    match &entries[result.first_kept_entry_index] {
        SessionEntry::Message(m) => {
            let role = m.message.role().unwrap();
            assert!(role == "user" || role == "assistant");
        }
        other => panic!("expected message entry, got {other:?}"),
    }

    let mut b2 = EntryBuilder::new();
    let single = vec![b2.message(AgentMessage::Message(Message::Assistant(assistant_msg("a", None))))];
    assert_eq!(find_cut_point(&single, 0, single.len(), 1000).first_kept_entry_index, 0);

    let mut b3 = EntryBuilder::new();
    let custom_entries = vec![
        b3.message(user_msg("hi")),
        b3.message(AgentMessage::Message(Message::Assistant(assistant_msg("hello", None)))),
        b3.custom_message(&"x".repeat(4000)),
        b3.message(AgentMessage::Message(Message::Assistant(assistant_msg("ok", None)))),
    ];
    let tiny = find_cut_point(&custom_entries, 0, custom_entries.len(), 1);
    assert_eq!(tiny.first_kept_entry_index, 3);
    assert!(tiny.is_split_turn);
    assert_eq!(tiny.turn_start_index, Some(2));

    let fits = find_cut_point(&custom_entries, 0, custom_entries.len(), 2);
    assert_eq!(fits.first_kept_entry_index, 2);
    assert!(!fits.is_split_turn);
    assert_eq!(fits.turn_start_index, None);

    // #9740: oversized trailing tool result falls back to preceding assistant toolCall
    let mut b4 = EntryBuilder::new();
    let mut args = JsonObject::new();
    args.insert("path".to_owned(), json!("big.txt"));
    let mut tc_msg = assistant_msg("", None);
    tc_msg.content = vec![AssistantContentBlock::ToolCall(faux_tool_call("read", args, None))];
    tc_msg.stop_reason = StopReason::ToolUse;
    let old_user = b4.message(user_msg("old history"));
    let old_assistant = b4.message(AgentMessage::Message(Message::Assistant(assistant_msg(
        "old answer",
        None,
    ))));
    let current_user = b4.message(user_msg("read the large file"));
    let tool_call = b4.message(AgentMessage::Message(Message::Assistant(tc_msg)));
    let tool_result = b4.message(AgentMessage::Message(Message::ToolResult(ToolResultMessage {
        tool_call_id: "call-1".to_owned(),
        tool_name: "read".to_owned(),
        content: vec![UserContentBlock::Text(faux_text(&"x".repeat(8000)))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
    })));
    let entries4 = vec![
        old_user.clone(),
        old_assistant.clone(),
        current_user.clone(),
        tool_call.clone(),
        tool_result,
    ];
    let res4 = find_cut_point(&entries4, 0, entries4.len(), 1000);
    assert_eq!(res4.first_kept_entry_index, 3);
    assert_eq!(res4.turn_start_index, Some(2));
    assert!(res4.is_split_turn);

    let prep = prepare_compaction(
        &entries4,
        &CompactionSettings {
            keep_recent_tokens: 1000,
            ..DEFAULT_COMPACTION_SETTINGS
        },
    )
    .unwrap();
    assert_eq!(prep.first_kept_entry_id, tool_call.id());
    assert_eq!(prep.messages_to_summarize.len(), 2);
    assert_eq!(prep.turn_prefix_messages.len(), 1);
}

#[test]
fn build_session_context_and_compactions() {
    let mut b = EntryBuilder::new();
    let entries = vec![
        b.message(user_msg("1")),
        b.message(AgentMessage::Message(Message::Assistant(assistant_msg("a", None)))),
        b.message(user_msg("2")),
        b.message(AgentMessage::Message(Message::Assistant(assistant_msg("b", None)))),
    ];
    let loaded = build_session_context(&entries, None, None);
    assert_eq!(loaded.messages.len(), 4);
    assert_eq!(loaded.thinking_level, "off");

    let mut b2 = EntryBuilder::new();
    let u1 = b2.message(user_msg("1"));
    let a1 = b2.message(AgentMessage::Message(Message::Assistant(assistant_msg("a", None))));
    let u2 = b2.message(user_msg("2"));
    let a2 = b2.message(AgentMessage::Message(Message::Assistant(assistant_msg("b", None))));
    let c1 = b2.compaction("Summary of 1,a,2,b", u2.id());
    let u3 = b2.message(user_msg("3"));
    let a3 = b2.message(AgentMessage::Message(Message::Assistant(assistant_msg("c", None))));
    let loaded2 = build_session_context(&[u1, a1, u2, a2, c1, u3, a3], None, None);
    assert_eq!(loaded2.messages.len(), 5);
    assert_eq!(loaded2.messages[0].role(), Some("compactionSummary"));

    let mut b3 = EntryBuilder::new();
    let entries3 = vec![
        b3.message(user_msg("1")),
        b3.model_change("openai", "gpt-4"),
        b3.message(AgentMessage::Message(Message::Assistant(assistant_msg("a", None)))),
        b3.thinking_level("high"),
    ];
    let loaded3 = build_session_context(&entries3, None, None);
    assert_eq!(loaded3.thinking_level, "high");
    assert_eq!(
        loaded3
            .model
            .as_ref()
            .map(|m| (m.provider.as_str(), m.model_id.as_str())),
        Some(("anthropic", "claude-sonnet-4-5"))
    );
}

#[test]
fn prepare_compaction_skips_system_messages_and_handles_previous_compactions() {
    let mut b = EntryBuilder::new();
    let mut sections = indexmap::IndexMap::new();
    sections.insert("preamble".to_owned(), Some("current prompt".to_owned()));
    let system = b.message(AgentMessage::Message(Message::System(SystemMessage {
        content: SystemMessageContent::Text(String::new()),
        sections: Some(sections),
        tools_added: None,
        tools_removed: None,
        timestamp: 0,
    })));
    let user = b.message(user_msg("one long turn"));
    let assistant = b.message(AgentMessage::Message(Message::Assistant(assistant_msg(
        "assistant suffix",
        None,
    ))));
    let prep = prepare_compaction(
        &[system, user, assistant.clone()],
        &CompactionSettings {
            keep_recent_tokens: 1,
            ..DEFAULT_COMPACTION_SETTINGS
        },
    )
    .unwrap();
    assert_eq!(prep.first_kept_entry_id, assistant.id());
    assert!(prep.is_split_turn);
    assert!(prep.messages_to_summarize.is_empty());
    assert_eq!(prep.turn_prefix_messages.len(), 1);

    let mut b2 = EntryBuilder::new();
    let u1 = b2.message(user_msg(&"user msg 1 (summarized by compaction1)".repeat(4)));
    let a1 = b2.message(AgentMessage::Message(Message::Assistant(assistant_msg(
        &"assistant msg 1".repeat(4),
        None,
    ))));
    let u2 = b2.message(user_msg(&"user msg 2 - kept by compaction1 ".repeat(12)));
    let a2 = b2.message(AgentMessage::Message(Message::Assistant(assistant_msg(
        &"assistant msg 2 ".repeat(12),
        None,
    ))));
    let u3 = b2.message(user_msg(&"user msg 3 - kept by compaction1 ".repeat(12)));
    let a3 = b2.message(AgentMessage::Message(Message::Assistant(assistant_msg(
        &"assistant msg 3 ".repeat(12),
        Some(mock_usage(5000, 1000, 0, 0)),
    ))));
    let compaction1 = b2.compaction("First summary", u2.id());
    let u4 = b2.message(user_msg(&"user msg 4 (new after compaction1) ".repeat(12)));
    let a4 = b2.message(AgentMessage::Message(Message::Assistant(assistant_msg(
        &"assistant msg 4 ".repeat(12),
        Some(mock_usage(8000, 2000, 0, 0)),
    ))));
    let prep2 = prepare_compaction(
        &[u1, a1, u2, a2, u3, a3, compaction1, u4, a4],
        &CompactionSettings {
            keep_recent_tokens: 100,
            ..DEFAULT_COMPACTION_SETTINGS
        },
    )
    .unwrap();
    let summarized = extract_text(&prep2.messages_to_summarize);
    assert!(summarized.contains("user msg 2 - kept by compaction1"));
    assert!(summarized.contains("user msg 3 - kept by compaction1"));
    assert!(!summarized.contains("First summary"));
    assert_eq!(prep2.previous_summary.as_deref(), Some("First summary"));
}

#[test]
fn large_session_fixture_parses_and_finds_cut_point() {
    let entries = load_large_session_entries();
    assert!(entries.len() > 100);
    let cut = find_cut_point(
        &entries,
        0,
        entries.len(),
        DEFAULT_COMPACTION_SETTINGS.keep_recent_tokens,
    );
    match &entries[cut.first_kept_entry_index] {
        SessionEntry::Message(m) => {
            let role = m.message.role().unwrap();
            assert!(role == "user" || role == "assistant");
        }
        other => panic!("expected message entry, got {other:?}"),
    }
    let loaded = build_session_context(&entries, None, None);
    assert!(loaded.messages.len() > 100);
    assert!(loaded.model.is_some());
}
