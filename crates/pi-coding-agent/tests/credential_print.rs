//! Port of packages/coding-agent/test/credential-print.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use indexmap::IndexMap;
use pi_ai::{ApiKeyCredential, Credential, CredentialStore, InMemoryModelsStore, OAuthCredential};
use pi_coding_agent::cli::args::parse_args;
use pi_coding_agent::cli::auth_command::{
    AuthCommand, AuthCommandKind, is_auth_command_help, parse_auth_command,
};
use pi_coding_agent::cli::credential_print::resolve_credential_for_print;
use pi_coding_agent::core::auth_storage::{AuthStorage, AuthStorageData};
use pi_coding_agent::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
use pi_coding_agent::main_::main;
use pretty_assertions::assert_eq;
use serde_json::json;

fn make_jwt(account_id: &str) -> String {
    let header = pi_js::b64::btoa(r#"{"alg":"none","typ":"JWT"}"#);
    let payload_json = pi_js::json::stringify(&json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id,
        }
    }));
    let payload = pi_js::b64::btoa(&payload_json);
    format!("{header}.{payload}.sig")
}

async fn create_runtime(credentials: Arc<AuthStorage>) -> ModelRuntime {
    ModelRuntime::create(ModelRuntimeCreateOptions {
        credentials: Some(credentials),
        models_path: None,
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap()
}

fn s(items: &[&str]) -> Vec<String> {
    items.iter().map(|v| (*v).to_string()).collect()
}

#[tokio::test]
async fn prints_a_resolved_api_key() {
    let mut data: AuthStorageData = IndexMap::new();
    data.insert(
        "openai".to_string(),
        Credential::ApiKey(ApiKeyCredential {
            key: Some("test-api-key".to_string()),
            env: None,
        }),
    );
    let runtime = create_runtime(Arc::new(AuthStorage::in_memory(Some(data)))).await;
    let args = parse_args(&s(&["--provider", "openai"]));

    let resolved = resolve_credential_for_print(&args, &runtime, AuthCommandKind::ApiKey, None, None)
        .await
        .unwrap();
    assert_eq!(resolved, "test-api-key");
}

#[tokio::test]
async fn prints_bearer_tokens_resolved_from_an_authorization_header() {
    let mut data: AuthStorageData = IndexMap::new();
    data.insert(
        "kimi-coding".to_string(),
        Credential::OAuth(OAuthCredential::new(
            "test-refresh-token",
            "header-test-token",
            pi_js::time::now_ms() + 60 * 60 * 1000,
        )),
    );
    let runtime = create_runtime(Arc::new(AuthStorage::in_memory(Some(data)))).await;
    let args = parse_args(&s(&["--provider", "kimi-coding"]));

    let resolved = resolve_credential_for_print(&args, &runtime, AuthCommandKind::BearerToken, None, None)
        .await
        .unwrap();
    assert_eq!(resolved, "header-test-token");
}

#[tokio::test]
async fn refreshes_an_expired_oauth_token_before_printing_it() {
    let mut data: AuthStorageData = IndexMap::new();
    data.insert(
        "openai-codex".to_string(),
        Credential::OAuth(OAuthCredential::new("test-refresh-token", "old-test-token", 0)),
    );
    let storage = Arc::new(AuthStorage::in_memory(Some(data)));
    let runtime = create_runtime(storage.clone()).await;

    let fresh_token = make_jwt("acct_cred_print");
    let expected_token = fresh_token.clone();
    let refresh_calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = refresh_calls.clone();
    let _fetch_guard = pi_js::fetch::testing::mock_fetch(move |req| {
        calls_ref.fetch_add(1, Ordering::SeqCst);
        let token = fresh_token.clone();
        Box::pin(async move {
            assert_eq!(req.url, "https://auth.openai.com/oauth/token");
            let body = pi_js::json::stringify(&json!({
                "access_token": token,
                "refresh_token": "test-refresh-token",
                "expires_in": 3600,
            }));
            Ok(pi_js::fetch::testing::response(
                200,
                &[("content-type", "application/json")],
                body,
            ))
        })
    });

    let args = parse_args(&s(&["--provider", "openai-codex"]));
    let resolved = resolve_credential_for_print(&args, &runtime, AuthCommandKind::BearerToken, None, None)
        .await
        .unwrap();
    assert_eq!(resolved, expected_token);
    assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);

    let stored = storage.read("openai-codex", None).await.unwrap().unwrap();
    assert_eq!(stored.as_oauth().map(|c| c.access.as_str()), Some(expected_token.as_str()));
}

#[tokio::test]
async fn reports_unknown_auth_options_like_package_commands() {
    let capture = pi_js::console::testing::capture();
    let err = main(
        &s(&["auth", "check", "--provider", "openai-codex", "--credentails"]),
        None,
    )
    .await
    .unwrap_err();
    let stderr = capture.stderr();
    assert!(matches!(err, pi_js::Error::Exit(1)));
    assert!(stderr.contains("Unknown option --credentails for \"auth check\"."));
    assert!(stderr.contains(
        "Use \"pi --help\" or \"pi auth check --provider <provider> [--json] [--credentials] [--no-refresh]\"."
    ));
}

#[tokio::test]
async fn parses_credential_commands_and_rejects_invalid_arguments_or_credential_types() {
    let mut data: AuthStorageData = IndexMap::new();
    data.insert(
        "openai-codex".to_string(),
        Credential::OAuth(OAuthCredential::new(
            "test-refresh-token",
            "test-token-not-to-be-printed",
            pi_js::time::now_ms() + 60 * 60 * 1000,
        )),
    );
    let runtime = create_runtime(Arc::new(AuthStorage::in_memory(Some(data)))).await;

    assert_eq!(
        parse_auth_command(&["auth", "print-api-key", "--provider", "openai"]).unwrap(),
        Some(AuthCommand {
            kind: AuthCommandKind::ApiKey,
            args: s(&["--provider", "openai"]),
            json: false,
            credentials: false,
            no_refresh: false,
            min_expiry_ms: None,
        })
    );
    let bearer = parse_auth_command(&["auth", "print-bearer-token"]).unwrap().unwrap();
    assert_eq!(bearer.kind, AuthCommandKind::BearerToken);

    assert_eq!(
        parse_auth_command(&["auth", "print-bearer-token", "--min-expiry", "30m"]).unwrap(),
        Some(AuthCommand {
            kind: AuthCommandKind::BearerToken,
            args: vec![],
            json: false,
            credentials: false,
            no_refresh: false,
            min_expiry_ms: Some(30 * 60_000),
        })
    );

    let err = parse_auth_command(&["auth", "print-api-key", "--min-expiry", "30m"]).unwrap_err();
    assert!(err.to_string().contains("only supported by print-bearer-token"));

    assert!(is_auth_command_help(&["auth", "--help"]));
    assert!(is_auth_command_help(&["auth", "print-api-key", "--help"]));
    assert!(is_auth_command_help(&["auth", "print-bearer-token", "-h"]));
    assert!(is_auth_command_help(&["auth", "check", "--help"]));

    let unknown_err = parse_auth_command(&["auth", "unknown"]).unwrap_err();
    assert!(unknown_err.to_string().contains("Unknown auth command"));

    let missing_err = resolve_credential_for_print(&parse_args(&[]), &runtime, AuthCommandKind::ApiKey, None, None)
        .await
        .unwrap_err();
    assert!(
        missing_err
            .to_string()
            .contains("requires --provider <provider> or --model <model>")
    );

    let oauth_err = resolve_credential_for_print(
        &parse_args(&s(&["--provider", "openai-codex"])),
        &runtime,
        AuthCommandKind::ApiKey,
        None,
        None,
    )
    .await
    .unwrap_err();
    assert!(oauth_err.to_string().contains("configured with OAuth"));
}
