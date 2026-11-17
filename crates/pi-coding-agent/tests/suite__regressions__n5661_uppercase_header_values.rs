//! Port of packages/coding-agent/test/suite/regressions/5661-uppercase-header-values.test.ts

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::ProviderHeaders;
use pi_coding_agent::config::ENV_AGENT_DIR;
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_registry::ResolvedRequestAuth;
use pi_coding_agent::migrations::run_migrations;
use pi_js::env::testing::EnvGuard;
use serde_json::{Value, json};

use support::model_runtime_test_utils::create_model_registry;
use support::test_harness::{HarnessOptions, create_harness};

#[tokio::test(flavor = "current_thread")]
async fn keeps_uppercase_header_strings_as_literals_during_startup_migrations() {
    let harness = create_harness(Some(HarnessOptions::default())).await.expect("harness");
    let _k1 = EnvGuard::set("CUSTOM_API_KEY", Some("env-CUSTOM_API_KEY"));
    let _k2 = EnvGuard::set("BEARER", Some("env-BEARER"));

    let models_path = pi_js::path::join(&[&harness.temp_dir, "models.json"]);
    let auth_path = pi_js::path::join(&[&harness.temp_dir, "auth.json"]);
    let payload = json!({
        "providers": {
            "my-provider": {
                "baseUrl": "https://example.com/v1",
                "apiKey": "CUSTOM_API_KEY",
                "api": "openai-completions",
                "headers": { "Authorization": "BEARER" },
                "models": [{ "id": "my-model" }]
            }
        }
    });
    std::fs::write(
        &models_path,
        format!("{}\n", pi_js::json::stringify_pretty(&payload, "  ")),
    )
    .expect("write models.json");

    {
        let _agent_dir = EnvGuard::set(&ENV_AGENT_DIR, Some(&harness.temp_dir));
        let _ = run_migrations(&harness.temp_dir);
    }

    let migrated: Value = pi_js::json::parse(&std::fs::read_to_string(&models_path).expect("read")).expect("parse");
    assert_eq!(migrated["providers"]["my-provider"]["apiKey"], json!("CUSTOM_API_KEY"));
    assert_eq!(
        migrated["providers"]["my-provider"]["headers"]["Authorization"],
        json!("BEARER")
    );

    let registry = create_model_registry(Arc::new(AuthStorage::create(Some(&auth_path))), Some(&models_path))
        .await
        .expect("create registry");
    let model = registry.find("my-provider", "my-model").expect("my-model found");
    let expected_headers: ProviderHeaders = IndexMap::from([("Authorization".to_owned(), Some("BEARER".to_owned()))]);
    match registry.get_api_key_and_headers(&model).await {
        ResolvedRequestAuth::Ok { api_key, headers, .. } => {
            assert_eq!(api_key.as_deref(), Some("CUSTOM_API_KEY"));
            assert_eq!(headers, Some(expected_headers));
        }
        ResolvedRequestAuth::Err { error } => panic!("unexpected auth error: {error}"),
    }

    harness.cleanup();
}
