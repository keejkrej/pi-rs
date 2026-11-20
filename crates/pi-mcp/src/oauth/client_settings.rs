//! Port of packages/mcp/src/oauth/client-settings.ts

#![allow(dead_code, unused_variables)]

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::auth_provider::McpFetch;
use crate::oauth::flow::OAuthClientMetadataDocument;
use crate::oauth::provider::{
    ClientMetadataDocumentFn, McpOAuthProvider, McpOAuthProviderClientMetadata, McpOAuthProviderOptions,
    McpOAuthStateStore,
};
use crate::oauth::types::{AuthorizationServerMetadata, OAuthClientInformationMixed};
use pi_js::abort::AbortSignal;
use pi_js::fetch::Request;
use pi_js::{BoxFuture, Error, Result};

/// How the application identifies itself to an authorization server without a configured `clientId`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum McpClientRegistration {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "dcr")]
    Dcr,
    #[serde(rename = "cimd")]
    Cimd,
}

/// Client settings of one MCP server, usually from the application's configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpOAuthSettings {
    /// Pre-registered client id. Without it, sign-in uses a Client ID Metadata Document or registers a client.
    pub client_id: Option<String>,
    /// Already resolved.
    pub client_secret: Option<String>,
    /// Port of the loopback callback server, for clients registered with a fixed redirect URI. Without
    /// `callbackUrl`, the redirect URI is `http://127.0.0.1:<port>/callback`. Only `listenForLoopbackSignIn`
    /// and the redirect URI of refreshes use it; `startMcpSignIn` and `completeMcpSignIn` take theirs from
    /// the caller.
    pub callback_port: Option<u16>,
    /// Loopback redirect URI registered for `clientId`, for example `http://localhost:8080/oauth/callback`.
    /// Without a port, the callback server listens on `callbackPort` or a free port, which is added to the
    /// URI (RFC 8252). Default: `http://127.0.0.1/callback`. Like `callbackPort`, only `listenForLoopbackSignIn`
    /// and refreshes use it.
    pub callback_url: Option<String>,
    /// Scopes to request, separated by spaces. Default: the scopes the server advertises.
    pub scope: Option<String>,
    /// `client_name` for dynamic client registration. Default: the application's `clientName`.
    pub client_name: Option<String>,
    /// How to identify without `clientId`.
    pub client_registration: Option<McpClientRegistration>,
    /// Authorization server metadata document to use instead of discovery through the server, for servers
    /// that advertise a wrong authorization server or none. It is trusted as configured.
    pub auth_server_metadata_url: Option<url::Url>,
}

/// The Client ID Metadata Documents an application publishes: `<baseUrl>/client.json`, and for each MCP
/// server `<baseUrl>/<callback ID>/client.json`, whose redirect URIs carry the path `<path>/<callback ID>`.
pub trait McpClientMetadataDocuments: Send + Sync {
    /// For example `https://pi.dev/oauth`.
    fn base_url(&self) -> &str;
    /// Whether the documents list `redirectUrl`. Loopback redirect URIs match on any port (RFC 8252).
    fn lists_redirect_url(&self, redirect_url: &str) -> bool;
}

/// State store of one MCP server, with a lock that keeps other processes from refreshing at the same time.
#[async_trait]
pub trait McpOAuthServerStore: McpOAuthStateStore {
    /// Run `work` while no other process refreshes the server's tokens. Not reentrant: `startMcpSignIn` and
    /// `createMcpAuthProvider` take it themselves, so never call them from `work`. Fail with
    /// [`crate::oauth::errors::McpRefreshLockBusyError`] when another process holds the lock for the whole wait.
    // PORT: Object-safe boxed future because `Arc<dyn McpOAuthServerStore>` cannot invoke generic async methods directly.
    async fn with_refresh_lock_boxed(
        &self,
        work: Box<dyn FnOnce() -> BoxFuture<Result<Box<dyn std::any::Any + Send>>> + Send>,
    ) -> Result<Box<dyn std::any::Any + Send>>;
}

/// Typed convenience helper for [`McpOAuthServerStore::with_refresh_lock_boxed`].
#[async_trait]
pub trait McpOAuthServerStoreExt: McpOAuthServerStore {
    async fn with_refresh_lock<T, F, Fut>(&self, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let boxed = self
            .with_refresh_lock_boxed(Box::new(move || {
                Box::pin(async move {
                    let value = work().await?;
                    Ok(Box::new(value) as Box<dyn std::any::Any + Send>)
                })
            }))
            .await?;
        Ok(*boxed
            .downcast::<T>()
            .expect("with_refresh_lock_boxed returned unexpected type"))
    }
}

impl<S: McpOAuthServerStore + ?Sized> McpOAuthServerStoreExt for S {}

pub type OnRedirectCallback = Arc<dyn Fn(url::Url) -> BoxFuture<Result<()>> + Send + Sync>;

/// The OAuth client of one MCP server: where its state lives, its settings, and the application's identity.
#[derive(Clone)]
pub struct McpOAuthClientOptions {
    pub server_url: String,
    pub store: Arc<dyn McpOAuthServerStore>,
    pub settings: McpOAuthSettings,
    /// `client_name` for dynamic client registration unless `settings.clientName` names another: the application.
    pub client_name: String,
    /// Omitted: dynamic client registration only.
    pub client_metadata_documents: Option<Arc<dyn McpClientMetadataDocuments>>,
}

/// An `McpOAuthProvider` for the client, redirecting to `redirectUrl` or the document's variant of it.
pub fn create_oauth_client_provider(
    options: &McpOAuthClientOptions,
    redirect_url: &str,
    on_redirect: Option<OnRedirectCallback>,
) -> McpOAuthProvider {
    let settings = &options.settings;
    let documents = &options.client_metadata_documents;
    let client_name = settings
        .client_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&options.client_name)
        .to_string();
    let redirect_owned = redirect_url.to_string();
    let doc_fn: Option<ClientMetadataDocumentFn> = if offers_client_metadata_document(
        settings,
        documents.as_deref(),
        redirect_url,
    ) {
        let docs_opt = documents.clone();
        let redirect_for_doc = redirect_owned.clone();
        let server_owned = options.server_url.clone();
        let registration = settings.client_registration;
        Some(Arc::new(move |metadata| {
            let Some(docs) = &docs_opt else {
                return Err(Error::msg(format!(
                    "oauth.clientRegistration \"cimd\" needs Client ID Metadata Documents that list the redirect URI {redirect_for_doc}"
                )));
            };
            if !docs.lists_redirect_url(&redirect_for_doc) {
                return Err(Error::msg(format!(
                    "oauth.clientRegistration \"cimd\" needs Client ID Metadata Documents that list the redirect URI {redirect_for_doc}"
                )));
            }
            let document = client_metadata_document(&server_owned, &redirect_for_doc, metadata, docs.base_url());
            if document.is_none() && registration == Some(McpClientRegistration::Cimd) {
                return Err(Error::msg(
                    "The authorization server does not support Client ID Metadata Documents for public clients; remove oauth.clientRegistration \"cimd\"",
                ));
            }
            Ok(document)
        }))
    } else {
        None
    };

    let on_redirect = on_redirect.unwrap_or_else(|| Arc::new(|_url| Box::pin(async { Ok(()) })));
    McpOAuthProvider::new_with_document(
        McpOAuthProviderOptions {
            server_url: options.server_url.clone(),
            redirect_url: redirect_owned,
            client_metadata: McpOAuthProviderClientMetadata {
                client_name: Some(client_name),
                ..Default::default()
            },
            client_id: settings.client_id.clone(),
            client_secret: settings.client_secret.clone(),
            store: Some(options.store.clone()),
            on_redirect,
        },
        doc_fn,
    )
}

/// Whether the client may identify with a Client ID Metadata Document instead of registering. `auto`
/// registers when the user asked for a client name or the documents do not list the redirect URI.
pub fn offers_client_metadata_document(
    settings: &McpOAuthSettings,
    documents: Option<&dyn McpClientMetadataDocuments>,
    redirect_url: &str,
) -> bool {
    let registration = settings.client_registration.unwrap_or(McpClientRegistration::Auto);
    if registration != McpClientRegistration::Auto {
        return registration == McpClientRegistration::Cimd;
    }
    settings.client_id.as_deref().is_none_or(str::is_empty)
        && settings.client_name.as_deref().is_none_or(str::is_empty)
        && documents.is_some_and(|d| d.lists_redirect_url(redirect_url))
}

/// The Client ID Metadata Document to use, or `None` when the authorization server does not support documents
/// for public clients. Without the `iss` parameter in authorization responses (RFC 9207), the redirect URI
/// and the document are specific to the MCP server, so a response cannot be mixed up with one from another
/// authorization server (RFC 9700 section 4.4.2.2).
fn client_metadata_document(
    server_url: &str,
    redirect_url: &str,
    metadata: Option<&AuthorizationServerMetadata>,
    base_url: &str,
) -> Option<OAuthClientMetadataDocument> {
    let meta = metadata?;
    if meta.client_id_metadata_document_supported != Some(true)
        || !meta
            .token_endpoint_auth_methods_supported
            .as_ref()
            .is_some_and(|methods| methods.iter().any(|m| m == "none"))
    {
        return None;
    }
    if meta.authorization_response_iss_parameter_supported == Some(true) {
        return Some(OAuthClientMetadataDocument {
            url: format!("{base_url}/client.json"),
            redirect_url: redirect_url.to_string(),
        });
    }
    let id = callback_id(server_url).ok()?;
    let mut redirect = url::Url::parse(redirect_url).ok()?;
    let next_path = server_specific_path(redirect.path(), server_url).ok()?;
    redirect.set_path(&next_path);
    Some(OAuthClientMetadataDocument {
        url: format!("{base_url}/{id}/client.json"),
        redirect_url: redirect.to_string(),
    })
}

/// The path of the redirect URI of a server-specific Client ID Metadata Document: `<path>/<callback ID>`.
pub fn server_specific_path(path: &str, server_url: &str) -> Result<String> {
    let base = path.strip_suffix('/').unwrap_or(path);
    Ok(format!("{base}/{}", callback_id(server_url)?))
}

/// 12 characters identifying an MCP server URL in callback paths, computed like Codex does.
fn callback_id(server_url: &str) -> Result<String> {
    let mut url = url::Url::parse(server_url).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
    url.set_fragment(None);
    let digest = pi_js::crypto::sha256(url.as_str().as_bytes());
    Ok(pi_js::b64::encode_url(&digest[..9]))
}

/// Redirect URIs a stored client was registered with.
pub fn registered_redirect_urls(client: Option<&OAuthClientInformationMixed>) -> &[String] {
    client.and_then(|c| c.redirect_uris.as_deref()).unwrap_or(&[])
}

/// Bounds each request to the authorization server, so an unresponsive one cannot hold the refresh lock,
/// delay shutdown, or keep a sign-in waiting for long.
const OAUTH_REQUEST_TIMEOUT_MS: u64 = 15_000;

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

/// `fetch` with `OAUTH_REQUEST_TIMEOUT_MS` per request, on top of the request's own signal.
pub fn timed_fetch(fetch: Option<McpFetch>) -> McpFetch {
    let inner = fetch.unwrap_or_else(default_fetch);
    Arc::new(move |input, init| {
        let inner = inner.clone();
        let url = input.to_string();
        Box::pin(async move {
            let timeout = AbortSignal::timeout(OAUTH_REQUEST_TIMEOUT_MS);
            let mut next = init.unwrap_or_default();
            next.signal = Some(match next.signal {
                Some(sig) => AbortSignal::any(&[sig, timeout]),
                None => timeout,
            });
            inner(&url, Some(next)).await
        })
    })
}

const CALLBACK_HOST: &str = "127.0.0.1";
const CALLBACK_PATH: &str = "/callback";

/// Redirect URI when the settings name none and no client is stored.
pub const DEFAULT_REDIRECT_URL: &str = "http://127.0.0.1/callback";

/// Where the loopback callback server listens and the redirect URI it serves, from `callbackUrl` and `callbackPort`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopbackSettings {
    /// The redirect URI, without the port unless one is configured.
    pub url: String,
    /// Address to listen on.
    pub host: String,
    /// Host name in the redirect URI.
    pub redirect_host: String,
    pub port: Option<u16>,
    pub path: String,
    /// The exact redirect URI, when the port is fixed.
    pub fixed_redirect_url: Option<String>,
}

/// The loopback callback settings of `settings`.
pub fn loopback_settings(settings: &McpOAuthSettings) -> Result<LoopbackSettings> {
    let configured = settings.callback_url.as_deref().unwrap_or(DEFAULT_REDIRECT_URL);
    let mut url = url::Url::parse(configured).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
    let raw_host = url.host_str().unwrap_or("");
    let address = raw_host.trim_start_matches('[').trim_end_matches(']').to_string();
    let url_port = url.port();
    let port = url_port.or(settings.callback_port);
    let mut fixed_redirect_url: Option<String> = None;
    // A configured URI with a port is sent exactly as written, since servers compare it as a string.
    if url_port.is_some() {
        fixed_redirect_url = settings.callback_url.clone();
    } else if let Some(p) = port {
        let _ = url.set_port(Some(p));
        fixed_redirect_url = Some(url.to_string());
    }
    let host = if address == "localhost" {
        CALLBACK_HOST.to_string()
    } else {
        address.clone()
    };
    Ok(LoopbackSettings {
        url: configured.to_string(),
        host,
        redirect_host: address,
        port,
        path: url.path().to_string(),
        fixed_redirect_url,
    })
}
