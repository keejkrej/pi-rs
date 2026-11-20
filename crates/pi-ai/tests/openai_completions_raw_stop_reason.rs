//! Port of packages/ai/test/openai-completions-raw-stop-reason.test.ts

use std::sync::Arc;

use pi_ai::api::openai_completions::stream as stream_openai_completions;
use pi_ai::compat::normalize_context;
use pi_ai::types::{
    ApiStreamOptions, Context, Message, Model, ModelCost, ModelInput, StopReason, StreamOptions, UserMessage,
    UserMessageContent,
};
use pi_js::fetch::{Headers, Response};
use pi_js::time;
use serde_json::json;

fn test_model() -> Model {
    Model {
        r#type: None,
        id: "test-model".to_owned(),
        name: "Test Model".to_owned(),
        api: "openai-completions".to_owned(),
        provider: "openai".to_owned(),
        base_url: "https://api.openai.com/v1".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 4096,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn test_context() -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
    })
}

#[tokio::test]
async fn preserves_raw_finish_reasons_for_successful_stops() {
    let sse = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({ "id": "chatcmpl-1", "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }] })
    );
    let msg = stream_openai_completions(
        &test_model(),
        &test_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                fetch: Some(Arc::new(move |_| {
                    let s = sse.clone();
                    Box::pin(async move {
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            s,
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(msg.stop_reason, StopReason::Stop);
    assert_eq!(msg.raw_stop_reason.as_deref(), Some("stop"));
    assert_eq!(msg.error_message, None);
}

#[tokio::test]
async fn preserves_raw_finish_reasons_for_provider_error_stops() {
    let sse = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({ "id": "chatcmpl-2", "choices": [{ "index": 0, "delta": {}, "finish_reason": "content_filter" }] })
    );
    let msg = stream_openai_completions(
        &test_model(),
        &test_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                fetch: Some(Arc::new(move |_| {
                    let s = sse.clone();
                    Box::pin(async move {
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            s,
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(msg.stop_reason, StopReason::Error);
    assert_eq!(msg.raw_stop_reason.as_deref(), Some("content_filter"));
    assert_eq!(
        msg.error_message.as_deref(),
        Some("Provider finish_reason: content_filter")
    );
}
