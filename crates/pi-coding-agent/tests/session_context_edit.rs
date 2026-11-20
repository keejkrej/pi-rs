//! Port of `packages/coding-agent/test/session-context-edit.test.ts`.

use pi_agent_core::AgentMessage;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, SystemMessage, SystemMessageContent, TextContent,
    Tool, ToolResultMessage, Usage, UserContentBlock, UserMessage, UserMessageContent, faux_text,
};
use pi_coding_agent::core::compaction::{
    DEFAULT_COMPACTION_SETTINGS, estimate_projected_context_tokens, prepare_compaction,
};
use pi_coding_agent::core::messages::CustomMessageContent;
use pi_coding_agent::core::session_manager::{
    ContextEditReplacement, ContextEditableContent, SessionEntry, SessionManager,
};
use pi_js::vendor::typebox::Schema;
use serde_json::{Value, json};

fn assistant(text: &str) -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContentBlock::Text(faux_text(text))],
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: 10,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 11,
            cost: Default::default(),
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
        duration_ms: None,
    }
}

fn user_text(text: &str) -> AgentMessage {
    AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_owned()),
        timestamp: 0,
    }))
}

fn agent_text(msg: &AgentMessage) -> String {
    match msg {
        AgentMessage::Message(Message::User(u)) => match &u.content {
            UserMessageContent::Text(t) => t.clone(),
            UserMessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    UserContentBlock::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect(),
        },
        AgentMessage::Message(Message::Assistant(a)) => a
            .content
            .iter()
            .filter_map(|b| match b {
                AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect(),
        AgentMessage::Message(Message::ToolResult(tr)) => tr
            .content
            .iter()
            .filter_map(|b| match b {
                UserContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect(),
        AgentMessage::Custom(val) => val
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    }
}

#[test]
fn omits_target_only_from_model_projection() {
    let sm = SessionManager::in_memory(None, None, None).unwrap();
    sm.append_message(user_text("request")).unwrap();
    let assistant_id = sm
        .append_message(AgentMessage::Message(Message::Assistant(assistant("partial"))))
        .unwrap();
    let result = ToolResultMessage {
        tool_call_id: "call-1".to_owned(),
        tool_name: "read".to_owned(),
        content: vec![UserContentBlock::Text(TextContent {
            text: "raw output".to_owned(),
            text_signature: None,
        })],
        details: Some(json!({ "path": "large.txt" })),
        is_error: true,
        nested_calls: None,
        usage: None,
        timestamp: 0,
        duration_ms: None,
    };
    let result_id = sm
        .append_message(AgentMessage::Message(Message::ToolResult(result.clone())))
        .unwrap();
    sm.append_context_edit(&assistant_id, None).unwrap();
    sm.append_context_edit(&result_id, None).unwrap();

    let msg_count = sm
        .get_branch(None)
        .into_iter()
        .filter(|e| matches!(e, SessionEntry::Message(_)))
        .count();
    assert_eq!(msg_count, 3);
    let proj_omitted = sm.build_session_projection();
    let roles: Vec<Option<&str>> = proj_omitted.messages.iter().map(|m| m.role()).collect();
    assert_eq!(roles, vec![Some("user")]);
    match sm.get_entry(&result_id).unwrap() {
        SessionEntry::Message(m) => assert_eq!(m.message, AgentMessage::Message(Message::ToolResult(result))),
        other => panic!("unexpected entry: {other:?}"),
    }
}

#[test]
fn replaces_only_content_and_lets_latest_edit_win() {
    let sm = SessionManager::in_memory(None, None, None).unwrap();
    let target_id = sm
        .append_message(AgentMessage::Message(Message::Assistant(assistant("original"))))
        .unwrap();
    sm.append_context_edit(
        &target_id,
        Some(ContextEditReplacement {
            content: ContextEditableContent::Blocks(vec![json!({ "type": "text", "text": "first" })]),
        }),
    )
    .unwrap();
    sm.append_context_edit(&target_id, None).unwrap();
    sm.append_context_edit(
        &target_id,
        Some(ContextEditReplacement {
            content: ContextEditableContent::Blocks(vec![json!({ "type": "text", "text": "restored" })]),
        }),
    )
    .unwrap();

    let proj = sm.build_session_projection();
    let first = &proj.messages[0];
    assert_eq!(agent_text(first), "restored");
    match first {
        AgentMessage::Message(Message::Assistant(a)) => assert_eq!(a.usage.total_tokens, 11),
        other => panic!("expected assistant: {other:?}"),
    }
    match sm.get_entry(&target_id).unwrap() {
        SessionEntry::Message(m) => assert_eq!(agent_text(&m.message), "original"),
        other => panic!("expected message entry: {other:?}"),
    }
}

#[test]
fn normalizes_string_replacements_and_keeps_edits_branch_relative() {
    let sm = SessionManager::in_memory(None, None, None).unwrap();
    let assistant_id = sm
        .append_message(AgentMessage::Message(Message::Assistant(assistant("original"))))
        .unwrap();
    let result_id = sm
        .append_message(AgentMessage::Message(Message::ToolResult(ToolResultMessage {
            tool_call_id: "call-1".to_owned(),
            tool_name: "read".to_owned(),
            content: vec![UserContentBlock::Text(TextContent {
                text: "original result".to_owned(),
                text_signature: None,
            })],
            details: None,
            is_error: false,
            nested_calls: None,
            usage: None,
            timestamp: 0,
            duration_ms: None,
        })))
        .unwrap();

    sm.append_context_edit(
        &assistant_id,
        Some(ContextEditReplacement {
            content: ContextEditableContent::Text("assistant replacement".to_owned()),
        }),
    )
    .unwrap();
    sm.append_context_edit(
        &result_id,
        Some(ContextEditReplacement {
            content: ContextEditableContent::Text("result replacement".to_owned()),
        }),
    )
    .unwrap();

    let proj = sm.build_session_projection();
    assert_eq!(agent_text(&proj.messages[0]), "assistant replacement");
    assert_eq!(agent_text(&proj.messages[1]), "result replacement");

    // Branch-relative edits
    let sm2 = SessionManager::in_memory(None, None, None).unwrap();
    let target_id = sm2.append_message(user_text("original")).unwrap();
    sm2.append_context_edit(
        &target_id,
        Some(ContextEditReplacement {
            content: ContextEditableContent::Text("edited".to_owned()),
        }),
    )
    .unwrap();
    assert_eq!(agent_text(&sm2.build_session_projection().messages[0]), "edited");
    sm2.branch(&target_id).unwrap();
    assert_eq!(agent_text(&sm2.build_session_projection().messages[0]), "original");
}

#[test]
fn handles_self_referencing_repeated_and_post_compaction_edits() {
    let sm = SessionManager::in_memory(None, None, None).unwrap();
    sm.append_message(user_text("discarded")).unwrap();
    let compaction_id = sm
        .append_compaction("exact handoff", None, 100, None, None, None)
        .unwrap();
    sm.append_message(user_text("after")).unwrap();

    match sm.get_entry(&compaction_id).unwrap() {
        SessionEntry::Compaction(c) => assert_eq!(c.first_kept_entry_id, compaction_id),
        other => panic!("expected compaction entry: {other:?}"),
    }
    let proj = sm.build_session_projection();
    let roles: Vec<Option<&str>> = proj.messages.iter().map(|m| m.role()).collect();
    assert_eq!(roles, vec![Some("compactionSummary"), Some("user")]);
    let texts: Vec<String> = proj.messages.iter().map(agent_text).collect();
    assert_eq!(texts, vec!["exact handoff".to_owned(), "after".to_owned()]);

    // Post-compaction edit applies to retained pre-compaction entry
    let sm2 = SessionManager::in_memory(None, None, None).unwrap();
    sm2.append_message(user_text("summarized")).unwrap();
    let retained_id = sm2.append_message(user_text("original retained")).unwrap();
    sm2.append_compaction("summary", Some(&retained_id), 100, None, None, None)
        .unwrap();
    sm2.append_context_edit(
        &retained_id,
        Some(ContextEditReplacement {
            content: ContextEditableContent::Text("edited retained".to_owned()),
        }),
    )
    .unwrap();
    let texts2: Vec<String> = sm2.build_session_projection().messages.iter().map(agent_text).collect();
    assert_eq!(texts2, vec!["summary".to_owned(), "edited retained".to_owned()]);

    // Repeated compaction uses only newest summary
    let sm3 = SessionManager::in_memory(None, None, None).unwrap();
    sm3.append_message(user_text("summarized first")).unwrap();
    let ret3 = sm3.append_message(user_text("retained")).unwrap();
    sm3.append_compaction("first summary", Some(&ret3), 100, None, None, None)
        .unwrap();
    sm3.append_message(AgentMessage::Message(Message::Assistant(assistant("after first"))))
        .unwrap();
    sm3.append_compaction("second summary", Some(&ret3), 80, None, None, None)
        .unwrap();
    sm3.append_message(user_text(&"new tail ".repeat(100))).unwrap();
    let summaries: Vec<String> = sm3
        .build_session_projection()
        .messages
        .iter()
        .filter(|m| m.role() == Some("compactionSummary"))
        .map(agent_text)
        .collect();
    assert_eq!(summaries, vec!["second summary".to_owned()]);
    let mut settings = DEFAULT_COMPACTION_SETTINGS;
    settings.keep_recent_tokens = 1;
    let prep = prepare_compaction(&sm3.get_branch(None), &settings).unwrap();
    assert_eq!(prep.previous_summary.as_deref(), Some("second summary"));
}

#[test]
fn invalidates_stale_usage_across_edits_and_compactions_and_includes_system_tools() {
    let sm = SessionManager::in_memory(None, None, None).unwrap();
    let large_id = sm.append_message(user_text(&"discarded input ".repeat(2_000))).unwrap();
    let mut resp = assistant("small answer");
    resp.usage.input = 10_000;
    resp.usage.total_tokens = 10_001;
    let assistant_id = sm
        .append_message(AgentMessage::Message(Message::Assistant(resp)))
        .unwrap();
    sm.append_context_edit(&large_id, None).unwrap();

    let edited_est = estimate_projected_context_tokens(&sm.build_session_projection(), &sm.get_branch(None));
    assert_eq!(edited_est.usage_tokens, 0);
    assert!(edited_est.tokens < 100);

    sm.append_context_edit(&assistant_id, None).unwrap();
    assert_eq!(
        estimate_projected_context_tokens(&sm.build_session_projection(), &sm.get_branch(None)).tokens,
        0
    );

    // Captures assistant usage recorded after latest context edit
    let sm2 = SessionManager::in_memory(None, None, None).unwrap();
    let user_id = sm2.append_message(user_text("original")).unwrap();
    sm2.append_context_edit(
        &user_id,
        Some(ContextEditReplacement {
            content: ContextEditableContent::Text("edited".to_owned()),
        }),
    )
    .unwrap();
    let mut r2 = assistant("answer");
    r2.usage.input = 4_000;
    r2.usage.output = 100;
    r2.usage.total_tokens = 4_100;
    sm2.append_message(AgentMessage::Message(Message::Assistant(r2)))
        .unwrap();
    sm2.append_message(user_text("next")).unwrap();
    let est2 = estimate_projected_context_tokens(&sm2.build_session_projection(), &sm2.get_branch(None));
    assert_eq!(est2.usage_tokens, 4_100);
    assert_eq!(est2.trailing_tokens, 1);
    assert_eq!(est2.tokens, 4_101);

    // Does not reuse post-edit usage after later compaction
    let sm3 = SessionManager::in_memory(None, None, None).unwrap();
    let u3 = sm3.append_message(user_text("small input")).unwrap();
    sm3.append_context_edit(
        &u3,
        Some(ContextEditReplacement {
            content: ContextEditableContent::Text("edited input".to_owned()),
        }),
    )
    .unwrap();
    let mut r3 = assistant("answer");
    r3.usage.input = 50_000;
    r3.usage.output = 1;
    r3.usage.total_tokens = 50_001;
    sm3.append_message(AgentMessage::Message(Message::Assistant(r3)))
        .unwrap();
    sm3.append_compaction("small summary", Some(&u3), 50_001, None, None, None)
        .unwrap();
    let est3 = estimate_projected_context_tokens(&sm3.build_session_projection(), &sm3.get_branch(None));
    assert_eq!(est3.usage_tokens, 0);
    assert!(est3.tokens < 100);

    // System prompt + tool declarations included in edited estimates
    let sm4 = SessionManager::in_memory(None, None, None).unwrap();
    let schema: Schema = pi_js::json::parse_as(r#"{"type":"object","properties":{}}"#).unwrap();
    sm4.append_message(AgentMessage::Message(Message::System(SystemMessage {
        content: SystemMessageContent::Text("system prompt ".repeat(3_000)),
        sections: None,
        tools_added: Some(vec![Tool {
            name: "example".to_owned(),
            description: "tool declaration ".repeat(100),
            parameters: schema,
            constrained_sampling: None,
        }]),
        tools_removed: None,
        timestamp: 0,
    })))
    .unwrap();
    let u4 = sm4.append_message(user_text("ask")).unwrap();
    sm4.append_message(AgentMessage::Message(Message::Assistant(assistant("done"))))
        .unwrap();
    sm4.append_context_edit(
        &u4,
        Some(ContextEditReplacement {
            content: ContextEditableContent::Text("ask".to_owned()),
        }),
    )
    .unwrap();
    assert!(estimate_projected_context_tokens(&sm4.build_session_projection(), &sm4.get_branch(None)).tokens > 10_000);
}

#[test]
fn boundary_edits_omitted_recovery_and_edited_compaction_inputs() {
    let mut settings = DEFAULT_COMPACTION_SETTINGS;
    settings.keep_recent_tokens = 1;

    // Boundary replacement kept out of summarization
    let sm = SessionManager::in_memory(None, None, None).unwrap();
    sm.append_message(user_text("old request")).unwrap();
    sm.append_message(AgentMessage::Message(Message::Assistant(assistant("old answer"))))
        .unwrap();
    let replaced_user_id = sm.append_message(user_text("original input")).unwrap();
    let assistant_id = sm
        .append_message(AgentMessage::Message(Message::Assistant(assistant("answered"))))
        .unwrap();
    sm.append_context_edit(
        &replaced_user_id,
        Some(ContextEditReplacement {
            content: ContextEditableContent::Text("NEW-INSTRUCTION ".repeat(100)),
        }),
    )
    .unwrap();
    sm.append_context_edit(&assistant_id, None).unwrap();
    sm.append_custom_entry("bookkeeping", Some(json!({ "source": "test" })))
        .unwrap();
    let prep = prepare_compaction(&sm.get_branch(None), &settings).unwrap();
    assert_eq!(prep.first_kept_entry_id, replaced_user_id);
    assert!(!pi_js::json::stringify(&prep.messages_to_summarize).contains("NEW-INSTRUCTION"));
    assert!(!pi_js::json::stringify(&prep.turn_prefix_messages).contains("NEW-INSTRUCTION"));

    // Metadata doesn't advance past unsent custom message
    let sm2 = SessionManager::in_memory(None, None, None).unwrap();
    sm2.append_message(user_text("old request")).unwrap();
    sm2.append_message(AgentMessage::Message(Message::Assistant(assistant("old answer"))))
        .unwrap();
    let instr_id = sm2
        .append_custom_message_entry(
            "next-work",
            CustomMessageContent::Text("UNSENT-INSTRUCTION ".repeat(100)),
            false,
            None,
        )
        .unwrap();
    sm2.append_custom_entry("bookkeeping", Some(json!({ "source": "test" })))
        .unwrap();
    let prep2 = prepare_compaction(&sm2.get_branch(None), &settings).unwrap();
    assert_eq!(prep2.first_kept_entry_id, instr_id);
    assert!(!pi_js::json::stringify(&prep2.messages_to_summarize).contains("UNSENT-INSTRUCTION"));

    // Omitted custom message not treated as recovery attempt
    let sm3 = SessionManager::in_memory(None, None, None).unwrap();
    sm3.append_message(user_text(&"unanswered input ".repeat(100))).unwrap();
    let custom_id = sm3
        .append_custom_message_entry(
            "temporary",
            CustomMessageContent::Text("temporary context".to_owned()),
            false,
            None,
        )
        .unwrap();
    sm3.append_context_edit(&custom_id, None).unwrap();
    assert!(prepare_compaction(&sm3.get_branch(None), &settings).is_none());

    // Omitted assistant recovery suffix advances past user input into turnPrefix
    let sm4 = SessionManager::in_memory(None, None, None).unwrap();
    let user_id = sm4.append_message(user_text(&"recovery input ".repeat(100))).unwrap();
    let attempt_id = sm4
        .append_message(AgentMessage::Message(Message::Assistant(assistant("failed attempt"))))
        .unwrap();
    sm4.append_context_edit(&attempt_id, None).unwrap();
    sm4.append_custom_entry("bookkeeping", Some(json!({ "source": "test" })))
        .unwrap();
    let prep4 = prepare_compaction(&sm4.get_branch(None), &settings).unwrap();
    assert_eq!(prep4.first_kept_entry_id, attempt_id);
    assert_ne!(user_id, attempt_id);
    assert_eq!(prep4.turn_prefix_messages.len(), 1);
    assert!(agent_text(&prep4.turn_prefix_messages[0]).contains("recovery input"));
    assert!(!pi_js::json::stringify(&prep4.messages_to_summarize).contains("recovery input"));
}
