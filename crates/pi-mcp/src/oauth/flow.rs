//! Port of packages/mcp/src/oauth/flow.ts
//!
//! Adapted from modelcontextprotocol/typescript-sdk v1.29.0 src/client/auth.ts.
//! Copyright (c) 2024 Anthropic, PBC. Licensed under MIT; see LICENSES/.
//! Modified to remove SDK/Zod dependencies and use WebCrypto for PKCE.

#![allow(dead_code, unused_variables)]

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::auth_provider::{AuthProvider, McpFetch};
use crate::oauth::types::{
    AuthorizationServerMetadata, OAuthClientInformation, OAuthClientInformationFull, OAuthClientInformationMixed,
    OAuthClientMetadata, OAuthDiscoveryState, OAuthTokens,
};
use pi_js::Result;

/// `URLSearchParams` for token-endpoint bodies.
///
/// // PORT: `BoxFuture` is `'static`, so [`AddClientAuthentication`] takes and returns this value
/// instead of mutating the caller's params in place across an await.
#[derive(Clone, Debug, Default)]
pub struct UrlSearchParams {
    pub entries: Vec<(String, String)>,
}

impl UrlSearchParams {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn set(&mut self, name: &str, value: &str) {
        todo!("port: UrlSearchParams::set")
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        todo!("port: UrlSearchParams::get")
    }
}

/// `(headers, params, url, metadata?) => void | Promise<void>`.
///
/// // PORT: returns the mutated headers and params. A `'static` future cannot borrow the caller's
/// `Headers` and `URLSearchParams` the way the TS callback mutates them in place.
pub type AddClientAuthentication = Arc<
    dyn Fn(
            pi_js::fetch::Headers,
            UrlSearchParams,
            String,
            Option<AuthorizationServerMetadata>,
        ) -> pi_js::BoxFuture<Result<(pi_js::fetch::Headers, UrlSearchParams)>>
        + Send
        + Sync,
>;

/// `"all" | "client" | "tokens" | "verifier" | "discovery"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvalidateCredentialsKind {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "client")]
    Client,
    #[serde(rename = "tokens")]
    Tokens,
    #[serde(rename = "verifier")]
    Verifier,
    #[serde(rename = "discovery")]
    Discovery,
}

#[async_trait]
pub trait OAuthClientProvider: Send + Sync {
    fn redirect_url(&self) -> String;
    fn client_metadata(&self) -> &OAuthClientMetadata;
    fn client_metadata_url(&self) -> Option<&str> {
        None
    }
    async fn state(&self) -> Result<Option<String>> {
        Ok(None)
    }
    async fn client_information(&self) -> Result<Option<OAuthClientInformationMixed>>;
    async fn save_client_information(&self, information: OAuthClientInformationMixed) -> Result<()> {
        let _ = information;
        Ok(())
    }
    async fn tokens(&self) -> Result<Option<OAuthTokens>>;
    async fn save_tokens(&self, tokens: OAuthTokens) -> Result<()>;
    async fn redirect_to_authorization(&self, url: url::Url) -> Result<()>;
    async fn save_code_verifier(&self, verifier: &str) -> Result<()>;
    async fn code_verifier(&self) -> Result<String>;
    fn add_client_authentication(&self) -> Option<AddClientAuthentication> {
        None
    }
    async fn invalidate_credentials(&self, kind: InvalidateCredentialsKind) -> Result<()> {
        let _ = kind;
        Ok(())
    }
    async fn save_discovery_state(&self, state: OAuthDiscoveryState) -> Result<()> {
        let _ = state;
        Ok(())
    }
    async fn discovery_state(&self) -> Result<Option<OAuthDiscoveryState>> {
        Ok(None)
    }
}

#[derive(Clone)]
pub struct OAuthFlowOptions {
    pub server_url: String,
    pub authorization_code: Option<String>,
    /// `iss` parameter of the authorization response that delivered `authorizationCode` (RFC 9207).
    pub iss: Option<String>,
    pub scope: Option<String>,
    pub resource_metadata_url: Option<url::Url>,
    /// Authorization server metadata document to use instead of discovery, for servers that advertise a
    /// wrong authorization server or none. It is trusted as configured. Must use https, except on loopback.
    pub authorization_server_metadata_url: Option<url::Url>,
    pub fetch: Option<McpFetch>,
    pub skip_issuer_validation: Option<bool>,
    /// Go straight to the authorization redirect instead of refreshing stored tokens, for example when the
    /// server asks for scopes the current grant lacks (a refresh keeps the old scope).
    pub skip_refresh: Option<bool>,
}

/// `"AUTHORIZED" | "REDIRECT"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OAuthFlowResult {
    #[serde(rename = "AUTHORIZED")]
    Authorized,
    #[serde(rename = "REDIRECT")]
    Redirect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClientAuthMethod {
    ClientSecretBasic,
    ClientSecretPost,
    None,
}

#[derive(Clone)]
pub struct TokenRequestOptions {
    pub metadata: Option<AuthorizationServerMetadata>,
    pub client_information: OAuthClientInformationMixed,
    pub resource: Option<String>,
    pub add_client_authentication: Option<AddClientAuthentication>,
    pub fetch: Option<McpFetch>,
}

#[derive(Clone)]
pub struct StartAuthorizationOptions {
    pub metadata: Option<AuthorizationServerMetadata>,
    pub client_information: OAuthClientInformationMixed,
    pub redirect_url: String,
    pub scope: Option<String>,
    pub state: Option<String>,
    pub resource: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartAuthorizationResult {
    pub authorization_url: url::Url,
    pub code_verifier: String,
}

#[derive(Clone)]
pub struct RegisterClientOptions {
    pub metadata: Option<AuthorizationServerMetadata>,
    pub client_metadata: OAuthClientMetadata,
    pub scope: Option<String>,
    pub fetch: Option<McpFetch>,
}

#[derive(Clone)]
pub struct ExchangeAuthorizationCodeOptions {
    pub metadata: Option<AuthorizationServerMetadata>,
    pub client_information: OAuthClientInformationMixed,
    pub resource: Option<String>,
    pub add_client_authentication: Option<AddClientAuthentication>,
    pub fetch: Option<McpFetch>,
    pub code: String,
    pub code_verifier: String,
    pub redirect_url: String,
}

#[derive(Clone)]
pub struct RefreshAuthorizationOptions {
    pub metadata: Option<AuthorizationServerMetadata>,
    pub client_information: OAuthClientInformationMixed,
    pub resource: Option<String>,
    pub add_client_authentication: Option<AddClientAuthentication>,
    pub fetch: Option<McpFetch>,
    pub refresh_token: String,
}

struct Pkce {
    verifier: String,
    challenge: String,
}

fn loopback(hostname: &str) -> bool {
    todo!("port: loopback")
}

fn secure_endpoint(value: &str) -> Result<url::Url> {
    todo!("port: secure_endpoint")
}

fn select_client_auth_method(information: &OAuthClientInformationMixed, supported: &[String]) -> ClientAuthMethod {
    todo!("port: select_client_auth_method")
}

fn apply_client_authentication(
    method: ClientAuthMethod,
    information: &OAuthClientInformation,
    headers: &mut pi_js::fetch::Headers,
    params: &mut UrlSearchParams,
) {
    todo!("port: apply_client_authentication")
}

async fn pkce() -> Result<Pkce> {
    todo!("port: pkce")
}

pub async fn start_authorization(
    authorization_server_url: &str,
    options: StartAuthorizationOptions,
) -> Result<StartAuthorizationResult> {
    todo!("port: start_authorization")
}

async fn token_request(
    authorization_server_url: &str,
    options: &TokenRequestOptions,
    params: UrlSearchParams,
) -> Result<OAuthTokens> {
    todo!("port: token_request")
}

pub async fn register_client(
    authorization_server_url: &str,
    options: RegisterClientOptions,
) -> Result<OAuthClientInformationFull> {
    todo!("port: register_client")
}

pub async fn exchange_authorization_code(
    authorization_server_url: &str,
    options: ExchangeAuthorizationCodeOptions,
) -> Result<OAuthTokens> {
    todo!("port: exchange_authorization_code")
}

pub async fn refresh_authorization(
    authorization_server_url: &str,
    options: RefreshAuthorizationOptions,
) -> Result<OAuthTokens> {
    todo!("port: refresh_authorization")
}

fn with_scope(tokens: OAuthTokens, scope: Option<&str>) -> OAuthTokens {
    todo!("port: with_scope")
}

/// Scopes for a step-up authorization: the challenged scopes plus the ones granted so far, since a
/// challenge may list only the missing scopes and a token with just those would lose access the old
/// one had (SEP-2350). Without challenged scopes, `undefined` lets the flow pick its default.
pub fn step_up_scope(granted: Option<&str>, challenged: Option<&str>) -> Option<String> {
    todo!("port: step_up_scope")
}

async fn run_flow(provider: &dyn OAuthClientProvider, options: &OAuthFlowOptions) -> Result<OAuthFlowResult> {
    todo!("port: run_flow")
}

pub async fn authorize_mcp(provider: &dyn OAuthClientProvider, options: OAuthFlowOptions) -> Result<OAuthFlowResult> {
    todo!("port: authorize_mcp")
}

/// Auth provider for `StreamableHttpTransport`. After a 401 it refreshes the tokens, or throws
/// `McpOAuthAuthorizationRequiredError` when the user has to authorize (again). Concurrent 401s share
/// one refresh, and a request whose token was already replaced is just retried: with rotating refresh
/// tokens, a second refresh with the old refresh token would fail and discard the new grant.
pub fn adapt_oauth_provider(provider: Arc<dyn OAuthClientProvider>) -> Arc<dyn AuthProvider + Send + Sync> {
    todo!("port: adapt_oauth_provider")
}
