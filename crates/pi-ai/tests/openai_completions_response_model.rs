//! Port of packages/ai/test/openai-completions-response-model.test.ts

use std::sync::Arc;

use pi_ai::compat::complete;
use pi_ai::types::{
    Context, Message, Model, ModelCost, ModelInput, StopReason, StreamOptions, UserMessage, UserMessageContent,
};
use pi_js::fetch::{Headers, Response};
use pi_js::time;
use serde_json::{Value, json};

fn openrouter_auto() -> Model {
    Model {
        r#type: None,
        id: "openrouter/auto".to_owned(),
        name: "OpenRouter Auto".to_owned(),
        api: "openai-completions".to_owned(),
        provider: "openrouter".to_owned(),
        base_url: "https://openrouter.ai/api/v1".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 200_000,
        max_tokens: 8192,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

async fn run_with_chunks(chunks: Vec<Value>) -> pi_ai::types::AssistantMessage {
    let mut sse = String::new();
    for c in chunks {
        sse.push_str(&format!("data: {c}\n\n"));
    }
    sse.push_str("data: [DONE]\n\n");

    complete(
        &openrouter_auto(),
        &Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("hi".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        Some(
            StreamOptions {
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
            }
            .into(),
        ),
    )
    .await
}

#[tokio::test]
async fn surfaces_routed_chunk_model_on_response_model_without_changing_model() {
    let message = run_with_chunks(vec![
        json!({
            "id": "chatcmpl-1",
            "model": "anthropic/claude-opus-4.8",
            "choices": [{ "index": 0, "delta": { "content": "hi" } }],
        }),
        json!({
            "id": "chatcmpl-1",
            "model": "anthropic/claude-opus-4.8",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            },
        }),
    ])
    .await;

    assert_eq!(message.model, "openrouter/auto");
    assert_eq!(message.response_model.as_deref(), Some("anthropic/claude-opus-4.8"));
    assert_eq!(message.provider, "openrouter");
    assert_eq!(message.stop_reason, StopReason::Stop);
}

#[tokio::test]
async fn leaves_response_model_undefined_when_chunks_echo_the_requested_id() {
    let message = run_with_chunks(vec![
        json!({
            "id": "chatcmpl-2",
            "model": "openrouter/auto",
            "choices": [{ "index": 0, "delta": { "content": "hi" } }],
        }),
        json!({
            "id": "chatcmpl-2",
            "model": "openrouter/auto",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            },
        }),
    ])
    .await;

    assert_eq!(message.model, "openrouter/auto");
    assert_eq!(message.response_model, None);
}

#[tokio::test]
async fn ignores_empty_or_missing_chunk_model() {
    let message = run_with_chunks(vec![
        json!({
            "id": "chatcmpl-3",
            "choices": [{ "index": 0, "delta": { "content": "hi" } }],
        }),
        json!({
            "id": "chatcmpl-3",
            "model": "",
            "choices": [{ "index": 0, "delta": { "content": "!" } }],
        }),
        json!({
            "id": "chatcmpl-3",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 2,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            },
        }),
    ])
    .await;

    assert_eq!(message.model, "openrouter/auto");
    assert_eq!(message.response_model, None);
}
