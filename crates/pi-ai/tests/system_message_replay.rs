//! Port of packages/ai/test/system-message-replay.test.ts (unit ai-utils-messages)

use indexmap::IndexMap;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, ConstrainedSampling, Context, Message, StopReason, SystemMessage,
    SystemMessageContent, TextContent, Tool, ToolReference, Usage, UserMessage, UserMessageContent,
};
use pi_ai::utils::text::{get_system_message_text, render_system_message_update};
use pi_ai::utils::transcript::{
    ToolStateChanges, collapse_system_messages, declarations_equal, get_current_system_message,
    get_current_system_prompt, get_tool_state_changes, has_non_additive_tool_changes, has_tool_redefinitions,
    normalize_context,
};
use pi_js::vendor::typebox::t;

fn tool(name: &str, desc: Option<&str>) -> Tool {
    Tool {
        name: name.to_string(),
        description: desc.map(str::to_string).unwrap_or_else(|| format!("{name} tool")),
        parameters: t::object(std::iter::empty::<(&str, pi_js::vendor::typebox::Schema)>()),
        constrained_sampling: None,
    }
}

fn sections(pairs: &[(&str, Option<&str>)]) -> IndexMap<String, Option<String>> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.map(str::to_string)))
        .collect()
}

fn sample_transcript() -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: None,
        tools: None,
        messages: vec![
            Message::System(SystemMessage {
                content: SystemMessageContent::Text("base".to_string()),
                sections: Some(sections(&[("a", Some("<a>1</a>")), ("b", Some("<b>1</b>"))])),
                tools_added: Some(vec![tool("first", None)]),
                tools_removed: None,
                timestamp: 10,
            }),
            Message::User(UserMessage {
                content: UserMessageContent::Text("hello".to_string()),
                timestamp: 11,
            }),
            Message::System(SystemMessage {
                content: SystemMessageContent::Text("also do this".to_string()),
                sections: None,
                tools_added: None,
                tools_removed: None,
                timestamp: 12,
            }),
            Message::Assistant(AssistantMessage {
                content: vec![AssistantContentBlock::Text(TextContent {
                    text: "ok".to_string(),
                    text_signature: None,
                })],
                api: String::new(),
                provider: String::new().into(),
                model: String::new(),
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
                timestamp: 13,
            }),
            Message::System(SystemMessage {
                content: SystemMessageContent::Text(String::new()),
                sections: Some(sections(&[
                    ("a", Some("<a>2</a>")),
                    ("b", None),
                    ("c", Some("<c>1</c>")),
                ])),
                tools_removed: Some(vec![ToolReference {
                    name: "first".to_string(),
                }]),
                tools_added: Some(vec![tool("second", None)]),
                timestamp: 14,
            }),
        ],
    })
}

#[test]
fn replays_content_sections_and_tools_and_collapses() {
    let transcript = sample_transcript();
    let current = get_current_system_message(&transcript.messages).unwrap();
    assert_eq!(
        current.content,
        SystemMessageContent::Text("base\n\nalso do this".to_string())
    );
    assert_eq!(
        current.sections,
        Some(sections(&[("a", Some("<a>2</a>")), ("c", Some("<c>1</c>"))]))
    );
    assert_eq!(current.tools_added, Some(vec![tool("second", None)]));
    assert_eq!(current.timestamp, 10);
    assert_eq!(
        get_current_system_prompt(&transcript.messages),
        "base\n\nalso do this\n\n<a>2</a>\n\n<c>1</c>"
    );

    let collapsed = collapse_system_messages(&transcript);
    let roles: Vec<_> = collapsed.messages.iter().map(|m| m.role()).collect();
    assert_eq!(roles, vec!["system", "user", "assistant"]);
    assert_eq!(collapse_system_messages(&collapsed), collapsed);
}

#[test]
fn replay_empty_and_late_patch_and_renders() {
    let empty_ctx = normalize_context(&Context {
        system_prompt: None,
        tools: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_string()),
            timestamp: 1,
        })],
    });
    assert!(get_current_system_message(&empty_ctx.messages).is_none());
    assert_eq!(get_current_system_prompt(&empty_ctx.messages), "");
    assert_eq!(collapse_system_messages(&empty_ctx).messages, empty_ctx.messages);

    let late = normalize_context(&Context {
        system_prompt: None,
        tools: None,
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("old session".to_string()),
                timestamp: 1,
            }),
            Message::System(SystemMessage {
                content: SystemMessageContent::Text(String::new()),
                sections: Some(sections(&[("preamble", Some("You are pi."))])),
                tools_added: Some(vec![tool("x", None)]),
                tools_removed: None,
                timestamp: 2,
            }),
        ],
    });
    assert_eq!(get_current_system_prompt(&late.messages), "You are pi.");
    let collapsed = collapse_system_messages(&late);
    match &collapsed.messages[0] {
        Message::System(s) => assert_eq!(s.tools_added, Some(vec![tool("x", None)])),
        other => panic!("expected system message, got {other:?}"),
    }

    let transcript = sample_transcript();
    let (Message::System(leading), Message::System(update)) = (&transcript.messages[0], &transcript.messages[4]) else {
        panic!("expected system messages");
    };
    assert_eq!(get_system_message_text(leading), "base\n\n<a>1</a>\n\n<b>1</b>");
    assert_eq!(
        render_system_message_update(update),
        [
            "Updated system prompt section \"a\":\n\n<a>2</a>",
            "Removed system prompt section \"b\".",
            "Updated system prompt section \"c\":\n\n<c>1</c>",
        ]
        .join("\n\n")
    );
}

#[test]
fn normalizes_legacy_fields_and_tracks_tool_declaration_changes() {
    let user = Message::User(UserMessage {
        content: UserMessageContent::Text("hi".to_string()),
        timestamp: 1,
    });
    assert_eq!(
        normalize_context(&Context {
            system_prompt: None,
            tools: None,
            messages: vec![user.clone()],
        })
        .messages,
        vec![user.clone()]
    );
    assert_eq!(
        normalize_context(&Context {
            system_prompt: Some(String::new()),
            tools: Some(Vec::new()),
            messages: vec![user.clone()],
        })
        .messages,
        vec![user.clone()]
    );
    let normalized = normalize_context(&Context {
        system_prompt: Some("be brief".to_string()),
        tools: Some(vec![tool("a", None)]),
        messages: vec![user.clone()],
    });
    assert_eq!(normalized.messages.len(), 2);
    match &normalized.messages[0] {
        Message::System(s) => {
            assert_eq!(s.content, SystemMessageContent::Text("be brief".to_string()));
            assert_eq!(s.tools_added, Some(vec![tool("a", None)]));
            assert_eq!(s.timestamp, 0);
        }
        other => panic!("expected system, got {other:?}"),
    }

    assert!(declarations_equal(&tool("a", None), &tool("a", None)));
    assert!(!declarations_equal(&tool("a", None), &tool("a", Some("changed"))));
    let mut with_cs = tool("a", None);
    with_cs.constrained_sampling = Some(ConstrainedSampling::Disabled);
    assert!(!declarations_equal(&tool("a", None), &with_cs));

    let changes = get_tool_state_changes(
        &[tool("a", None), tool("b", None)],
        &[tool("b", Some("changed")), tool("c", None)],
    );
    assert_eq!(
        changes,
        ToolStateChanges {
            tools_added: vec![tool("b", Some("changed")), tool("c", None)],
            tools_removed: vec![
                ToolReference { name: "a".to_string() },
                ToolReference { name: "b".to_string() },
            ],
        }
    );
    assert_eq!(
        get_tool_state_changes(&[tool("a", None)], &[tool("a", None)]),
        ToolStateChanges {
            tools_added: Vec::new(),
            tools_removed: Vec::new(),
        }
    );

    let transcript = sample_transcript();
    assert!(has_non_additive_tool_changes(&transcript.messages));
    assert!(!has_tool_redefinitions(&transcript.messages));

    let additive = normalize_context(&Context {
        system_prompt: None,
        tools: None,
        messages: vec![
            Message::System(SystemMessage {
                content: SystemMessageContent::Text(String::new()),
                sections: None,
                tools_added: Some(vec![tool("a", None)]),
                tools_removed: None,
                timestamp: 1,
            }),
            Message::System(SystemMessage {
                content: SystemMessageContent::Text(String::new()),
                sections: None,
                tools_added: Some(vec![tool("b", None)]),
                tools_removed: None,
                timestamp: 2,
            }),
        ],
    });
    assert!(!has_non_additive_tool_changes(&additive.messages));

    let redeclared = normalize_context(&Context {
        system_prompt: None,
        tools: None,
        messages: vec![
            Message::System(SystemMessage {
                content: SystemMessageContent::Text(String::new()),
                sections: None,
                tools_added: Some(vec![tool("a", None)]),
                tools_removed: None,
                timestamp: 1,
            }),
            Message::System(SystemMessage {
                content: SystemMessageContent::Text(String::new()),
                sections: None,
                tools_added: Some(vec![tool("a", Some("changed"))]),
                tools_removed: None,
                timestamp: 2,
            }),
        ],
    });
    assert!(has_non_additive_tool_changes(&redeclared.messages));
    assert!(has_tool_redefinitions(&redeclared.messages));
}
