//! Port of packages/ai/test/openai-responses-prompt-cache.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::api::openai_responses::stream;
use pi_ai::compat::normalize_context;
use pi_ai::types::{
    ApiStreamOptions, CacheRetention, Context, Message, Model, ModelCompat, ModelCost, ModelInput,
    OpenAIResponsesCompat, PromptCacheKeyMode, StreamOptions, UserMessage, UserMessageContent,
};
use pi_js::fetch::{Headers, Request, Response};
use serde_json::Value;

fn create_model(prompt_cache_key_mode: Option<PromptCacheKeyMode>) -> Model {
    Model {
        r#type: None,
        id: "test-model".to_owned(),
        name: "Test Model".to_owned(),
        api: "openai-responses".to_owned(),
        provider: "test-proxy".into(),
        base_url: "https://proxy.example.com/v1".to_owned(),
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
        compat: Some(ModelCompat::OpenAIResponses(OpenAIResponsesCompat {
            prompt_cache_key_mode,
            ..Default::default()
        })),
        sampling_params_by_thinking_level: None,
    }
}

async fn capture_payload(
    prompt_cache_key_mode: Option<PromptCacheKeyMode>,
    cache_retention: Option<CacheRetention>,
) -> Value {
    let payload = Arc::new(Mutex::new(None::<Value>));
    let p_c = Arc::clone(&payload);
    let ctx = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_owned()),
            timestamp: 1,
        })],
        tools: None,
    });
    stream(
        &create_model(prompt_cache_key_mode),
        &ctx,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test-key".to_owned()),
                session_id: Some("session-123".to_owned()),
                cache_retention,
                fetch: Some(Arc::new(move |req: Request| {
                    *p_c.lock().unwrap() = req.body_json();
                    Box::pin(async move {
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2,\"input_tokens_details\":{\"cached_tokens\":0}}}}\n\n",
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
    payload.lock().unwrap().take().expect("captured payload")
}

#[tokio::test]
async fn openai_responses_prompt_cache_key_policy() {
    assert_eq!(
        capture_payload(None, None)
            .await
            .get("prompt_cache_key")
            .and_then(Value::as_str),
        Some("session-123")
    );
    assert_eq!(
        capture_payload(Some(PromptCacheKeyMode::Enabled), None)
            .await
            .get("prompt_cache_key")
            .and_then(Value::as_str),
        Some("session-123")
    );
    assert!(
        capture_payload(Some(PromptCacheKeyMode::Disabled), None)
            .await
            .get("prompt_cache_key")
            .is_none()
    );
    assert!(
        capture_payload(Some(PromptCacheKeyMode::Enabled), Some(CacheRetention::None))
            .await
            .get("prompt_cache_key")
            .is_none()
    );
}
