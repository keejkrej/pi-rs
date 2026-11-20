//! Port of packages/mcp/src/oauth/flow.ts
//!
//! Adapted from modelcontextprotocol/typescript-sdk v1.29.0 src/client/auth.ts.
//! Copyright (c) 2024 Anthropic, PBC. Licensed under MIT; see LICENSES/.
//! Modified to remove SDK/Zod dependencies and use WebCrypto for PKCE.

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexSet;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::auth_provider::{AuthProvider, McpFetch, McpFetchInit, UnauthorizedContext};
use crate::oauth::discovery::{
    DiscoverAuthorizationServerMetadataOptions, DiscoverOAuthServerInfoOptions, discover_authorization_server_metadata,
    discover_oauth_server_info, parse_www_authenticate, select_resource,
};
use crate::oauth::errors::{
    McpOAuthAuthorizationRequiredError, OAuthError, OAuthInsecureEndpointError, OAuthIssuerMismatchError,
    OAuthRegistrationError,
};
use crate::oauth::types::{
    AuthorizationServerMetadata, OAuthClientInformation, OAuthClientInformationFull, OAuthClientInformationMixed,
    OAuthClientMetadata, OAuthDiscoveryState, OAuthTokens, is_http_url, parse_client_information, parse_oauth_tokens,
};
use crate::protocol::jsonrpc::to_error;
use pi_js::abort::AbortSignal;
use pi_js::fetch::{Body, Headers, Request};
use pi_js::{BoxFuture, Error, Result};

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
        let mut replaced = false;
        let mut next = Vec::with_capacity(self.entries.len() + 1);
        for (k, v) in self.entries.drain(..) {
            if k == name {
                if !replaced {
                    next.push((k, value.to_string()));
                    replaced = true;
                }
            } else {
                next.push((k, v));
            }
        }
        if !replaced {
            next.push((name.to_string(), value.to_string()));
        }
        self.entries = next;
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

fn default_fetch() -> McpFetch {
    Arc::new(|url, init| {
        let url = url.to_string();
        Box::pin(async move {
            let init = init.unwrap_or_default();
            let mut req = Request::new(init.method.unwrap_or_else(|| "GET".to_string()), url);
            if let Some(headers) = init.headers {
                req = req.with_headers(headers);
            }
            if let Some(body) = init.body {
                req = req.body(body);
            }
            if let Some(signal) = init.signal {
                req = req.signal(signal);
            }
            pi_js::fetch::fetch(req).await
        })
    })
}

fn set_url_query_param(url: &mut url::Url, key: &str, value: &str) {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut replaced = false;
    for (k, v) in url.query_pairs() {
        if k == key {
            if !replaced {
                pairs.push((k.into_owned(), value.to_string()));
                replaced = true;
            }
        } else {
            pairs.push((k.into_owned(), v.into_owned()));
        }
    }
    if !replaced {
        pairs.push((key.to_string(), value.to_string()));
    }
    url.query_pairs_mut().clear().extend_pairs(pairs);
}

pub(crate) fn clone_oauth_error(error: &Error) -> Error {
    if let Some(e) = error.downcast_ref::<OAuthError>() {
        return Error::typed("OAuthError", e.clone());
    }
    if let Some(e) = error.downcast_ref::<OAuthIssuerMismatchError>() {
        return Error::typed("OAuthIssuerMismatchError", e.clone());
    }
    if let Some(e) = error.downcast_ref::<OAuthInsecureEndpointError>() {
        return Error::typed("OAuthInsecureEndpointError", e.clone());
    }
    if let Some(e) = error.downcast_ref::<OAuthRegistrationError>() {
        return Error::typed("OAuthRegistrationError", e.clone());
    }
    if let Some(e) = error.downcast_ref::<McpOAuthAuthorizationRequiredError>() {
        return Error::typed("McpOAuthAuthorizationRequiredError", e.clone());
    }
    to_error(error)
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

/// A Client ID Metadata Document: an https URL used as `client_id`, and a redirect URI it lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthClientMetadataDocument {
    pub url: String,
    pub redirect_url: String,
}

#[async_trait]
pub trait OAuthClientProvider: Send + Sync {
    fn redirect_url(&self) -> String;
    fn client_metadata(&self) -> &OAuthClientMetadata;
    /// Client ID Metadata Document to identify as instead of registering dynamically, or `None` to
    /// register. Called when no client information is stored; the document is not stored. `metadata` is
    /// `None` when the authorization server has none; check `client_id_metadata_document_supported`.
    fn client_metadata_document(
        &self,
        metadata: Option<&AuthorizationServerMetadata>,
    ) -> Result<Option<OAuthClientMetadataDocument>> {
        let _ = metadata;
        Ok(None)
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

// PORT: Additive options struct so existing `OAuthFlowOptions` struct literals keep compiling cleanly.
#[derive(Clone, Default)]
pub struct OAuthFlowExtraOptions {
    /// Aborts every request of the flow. Requests have no time limit of their own; combine with a timeout as needed.
    pub signal: Option<AbortSignal>,
    /// Only refresh the stored tokens: return `REDIRECT` instead of registering a client or preparing an
    /// authorization request, so a pending authorization (its `state` and PKCE verifier) stays usable.
    pub refresh_only: Option<bool>,
    /// With `authorizationCode`: the redirect URI the authorization request used. The exchange fails before
    /// sending the code when the flow would send another one (RFC 6749 section 4.1.3 requires the same).
    pub authorization_redirect_url: Option<String>,
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
    pub signal: Option<AbortSignal>,
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
    pub signal: Option<AbortSignal>,
}

#[derive(Clone)]
pub struct ExchangeAuthorizationCodeOptions {
    pub metadata: Option<AuthorizationServerMetadata>,
    pub client_information: OAuthClientInformationMixed,
    pub resource: Option<String>,
    pub add_client_authentication: Option<AddClientAuthentication>,
    pub fetch: Option<McpFetch>,
    pub signal: Option<AbortSignal>,
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
    pub signal: Option<AbortSignal>,
    pub refresh_token: String,
}

struct Pkce {
    verifier: String,
    challenge: String,
}

fn loopback(hostname: &str) -> bool {
    matches!(hostname, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

/// The OpenID Connect `application_type` for `redirect_uris` (MCP SEP-837). Without one, OpenID Connect servers
/// assume `web`, which rejects http loopback redirect URIs. Loopback hosts and custom schemes are native (RFC 8252).
fn application_type(redirect_uris: &[String]) -> &'static str {
    let native = redirect_uris.iter().any(|uri| {
        let Ok(url) = url::Url::parse(uri) else {
            return false;
        };
        !is_http_url(&url) || loopback(url.host_str().unwrap_or(""))
    });
    if native { "native" } else { "web" }
}

fn secure_endpoint(value: &str) -> Result<url::Url> {
    let url = url::Url::parse(value).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
    let host = url.host_str().unwrap_or("");
    if url.scheme() != "https" && !loopback(host) {
        return Err(Error::typed(
            "OAuthInsecureEndpointError",
            OAuthInsecureEndpointError::new(url.as_str()),
        ));
    }
    Ok(url)
}

fn select_client_auth_method(information: &OAuthClientInformationMixed, supported: &[String]) -> ClientAuthMethod {
    let hinted = information.token_endpoint_auth_method.as_deref();
    if let Some(h) = hinted
        && matches!(h, "client_secret_basic" | "client_secret_post" | "none")
        && (supported.is_empty() || supported.iter().any(|s| s == h))
    {
        return match h {
            "client_secret_basic" => ClientAuthMethod::ClientSecretBasic,
            "client_secret_post" => ClientAuthMethod::ClientSecretPost,
            _ => ClientAuthMethod::None,
        };
    }
    let has_secret = information.client_secret.as_deref().is_some_and(|s| !s.is_empty());
    if supported.is_empty() {
        return if has_secret {
            ClientAuthMethod::ClientSecretBasic
        } else {
            ClientAuthMethod::None
        };
    }
    if has_secret && supported.iter().any(|s| s == "client_secret_basic") {
        return ClientAuthMethod::ClientSecretBasic;
    }
    if has_secret && supported.iter().any(|s| s == "client_secret_post") {
        return ClientAuthMethod::ClientSecretPost;
    }
    if supported.iter().any(|s| s == "none") {
        return ClientAuthMethod::None;
    }
    if has_secret {
        ClientAuthMethod::ClientSecretPost
    } else {
        ClientAuthMethod::None
    }
}

fn apply_client_authentication(
    method: ClientAuthMethod,
    information: &OAuthClientInformation,
    headers: &mut Headers,
    params: &mut UrlSearchParams,
) -> Result<()> {
    if method == ClientAuthMethod::ClientSecretBasic {
        let Some(secret) = information.client_secret.as_deref().filter(|s| !s.is_empty()) else {
            return Err(Error::msg("client_secret_basic requires a client secret"));
        };
        let encoded = pi_js::b64::encode(format!("{}:{secret}", information.client_id).as_bytes());
        headers.set("Authorization", &format!("Basic {encoded}"));
    } else {
        params.set("client_id", &information.client_id);
        if method == ClientAuthMethod::ClientSecretPost
            && let Some(secret) = information.client_secret.as_deref().filter(|s| !s.is_empty())
        {
            params.set("client_secret", secret);
        }
    }
    Ok(())
}

async fn pkce() -> Result<Pkce> {
    let bytes = pi_js::crypto::random_bytes(32);
    let verifier = pi_js::b64::encode_url(&bytes);
    let digest = pi_js::crypto::sha256(verifier.as_bytes());
    let challenge = pi_js::b64::encode_url(&digest);
    Ok(Pkce { verifier, challenge })
}

pub async fn start_authorization(
    authorization_server_url: &str,
    options: StartAuthorizationOptions,
) -> Result<StartAuthorizationResult> {
    if let Some(metadata) = &options.metadata {
        if !metadata.response_types_supported.iter().any(|r| r == "code") {
            return Err(Error::msg("Authorization server does not support authorization codes"));
        }
        if let Some(methods) = &metadata.code_challenge_methods_supported
            && !methods.iter().any(|m| m == "S256")
        {
            return Err(Error::msg("Authorization server does not support PKCE S256"));
        }
    }

    let mut url = if let Some(endpoint) = options.metadata.as_ref().map(|m| m.authorization_endpoint.as_str()) {
        url::Url::parse(endpoint).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?
    } else {
        url::Url::parse(authorization_server_url)
            .and_then(|u| u.join("/authorize"))
            .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?
    };
    // Callers open this URL with the platform URL handler, which also launches files and custom protocols.
    if !is_http_url(&url) {
        return Err(Error::msg(format!("Invalid authorization endpoint {url}")));
    }

    let Pkce { verifier, challenge } = pkce().await?;
    set_url_query_param(&mut url, "response_type", "code");
    set_url_query_param(&mut url, "client_id", &options.client_information.client_id);
    set_url_query_param(&mut url, "code_challenge", &challenge);
    set_url_query_param(&mut url, "code_challenge_method", "S256");
    set_url_query_param(&mut url, "redirect_uri", &options.redirect_url);
    if let Some(state) = options.state.as_deref().filter(|s| !s.is_empty()) {
        set_url_query_param(&mut url, "state", state);
    }
    if let Some(scope) = options.scope.as_deref().filter(|s| !s.is_empty()) {
        set_url_query_param(&mut url, "scope", scope);
        if scope
            .split(|c: char| pi_js::str16::is_js_whitespace(c))
            .any(|w| w == "offline_access")
        {
            set_url_query_param(&mut url, "prompt", "consent");
        }
    }
    if let Some(resource) = options.resource.as_deref().filter(|s| !s.is_empty()) {
        set_url_query_param(&mut url, "resource", resource);
    }

    Ok(StartAuthorizationResult {
        authorization_url: url,
        code_verifier: verifier,
    })
}

async fn token_request(
    authorization_server_url: &str,
    options: &TokenRequestOptions,
    mut params: UrlSearchParams,
) -> Result<OAuthTokens> {
    let raw_endpoint = match options.metadata.as_ref().map(|m| m.token_endpoint.as_str()) {
        Some(ep) => ep.to_string(),
        None => url::Url::parse(authorization_server_url)
            .and_then(|u| u.join("/token"))
            .map(|u| u.to_string())
            .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?,
    };
    let url = secure_endpoint(&raw_endpoint)?;

    let mut headers = Headers::new();
    headers.set("Accept", "application/json");
    headers.set("content-type", "application/x-www-form-urlencoded");
    if let Some(resource) = options.resource.as_deref().filter(|s| !s.is_empty()) {
        params.set("resource", resource);
    }

    if let Some(add_auth) = &options.add_client_authentication {
        let (new_headers, new_params) = add_auth(headers, params, url.to_string(), options.metadata.clone()).await?;
        headers = new_headers;
        params = new_params;
    } else {
        let supported = options
            .metadata
            .as_ref()
            .and_then(|m| m.token_endpoint_auth_methods_supported.as_deref())
            .unwrap_or(&[]);
        let method = select_client_auth_method(&options.client_information, supported);
        let client_info = OAuthClientInformation {
            client_id: options.client_information.client_id.clone(),
            client_secret: options.client_information.client_secret.clone(),
            client_id_issued_at: options.client_information.client_id_issued_at,
            client_secret_expires_at: options.client_information.client_secret_expires_at,
        };
        apply_client_authentication(method, &client_info, &mut headers, &mut params)?;
    }

    let body_str = {
        let mut ser = form_urlencoded::Serializer::new(String::new());
        for (k, v) in &params.entries {
            ser.append_pair(k, v);
        }
        ser.finish()
    };

    let fetch = options.fetch.clone().unwrap_or_else(default_fetch);
    let response = fetch(
        url.as_str(),
        Some(McpFetchInit {
            method: Some("POST".to_string()),
            headers: Some(headers),
            body: Some(Body::Text(body_str)),
            signal: options.signal.clone(),
        }),
    )
    .await?;

    let status = response.status();
    let ok = response.ok();
    let text = response.text().await?;
    let value = pi_js::json::parse(&text).ok();

    // Servers may report OAuth errors with any status, so check the body before the status.
    if let Some(Value::Object(ref obj)) = value
        && let Some(code) = obj.get("error").and_then(Value::as_str)
    {
        let description = obj.get("error_description").and_then(Value::as_str).unwrap_or(code);
        let error_uri = obj.get("error_uri").and_then(Value::as_str).map(String::from);
        return Err(Error::typed(
            "OAuthError",
            OAuthError::new(code, description, error_uri),
        ));
    }
    if !ok {
        return Err(Error::typed(
            "OAuthError",
            OAuthError::new("server_error", format!("HTTP {status}: {text}"), None),
        ));
    }
    parse_oauth_tokens(value.unwrap_or(Value::Null))
}

pub async fn register_client(
    authorization_server_url: &str,
    options: RegisterClientOptions,
) -> Result<OAuthClientInformationFull> {
    let endpoint = options
        .metadata
        .as_ref()
        .and_then(|m| m.registration_endpoint.as_deref());
    if options.metadata.is_some() && endpoint.is_none() {
        return Err(Error::msg(
            "Authorization server does not support dynamic client registration",
        ));
    }
    let url = if let Some(ep) = endpoint {
        url::Url::parse(ep).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?
    } else {
        url::Url::parse(authorization_server_url)
            .and_then(|u| u.join("/register"))
            .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?
    };

    let mut headers = Headers::new();
    headers.set("Accept", "application/json");
    headers.set("content-type", "application/json");

    let mut body_val = serde_json::to_value(&options.client_metadata).unwrap_or_else(|_| Value::Object(Map::new()));
    if let Value::Object(ref mut obj) = body_val {
        if !obj.contains_key("application_type") {
            let derived = application_type(&options.client_metadata.redirect_uris);
            obj.insert("application_type".to_string(), Value::String(derived.to_string()));
        }
        if let Some(scope) = options.scope.as_deref().filter(|s| !s.is_empty()) {
            obj.insert("scope".to_string(), Value::String(scope.to_string()));
        }
    }
    let body_json = pi_js::json::stringify(&body_val);

    let fetch = options.fetch.unwrap_or_else(default_fetch);
    let response = fetch(
        url.as_str(),
        Some(McpFetchInit {
            method: Some("POST".to_string()),
            headers: Some(headers),
            body: Some(Body::Text(body_json)),
            signal: options.signal,
        }),
    )
    .await?;

    if !response.ok() {
        let status = i64::from(response.status());
        let body = response.text().await.unwrap_or_default();
        return Err(Error::typed(
            "OAuthRegistrationError",
            OAuthRegistrationError::new(status, body),
        ));
    }
    parse_client_information(response.json().await?)
}

pub async fn exchange_authorization_code(
    authorization_server_url: &str,
    options: ExchangeAuthorizationCodeOptions,
) -> Result<OAuthTokens> {
    let mut params = UrlSearchParams::new();
    params.set("grant_type", "authorization_code");
    params.set("code", &options.code);
    params.set("code_verifier", &options.code_verifier);
    params.set("redirect_uri", &options.redirect_url);
    let token_options = TokenRequestOptions {
        metadata: options.metadata,
        client_information: options.client_information,
        resource: options.resource,
        add_client_authentication: options.add_client_authentication,
        fetch: options.fetch,
        signal: options.signal,
    };
    token_request(authorization_server_url, &token_options, params).await
}

pub async fn refresh_authorization(
    authorization_server_url: &str,
    options: RefreshAuthorizationOptions,
) -> Result<OAuthTokens> {
    let mut params = UrlSearchParams::new();
    params.set("grant_type", "refresh_token");
    params.set("refresh_token", &options.refresh_token);
    let token_options = TokenRequestOptions {
        metadata: options.metadata,
        client_information: options.client_information,
        resource: options.resource,
        add_client_authentication: options.add_client_authentication,
        fetch: options.fetch,
        signal: options.signal,
    };
    let mut tokens = token_request(authorization_server_url, &token_options, params).await?;
    if tokens.refresh_token.is_none() {
        tokens.refresh_token = Some(options.refresh_token);
    }
    Ok(tokens)
}

fn with_scope(mut tokens: OAuthTokens, scope: Option<&str>) -> OAuthTokens {
    if tokens.scope.is_none()
        && let Some(s) = scope.filter(|s| !s.is_empty())
    {
        tokens.scope = Some(s.to_string());
    }
    tokens
}

/// Scopes for a step-up authorization: the challenged scopes plus the ones granted so far, since a
/// challenge may list only the missing scopes and a token with just those would lose access the old
/// one had (SEP-2350). Without challenged scopes, `undefined` lets the flow pick its default.
pub fn step_up_scope(granted: Option<&str>, challenged: Option<&str>) -> Option<String> {
    let challenged = challenged.filter(|c| !c.is_empty())?;
    let mut scopes = IndexSet::new();
    for src in [granted, Some(challenged)] {
        if let Some(s) = src {
            for word in s
                .split(|c: char| pi_js::str16::is_js_whitespace(c))
                .filter(|w| !w.is_empty())
            {
                scopes.insert(word.to_string());
            }
        }
    }
    Some(scopes.into_iter().collect::<Vec<_>>().join(" "))
}

/// What a flow used, for deciding what to invalidate after it failed.
#[derive(Default)]
struct FlowUse {
    /// `client_id` of the stored client the flow used; `None` when none was stored.
    stored_client_id: Option<String>,
}

async fn run_flow(
    provider: &dyn OAuthClientProvider,
    options: &OAuthFlowOptions,
    extra: &OAuthFlowExtraOptions,
    use_state: &mut FlowUse,
) -> Result<OAuthFlowResult> {
    let metadata_url = options
        .authorization_server_metadata_url
        .as_ref()
        .map(|u| secure_endpoint(u.as_str()))
        .transpose()?;

    // With a configured metadata URL, discovery is not cached, so changing the URL applies at once.
    let cached = if metadata_url.is_some() {
        None
    } else {
        provider.discovery_state().await?
    };

    let (authorization_server_url, authorization_server_metadata, resource_metadata) = match cached {
        Some(c) if !c.authorization_server_url.is_empty() => {
            let meta = match c.authorization_server_metadata {
                Some(m) => Some(m),
                None => {
                    discover_authorization_server_metadata(
                        &c.authorization_server_url,
                        Some(DiscoverAuthorizationServerMetadataOptions {
                            fetch: options.fetch.clone(),
                            protocol_version: None,
                            skip_issuer_validation: options.skip_issuer_validation,
                            signal: extra.signal.clone(),
                        }),
                    )
                    .await?
                }
            };
            (c.authorization_server_url, meta, c.resource_metadata)
        }
        _ => {
            let info = discover_oauth_server_info(
                &options.server_url,
                Some(DiscoverOAuthServerInfoOptions {
                    resource_metadata_url: options.resource_metadata_url.clone(),
                    authorization_server_metadata_url: metadata_url.clone(),
                    fetch: options.fetch.clone(),
                    skip_issuer_validation: options.skip_issuer_validation,
                    signal: extra.signal.clone(),
                }),
            )
            .await?;
            (
                info.authorization_server_url,
                info.authorization_server_metadata,
                info.resource_metadata,
            )
        }
    };

    if metadata_url.is_none() {
        provider
            .save_discovery_state(OAuthDiscoveryState {
                authorization_server_url: authorization_server_url.clone(),
                authorization_server_metadata: authorization_server_metadata.clone(),
                resource_metadata: resource_metadata.clone(),
                resource_metadata_url: options.resource_metadata_url.as_ref().map(|u| u.to_string()),
            })
            .await?;
    }

    let metadata = authorization_server_metadata;
    let resource = select_resource(&options.server_url, resource_metadata.as_ref())?;
    let scope = options
        .scope
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            resource_metadata
                .as_ref()
                .and_then(|m| m.scopes_supported.as_ref())
                .map(|s| s.join(" "))
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            provider
                .client_metadata()
                .scope
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(String::from)
        });

    let stored = provider.client_information().await?;
    use_state.stored_client_id = stored.as_ref().map(|c| c.client_id.clone());
    let client_document = if stored.is_none() {
        provider.client_metadata_document(metadata.as_ref())?
    } else {
        None
    };
    if let Some(ref doc) = client_document {
        let parsed = url::Url::parse(&doc.url).map_err(|_| Error::msg("Invalid OAuth client metadata URL"))?;
        if parsed.scheme() != "https" || parsed.path() == "/" {
            return Err(Error::msg("Invalid OAuth client metadata URL"));
        }
    }
    // The document's redirect URI may differ from the provider's, for example by a server-specific path.
    let redirect_url = client_document
        .as_ref()
        .map(|d| d.redirect_url.clone())
        .unwrap_or_else(|| provider.redirect_url());
    // Checked first: another redirect URI also explains a client that is missing for the exchange.
    if options.authorization_code.as_deref().is_some_and(|c| !c.is_empty())
        && let Some(auth_redirect_url) = extra.authorization_redirect_url.as_deref()
        && redirect_url != auth_redirect_url
    {
        return Err(Error::msg(format!(
            "The code exchange would use the redirect URI {redirect_url}, but the authorization request used {auth_redirect_url}"
        )));
    }

    let document_client = client_document.as_ref().map(|doc| {
        OAuthClientInformationMixed::from(OAuthClientInformation {
            client_id: doc.url.clone(),
            client_secret: None,
            client_id_issued_at: None,
            client_secret_expires_at: None,
        })
    });
    let client = match stored.or(document_client) {
        Some(existing) => existing,
        None => {
            if extra.refresh_only.unwrap_or(false) {
                return Ok(OAuthFlowResult::Redirect);
            }
            if options.authorization_code.as_deref().is_some_and(|c| !c.is_empty()) {
                return Err(Error::msg(
                    "OAuth client information is missing during code exchange: the authorization request's client was removed, or the client settings changed",
                ));
            }
            let registered = register_client(
                &authorization_server_url,
                RegisterClientOptions {
                    metadata: metadata.clone(),
                    client_metadata: provider.client_metadata().clone(),
                    scope: scope.clone(),
                    fetch: options.fetch.clone(),
                    signal: extra.signal.clone(),
                },
            )
            .await?;
            let mixed = OAuthClientInformationMixed::from(registered);
            provider.save_client_information(mixed.clone()).await?;
            mixed
        }
    };

    let add_client_auth = provider.add_client_authentication();

    if let Some(code) = options.authorization_code.as_deref().filter(|c| !c.is_empty()) {
        // RFC 9207: never send a code from another authorization server to this one. Without metadata there
        // is no issuer to compare with, so any `iss` is rejected.
        let iss_required = metadata
            .as_ref()
            .and_then(|m| m.authorization_response_iss_parameter_supported)
            == Some(true);
        let expected_issuer = metadata.as_ref().map(|m| m.issuer.as_str());
        if (options.iss.is_some() || iss_required) && options.iss.as_deref() != expected_issuer {
            return Err(Error::typed(
                "OAuthIssuerMismatchError",
                OAuthIssuerMismatchError::new(expected_issuer.map(str::to_string), options.iss.clone()),
            ));
        }
        let tokens = exchange_authorization_code(
            &authorization_server_url,
            ExchangeAuthorizationCodeOptions {
                metadata,
                client_information: client,
                resource,
                add_client_authentication: add_client_auth,
                fetch: options.fetch.clone(),
                signal: extra.signal.clone(),
                code: code.to_string(),
                code_verifier: provider.code_verifier().await?,
                redirect_url,
            },
        )
        .await?;
        provider.save_tokens(with_scope(tokens, scope.as_deref())).await?;
        return Ok(OAuthFlowResult::Authorized);
    }

    let existing = if options.skip_refresh.unwrap_or(false) {
        None
    } else {
        provider.tokens().await?
    };
    if let Some(ref existing_tokens) = existing
        && let Some(ref refresh_tok) = existing_tokens.refresh_token
        && !refresh_tok.is_empty()
    {
        match refresh_authorization(
            &authorization_server_url,
            RefreshAuthorizationOptions {
                metadata: metadata.clone(),
                client_information: client.clone(),
                resource: resource.clone(),
                add_client_authentication: add_client_auth.clone(),
                fetch: options.fetch.clone(),
                signal: extra.signal.clone(),
                refresh_token: refresh_tok.clone(),
            },
        )
        .await
        {
            Ok(tokens) => {
                provider
                    .save_tokens(with_scope(tokens, existing_tokens.scope.as_deref()))
                    .await?;
                return Ok(OAuthFlowResult::Authorized);
            }
            Err(err) => {
                if extra.signal.as_ref().is_some_and(AbortSignal::aborted)
                    || err.downcast_ref::<OAuthInsecureEndpointError>().is_some()
                {
                    return Err(err);
                }
                if let Some(oauth_err) = err.downcast_ref::<OAuthError>()
                    && oauth_err.code != "server_error"
                {
                    return Err(err);
                }
            }
        }
    }

    if extra.refresh_only.unwrap_or(false) {
        return Ok(OAuthFlowResult::Redirect);
    }

    let state = provider.state().await?;
    let authorization = start_authorization(
        &authorization_server_url,
        StartAuthorizationOptions {
            metadata,
            client_information: client,
            redirect_url,
            scope,
            state,
            resource,
        },
    )
    .await?;
    provider.save_code_verifier(&authorization.code_verifier).await?;
    provider
        .redirect_to_authorization(authorization.authorization_url)
        .await?;
    Ok(OAuthFlowResult::Redirect)
}

pub async fn authorize_mcp(provider: &dyn OAuthClientProvider, options: OAuthFlowOptions) -> Result<OAuthFlowResult> {
    authorize_mcp_with_options(provider, options, OAuthFlowExtraOptions::default()).await
}

// PORT: Additive entrypoint taking `OAuthFlowExtraOptions` so existing `OAuthFlowOptions` callers compile.
pub async fn authorize_mcp_with_options(
    provider: &dyn OAuthClientProvider,
    options: OAuthFlowOptions,
    extra: OAuthFlowExtraOptions,
) -> Result<OAuthFlowResult> {
    let mut use_state = FlowUse::default();
    match run_flow(provider, &options, &extra, &mut use_state).await {
        Ok(res) => Ok(res),
        Err(err) => {
            if let Some(oauth_err) = err.downcast_ref::<OAuthError>() {
                if matches!(oauth_err.code.as_str(), "invalid_client" | "unauthorized_client") {
                    // A refresh keeps a pending authorization's `state` and verifier: they are the user's sign-in.
                    // A client stored meanwhile (by a sign-in that completed) is not the one the server rejected,
                    // so it stays.
                    if extra.refresh_only.unwrap_or(false) {
                        let current_id = provider.client_information().await?.map(|c| c.client_id);
                        if current_id == use_state.stored_client_id {
                            for kind in [
                                InvalidateCredentialsKind::Client,
                                InvalidateCredentialsKind::Tokens,
                                InvalidateCredentialsKind::Discovery,
                            ] {
                                provider.invalidate_credentials(kind).await?;
                            }
                        }
                    } else {
                        provider.invalidate_credentials(InvalidateCredentialsKind::All).await?;
                    }
                    let mut retry_use = FlowUse::default();
                    return run_flow(provider, &options, &extra, &mut retry_use).await;
                }
                if oauth_err.code == "invalid_grant" {
                    provider
                        .invalidate_credentials(InvalidateCredentialsKind::Tokens)
                        .await?;
                    let mut retry_use = FlowUse::default();
                    return run_flow(provider, &options, &extra, &mut retry_use).await;
                }
            }
            Err(err)
        }
    }
}

type SharedOutcome = Option<std::result::Result<(), Arc<Error>>>;

struct AdaptedOAuthProvider {
    provider: Arc<dyn OAuthClientProvider>,
    in_flight: Arc<Mutex<Option<tokio::sync::watch::Receiver<SharedOutcome>>>>,
}

#[async_trait]
impl AuthProvider for AdaptedOAuthProvider {
    async fn token(&self) -> Result<Option<String>> {
        Ok(self.provider.tokens().await?.map(|t| t.access_token))
    }

    fn on_unauthorized(&self, context: UnauthorizedContext) -> Option<BoxFuture<Result<()>>> {
        let provider = Arc::clone(&self.provider);
        let in_flight = Arc::clone(&self.in_flight);
        Some(Box::pin(async move {
            let challenge = parse_www_authenticate(context.response.headers().get("www-authenticate").as_deref());
            context.response.cancel_body();
            let insufficient_scope = challenge.error.as_deref() == Some("insufficient_scope");
            let has_in_flight = in_flight.lock().unwrap_or_else(|e| e.into_inner()).is_some();
            if !insufficient_scope
                && !has_in_flight
                && let Some(ref used_token) = context.token
                && let Some(current) = provider.tokens().await?.map(|t| t.access_token)
                && &current != used_token
            {
                return Ok(());
            }

            let maybe_rx = {
                let mut guard = in_flight.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref rx) = *guard {
                    Err(rx.clone())
                } else {
                    let (tx, rx) = tokio::sync::watch::channel(None);
                    *guard = Some(rx);
                    Ok(tx)
                }
            };

            match maybe_rx {
                Err(mut rx) => loop {
                    if let Some(ref outcome) = *rx.borrow() {
                        return match outcome {
                            Ok(()) => Ok(()),
                            Err(err) => Err(clone_oauth_error(err)),
                        };
                    }
                    if rx.changed().await.is_err() {
                        return Err(Error::msg("OAuth refresh abandoned"));
                    }
                },
                Ok(tx) => {
                    let server_url = context.server_url.to_string();
                    let fetch = context.fetch;
                    struct ResetGuard {
                        slot: Arc<Mutex<Option<tokio::sync::watch::Receiver<SharedOutcome>>>>,
                        tx: Option<tokio::sync::watch::Sender<SharedOutcome>>,
                    }
                    impl Drop for ResetGuard {
                        fn drop(&mut self) {
                            *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
                            if let Some(tx) = self.tx.take() {
                                let _ = tx.send(Some(Err(Arc::new(Error::msg("OAuth refresh cancelled")))));
                            }
                        }
                    }
                    let mut reset = ResetGuard {
                        slot: Arc::clone(&in_flight),
                        tx: Some(tx),
                    };

                    let res: Result<()> = async {
                        let granted = if insufficient_scope {
                            provider.tokens().await?
                        } else {
                            None
                        };
                        let scope = if insufficient_scope {
                            step_up_scope(
                                granted.as_ref().and_then(|t| t.scope.as_deref()),
                                challenge.scope.as_deref(),
                            )
                        } else {
                            challenge.scope
                        };
                        let flow_result = authorize_mcp(
                            provider.as_ref(),
                            OAuthFlowOptions {
                                server_url,
                                authorization_code: None,
                                iss: None,
                                scope,
                                resource_metadata_url: challenge.resource_metadata_url,
                                authorization_server_metadata_url: None,
                                fetch: Some(fetch),
                                skip_issuer_validation: None,
                                skip_refresh: Some(insufficient_scope),
                            },
                        )
                        .await?;
                        if flow_result == OAuthFlowResult::Redirect {
                            return Err(Error::typed(
                                "McpOAuthAuthorizationRequiredError",
                                McpOAuthAuthorizationRequiredError::new(),
                            ));
                        }
                        Ok(())
                    }
                    .await;

                    *reset.slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    if let Some(tx) = reset.tx.take() {
                        match &res {
                            Ok(()) => {
                                let _ = tx.send(Some(Ok(())));
                            }
                            Err(e) => {
                                let _ = tx.send(Some(Err(Arc::new(clone_oauth_error(e)))));
                            }
                        }
                    }
                    res
                }
            }
        }))
    }
}

/// Auth provider for `StreamableHttpTransport`. After a 401 it refreshes the tokens, or throws
/// `McpOAuthAuthorizationRequiredError` when the user has to authorize (again). Concurrent 401s share
/// one refresh, and a request whose token was already replaced is just retried: with rotating refresh
/// tokens, a second refresh with the old refresh token would fail and discard the new grant.
pub fn adapt_oauth_provider(provider: Arc<dyn OAuthClientProvider>) -> Arc<dyn AuthProvider + Send + Sync> {
    Arc::new(AdaptedOAuthProvider {
        provider,
        in_flight: Arc::new(Mutex::new(None)),
    })
}
