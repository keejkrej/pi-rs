//! Port of packages/coding-agent/test/model-runtime-cloudflare-compat.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::compat::{complete, reset_api_providers};
use pi_ai::types::{ApiStreamOptions, Context, StopReason, StreamOptions};
use pi_ai::{ApiKeyCredential, Credential};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_registry::{ModelRegistry, ResolvedRequestAuth};
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_js::fetch::Headers;
use pi_js::fetch::testing::{mock_fetch, response as fetch_response};
use serde_json::json;

const CLOUDFLARE_COMPAT_URL: &str =
    "https://gateway.ai.cloudflare.com/v1/test-account/test-gateway/compat/chat/completions";

#[derive(Clone)]
struct CapturedRequest {
    url: String,
    headers: Headers,
}

fn capture_sse_fetch(captured: Arc<Mutex<Vec<CapturedRequest>>>) -> pi_js::seam::Guard {
    mock_fetch(move |req| {
        let cap = captured.clone();
        Box::pin(async move {
            cap.lock().unwrap().push(CapturedRequest {
                url: req.url.clone(),
                headers: req.headers.clone(),
            });
            let chunk = pi_js::json::stringify(&json!({
                "id": "chatcmpl-test",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": "test",
                "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
            }));
            let sse = format!("data: {chunk}\n\ndata: [DONE]\n\n");
            Ok(fetch_response(200, &[("content-type", "text/event-stream")], sse))
        })
    })
}

async fn create_cloudflare_runtime() -> (ModelRuntime, ModelRegistry) {
    let auth = Arc::new(AuthStorage::in_memory(None));
    let mut env = IndexMap::new();
    env.insert("CLOUDFLARE_ACCOUNT_ID".to_owned(), "test-account".to_owned());
    env.insert("CLOUDFLARE_GATEWAY_ID".to_owned(), "test-gateway".to_owned());
    auth.modify_with(
        "cloudflare-ai-gateway",
        move |_| {
            let env = env.clone();
            async move {
                Ok(Some(Credential::ApiKey(ApiKeyCredential {
                    key: Some("test-token".to_owned()),
                    env: Some(env),
                })))
            }
        },
        None,
    )
    .await
    .unwrap();
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(auth),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();
    let registry = ModelRegistry::new(runtime.clone());
    (runtime, registry)
}

#[tokio::test]
async fn materializes_cloudflare_endpoint_through_model_runtime_streaming() {
    let (runtime, _registry) = create_cloudflare_runtime().await;
    let model = runtime
        .get_model("cloudflare-ai-gateway", "workers-ai/@cf/moonshotai/kimi-k2.6")
        .expect("cloudflare compat model");

    reset_api_providers();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let _guard = capture_sse_fetch(captured.clone());

    let result = runtime
        .complete_simple(
            &model,
            &Context {
                system_prompt: None,
                messages: Vec::new(),
                tools: None,
            },
            None,
        )
        .await;

    assert_eq!(result.stop_reason, StopReason::Stop);
    let reqs = captured.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].url, CLOUDFLARE_COMPAT_URL);
    assert_eq!(
        reqs[0].headers.get("cf-aig-authorization").as_deref(),
        Some("Bearer test-token")
    );
}

#[tokio::test]
async fn materializes_cloudflare_endpoint_after_extension_style_auth_resolution() {
    let (_runtime, registry) = create_cloudflare_runtime().await;
    let model = registry
        .find("cloudflare-ai-gateway", "workers-ai/@cf/moonshotai/kimi-k2.6")
        .expect("cloudflare compat model");

    reset_api_providers();
    let (api_key, headers, base_url, env) = match registry.get_api_key_and_headers(&model).await {
        ResolvedRequestAuth::Ok {
            api_key,
            headers,
            base_url,
            env,
        } => (api_key, headers, base_url, env),
        ResolvedRequestAuth::Err { error } => panic!("unexpected auth error: {error}"),
    };
    let hdr_map = headers.as_ref().expect("headers");
    assert_eq!(
        hdr_map.get("cf-aig-authorization").and_then(|v| v.as_deref()),
        Some("Bearer test-token")
    );
    assert_eq!(hdr_map.get("Authorization"), Some(&None));
    assert_eq!(hdr_map.get("x-api-key"), Some(&None));

    let captured = Arc::new(Mutex::new(Vec::new()));
    let _guard = capture_sse_fetch(captured.clone());
    let mut mdl = model.clone();
    if let Some(url) = base_url {
        mdl.base_url = url;
    }

    let result = complete(
        &mdl,
        &Context {
            system_prompt: None,
            messages: Vec::new(),
            tools: None,
        },
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key,
                headers,
                env,
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .await;

    assert_eq!(result.stop_reason, StopReason::Stop);
    let reqs = captured.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].url, CLOUDFLARE_COMPAT_URL);
    assert_eq!(
        reqs[0].headers.get("cf-aig-authorization").as_deref(),
        Some("Bearer test-token")
    );
    assert!(!reqs[0].headers.has("authorization"));
    assert!(!reqs[0].headers.has("x-api-key"));
}
