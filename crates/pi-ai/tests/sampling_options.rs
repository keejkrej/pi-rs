//! Port of packages/ai/test/sampling-options.test.ts (unit ai-providers-builtin)

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::compat::{stream, stream_simple};
use pi_ai::types::{
    ApiStreamOptions, Context, JsonObject, Message, Model, ModelCost, ModelInput, ModelThinkingLevel, OnPayloadFn,
    SamplingParamsByThinkingLevel, SimpleStreamOptions, StreamOptions, ThinkingLevel, ThinkingLevelMap, UserMessage,
    UserMessageContent,
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
        sampling_params_by_thinking_level: None,
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

async fn capture_api_payload(model: &Model, opts: ApiStreamOptions) -> Value {
    let slot = Arc::new(Mutex::new(None));
    let mut api_opts = opts;
    api_opts.base.api_key = Some("fake-key".to_string());
    api_opts.base.on_payload = Some(capture_hook(Arc::clone(&slot)));
    stream(model, &make_context(), Some(api_opts)).result().await;
    slot.lock().unwrap().clone().expect("Expected payload to be captured")
}

async fn capture_payload(model: &Model, opts: Option<StreamOptions>) -> Value {
    capture_api_payload(
        model,
        ApiStreamOptions {
            base: opts.unwrap_or_default(),
            ..Default::default()
        },
    )
    .await
}

async fn capture_simple_payload(model: &Model, opts: Option<SimpleStreamOptions>) -> Value {
    let slot = Arc::new(Mutex::new(None));
    let mut simple = opts.unwrap_or_default();
    simple.base.api_key = Some("fake-key".to_string());
    simple.base.on_payload = Some(capture_hook(Arc::clone(&slot)));
    stream_simple(model, &make_context(), Some(simple)).result().await;
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

#[tokio::test]
async fn applies_sampling_params_for_the_effective_thinking_level_over_model_defaults() {
    let mut tmap = ThinkingLevelMap::new();
    tmap.insert(ModelThinkingLevel::Low, None);
    tmap.insert(ModelThinkingLevel::Medium, None);

    let mut by_level: SamplingParamsByThinkingLevel = IndexMap::new();
    by_level.insert(
        ModelThinkingLevel::High,
        obj(json!({ "temperature": 0.8, "top_k": 64 })),
    );

    let mut model = make_model(
        "openai-completions",
        Some(obj(json!({ "temperature": 1, "top_p": 0.95 }))),
    );
    model.reasoning = true;
    model.thinking_level_map = Some(tmap);
    model.sampling_params_by_thinking_level = Some(by_level);

    let payload = capture_simple_payload(
        &model,
        Some(SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::Low),
            ..Default::default()
        }),
    )
    .await;

    assert_eq!(payload["temperature"], 0.8);
    assert_eq!(payload["top_p"], 0.95);
    assert_eq!(payload["top_k"], 64);
}

#[tokio::test]
async fn applies_off_sampling_params_when_reasoning_is_disabled() {
    let mut by_level: SamplingParamsByThinkingLevel = IndexMap::new();
    by_level.insert(ModelThinkingLevel::Off, obj(json!({ "temperature": 0.7 })));

    let mut model = make_model("openai-completions", None);
    model.sampling_params_by_thinking_level = Some(by_level);

    let payload = capture_simple_payload(&model, None).await;
    assert_eq!(payload["temperature"], 0.7);
}

#[tokio::test]
async fn merges_stream_option_keys_over_thinking_level_keys() {
    let mut by_level: SamplingParamsByThinkingLevel = IndexMap::new();
    by_level.insert(
        ModelThinkingLevel::Low,
        obj(json!({ "temperature": 0.6, "top_p": 0.95 })),
    );

    let mut model = make_model("openai-completions", None);
    model.reasoning = true;
    model.sampling_params_by_thinking_level = Some(by_level);

    let payload = capture_simple_payload(
        &model,
        Some(SimpleStreamOptions {
            base: StreamOptions {
                sampling_params: Some(obj(json!({ "top_p": 0.5 }))),
                ..Default::default()
            },
            reasoning: Some(ThinkingLevel::Low),
            ..Default::default()
        }),
    )
    .await;

    assert_eq!(payload["temperature"], 0.6);
    assert_eq!(payload["top_p"], 0.5);
}

#[tokio::test]
async fn applies_thinking_level_params_between_model_and_request_params_across_openai_apis() {
    let mut by_level: SamplingParamsByThinkingLevel = IndexMap::new();
    by_level.insert(ModelThinkingLevel::Low, obj(json!({ "temperature": 0.6, "top_k": 64 })));

    for api in ["openai-completions", "openai-responses", "azure-openai-responses"] {
        let mut model = make_model(api, Some(obj(json!({ "temperature": 1, "top_p": 0.95 }))));
        model.reasoning = true;
        model.sampling_params_by_thinking_level = Some(by_level.clone());

        let opts = ApiStreamOptions {
            base: StreamOptions {
                sampling_params: Some(obj(json!({ "top_p": 0.5 }))),
                ..Default::default()
            },
            ..Default::default()
        }
        .with_extra("reasoningEffort", json!("low"));

        let payload = capture_api_payload(&model, opts).await;
        assert_eq!(payload["temperature"], 0.6, "{api}");
        assert_eq!(payload["top_p"], 0.5, "{api}");
        assert_eq!(payload["top_k"], 64, "{api}");
    }
}

#[tokio::test]
async fn uses_medium_sampling_params_for_summary_only_responses_requests() {
    let mut by_level: SamplingParamsByThinkingLevel = IndexMap::new();
    by_level.insert(ModelThinkingLevel::Off, obj(json!({ "temperature": 0.7 })));
    by_level.insert(ModelThinkingLevel::Medium, obj(json!({ "temperature": 0.8 })));

    for api in ["openai-responses", "azure-openai-responses"] {
        let mut model = make_model(api, None);
        model.reasoning = true;
        model.sampling_params_by_thinking_level = Some(by_level.clone());

        let opts = ApiStreamOptions::default().with_extra("reasoningSummary", json!("auto"));
        let payload = capture_api_payload(&model, opts).await;
        assert_eq!(payload["reasoning"]["effort"], "medium", "{api}");
        assert_eq!(payload["temperature"], 0.8, "{api}");
    }
}
