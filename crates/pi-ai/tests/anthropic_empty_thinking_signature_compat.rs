//! Port of packages/ai/test/anthropic-empty-thinking-signature-compat.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::compat::{get_model, get_models, stream_simple};
use pi_ai::types::{
    AnthropicMessagesCompat, AssistantContentBlock, AssistantMessage, Context, Message, Model, ModelCompat, ModelCost,
    ModelInput, SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingContent, Usage, UserMessage,
    UserMessageContent,
};
use pi_js::{Error, time};
use serde_json::{Value, json};

fn make_model(allow_empty_signature: Option<bool>) -> Model {
    Model {
        r#type: None,
        id: "mimo-v2.5-pro".to_owned(),
        name: "MiMo-V2.5-Pro".to_owned(),
        api: "anthropic-messages".to_owned(),
        provider: "xiaomi-token-plan-ams".to_owned(),
        base_url: "http://127.0.0.1:9/anthropic".to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 1_048_576,
        max_tokens: 1024,
        headers: None,
        sampling_params: None,
        compat: allow_empty_signature.map(|flag| {
            ModelCompat::AnthropicMessages(AnthropicMessagesCompat {
                allow_empty_signature: Some(flag),
                ..Default::default()
            })
        }),
    }
}

fn make_context(
    thinking_signature: &str,
    thinking: &str,
    provider: &str,
    model_id: &str,
    extra_text: Option<&str>,
) -> Context {
    let mut content = vec![AssistantContentBlock::Thinking(ThinkingContent {
        thinking: thinking.to_owned(),
        thinking_signature: Some(thinking_signature.to_owned()),
        redacted: None,
    })];
    if let Some(txt) = extra_text {
        content.push(AssistantContentBlock::Text(TextContent::new(txt)));
    }
    let assistant = AssistantMessage {
        content,
        api: "anthropic-messages".to_owned(),
        provider: provider.to_owned(),
        model: model_id.to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: time::now_ms(),
    };
    Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("first".to_owned()),
                timestamp: time::now_ms(),
            }),
            Message::Assistant(assistant),
            Message::User(UserMessage {
                content: UserMessageContent::Text("second".to_owned()),
                timestamp: time::now_ms(),
            }),
        ],
        tools: None,
    }
}

async fn capture_payload(model: &Model, context: &Context) -> Value {
    let captured = Arc::new(Mutex::new(None));
    let cap_c = captured.clone();

    stream_simple(
        model,
        context,
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
            ..Default::default()
        }),
    )
    .result()
    .await;

    captured
        .lock()
        .unwrap()
        .take()
        .expect("Expected payload capture before request")
}

fn assistant_content(payload: &Value) -> &Vec<Value> {
    payload
        .get("messages")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .unwrap()
        .get("content")
        .and_then(Value::as_array)
        .unwrap()
}

#[tokio::test]
async fn converts_empty_signature_thinking_to_text_by_default() {
    let payload = capture_payload(
        &make_model(None),
        &make_context("", "internal reasoning", "xiaomi-token-plan-ams", "mimo-v2.5-pro", None),
    )
    .await;
    assert_eq!(
        assistant_content(&payload),
        &vec![json!({ "type": "text", "text": "internal reasoning" })]
    );
}

#[tokio::test]
async fn preserves_empty_thinking_text_when_the_signature_is_present() {
    let payload = capture_payload(
        &make_model(None),
        &make_context("signed-thinking", "", "xiaomi-token-plan-ams", "mimo-v2.5-pro", None),
    )
    .await;
    assert_eq!(
        assistant_content(&payload),
        &vec![json!({ "type": "thinking", "thinking": "", "signature": "signed-thinking" })]
    );
}

#[tokio::test]
async fn preserves_empty_signature_thinking_when_allow_empty_signature_is_enabled() {
    let payload = capture_payload(
        &make_model(Some(true)),
        &make_context(
            " ",
            "internal reasoning",
            "xiaomi-token-plan-ams",
            "mimo-v2.5-pro",
            None,
        ),
    )
    .await;
    assert_eq!(
        assistant_content(&payload),
        &vec![json!({ "type": "thinking", "thinking": "internal reasoning", "signature": "" })]
    );
}

#[test]
fn allows_empty_thinking_signatures_for_every_vercel_ai_gateway_model() {
    let models = get_models(Some("vercel-ai-gateway"));
    assert!(!models.is_empty());
    assert!(models.iter().all(|model| {
        matches!(
            &model.compat,
            Some(ModelCompat::AnthropicMessages(c)) if c.allow_empty_signature == Some(true)
        )
    }));
}

#[tokio::test]
async fn preserves_unsigned_thinking_for_fireworks_models() {
    for model_id in [
        "accounts/fireworks/models/deepseek-v4p1-flash",
        "accounts/fireworks/models/qwen3p8-max",
        "accounts/fireworks/models/qwen3p8-2p4t-a95b",
        "accounts/fireworks/models/nemotron-3-ultra-nvfp4",
    ] {
        let model = get_model("fireworks", model_id).unwrap();
        assert!(matches!(
            &model.compat,
            Some(ModelCompat::AnthropicMessages(c)) if c.allow_empty_signature == Some(true)
        ));
        let context = make_context("", "internal reasoning", "fireworks", model_id, Some("answer"));
        let payload = capture_payload(&model, &context).await;
        assert_eq!(
            assistant_content(&payload),
            &vec![
                json!({ "type": "thinking", "thinking": "internal reasoning", "signature": "" }),
                json!({ "type": "text", "text": "answer" }),
            ]
        );
    }
}

#[tokio::test]
async fn still_converts_cross_model_fireworks_thinking_to_text() {
    let model = get_model("fireworks", "accounts/fireworks/models/deepseek-v4p1-flash").unwrap();
    let payload = capture_payload(
        &model,
        &make_context(
            "",
            "internal reasoning",
            "fireworks",
            "accounts/fireworks/models/nemotron-3-ultra-nvfp4",
            None,
        ),
    )
    .await;
    assert_eq!(
        assistant_content(&payload),
        &vec![json!({ "type": "text", "text": "internal reasoning" })]
    );
}

#[tokio::test]
async fn allows_empty_signatures_for_kimi_coding_k3() {
    let model = get_model("kimi-coding", "k3").unwrap();
    assert!(matches!(
        &model.compat,
        Some(ModelCompat::AnthropicMessages(c)) if c.allow_empty_signature == Some(true)
    ));
    let payload = capture_payload(
        &model,
        &make_context(" ", "internal reasoning", "kimi-coding", "k3", None),
    )
    .await;
    assert_eq!(
        assistant_content(&payload),
        &vec![json!({ "type": "thinking", "thinking": "internal reasoning", "signature": "" })]
    );
}
