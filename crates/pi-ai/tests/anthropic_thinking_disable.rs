//! Port of packages/ai/test/anthropic-thinking-disable.test.ts

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use pi_ai::compat::{get_model, stream_simple};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessageEvent, Context, Message, Model, SimpleStreamOptions, StopReason,
    StreamOptions, ThinkingLevel, UserMessage, UserMessageContent,
};
use pi_js::{Error, time};
use serde_json::{Value, json};

fn make_payload_capture_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Hello".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
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
        &make_payload_capture_context(),
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
async fn sends_thinking_type_disabled_for_budget_based_reasoning_models_when_thinking_is_off() {
    let payload = capture_payload(&get_model("anthropic", "claude-sonnet-4-5").unwrap(), None).await;
    assert_eq!(payload.get("thinking"), Some(&json!({ "type": "disabled" })));
    assert!(payload.get("output_config").is_none());
}

#[tokio::test]
async fn sends_thinking_type_disabled_for_adaptive_reasoning_models_when_thinking_is_off() {
    let payload = capture_payload(&get_model("anthropic", "claude-opus-4-6").unwrap(), None).await;
    assert_eq!(payload.get("thinking"), Some(&json!({ "type": "disabled" })));
    assert!(payload.get("output_config").is_none());
}

#[tokio::test]
async fn sends_thinking_type_disabled_for_claude_opus_4_8_when_thinking_is_off() {
    let payload = capture_payload(&get_model("anthropic", "claude-opus-4-8").unwrap(), None).await;
    assert_eq!(payload.get("thinking"), Some(&json!({ "type": "disabled" })));
    assert!(payload.get("output_config").is_none());
}

#[tokio::test]
async fn omits_thinking_type_disabled_for_claude_fable_5_when_thinking_is_off() {
    let payload = capture_payload(&get_model("anthropic", "claude-fable-5").unwrap(), None).await;
    assert!(payload.get("thinking").is_none());
    assert!(payload.get("output_config").is_none());
}

#[tokio::test]
async fn uses_adaptive_thinking_for_claude_opus_4_8_when_reasoning_is_enabled() {
    let payload = capture_payload(
        &get_model("anthropic", "claude-opus-4-8").unwrap(),
        Some(ThinkingLevel::High),
    )
    .await;
    assert_eq!(
        payload.get("thinking"),
        Some(&json!({ "type": "adaptive", "display": "summarized" }))
    );
    assert_eq!(payload.get("output_config"), Some(&json!({ "effort": "high" })));
}

#[tokio::test]
async fn uses_adaptive_thinking_for_claude_sonnet_5_when_reasoning_is_enabled() {
    let payload = capture_payload(
        &get_model("anthropic", "claude-sonnet-5").unwrap(),
        Some(ThinkingLevel::High),
    )
    .await;
    assert_eq!(
        payload.get("thinking"),
        Some(&json!({ "type": "adaptive", "display": "summarized" }))
    );
    assert_eq!(payload.get("output_config"), Some(&json!({ "effort": "high" })));
}

#[tokio::test]
async fn maps_xhigh_reasoning_to_effort_xhigh_for_claude_opus_4_8() {
    let payload = capture_payload(
        &get_model("anthropic", "claude-opus-4-8").unwrap(),
        Some(ThinkingLevel::XHigh),
    )
    .await;
    assert_eq!(
        payload.get("thinking"),
        Some(&json!({ "type": "adaptive", "display": "summarized" }))
    );
    assert_eq!(payload.get("output_config"), Some(&json!({ "effort": "xhigh" })));
}

#[tokio::test]
#[ignore = "live e2e requires credentials"]
async fn disables_thinking_for_claude_reasoning_models_live_e2e() {
    let model = get_model("anthropic", "claude-sonnet-4-5").unwrap();
    let context = Context {
        system_prompt: Some(
            "You are a precise assistant. Follow the requested output format exactly.".to_owned(),
        ),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text(
                "Before replying, carefully solve 36863 * 5279 internally. Then reply with the word pong repeated exactly 40 times, separated by single spaces. Do not add any other text.".to_owned(),
            ),
            timestamp: time::now_ms(),
        })],
        tools: None,
    };

    let mut s = stream_simple(
        &model,
        &context,
        Some(SimpleStreamOptions {
            base: StreamOptions {
                temperature: Some(0.0),
                max_tokens: Some(160),
                ..Default::default()
            },
            ..Default::default()
        }),
    );

    let mut thinking_events = 0usize;
    while let Some(ev) = s.next().await {
        if matches!(
            ev,
            AssistantMessageEvent::ThinkingStart { .. }
                | AssistantMessageEvent::ThinkingDelta { .. }
                | AssistantMessageEvent::ThinkingEnd { .. }
        ) {
            thinking_events += 1;
        }
    }
    let response = s.result().await;
    assert_eq!(response.stop_reason, StopReason::Stop);
    assert_eq!(thinking_events, 0);
    assert!(
        !response
            .content
            .iter()
            .any(|b| matches!(b, AssistantContentBlock::Thinking(_)))
    );
}
