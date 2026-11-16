//! Port of packages/ai/test/radius-provider.test.ts

use std::sync::Arc;

use serde_json::json;

use pi_ai::auth::credential_store::InMemoryCredentialStore;
use pi_ai::auth::types::ApiKeyCredential;
use pi_ai::auth::types::Credential;
use pi_ai::models::{CreateModelsOptions, ModelsRefreshOptions, create_models};
use pi_ai::models_store::{InMemoryModelsStore, ModelsStore, ModelsStoreEntry};
use pi_ai::providers::radius::{RadiusProviderOptions, radius_provider, radius_provider_with_options};
use pi_ai::providers::radius_config::{RadiusGatewayConfig, RadiusGatewayModel, get_radius_models_from_config};
use pi_ai::types::{AnyModel, ModelCost, ModelModality};
use pi_js::fetch::testing::{mock_fetch, response};

fn radius_config() -> RadiusGatewayConfig {
    RadiusGatewayConfig {
        base_url: "https://radius.example/v1".to_owned(),
        models: vec![
            RadiusGatewayModel {
                id: "balanced".to_owned(),
                name: "Fresh Balanced".to_owned(),
                reasoning: true,
                thinking_level_map: None,
                input: vec![ModelModality::Text],
                cost: ModelCost {
                    input: 1.0,
                    output: 2.0,
                    cache_read: 0.1,
                    cache_write: 0.0,
                    tiers: None,
                },
                context_window: 424_242,
                max_tokens: 32_000,
            },
            RadiusGatewayModel {
                id: "organization-only".to_owned(),
                name: "Organization Only".to_owned(),
                reasoning: false,
                thinking_level_map: None,
                input: vec![ModelModality::Text],
                cost: ModelCost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    tiers: None,
                },
                context_window: 128_000,
                max_tokens: 16_000,
            },
        ],
    }
}

#[test]
fn ships_a_static_public_catalog_for_the_default_gateway() {
    let provider = radius_provider();
    let models = provider.get_models();
    assert!(!models.is_empty());
    let balanced = models
        .iter()
        .find(|m| m.id == "balanced")
        .expect("missing balanced model");
    assert_eq!(balanced.provider, "radius");
    assert_eq!(balanced.api, "pi-messages");
}

#[test]
fn does_not_apply_the_public_radius_catalog_to_custom_gateways() {
    let provider = radius_provider_with_options(RadiusProviderOptions {
        id: Some("radius-dev".to_owned()),
        name: None,
        gateway: Some("http://localhost:8788".to_owned()),
    });
    assert!(provider.get_models().is_empty());
}

#[tokio::test]
async fn overlays_refreshed_models_on_the_static_public_catalog() {
    let cfg = radius_config();
    let cfg_len = cfg.models.len();
    let body = pi_js::json::stringify(&cfg);
    let _guard = mock_fetch(move |_req| {
        let body = body.clone();
        Box::pin(async move { Ok(response(200, &[("content-type", "application/json")], body)) })
    });

    let credentials = Arc::new(InMemoryCredentialStore::new());
    credentials
        .modify_with(
            "radius",
            |_| async {
                Ok(Some(Credential::ApiKey(ApiKeyCredential {
                    key: Some("radius-key".to_owned()),
                    env: None,
                })))
            },
            None,
        )
        .await
        .unwrap();

    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials as Arc<dyn pi_ai::auth::types::CredentialStore + Send + Sync>),
        ..Default::default()
    }));
    models.set_provider(radius_provider());

    let result = models
        .refresh(Some(ModelsRefreshOptions {
            providers: Some(vec!["radius".to_owned()]),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert!(result.errors.is_empty());
    let balanced = models.get_model("radius", "balanced").expect("balanced model");
    assert_eq!(balanced.name, "Fresh Balanced");
    assert_eq!(balanced.base_url, "https://radius.example/v1");
    assert_eq!(balanced.context_window, 424_242);
    assert!(models.get_model("radius", "organization-only").is_some());
    assert!(models.get_models(Some("radius")).len() > cfg_len);
    let _ = json!({});
}

#[tokio::test]
async fn overlays_a_cached_effective_catalog_without_network_access() {
    let store = Arc::new(InMemoryModelsStore::new());
    let cached_models: Vec<AnyModel> = get_radius_models_from_config("radius", &radius_config())
        .into_iter()
        .map(AnyModel::Chat)
        .collect();
    store
        .write(
            "radius",
            ModelsStoreEntry {
                models: cached_models,
                checked_at: Some(pi_js::time::now_ms()),
                last_modified: None,
                etag: None,
            },
            None,
        )
        .await
        .unwrap();

    let models = create_models(Some(CreateModelsOptions {
        models_store: Some(store as Arc<dyn ModelsStore + Send + Sync>),
        ..Default::default()
    }));
    models.set_provider(radius_provider());

    models
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["radius".to_owned()]),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert_eq!(
        models.get_model("radius", "balanced").map(|m| m.name).as_deref(),
        Some("Fresh Balanced")
    );
    assert!(models.get_model("radius", "organization-only").is_some());
}
