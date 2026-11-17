//! Port of packages/ai/test/sampling-options.test.ts (unit ai-providers-builtin)

use std::sync::{Arc, Mutex};

use pi_ai::compat::{stream, stream_simple};
use pi_ai::types::{
    ApiStreamOptions, Context, JsonObject, Message, Model, ModelCost, ModelInput, OnPayloadFn, SimpleStreamOptions,
    StreamOptions, UserMessage, UserMessageContent,
};
use serde_json::{Map, Value, json};

fn make_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Hello".to_string()),
            timestamp: 1,
        })],
        tools: None,
    }
}

fn make_model(api: &str, sampling_params: Option<JsonObject>) -> Model {
    Model {
        r#type: None,
        id: "custom-model".to_string(),
        name: "Custom Model".to_string(),
        api: api.to_string(),
        provider: "custom-provider".into(),
        base_url: "http://127.0.0.1:9/v1".to_string(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128000,
        max_tokens: 16384,
        headers: None,
        sampling_params,
        compat: None,
    }
}

fn capture_hook(slot: Arc<Mutex<Option<Value>>>) -> OnPayloadFn<Model> {
    Arc::new(move |payload, _model| {
        let slot = Arc::clone(&slot);
        Box::pin(async move {
            *slot.lock().unwrap() = Some(payload);
            Err(pi_js::Error::msg("payload captured"))
        })
    })
}

fn obj(val: Value) -> JsonObject {
    let Map { .. } = Map::new();
    match val {
        Value::Object(m) => m.into_iter().collect(),
        _ => JsonObject::new(),
    }
}

async fn capture_payload(model: &Model, opts: Option<StreamOptions>) -> Value {
    let slot = Arc::new(Mutex::new(None));
    let mut base = opts.unwrap_or_default();
    base.api_key = Some("fake-key".to_string());
    base.on_payload = Some(capture_hook(Arc::clone(&slot)));
    stream(
        model,
        &make_context(),
        Some(ApiStreamOptions {
            base,
            ..Default::default()
        }),
    )
    .result()
    .await;
    slot.lock().unwrap().clone().expect("Expected payload to be captured")
}

#[tokio::test]
async fn merges_request_sampling_params_and_omits_when_unset() {
    let payload = capture_payload(
        &make_model("openai-completions", None),
        Some(StreamOptions {
            sampling_params: Some(obj(json!({ "top_p": 0.95, "top_k": 0, "min_p": 0 }))),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(payload["top_p"], 0.95);
    assert_eq!(payload["top_k"], 0);
    assert_eq!(payload["min_p"], 0);

    let empty_payload = capture_payload(&make_model("openai-completions", None), None).await;
    assert!(empty_payload.get("temperature").is_none());
    assert!(empty_payload.get("top_p").is_none());
}

#[tokio::test]
async fn applies_model_level_sampling_params_with_request_precedence() {
    for api in ["openai-completions", "openai-responses", "azure-openai-responses"] {
        let payload = capture_payload(
            &make_model(api, Some(obj(json!({ "top_p": 0.95, "min_p": 0.05 })))),
            Some(StreamOptions {
                sampling_params: Some(obj(json!({ "top_p": 0.5 }))),
                ..Default::default()
            }),
        )
        .await;
        assert_eq!(payload["top_p"], 0.5, "api={api}");
        assert_eq!(payload["min_p"], 0.05, "api={api}");
    }
}

#[tokio::test]
async fn passes_sampling_params_through_stream_simple_and_overrides_named_fields() {
    let slot = Arc::new(Mutex::new(None));
    stream_simple(
        &make_model("openai-completions", None),
        &make_context(),
        Some(SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("fake-key".to_string()),
                sampling_params: Some(obj(json!({ "top_p": 0.5 }))),
                on_payload: Some(capture_hook(Arc::clone(&slot))),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;
    assert_eq!(slot.lock().unwrap().as_ref().unwrap()["top_p"], 0.5);

    let overridden = capture_payload(
        &make_model("openai-completions", None),
        Some(StreamOptions {
            temperature: Some(0.0),
            sampling_params: Some(obj(json!({ "temperature": 1 }))),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(overridden["temperature"], 1);

    let anthropic = capture_payload(
        &make_model("anthropic-messages", None),
        Some(StreamOptions {
            sampling_params: Some(obj(json!({ "top_p": 0.9, "top_k": 40 }))),
            ..Default::default()
        }),
    )
    .await;
    assert!(anthropic.get("top_p").is_none());
    assert!(anthropic.get("top_k").is_none());
}
