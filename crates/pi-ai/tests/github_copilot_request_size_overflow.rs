//! Port of packages/ai/test/github-copilot-request-size-overflow.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_ai::api::openai_responses::stream;
use pi_ai::compat::normalize_context;
use pi_ai::types::{
    ApiStreamOptions, Context, Message, Model, ModelCost, ModelInput, StopReason, StreamOptions, UserMessage,
    UserMessageContent,
};
use pi_ai::utils::overflow::is_context_overflow;
use pi_js::fetch::{Headers, Response};
use serde_json::json;

fn copilot_model() -> Model {
    Model {
        r#type: None,
        id: "copilot-test-model".to_owned(),
        name: "Copilot test model".to_owned(),
        api: "openai-responses".to_owned(),
        provider: "github-copilot".into(),
        base_url: "https://copilot.invalid".to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text, ModelInput::Image],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 1_000_000,
        max_tokens: 128_000,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

#[tokio::test]
async fn classifies_copilot_http_413_nested_and_flat_vs_400_through_real_http_client() {
    let model = copilot_model();
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Continue".to_owned()),
            timestamp: 0,
        })],
        tools: None,
    });

    for (status, nested, expected_overflow) in [(413_u16, true, true), (413, false, true), (400, true, false)] {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_ref = Arc::clone(&calls);
        let error = json!({ "message": "failed to parse request", "code": "" });
        let body = if nested { json!({ "error": error }) } else { error };

        let result = stream(
            &model,
            &context,
            Some(ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some("test-key".to_owned()),
                    max_retries: Some(0),
                    fetch: Some(Arc::new(move |_| {
                        calls_ref.fetch_add(1, Ordering::SeqCst);
                        let payload = pi_js::json::stringify(&body);
                        Box::pin(async move {
                            Ok(Response::new(
                                status,
                                Headers::from_pairs([("content-type", "application/json")]),
                                payload,
                            ))
                        })
                    })),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .result()
        .await;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.provider.as_str(), "github-copilot");
        assert_eq!(result.stop_reason, StopReason::Error);
        assert_eq!(result.usage.total_tokens, 0);
        let msg = result.error_message.as_deref().unwrap_or("");
        assert!(msg.contains(&format!("({status})")), "{msg}");
        assert!(msg.contains("failed to parse request"), "{msg}");
        assert_eq!(
            is_context_overflow(&result, Some(model.context_window)),
            expected_overflow
        );
    }
}
