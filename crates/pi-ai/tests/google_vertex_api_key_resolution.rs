//! Port of packages/ai/test/google-vertex-api-key-resolution.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::api::google_vertex::{
    GoogleVertexOptions, resolve_vertex_request_config, stream as stream_google_vertex, testing::mock_adc_token,
};
use pi_ai::providers::google_vertex_models::google_vertex_models;
use pi_ai::utils::transcript::normalize_context;
use pi_ai::{Context, Message, Model, ProviderHeaders, StreamOptions, UserMessage};
use pi_js::env::testing::EnvGuard;
use pi_js::fetch::Headers;
use pi_js::fetch::testing::{mock_fetch, response};
use pretty_assertions::assert_eq;
use serde_json::json;

fn vertex_model() -> Model {
    google_vertex_models()
        .get("gemini-3-flash-preview")
        .expect("gemini-3-flash-preview should exist in google-vertex catalog")
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

fn pi_user_agent() -> String {
    format!(
        "pi ({} {}; {})",
        pi_js::env::platform(),
        pi_js::env::os_release(),
        pi_js::env::arch()
    )
}

fn ok_vertex_sse() -> String {
    let chunk = json!({
        "responseId": "vertex-response-id",
        "candidates": [{
            "content": { "parts": [{ "text": "ok" }] },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 1,
            "candidatesTokenCount": 1,
            "totalTokenCount": 2
        }
    });
    format!("data: {}\r\n\r\n", pi_js::json::stringify(&chunk))
}

#[tokio::test]
async fn falls_back_to_adc_when_api_key_is_placeholder_marker() {
    let _env = EnvGuard::set("GOOGLE_CLOUD_API_KEY", None);
    let _adc = mock_adc_token("adc-bearer-1");
    let captured_req: Arc<Mutex<Option<(String, Headers)>>> = Arc::new(Mutex::new(None));
    let req_slot = Arc::clone(&captured_req);
    let _fetch = mock_fetch(move |req| {
        let slot = Arc::clone(&req_slot);
        let sse = ok_vertex_sse();
        Box::pin(async move {
            *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((req.url, req.headers));
            Ok(response(200, &[("content-type", "text/event-stream")], sse))
        })
    });

    let model = vertex_model();
    let opts = GoogleVertexOptions {
        base: StreamOptions {
            api_key: Some("<authenticated>".to_owned()),
            ..Default::default()
        },
        project: Some("test-project".to_owned()),
        location: Some("us-central1".to_owned()),
        ..Default::default()
    };

    let cfg = resolve_vertex_request_config(&model, &opts).expect("config should resolve");
    assert!(cfg.vertexai);
    assert_eq!(cfg.project.as_deref(), Some("test-project"));
    assert_eq!(cfg.location.as_deref(), Some("us-central1"));
    assert_eq!(cfg.api_version, "v1");
    assert_eq!(cfg.api_key, None);

    let _ = stream_google_vertex(&model, &normalize_context(&sample_context()), Some(opts.into()))
        .result()
        .await;
    let (url, hdrs) = captured_req
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .expect("request captured");
    assert!(url.contains("/projects/test-project/locations/us-central1/"));
    assert_eq!(hdrs.get("authorization").as_deref(), Some("Bearer adc-bearer-1"));
    assert_eq!(hdrs.get("x-goog-api-key"), None);
}

#[tokio::test]
async fn falls_back_to_adc_when_api_key_is_gcp_vertex_credentials_marker() {
    let _env = EnvGuard::set("GOOGLE_CLOUD_API_KEY", None);
    let _adc = mock_adc_token("adc-bearer-2");
    let _fetch = mock_fetch(move |_req| {
        let sse = ok_vertex_sse();
        Box::pin(async move { Ok(response(200, &[("content-type", "text/event-stream")], sse)) })
    });

    let model = vertex_model();
    let opts = GoogleVertexOptions {
        base: StreamOptions {
            api_key: Some("gcp-vertex-credentials".to_owned()),
            ..Default::default()
        },
        project: Some("test-project".to_owned()),
        location: Some("us-central1".to_owned()),
        ..Default::default()
    };

    let cfg = resolve_vertex_request_config(&model, &opts).expect("config should resolve");
    assert!(cfg.vertexai);
    assert_eq!(cfg.project.as_deref(), Some("test-project"));
    assert_eq!(cfg.location.as_deref(), Some("us-central1"));
    assert_eq!(cfg.api_version, "v1");
    assert_eq!(cfg.api_key, None);

    let msg = stream_google_vertex(&model, &normalize_context(&sample_context()), Some(opts.into()))
        .result()
        .await;
    assert_eq!(msg.response_id.as_deref(), Some("vertex-response-id"));
}

#[tokio::test]
async fn falls_back_to_adc_when_google_cloud_api_key_env_is_placeholder_marker() {
    let _env = EnvGuard::set("GOOGLE_CLOUD_API_KEY", Some("<authenticated>"));
    let _adc = mock_adc_token("adc-bearer-3");
    let _fetch = mock_fetch(move |_req| {
        let sse = ok_vertex_sse();
        Box::pin(async move { Ok(response(200, &[("content-type", "text/event-stream")], sse)) })
    });

    let model = vertex_model();
    let opts = GoogleVertexOptions {
        project: Some("test-project".to_owned()),
        location: Some("us-central1".to_owned()),
        ..Default::default()
    };

    let cfg = resolve_vertex_request_config(&model, &opts).expect("config should resolve");
    assert!(cfg.vertexai);
    assert_eq!(cfg.project.as_deref(), Some("test-project"));
    assert_eq!(cfg.location.as_deref(), Some("us-central1"));
    assert_eq!(cfg.api_version, "v1");
    assert_eq!(cfg.api_key, None);

    let msg = stream_google_vertex(&model, &normalize_context(&sample_context()), Some(opts.into()))
        .result()
        .await;
    assert_eq!(msg.response_id.as_deref(), Some("vertex-response-id"));
}

#[tokio::test]
async fn still_uses_api_key_client_for_real_api_keys() {
    let _env = EnvGuard::set("GOOGLE_CLOUD_API_KEY", None);
    let captured_req: Arc<Mutex<Option<(String, Headers)>>> = Arc::new(Mutex::new(None));
    let req_slot = Arc::clone(&captured_req);
    let _fetch = mock_fetch(move |req| {
        let slot = Arc::clone(&req_slot);
        let sse = ok_vertex_sse();
        Box::pin(async move {
            *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((req.url, req.headers));
            Ok(response(200, &[("content-type", "text/event-stream")], sse))
        })
    });

    let model = vertex_model();
    let real_key = "AIzaSyExampleRealisticLookingApiKey123456";
    let opts = GoogleVertexOptions {
        base: StreamOptions {
            api_key: Some(real_key.to_owned()),
            ..Default::default()
        },
        ..Default::default()
    };

    let cfg = resolve_vertex_request_config(&model, &opts).expect("config should resolve");
    assert!(cfg.vertexai);
    assert_eq!(cfg.api_key.as_deref(), Some(real_key));
    assert_eq!(cfg.api_version, "v1");
    assert_eq!(cfg.project, None);
    assert_eq!(cfg.location, None);

    let _ = stream_google_vertex(&model, &normalize_context(&sample_context()), Some(opts.into()))
        .result()
        .await;
    let (url, hdrs) = captured_req
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .expect("request captured");
    assert!(!url.contains("/projects/"));
    assert_eq!(hdrs.get("x-goog-api-key").as_deref(), Some(real_key));
}

#[test]
fn does_not_forward_generated_vertex_base_url_placeholders_and_allows_user_agent_override() {
    let model = vertex_model();
    assert!(
        model.base_url.contains("{location}"),
        "catalog model base_url expected to contain placeholder: {}",
        model.base_url
    );

    let default_opts = GoogleVertexOptions {
        project: Some("test-project".to_owned()),
        location: Some("us-central1".to_owned()),
        ..Default::default()
    };
    let default_cfg = resolve_vertex_request_config(&model, &default_opts).expect("default config");
    assert_eq!(default_cfg.base_url, None);
    let mut expected_default = IndexMap::new();
    expected_default.insert("User-Agent".to_owned(), pi_user_agent());
    assert_eq!(default_cfg.headers, expected_default);

    let mut overrides = ProviderHeaders::new();
    overrides.insert("User-Agent".to_owned(), Some("custom-agent".to_owned()));
    let custom_opts = GoogleVertexOptions {
        base: StreamOptions {
            headers: Some(overrides),
            ..Default::default()
        },
        project: Some("test-project".to_owned()),
        location: Some("us-central1".to_owned()),
        ..Default::default()
    };
    let custom_cfg = resolve_vertex_request_config(&model, &custom_opts).expect("custom config");
    let mut expected_custom = IndexMap::new();
    expected_custom.insert("User-Agent".to_owned(), "custom-agent".to_owned());
    assert_eq!(custom_cfg.headers, expected_custom);
}

#[test]
fn forwards_custom_base_url_to_adc_and_api_key_clients_and_omits_duplicate_api_version() {
    let mut custom_model = vertex_model();
    custom_model.base_url = "https://proxy.example.com".to_owned();

    let adc_cfg = resolve_vertex_request_config(
        &custom_model,
        &GoogleVertexOptions {
            project: Some("test-project".to_owned()),
            location: Some("us-central1".to_owned()),
            ..Default::default()
        },
    )
    .expect("adc custom base url config");
    assert!(adc_cfg.vertexai);
    assert_eq!(adc_cfg.project.as_deref(), Some("test-project"));
    assert_eq!(adc_cfg.location.as_deref(), Some("us-central1"));
    assert_eq!(adc_cfg.api_version, "v1");
    assert_eq!(adc_cfg.base_url.as_deref(), Some("https://proxy.example.com"));
    assert_eq!(adc_cfg.base_url_resource_scope, Some("COLLECTION"));

    let key_cfg = resolve_vertex_request_config(
        &custom_model,
        &GoogleVertexOptions {
            base: StreamOptions {
                api_key: Some("AIzaSyExampleRealisticLookingApiKey123456".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .expect("api key custom base url config");
    assert!(key_cfg.vertexai);
    assert_eq!(
        key_cfg.api_key.as_deref(),
        Some("AIzaSyExampleRealisticLookingApiKey123456")
    );
    assert_eq!(key_cfg.api_version, "v1");
    assert_eq!(key_cfg.base_url.as_deref(), Some("https://proxy.example.com"));
    assert_eq!(key_cfg.base_url_resource_scope, Some("COLLECTION"));

    custom_model.base_url = "https://proxy.example.com/v1/projects/test-project/locations/global".to_owned();
    let versioned_cfg = resolve_vertex_request_config(
        &custom_model,
        &GoogleVertexOptions {
            project: Some("test-project".to_owned()),
            location: Some("us-central1".to_owned()),
            ..Default::default()
        },
    )
    .expect("versioned custom base url config");
    assert_eq!(
        versioned_cfg.base_url.as_deref(),
        Some("https://proxy.example.com/v1/projects/test-project/locations/global")
    );
    assert_eq!(versioned_cfg.base_url_resource_scope, Some("COLLECTION"));
    assert_eq!(versioned_cfg.api_version, "");
}
