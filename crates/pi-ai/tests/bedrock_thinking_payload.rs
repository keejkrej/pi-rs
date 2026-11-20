//! Port of packages/ai/test/bedrock-thinking-payload.test.ts

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use pi_ai::api::bedrock_converse_stream::{BedrockOptions, stream_with_options};
use pi_ai::providers::amazon_bedrock_models::amazon_bedrock_models;
use pi_ai::types::{
    AssistantMessageEvent, Context, JsonValue, Message, Model, StopReason, StreamOptions, ThinkingLevel, UserMessage,
    UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;
use serde_json::json;

const THINKING_BINDING_CONTROLS_BETA: &str = "thinking-binding-controls-2026-08-01";

fn adaptive_with_binding() -> JsonValue {
    json!({
        "type": "adaptive",
        "display": "summarized",
        "block_binding": { "prefix_mismatch_behavior": "drop_block" }
    })
}

fn has_bedrock_credentials() -> bool {
    let env_non_empty = |name: &str| pi_js::env::var(name).filter(|v| !v.is_empty()).is_some();
    env_non_empty("AWS_PROFILE")
        || (env_non_empty("AWS_ACCESS_KEY_ID") && env_non_empty("AWS_SECRET_ACCESS_KEY"))
        || env_non_empty("AWS_BEARER_TOKEN_BEDROCK")
}

fn make_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Hello".to_string()),
            timestamp: pi_js::time::now_ms(),
        })],
        tools: None,
    }
}

async fn capture_payload(model: &Model, options: Option<BedrockOptions>) -> JsonValue {
    let captured: Arc<Mutex<Option<JsonValue>>> = Arc::new(Mutex::new(None));
    let captured_ref = Arc::clone(&captured);

    let mut opts = options.unwrap_or_default();
    if opts.reasoning.is_none() {
        opts.reasoning = Some(ThinkingLevel::High);
    }
    opts.base.on_payload = Some(Arc::new(move |payload, _model| {
        let captured_inner = Arc::clone(&captured_ref);
        Box::pin(async move {
            *captured_inner.lock().unwrap() = Some(payload);
            Err(pi_js::Error::js("PayloadCaptured", "payload captured"))
        })
    }));

    let context = normalize_context(&make_context());
    let mut s = stream_with_options(model, &context, Some(opts));
    while let Some(event) = s.next().await {
        if matches!(event, AssistantMessageEvent::Error { .. }) {
            break;
        }
    }

    captured
        .lock()
        .unwrap()
        .clone()
        .expect("Expected Bedrock payload to be captured before request abort")
}

#[tokio::test]
async fn uses_adaptive_thinking_for_claude_opus_4_8_when_reasoning_enabled() {
    let mut model = amazon_bedrock_models()
        .get("global.anthropic.claude-opus-4-6-v1")
        .expect("claude-opus-4-6-v1")
        .clone();
    model.id = "global.anthropic.claude-opus-4-8-v1".to_string();
    model.name = "Claude Opus 4.8 (Global)".to_string();

    let payload = capture_payload(&model, None).await;
    assert_eq!(
        payload["additionalModelRequestFields"]["thinking"],
        adaptive_with_binding()
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["output_config"],
        json!({ "effort": "high" })
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["anthropic_beta"],
        json!([THINKING_BINDING_CONTROLS_BETA])
    );
}

#[tokio::test]
async fn maps_xhigh_reasoning_to_effort_xhigh_for_claude_opus_4_8() {
    let mut model = amazon_bedrock_models()
        .get("global.anthropic.claude-opus-4-6-v1")
        .expect("claude-opus-4-6-v1")
        .clone();
    model.id = "global.anthropic.claude-opus-4-8-v1".to_string();
    model.name = "Claude Opus 4.8 (Global)".to_string();

    let payload = capture_payload(
        &model,
        Some(BedrockOptions {
            reasoning: Some(ThinkingLevel::XHigh),
            ..BedrockOptions::default()
        }),
    )
    .await;
    assert_eq!(
        payload["additionalModelRequestFields"]["thinking"],
        adaptive_with_binding()
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["output_config"],
        json!({ "effort": "xhigh" })
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["anthropic_beta"],
        json!([THINKING_BINDING_CONTROLS_BETA])
    );
}

#[tokio::test]
async fn uses_adaptive_thinking_for_claude_fable_5_when_reasoning_enabled() {
    let model = amazon_bedrock_models()
        .get("global.anthropic.claude-fable-5")
        .expect("global.anthropic.claude-fable-5");

    let payload = capture_payload(model, None).await;
    assert_eq!(
        payload["additionalModelRequestFields"]["thinking"],
        adaptive_with_binding()
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["output_config"],
        json!({ "effort": "high" })
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["anthropic_beta"],
        json!([THINKING_BINDING_CONTROLS_BETA])
    );
}

#[tokio::test]
async fn uses_adaptive_thinking_for_claude_sonnet_5_when_reasoning_enabled() {
    let model = amazon_bedrock_models()
        .get("global.anthropic.claude-sonnet-5")
        .expect("global.anthropic.claude-sonnet-5");

    let payload = capture_payload(model, None).await;
    assert_eq!(
        payload["additionalModelRequestFields"]["thinking"],
        adaptive_with_binding()
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["output_config"],
        json!({ "effort": "high" })
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["anthropic_beta"],
        json!([THINKING_BINDING_CONTROLS_BETA])
    );
}

#[tokio::test]
async fn uses_adaptive_thinking_for_claude_opus_5_when_reasoning_enabled() {
    let model = amazon_bedrock_models()
        .get("global.anthropic.claude-opus-5")
        .expect("global.anthropic.claude-opus-5");

    let payload = capture_payload(model, None).await;
    assert_eq!(
        payload["additionalModelRequestFields"]["thinking"],
        adaptive_with_binding()
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["output_config"],
        json!({ "effort": "high" })
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["anthropic_beta"],
        json!([THINKING_BINDING_CONTROLS_BETA])
    );
}

#[tokio::test]
async fn maps_xhigh_reasoning_to_effort_xhigh_for_claude_opus_5() {
    let model = amazon_bedrock_models()
        .get("global.anthropic.claude-opus-5")
        .expect("global.anthropic.claude-opus-5");

    let payload = capture_payload(
        model,
        Some(BedrockOptions {
            reasoning: Some(ThinkingLevel::XHigh),
            ..BedrockOptions::default()
        }),
    )
    .await;
    assert_eq!(
        payload["additionalModelRequestFields"]["thinking"],
        adaptive_with_binding()
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["output_config"],
        json!({ "effort": "xhigh" })
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["anthropic_beta"],
        json!([THINKING_BINDING_CONTROLS_BETA])
    );
}

#[tokio::test]
async fn maps_xhigh_reasoning_to_effort_xhigh_for_claude_fable_5() {
    let model = amazon_bedrock_models()
        .get("global.anthropic.claude-fable-5")
        .expect("global.anthropic.claude-fable-5");

    let payload = capture_payload(
        model,
        Some(BedrockOptions {
            reasoning: Some(ThinkingLevel::XHigh),
            ..BedrockOptions::default()
        }),
    )
    .await;
    assert_eq!(
        payload["additionalModelRequestFields"]["thinking"],
        adaptive_with_binding()
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["output_config"],
        json!({ "effort": "xhigh" })
    );
}

// https://github.com/earendil-works/pi/issues/10324
#[tokio::test]
async fn sends_block_binding_and_the_binding_beta_for_claude_opus_5_5() {
    let model = amazon_bedrock_models()
        .get("global.anthropic.claude-opus-5-5")
        .expect("global.anthropic.claude-opus-5-5");

    let payload = capture_payload(model, None).await;
    assert_eq!(
        payload["additionalModelRequestFields"]["thinking"],
        adaptive_with_binding()
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["anthropic_beta"],
        json!([THINKING_BINDING_CONTROLS_BETA])
    );
}

// Bedrock rejects block_binding on 4.6 models: "Extra inputs are not permitted" (#10324)
#[tokio::test]
async fn omits_block_binding_for_4_6_models() {
    for model_id in [
        "global.anthropic.claude-opus-4-6-v1",
        "global.anthropic.claude-sonnet-4-6",
    ] {
        let model = amazon_bedrock_models().get(model_id).expect(model_id);
        let payload = capture_payload(model, None).await;
        assert_eq!(
            payload["additionalModelRequestFields"]["thinking"],
            json!({ "type": "adaptive", "display": "summarized" })
        );
        assert!(payload["additionalModelRequestFields"].get("anthropic_beta").is_none());
    }
}

#[tokio::test]
async fn omits_display_for_govcloud_model_ids_on_non_adaptive_claude_thinking() {
    let mut model = amazon_bedrock_models()
        .get("us.anthropic.claude-sonnet-4-5-20250929-v1:0")
        .expect("claude-sonnet-4-5")
        .clone();
    model.id = "us-gov.anthropic.claude-sonnet-4-5-20250929-v1:0".to_string();
    model.name = "Claude Sonnet 4.5 (GovCloud)".to_string();

    let payload = capture_payload(&model, None).await;
    assert_eq!(
        payload["additionalModelRequestFields"]["thinking"],
        json!({ "type": "enabled", "budget_tokens": 16384 })
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["anthropic_beta"],
        json!(["interleaved-thinking-2025-05-14"])
    );
}

#[tokio::test]
async fn omits_display_for_govcloud_regions_on_adaptive_claude_thinking() {
    let mut model = amazon_bedrock_models()
        .get("global.anthropic.claude-opus-4-6-v1")
        .expect("claude-opus-4-6-v1")
        .clone();
    model.id = "global.anthropic.claude-opus-4-8-v1".to_string();
    model.name = "Claude Opus 4.8 (Global)".to_string();

    let payload = capture_payload(
        &model,
        Some(BedrockOptions {
            region: Some("us-gov-west-1".to_string()),
            ..BedrockOptions::default()
        }),
    )
    .await;
    assert_eq!(
        payload["additionalModelRequestFields"]["thinking"],
        json!({ "type": "adaptive" })
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["output_config"],
        json!({ "effort": "high" })
    );
    assert!(payload["additionalModelRequestFields"].get("anthropic_beta").is_none());
}

#[tokio::test]
async fn uses_model_max_tokens_cap_instead_of_bedrock_default_e2e_when_enabled() {
    if !has_bedrock_credentials() || pi_js::env::var("BEDROCK_RUN_LIVE_E2E").as_deref() != Some("1") {
        return;
    }

    let mut model = amazon_bedrock_models()
        .get("global.anthropic.claude-sonnet-4-6")
        .expect("claude-sonnet-4-6")
        .clone();
    model.max_tokens = 6000;

    let context = normalize_context(&Context {
        system_prompt: Some(
            "You are a deterministic text generator. Follow the requested output format exactly."
                .to_string(),
        ),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text(
                "Output exactly 5200 repetitions of the token alpha, separated by single spaces. Do not number them. Do not use markdown. Do not add any other text."
                    .to_string(),
            ),
            timestamp: pi_js::time::now_ms(),
        })],
        tools: None,
    });

    let response = stream_with_options(
        &model,
        &context,
        Some(BedrockOptions {
            reasoning: Some(ThinkingLevel::Low),
            ..BedrockOptions::default()
        }),
    )
    .result()
    .await;

    assert_ne!(response.stop_reason, StopReason::Error, "{:?}", response.error_message);
    assert!(response.usage.output > 4096);
}

#[tokio::test]
async fn application_inference_profile_uses_adaptive_thinking_via_model_name() {
    let mut model = amazon_bedrock_models()
        .get("global.anthropic.claude-opus-4-6-v1")
        .expect("claude-opus-4-6-v1")
        .clone();
    model.id = "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/my-profile".to_string();
    model.name = "Claude Opus 4.6".to_string();

    let payload = capture_payload(&model, None).await;
    assert_eq!(
        payload["additionalModelRequestFields"]["thinking"],
        json!({ "type": "adaptive", "display": "summarized" })
    );
    assert_eq!(
        payload["additionalModelRequestFields"]["output_config"],
        json!({ "effort": "high" })
    );
}

#[tokio::test]
async fn application_inference_profile_injects_cache_points_via_model_name() {
    let mut model = amazon_bedrock_models()
        .get("global.anthropic.claude-opus-4-6-v1")
        .expect("claude-opus-4-6-v1")
        .clone();
    model.id = "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/my-profile".to_string();
    model.name = "Claude Sonnet 4.6".to_string();

    let captured: Arc<Mutex<Option<JsonValue>>> = Arc::new(Mutex::new(None));
    let captured_ref = Arc::clone(&captured);
    let context = normalize_context(&Context {
        system_prompt: Some("You are helpful.".to_string()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Hello".to_string()),
            timestamp: 1,
        })],
        tools: None,
    });
    let options = BedrockOptions {
        base: StreamOptions {
            on_payload: Some(Arc::new(move |payload, _model| {
                let captured_inner = Arc::clone(&captured_ref);
                Box::pin(async move {
                    *captured_inner.lock().unwrap() = Some(payload);
                    Err(pi_js::Error::js("PayloadCaptured", "payload captured"))
                })
            })),
            ..StreamOptions::default()
        },
        ..BedrockOptions::default()
    };

    let mut s = stream_with_options(&model, &context, Some(options));
    while let Some(event) = s.next().await {
        if matches!(event, AssistantMessageEvent::Error { .. }) {
            break;
        }
    }

    let payload = captured.lock().unwrap().clone().expect("captured payload");
    let sys = payload["system"].as_array().unwrap();
    assert_eq!(sys.len(), 2);
    assert!(sys[1].get("cachePoint").is_some());

    let msgs = payload["messages"].as_array().unwrap();
    let last_content = msgs.last().unwrap()["content"].as_array().unwrap();
    assert!(last_content.last().unwrap().get("cachePoint").is_some());
}

#[tokio::test]
async fn application_inference_profile_falls_back_to_fixed_budget_thinking_via_model_name() {
    let mut model = amazon_bedrock_models()
        .get("us.anthropic.claude-sonnet-4-5-20250929-v1:0")
        .expect("claude-sonnet-4-5")
        .clone();
    model.id = "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/my-profile".to_string();
    model.name = "Claude Sonnet 4.5".to_string();

    let payload = capture_payload(&model, None).await;
    assert_eq!(
        payload["additionalModelRequestFields"]["thinking"]["type"],
        json!("enabled")
    );
    assert!(payload["additionalModelRequestFields"]["thinking"]["budget_tokens"].is_number());
    assert_eq!(
        payload["additionalModelRequestFields"]["anthropic_beta"],
        json!(["interleaved-thinking-2025-05-14"])
    );
}

#[tokio::test]
async fn sends_reasoning_effort_for_gpt_6_and_gpt_5_6_on_bedrock() {
    for (reasoning, effort) in [
        (ThinkingLevel::Minimal, "low"),
        (ThinkingLevel::Low, "low"),
        (ThinkingLevel::Medium, "medium"),
        (ThinkingLevel::High, "high"),
        (ThinkingLevel::XHigh, "xhigh"),
        (ThinkingLevel::Max, "max"),
    ] {
        for id in [
            "global.openai.gpt-6-sol",
            "us.openai.gpt-6-luna",
            "global.openai.gpt-5.6-sol",
        ] {
            let model = amazon_bedrock_models().get(id).expect(id);
            let payload = capture_payload(
                model,
                Some(BedrockOptions {
                    reasoning: Some(reasoning),
                    ..BedrockOptions::default()
                }),
            )
            .await;
            assert_eq!(
                payload["additionalModelRequestFields"],
                json!({ "reasoning": { "effort": effort } }),
                "{id}"
            );
        }
    }
}

#[tokio::test]
async fn sends_reasoning_effort_when_only_model_name_identifies_a_gpt_model() {
    let mut model = amazon_bedrock_models()
        .get("global.openai.gpt-6-sol")
        .expect("global.openai.gpt-6-sol")
        .clone();
    model.id = "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/my-profile".to_string();
    model.name = "GPT-6 Sol".to_string();

    let payload = capture_payload(
        &model,
        Some(BedrockOptions {
            reasoning: Some(ThinkingLevel::Medium),
            ..BedrockOptions::default()
        }),
    )
    .await;
    assert_eq!(
        payload["additionalModelRequestFields"],
        json!({ "reasoning": { "effort": "medium" } })
    );
}

#[tokio::test]
async fn sends_flat_reasoning_effort_for_gpt_oss_clamped_to_high() {
    let model = amazon_bedrock_models()
        .get("openai.gpt-oss-120b-1:0")
        .expect("openai.gpt-oss-120b-1:0");

    for (reasoning, expected) in [
        (ThinkingLevel::Minimal, "low"),
        (ThinkingLevel::Medium, "medium"),
        (ThinkingLevel::XHigh, "high"),
    ] {
        let payload = capture_payload(
            model,
            Some(BedrockOptions {
                reasoning: Some(reasoning),
                ..BedrockOptions::default()
            }),
        )
        .await;
        assert_eq!(
            payload["additionalModelRequestFields"],
            json!({ "reasoning_effort": expected })
        );
    }
}

#[tokio::test]
async fn sends_no_reasoning_fields_when_bedrock_gpt_reasoning_is_off() {
    let model = amazon_bedrock_models()
        .get("global.openai.gpt-6-sol")
        .expect("global.openai.gpt-6-sol");
    let captured: Arc<Mutex<Option<JsonValue>>> = Arc::new(Mutex::new(None));
    let captured_ref = Arc::clone(&captured);

    let context = normalize_context(&make_context());
    let mut s = stream_with_options(
        model,
        &context,
        Some(BedrockOptions {
            base: StreamOptions {
                on_payload: Some(Arc::new(move |payload, _model| {
                    let captured_inner = Arc::clone(&captured_ref);
                    Box::pin(async move {
                        *captured_inner.lock().unwrap() = Some(payload);
                        Err(pi_js::Error::js("PayloadCaptured", "payload captured"))
                    })
                })),
                ..StreamOptions::default()
            },
            ..BedrockOptions::default()
        }),
    );
    while let Some(event) = s.next().await {
        if matches!(event, AssistantMessageEvent::Error { .. }) {
            break;
        }
    }

    let payload = captured.lock().unwrap().clone().expect("expected payload");
    assert!(payload.get("additionalModelRequestFields").is_none());
}
