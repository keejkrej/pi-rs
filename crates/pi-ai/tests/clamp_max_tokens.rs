//! Port of packages/ai/test/clamp-max-tokens.test.ts

use pi_ai::api::simple_options::clamp_max_tokens_to_context;
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Context, Message, Model, StopReason, TextContent, TranscriptContext,
    Usage, UsageCost, UserMessage, UserMessageContent,
};

const WINDOW: i64 = 1_048_576;

fn model_with(max_tokens: i64, context_window: Option<i64>) -> Model {
    let mut base = get_model("openai", "gpt-4o-mini").unwrap();
    base.compat = None;
    base.api = "openai-completions".to_owned();
    base.context_window = context_window.unwrap_or(WINDOW);
    base.max_tokens = max_tokens;
    base
}

fn context_with_input(input_tokens: i64) -> TranscriptContext {
    let assistant = AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new("ok"))],
        api: "openai-completions".to_owned(),
        provider: "openai".to_owned(),
        model: "gpt-4o-mini".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: input_tokens,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: input_tokens,
            cost: UsageCost::default(),
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 2,
        duration_ms: None,
    };
    normalize_context(&Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("hi".to_owned()),
                timestamp: 1,
            }),
            Message::Assistant(assistant),
        ],
        tools: None,
    })
}

#[test]
fn clamp_max_tokens_to_context_cases() {
    // #10497: leaves 15% of input unused so OpenRouter overcounts fit
    let max_tokens = clamp_max_tokens_to_context(&model_with(943_718, None), &context_with_input(220_815), 943_718);
    assert_eq!(max_tokens, WINDOW - 220_815 - 33_123);
    assert!(229_747 + max_tokens <= WINDOW);

    // Leaves at least 4096 tokens unused for small inputs
    assert_eq!(
        clamp_max_tokens_to_context(&model_with(WINDOW, None), &context_with_input(10_000), WINDOW),
        WINDOW - 10_000 - 4096
    );

    // Keeps model output limit when it already fits
    assert_eq!(
        clamp_max_tokens_to_context(&model_with(64_000, None), &context_with_input(100_000), 64_000),
        64_000
    );

    // Sends up to 32768 tokens when proportional margin leaves no room
    assert_eq!(
        clamp_max_tokens_to_context(&model_with(943_718, None), &context_with_input(912_000), 943_718),
        32_768
    );

    // Uses real room minus 4096 when less than 32768 tokens fit
    assert_eq!(
        clamp_max_tokens_to_context(&model_with(943_718, None), &context_with_input(1_030_000), 943_718),
        WINDOW - 1_030_000 - 4096
    );

    // Keeps only fixed 4096 margin for small windows
    assert_eq!(
        clamp_max_tokens_to_context(&model_with(32_768, Some(32_768)), &context_with_input(20_000), 32_768),
        32_768 - 20_000 - 4096
    );

    // Sends 1 token when input already exceeds the window
    assert_eq!(
        clamp_max_tokens_to_context(&model_with(943_718, None), &context_with_input(1_050_000), 943_718),
        1
    );
}
