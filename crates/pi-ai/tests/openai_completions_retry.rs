//! Port of packages/ai/test/openai-completions-retry.test.ts

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_ai::api::openai_completions::stream as stream_openai_completions;
use pi_ai::compat::normalize_context;
use pi_ai::types::{
    ApiStreamOptions, Context, Message, Model, ModelCost, ModelInput, StopReason, StreamOptions, TextContent,
    UserContentBlock, UserMessage, UserMessageContent,
};
use pi_js::fetch::{Headers, Response};
use serde_json::json;

fn test_model() -> Model {
    Model {
        r#type: None,
        id: "test-model".to_owned(),
        name: "Test Model".to_owned(),
        api: "openai-completions".to_owned(),
        provider: "opencode-go".to_owned(),
        base_url: "https://opencode.ai/zen/go/v1".to_owned(),
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

fn test_context() -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: Some(String::new()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new("hi"))]),
            timestamp: 0,
        })],
        tools: Some(Vec::new()),
    })
}

fn ok_sse() -> Response {
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({
            "id": "chatcmpl-test",
            "choices": [{ "index": 0, "delta": { "content": "ok" } }],
        }),
        json!({
            "id": "chatcmpl-test",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        }),
    );
    Response::new(200, Headers::from_pairs([("content-type", "text/event-stream")]), body)
}

#[tokio::test]
async fn makes_single_attempt_without_retries_by_default_or_on_success() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_c = calls.clone();

    let result = stream_openai_completions(
        &test_model(),
        &test_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                max_retries: Some(0),
                fetch: Some(Arc::new(move |_| {
                    calls_c.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async move { Ok(ok_sse()) })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn honors_provider_retries_using_server_retry_after_ms_headers() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_c = calls.clone();

    let responses = Arc::new(Mutex::new(vec![
        Response::new(
            429,
            Headers::from_pairs([("content-type", "application/json"), ("retry-after-ms", "100")]),
            json!({ "error": { "message": "rate limited" } }).to_string(),
        ),
        Response::new(
            500,
            Headers::from_pairs([("content-type", "application/json"), ("retry-after-ms", "100")]),
            json!({ "error": { "message": "server error" } }).to_string(),
        ),
    ]));
    let resp_c = responses.clone();

    let stream = stream_openai_completions(
        &test_model(),
        &test_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                max_retries: Some(2),
                max_retry_delay_ms: Some(100),
                fetch: Some(Arc::new(move |_| {
                    calls_c.fetch_add(1, Ordering::SeqCst);
                    let mut guard = resp_c.lock().unwrap();
                    let next = if !guard.is_empty() { guard.remove(0) } else { ok_sse() };
                    Box::pin(async move { Ok(next) })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    );

    let handle = tokio::spawn(async move { stream.result().await });
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    tokio::time::advance(Duration::from_millis(99)).await;
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    tokio::time::advance(Duration::from_millis(99)).await;
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    tokio::time::advance(Duration::from_millis(1)).await;
    let result = handle.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(result.stop_reason, StopReason::Stop);
}

#[tokio::test]
async fn fails_immediately_when_provider_requested_retry_delay_exceeds_limit() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_c = calls.clone();

    let result = stream_openai_completions(
        &test_model(),
        &test_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                max_retries: Some(2),
                max_retry_delay_ms: Some(1000),
                fetch: Some(Arc::new(move |_| {
                    calls_c.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async move {
                        Ok(Response::new(
                            429,
                            Headers::from_pairs([("content-type", "application/json"), ("retry-after", "277403")]),
                            json!({ "error": { "message": "rate limited" } }).to_string(),
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

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.stop_reason, StopReason::Error);
    let msg = result.error_message.as_deref().unwrap_or("");
    assert!(msg.contains("Server requested 277403s retry delay (max: 1s)"), "{msg}");
    assert!(msg.contains("rate limited"), "{msg}");
}
