//! Port of packages/ai/test/transform-messages-cross-model-thinking.test.ts

use indexmap::IndexMap;
use pi_ai::api::transform_messages::transform_messages;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Message, Model, ModelCost, ModelInput, StopReason, TextContent,
    ThinkingContent, ToolCall, ToolResultMessage, Usage, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_js::time;
use serde_json::json;

fn kimi() -> Model {
    Model {
        r#type: None,
        id: "moonshotai/Kimi-K3".to_owned(),
        name: "Kimi K3".to_owned(),
        api: "openai-completions".to_owned(),
        provider: "sference".into(),
        base_url: "https://api.sference.com/v1".to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text, ModelInput::Image],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 1_048_576,
        max_tokens: 32_768,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn assistant(model: &str, content: Vec<AssistantContentBlock>, stop_reason: StopReason) -> AssistantMessage {
    AssistantMessage {
        content,
        api: "openai-completions".to_owned(),
        provider: "sference".into(),
        model: model.to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: time::now_ms(),
        duration_ms: None,
    }
}

fn thinking_block(text: &str) -> AssistantContentBlock {
    AssistantContentBlock::Thinking(ThinkingContent {
        thinking: text.to_owned(),
        thinking_signature: Some("reasoning_content".to_owned()),
        redacted: None,
    })
}

// https://github.com/earendil-works/pi/issues/10995
#[test]
fn does_not_send_another_models_readable_thinking_as_text() {
    let target = kimi();
    let now = time::now_ms();
    let mut args = IndexMap::new();
    args.insert("command".to_owned(), json!("curl -v"));

    let messages = vec![
        Message::User(UserMessage {
            content: UserMessageContent::Text("Why does curl work but node fails?".to_owned()),
            timestamp: now,
        }),
        Message::Assistant(assistant(
            "zai-org/GLM-5.3",
            vec![
                thinking_block("The user asks about ECONNRESET. Candidates: proxy, IPv6, TLS."),
                AssistantContentBlock::Text(TextContent::new("Let me check the TLS versions.")),
                AssistantContentBlock::ToolCall(ToolCall::new("call_1", "bash", args.clone())),
            ],
            StopReason::ToolUse,
        )),
        Message::ToolResult(ToolResultMessage {
            tool_call_id: "call_1".to_owned(),
            tool_name: "bash".to_owned(),
            content: vec![UserContentBlock::Text(TextContent::new("TLSv1.2"))],
            details: None,
            is_error: false,
            nested_calls: None,
            usage: None,
            timestamp: now,
            duration_ms: None,
        }),
    ];

    let result = transform_messages(&messages, &target, None);
    let asst = result
        .iter()
        .find_map(|m| match m {
            Message::Assistant(a) => Some(a),
            _ => None,
        })
        .unwrap();

    assert_eq!(
        asst.content,
        vec![
            AssistantContentBlock::Text(TextContent::new("Let me check the TLS versions.")),
            AssistantContentBlock::ToolCall(ToolCall::new("call_1", "bash", args)),
        ]
    );
}

#[test]
fn keeps_thinking_only_from_the_target_model_when_models_alternate() {
    let target = kimi();
    let now = time::now_ms();
    let messages = vec![
        Message::User(UserMessage {
            content: UserMessageContent::Text("1".to_owned()),
            timestamp: now,
        }),
        Message::Assistant(assistant(
            "moonshotai/Kimi-K3",
            vec![
                thinking_block("kimi 1"),
                AssistantContentBlock::Text(TextContent::new("a")),
            ],
            StopReason::Stop,
        )),
        Message::User(UserMessage {
            content: UserMessageContent::Text("2".to_owned()),
            timestamp: now,
        }),
        Message::Assistant(assistant(
            "zai-org/GLM-5.3",
            vec![
                thinking_block("glm 1"),
                AssistantContentBlock::Text(TextContent::new("b")),
            ],
            StopReason::Stop,
        )),
        Message::User(UserMessage {
            content: UserMessageContent::Text("3".to_owned()),
            timestamp: now,
        }),
        Message::Assistant(assistant(
            "moonshotai/Kimi-K3",
            vec![
                thinking_block("kimi 2"),
                AssistantContentBlock::Text(TextContent::new("c")),
            ],
            StopReason::Stop,
        )),
        Message::User(UserMessage {
            content: UserMessageContent::Text("4".to_owned()),
            timestamp: now,
        }),
        Message::Assistant(assistant(
            "zai-org/GLM-5.3",
            vec![
                thinking_block("glm 2"),
                AssistantContentBlock::Text(TextContent::new("d")),
            ],
            StopReason::Stop,
        )),
        Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_owned()),
            timestamp: now,
        }),
    ];

    let result = transform_messages(&messages, &target, None);
    let contents: Vec<Vec<AssistantContentBlock>> = result
        .into_iter()
        .filter_map(|m| match m {
            Message::Assistant(a) => Some(a.content),
            _ => None,
        })
        .collect();

    assert_eq!(
        contents,
        vec![
            vec![
                thinking_block("kimi 1"),
                AssistantContentBlock::Text(TextContent::new("a"))
            ],
            vec![AssistantContentBlock::Text(TextContent::new("b"))],
            vec![
                thinking_block("kimi 2"),
                AssistantContentBlock::Text(TextContent::new("c"))
            ],
            vec![AssistantContentBlock::Text(TextContent::new("d"))],
        ]
    );
}
