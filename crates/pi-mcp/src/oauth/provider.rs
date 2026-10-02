//! Port of packages/mcp/src/oauth/provider.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::oauth::flow::{InvalidateCredentialsKind, OAuthClientProvider};
use crate::oauth::types::{OAuthClientInformationMixed, OAuthClientMetadata, OAuthDiscoveryState, OAuthTokens};
use pi_js::Result;

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
    todo!("port: MemoryOAuthStateStore::load")
}

fn memory_oauth_state_store_save(store: &MemoryOAuthStateStore, state: McpOAuthState) {
    todo!("port: MemoryOAuthStateStore::save")
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
        todo!("port: McpOAuthProvider::new")
    }

    async fn load(&self) -> Result<McpOAuthState> {
        todo!("port: McpOAuthProvider::load")
    }

    async fn update<F>(&self, update: F) -> Result<()>
    where
        F: FnOnce(McpOAuthState) -> McpOAuthState + Send,
    {
        todo!("port: McpOAuthProvider::update")
    }

    /// Stored state for another server URL is ignored so credentials never leak across servers.
    fn own(&self, state: Option<McpOAuthState>) -> McpOAuthState {
        todo!("port: McpOAuthProvider::own")
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
        todo!("port: McpOAuthProvider::state")
    }

    async fn client_information(&self) -> Result<Option<OAuthClientInformationMixed>> {
        todo!("port: McpOAuthProvider::client_information")
    }

    async fn save_client_information(&self, information: OAuthClientInformationMixed) -> Result<()> {
        todo!("port: McpOAuthProvider::save_client_information")
    }

    async fn tokens(&self) -> Result<Option<OAuthTokens>> {
        todo!("port: McpOAuthProvider::tokens")
    }

    async fn save_tokens(&self, tokens: OAuthTokens) -> Result<()> {
        todo!("port: McpOAuthProvider::save_tokens")
    }

    async fn redirect_to_authorization(&self, url: url::Url) -> Result<()> {
        todo!("port: McpOAuthProvider::redirect_to_authorization")
    }

    async fn save_code_verifier(&self, verifier: &str) -> Result<()> {
        todo!("port: McpOAuthProvider::save_code_verifier")
    }

    async fn code_verifier(&self) -> Result<String> {
        todo!("port: McpOAuthProvider::code_verifier")
    }

    async fn invalidate_credentials(&self, kind: InvalidateCredentialsKind) -> Result<()> {
        todo!("port: McpOAuthProvider::invalidate_credentials")
    }

    async fn save_discovery_state(&self, state: OAuthDiscoveryState) -> Result<()> {
        todo!("port: McpOAuthProvider::save_discovery_state")
    }

    async fn discovery_state(&self) -> Result<Option<OAuthDiscoveryState>> {
        todo!("port: McpOAuthProvider::discovery_state")
    }
}
