//! Port of packages/coding-agent/test/suite/regressions/6999-models-json-hot-reload.test.ts
//!
//! // WIRE(@tui-w3): instantiate `ModelSelectorComponent` and verify UI re-render on `/model` open
//! // once `modes/interactive/components/model_selector.rs` lands.

use std::fs;
use std::sync::Arc;

use pi_ai::auth::credential_store::InMemoryCredentialStore;
use pi_ai::models::ModelsRefreshOptions;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use serde_json::{Value, json};
use tempfile::TempDir;

fn models_json(provider: &str, model: &str) -> Value {
    json!({
        "providers": {
            provider: {
                "baseUrl": "https://example.test/v1",
                "api": "openai-completions",
                "apiKey": "test-key",
                "models": [{ "id": model }]
            }
        }
    })
}

#[tokio::test]
async fn reloads_models_json_when_refreshing_runtime_catalog() {
    let temp_dir: TempDir = tempfile::tempdir().expect("create tempdir");
    let models_path = temp_dir.path().join("models.json");
    let models_path_str = models_path.to_string_lossy().into_owned();

    fs::write(
        &models_path,
        pi_js::json::stringify(&models_json("old-provider", "old-model")),
    )
    .expect("write initial models.json");

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(InMemoryCredentialStore::new())),
        models_path: Some(Some(models_path_str)),
        allow_model_network: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .expect("create runtime");

    assert!(runtime.get_model("old-provider", "old-model").is_some());

    fs::write(
        &models_path,
        pi_js::json::stringify(&models_json("new-provider", "new-model")),
    )
    .expect("overwrite models.json");

    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            force: Some(true),
            providers: None,
            signal: None,
        }))
        .await
        .expect("refresh runtime");

    assert!(runtime.get_model("new-provider", "new-model").is_some());
    assert!(runtime.get_model("old-provider", "old-model").is_none());
    let available = runtime.get_available_snapshot();
    assert!(
        available
            .iter()
            .any(|m| m.provider == "new-provider" && m.id == "new-model")
    );
    assert!(
        !available
            .iter()
            .any(|m| m.provider == "old-provider" && m.id == "old-model")
    );
}
