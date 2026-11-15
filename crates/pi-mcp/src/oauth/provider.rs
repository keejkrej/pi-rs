//! Port of packages/mcp/src/oauth/provider.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::oauth::flow::{InvalidateCredentialsKind, OAuthClientProvider};
use crate::oauth::types::{
    OAuthClientInformation, OAuthClientInformationMixed, OAuthClientMetadata, OAuthDiscoveryState, OAuthTokens,
};
use pi_js::{Error, Result};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthState {
    pub server_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_information: Option<OAuthClientInformationMixed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<OAuthTokens>,
    /// When the access token expires, in milliseconds since the epoch, from `expires_in` at the time it was saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_expire_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_verifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<OAuthDiscoveryState>,
}

#[async_trait]
pub trait McpOAuthStateStore: Send + Sync {
    async fn load(&self) -> Result<Option<McpOAuthState>>;
    async fn save(&self, state: McpOAuthState) -> Result<()>;
}

/// `OAuthClientMetadata` with `redirect_uris` optional, as accepted by [`McpOAuthProviderOptions`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpOAuthProviderClientMetadata {
    pub redirect_uris: Option<Vec<String>>,
    pub token_endpoint_auth_method: Option<String>,
    pub grant_types: Option<Vec<String>>,
    pub response_types: Option<Vec<String>>,
    pub client_name: Option<String>,
    pub client_uri: Option<String>,
    pub logo_uri: Option<String>,
    pub scope: Option<String>,
    pub contacts: Option<Vec<String>>,
    pub tos_uri: Option<String>,
    pub policy_uri: Option<String>,
    pub jwks_uri: Option<String>,
    pub jwks: Option<serde_json::Value>,
    pub software_id: Option<String>,
    pub software_version: Option<String>,
    pub software_statement: Option<String>,
}

#[derive(Clone)]
pub struct McpOAuthProviderOptions {
    pub server_url: String,
    pub redirect_url: String,
    pub client_metadata: McpOAuthProviderClientMetadata,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub store: Option<Arc<dyn McpOAuthStateStore>>,
    pub on_redirect: Arc<dyn Fn(url::Url) -> pi_js::BoxFuture<Result<()>> + Send + Sync>,
}

#[derive(Clone)]
pub struct MemoryOAuthStateStore {
    inner: Arc<MemoryOAuthStateStoreInner>,
}

struct MemoryOAuthStateStoreInner {
    value: Mutex<Option<McpOAuthState>>,
}

fn memory_oauth_state_store_load(store: &MemoryOAuthStateStore) -> Option<McpOAuthState> {
    store.inner.value.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn memory_oauth_state_store_save(store: &MemoryOAuthStateStore, state: McpOAuthState) {
    *store.inner.value.lock().unwrap_or_else(|e| e.into_inner()) = Some(state);
}

impl MemoryOAuthStateStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MemoryOAuthStateStoreInner {
                value: Mutex::new(None),
            }),
        }
    }

    pub fn load(&self) -> Option<McpOAuthState> {
        memory_oauth_state_store_load(self)
    }

    pub fn save(&self, state: McpOAuthState) {
        memory_oauth_state_store_save(self, state)
    }
}

impl Default for MemoryOAuthStateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl McpOAuthStateStore for MemoryOAuthStateStore {
    async fn load(&self) -> Result<Option<McpOAuthState>> {
        Ok(memory_oauth_state_store_load(self))
    }

    async fn save(&self, state: McpOAuthState) -> Result<()> {
        memory_oauth_state_store_save(self, state);
        Ok(())
    }
}

/// Default stateful provider for one exact MCP server URL. Applications inject durable storage if needed.
#[derive(Clone)]
pub struct McpOAuthProvider {
    inner: Arc<McpOAuthProviderInner>,
}

struct McpOAuthProviderInner {
    redirect_url: String,
    client_metadata: OAuthClientMetadata,
    server_url: String,
    configured_client: Option<OAuthClientInformationMixed>,
    store: Arc<dyn McpOAuthStateStore>,
    on_redirect: Arc<dyn Fn(url::Url) -> pi_js::BoxFuture<Result<()>> + Send + Sync>,
    // PORT: TS `writes` is a promise chain (`this.writes = this.writes.then(...)`). §4.4 maps that
    // serialization chain to a `tokio::sync::Mutex`.
    writes: tokio::sync::Mutex<()>,
}

impl McpOAuthProvider {
    pub fn new(options: McpOAuthProviderOptions) -> Self {
        let server_url = url::Url::parse(&options.server_url)
            .unwrap_or_else(|e| panic!("Invalid MCP server URL '{}': {e}", options.server_url))
            .to_string();
        let redirect_url = options.redirect_url;
        let has_secret = options.client_secret.as_deref().is_some_and(|s| !s.is_empty());
        let meta = options.client_metadata;
        let client_metadata = OAuthClientMetadata {
            redirect_uris: meta.redirect_uris.unwrap_or_else(|| vec![redirect_url.clone()]),
            token_endpoint_auth_method: Some(meta.token_endpoint_auth_method.unwrap_or_else(|| {
                if has_secret {
                    "client_secret_post".to_string()
                } else {
                    "none".to_string()
                }
            })),
            grant_types: Some(
                meta.grant_types
                    .unwrap_or_else(|| vec!["authorization_code".to_string(), "refresh_token".to_string()]),
            ),
            response_types: Some(meta.response_types.unwrap_or_else(|| vec!["code".to_string()])),
            client_name: meta.client_name,
            client_uri: meta.client_uri,
            logo_uri: meta.logo_uri,
            scope: meta.scope,
            contacts: meta.contacts,
            tos_uri: meta.tos_uri,
            policy_uri: meta.policy_uri,
            jwks_uri: meta.jwks_uri,
            jwks: meta.jwks,
            software_id: meta.software_id,
            software_version: meta.software_version,
            software_statement: meta.software_statement,
        };
        let configured_client = options
            .client_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(|client_id| {
                OAuthClientInformationMixed::from(OAuthClientInformation {
                    client_id: client_id.to_string(),
                    client_secret: options.client_secret.filter(|s| !s.is_empty()),
                    client_id_issued_at: None,
                    client_secret_expires_at: None,
                })
            });
        let store = options.store.unwrap_or_else(|| Arc::new(MemoryOAuthStateStore::new()));
        Self {
            inner: Arc::new(McpOAuthProviderInner {
                redirect_url,
                client_metadata,
                server_url,
                configured_client,
                store,
                on_redirect: options.on_redirect,
                writes: tokio::sync::Mutex::new(()),
            }),
        }
    }

    async fn load(&self) -> Result<McpOAuthState> {
        let _guard = self.inner.writes.lock().await;
        Ok(self.own(self.inner.store.load().await?))
    }

    async fn update<F>(&self, update: F) -> Result<()>
    where
        F: FnOnce(McpOAuthState) -> McpOAuthState + Send,
    {
        let _guard = self.inner.writes.lock().await;
        let current = self.own(self.inner.store.load().await?);
        let next = update(current);
        self.inner.store.save(next).await
    }

    /// Stored state for another server URL is ignored so credentials never leak across servers.
    fn own(&self, state: Option<McpOAuthState>) -> McpOAuthState {
        match state {
            Some(s) if s.server_url == self.inner.server_url => s,
            _ => McpOAuthState {
                server_url: self.inner.server_url.clone(),
                client_information: None,
                tokens: None,
                tokens_expire_at: None,
                code_verifier: None,
                oauth_state: None,
                discovery: None,
            },
        }
    }
}

#[async_trait]
impl OAuthClientProvider for McpOAuthProvider {
    fn redirect_url(&self) -> String {
        self.inner.redirect_url.clone()
    }

    fn client_metadata(&self) -> &OAuthClientMetadata {
        &self.inner.client_metadata
    }

    async fn state(&self) -> Result<Option<String>> {
        if let Some(existing) = self.load().await?.oauth_state.filter(|s| !s.is_empty()) {
            return Ok(Some(existing));
        }
        let state = hex::encode(pi_js::crypto::random_bytes(32));
        let state_clone = state.clone();
        self.update(move |mut value| {
            value.oauth_state = Some(state_clone);
            value
        })
        .await?;
        Ok(Some(state))
    }

    async fn client_information(&self) -> Result<Option<OAuthClientInformationMixed>> {
        if let Some(configured) = &self.inner.configured_client {
            return Ok(Some(configured.clone()));
        }
        Ok(self.load().await?.client_information)
    }

    async fn save_client_information(&self, information: OAuthClientInformationMixed) -> Result<()> {
        if self.inner.configured_client.is_some() {
            return Ok(());
        }
        self.update(move |mut value| {
            value.client_information = Some(information);
            value
        })
        .await
    }

    async fn tokens(&self) -> Result<Option<OAuthTokens>> {
        Ok(self.load().await?.tokens)
    }

    async fn save_tokens(&self, tokens: OAuthTokens) -> Result<()> {
        let expires_at = tokens
            .expires_in
            .map(|secs| pi_js::time::now_ms() + (secs * 1000.0) as i64);
        self.update(move |mut value| {
            value.tokens = Some(tokens);
            value.tokens_expire_at = expires_at;
            value
        })
        .await
    }

    async fn redirect_to_authorization(&self, url: url::Url) -> Result<()> {
        (self.inner.on_redirect)(url).await
    }

    async fn save_code_verifier(&self, verifier: &str) -> Result<()> {
        let verifier = verifier.to_string();
        self.update(move |mut value| {
            value.code_verifier = Some(verifier);
            value
        })
        .await
    }

    async fn code_verifier(&self) -> Result<String> {
        self.load()
            .await?
            .code_verifier
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error::msg("No OAuth PKCE code verifier is stored"))
    }

    async fn invalidate_credentials(&self, kind: InvalidateCredentialsKind) -> Result<()> {
        self.update(move |mut next| {
            if matches!(kind, InvalidateCredentialsKind::All | InvalidateCredentialsKind::Client) {
                next.client_information = None;
            }
            if matches!(kind, InvalidateCredentialsKind::All | InvalidateCredentialsKind::Tokens) {
                next.tokens = None;
                next.tokens_expire_at = None;
            }
            if matches!(
                kind,
                InvalidateCredentialsKind::All | InvalidateCredentialsKind::Verifier
            ) {
                next.code_verifier = None;
            }
            if matches!(
                kind,
                InvalidateCredentialsKind::All | InvalidateCredentialsKind::Discovery
            ) {
                next.discovery = None;
            }
            if matches!(kind, InvalidateCredentialsKind::All) {
                next.oauth_state = None;
            }
            next
        })
        .await
    }

    async fn save_discovery_state(&self, state: OAuthDiscoveryState) -> Result<()> {
        self.update(move |mut value| {
            value.discovery = Some(state);
            value
        })
        .await
    }

    async fn discovery_state(&self) -> Result<Option<OAuthDiscoveryState>> {
        Ok(self.load().await?.discovery)
    }
}
