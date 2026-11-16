//! Port of packages/ai/test/provider-error-body-regression.test.ts (unit ai-providers-builtin)

use std::sync::Arc;

use pi_ai::api::openai_completions::stream as stream_openai_completions;
use pi_ai::api::openai_responses::stream as stream_openai_responses;
use pi_ai::types::{
    ApiStreamOptions, Context, FetchFunction, Message, Model, ModelCost, ModelInput, StopReason, StreamOptions,
    UserMessage, UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;
use pi_js::fetch::{Headers, Response as FetchResponse};

fn make_context() -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_string()),
            timestamp: 0,
        })],
        tools: None,
    })
}

fn completions_model() -> Model {
    Model {
        r#type: None,
        id: "test-model".to_string(),
        name: "Test Model".to_string(),
        api: "openai-completions".to_string(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api/v1".to_string(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn responses_model() -> Model {
    Model {
        r#type: None,
        id: "gpt-test".to_string(),
        name: "GPT Test".to_string(),
        api: "openai-responses".to_string(),
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".to_string(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn error_fetch(status: u16, body: &'static str) -> FetchFunction {
    Arc::new(move |_req| {
        Box::pin(async move {
            let mut headers = Headers::new();
            headers.set("content-type", "application/json");
            Ok(FetchResponse::new(status, headers, body.as_bytes().to_vec()))
        })
    })
}

#[tokio::test]
async fn openai_completions_and_responses_surface_status_and_body_without_duplicating_raw_metadata() {
    // WIRE(@mcp-w2): also test bedrock_converse_stream error metadata once @mcp-w2 lands.
    let context = make_context();

    let output = stream_openai_completions(
        &completions_model(),
        &context,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_string()),
                fetch: Some(error_fetch(403, r#"{"error":"blocked by gateway WAF"}"#)),
                max_retries: Some(0),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;
    assert_eq!(output.stop_reason, StopReason::Error);
    let err = output.error_message.as_deref().unwrap_or("");
    assert!(err.contains("403"), "err={err}");
    assert!(err.contains("blocked by gateway WAF"), "err={err}");

    let raw_body =
        r#"{"message":"Provider returned error","code":403,"metadata":{"raw":"upstream WAF blocked policy XYZ"}}"#;
    let openrouter_out = stream_openai_completions(
        &completions_model(),
        &context,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_string()),
                fetch: Some(error_fetch(403, raw_body)),
                max_retries: Some(0),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;
    let err = openrouter_out.error_message.as_deref().unwrap_or("");
    assert!(err.contains("upstream WAF blocked policy XYZ"), "err={err}");
    assert_eq!(err.matches("upstream WAF blocked policy XYZ").count(), 1, "err={err}");

    let resp_out = stream_openai_responses(
        &responses_model(),
        &context,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_string()),
                fetch: Some(error_fetch(403, r#"{"error":"blocked by gateway WAF"}"#)),
                max_retries: Some(0),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;
    assert_eq!(resp_out.stop_reason, StopReason::Error);
    let err = resp_out.error_message.as_deref().unwrap_or("");
    assert!(err.contains("OpenAI API error (403)"), "err={err}");
    assert!(err.contains("blocked by gateway WAF"), "err={err}");
}
