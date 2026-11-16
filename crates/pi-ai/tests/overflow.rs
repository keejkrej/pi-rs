//! Port of packages/ai/test/overflow.test.ts (unit ai-utils-errors)

use pi_ai::types::{AssistantMessage, StopReason, Usage};
use pi_ai::utils::overflow::{is_context_overflow, is_recoverable_length};

fn create_error_message(error_message: &str, provider: &str) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: "openai-completions".to_string(),
        provider: provider.into(),
        model: "qwen3.5:35b".to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Error,
        deferred: None,
        error_message: Some(error_message.to_string()),
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1,
    }
}

fn create_length_stop_message(
    input: i64,
    cache_read: i64,
    output: i64,
    cache_write: i64,
    api: &str,
    provider: &str,
    model: &str,
) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: api.to_string(),
        provider: provider.into(),
        model: model.to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input,
            output,
            cache_read,
            cache_write,
            total_tokens: input + cache_read + cache_write + output,
            ..Usage::default()
        },
        stop_reason: StopReason::Length,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1,
    }
}

#[test]
fn detects_overflow_error_messages() {
    let msg = create_error_message(
        "400 `prompt too long; exceeded max context length by 100918 tokens`",
        "ollama",
    );
    assert!(is_context_overflow(&msg, Some(32768)));

    let zai = create_error_message(r#"400 {"code":"1261","message":"Prompt too long"}"#, "zai");
    assert!(is_context_overflow(&zai, Some(1_048_576)));

    let zai_cn = create_error_message(r#"400 {"code":"1261","message":"Prompt exceeds max length"}"#, "zai");
    assert!(is_context_overflow(&zai_cn, Some(1_048_576)));

    let together = create_error_message(
        "400 The input (516368 tokens) is longer than the model's context length (262144 tokens).",
        "ollama",
    );
    assert!(is_context_overflow(&together, Some(262_144)));

    let litellm = create_error_message(
        "Error: 503 litellm.ServiceUnavailableError: litellm.MidStreamFallbackError: litellm.APIConnectionError: APIConnectionError: OpenAIException - Requested token count exceeds the model's maximum context length of 131072 tokens.",
        "ollama",
    );
    assert!(is_context_overflow(&litellm, Some(131_072)));

    let parens = create_error_message(
        "Error: 400 Input length (265330) exceeds model's maximum context length (262144).",
        "ollama",
    );
    assert!(is_context_overflow(&parens, Some(262_144)));

    let poolside = create_error_message(
        "Provider returned error: Input length 131393 exceeds the maximum allowed input length of 131040 tokens.",
        "ollama",
    );
    assert!(is_context_overflow(&poolside, Some(131_072)));

    let ds4 = create_error_message(
        "400 Prompt has 256468 tokens, but the configured context size is 256000 tokens",
        "ollama",
    );
    assert!(is_context_overflow(&ds4, Some(256_000)));

    let ds4_comma = create_error_message(
        "Prompt has 5,958,968 tokens, but the configured context size is 256,000 tokens",
        "ollama",
    );
    assert!(is_context_overflow(&ds4_comma, Some(256_000)));

    let non_overflow = create_error_message("500 `model runner crashed unexpectedly`", "ollama");
    assert!(!is_context_overflow(&non_overflow, Some(32_768)));

    for err_msg in ["400 status code (no body)", "413 status code (no body)"] {
        assert!(is_context_overflow(
            &create_error_message(err_msg, "cerebras"),
            Some(131_072)
        ));
        assert!(!is_context_overflow(
            &create_error_message(err_msg, "opencode-go"),
            Some(1_000_000)
        ));
    }

    assert!(!is_context_overflow(
        &create_error_message(
            "Throttling error: Too many tokens, please wait before trying again.",
            "ollama"
        ),
        Some(200_000)
    ));
    assert!(!is_context_overflow(
        &create_error_message("Service unavailable: The service is temporarily unavailable.", "ollama"),
        Some(200_000)
    ));
    assert!(!is_context_overflow(
        &create_error_message("Rate limit exceeded, please retry after 30 seconds.", "ollama"),
        Some(200_000)
    ));
    assert!(!is_context_overflow(
        &create_error_message("Too many requests. Please slow down.", "ollama"),
        Some(200_000)
    ));
}

#[test]
fn detects_length_stop_overflow_and_recoverable_length() {
    let xiaomi = create_length_stop_message(58, 1_048_512, 0, 0, "openai-completions", "xiaomi", "mimo-v2.5-pro");
    assert!(is_context_overflow(&xiaomi, Some(1_048_576)));

    let recoverable = create_length_stop_message(3, 253_584, 16, 25_554, "openai-responses", "openai", "gpt-5.6-sol");
    assert!(is_recoverable_length(&recoverable, 128_000));

    let reached = create_length_stop_message(4062, 0, 1024, 0, "openai-completions", "test-provider", "test-model");
    assert!(!is_recoverable_length(&reached, 1024));

    let zero_out = create_length_stop_message(100, 0, 0, 0, "openai-completions", "test-provider", "test-model");
    assert!(is_recoverable_length(&zero_out, 128_000));
    assert!(!is_context_overflow(&zero_out, Some(200_000)));

    let normal_length =
        create_length_stop_message(1000, 0, 4096, 0, "openai-completions", "test-provider", "test-model");
    assert!(!is_context_overflow(&normal_length, Some(200_000)));
}
