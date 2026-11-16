//! Port of packages/ai/test/provider-error-body-passthrough.test.ts (unit ai-providers-builtin)

use std::sync::Arc;

use pi_ai::api::openrouter_images::generate_images as generate_openrouter_images;
use pi_ai::images::generate_images;
use pi_ai::images_api_registry::{clear_images_api_providers, register_images_api_provider};
use pi_ai::providers::images::register_builtins::register_built_in_images_api_providers;
use pi_ai::types::{
    AssistantImages, ImageModel, ImagesContext, ImagesFunction, ImagesOptions, ImagesStopReason, ModelCost, ModelInput,
    ModelOutput, ProviderRequestOptions, TextContent, UserContentBlock,
};
use pi_ai::utils::error_body::{SdkErrorInfo, format_provider_error, normalize_provider_error};
use pi_js::fetch::{Headers, Response};
use serde_json::json;
use serial_test::serial;

struct ImagesResetGuard;

impl Drop for ImagesResetGuard {
    fn drop(&mut self) {
        clear_images_api_providers();
    }
}

#[tokio::test]
#[serial]
async fn surfaces_http_body_reason_instead_of_opaque_sdk_message() {
    clear_images_api_providers();
    register_built_in_images_api_providers();
    let _guard = ImagesResetGuard;

    let model = ImageModel {
        id: "black-forest-labs/flux.2-pro".to_string(),
        name: "FLUX.2 Pro".to_string(),
        api: "openrouter-images".to_string(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api/v1".to_string(),
        input: vec![ModelInput::Text, ModelInput::Image],
        input_limits: None,
        output: vec![ModelOutput::Image],
        cost: ModelCost {
            input: 0.015,
            output: 0.03,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        headers: None,
    };

    let context = ImagesContext {
        input: vec![UserContentBlock::Text(TextContent {
            text: "Generate a dog".to_string(),
            text_signature: None,
        })],
    };
    // Trigger INIT.call_once before overriding with our mock image provider.
    let _ = generate_images(&model, &context, None).await;

    let image_fn: ImagesFunction = Arc::new(|req_model, _ctx, _opts| {
        let model = req_model.clone();
        Box::pin(async move {
            let js_err = SdkErrorInfo {
                status_code: None,
                status: Some(403),
                metadata_http_status_code: None,
                response_status_code: None,
                body: None,
                error: Some(json!({ "error": "blocked by gateway WAF" })),
                response_body: None,
                message: "403 status code (no body)".to_string(),
            }
            .into_js_error();
            let norm = normalize_provider_error(&js_err);
            AssistantImages {
                api: model.api,
                provider: model.provider,
                model: model.id,
                response_id: None,
                output: Vec::new(),
                usage: None,
                stop_reason: ImagesStopReason::Error,
                error_message: Some(format_provider_error(&norm, None)),
                timestamp: 1,
            }
        })
    });

    register_images_api_provider("openrouter-images", image_fn, None);

    let output = generate_images(&model, &context, None).await.unwrap();
    assert_eq!(output.stop_reason, ImagesStopReason::Error);
    let err = output.error_message.as_deref().unwrap_or("");
    assert!(err.contains("403"), "err={err}");
    assert!(err.contains("blocked by gateway WAF"), "err={err}");
    assert_ne!(err, "403 status code (no body)");

    let direct = generate_openrouter_images(
        &model,
        &context,
        Some(ImagesOptions {
            base: ProviderRequestOptions {
                api_key: Some("test-key".to_owned()),
                fetch: Some(Arc::new(|_req| {
                    Box::pin(async {
                        Ok(Response::new(
                            403,
                            Headers::from_pairs([("content-type", "application/json")]),
                            r#"{"error":"blocked by gateway WAF"}"#,
                        ))
                    })
                })),
                max_retries: Some(0),
                ..Default::default()
            },
            metadata: None,
        }),
    )
    .await;
    assert_eq!(direct.stop_reason, ImagesStopReason::Error);
    let direct_err = direct.error_message.as_deref().unwrap_or("");
    assert!(direct_err.contains("403"), "direct_err={direct_err}");
    assert!(direct_err.contains("blocked by gateway WAF"), "direct_err={direct_err}");
}
