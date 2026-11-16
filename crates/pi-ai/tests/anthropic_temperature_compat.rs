//! Port of packages/ai/test/anthropic-temperature-compat.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::compat::{get_model, stream_simple};
use pi_ai::types::{
    AnthropicMessagesCompat, Context, Message, Model, ModelCompat, ModelCost, ModelInput, SimpleStreamOptions,
    StreamOptions, UserMessage, UserMessageContent,
};
use pi_js::{Error, time};
use serde_json::Value;

fn make_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Hello".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
    }
}

fn make_custom_model(compat: Option<AnthropicMessagesCompat>) -> Model {
    Model {
        r#type: None,
        id: "vendor--claude-opus-4-7".to_owned(),
        name: "Vendor Proxy Opus 4.7".to_owned(),
        api: "anthropic-messages".to_owned(),
        provider: "vendor-proxy".to_owned(),
        base_url: "http://127.0.0.1:9".to_owned(),
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
        compat: compat.map(ModelCompat::AnthropicMessages),
    }
}

async fn capture_payload(model: &Model, temperature: Option<f64>) -> Value {
    let captured = Arc::new(Mutex::new(None));
    let cap_c = captured.clone();
    let payload_model = Model {
        base_url: "http://127.0.0.1:9".to_owned(),
        ..model.clone()
    };

    stream_simple(
        &payload_model,
        &make_context(),
        Some(SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("fake-key".to_owned()),
                temperature,
                on_payload: Some(Arc::new(move |payload, _| {
                    let cap = cap_c.clone();
                    Box::pin(async move {
                        *cap.lock().unwrap() = Some(payload);
                        Err(Error::js("PayloadCaptured", "payload captured"))
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
        .expect("Expected payload to be captured before request failure")
}

#[tokio::test]
async fn omits_temperature_for_claude_opus_4_7() {
    let payload = capture_payload(&get_model("anthropic", "claude-opus-4-7").unwrap(), Some(0.0)).await;
    assert!(payload.get("temperature").is_none());
}

#[tokio::test]
async fn omits_temperature_for_claude_opus_4_8() {
    let payload = capture_payload(&get_model("anthropic", "claude-opus-4-8").unwrap(), Some(0.0)).await;
    assert!(payload.get("temperature").is_none());
}

#[tokio::test]
async fn omits_default_temperature_for_claude_opus_4_7() {
    let payload = capture_payload(&get_model("anthropic", "claude-opus-4-7").unwrap(), Some(1.0)).await;
    assert!(payload.get("temperature").is_none());
}

#[tokio::test]
async fn keeps_temperature_for_claude_opus_4_6() {
    let payload = capture_payload(&get_model("anthropic", "claude-opus-4-6").unwrap(), Some(0.0)).await;
    assert_eq!(payload.get("temperature").and_then(Value::as_f64), Some(0.0));
}

#[tokio::test]
async fn keeps_temperature_for_claude_sonnet_4_6() {
    let payload = capture_payload(&get_model("anthropic", "claude-sonnet-4-6").unwrap(), Some(0.0)).await;
    assert_eq!(payload.get("temperature").and_then(Value::as_f64), Some(0.0));
}

#[tokio::test]
async fn omits_temperature_for_custom_models_with_supports_temperature_disabled() {
    let payload = capture_payload(
        &make_custom_model(Some(AnthropicMessagesCompat {
            supports_temperature: Some(false),
            ..Default::default()
        })),
        Some(0.0),
    )
    .await;
    assert!(payload.get("temperature").is_none());
}
