//! Port of packages/ai/test/context-estimate.test.ts (unit ai-utils-messages)

use pi_ai::api::simple_options::build_base_options;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Context, Message, Model, ModelCost, ModelInput, StopReason, TextContent,
    Usage, UserMessage, UserMessageContent,
};
use pi_ai::utils::estimate::estimate_context_tokens;
use pi_ai::utils::transcript::normalize_context;

fn create_usage(total_tokens: i64) -> Usage {
    Usage {
        input: total_tokens,
        total_tokens,
        ..Usage::default()
    }
}

fn create_assistant(timestamp: i64, total_tokens: i64) -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent {
            text: "kept".to_string(),
            text_signature: None,
        })],
        api: "openai-responses".to_string(),
        provider: "openai".into(),
        model: "test-model".to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: create_usage(total_tokens),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp,
    }
}

fn test_model() -> Model {
    Model {
        r#type: None,
        id: "test-model".to_string(),
        name: "Test Model".to_string(),
        api: "openai-responses".to_string(),
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".to_string(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 10_000,
        max_tokens: 8_000,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

#[test]
fn ignores_stale_assistant_usage_after_newer_message_inserted_before_it() {
    let context = normalize_context(&Context {
        system_prompt: Some("system".to_string()),
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("summary".to_string()),
                timestamp: 200,
            }),
            Message::Assistant(create_assistant(100, 9_500)),
            Message::User(UserMessage {
                content: UserMessageContent::Text("x".repeat(4_000)),
                timestamp: 300,
            }),
        ],
        tools: None,
    });

    let est = estimate_context_tokens(&context);
    assert_eq!(est.tokens, 1_005);
    assert_eq!(est.usage_tokens, 0);
    assert_eq!(est.trailing_tokens, 1_005);
    assert_eq!(est.last_usage_index, None);

    assert_eq!(
        build_base_options(&test_model(), &context, None, None).max_tokens,
        Some(4_899)
    );
}

#[test]
fn uses_assistant_usage_again_after_response_to_inserted_context() {
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("summary".to_string()),
                timestamp: 200,
            }),
            Message::Assistant(create_assistant(100, 9_500)),
            Message::User(UserMessage {
                content: UserMessageContent::Text("new prompt".to_string()),
                timestamp: 300,
            }),
            Message::Assistant(create_assistant(400, 2_000)),
            Message::User(UserMessage {
                content: UserMessageContent::Text("tail".to_string()),
                timestamp: 500,
            }),
        ],
        tools: None,
    });

    let est = estimate_context_tokens(&context);
    assert_eq!(est.tokens, 2_001);
    assert_eq!(est.usage_tokens, 2_000);
    assert_eq!(est.trailing_tokens, 1);
    assert_eq!(est.last_usage_index, Some(3));
}
