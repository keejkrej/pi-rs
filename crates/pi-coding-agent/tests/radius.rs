//! Port of packages/coding-agent/test/radius.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::auth::types::{Credential, OAuthCredential};
use pi_ai::models_store::{InMemoryModelsStore, ModelsStore};
use pi_coding_agent::core::auth_storage::{AuthStorage, AuthStorageData};
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_coding_agent::core::radius::RADIUS_PROVIDER_ID;
use pi_js::fetch::Request;
use pi_js::fetch::testing::{deny_network, mock_fetch, response as fetch_response};
use pi_js::fs::write_file as js_write_file;
use pi_js::json::stringify;
use pi_js::time::now_ms;
use serde_json::{Value, json};
use tempfile::TempDir;

fn radius_config(base_url: &str) -> Value {
    json!({
        "baseUrl": base_url,
        "models": [
            {
                "id": "auto",
                "name": "Radius Auto",
                "reasoning": false,
                "input": ["text"],
                "cost": { "input": 1, "output": 2, "cacheRead": 0.1, "cacheWrite": 0.2 },
                "contextWindow": 128000,
                "maxTokens": 16384
            }
        ]
    })
}

fn radius_oauth_credential(gateway_base_url: Option<&str>) -> Credential {
    let mut extra = IndexMap::new();
    if let Some(base_url) = gateway_base_url {
        extra.insert("gatewayConfig".to_owned(), radius_config(base_url));
    }
    Credential::OAuth(OAuthCredential {
        refresh: "refresh-token".to_owned(),
        access: "access-token".to_owned(),
        expires: now_ms() + 60 * 60 * 1000,
        extra,
    })
}

#[tokio::test]
async fn restores_legacy_credential_catalog_without_network_access() {
    let _deny = deny_network();
    let mut data: AuthStorageData = IndexMap::new();
    data.insert(
        RADIUS_PROVIDER_ID.to_owned(),
        radius_oauth_credential(Some("https://radius.example.com/v1")),
    );

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(Some(data)))),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        models_path: Some(None),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    let model = runtime.get_model(RADIUS_PROVIDER_ID, "auto").unwrap();
    assert_eq!(model.api, "pi-messages");
    assert_eq!(model.base_url, "https://radius.example.com/v1");
    assert_eq!(runtime.get_provider(RADIUS_PROVIDER_ID).unwrap().name(), "Radius");
    assert!(runtime.has_configured_auth(RADIUS_PROVIDER_ID));
}

#[tokio::test]
async fn fetches_and_stores_catalog_for_configured_radius_auth() {
    let recorded: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
    let rec_clone = recorded.clone();
    let _guard = mock_fetch(move |req| {
        rec_clone.lock().unwrap().push(req);
        let body = stringify(&radius_config("https://radius.example.com/v1"));
        Box::pin(async move { Ok(fetch_response(200, &[("content-type", "application/json")], body)) })
    });

    let models_store = Arc::new(InMemoryModelsStore::new());
    let mut data: AuthStorageData = IndexMap::new();
    data.insert(RADIUS_PROVIDER_ID.to_owned(), radius_oauth_credential(None));

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(Some(data)))),
        models_store: Some(models_store.clone()),
        models_path: Some(None),
        allow_model_network: Some(true),
        ..Default::default()
    })
    .await
    .unwrap();

    assert!(runtime.get_model(RADIUS_PROVIDER_ID, "auto").is_some());
    let stored = models_store.read(RADIUS_PROVIDER_ID, None).await.unwrap().unwrap();
    assert_eq!(stored.models.len(), 1);

    let calls = recorded.lock().unwrap();
    let radius_req = calls
        .iter()
        .find(|r| r.url == "https://radius.pi.dev/v1/config")
        .expect("expected radius config request");
    assert_eq!(
        radius_req.headers.get("authorization").as_deref(),
        Some("Bearer access-token")
    );
}

#[tokio::test]
async fn does_not_refresh_catalogs_over_network_by_default() {
    let _deny = deny_network();
    let mut data: AuthStorageData = IndexMap::new();
    data.insert(
        RADIUS_PROVIDER_ID.to_owned(),
        radius_oauth_credential(Some("https://radius.example.com/v1")),
    );

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(Some(data)))),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();

    assert!(runtime.get_model(RADIUS_PROVIDER_ID, "auto").is_some());
}

#[tokio::test]
async fn does_not_fetch_or_make_radius_models_available_without_configured_auth() {
    let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let rec_clone = recorded.clone();
    let _guard = mock_fetch(move |req| {
        rec_clone.lock().unwrap().push(req.url.clone());
        Box::pin(async move { Ok(fetch_response(404, &[], "not found")) })
    });

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        models_path: Some(None),
        allow_model_network: Some(true),
        ..Default::default()
    })
    .await
    .unwrap();

    let radius_available: Vec<_> = runtime
        .get_available_snapshot()
        .into_iter()
        .filter(|m| m.provider == RADIUS_PROVIDER_ID)
        .collect();
    assert!(radius_available.is_empty());
    let fetched = recorded.lock().unwrap();
    assert!(!fetched.iter().any(|u| u.contains("radius.pi.dev/v1/config")));
}

#[tokio::test]
async fn supports_custom_radius_gateways_from_models_json() {
    let _guard = mock_fetch(move |_req| {
        let body = stringify(&radius_config("http://localhost:8788/v1"));
        Box::pin(async move { Ok(fetch_response(200, &[("content-type", "application/json")], body)) })
    });

    let dir = TempDir::new().unwrap();
    let models_path = dir.path().join("models.json");
    let models_path_str = models_path.to_str().unwrap();
    let raw = json!({
        "providers": {
            "radius-dev": {
                "name": "Radius (dev)",
                "baseUrl": "http://localhost:8788",
                "oauth": "radius"
            }
        }
    });
    js_write_file(models_path_str, stringify(&raw), None).unwrap();

    let mut data: AuthStorageData = IndexMap::new();
    data.insert("radius-dev".to_owned(), radius_oauth_credential(None));

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(Some(data)))),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        models_path: Some(Some(models_path_str.to_owned())),
        allow_model_network: Some(true),
        ..Default::default()
    })
    .await
    .unwrap();

    let model = runtime.get_model("radius-dev", "auto").unwrap();
    assert_eq!(model.api, "pi-messages");
    assert_eq!(model.base_url, "http://localhost:8788/v1");
    assert_eq!(runtime.get_provider("radius-dev").unwrap().name(), "Radius (dev)");
}

#[tokio::test]
async fn requires_base_url_for_custom_radius_gateways() {
    let dir = TempDir::new().unwrap();
    let models_path = dir.path().join("models.json");
    let models_path_str = models_path.to_str().unwrap();
    let raw = json!({
        "providers": {
            "radius-dev": {
                "oauth": "radius"
            }
        }
    });
    js_write_file(models_path_str, stringify(&raw), None).unwrap();

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        models_path: Some(Some(models_path_str.to_owned())),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    assert!(
        runtime
            .get_error()
            .unwrap_or_default()
            .contains("\"baseUrl\" is required when \"oauth\" is set")
    );
}
