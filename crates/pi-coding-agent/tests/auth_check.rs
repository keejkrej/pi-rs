//! Port of packages/coding-agent/test/auth-check.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use indexmap::IndexMap;
use pi_ai::{ApiKeyCredential, Credential, CredentialStore, InMemoryModelsStore, OAuthCredential};
use pi_coding_agent::cli::args::parse_args;
use pi_coding_agent::cli::auth_check::{
    AuthCheckOptions, AuthCheckReason, AuthCheckResult, AuthCheckStatus, check_provider_auth,
    create_auth_check_model_runtime, get_provider_credential,
};
use pi_coding_agent::cli::auth_command::{AuthCommand, AuthCommandKind, parse_auth_command};
use pi_coding_agent::core::auth_storage::{AuthStorage, AuthStorageData, ReadOnlyAuthStorage};
use pi_coding_agent::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
use pretty_assertions::assert_eq;
use serde_json::json;

fn make_jwt(account_id: &str) -> String {
    let header = pi_js::b64::btoa(r#"{"alg":"none","typ":"JWT"}"#).unwrap();
    let payload_json = pi_js::json::stringify(&json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id,
        }
    }));
    let payload = pi_js::b64::btoa(&payload_json).unwrap();
    format!("{header}.{payload}.sig")
}

async fn create_runtime(credentials: Arc<dyn CredentialStore>) -> ModelRuntime {
    ModelRuntime::create(ModelRuntimeCreateOptions {
        credentials: Some(credentials),
        models_path: None,
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        allow_model_network: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .unwrap()
}

fn s(items: &[&str]) -> Vec<String> {
    items.iter().map(|v| (*v).to_string()).collect()
}

fn api_key_store(provider: &str, key: &str) -> Arc<AuthStorage> {
    let mut data: AuthStorageData = IndexMap::new();
    data.insert(
        provider.to_string(),
        Credential::ApiKey(ApiKeyCredential {
            key: Some(key.to_string()),
            env: None,
        }),
    );
    Arc::new(AuthStorage::in_memory(Some(data)))
}

#[tokio::test]
async fn reports_a_configured_provider_as_ready() {
    let runtime = create_runtime(api_key_store("openai", "test-key")).await;
    let res = check_provider_auth(&parse_args(&s(&["--provider", "openai"])), &runtime, None)
        .await
        .unwrap();
    assert_eq!(
        res,
        AuthCheckResult {
            status: AuthCheckStatus::Ready,
            provider: "openai".to_string(),
            reason: None,
            auth_type: Some("api_key".to_string()),
        }
    );
}

#[tokio::test]
async fn resolves_the_provider_from_model() {
    let runtime = create_runtime(api_key_store("openai", "test-key")).await;
    let res = check_provider_auth(&parse_args(&s(&["--model", "openai/gpt-5.5"])), &runtime, None)
        .await
        .unwrap();
    assert_eq!(
        res,
        AuthCheckResult {
            status: AuthCheckStatus::Ready,
            provider: "openai".to_string(),
            reason: None,
            auth_type: Some("api_key".to_string()),
        }
    );

    let res2 = check_provider_auth(
        &parse_args(&s(&["--provider", "openai", "--model", "gpt-5.5"])),
        &runtime,
        None,
    )
    .await
    .unwrap();
    assert_eq!(res2.status, AuthCheckStatus::Ready);
    assert_eq!(res2.provider, "openai");
}

#[tokio::test]
async fn reads_credentials_without_refreshing_oauth_when_requested() {
    let api_credentials = api_key_store("openai", "test-key");
    let api_runtime = create_runtime(api_credentials.clone()).await;
    let api_cred = get_provider_credential(
        "openai",
        &api_runtime,
        api_credentials.as_ref(),
        AuthCheckOptions { refresh: false },
    )
    .await
    .unwrap();
    assert_eq!(api_cred.as_deref(), Some("test-key"));

    let mut data: AuthStorageData = IndexMap::new();
    data.insert(
        "openai-codex".to_string(),
        Credential::OAuth(OAuthCredential::new("refresh-token", "old-token", 0)),
    );
    let credentials = Arc::new(AuthStorage::in_memory(Some(data)));
    let oauth_runtime = create_runtime(credentials.clone()).await;
    let refresh_calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = refresh_calls.clone();
    let _fetch_guard = pi_js::fetch::testing::mock_fetch(move |_req| {
        calls_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(pi_js::fetch::testing::response(500, &[], "unexpected refresh fetch")) })
    });

    let cred = get_provider_credential(
        "openai-codex",
        &oauth_runtime,
        credentials.as_ref(),
        AuthCheckOptions { refresh: false },
    )
    .await
    .unwrap();
    assert_eq!(cred.as_deref(), Some("old-token"));
    assert_eq!(refresh_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn refreshes_oauth_when_refresh_is_enabled() {
    let mut data: AuthStorageData = IndexMap::new();
    data.insert(
        "openai-codex".to_string(),
        Credential::OAuth(OAuthCredential::new("refresh-token", "old-token", 0)),
    );
    let credentials = Arc::new(AuthStorage::in_memory(Some(data)));
    let runtime = create_runtime(credentials).await;

    let fresh_jwt = make_jwt("acct_auth_check");
    let refresh_calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = refresh_calls.clone();
    let _fetch_guard = pi_js::fetch::testing::mock_fetch(move |req| {
        calls_ref.fetch_add(1, Ordering::SeqCst);
        let token = fresh_jwt.clone();
        Box::pin(async move {
            assert_eq!(req.url, "https://auth.openai.com/oauth/token");
            let body = pi_js::json::stringify(&json!({
                "access_token": token,
                "refresh_token": "refresh-token",
                "expires_in": 3600,
            }));
            Ok(pi_js::fetch::testing::response(
                200,
                &[("content-type", "application/json")],
                body,
            ))
        })
    });

    let res = check_provider_auth(
        &parse_args(&s(&["--provider", "openai-codex"])),
        &runtime,
        Some(AuthCheckOptions { refresh: true }),
    )
    .await
    .unwrap();
    assert_eq!(res.status, AuthCheckStatus::Ready);
    assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn reports_an_unknown_provider_as_not_ready() {
    let runtime = create_runtime(Arc::new(AuthStorage::in_memory(None))).await;
    let res = check_provider_auth(&parse_args(&s(&["--provider", "not-installed"])), &runtime, None)
        .await
        .unwrap();
    assert_eq!(
        res,
        AuthCheckResult {
            status: AuthCheckStatus::NotReady,
            provider: "not-installed".to_string(),
            reason: Some(AuthCheckReason::ProviderNotFound),
            auth_type: None,
        }
    );
}

#[tokio::test]
async fn does_not_treat_an_unresolved_stored_environment_reference_as_configured() {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-test-auth-check-"])).unwrap();
    let auth_path = pi_js::path::join(&[&temp_dir, "auth.json"]);
    pi_js::fs::write_file(
        &auth_path,
        pi_js::json::stringify(&json!({
            "openai": { "type": "api_key", "key": "$MISSING_AUTH_CHECK_KEY" }
        }))
        .as_bytes(),
        None,
    )
    .unwrap();
    let runtime = create_runtime(Arc::new(ReadOnlyAuthStorage::new(Some(&auth_path)))).await;
    let res = check_provider_auth(&parse_args(&s(&["--provider", "openai"])), &runtime, None)
        .await
        .unwrap();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
    assert_eq!(
        res,
        AuthCheckResult {
            status: AuthCheckStatus::NotReady,
            provider: "openai".to_string(),
            reason: Some(AuthCheckReason::CredentialsNotConfigured),
            auth_type: None,
        }
    );
}

#[tokio::test]
async fn reports_malformed_auth_state_as_invalid() {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-test-auth-check-"])).unwrap();
    let auth_path = pi_js::path::join(&[&temp_dir, "auth.json"]);
    pi_js::fs::write_file(&auth_path, b"{invalid-json", None).unwrap();
    let runtime = create_runtime(Arc::new(ReadOnlyAuthStorage::new(Some(&auth_path)))).await;
    let res = check_provider_auth(&parse_args(&s(&["--provider", "openai"])), &runtime, None)
        .await
        .unwrap();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
    assert_eq!(
        res,
        AuthCheckResult {
            status: AuthCheckStatus::Invalid,
            provider: "openai".to_string(),
            reason: Some(AuthCheckReason::InvalidState),
            auth_type: None,
        }
    );
}

#[tokio::test]
async fn does_not_create_an_auth_file_or_its_parent_directory() {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-test-auth-check-"])).unwrap();
    let agent_dir = pi_js::path::join(&[&temp_dir, "agent"]);
    let auth_path = pi_js::path::join(&[&agent_dir, "auth.json"]);
    let runtime = create_runtime(Arc::new(ReadOnlyAuthStorage::new(Some(&auth_path)))).await;

    let res = check_provider_auth(&parse_args(&s(&["--provider", "openai"])), &runtime, None)
        .await
        .unwrap();
    let auth_exists = pi_js::fs::exists(&auth_path);
    let agent_exists = pi_js::fs::exists(&agent_dir);
    let _ = pi_js::fs::rm(&temp_dir, true, true);

    assert_eq!(res.status, AuthCheckStatus::NotReady);
    assert_eq!(res.reason, Some(AuthCheckReason::CredentialsNotConfigured));
    assert!(!auth_exists);
    assert!(!agent_exists);
}

#[test]
fn accepts_optional_json_output_credential_output_and_no_refresh() {
    assert_eq!(
        parse_auth_command(&["auth", "check", "--provider", "openai"]).unwrap(),
        Some(AuthCommand {
            kind: AuthCommandKind::Check,
            args: s(&["--provider", "openai"]),
            json: false,
            credentials: false,
            no_refresh: false,
            min_expiry_ms: None,
        })
    );
    assert_eq!(
        parse_auth_command(&[
            "auth",
            "check",
            "--json",
            "--credentials",
            "--no-refresh",
            "--provider",
            "openai",
        ])
        .unwrap(),
        Some(AuthCommand {
            kind: AuthCommandKind::Check,
            args: s(&["--provider", "openai"]),
            json: true,
            credentials: true,
            no_refresh: true,
            min_expiry_ms: None,
        })
    );
}

#[tokio::test]
async fn creates_an_auth_check_runtime_without_catalog_storage() {
    let runtime = create_auth_check_model_runtime(Arc::new(AuthStorage::in_memory(None)))
        .await
        .unwrap();
    assert!(runtime.get_provider("openai").is_some());
}
