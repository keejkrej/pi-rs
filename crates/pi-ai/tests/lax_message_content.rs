//! Port of packages/ai/test/lax-message-content.test.ts (unit ai-utils-messages)

use pi_ai::api::transform_messages::transform_messages;
use pi_ai::types::{
    AssistantMessage, Message, Model, ModelCost, ModelInput, ToolResultMessage, UserMessage, UserMessageContent,
};

fn make_text_only_model() -> Model {
    Model {
        r#type: None,
        id: "test-model".to_string(),
        name: "Test Model".to_string(),
        api: "openai-completions".to_string(),
        provider: "openai".into(),
        base_url: "https://example.invalid/v1".to_string(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128000,
        max_tokens: 16000,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

#[test]
fn normalizes_null_and_missing_content_without_panicking() {
    let raw = r#"[
        { "role": "user", "content": null, "timestamp": 1 },
        {
            "role": "assistant",
            "content": null,
            "api": "openai-completions",
            "provider": "openai",
            "model": "test-model",
            "usage": {
                "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
            },
            "stopReason": "stop",
            "timestamp": 2
        },
        {
            "role": "toolResult",
            "toolCallId": "call_1",
            "toolName": "web_search",
            "isError": false,
            "timestamp": 3
        }
    ]"#;

    let messages: Vec<Message> = serde_json::from_str(raw).unwrap();
    let result = transform_messages(&messages, &make_text_only_model(), None);

    assert_eq!(result.len(), 3);
    match &result[0] {
        Message::User(UserMessage { content, .. }) => match content {
            UserMessageContent::Blocks(blocks) => assert!(blocks.is_empty()),
            UserMessageContent::Text(t) => assert!(t.is_empty()),
        },
        other => panic!("expected user message, got {other:?}"),
    }
    match &result[1] {
        Message::Assistant(AssistantMessage { content, .. }) => assert!(content.is_empty()),
        other => panic!("expected assistant message, got {other:?}"),
    }
    match &result[2] {
        Message::ToolResult(ToolResultMessage { content, .. }) => assert!(content.is_empty()),
        other => panic!("expected toolResult message, got {other:?}"),
    }
}
