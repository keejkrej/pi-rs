//! Port of packages/coding-agent/test/config-value-migration.test.ts

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use pi_coding_agent::config::ENV_AGENT_DIR;
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::migrations::run_migrations;
use pi_js::env::testing::EnvGuard;
use serde_json::{Value, json};
use tempfile::TempDir;

use support::model_runtime_test_utils::create_model_registry;

#[test]
fn leaves_uppercase_auth_json_api_key_values_unchanged() {
    let dir = TempDir::new().expect("temp dir");
    let agent_dir = dir.path().to_str().expect("utf8");
    let auth_path = dir.path().join("auth.json");
    let payload = json!({
        "anthropic": { "type": "api_key", "key": "ANTHROPIC_API_KEY" },
        "openai": { "type": "api_key", "key": "$OPENAI_API_KEY" },
        "opencode": { "type": "api_key", "key": "public" },
        "github": { "type": "oauth", "access": "ACCESS_TOKEN", "refresh": "REFRESH_TOKEN", "expires": 1 }
    });
    std::fs::write(
        &auth_path,
        format!("{}\n", pi_js::json::stringify_pretty(&payload, "  ")),
    )
    .expect("write auth.json");

    {
        let _guard = EnvGuard::set(&ENV_AGENT_DIR, Some(agent_dir));
        let res = run_migrations(agent_dir);
        assert!(res.migrated_auth_providers.is_empty());
        assert!(res.deprecation_warnings.is_empty());
    }

    let raw = std::fs::read_to_string(&auth_path).expect("read auth.json");
    let migrated: Value = pi_js::json::parse(&raw).expect("parse auth.json");
    assert_eq!(migrated["anthropic"]["key"], "ANTHROPIC_API_KEY");
    assert_eq!(migrated["openai"]["key"], "$OPENAI_API_KEY");
    assert_eq!(migrated["opencode"]["key"], "public");
    assert_eq!(migrated["github"]["access"], "ACCESS_TOKEN");
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_throw_on_malformed_or_blank_models_json_during_migrations() {
    for (_name, content) in [("malformed", "{\n  \"providers\": {\n"), ("blank", "")] {
        let dir = TempDir::new().expect("temp dir");
        let agent_dir = dir.path().to_str().expect("utf8");
        let models_path = dir.path().join("models.json");
        let auth_path = dir.path().join("auth.json");
        std::fs::write(&models_path, content).expect("write models.json");

        {
            let _guard = EnvGuard::set(&ENV_AGENT_DIR, Some(agent_dir));
            let _ = run_migrations(agent_dir);
        }

        assert_eq!(
            std::fs::read_to_string(&models_path).expect("read models.json"),
            content
        );

        let registry = create_model_registry(
            Arc::new(AuthStorage::create(Some(auth_path.to_str().expect("utf8")))),
            Some(models_path.to_str().expect("utf8")),
        )
        .await
        .expect("create registry");
        let load_error = registry.get_error().expect("expected load error");
        assert!(load_error.contains("Failed to parse models.json"), "{load_error}");
        assert!(
            load_error.contains(&format!("File: {}", models_path.to_str().expect("utf8"))),
            "{load_error}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn leaves_uppercase_models_json_api_key_and_header_values_unchanged() {
    let dir = TempDir::new().expect("temp dir");
    let agent_dir = dir.path().to_str().expect("utf8");
    let models_path = dir.path().join("models.json");
    let auth_path = dir.path().join("auth.json");

    let _g1 = EnvGuard::set("CUSTOM_API_KEY", Some("env-CUSTOM_API_KEY"));
    let _g2 = EnvGuard::set("HEADER_API_KEY", Some("env-HEADER_API_KEY"));
    let _g3 = EnvGuard::set("MODEL_API_KEY", Some("env-MODEL_API_KEY"));
    let _g4 = EnvGuard::set("OVERRIDE_API_KEY", Some("env-OVERRIDE_API_KEY"));

    let payload = json!({
        "providers": {
            "custom-provider": {
                "baseUrl": "https://example.com/v1",
                "apiKey": "CUSTOM_API_KEY",
                "api": "openai-completions",
                "headers": {
                    "x-api-key": "HEADER_API_KEY",
                    "x-literal": "literal"
                },
                "models": [
                    {
                        "id": "model-a",
                        "headers": { "x-model-key": "MODEL_API_KEY" }
                    }
                ],
                "modelOverrides": {
                    "model-b": { "headers": { "x-override-key": "OVERRIDE_API_KEY" } }
                }
            }
        }
    });
    std::fs::write(
        &models_path,
        format!("{}\n", pi_js::json::stringify_pretty(&payload, "  ")),
    )
    .expect("write models.json");

    {
        let _guard = EnvGuard::set(&ENV_AGENT_DIR, Some(agent_dir));
        let res = run_migrations(agent_dir);
        assert!(res.migrated_auth_providers.is_empty());
        assert!(res.deprecation_warnings.is_empty());
    }

    let migrated: Value = pi_js::json::parse(&std::fs::read_to_string(&models_path).expect("read")).expect("parse");
    let provider = &migrated["providers"]["custom-provider"];
    assert_eq!(provider["apiKey"], "CUSTOM_API_KEY");
    assert_eq!(provider["headers"]["x-api-key"], "HEADER_API_KEY");
    assert_eq!(provider["headers"]["x-literal"], "literal");
    assert_eq!(provider["models"][0]["headers"]["x-model-key"], "MODEL_API_KEY");
    assert_eq!(
        provider["modelOverrides"]["model-b"]["headers"]["x-override-key"],
        "OVERRIDE_API_KEY"
    );

    let registry = create_model_registry(
        Arc::new(AuthStorage::create(Some(auth_path.to_str().expect("utf8")))),
        Some(models_path.to_str().expect("utf8")),
    )
    .await
    .expect("create registry");
    let model = registry.find("custom-provider", "model-a").expect("model-a found");
    assert_eq!(
        registry.get_api_key_for_provider("custom-provider").await.as_deref(),
        Some("CUSTOM_API_KEY")
    );
    let auth_and_headers = registry.get_api_key_and_headers(&model).await.expect("headers");
    let expected_headers = IndexMap::from([
        ("x-api-key".to_owned(), "HEADER_API_KEY".to_owned()),
        ("x-literal".to_owned(), "literal".to_owned()),
        ("x-model-key".to_owned(), "MODEL_API_KEY".to_owned()),
    ]);
    assert!(auth_and_headers.ok);
    assert_eq!(auth_and_headers.api_key.as_deref(), Some("CUSTOM_API_KEY"));
    assert_eq!(auth_and_headers.headers, Some(expected_headers));
}
