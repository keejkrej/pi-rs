//! Port of packages/ai/test/google-thinking-level-map.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::api::google_generative_ai::stream_simple as stream_simple_google;
use pi_ai::api::google_shared::{ResolvedGoogleThinkingLevel, resolve_google_thinking_level};
use pi_ai::api::google_vertex::stream_simple as stream_simple_vertex;
use pi_ai::utils::transcript::normalize_context;
use pi_ai::{
    Context, Message, Model, ModelCost, ModelInput, ModelThinkingLevel, OnPayloadFn, SimpleStreamOptions,
    StreamOptions, ThinkingBudgets, ThinkingLevel, ThinkingLevelMap, UserMessage,
};
use pi_js::{BoxFuture, Error};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

fn sample_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: "Hello".into(),
            timestamp: 0,
        })],
        tools: None,
    }
}

fn google_model(id: &str, thinking_level_map: ThinkingLevelMap) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: id.to_owned(),
        api: "google-generative-ai".to_owned(),
        provider: "test-google".to_owned(),
        base_url: "https://example.invalid/v1beta".to_owned(),
        reasoning: true,
        thinking_level_map: Some(thinking_level_map),
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 4_096,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn vertex_model(id: &str, thinking_level_map: ThinkingLevelMap) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: id.to_owned(),
        api: "google-vertex".to_owned(),
        provider: "test-vertex".to_owned(),
        base_url: "https://example.invalid/v1".to_owned(),
        reasoning: true,
        thinking_level_map: Some(thinking_level_map),
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 4_096,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn level_map(entries: &[(ModelThinkingLevel, Option<&str>)]) -> ThinkingLevelMap {
    let mut map = ThinkingLevelMap::new();
    for (k, v) in entries {
        map.insert(*k, v.map(str::to_owned));
    }
    map
}

fn capture_payload_cb(slot: Arc<Mutex<Option<Value>>>) -> OnPayloadFn<Model> {
    Arc::new(move |request, _model| {
        let s = Arc::clone(&slot);
        Box::pin(async move {
            *s.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(request);
            Err(Error::msg("payload captured"))
        }) as BoxFuture<pi_js::Result<Option<Value>>>
    })
}

async fn capture_google_payload(
    model: &Model,
    reasoning: Option<ThinkingLevel>,
    thinking_budgets: Option<ThinkingBudgets>,
) -> Value {
    let payload_slot = Arc::new(Mutex::new(None));
    let opts = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some("test".to_owned()),
            on_payload: Some(capture_payload_cb(Arc::clone(&payload_slot))),
            ..Default::default()
        },
        tool_choice: None,
        reasoning,
        deferred: None,
        thinking_budgets,
    };

    let result = stream_simple_google(model, &normalize_context(&sample_context()), Some(opts))
        .result()
        .await;
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|m| m.contains("payload captured")),
        "unexpected error_message: {:?}",
        result.error_message
    );

    let guard = payload_slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.as_ref().cloned().expect("Google payload was not captured")
}

async fn capture_vertex_payload(
    model: &Model,
    reasoning: Option<ThinkingLevel>,
    thinking_budgets: Option<ThinkingBudgets>,
) -> Value {
    let payload_slot = Arc::new(Mutex::new(None));
    let opts = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some("test".to_owned()),
            on_payload: Some(capture_payload_cb(Arc::clone(&payload_slot))),
            ..Default::default()
        },
        tool_choice: None,
        reasoning,
        deferred: None,
        thinking_budgets,
    };

    let result = stream_simple_vertex(model, &normalize_context(&sample_context()), Some(opts))
        .result()
        .await;
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|m| m.contains("payload captured")),
        "unexpected error_message: {:?}",
        result.error_message
    );

    let guard = payload_slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.as_ref().cloned().expect("Vertex payload was not captured")
}

#[test]
fn exhaustively_resolves_supported_logical_levels_and_mapping_values() {
    let default_expectations = [
        (ThinkingLevel::Minimal, ResolvedGoogleThinkingLevel::Minimal),
        (ThinkingLevel::Low, ResolvedGoogleThinkingLevel::Low),
        (ThinkingLevel::Medium, ResolvedGoogleThinkingLevel::Medium),
        (ThinkingLevel::High, ResolvedGoogleThinkingLevel::High),
    ];
    for (level, expected) in default_expectations {
        let model = google_model("gemini-3.7-flash", ThinkingLevelMap::new());
        assert_eq!(
            resolve_google_thinking_level(&model, level).expect("default level should resolve"),
            expected
        );
    }

    let mapped_expectations = [
        ("minimal", ResolvedGoogleThinkingLevel::Minimal),
        ("low", ResolvedGoogleThinkingLevel::Low),
        ("medium", ResolvedGoogleThinkingLevel::Medium),
        ("high", ResolvedGoogleThinkingLevel::High),
        ("MINIMAL", ResolvedGoogleThinkingLevel::Minimal),
        ("LOW", ResolvedGoogleThinkingLevel::Low),
        ("MEDIUM", ResolvedGoogleThinkingLevel::Medium),
        ("HIGH", ResolvedGoogleThinkingLevel::High),
    ];
    for (mapped, expected) in mapped_expectations {
        let model = google_model(
            "gemini-3.7-flash",
            level_map(&[
                (ModelThinkingLevel::High, Some(mapped)),
                (ModelThinkingLevel::XHigh, Some(mapped)),
                (ModelThinkingLevel::Max, Some(mapped)),
            ]),
        );
        assert_eq!(
            resolve_google_thinking_level(&model, ThinkingLevel::High).expect("high"),
            expected
        );
        assert_eq!(
            resolve_google_thinking_level(&model, ThinkingLevel::XHigh).expect("xhigh"),
            expected
        );
        assert_eq!(
            resolve_google_thinking_level(&model, ThinkingLevel::Max).expect("max"),
            expected
        );
    }

    let invalid_model = google_model(
        "gemini-3.7-flash",
        level_map(&[(ModelThinkingLevel::XHigh, Some("extreme"))]),
    );
    let err =
        resolve_google_thinking_level(&invalid_model, ThinkingLevel::XHigh).expect_err("extreme mapping must fail");
    assert_eq!(
        err.to_string(),
        "Unsupported Google thinking level mapping for test-google/gemini-3.7-flash: xhigh -> extreme"
    );

    let unmapped_err = resolve_google_thinking_level(
        &google_model("gemini-3.7-flash", ThinkingLevelMap::new()),
        ThinkingLevel::Max,
    )
    .expect_err("unmapped max must fail");
    assert_eq!(
        unmapped_err.to_string(),
        "Unsupported Google thinking level mapping for test-google/gemini-3.7-flash: max -> undefined"
    );
}

#[tokio::test]
async fn uses_lowest_supported_level_when_reasoning_is_omitted() {
    let map = level_map(&[
        (ModelThinkingLevel::Off, None),
        (ModelThinkingLevel::Minimal, None),
        (ModelThinkingLevel::Low, Some("low")),
        (ModelThinkingLevel::Medium, Some("medium")),
        (ModelThinkingLevel::High, Some("high")),
        (ModelThinkingLevel::XHigh, None),
        (ModelThinkingLevel::Max, None),
    ]);

    let google_payload = capture_google_payload(&google_model("gemini-3.8-flash", map.clone()), None, None).await;
    assert_eq!(
        google_payload["config"]["thinkingConfig"],
        json!({ "thinkingLevel": "LOW" })
    );

    let vertex_payload = capture_vertex_payload(&vertex_model("gemini-3.8-flash", map), None, None).await;
    assert_eq!(
        vertex_payload["config"]["thinkingConfig"],
        json!({ "thinkingLevel": "LOW" })
    );
}

#[tokio::test]
async fn preserves_native_medium_effort_for_gemini_3_1_pro() {
    let map = level_map(&[
        (ModelThinkingLevel::Off, None),
        (ModelThinkingLevel::Minimal, None),
        (ModelThinkingLevel::Low, Some("low")),
        (ModelThinkingLevel::Medium, Some("medium")),
        (ModelThinkingLevel::High, Some("high")),
        (ModelThinkingLevel::XHigh, None),
        (ModelThinkingLevel::Max, None),
    ]);

    let google_payload = capture_google_payload(
        &google_model("gemini-3.1-pro-preview", map.clone()),
        Some(ThinkingLevel::Medium),
        None,
    )
    .await;
    assert_eq!(
        google_payload["config"]["thinkingConfig"],
        json!({ "includeThoughts": true, "thinkingLevel": "MEDIUM" })
    );

    let vertex_payload = capture_vertex_payload(
        &vertex_model("gemini-3.1-pro-preview", map),
        Some(ThinkingLevel::Medium),
        None,
    )
    .await;
    assert_eq!(
        vertex_payload["config"]["thinkingConfig"],
        json!({ "includeThoughts": true, "thinkingLevel": "MEDIUM" })
    );
}

#[tokio::test]
async fn disables_gemini_2_5_thinking_when_reasoning_is_omitted() {
    let g_payload =
        capture_google_payload(&google_model("gemini-2.5-flash", ThinkingLevelMap::new()), None, None).await;
    assert_eq!(g_payload["config"]["thinkingConfig"], json!({ "thinkingBudget": 0 }));

    let v_payload =
        capture_vertex_payload(&vertex_model("gemini-2.5-flash", ThinkingLevelMap::new()), None, None).await;
    assert_eq!(v_payload["config"]["thinkingConfig"], json!({ "thinkingBudget": 0 }));
}

// Regression test for https://github.com/earendil-works/pi/issues/10890
#[tokio::test]
async fn uses_thinking_levels_for_gemini_4_models() {
    let g_payload = capture_google_payload(
        &google_model("gemini-4-argon", ThinkingLevelMap::new()),
        Some(ThinkingLevel::Medium),
        None,
    )
    .await;
    assert_eq!(
        g_payload["config"]["thinkingConfig"],
        json!({ "includeThoughts": true, "thinkingLevel": "MEDIUM" })
    );

    let v_payload = capture_vertex_payload(
        &vertex_model("gemini-4-argon", ThinkingLevelMap::new()),
        Some(ThinkingLevel::Medium),
        None,
    )
    .await;
    assert_eq!(
        v_payload["config"]["thinkingConfig"],
        json!({ "includeThoughts": true, "thinkingLevel": "MEDIUM" })
    );
}

#[tokio::test]
async fn maps_google_generative_ai_xhigh_and_max_to_supported_level() {
    let map = level_map(&[
        (ModelThinkingLevel::XHigh, Some("high")),
        (ModelThinkingLevel::Max, Some("high")),
    ]);
    let model = google_model("gemini-3.7-flash", map);

    for level in [ThinkingLevel::XHigh, ThinkingLevel::Max] {
        let payload = capture_google_payload(&model, Some(level), None).await;
        assert_eq!(
            payload["config"]["thinkingConfig"],
            json!({ "includeThoughts": true, "thinkingLevel": "HIGH" })
        );
    }
}

#[tokio::test]
async fn honors_uppercase_provider_values_for_standard_google_levels() {
    let model = google_model(
        "gemini-3.7-flash",
        level_map(&[(ModelThinkingLevel::High, Some("LOW"))]),
    );
    let payload = capture_google_payload(&model, Some(ThinkingLevel::High), None).await;
    assert_eq!(payload["config"]["thinkingConfig"]["thinkingLevel"], json!("LOW"));
}

#[tokio::test]
async fn uses_mapped_google_levels_for_token_budgets() {
    let model = google_model(
        "gemini-2.5-flash",
        level_map(&[(ModelThinkingLevel::XHigh, Some("high"))]),
    );
    let payload = capture_google_payload(
        &model,
        Some(ThinkingLevel::XHigh),
        Some(ThinkingBudgets {
            high: Some(1234),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(payload["config"]["thinkingConfig"]["thinkingBudget"], json!(1234));
}

#[tokio::test]
async fn maps_google_vertex_extended_levels_and_token_budgets() {
    let ext_model = vertex_model(
        "gemini-3.7-flash",
        level_map(&[(ModelThinkingLevel::XHigh, Some("high"))]),
    );
    let ext_payload = capture_vertex_payload(&ext_model, Some(ThinkingLevel::XHigh), None).await;
    assert_eq!(
        ext_payload["config"]["thinkingConfig"],
        json!({ "includeThoughts": true, "thinkingLevel": "HIGH" })
    );

    let budget_model = vertex_model(
        "gemini-2.5-flash",
        level_map(&[(ModelThinkingLevel::Max, Some("high"))]),
    );
    let budget_payload = capture_vertex_payload(
        &budget_model,
        Some(ThinkingLevel::Max),
        Some(ThinkingBudgets {
            high: Some(4321),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(
        budget_payload["config"]["thinkingConfig"]["thinkingBudget"],
        json!(4321)
    );
}
