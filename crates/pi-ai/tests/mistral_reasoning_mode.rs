//! Port of packages/ai/test/mistral-reasoning-mode.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::api::mistral_conversations::stream_simple;
use pi_ai::utils::transcript::normalize_context;
use pi_ai::{
    CacheRetention, Context, FetchFunction, Message, Model, ModelCost, ModelInput, ModelThinkingLevel, OnPayloadFn,
    SimpleStreamOptions, StreamOptions, ThinkingLevel, ThinkingLevelMap, UserMessage,
};
use pi_js::{BoxFuture, Error};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

fn level_map(entries: &[(ModelThinkingLevel, Option<&str>)]) -> ThinkingLevelMap {
    let mut map = ThinkingLevelMap::new();
    for (k, v) in entries {
        map.insert(*k, v.map(str::to_owned));
    }
    map
}

fn none_high_levels() -> ThinkingLevelMap {
    level_map(&[
        (ModelThinkingLevel::Off, Some("none")),
        (ModelThinkingLevel::Minimal, None),
        (ModelThinkingLevel::Low, None),
        (ModelThinkingLevel::Medium, None),
        (ModelThinkingLevel::High, Some("high")),
        (ModelThinkingLevel::XHigh, None),
        (ModelThinkingLevel::Max, None),
    ])
}

fn glm_5_2_levels() -> ThinkingLevelMap {
    let mut map = none_high_levels();
    map.insert(ModelThinkingLevel::Max, Some("max".to_owned()));
    map
}

fn glm_5_3_levels() -> ThinkingLevelMap {
    let mut map = none_high_levels();
    map.insert(ModelThinkingLevel::Off, None);
    map.insert(ModelThinkingLevel::Low, Some("low".to_owned()));
    map.insert(ModelThinkingLevel::Max, Some("max".to_owned()));
    map
}

fn make_model(id: &str, reasoning: bool, thinking_level_map: Option<ThinkingLevelMap>) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: id.to_owned(),
        api: "mistral-conversations".to_owned(),
        provider: "mistral".to_owned(),
        base_url: "http://127.0.0.1:9".to_owned(),
        reasoning,
        thinking_level_map,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 16_384,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn make_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: "Hello".into(),
            timestamp: pi_js::time::now_ms(),
        })],
        tools: None,
    }
}

async fn capture_payload(model: &Model, mut options: SimpleStreamOptions) -> Value {
    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&captured);
    let on_payload: OnPayloadFn<Model> = Arc::new(move |payload, _model| {
        let s = Arc::clone(&slot);
        Box::pin(async move {
            *s.lock().unwrap_or_else(|p| p.into_inner()) = Some(payload.clone());
            Ok(Some(payload))
        }) as BoxFuture<pi_js::Result<Option<Value>>>
    });

    // PORT: mocks immediate connection refusal instead of opening a socket to 127.0.0.1:9.
    let fail_fetch: FetchFunction =
        Arc::new(|_req| Box::pin(async { Err(Error::msg("connect ECONNREFUSED 127.0.0.1:9")) }));

    options.base.api_key = Some("fake-key".to_owned());
    options.base.on_payload = Some(on_payload);
    options.base.fetch = Some(fail_fetch);

    let _ = stream_simple(model, &normalize_context(&make_context()), Some(options))
        .result()
        .await;

    captured
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .expect("Expected payload to be captured before request failure")
}

#[tokio::test]
async fn uses_prompt_mode_for_magistral_and_omits_when_off() {
    let magistral = make_model("magistral-medium-latest", true, None);
    let enabled = capture_payload(
        &magistral,
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::Medium),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(enabled["promptMode"], json!("reasoning"));
    assert_eq!(enabled.get("reasoningEffort"), None);

    let off = capture_payload(&magistral, SimpleStreamOptions::default()).await;
    assert_eq!(off.get("promptMode"), None);
    assert_eq!(off.get("reasoningEffort"), None);
}

#[tokio::test]
async fn maps_reasoning_effort_for_models_with_thinking_level_map() {
    for (model_id, map) in [
        ("mistral-small-2603", none_high_levels()),
        ("mistral-medium-latest", none_high_levels()),
        ("zai-glm-5-2", glm_5_2_levels()),
    ] {
        let model = make_model(model_id, true, Some(map));

        let high_payload = capture_payload(
            &model,
            SimpleStreamOptions {
                reasoning: Some(ThinkingLevel::High),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(high_payload["reasoningEffort"], json!("high"));
        assert_eq!(high_payload.get("promptMode"), None);

        let clamped_payload = capture_payload(
            &model,
            SimpleStreamOptions {
                reasoning: Some(ThinkingLevel::Low),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(clamped_payload["reasoningEffort"], json!("high"));

        let off_payload = capture_payload(&model, SimpleStreamOptions::default()).await;
        assert_eq!(off_payload["reasoningEffort"], json!("none"));
        assert_eq!(off_payload.get("promptMode"), None);
    }
}

#[tokio::test]
async fn sends_max_for_glm_5_2_and_levels_for_glm_5_3() {
    let glm_5_2 = make_model("zai-glm-5-2", true, Some(glm_5_2_levels()));
    let max_payload = capture_payload(
        &glm_5_2,
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::Max),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(max_payload["reasoningEffort"], json!("max"));

    let glm_5_3 = make_model("zai-glm-5-3", true, Some(glm_5_3_levels()));
    for (level, expected) in [
        (ThinkingLevel::Low, "low"),
        (ThinkingLevel::High, "high"),
        (ThinkingLevel::Max, "max"),
    ] {
        let payload = capture_payload(
            &glm_5_3,
            SimpleStreamOptions {
                reasoning: Some(level),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(payload["reasoningEffort"], json!(expected));
        assert_eq!(payload.get("promptMode"), None);
    }

    let medium_clamped = capture_payload(
        &glm_5_3,
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::Medium),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(medium_clamped["reasoningEffort"], json!("high"));
}

#[tokio::test]
async fn omits_reasoning_for_non_reasoning_models_and_handles_prompt_cache_key() {
    let non_reasoning = make_model("mistral-medium-2505", false, None);
    let nr_payload = capture_payload(
        &non_reasoning,
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::Medium),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(nr_payload.get("reasoningEffort"), None);
    assert_eq!(nr_payload.get("promptMode"), None);

    let cached_model = make_model("mistral-large-latest", false, None);
    let cached_payload = capture_payload(
        &cached_model,
        SimpleStreamOptions {
            base: StreamOptions {
                session_id: Some("session-123".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(cached_payload["promptCacheKey"], json!("session-123"));

    let uncached_payload = capture_payload(
        &cached_model,
        SimpleStreamOptions {
            base: StreamOptions {
                session_id: Some("session-123".to_owned()),
                cache_retention: Some(CacheRetention::None),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(uncached_payload.get("promptCacheKey"), None);
}
