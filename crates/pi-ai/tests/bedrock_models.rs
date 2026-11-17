//! Port of packages/ai/test/bedrock-models.test.ts

use pi_ai::api::bedrock_converse_stream::stream_with_options;
use pi_ai::providers::amazon_bedrock_models::amazon_bedrock_models;
use pi_ai::types::{AssistantContentBlock, Context, Message, StopReason, UserMessage, UserMessageContent};
use pi_ai::utils::transcript::normalize_context;

fn has_bedrock_credentials() -> bool {
    let env_non_empty = |name: &str| pi_js::env::var(name).filter(|v| !v.is_empty()).is_some();
    env_non_empty("AWS_PROFILE")
        || (env_non_empty("AWS_ACCESS_KEY_ID") && env_non_empty("AWS_SECRET_ACCESS_KEY"))
        || env_non_empty("AWS_BEARER_TOKEN_BEDROCK")
}

#[test]
fn should_get_all_available_bedrock_models() {
    let models = amazon_bedrock_models();
    assert!(!models.is_empty());
}

#[test]
fn exposes_claude_opus_5_through_inference_profile_only() {
    let models = amazon_bedrock_models();
    assert!(models.contains_key("global.anthropic.claude-opus-5"));
    assert!(!models.contains_key("anthropic.claude-opus-5"));
}

#[tokio::test]
async fn extensive_bedrock_models_e2e_when_enabled() {
    let enabled = pi_js::env::var("BEDROCK_EXTENSIVE_MODEL_TEST")
        .filter(|v| !v.is_empty())
        .is_some();
    if !has_bedrock_credentials() || !enabled {
        return;
    }

    let models: Vec<_> = amazon_bedrock_models().values().cloned().collect();
    for model in models {
        let context = normalize_context(&Context {
            system_prompt: Some("You are a helpful assistant. Be extremely concise.".to_string()),
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Reply with exactly: 'OK'".to_string()),
                timestamp: pi_js::time::now_ms(),
            })],
            tools: None,
        });

        let response = stream_with_options(&model, &context, None).result().await;
        assert_ne!(response.stop_reason, StopReason::Error);
        assert!(!response.content.is_empty());
        assert!(response.usage.input + response.usage.cache_read > 0);
        assert!(response.usage.output > 0);
        assert!(response.error_message.is_none());

        let text_content: String = response
            .content
            .iter()
            .filter_map(|b| match b {
                AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_string();
        assert!(!text_content.is_empty(), "empty text for {}", model.id);
    }
}
