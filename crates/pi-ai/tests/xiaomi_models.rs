//! Port of packages/ai/test/xiaomi-models.test.ts

use pi_ai::compat::{get_models, stream_simple};
use pi_ai::models::get_supported_thinking_levels;
use pi_ai::types::{
    Context, Message, ModelThinkingLevel, SimpleStreamOptions, StreamOptions, ThinkingLevel, UserMessage,
    UserMessageContent,
};
use serde_json::{Value, json};

const XIAOMI_PROVIDERS: &[&str] = &[
    "xiaomi",
    "xiaomi-token-plan-cn",
    "xiaomi-token-plan-ams",
    "xiaomi-token-plan-sgp",
];
const DEPRECATED_MODEL_IDS: &[&str] = &["mimo-v2-flash", "mimo-v2-omni", "mimo-v2-pro"];
const REPLACEMENT_MODEL_IDS: &[&str] = &["mimo-v2.6-flash", "mimo-v2.6-pro"];

#[test]
fn omits_deprecated_models_from_xiaomi_providers() {
    for &provider in XIAOMI_PROVIDERS {
        let model_ids: Vec<String> = get_models(Some(provider)).into_iter().map(|m| m.id).collect();
        for &deprecated in DEPRECATED_MODEL_IDS {
            assert!(
                !model_ids.iter().any(|id| id == deprecated),
                "{provider} should omit {deprecated}"
            );
        }
    }
}

#[test]
fn keeps_replacement_models_on_xiaomi_providers() {
    for &provider in XIAOMI_PROVIDERS {
        let model_ids: Vec<String> = get_models(Some(provider)).into_iter().map(|m| m.id).collect();
        for &replacement in REPLACEMENT_MODEL_IDS {
            assert!(
                model_ids.iter().any(|id| id == replacement),
                "{provider} should keep {replacement}"
            );
        }
    }
}

// #10818: MiMo Chat Completions only supports a thinking on/off toggle.
#[tokio::test]
async fn models_xiaomi_reasoning_as_an_off_on_toggle() {
    for &provider in XIAOMI_PROVIDERS {
        let models: Vec<_> = get_models(Some(provider)).into_iter().filter(|m| m.reasoning).collect();
        assert!(!models.is_empty(), "{provider} should have reasoning models");

        for model in models {
            assert_eq!(
                get_supported_thinking_levels(&model),
                vec![ModelThinkingLevel::Off, ModelThinkingLevel::High]
            );

            let capture = |reasoning: Option<ThinkingLevel>| {
                let model = model.clone();
                async move {
                    let payload = std::sync::Arc::new(std::sync::Mutex::new(None::<Value>));
                    let p_ref = std::sync::Arc::clone(&payload);
                    let ctx = Context {
                        system_prompt: None,
                        messages: vec![Message::User(UserMessage {
                            content: UserMessageContent::Text("test".to_owned()),
                            timestamp: 0,
                        })],
                        tools: None,
                    };
                    stream_simple(
                        &model,
                        &ctx,
                        Some(SimpleStreamOptions {
                            base: StreamOptions {
                                api_key: Some("test-xiaomi-key".to_owned()),
                                on_payload: Some(std::sync::Arc::new(move |val, _| {
                                    let p_ref = std::sync::Arc::clone(&p_ref);
                                    Box::pin(async move {
                                        *p_ref.lock().unwrap() = Some(val);
                                        Err(pi_js::Error::msg("payload captured"))
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
                    let captured = payload.lock().unwrap().take();
                    captured.expect("expected captured payload")
                }
            };

            let enabled = capture(Some(ThinkingLevel::High)).await;
            assert_eq!(enabled.get("thinking"), Some(&json!({ "type": "enabled" })));
            assert!(enabled.get("reasoning_effort").is_none());

            let disabled = capture(None).await;
            assert_eq!(disabled.get("thinking"), Some(&json!({ "type": "disabled" })));
            assert!(disabled.get("reasoning_effort").is_none());
        }
    }
}
