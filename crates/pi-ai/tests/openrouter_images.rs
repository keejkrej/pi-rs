//! Port of packages/ai/test/openrouter-images.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::{Value, json};

use pi_ai::api::openrouter_images::generate_images;
use pi_ai::providers::openrouter::openrouter_provider;
use pi_ai::types::{
    ImageModel, ImagesContext, ImagesOptions, ImagesStopReason, ModelCost, ModelModality, ProviderHeaders, TextContent,
    UserContentBlock,
};
use pi_js::Error;
use pi_js::abort::AbortController;
use pi_js::fetch::testing::{mock_fetch, response};

fn gemini_image_model() -> ImageModel {
    let mut headers = IndexMap::new();
    headers.insert("HTTP-Referer".to_owned(), "https://example.com".to_owned());
    ImageModel {
        id: "google/gemini-3.1-flash-image-preview".to_owned(),
        name: "Gemini 3.1 Flash Image Preview".to_owned(),
        api: "openrouter-images".to_owned(),
        provider: "openrouter".to_owned(),
        base_url: "https://openrouter.ai/api/v1".to_owned(),
        input: vec![ModelModality::Text, ModelModality::Image],
        input_limits: None,
        output: vec![ModelModality::Text, ModelModality::Image],
        cost: ModelCost {
            input: 0.015,
            output: 0.03,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        headers: Some(headers),
    }
}

fn flux_image_model() -> ImageModel {
    ImageModel {
        id: "black-forest-labs/flux.2-pro".to_owned(),
        name: "FLUX.2 Pro".to_owned(),
        api: "openrouter-images".to_owned(),
        provider: "openrouter".to_owned(),
        base_url: "https://openrouter.ai/api/v1".to_owned(),
        input: vec![ModelModality::Text, ModelModality::Image],
        input_limits: None,
        output: vec![ModelModality::Image],
        cost: ModelCost {
            input: 0.015,
            output: 0.03,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        headers: None,
    }
}

fn dog_context() -> ImagesContext {
    ImagesContext {
        input: vec![UserContentBlock::Text(TextContent::new("Generate a dog"))],
    }
}

#[tokio::test]
async fn returns_text_plus_images_in_final_output() {
    let captured_url = Arc::new(Mutex::new(None::<String>));
    let captured_body = Arc::new(Mutex::new(None::<Value>));
    let captured_headers = Arc::new(Mutex::new(None::<IndexMap<String, String>>));

    let url_ref = Arc::clone(&captured_url);
    let body_ref = Arc::clone(&captured_body);
    let headers_ref = Arc::clone(&captured_headers);

    let _guard = mock_fetch(move |req| {
        let url_ref = Arc::clone(&url_ref);
        let body_ref = Arc::clone(&body_ref);
        let headers_ref = Arc::clone(&headers_ref);
        Box::pin(async move {
            *url_ref.lock().unwrap() = Some(req.url.clone());
            *body_ref.lock().unwrap() = req.body_json();
            *headers_ref.lock().unwrap() = Some(req.headers.entries().into_iter().collect());
            let body = pi_js::json::stringify(&json!({
                "id": "img-1",
                "usage": {
                    "prompt_tokens": 12,
                    "completion_tokens": 34,
                    "prompt_tokens_details": { "cached_tokens": 0 },
                },
                "choices": [{
                    "message": {
                        "content": "Here is your image.",
                        "images": [{ "image_url": "data:image/png;base64,ZmFrZS1wbmc=" }],
                    }
                }],
            }));
            Ok(response(200, &[("content-type", "application/json")], body))
        })
    });

    let model = gemini_image_model();
    let mut opts = ImagesOptions::default();
    opts.base.api_key = Some("test".to_owned());

    let output = generate_images(&model, &dog_context(), Some(opts)).await;

    assert_eq!(output.stop_reason, ImagesStopReason::Stop);
    assert_eq!(output.response_id.as_deref(), Some("img-1"));
    match &output.output[0] {
        UserContentBlock::Text(t) => assert_eq!(t.text, "Here is your image."),
        other => panic!("expected text content, got {other:?}"),
    }
    match &output.output[1] {
        UserContentBlock::Image(img) => {
            assert_eq!(img.mime_type, "image/png");
            assert_eq!(img.data, "ZmFrZS1wbmc=");
        }
        other => panic!("expected image content, got {other:?}"),
    }

    assert_eq!(
        captured_url.lock().unwrap().as_deref(),
        Some("https://openrouter.ai/api/v1/chat/completions")
    );
    let headers = captured_headers.lock().unwrap().clone().unwrap();
    assert_eq!(
        headers.get("http-referer").map(String::as_str),
        Some("https://example.com")
    );
    assert_eq!(headers.get("authorization").map(String::as_str), Some("Bearer test"));

    let params = captured_body.lock().unwrap().clone().unwrap();
    assert_eq!(params["stream"], false);
    assert_eq!(params["modalities"], json!(["image", "text"]));
    assert_eq!(params["messages"][0]["content"][0]["type"], "text");
    assert_eq!(params["messages"][0]["content"][0]["text"], "Generate a dog");
}

#[tokio::test]
async fn passes_through_abort_signal_and_returns_aborted_result() {
    let saw_aborted_signal = Arc::new(Mutex::new(false));
    let saw_ref = Arc::clone(&saw_aborted_signal);

    let _guard = mock_fetch(move |req| {
        let saw_ref = Arc::clone(&saw_ref);
        Box::pin(async move {
            *saw_ref.lock().unwrap() = req.signal.as_ref().is_some_and(|s| s.aborted());
            Err(Error::msg("Request aborted"))
        })
    });

    let controller = AbortController::new();
    controller.abort(None);

    let mut opts = ImagesOptions::default();
    opts.base.api_key = Some("test".to_owned());
    opts.base.signal = Some(controller.signal());

    let output = generate_images(&flux_image_model(), &dog_context(), Some(opts)).await;
    assert_eq!(output.stop_reason, ImagesStopReason::Aborted);
    assert_eq!(output.error_message.as_deref(), Some("Request aborted"));
    assert!(*saw_aborted_signal.lock().unwrap());
}

#[tokio::test]
async fn generate_images_resolves_the_final_assistant_images_result() {
    let captured_body = Arc::new(Mutex::new(None::<Value>));
    let body_ref = Arc::clone(&captured_body);

    let _guard = mock_fetch(move |req| {
        let body_ref = Arc::clone(&body_ref);
        Box::pin(async move {
            *body_ref.lock().unwrap() = req.body_json();
            let body = pi_js::json::stringify(&json!({
                "id": "img-1",
                "usage": { "prompt_tokens": 12, "completion_tokens": 34, "prompt_tokens_details": { "cached_tokens": 0 } },
                "choices": [{
                    "message": {
                        "content": "Here is your image.",
                        "images": [{ "image_url": { "url": "data:image/png;base64,ZmFrZS1wbmc=" } }],
                    }
                }],
            }));
            Ok(response(200, &[("content-type", "application/json")], body))
        })
    });

    let provider = openrouter_provider();
    let mut opts = ImagesOptions::default();
    opts.base.api_key = Some("test".to_owned());

    let output = provider
        .generate_images(&flux_image_model(), &dog_context(), Some(opts))
        .expect("openrouter provider must handle openrouter-images")
        .await;

    assert!(
        output
            .output
            .iter()
            .any(|item| matches!(item, UserContentBlock::Image(_)))
    );
    let params = captured_body.lock().unwrap().clone().unwrap();
    // Image-only models must not request text output.
    assert_eq!(params["modalities"], json!(["image"]));
}

#[tokio::test]
async fn computes_cached_read_and_cache_write_usage_and_formats_http_errors() {
    let _guard = mock_fetch(move |_req| {
        Box::pin(async move {
            Ok(response(
                400,
                &[("content-type", "application/json")],
                r#"{"error":{"message":"invalid prompt"}}"#,
            ))
        })
    });

    let mut hdrs = ProviderHeaders::new();
    hdrs.insert("Authorization".to_owned(), None);
    let mut opts = ImagesOptions::default();
    opts.base.api_key = Some("test".to_owned());
    opts.base.headers = Some(hdrs);

    let err_out = generate_images(&flux_image_model(), &dog_context(), Some(opts)).await;
    assert_eq!(err_out.stop_reason, ImagesStopReason::Error);
    let msg = err_out.error_message.as_deref().unwrap_or_default();
    assert!(msg.starts_with("400:"), "unexpected error format: {msg}");
    assert!(msg.contains("invalid prompt"));
}
