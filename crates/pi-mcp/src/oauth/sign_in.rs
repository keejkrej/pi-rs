//! Port of packages/mcp/src/oauth/sign-in.ts

#![allow(dead_code, unused_variables)]

use std::future::Future;
use std::sync::{Arc, Mutex};

use indexmap::IndexSet;
use serde::{Deserialize, Serialize};

use crate::oauth::auth_provider::{RefreshStoredTokensOptions, refresh_stored_tokens};
use crate::oauth::callback::OAuthCallback;
use crate::oauth::client_settings::{
    McpClientMetadataDocuments, McpClientRegistration, McpOAuthClientOptions, McpOAuthServerStore,
    McpOAuthServerStoreExt, McpOAuthSettings, create_oauth_client_provider, offers_client_metadata_document,
    registered_redirect_urls, timed_fetch,
};
use crate::oauth::errors::McpRefreshLockBusyError;
use crate::oauth::flow::{
    OAuthClientProvider, OAuthFlowExtraOptions, OAuthFlowOptions, OAuthFlowResult, authorize_mcp_with_options,
    step_up_scope,
};
use crate::oauth::provider::McpOAuthState;
use crate::oauth::types::OAuthChallenge;
use pi_js::abort::AbortSignal;
use pi_js::{Error, Result};

/// An authorization response: from a callback server, a callback route, or `responseFromRedirectUrl`.
pub type McpAuthorizationResponse = OAuthCallback;

/// Options of both halves of a sign-in. Pass the same values to `startMcpSignIn` and `completeMcpSignIn`.
#[derive(Clone)]
pub struct McpSignInOptions {
    pub server_url: String,
    pub store: Arc<dyn McpOAuthServerStore>,
    pub settings: McpOAuthSettings,
    pub client_name: String,
    pub client_metadata_documents: Option<Arc<dyn McpClientMetadataDocuments>>,
    /// Redirect URI chosen by the caller: a loopback one (`listenForLoopbackSignIn`) or an https callback
    /// route of the application. Pass the same one to both halves: the authorization request may use a
    /// variant specific to the MCP server (`McpSignInRedirect.redirectUrl`), which the flow derives from it.
    pub redirect_url: String,
    /// The server's challenge that asked for the sign-in; its resource metadata URL and scope apply.
    pub challenge: Option<OAuthChallenge>,
    /// Aborting stops the sign-in at once with `McpSignInCancelledError`, also while `startMcpSignIn` waits
    /// for the store's refresh lock. A refresh already sent still completes and saves its tokens.
    pub signal: Option<AbortSignal>,
}

impl McpSignInOptions {
    fn client_options(&self) -> McpOAuthClientOptions {
        McpOAuthClientOptions {
            server_url: self.server_url.clone(),
            store: self.store.clone(),
            settings: self.settings.clone(),
            client_name: self.client_name.clone(),
            client_metadata_documents: self.client_metadata_documents.clone(),
        }
    }
}

/// The browser step of a sign-in, from `startMcpSignIn`. Plain data, so an application can persist it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSignInRedirect {
    /// Open in the user's browser.
    pub authorization_url: String,
    /// The redirect URI of the authorization request; the authorization response must arrive on it.
    pub redirect_url: String,
    /// The `state` parameter the authorization response must carry.
    pub state: String,
    /// The scope requested, if any. Saved with the tokens when the token response names none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// Result of `startMcpSignIn`: signed in by refreshing the stored tokens, or the browser step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpSignInStart {
    Authorized,
    Redirect(McpSignInRedirect),
}

/// The user or the application cancelled a sign-in (its `signal` was aborted, or the user gave up).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct McpSignInCancelledError {
    pub message: String,
}

impl McpSignInCancelledError {
    pub const NAME: &'static str = "McpSignInCancelledError";

    pub fn new() -> Self {
        Self {
            message: "Sign-in cancelled".to_string(),
        }
    }
}

impl Default for McpSignInCancelledError {
    fn default() -> Self {
        Self::new()
    }
}

fn cancelled_error() -> Error {
    Error::typed(McpSignInCancelledError::NAME, McpSignInCancelledError::new())
}

/// First half of a sign-in. Without a step-up (`insufficient_scope`), it first tries to refresh the stored
/// tokens with the stored client, whatever `redirectUrl` is (`authorized`). The refresh takes the store's
/// refresh lock, so never call this while holding it; when another process holds the lock for the whole
/// wait (`McpRefreshLockBusyError`), the refresh is skipped, and any other failure of the lock fails the
/// sign-in. Otherwise it prepares the browser step (`redirect`): a fresh `state`, and a stored client that
/// cannot use `redirectUrl` is dropped with its tokens. Requests to the authorization server are
/// time-limited.
pub async fn start_mcp_sign_in(options: McpSignInOptions) -> Result<McpSignInStart> {
    if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Err(cancelled_error());
    }
    // A refresh keeps the granted scope; a server asking for more needs the browser step.
    let step_up = options.challenge.as_ref().and_then(|c| c.error.as_deref()) == Some("insufficient_scope");
    if !step_up && refresh_before_sign_in(&options).await? == OAuthFlowResult::Authorized {
        return Ok(McpSignInStart::Authorized);
    }
    cancellable(options.signal.as_ref(), start_browser_step(&options, step_up))
        .await
        .map(McpSignInStart::Redirect)
}

/// Refresh the stored tokens, unless `cimd` is to replace a registered client. The refresh uses
/// `redirectUrl` when the tokens came from a Client ID Metadata Document whose documents list it.
async fn refresh_before_sign_in(options: &McpSignInOptions) -> Result<OAuthFlowResult> {
    let options_owned = options.clone();
    let handle = tokio::spawn(async move {
        let signal = options_owned.signal.clone();
        let client = options_owned.client_options();
        let challenge = options_owned.challenge.clone();
        let redirect_url = options_owned.redirect_url.clone();
        let store = options_owned.store.clone();
        let store_inner = store.clone();
        let settings = options_owned.settings.clone();

        store
            .with_refresh_lock(move || async move {
                // The lock may arrive after the sign-in was cancelled: return at once, so it is released.
                if signal.as_ref().is_some_and(AbortSignal::aborted) {
                    return Ok(OAuthFlowResult::Redirect);
                }
                let state = store_inner.load().await?;
                let has_refresh = state
                    .as_ref()
                    .and_then(|s| s.tokens.as_ref())
                    .and_then(|t| t.refresh_token.as_deref())
                    .is_some_and(|r| !r.is_empty());
                if !has_refresh {
                    return Ok(OAuthFlowResult::Redirect);
                }
                let state = state.expect("state present when refresh_token exists");
                if settings.client_registration == Some(McpClientRegistration::Cimd)
                    && state.client_information.is_some()
                    && settings.client_id.as_deref().is_none_or(str::is_empty)
                {
                    return Ok(OAuthFlowResult::Redirect);
                }
                refresh_stored_tokens(
                    &client,
                    &state,
                    RefreshStoredTokensOptions {
                        fetch: None,
                        challenge,
                        redirect_url: Some(redirect_url),
                    },
                )
                .await
            })
            .await
    });

    let result = if let Some(sig) = options.signal.as_ref() {
        tokio::select! {
            joined = handle => joined.unwrap_or_else(|_| Err(Error::msg("OAuth refresh lock task panicked"))),
            () = sig.cancelled() => return Err(cancelled_error()),
        }
    } else {
        handle
            .await
            .unwrap_or_else(|_| Err(Error::msg("OAuth refresh lock task panicked")))
    };

    match result {
        Ok(flow) => Ok(flow),
        // Another process is refreshing and took too long. The user signs in instead.
        Err(err) if err.downcast_ref::<McpRefreshLockBusyError>().is_some() => Ok(OAuthFlowResult::Redirect),
        Err(err) => Err(err),
    }
}

async fn start_browser_step(options: &McpSignInOptions, step_up: bool) -> Result<McpSignInRedirect> {
    let stored = options.store.load().await?;
    if let Some(ref existing) = stored {
        options.store.save(state_for_sign_in(existing, options)).await?;
    }
    let auth_url_slot: Arc<Mutex<Option<url::Url>>> = Arc::new(Mutex::new(None));
    let auth_url_writer = Arc::clone(&auth_url_slot);
    let provider = create_oauth_client_provider(
        &options.client_options(),
        &options.redirect_url,
        Some(Arc::new(move |url| {
            *auth_url_writer.lock().unwrap_or_else(|e| e.into_inner()) = Some(url);
            Box::pin(async { Ok(()) })
        })),
    );
    // A server asking for more scope gets it on top of the configured scope and, since the challenge
    // may list only the missing scopes, on top of the scope granted so far.
    let challenge_scope = options.challenge.as_ref().and_then(|c| c.scope.as_deref());
    let granted_scope = stored
        .as_ref()
        .and_then(|s| s.tokens.as_ref())
        .and_then(|t| t.scope.as_deref());
    let extra_scope = if step_up {
        step_up_scope(granted_scope, challenge_scope)
    } else {
        challenge_scope.map(str::to_string)
    };
    let scope = merge_scopes(&[options.settings.scope.as_deref(), extra_scope.as_deref()]);
    let (flow_opts, extra_opts) = flow_options(options);
    authorize_mcp_with_options(
        &provider,
        OAuthFlowOptions {
            scope,
            skip_refresh: Some(true),
            ..flow_opts
        },
        extra_opts,
    )
    .await?;

    let authorization_url = auth_url_slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .ok_or_else(|| Error::msg("OAuth flow did not produce an authorization URL"))?;
    let redirect_url = authorization_url
        .query_pairs()
        .find(|(k, _)| k == "redirect_uri")
        .map(|(_, v)| v.into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| options.redirect_url.clone());
    let requested_scope = authorization_url
        .query_pairs()
        .find(|(k, _)| k == "scope")
        .map(|(_, v)| v.into_owned())
        .filter(|s| !s.is_empty());
    let state = provider.state().await?.unwrap_or_default();
    Ok(McpSignInRedirect {
        authorization_url: authorization_url.to_string(),
        redirect_url,
        state,
        scope: requested_scope,
    })
}

/// The stored state for a browser step: without the last sign-in's `state`, and without a client that
/// cannot use `redirectUrl`, along with its tokens.
fn state_for_sign_in(stored: &McpOAuthState, options: &McpSignInOptions) -> McpOAuthState {
    let settings = &options.settings;
    let redirect_url = &options.redirect_url;
    let mut next = stored.clone();
    next.oauth_state = None;
    // A registered client cannot use another redirect URI, and its tokens belong to it. `cimd` replaces a
    // registered client; `auto` keeps it. A Client ID Metadata Document is not stored, so tokens without
    // a stored client belong to the document.
    let has_configured_id = settings.client_id.as_deref().is_some_and(|s| !s.is_empty());
    let keep_client = has_configured_id
        || match stored.client_information.as_ref() {
            Some(client_info) => {
                settings.client_registration != Some(McpClientRegistration::Cimd)
                    && registered_redirect_urls(Some(client_info))
                        .iter()
                        .any(|u| u == redirect_url)
            }
            None => {
                offers_client_metadata_document(settings, options.client_metadata_documents.as_deref(), redirect_url)
            }
        };
    if !keep_client {
        next.client_information = None;
        next.tokens = None;
        next.tokens_expire_at = None;
    }
    next
}

/// Scopes of both lists, each once.
fn merge_scopes(scopes: &[Option<&str>]) -> Option<String> {
    let mut merged = IndexSet::new();
    for src in scopes.iter().flatten() {
        for word in src
            .split(|c: char| pi_js::str16::is_js_whitespace(c))
            .filter(|w| !w.is_empty())
        {
            merged.insert(word.to_string());
        }
    }
    if merged.is_empty() {
        None
    } else {
        Some(merged.into_iter().collect::<Vec<_>>().join(" "))
    }
}

/// Second half of a sign-in: exchanges the authorization code of `response` and saves the tokens. Pass
/// the options given to `startMcpSignIn`, including its `redirectUrl` (not `authorization.redirectUrl`).
/// Fails when the response carries another `state`, when another sign-in started with the store since, or
/// when the exchange would use another redirect URI than the authorization request (the options changed).
/// A redirect URL the user pasted goes through `responseFromRedirectUrl` first, which checks its path.
pub async fn complete_mcp_sign_in(
    options: McpSignInOptions,
    authorization: McpSignInRedirect,
    response: McpAuthorizationResponse,
) -> Result<()> {
    let signal = options.signal.clone();
    cancellable(signal.as_ref(), async move {
        if response.state != authorization.state {
            return Err(Error::msg("The authorization response belongs to a different sign-in"));
        }
        // The stored PKCE verifier belongs to the latest sign-in; exchanging with it would fail with
        // `invalid_grant`, which discards the stored tokens.
        let stored_state = options.store.load().await?.and_then(|s| s.oauth_state);
        if stored_state.as_deref() != Some(authorization.state.as_str()) {
            return Err(Error::msg(
                "Another sign-in to this MCP server started since; sign in again",
            ));
        }
        let provider = create_oauth_client_provider(&options.client_options(), &options.redirect_url, None);
        let (flow_opts, extra_opts) = flow_options(&options);
        authorize_mcp_with_options(
            &provider,
            OAuthFlowOptions {
                scope: authorization.scope,
                authorization_code: Some(response.code),
                iss: response.iss,
                ..flow_opts
            },
            OAuthFlowExtraOptions {
                authorization_redirect_url: Some(authorization.redirect_url),
                ..extra_opts
            },
        )
        .await?;
        Ok(())
    })
    .await
}

fn flow_options(options: &McpSignInOptions) -> (OAuthFlowOptions, OAuthFlowExtraOptions) {
    (
        OAuthFlowOptions {
            server_url: options.server_url.clone(),
            authorization_code: None,
            iss: None,
            scope: None,
            resource_metadata_url: options.challenge.as_ref().and_then(|c| c.resource_metadata_url.clone()),
            authorization_server_metadata_url: options.settings.auth_server_metadata_url.clone(),
            fetch: Some(timed_fetch(None)),
            skip_issuer_validation: None,
            skip_refresh: None,
        },
        OAuthFlowExtraOptions {
            signal: options.signal.clone(),
            refresh_only: None,
            authorization_redirect_url: None,
        },
    )
}

/// Run `fut`, reporting an abort of `signal` as `McpSignInCancelledError`.
pub async fn cancellable<T, F>(signal: Option<&AbortSignal>, fut: F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    if signal.is_some_and(AbortSignal::aborted) {
        return Err(cancelled_error());
    }
    let result = match signal {
        Some(sig) => {
            tokio::select! {
                res = fut => res,
                () = sig.cancelled() => return Err(cancelled_error()),
            }
        }
        None => fut.await,
    };
    // Aborted requests fail with the signal's reason; report them as the cancellation they are.
    if signal.is_some_and(AbortSignal::aborted) {
        return Err(cancelled_error());
    }
    result
}
