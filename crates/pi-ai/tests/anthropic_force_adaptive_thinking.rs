//! Port of packages/ai/test/anthropic-force-adaptive-thinking.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::compat::{get_model, stream_simple};
use pi_ai::types::{
    AnthropicMessagesCompat, Context, Message, Model, ModelCompat, ModelCost, ModelInput, SimpleStreamOptions,
    StreamOptions, ThinkingLevel, UserMessage, UserMessageContent,
};
use pi_js::{Error, time};
use serde_json::{Value, json};

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
        id: "vendor--claude-opus-latest".to_owned(),
        name: "Vendor Proxy Opus Latest".to_owned(),
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

async fn capture_payload(model: &Model, reasoning: Option<ThinkingLevel>) -> Value {
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
                on_payload: Some(Arc::new(move |payload, _| {
                    let cap = cap_c.clone();
                    Box::pin(async move {
                        *cap.lock().unwrap() = Some(payload);
                        Err(Error::js("PayloadCaptured", "payload captured"))
                    })
                })),
                ..Default::default()
            },
            reasoning,
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
async fn sends_legacy_thinking_payload_for_custom_model_ids_by_default() {
    let payload = capture_payload(&make_custom_model(None), Some(ThinkingLevel::Medium)).await;
    assert_eq!(
        payload
            .get("thinking")
            .and_then(|t| t.get("type"))
            .and_then(Value::as_str),
        Some("enabled")
    );
    assert!(payload.get("output_config").is_none());
}

#[tokio::test]
async fn sends_adaptive_thinking_payload_when_compat_force_adaptive_thinking_is_true() {
    let payload = capture_payload(
        &make_custom_model(Some(AnthropicMessagesCompat {
            force_adaptive_thinking: Some(true),
            ..Default::default()
        })),
        Some(ThinkingLevel::Medium),
    )
    .await;
    assert_eq!(
        payload.get("thinking"),
        Some(&json!({ "type": "adaptive", "display": "summarized" }))
    );
    assert_eq!(payload.get("output_config"), Some(&json!({ "effort": "medium" })));
}

#[tokio::test]
async fn uses_adaptive_thinking_with_native_xhigh_effort_for_claude_fable_5() {
    let model = get_model("anthropic", "claude-fable-5").unwrap();
    let payload = capture_payload(&model, Some(ThinkingLevel::XHigh)).await;
    assert_eq!(
        payload.get("thinking"),
        Some(&json!({ "type": "adaptive", "display": "summarized" }))
    );
    assert_eq!(payload.get("output_config"), Some(&json!({ "effort": "xhigh" })));
}

#[tokio::test]
async fn uses_adaptive_thinking_effort_without_a_token_budget_for_kimi_coding() {
    for (model_id, reasoning, effort) in [
        ("kimi-for-coding", ThinkingLevel::Medium, "medium"),
        ("k3", ThinkingLevel::Max, "max"),
        ("kimi-for-coding-highspeed", ThinkingLevel::Medium, "medium"),
    ] {
        let model = get_model("kimi-coding", model_id).unwrap();
        let payload = capture_payload(&model, Some(reasoning)).await;
        assert_eq!(
            payload.get("thinking"),
            Some(&json!({ "type": "adaptive", "display": "summarized" }))
        );
        assert_eq!(payload.get("output_config"), Some(&json!({ "effort": effort })));
    }
}

#[tokio::test]
async fn allows_built_in_adaptive_models_to_opt_out_with_compat_force_adaptive_thinking_false() {
    let mut model = get_model("anthropic", "claude-opus-4-8").unwrap();
    model.compat = Some(ModelCompat::AnthropicMessages(AnthropicMessagesCompat {
        force_adaptive_thinking: Some(false),
        ..Default::default()
    }));
    let payload = capture_payload(&model, Some(ThinkingLevel::Medium)).await;
    assert_eq!(
        payload
            .get("thinking")
            .and_then(|t| t.get("type"))
            .and_then(Value::as_str),
        Some("enabled")
    );
    assert!(payload.get("output_config").is_none());
}

#[tokio::test]
async fn preserves_thinking_type_disabled_when_reasoning_is_off_regardless_of_override() {
    let payload = capture_payload(
        &make_custom_model(Some(AnthropicMessagesCompat {
            force_adaptive_thinking: Some(true),
            ..Default::default()
        })),
        None,
    )
    .await;
    assert_eq!(payload.get("thinking"), Some(&json!({ "type": "disabled" })));
    assert!(payload.get("output_config").is_none());
}
