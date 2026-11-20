//! Port of packages/ai/test/mistral-raw-stop-reason.test.ts

use std::sync::Arc;

use pi_ai::api::mistral_conversations::{MistralOptions, stream_with_options};
use pi_ai::providers::mistral_models::mistral_models;
use pi_ai::utils::retry::is_retryable_assistant_error;
use pi_ai::utils::transcript::normalize_context;
use pi_ai::{Context, FetchFunction, Message, Model, StopReason, StreamOptions, UserMessage};
use pi_js::fetch::testing::response;
use pretty_assertions::assert_eq;
use serde_json::json;

fn test_model() -> Model {
    mistral_models()
        .get("devstral-medium-latest")
        .expect("devstral-medium-latest should exist in mistral catalog")
        .clone()
}

fn sample_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: "hello".into(),
            timestamp: pi_js::time::now_ms(),
        })],
        tools: None,
    }
}

fn create_fetch(model_id: &str, finish_reason: &str) -> FetchFunction {
    let payload = json!({
        "id": "mistral-response-id",
        "model": model_id,
        "choices": [{
            "index": 0,
            "finish_reason": finish_reason,
            "delta": {}
        }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 0,
            "total_tokens": 1
        }
    });
    let body = format!("data: {}\n\ndata: [DONE]\n\n", pi_js::json::stringify(&payload));
    Arc::new(move |_req| {
        let b = body.clone();
        Box::pin(async move { Ok(response(200, &[("content-type", "text/event-stream")], b)) })
    })
}

#[tokio::test]
async fn preserves_raw_mistral_finish_reasons_for_successful_stops() {
    let model = test_model();
    let opts = MistralOptions {
        base: StreamOptions {
            api_key: Some("test".to_owned()),
            fetch: Some(create_fetch(&model.id, "stop")),
            ..Default::default()
        },
        ..Default::default()
    };

    let message = stream_with_options(&model, &normalize_context(&sample_context()), Some(opts))
        .result()
        .await;

    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("stop"));
    assert_eq!(message.error_message, None);
}

#[tokio::test]
async fn preserves_raw_mistral_finish_reasons_for_provider_error_stops() {
    let model = test_model();
    let opts = MistralOptions {
        base: StreamOptions {
            api_key: Some("test".to_owned()),
            fetch: Some(create_fetch(&model.id, "error")),
            ..Default::default()
        },
        ..Default::default()
    };

    let message = stream_with_options(&model, &normalize_context(&sample_context()), Some(opts))
        .result()
        .await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("error"));
    assert_eq!(
        message.error_message.as_deref(),
        Some("Provider stopped with: error (server error)")
    );
    // #10487
    assert!(is_retryable_assistant_error(&message));
}

#[tokio::test]
async fn treats_unknown_mistral_finish_reasons_as_provider_error_stops() {
    let model = test_model();
    let opts = MistralOptions {
        base: StreamOptions {
            api_key: Some("test".to_owned()),
            fetch: Some(create_fetch(&model.id, "unmapped_error")),
            ..Default::default()
        },
        ..Default::default()
    };

    let message = stream_with_options(&model, &normalize_context(&sample_context()), Some(opts))
        .result()
        .await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("unmapped_error"));
    assert_eq!(
        message.error_message.as_deref(),
        Some("Provider stopped with: unmapped_error")
    );
    assert!(!is_retryable_assistant_error(&message));
}
