//! Port of packages/ai/test/anthropic-eager-tool-input-compat.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::api::anthropic_messages::stream;
use pi_ai::compat::normalize_context;
use pi_ai::types::{
    AnthropicMessagesCompat, ApiStreamOptions, CacheRetention, Context, Message, Model, ModelCompat, ModelCost,
    ModelInput, StreamOptions, Tool, UserMessage, UserMessageContent,
};
use pi_js::fetch::{Headers, Request, Response};
use pi_js::time;
use pi_js::vendor::typebox::t;
use serde_json::Value;

struct CapturedRequest {
    headers: Headers,
    body: Value,
}

fn create_model(compat: Option<AnthropicMessagesCompat>) -> Model {
    let mut merged = compat.unwrap_or_default();
    if merged.force_adaptive_thinking.is_none() {
        merged.force_adaptive_thinking = Some(true);
    }
    Model {
        r#type: None,
        id: "claude-opus-4-8".to_owned(),
        name: "Claude Opus 4.8".to_owned(),
        api: "anthropic-messages".to_owned(),
        provider: "test-anthropic".to_owned(),
        base_url: "https://example.test".to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 200_000,
        max_tokens: 32_000,
        headers: None,
        sampling_params: None,
        compat: Some(ModelCompat::AnthropicMessages(merged)),
    }
}

fn sample_tool() -> Tool {
    Tool {
        name: "lookup".to_owned(),
        description: "Look up a value".to_owned(),
        parameters: t::object([("value", t::string())]),
        constrained_sampling: None,
    }
}

fn create_context(tools: Vec<Tool>) -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Use the tool".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: if tools.is_empty() { None } else { Some(tools) },
    }
}

async fn capture_anthropic_request(compat: Option<AnthropicMessagesCompat>, context: Context) -> CapturedRequest {
    let captured = Arc::new(Mutex::new(None));
    let cap_c = captured.clone();

    stream(
        &create_model(compat),
        &normalize_context(&context),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test-key".to_owned()),
                cache_retention: Some(CacheRetention::None),
                fetch: Some(Arc::new(move |req: Request| {
                    let cap = cap_c.clone();
                    Box::pin(async move {
                        let body = req.body_json().unwrap_or(Value::Null);
                        *cap.lock().unwrap() = Some(CapturedRequest {
                            headers: req.headers,
                            body,
                        });
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            "",
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

    captured
        .lock()
        .unwrap()
        .take()
        .expect("Anthropic request was not captured")
}

fn first_tool(body: &Value) -> &Value {
    &body
        .get("tools")
        .and_then(Value::as_array)
        .expect("Expected first tool in request body")[0]
}

#[tokio::test]
async fn sends_per_tool_eager_input_streaming_by_default() {
    let req = capture_anthropic_request(None, create_context(vec![sample_tool()])).await;
    assert_eq!(
        first_tool(&req.body).get("eager_input_streaming"),
        Some(&Value::Bool(true))
    );
    assert!(req.headers.get("anthropic-beta").is_none());
}

#[tokio::test]
async fn uses_the_legacy_fine_grained_tool_streaming_beta_when_eager_tool_input_streaming_is_disabled() {
    let req = capture_anthropic_request(
        Some(AnthropicMessagesCompat {
            supports_eager_tool_input_streaming: Some(false),
            ..Default::default()
        }),
        create_context(vec![sample_tool()]),
    )
    .await;

    assert!(first_tool(&req.body).get("eager_input_streaming").is_none());
    assert_eq!(
        req.headers.get("anthropic-beta").as_deref(),
        Some("fine-grained-tool-streaming-2025-05-14")
    );
}

#[tokio::test]
async fn does_not_send_the_legacy_fine_grained_tool_streaming_beta_when_there_are_no_tools() {
    let req = capture_anthropic_request(
        Some(AnthropicMessagesCompat {
            supports_eager_tool_input_streaming: Some(false),
            ..Default::default()
        }),
        create_context(Vec::new()),
    )
    .await;

    assert!(req.body.get("tools").is_none());
    assert!(req.headers.get("anthropic-beta").is_none());
}
