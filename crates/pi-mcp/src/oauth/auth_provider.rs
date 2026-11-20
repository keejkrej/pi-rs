//! Port of packages/mcp/src/oauth/auth-provider.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::auth_provider::{AuthProvider, McpFetch, UnauthorizedContext};
use crate::oauth::client_settings::{
    DEFAULT_REDIRECT_URL, McpClientMetadataDocuments, McpOAuthClientOptions, McpOAuthServerStore,
    McpOAuthServerStoreExt, McpOAuthSettings, create_oauth_client_provider, loopback_settings,
    registered_redirect_urls, timed_fetch,
};
use crate::oauth::discovery::parse_www_authenticate;
use crate::oauth::errors::McpOAuthAuthorizationRequiredError;
use crate::oauth::flow::{
    OAuthFlowExtraOptions, OAuthFlowOptions, OAuthFlowResult, authorize_mcp_with_options, clone_oauth_error,
};
use crate::oauth::provider::McpOAuthState;
use crate::oauth::types::OAuthChallenge;
use pi_js::{BoxFuture, Error, Result};

/// An `AuthProvider` that can say when its refreshes have finished.
#[async_trait]
pub trait McpAuthProvider: AuthProvider {
    /// Resolves when no refresh is running, so shutdown does not drop rotated tokens before they are saved.
    async fn settled(&self);
}

/// Access tokens this close to expiry are refreshed before they are sent.
const REFRESH_SKEW_MS: i64 = 30_000;

pub type McpSettingsFn = Arc<dyn Fn() -> Result<McpOAuthSettings> + Send + Sync>;
pub type OnChallengeFn = Arc<dyn Fn(&OAuthChallenge) + Send + Sync>;

#[derive(Clone)]
pub struct CreateMcpAuthProviderOptions {
    pub server_url: String,
    pub store: Arc<dyn McpOAuthServerStore>,
    pub settings: McpSettingsFn,
    pub client_name: String,
    pub client_metadata_documents: Option<Arc<dyn McpClientMetadataDocuments>>,
    pub on_challenge: OnChallengeFn,
    /// A redirect URI the application signs in with, such as its https callback route. Refreshing tokens
    /// from a sign-in with a Client ID Metadata Document needs a redirect URI the documents list; a
    /// refresh never redirects, so any listed one works. Default: the loopback redirect URI of `settings`.
    pub redirect_url: Option<String>,
}

#[derive(Clone, Default)]
pub struct RefreshStoredTokensOptions {
    pub fetch: Option<McpFetch>,
    pub challenge: Option<OAuthChallenge>,
    pub redirect_url: Option<String>,
}

type SharedOutcome = Option<std::result::Result<(), Arc<Error>>>;

struct StdMcpAuthProvider {
    options: CreateMcpAuthProviderOptions,
    refreshing: Arc<Mutex<Option<tokio::sync::watch::Receiver<SharedOutcome>>>>,
}

impl StdMcpAuthProvider {
    async fn wait_for_receiver(mut rx: tokio::sync::watch::Receiver<SharedOutcome>) -> Result<()> {
        loop {
            if let Some(ref outcome) = *rx.borrow() {
                return match outcome {
                    Ok(()) => Ok(()),
                    Err(err) => Err(clone_oauth_error(err)),
                };
            }
            if rx.changed().await.is_err() {
                return Err(Error::msg("OAuth refresh abandoned"));
            }
        }
    }

    /// Replace `stale_token`, the access token that expired or was rejected.
    async fn refresh(
        &self,
        stale_token: Option<String>,
        fetch: Option<McpFetch>,
        challenge: Option<OAuthChallenge>,
    ) -> Result<()> {
        enum RefreshAction {
            Wait(tokio::sync::watch::Receiver<SharedOutcome>),
            Run(tokio::sync::watch::Sender<SharedOutcome>),
        }

        let action = {
            let mut guard = self.refreshing.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref rx) = *guard {
                RefreshAction::Wait(rx.clone())
            } else {
                let (tx, rx) = tokio::sync::watch::channel(None);
                *guard = Some(rx);
                RefreshAction::Run(tx)
            }
        };

        match action {
            RefreshAction::Wait(rx) => Self::wait_for_receiver(rx).await,
            RefreshAction::Run(tx) => {
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

                let slot = Arc::clone(&self.refreshing);
                let options = self.options.clone();
                let handle = tokio::spawn(async move {
                    let mut reset = ResetGuard { slot, tx: Some(tx) };
                    let store = options.store.clone();
                    let store_inner = store.clone();
                    let server_url = options.server_url.clone();
                    let client_name = options.client_name.clone();
                    let docs = options.client_metadata_documents.clone();
                    let settings_fn = options.settings.clone();
                    let redirect_url = options.redirect_url.clone();

                    let res: Result<()> = store
                        .with_refresh_lock(move || async move {
                            let state = store_inner.load().await?;
                            let current_access = state
                                .as_ref()
                                .and_then(|s| s.tokens.as_ref())
                                .map(|t| t.access_token.as_str());
                            if current_access != stale_token.as_deref() {
                                return Ok(());
                            }
                            let has_refresh = state
                                .as_ref()
                                .and_then(|s| s.tokens.as_ref())
                                .and_then(|t| t.refresh_token.as_deref())
                                .is_some_and(|r| !r.is_empty());
                            if !has_refresh {
                                return Err(Error::typed(
                                    "McpOAuthAuthorizationRequiredError",
                                    McpOAuthAuthorizationRequiredError::new(),
                                ));
                            }
                            let state = state.expect("state present when refresh_token exists");
                            let settings = settings_fn()?;
                            let client = McpOAuthClientOptions {
                                server_url,
                                store: store_inner.clone(),
                                settings,
                                client_name,
                                client_metadata_documents: docs,
                            };
                            let result = refresh_stored_tokens(
                                &client,
                                &state,
                                RefreshStoredTokensOptions {
                                    fetch,
                                    challenge,
                                    redirect_url,
                                },
                            )
                            .await?;
                            if result == OAuthFlowResult::Redirect {
                                return Err(Error::typed(
                                    "McpOAuthAuthorizationRequiredError",
                                    McpOAuthAuthorizationRequiredError::new(),
                                ));
                            }
                            Ok(())
                        })
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
                });

                handle
                    .await
                    .unwrap_or_else(|_| Err(Error::msg("OAuth refresh task panicked")))
            }
        }
    }
}

#[async_trait]
impl AuthProvider for StdMcpAuthProvider {
    async fn token(&self) -> Result<Option<String>> {
        self.settled().await;
        let state = self.options.store.load().await?;
        let token = state
            .as_ref()
            .and_then(|s| s.tokens.as_ref())
            .map(|t| t.access_token.clone());
        let expired = state
            .as_ref()
            .and_then(|s| s.tokens_expire_at)
            .is_some_and(|exp| exp - REFRESH_SKEW_MS <= pi_js::time::now_ms());
        let has_refresh = state
            .as_ref()
            .and_then(|s| s.tokens.as_ref())
            .and_then(|t| t.refresh_token.as_deref())
            .is_some_and(|r| !r.is_empty());
        if !expired || !has_refresh {
            return Ok(token);
        }
        // Failures fall through: the request goes out with the old token and a 401 decides what happens.
        let _ = self.refresh(token, None, None).await;
        Ok(self
            .options
            .store
            .load()
            .await?
            .and_then(|s| s.tokens)
            .map(|t| t.access_token))
    }

    fn on_unauthorized(&self, context: UnauthorizedContext) -> Option<BoxFuture<Result<()>>> {
        let on_challenge = Arc::clone(&self.options.on_challenge);
        let provider = Self {
            options: self.options.clone(),
            refreshing: Arc::clone(&self.refreshing),
        };
        Some(Box::pin(async move {
            let challenge = parse_www_authenticate(context.response.headers().get("www-authenticate").as_deref());
            context.response.cancel_body();
            on_challenge(&challenge);
            // A refresh keeps the granted scope, so more scope needs a new sign-in.
            if challenge.error.as_deref() == Some("insufficient_scope") {
                return Err(Error::typed(
                    "McpOAuthAuthorizationRequiredError",
                    McpOAuthAuthorizationRequiredError::new(),
                ));
            }
            provider
                .refresh(context.token, Some(context.fetch), Some(challenge))
                .await
        }))
    }
}

#[async_trait]
impl McpAuthProvider for StdMcpAuthProvider {
    async fn settled(&self) {
        let rx = self.refreshing.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(rx) = rx {
            let _ = Self::wait_for_receiver(rx).await;
        }
    }
}

/// Auth provider for MCP connections: sends the stored access token and refreshes it when it is about
/// to expire or after a 401. Throws `McpOAuthAuthorizationRequiredError` when the user has to sign in,
/// including when the server asks for more scope (`insufficient_scope`). `onChallenge` receives the
/// server's `WWW-Authenticate` challenge so sign-in can use its resource metadata URL and scope.
/// `settings` is only called when a refresh is needed, so a secret that fails to resolve fails the
/// refresh instead of the whole connection setup.
///
/// Many servers rotate refresh tokens, so two refreshes with the same refresh token lose the grant.
/// Requests in this process share one refresh, and other processes are kept out by the store's
/// refresh lock, held from reading the tokens to saving new ones. Tokens that changed meanwhile
/// (another process refreshed them, or the user signed in) are used without refreshing.
pub fn create_mcp_auth_provider(options: CreateMcpAuthProviderOptions) -> Arc<dyn McpAuthProvider + Send + Sync> {
    Arc::new(StdMcpAuthProvider {
        options,
        refreshing: Arc::new(Mutex::new(None)),
    })
}

/// Refresh the tokens of `state` with the stored client (or the document it signed in with), or report
/// `REDIRECT` when that is not possible. `options.redirectUrl` is a redirect URI the application signs in
/// with, tried before the loopback ones of the settings and the stored client. Never registers a client or
/// prepares an authorization request, so a pending sign-in stays usable. Call while holding the store's
/// refresh lock. Not cancellable: a refresh the server answered may have rotated the refresh token, so its
/// answer must be saved.
pub async fn refresh_stored_tokens(
    client: &McpOAuthClientOptions,
    state: &McpOAuthState,
    options: RefreshStoredTokensOptions,
) -> Result<OAuthFlowResult> {
    // Refreshing never redirects the user; the redirect URI only decides whether tokens without a stored
    // client can be refreshed with a Client ID Metadata Document, so any URI the documents list works.
    let mut candidates = Vec::new();
    if let Some(url) = options.redirect_url {
        candidates.push(url);
    }
    if let Ok(lb) = loopback_settings(&client.settings)
        && let Some(fixed) = lb.fixed_redirect_url
    {
        candidates.push(fixed);
    }
    if let Some(first) = registered_redirect_urls(state.client_information.as_ref()).first() {
        candidates.push(first.clone());
    }
    let redirect_url = candidates
        .into_iter()
        .find(|url| {
            client
                .client_metadata_documents
                .as_ref()
                .is_some_and(|docs| docs.lists_redirect_url(url))
        })
        .unwrap_or_else(|| DEFAULT_REDIRECT_URL.to_string());

    authorize_mcp_with_options(
        &create_oauth_client_provider(client, &redirect_url, None),
        OAuthFlowOptions {
            server_url: client.server_url.clone(),
            authorization_code: None,
            iss: None,
            scope: None,
            resource_metadata_url: options.challenge.and_then(|c| c.resource_metadata_url),
            authorization_server_metadata_url: client.settings.auth_server_metadata_url.clone(),
            fetch: Some(timed_fetch(options.fetch)),
            skip_issuer_validation: None,
            skip_refresh: None,
        },
        OAuthFlowExtraOptions {
            signal: None,
            refresh_only: Some(true),
            authorization_redirect_url: None,
        },
    )
    .await
}
