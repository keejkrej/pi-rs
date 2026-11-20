//! Port of packages/ai/test/fireworks-models.test.ts (unit ai-providers-builtin)

// @generated-mods begin (scaffold-owned, do not edit)
#[path = "fireworks_models/part_2.rs"]
mod part_2;
// @generated-mods end

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::json;

use pi_ai::compat::{get_model, stream_simple};
use pi_ai::env_api_keys::{find_env_keys, get_env_api_key};
use pi_ai::models::get_supported_thinking_levels;
use pi_ai::types::{
    AnthropicMessagesCompat, CacheRetention, Context, Message, Model, ModelCompat, ModelCost, ModelInput,
    ModelThinkingLevel, SimpleStreamOptions, StreamOptions, ThinkingFormat, ThinkingLevel, UserMessage,
    UserMessageContent,
};
use pi_js::Error;
use pi_js::env::testing::EnvGuard;

fn user_context(text: &str) -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text(text.to_owned()),
            timestamp: 0,
        })],
        tools: None,
    }
}

#[test]
fn registers_non_glm_non_kimi_k3_models_via_anthropic_messages() {
    let model = get_model("fireworks", "accounts/fireworks/models/deepseek-v4p1-flash").expect("deepseek-v4p1-flash");
    assert_eq!(model.api, "anthropic-messages");
    assert_eq!(model.provider, "fireworks");
    assert_eq!(model.base_url, "https://api.fireworks.ai/inference");
    assert!(model.reasoning);
    assert_eq!(model.input, vec![ModelInput::Text, ModelInput::Image]);
}

#[test]
fn aligns_glm_5_3_fast_with_glm_5_3_openai_compatible_config() {
    let base = get_model("fireworks", "accounts/fireworks/models/glm-5p3").expect("glm-5p3");
    let fast = get_model("fireworks", "accounts/fireworks/routers/glm-5p3-fast").expect("glm-5p3-fast");
    assert_eq!(fast.api, base.api);
    assert_eq!(fast.base_url, base.base_url);
    assert_eq!(fast.compat, base.compat);
    assert_eq!(fast.thinking_level_map, base.thinking_level_map);
}

#[tokio::test]
async fn omits_unsupported_long_cache_retention_for_glm_5_3_models() {
    for model_id in [
        "accounts/fireworks/models/glm-5p3",
        "accounts/fireworks/routers/glm-5p3-fast",
    ] {
        let model = get_model("fireworks", model_id).expect(model_id);
        let captured = Arc::new(Mutex::new(None));
        let captured_ref = Arc::clone(&captured);

        let mut base = StreamOptions::default();
        base.api_key = Some("test-fireworks-key".to_owned());
        base.cache_retention = Some(CacheRetention::Long);
        base.session_id = Some("test-fireworks-session".to_owned());
        base.on_payload = Some(Arc::new(move |value, _| {
            let mut guard = captured_ref.lock().unwrap();
            *guard = Some(value);
            Box::pin(async { Err(Error::msg("payload captured")) })
        }));
        let opts = SimpleStreamOptions {
            base,
            ..Default::default()
        };

        let _ = stream_simple(&model, &user_context("test"), Some(opts)).result().await;
        let payload = captured.lock().unwrap().clone().expect("captured payload");
        assert!(payload.get("prompt_cache_retention").is_none());
    }
}

#[tokio::test]
async fn routes_kimi_k3_through_openai_compatible_api_with_native_effort() {
    let base = get_model("fireworks", "accounts/fireworks/models/kimi-k3").expect("kimi-k3");
    let fast = get_model("fireworks", "accounts/fireworks/routers/kimi-k3-fast").expect("kimi-k3-fast");

    let mut expected_map = IndexMap::new();
    expected_map.insert(ModelThinkingLevel::Off, None);
    expected_map.insert(ModelThinkingLevel::Minimal, None);
    expected_map.insert(ModelThinkingLevel::Low, Some("low".to_owned()));
    expected_map.insert(ModelThinkingLevel::Medium, None);
    expected_map.insert(ModelThinkingLevel::High, Some("high".to_owned()));
    expected_map.insert(ModelThinkingLevel::XHigh, None);
    expected_map.insert(ModelThinkingLevel::Max, Some("max".to_owned()));

    assert_eq!(base.api, "openai-completions");
    assert_eq!(base.base_url, "https://api.fireworks.ai/inference/v1");
    assert_eq!(base.thinking_level_map.as_ref(), Some(&expected_map));

    let compat = base
        .compat
        .as_ref()
        .and_then(ModelCompat::as_openai_completions)
        .expect("openai-completions compat");
    assert_eq!(compat.supports_store, Some(false));
    assert_eq!(compat.supports_developer_role, Some(false));
    assert_eq!(compat.supports_strict_mode, Some(true));
    assert_eq!(compat.requires_reasoning_content_on_assistant_messages, Some(true));
    assert_eq!(compat.thinking_format, Some(ThinkingFormat::OpenAI));
    assert_eq!(compat.supports_mid_convo_system_messages, Some(true));
    assert_eq!(compat.supports_mid_convo_tool_additions, Some(true));
    assert_eq!(compat.send_session_affinity_headers, Some(true));
    assert_eq!(compat.supports_long_cache_retention, Some(false));

    assert_eq!(fast.api, base.api);
    assert_eq!(fast.base_url, base.base_url);
    assert_eq!(fast.compat, base.compat);
    assert_eq!(fast.thinking_level_map, base.thinking_level_map);

    let captured = Arc::new(Mutex::new(None));
    let captured_ref = Arc::clone(&captured);
    let mut stream_base = StreamOptions::default();
    stream_base.api_key = Some("test-fireworks-key".to_owned());
    stream_base.on_payload = Some(Arc::new(move |value, _| {
        let mut guard = captured_ref.lock().unwrap();
        *guard = Some(value);
        Box::pin(async { Err(Error::msg("payload captured")) })
    }));
    let opts = SimpleStreamOptions {
        base: stream_base,
        reasoning: Some(ThinkingLevel::Max),
        ..Default::default()
    };

    let _ = stream_simple(&base, &user_context("test"), Some(opts)).result().await;
    let payload = captured.lock().unwrap().clone().expect("captured payload");
    assert_eq!(payload.get("reasoning_effort"), Some(&json!("max")));
}

#[tokio::test]
async fn sends_native_messages_effort_levels() {
    let cases: &[(&str, &[ModelThinkingLevel])] = &[
        (
            "accounts/fireworks/models/deepseek-v4p1-flash",
            &[
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Max,
            ],
        ),
        (
            "accounts/fireworks/models/qwen3p8-max",
            &[
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::XHigh,
            ],
        ),
        (
            "accounts/fireworks/models/qwen3p8-2p4t-a95b",
            &[
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::XHigh,
            ],
        ),
    ];

    for &(model_id, levels) in cases {
        let model = get_model("fireworks", model_id).expect(model_id);
        assert_eq!(model.api, "anthropic-messages");
        let compat = model
            .compat
            .as_ref()
            .and_then(ModelCompat::as_anthropic_messages)
            .expect("anthropic compat");
        assert_eq!(compat.force_adaptive_thinking, Some(true));
        assert_eq!(get_supported_thinking_levels(&model), levels);

        for &level in levels {
            let captured = Arc::new(Mutex::new(None));
            let captured_ref = Arc::clone(&captured);
            let mut base = StreamOptions::default();
            base.api_key = Some("test-fireworks-key".to_owned());
            base.on_payload = Some(Arc::new(move |value, _| {
                let mut guard = captured_ref.lock().unwrap();
                *guard = Some(value);
                Box::pin(async { Err(Error::msg("payload captured")) })
            }));
            let opts = SimpleStreamOptions {
                base,
                reasoning: level.as_thinking_level(),
                ..Default::default()
            };

            let _ = stream_simple(&model, &user_context("test"), Some(opts)).result().await;
            let payload = captured.lock().unwrap().clone().expect("captured payload");
            if level == ModelThinkingLevel::Off {
                assert_eq!(payload.get("thinking"), Some(&json!({ "type": "disabled" })));
                assert!(payload.get("output_config").is_none());
            } else {
                assert_eq!(
                    payload.get("thinking"),
                    Some(&json!({ "type": "adaptive", "display": "summarized" }))
                );
                assert_eq!(payload.get("output_config"), Some(&json!({ "effort": level.as_str() })));
            }
        }
    }
}

#[test]
fn exposes_distinct_native_effort_levels_for_glm_and_kimi() {
    let expected = vec![
        ModelThinkingLevel::Low,
        ModelThinkingLevel::High,
        ModelThinkingLevel::Max,
    ];
    for model_id in [
        "accounts/fireworks/models/glm-5p3",
        "accounts/fireworks/routers/glm-5p3-fast",
        "accounts/fireworks/models/kimi-k3",
        "accounts/fireworks/routers/kimi-k3-fast",
    ] {
        let model = get_model("fireworks", model_id).expect(model_id);
        assert_eq!(get_supported_thinking_levels(&model), expected);
    }
}

#[tokio::test]
async fn keeps_non_adaptive_messages_models_on_budget_based_thinking() {
    // Use a fixed fixture: the live catalog no longer lists toggle-only Messages models.
    let model = Model {
        r#type: None,
        id: "accounts/fireworks/models/test-toggle-reasoner".to_owned(),
        name: "Test Toggle Reasoner".to_owned(),
        api: "anthropic-messages".to_owned(),
        provider: "fireworks".to_owned(),
        base_url: "https://api.fireworks.ai/inference".to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 131_072,
        max_tokens: 131_072,
        headers: None,
        sampling_params: None,
        compat: Some(ModelCompat::AnthropicMessages(AnthropicMessagesCompat {
            allow_empty_signature: Some(true),
            send_session_affinity_headers: Some(true),
            supports_eager_tool_input_streaming: Some(false),
            supports_cache_control_on_tools: Some(false),
            supports_long_cache_retention: Some(false),
            ..Default::default()
        })),
        sampling_params_by_thinking_level: None,
    };
    let compat = model
        .compat
        .as_ref()
        .and_then(ModelCompat::as_anthropic_messages)
        .expect("anthropic compat");
    assert_eq!(compat.force_adaptive_thinking, None);

    let captured = Arc::new(Mutex::new(None));
    let captured_ref = Arc::clone(&captured);
    let mut base = StreamOptions::default();
    base.api_key = Some("test-fireworks-key".to_owned());
    base.on_payload = Some(Arc::new(move |value, _| {
        let mut guard = captured_ref.lock().unwrap();
        *guard = Some(value);
        Box::pin(async { Err(Error::msg("payload captured")) })
    }));
    let opts = SimpleStreamOptions {
        base,
        reasoning: Some(ThinkingLevel::High),
        ..Default::default()
    };

    let _ = stream_simple(&model, &user_context("test"), Some(opts)).result().await;
    let payload = captured.lock().unwrap().clone().expect("captured payload");
    assert_eq!(
        payload.get("thinking"),
        Some(&json!({ "type": "enabled", "budget_tokens": 16384, "display": "summarized" }))
    );
    assert!(payload.get("output_config").is_none());
}

#[test]
fn resolves_fireworks_api_key_from_environment() {
    let _guard = EnvGuard::set("FIREWORKS_API_KEY", Some("test-fireworks-key"));
    assert_eq!(find_env_keys("fireworks", None), Some(vec!["FIREWORKS_API_KEY"]));
    assert_eq!(
        get_env_api_key("fireworks", None),
        Some("test-fireworks-key".to_owned())
    );
}

#[test]
fn sets_fireworks_specific_compat_for_session_affinity_and_tool_fields() {
    let model = get_model("fireworks", "accounts/fireworks/models/deepseek-v4p1-flash").expect("deepseek-v4p1-flash");
    let compat = model
        .compat
        .as_ref()
        .and_then(ModelCompat::as_anthropic_messages)
        .expect("anthropic compat");
    assert_eq!(compat.send_session_affinity_headers, Some(true));
    assert_eq!(compat.supports_eager_tool_input_streaming, Some(false));
    assert_eq!(compat.supports_cache_control_on_tools, Some(false));
    assert_eq!(compat.supports_long_cache_retention, Some(false));
    assert_eq!(compat.allow_empty_signature, Some(true));
}
