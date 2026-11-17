//! Port of packages/coding-agent/test/mcp-oauth-store.test.ts

use std::sync::Arc;

use pi_coding_agent::extensions::mcp::oauth::McpOAuthCredentialStore;
// WIRE(@mcp-w3): switch to crate::core::auth_storage::{AuthStorageBackend, InMemoryAuthStorageBackend, auth_with_lock} once landed
use pi_coding_agent::extensions::wire::{AuthStorageBackend, InMemoryAuthStorageBackend, auth_with_lock};
use pi_mcp::oauth::{McpOAuthState, OAuthTokens};
use serde_json::{Value, json};

const SERVER_URL: &str = "https://mcp.example.com/mcp";

fn state(access_token: &str) -> McpOAuthState {
    McpOAuthState {
        server_url: SERVER_URL.to_string(),
        client_information: None,
        tokens: Some(OAuthTokens {
            access_token: access_token.to_string(),
            token_type: "Bearer".to_string(),
            expires_in: None,
            scope: None,
            refresh_token: None,
            id_token: None,
        }),
        tokens_expire_at: None,
        code_verifier: None,
        oauth_state: None,
        discovery: None,
    }
}

fn stored_keys(backend: &InMemoryAuthStorageBackend) -> Vec<String> {
    let raw = auth_with_lock(backend, |current| Ok((current.map(String::from), None)))
        .expect("read in-memory backend")
        .unwrap_or_else(|| "{}".to_string());
    let val = pi_js::json::parse(&raw).unwrap_or(Value::Null);
    val.as_object()
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn keeps_separate_credentials_for_servers_sharing_a_server_url() {
    let backend = Arc::new(InMemoryAuthStorageBackend::new());
    let store = McpOAuthCredentialStore::new(Some(backend), None);

    store
        .for_server("work", SERVER_URL)
        .expect("work store")
        .save(state("work-token"))
        .await
        .expect("save work");
    store
        .for_server("personal", SERVER_URL)
        .expect("personal store")
        .save(state("personal-token"))
        .await
        .expect("save personal");

    assert_eq!(
        store
            .for_server("work", SERVER_URL)
            .expect("work store")
            .load()
            .await
            .expect("load work")
            .and_then(|s| s.tokens)
            .map(|t| t.access_token)
            .as_deref(),
        Some("work-token")
    );
    assert_eq!(
        store
            .for_server("personal", SERVER_URL)
            .expect("personal store")
            .load()
            .await
            .expect("load personal")
            .and_then(|s| s.tokens)
            .map(|t| t.access_token)
            .as_deref(),
        Some("personal-token")
    );

    assert!(store.remove("work", SERVER_URL));
    assert!(
        store
            .for_server("work", SERVER_URL)
            .expect("work store")
            .load()
            .await
            .expect("load removed")
            .is_none()
    );
    assert_eq!(
        store.tokens("personal", SERVER_URL).map(|t| t.access_token).as_deref(),
        Some("personal-token")
    );
}

#[tokio::test]
async fn moves_credentials_stored_by_server_url_to_the_first_server_that_loads_them() {
    let backend = Arc::new(InMemoryAuthStorageBackend::new());
    let seed = pi_js::json::stringify(&json!({ SERVER_URL: state("legacy-token") }));
    auth_with_lock(backend.as_ref(), |_| Ok(((), Some(seed.clone())))).expect("seed backend");
    let store = McpOAuthCredentialStore::new(Some(Arc::clone(&backend) as Arc<dyn AuthStorageBackend>), None);

    // Reading tokens does not take the legacy state over.
    assert_eq!(
        store.tokens("work", SERVER_URL).map(|t| t.access_token).as_deref(),
        Some("legacy-token")
    );
    assert_eq!(stored_keys(&backend), vec![SERVER_URL]);

    assert_eq!(
        store
            .for_server("my_work", SERVER_URL)
            .expect("my_work store")
            .load()
            .await
            .expect("load my_work")
            .and_then(|s| s.tokens)
            .map(|t| t.access_token)
            .as_deref(),
        Some("legacy-token")
    );
    // Names differing only in `-` and `_` are the same server.
    assert_eq!(
        store.tokens("my-work", SERVER_URL).map(|t| t.access_token).as_deref(),
        Some("legacy-token")
    );
    assert!(
        store
            .for_server("personal", SERVER_URL)
            .expect("personal store")
            .load()
            .await
            .expect("load personal")
            .is_none()
    );
    assert_eq!(stored_keys(&backend), vec![format!("mcp__my_work|{SERVER_URL}")]);
}

#[test]
fn signs_out_of_credentials_stored_by_server_url() {
    let backend = Arc::new(InMemoryAuthStorageBackend::new());
    let seed = pi_js::json::stringify(&json!({ SERVER_URL: state("legacy-token") }));
    auth_with_lock(backend.as_ref(), |_| Ok(((), Some(seed.clone())))).expect("seed backend");
    let store = McpOAuthCredentialStore::new(Some(Arc::clone(&backend) as Arc<dyn AuthStorageBackend>), None);

    assert!(store.remove("work", SERVER_URL));
    assert!(stored_keys(&backend).is_empty());
    assert!(!store.remove("work", SERVER_URL));
}
