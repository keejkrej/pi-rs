//! Port of packages/coding-agent/test/mcp-oauth-refresh.test.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_coding_agent::core::auth_storage::{AuthStorageBackend, InMemoryAuthStorageBackend};
use pi_coding_agent::extensions::mcp::oauth::{
    McpAuthProvider, McpOAuthCredentialStore, McpOAuthServerStore, McpOAuthSettings, McpSignInPrompt,
    SignInMcpServerOptions, create_mcp_auth_provider, sign_in_mcp_server,
};
use pi_js::abort::AbortSignal;
use pi_js::fetch::{Headers, Request, Response, fetch};
use pi_mcp::auth_provider::{McpFetch, McpFetchInit, UnauthorizedContext};
use pi_mcp::oauth::{OAuthChallenge, OAuthIssuerMismatchError};
use tempfile::TempDir;

#[path = "support/suite/mcp_oauth_server.rs"]
mod mcp_oauth_server;

use mcp_oauth_server::{OAuthMcpServerHandle, StartOAuthMcpServerOptions, start_oauth_mcp_server};

struct HeadlessPrompt {
    opened: Arc<Mutex<Vec<url::Url>>>,
}

impl HeadlessPrompt {
    fn new() -> Self {
        Self {
            opened: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl McpSignInPrompt for HeadlessPrompt {
    fn show_authorization_url(&self, url: &url::Url) {
        self.opened.lock().unwrap_or_else(|e| e.into_inner()).push(url.clone());
        let url_str = url.to_string();
        tokio::spawn(async move {
            let _ = fetch(Request::new("GET", url_str)).await;
        });
    }

    async fn prompt_for_redirect_url(&self, signal: AbortSignal) -> pi_js::Result<Option<String>> {
        signal.cancelled().await;
        Ok(None)
    }
}

fn default_mcp_fetch() -> McpFetch {
    Arc::new(|url_str: &str, init: Option<McpFetchInit>| {
        let url_str = url_str.to_string();
        Box::pin(async move {
            let init = init.unwrap_or_default();
            let method = init.method.as_deref().unwrap_or("GET");
            let mut req = Request::new(method, url_str);
            if let Some(hdrs) = init.headers {
                req = req.with_headers(hdrs);
            }
            if let Some(body) = init.body {
                req = req.body(body);
            }
            if let Some(sig) = init.signal {
                req = req.signal(sig);
            }
            fetch(req).await
        })
    })
}

async fn setup_signed_in() -> (OAuthMcpServerHandle, TempDir, Arc<InMemoryAuthStorageBackend>) {
    let server = start_oauth_mcp_server(StartOAuthMcpServerOptions::default())
        .await
        .expect("start oauth mcp server");
    let lock_dir = tempfile::tempdir().expect("tempdir");
    let backend = Arc::new(InMemoryAuthStorageBackend::new());
    let cred_store = McpOAuthCredentialStore::new(
        Some(Arc::clone(&backend) as Arc<dyn AuthStorageBackend>),
        Some(lock_dir.path().to_string_lossy().into_owned()),
    );
    let store = cred_store.for_server("test", &server.url).expect("server store");
    sign_in_mcp_server(SignInMcpServerOptions {
        server_url: server.url.clone(),
        store,
        settings: McpOAuthSettings::default(),
        challenge: None,
        prompt: Arc::new(HeadlessPrompt::new()),
        signal: None,
    })
    .await
    .expect("sign_in_mcp_server");
    (server, lock_dir, backend)
}

fn make_process(
    server_url: &str,
    backend: &Arc<InMemoryAuthStorageBackend>,
    lock_dir: &TempDir,
) -> (Arc<dyn McpOAuthServerStore>, Arc<dyn McpAuthProvider>) {
    let cred_store = McpOAuthCredentialStore::new(
        Some(Arc::clone(backend) as Arc<dyn AuthStorageBackend>),
        Some(lock_dir.path().to_string_lossy().into_owned()),
    );
    let store = cred_store.for_server("test", server_url).expect("server store");
    let provider = create_mcp_auth_provider(
        server_url,
        Arc::clone(&store) as Arc<dyn McpOAuthServerStore>,
        Arc::new(|| Ok(McpOAuthSettings::default())),
        Arc::new(|_| {}),
    );
    (store, provider)
}

#[tokio::test]
async fn refreshes_once_when_several_processes_find_the_same_token_rejected() {
    let (server, lock_dir, backend) = setup_signed_in().await;
    server.expire_access_tokens();
    let processes = [
        make_process(&server.url, &backend, &lock_dir),
        make_process(&server.url, &backend, &lock_dir),
        make_process(&server.url, &backend, &lock_dir),
    ];

    let mcp_fetch = default_mcp_fetch();
    let parsed_url = url::Url::parse(&server.url).expect("valid server url");

    // The server rotates refresh tokens: a second refresh with refresh-1 would fail with invalid_grant.
    let futs = processes.iter().map(|(_, provider)| {
        let ctx = UnauthorizedContext {
            response: Response::null_body(401, Headers::new()),
            server_url: parsed_url.clone(),
            fetch: Arc::clone(&mcp_fetch),
            token: Some("access-1".to_string()),
        };
        async move {
            if let Some(fut) = provider.on_unauthorized(ctx) {
                fut.await.expect("on_unauthorized");
            }
        }
    });
    futures::future::join_all(futs).await;

    let refresh_count = server
        .log()
        .into_iter()
        .filter(|entry| entry == "token refresh")
        .count();
    assert_eq!(refresh_count, 1);

    for (_, provider) in &processes {
        assert_eq!(provider.token().await.expect("token").as_deref(), Some("access-2"));
    }

    // The lock is released.
    let remaining: Vec<_> = std::fs::read_dir(lock_dir.path()).expect("read lock_dir").collect();
    assert!(remaining.is_empty());

    server.close().await;
}

#[tokio::test]
async fn waits_for_a_running_refresh_to_save_the_new_tokens() {
    let (server, lock_dir, backend) = setup_signed_in().await;
    server.expire_access_tokens();
    let (store, provider) = make_process(&server.url, &backend, &lock_dir);

    let ctx = UnauthorizedContext {
        response: Response::null_body(401, Headers::new()),
        server_url: url::Url::parse(&server.url).expect("valid server url"),
        fetch: default_mcp_fetch(),
        token: Some("access-1".to_string()),
    };
    let refresh_fut = provider.on_unauthorized(ctx).expect("on_unauthorized future");
    let handle = tokio::spawn(refresh_fut);
    tokio::task::yield_now().await;
    provider.settled().await;
    assert_eq!(
        store
            .load()
            .await
            .expect("load")
            .and_then(|s| s.tokens)
            .map(|t| t.access_token)
            .as_deref(),
        Some("access-2")
    );
    handle.await.expect("join").expect("refresh ok");
    server.close().await;
}

async fn sign_in_with_settings(
    iss: Option<&str>,
    settings: impl FnOnce(&str) -> McpOAuthSettings,
) -> pi_js::Result<()> {
    let server = start_oauth_mcp_server(StartOAuthMcpServerOptions {
        iss: iss.map(String::from),
    })
    .await
    .expect("start oauth mcp server");
    let cred_store = McpOAuthCredentialStore::new(Some(Arc::new(InMemoryAuthStorageBackend::new())), None);
    let store = cred_store.for_server("test", &server.url)?;
    let cfg = settings(&server.origin);
    let res = sign_in_mcp_server(SignInMcpServerOptions {
        server_url: server.url.clone(),
        store,
        settings: cfg,
        challenge: None,
        prompt: Arc::new(HeadlessPrompt::new()),
        signal: None,
    })
    .await;
    server.close().await;
    res
}

#[tokio::test]
async fn rejects_authorization_response_from_another_issuer() {
    let err = sign_in_with_settings(Some("https://attacker.example"), |_| McpOAuthSettings::default())
        .await
        .expect_err("should reject wrong issuer");
    assert!(
        err.downcast_ref::<OAuthIssuerMismatchError>().is_some() || err.name() == "OAuthIssuerMismatchError",
        "unexpected error: {err:?}"
    );
}

#[tokio::test]
async fn keeps_granted_scope_when_server_asks_for_more() {
    let server = start_oauth_mcp_server(StartOAuthMcpServerOptions::default())
        .await
        .expect("start oauth mcp server");
    let cred_store = McpOAuthCredentialStore::new(Some(Arc::new(InMemoryAuthStorageBackend::new())), None);
    let store = cred_store.for_server("test", &server.url).expect("store");
    let prompt = Arc::new(HeadlessPrompt::new());

    sign_in_mcp_server(SignInMcpServerOptions {
        server_url: server.url.clone(),
        store: Arc::clone(&store),
        settings: McpOAuthSettings::default(),
        challenge: Some(OAuthChallenge {
            scope: Some("issues:read".to_string()),
            error: None,
            error_description: None,
            resource_metadata_url: None,
        }),
        prompt: Arc::clone(&prompt) as Arc<dyn McpSignInPrompt>,
        signal: None,
    })
    .await
    .expect("first sign in");

    assert_eq!(
        store
            .load()
            .await
            .expect("load 1")
            .and_then(|s| s.tokens)
            .and_then(|t| t.scope)
            .as_deref(),
        Some("issues:read")
    );

    sign_in_mcp_server(SignInMcpServerOptions {
        server_url: server.url.clone(),
        store: Arc::clone(&store),
        settings: McpOAuthSettings::default(),
        challenge: Some(OAuthChallenge {
            scope: Some("issues:write".to_string()),
            error: Some("insufficient_scope".to_string()),
            error_description: None,
            resource_metadata_url: None,
        }),
        prompt: Arc::clone(&prompt) as Arc<dyn McpSignInPrompt>,
        signal: None,
    })
    .await
    .expect("step-up sign in");

    let scopes: Vec<Option<String>> = prompt
        .opened
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|u| u.query_pairs().find(|(k, _)| k == "scope").map(|(_, v)| v.into_owned()))
        .collect();
    assert_eq!(
        scopes,
        vec![
            Some("issues:read".to_string()),
            Some("issues:read issues:write".to_string())
        ]
    );
    assert_eq!(
        store
            .load()
            .await
            .expect("load 2")
            .and_then(|s| s.tokens)
            .and_then(|t| t.scope)
            .as_deref(),
        Some("issues:read issues:write")
    );
    server.close().await;
}

// #10172
#[tokio::test]
async fn uses_configured_authorization_server_metadata_url() {
    let err = sign_in_with_settings(None, |origin| McpOAuthSettings {
        auth_server_metadata_url: Some(url::Url::parse(&format!("{origin}/missing")).expect("valid missing url")),
        ..Default::default()
    })
    .await
    .expect_err("should fail 404");
    assert!(
        err.message().contains("HTTP 404 loading authorization server metadata"),
        "got: {}",
        err.message()
    );
}
