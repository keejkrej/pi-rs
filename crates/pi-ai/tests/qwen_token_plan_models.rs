//! Port of packages/ai/test/qwen-token-plan-models.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::json;

use pi_ai::compat::{get_models, stream_simple};
use pi_ai::env_api_keys::find_env_keys;
use pi_ai::types::{
    Context, Message, Model, ModelThinkingLevel, ProviderEnv, SimpleStreamOptions, StreamOptions, ThinkingLevel,
    UserMessage, UserMessageContent,
};
use pi_js::fetch::testing::{chunked_response, mock_fetch};
use pi_js::time;

const TEXT_MODELS: &[&str] = &[
    "MiniMax-M2.5",
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro",
    "deepseek-v4-pro-0813",
    "deepseek-v4.1-flash",
    "deepseek-v4.1-flash-1017",
    "glm-5",
    "glm-5.1",
    "glm-5.2",
    "glm-5.3",
    "kimi-k2.5",
    "kimi-k2.6",
    "kimi-k2.7-code",
    "qwen3.6-flash",
    "qwen3.6-plus",
    "qwen3.7-max",
    "qwen3.7-plus",
    "qwen3.8-flash",
    "qwen3.8-max",
    "qwen4-max-preview",
];

const INDIVIDUAL_TEXT_MODELS: &[&str] = &[
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro",
    "deepseek-v4-pro-0813",
    "deepseek-v4.1-flash",
    "deepseek-v4.1-flash-1017",
    "glm-5.2",
    "glm-5.3",
    "qwen3.6-flash",
    "qwen3.7-max",
    "qwen3.7-plus",
    "qwen3.8-flash",
    "qwen3.8-max",
];

const IMAGE_MODELS: &[&str] = &[
    "qwen-image-2.0",
    "qwen-image-2.0-pro",
    "wan2.7-image",
    "wan2.7-image-pro",
];

const QWEN_THINKING_MODELS: &[&str] = &[
    "MiniMax-M2.5",
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro",
    "deepseek-v4-pro-0813",
    "deepseek-v4.1-flash",
    "deepseek-v4.1-flash-1017",
    "glm-5",
    "glm-5.1",
    "glm-5.2",
    "glm-5.3",
    "kimi-k2.5",
    "kimi-k2.6",
    "kimi-k2.7-code",
    "qwen3.6-flash",
    "qwen3.6-plus",
    "qwen3.7-max",
    "qwen3.7-plus",
    "qwen3.8-flash",
    "qwen3.8-max",
    "qwen4-max-preview",
];

const QWEN_REASONING_EFFORT_MODELS: &[&str] = &[
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro",
    "deepseek-v4-pro-0813",
    "deepseek-v4.1-flash",
    "deepseek-v4.1-flash-1017",
    "glm-5",
    "glm-5.1",
    "glm-5.2",
    "glm-5.3",
];

const QWEN38_MODELS: &[&str] = &["qwen3.8-flash", "qwen3.8-max"];

fn sse_stub_response() -> pi_js::fetch::Response {
    let chunk = json!({
        "choices": [{ "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "prompt_tokens_details": { "cached_tokens": 0 },
            "completion_tokens_details": { "reasoning_tokens": 0 }
        }
    });
    let frame = format!("data: {}\n\ndata: [DONE]\n\n", pi_js::json::stringify(&chunk));
    chunked_response(200, &[("content-type", "text/event-stream")], vec![frame])
}

async fn capture_qwen_payload(model: &Model, reasoning: ThinkingLevel) -> serde_json::Value {
    let _fetch_guard = mock_fetch(|_req| Box::pin(async { Ok(sse_stub_response()) }));
    let captured = Arc::new(Mutex::new(None));
    let captured_ref = Arc::clone(&captured);
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Hi".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
    };

    let mut base = StreamOptions::default();
    base.api_key = Some("test".to_owned());
    base.on_payload = Some(Arc::new(move |params, _| {
        let mut guard = captured_ref.lock().unwrap();
        *guard = Some(params);
        Box::pin(async { Ok(None) })
    }));
    let opts = SimpleStreamOptions {
        base,
        reasoning: Some(reasoning),
        ..Default::default()
    };

    let _ = stream_simple(model, &context, Some(opts)).result().await;
    captured.lock().unwrap().clone().expect("captured payload")
}

#[test]
fn exposes_exact_documented_individual_text_models_on_both_endpoints() {
    let mut expected: Vec<String> = INDIVIDUAL_TEXT_MODELS.iter().map(|s| (*s).to_owned()).collect();
    expected.sort();
    for provider in ["qwen-token-plan-individual", "qwen-token-plan-individual-cn"] {
        let mut model_ids: Vec<String> = get_models(Some(provider)).into_iter().map(|m| m.id).collect();
        model_ids.sort();
        assert_eq!(model_ids, expected, "{provider}");
    }
}

#[test]
fn reuses_international_and_china_token_plan_env_vars() {
    let mut env = ProviderEnv::new();
    env.insert("QWEN_TOKEN_PLAN_API_KEY".to_owned(), "test".to_owned());
    assert_eq!(
        find_env_keys("qwen-token-plan-individual", Some(&env)),
        Some(vec!["QWEN_TOKEN_PLAN_API_KEY"])
    );
    let mut env_cn = ProviderEnv::new();
    env_cn.insert("QWEN_TOKEN_PLAN_CN_API_KEY".to_owned(), "test".to_owned());
    assert_eq!(
        find_env_keys("qwen-token-plan-individual-cn", Some(&env_cn)),
        Some(vec!["QWEN_TOKEN_PLAN_CN_API_KEY"])
    );
}

#[test]
fn exposes_all_text_models_and_omits_image_models() {
    for provider in ["qwen-token-plan", "qwen-token-plan-cn"] {
        let model_ids: Vec<String> = get_models(Some(provider)).into_iter().map(|m| m.id).collect();
        for &expected in TEXT_MODELS {
            assert!(
                model_ids.iter().any(|id| id == expected),
                "{provider} should include {expected}"
            );
        }
        for &excluded in IMAGE_MODELS {
            assert!(
                !model_ids.iter().any(|id| id == excluded),
                "{provider} should not include {excluded}"
            );
        }
    }
}

#[tokio::test]
async fn sends_qwen_thinking_fields_for_all_thinking_models() {
    let mut cases: Vec<(&str, &str)> = Vec::new();
    for provider in ["qwen-token-plan", "qwen-token-plan-cn"] {
        for &model_id in QWEN_THINKING_MODELS {
            cases.push((provider, model_id));
        }
    }
    for provider in ["qwen-token-plan-individual", "qwen-token-plan-individual-cn"] {
        for &model_id in INDIVIDUAL_TEXT_MODELS {
            cases.push((provider, model_id));
        }
    }

    for (provider, model_id) in cases {
        let model = get_models(Some(provider))
            .into_iter()
            .find(|m| m.id == model_id)
            .unwrap_or_else(|| panic!("missing model {provider}/{model_id}"));
        let payload = capture_qwen_payload(&model, ThinkingLevel::High).await;
        assert_eq!(payload.get("enable_thinking"), Some(&json!(true)));
        assert!(payload.get("thinking").is_none());
    }
}

#[test]
fn exposes_qwen_reasoning_effort_and_qwen38_maps() {
    let mut effort_cases: Vec<(&str, &str)> = Vec::new();
    for provider in ["qwen-token-plan", "qwen-token-plan-cn"] {
        for &model_id in QWEN_REASONING_EFFORT_MODELS {
            effort_cases.push((provider, model_id));
        }
    }
    for provider in ["qwen-token-plan-individual", "qwen-token-plan-individual-cn"] {
        for model_id in [
            "deepseek-v4-flash-0731",
            "deepseek-v4-pro",
            "deepseek-v4-pro-0813",
            "deepseek-v4.1-flash",
            "deepseek-v4.1-flash-1017",
            "glm-5.2",
            "glm-5.3",
        ] {
            effort_cases.push((provider, model_id));
        }
    }

    for (provider, model_id) in effort_cases {
        let model = get_models(Some(provider))
            .into_iter()
            .find(|m| m.id == model_id)
            .unwrap_or_else(|| panic!("missing model {provider}/{model_id}"));
        let map = model.thinking_level_map.expect("thinking_level_map");
        assert_eq!(
            map.get(&ModelThinkingLevel::Minimal),
            Some(&None),
            "{provider}/{model_id}"
        );
        assert_eq!(
            map.get(&ModelThinkingLevel::Low),
            Some(
                &if matches!(model_id, "glm-5.3" | "deepseek-v4.1-flash" | "deepseek-v4.1-flash-1017") {
                    Some("low".to_owned())
                } else {
                    None
                }
            ),
            "{provider}/{model_id}"
        );
        assert_eq!(
            map.get(&ModelThinkingLevel::Medium),
            Some(&None),
            "{provider}/{model_id}"
        );
        assert_eq!(
            map.get(&ModelThinkingLevel::High),
            Some(&Some("high".to_owned())),
            "{provider}/{model_id}"
        );
        assert_eq!(
            map.get(&ModelThinkingLevel::XHigh),
            Some(&None),
            "{provider}/{model_id}"
        );
        assert_eq!(
            map.get(&ModelThinkingLevel::Max),
            Some(&Some("max".to_owned())),
            "{provider}/{model_id}"
        );
    }

    let mut expected_38 = IndexMap::new();
    expected_38.insert(ModelThinkingLevel::Minimal, None);
    expected_38.insert(ModelThinkingLevel::Low, Some("low".to_owned()));
    expected_38.insert(ModelThinkingLevel::Medium, Some("medium".to_owned()));
    expected_38.insert(ModelThinkingLevel::High, None);
    expected_38.insert(ModelThinkingLevel::XHigh, Some("xhigh".to_owned()));
    expected_38.insert(ModelThinkingLevel::Max, None);

    for provider in [
        "qwen-token-plan",
        "qwen-token-plan-cn",
        "qwen-token-plan-individual",
        "qwen-token-plan-individual-cn",
    ] {
        for &model_id in QWEN38_MODELS {
            let model = get_models(Some(provider))
                .into_iter()
                .find(|m| m.id == model_id)
                .unwrap_or_else(|| panic!("missing model {provider}/{model_id}"));
            let map = model.thinking_level_map.expect("thinking_level_map");
            for (k, v) in &expected_38 {
                assert_eq!(map.get(k), Some(v), "{provider}/{model_id} level {k}");
            }
        }
    }
}

#[test]
fn exposes_glm_5_3_reasoning_effort_levels_for_qwen_token_plan_individual() {
    let model = get_models(Some("qwen-token-plan-individual"))
        .into_iter()
        .find(|m| m.id == "glm-5.3")
        .expect("Missing model: qwen-token-plan-individual/glm-5.3");
    assert_eq!(
        model.base_url,
        "https://token-plan.maas.qwencloudapi.com/compatible-mode/v1"
    );
    let map = model.thinking_level_map.expect("thinking_level_map");
    assert_eq!(map.get(&ModelThinkingLevel::Minimal), Some(&None));
    assert_eq!(map.get(&ModelThinkingLevel::Low), Some(&Some("low".to_owned())));
    assert_eq!(map.get(&ModelThinkingLevel::Medium), Some(&None));
    assert_eq!(map.get(&ModelThinkingLevel::High), Some(&Some("high".to_owned())));
    assert_eq!(map.get(&ModelThinkingLevel::XHigh), Some(&None));
    assert_eq!(map.get(&ModelThinkingLevel::Max), Some(&Some("max".to_owned())));
}

#[test]
fn omits_retired_qwen3_8_max_preview() {
    for provider in [
        "qwen-token-plan",
        "qwen-token-plan-cn",
        "qwen-token-plan-individual",
        "qwen-token-plan-individual-cn",
    ] {
        let ids: Vec<String> = get_models(Some(provider)).into_iter().map(|m| m.id).collect();
        assert!(!ids.iter().any(|id| id == "qwen3.8-max-preview"));
    }
}

#[tokio::test]
async fn sends_qwen_reasoning_effort_and_xhigh_payloads() {
    let mut effort_cases: Vec<(&str, &str)> = Vec::new();
    for provider in ["qwen-token-plan", "qwen-token-plan-cn"] {
        for &model_id in QWEN_REASONING_EFFORT_MODELS {
            effort_cases.push((provider, model_id));
        }
    }
    for provider in ["qwen-token-plan-individual", "qwen-token-plan-individual-cn"] {
        for model_id in [
            "deepseek-v4-flash-0731",
            "deepseek-v4-pro",
            "deepseek-v4-pro-0813",
            "deepseek-v4.1-flash",
            "deepseek-v4.1-flash-1017",
            "glm-5.2",
            "glm-5.3",
        ] {
            effort_cases.push((provider, model_id));
        }
    }

    for (provider, model_id) in effort_cases {
        let model = get_models(Some(provider))
            .into_iter()
            .find(|m| m.id == model_id)
            .unwrap_or_else(|| panic!("missing model {provider}/{model_id}"));
        let payload = capture_qwen_payload(&model, ThinkingLevel::High).await;
        assert_eq!(payload.get("reasoning_effort"), Some(&json!("high")));
    }

    for provider in [
        "qwen-token-plan",
        "qwen-token-plan-cn",
        "qwen-token-plan-individual",
        "qwen-token-plan-individual-cn",
    ] {
        for &model_id in QWEN38_MODELS {
            let model = get_models(Some(provider))
                .into_iter()
                .find(|m| m.id == model_id)
                .unwrap_or_else(|| panic!("missing model {provider}/{model_id}"));
            let payload = capture_qwen_payload(&model, ThinkingLevel::XHigh).await;
            assert_eq!(payload.get("enable_thinking"), Some(&json!(true)));
            assert_eq!(payload.get("reasoning_effort"), Some(&json!("xhigh")));
            assert!(payload.get("thinking").is_none());
        }
    }
}
