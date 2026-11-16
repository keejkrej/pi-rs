//! Port of packages/ai/test/transform-messages-copilot-openai-to-anthropic.test.ts (unit ai-utils-messages)

use indexmap::IndexMap;
use pi_ai::api::transform_messages::transform_messages;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Message, Model, ModelCost, ModelInput, StopReason, TextContent,
    ThinkingContent, ToolCall, ToolResultMessage, Usage, UserContentBlock, UserMessage, UserMessageContent,
};
use serde_json::{Value, json};

fn anthropic_normalize_tool_call_id(id: &str, _model: &Model, _source: &AssistantMessage) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

fn make_copilot_claude_model() -> Model {
    Model {
        r#type: None,
        id: "claude-sonnet-4.6".to_string(),
        name: "Claude Sonnet 4.6".to_string(),
        api: "anthropic-messages".to_string(),
        provider: "github-copilot".into(),
        base_url: "https://api.individual.githubcopilot.com".to_string(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text, ModelInput::Image],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128000,
        max_tokens: 16000,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn make_assistant(content: Vec<AssistantContentBlock>) -> AssistantMessage {
    AssistantMessage {
        content,
        api: "openai-responses".to_string(),
        provider: "github-copilot".into(),
        model: "gpt-5".to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1,
    }
}

#[test]
fn converts_thinking_blocks_to_plain_text_when_source_model_differs() {
    let model = make_copilot_claude_model();
    let messages = vec![
        Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_string()),
            timestamp: 1,
        }),
        Message::Assistant(AssistantMessage {
            content: vec![
                AssistantContentBlock::Thinking(ThinkingContent {
                    thinking: "Let me think about this...".to_string(),
                    thinking_signature: Some("reasoning_content".to_string()),
                    redacted: None,
                }),
                AssistantContentBlock::Text(TextContent {
                    text: "Hi there!".to_string(),
                    text_signature: None,
                }),
            ],
            api: "openai-completions".to_string(),
            provider: "github-copilot".into(),
            model: "gpt-4o".to_string(),
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
        }),
    ];

    let result = transform_messages(&messages, &model, Some(&anthropic_normalize_tool_call_id));
    let assistant = result
        .iter()
        .find_map(|m| match m {
            Message::Assistant(a) => Some(a),
            _ => None,
        })
        .unwrap();
    let text_count = assistant
        .content
        .iter()
        .filter(|b| matches!(b, AssistantContentBlock::Text(_)))
        .count();
    let thinking_count = assistant
        .content
        .iter()
        .filter(|b| matches!(b, AssistantContentBlock::Thinking(_)))
        .count();
    assert_eq!(thinking_count, 0);
    assert!(text_count >= 2);
}

#[test]
fn removes_thought_signature_from_tool_calls_when_migrating_models() {
    let model = make_copilot_claude_model();
    let mut args = IndexMap::new();
    args.insert("command".to_string(), Value::String("ls".to_string()));
    let messages = vec![
        Message::User(UserMessage {
            content: UserMessageContent::Text("run a command".to_string()),
            timestamp: 1,
        }),
        Message::Assistant(make_assistant(vec![AssistantContentBlock::ToolCall(ToolCall {
            id: "call_123".to_string(),
            name: "bash".to_string(),
            namespace: None,
            arguments: args,
            thought_signature: Some(
                json!({ "type": "reasoning.encrypted", "id": "call_123", "data": "encrypted" }).to_string(),
            ),
        })])),
        Message::ToolResult(ToolResultMessage {
            tool_call_id: "call_123".to_string(),
            tool_name: "bash".to_string(),
            content: vec![UserContentBlock::Text(TextContent {
                text: "output".to_string(),
                text_signature: None,
            })],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 2,
        }),
    ];

    let result = transform_messages(&messages, &model, Some(&anthropic_normalize_tool_call_id));
    let tc = result
        .iter()
        .find_map(|m| match m {
            Message::Assistant(a) => a.content.iter().find_map(|b| match b {
                AssistantContentBlock::ToolCall(tc) => Some(tc),
                _ => None,
            }),
            _ => None,
        })
        .unwrap();
    assert_eq!(tc.thought_signature, None);
}

#[test]
fn adds_synthetic_tool_results_only_for_trailing_orphaned_tool_calls() {
    let model = make_copilot_claude_model();
    let mut args1 = IndexMap::new();
    args1.insert("path".to_string(), Value::String("README.md".to_string()));

    let orphan_messages = vec![
        Message::User(UserMessage {
            content: UserMessageContent::Text("read the file".to_string()),
            timestamp: 1,
        }),
        Message::Assistant(make_assistant(vec![AssistantContentBlock::ToolCall(ToolCall {
            id: "call_123|fc_123".to_string(),
            name: "read".to_string(),
            namespace: None,
            arguments: args1.clone(),
            thought_signature: None,
        })])),
    ];

    let res1 = transform_messages(&orphan_messages, &model, Some(&anthropic_normalize_tool_call_id));
    match res1.last().unwrap() {
        Message::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "call_123_fc_123");
            assert_eq!(tr.tool_name, "read");
            assert!(tr.is_error);
            assert!(matches!(
                &tr.content[0],
                UserContentBlock::Text(t) if t.text == "No result provided"
            ));
        }
        other => panic!("expected toolResult, got {other:?}"),
    }

    let mut args2 = IndexMap::new();
    args2.insert("command".to_string(), Value::String("pwd".to_string()));
    let partial_messages = vec![
        Message::User(UserMessage {
            content: UserMessageContent::Text("run commands".to_string()),
            timestamp: 1,
        }),
        Message::Assistant(make_assistant(vec![
            AssistantContentBlock::ToolCall(ToolCall {
                id: "call_1|fc_1".to_string(),
                name: "read".to_string(),
                namespace: None,
                arguments: args1,
                thought_signature: None,
            }),
            AssistantContentBlock::ToolCall(ToolCall {
                id: "call_2|fc_2".to_string(),
                name: "bash".to_string(),
                namespace: None,
                arguments: args2,
                thought_signature: None,
            }),
        ])),
        Message::ToolResult(ToolResultMessage {
            tool_call_id: "call_1|fc_1".to_string(),
            tool_name: "read".to_string(),
            content: vec![UserContentBlock::Text(TextContent {
                text: "done".to_string(),
                text_signature: None,
            })],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 2,
        }),
    ];

    let res2 = transform_messages(&partial_messages, &model, Some(&anthropic_normalize_tool_call_id));
    let synthetic: Vec<_> = res2
        .iter()
        .filter_map(|m| match m {
            Message::ToolResult(tr) if tr.is_error => Some(tr),
            _ => None,
        })
        .collect();
    assert_eq!(synthetic.len(), 1);
    assert_eq!(synthetic[0].tool_call_id, "call_2_fc_2");
    assert_eq!(synthetic[0].tool_name, "bash");
}
