//! Port of packages/mcp/src/oauth/loopback.ts

#![allow(dead_code, unused_variables)]

use std::sync::Arc;

use crate::oauth::callback::{OAuthCallbackPage, OAuthCallbackServer, OAuthCallbackServerOptions};
use crate::oauth::client_settings::{
    McpClientMetadataDocuments, McpOAuthSettings, loopback_settings, offers_client_metadata_document,
    registered_redirect_urls, server_specific_path,
};
use crate::oauth::provider::McpOAuthStateStore;
use crate::oauth::sign_in::{McpAuthorizationResponse, McpSignInCancelledError, McpSignInRedirect, cancellable};
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::{BoxFuture, Error, Result};

/// A loopback callback server and the redirect URI it serves.
#[derive(Clone)]
pub struct McpLoopbackCallback {
    pub callback: OAuthCallbackServer,
    /// Pass to `startMcpSignIn` and `completeMcpSignIn`.
    pub redirect_url: String,
}

#[derive(Clone)]
pub struct ListenForLoopbackSignInOptions {
    pub server_url: String,
    pub store: Arc<dyn McpOAuthStateStore>,
    pub settings: McpOAuthSettings,
    pub client_metadata_documents: Option<Arc<dyn McpClientMetadataDocuments>>,
    /// Renders the browser page after the redirect. Default: plain text.
    pub render_page: Option<Arc<dyn Fn(&OAuthCallbackPage) -> String + Send + Sync>>,
}

/// Listen for the authorization response of a sign-in on loopback, where `settings` say: `callbackUrl`
/// and `callbackPort`, else the port of the stored client's redirect URI (so that client stays valid) or
/// a free one. Pass the sign-in's `clientMetadataDocuments`, so the callback server also serves the
/// server-specific redirect URI. Close the callback server when the sign-in ends.
pub async fn listen_for_loopback_sign_in(options: ListenForLoopbackSignInOptions) -> Result<McpLoopbackCallback> {
    let loopback = loopback_settings(&options.settings)?;
    let stored = options.store.load().await?;
    let registered = registered_redirect_urls(stored.as_ref().and_then(|s| s.client_information.as_ref()))
        .first()
        .cloned();
    let reg_port = registered
        .as_deref()
        .and_then(|u| url::Url::parse(u).ok())
        .and_then(|u| u.port());
    let port = loopback.port.or(reg_port);
    let offers_document = offers_client_metadata_document(
        &options.settings,
        options.client_metadata_documents.as_deref(),
        &loopback.url,
    );
    let extra_paths = if offers_document {
        vec![server_specific_path(&loopback.path, &options.server_url)?]
    } else {
        Vec::new()
    };

    let make_listen_options = |port_num: Option<u16>| OAuthCallbackServerOptions {
        host: Some(loopback.host.clone()),
        redirect_host: Some(loopback.redirect_host.clone()),
        port: port_num.map(i64::from),
        path: Some(loopback.path.clone()),
        timeout_ms: None,
        render_page: options.render_page.clone(),
    };

    let callback = match OAuthCallbackServer::listen_with_extra_paths(
        Some(make_listen_options(port)),
        extra_paths.clone(),
    )
    .await
    {
        Ok(srv) => srv,
        Err(err) => {
            // A configured port is required; the stored client's port is only preferred.
            if loopback.port.is_some() || port.is_none() {
                return Err(err);
            }
            OAuthCallbackServer::listen_with_extra_paths(Some(make_listen_options(None)), extra_paths).await?
        }
    };

    let redirect_url = loopback
        .fixed_redirect_url
        .unwrap_or_else(|| callback.redirect_url().to_string());
    Ok(McpLoopbackCallback { callback, redirect_url })
}

pub type PromptForRedirectUrlFn = Arc<dyn Fn(AbortSignal) -> BoxFuture<Result<Option<String>>> + Send + Sync>;

#[derive(Clone)]
pub struct WaitForLoopbackSignInOptions {
    pub callback: OAuthCallbackServer,
    /// The `redirect` result of `startMcpSignIn`.
    pub authorization: McpSignInRedirect,
    /// Ask for the redirect URL, for when the browser cannot reach the loopback callback (for example over
    /// SSH). Aborted once the callback arrives or the sign-in is cancelled. Resolving to `None` or an
    /// empty string cancels the sign-in.
    pub prompt_for_redirect_url: Option<PromptForRedirectUrlFn>,
    pub signal: Option<AbortSignal>,
}

/// Wait for the authorization response on a loopback callback server, or for a redirect URL the user
/// pastes from the browser address bar, whichever comes first. Aborting `signal` stops the wait with
/// `McpSignInCancelledError`.
pub async fn wait_for_loopback_sign_in(options: WaitForLoopbackSignInOptions) -> Result<McpAuthorizationResponse> {
    let controller = AbortController::new();
    let combined_signal = match &options.signal {
        Some(sig) => AbortSignal::any(&[controller.signal(), sig.clone()]),
        None => controller.signal(),
    };
    struct AbortOnDrop(AbortController);
    impl Drop for AbortOnDrop {
        fn drop(&mut self) {
            self.0.abort(None);
        }
    }
    let _abort_guard = AbortOnDrop(controller);

    let auth_url = url::Url::parse(&options.authorization.redirect_url)
        .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
    let auth_path = auth_url.path().to_string();
    let state = options.authorization.state.clone();

    cancellable(options.signal.as_ref(), async {
        let from_browser = options.callback.wait_for_callback_on_path(&state, Some(&auth_path));
        if let Some(ref prompt) = options.prompt_for_redirect_url {
            let prompt_fut = prompt(combined_signal);
            tokio::select! {
                res = from_browser => res,
                prompt_res = prompt_fut => {
                    let input = prompt_res?.filter(|s| !pi_js::str16::trim(s).is_empty());
                    let Some(text) = input else {
                        return Err(Error::typed(
                            McpSignInCancelledError::NAME,
                            McpSignInCancelledError::new(),
                        ));
                    };
                    response_from_redirect_url(&text, &options.authorization)
                }
            }
        } else {
            tokio::select! {
                res = from_browser => res,
                () = combined_signal.cancelled() => Err(Error::typed(
                    McpSignInCancelledError::NAME,
                    McpSignInCancelledError::new(),
                )),
            }
        }
    })
    .await
}

/// The authorization response in a redirect URL the user pasted from the browser address bar. Fails when
/// the URL is not on the sign-in's redirect URI, carries another `state`, or reports an error.
pub fn response_from_redirect_url(input: &str, authorization: &McpSignInRedirect) -> Result<McpAuthorizationResponse> {
    let url = url::Url::parse(pi_js::str16::trim(input))
        .map_err(|_| Error::msg("Expected the full redirect URL from the browser address bar"))?;
    let redirect_url = url::Url::parse(&authorization.redirect_url)
        .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
    // A server-specific redirect URI tells authorization servers apart, so it must match exactly.
    if url.origin().ascii_serialization() != redirect_url.origin().ascii_serialization()
        || url.path() != redirect_url.path()
    {
        return Err(Error::msg(
            "The redirect URL does not match this sign-in's redirect URI",
        ));
    }
    let query_param =
        |name: &str| -> Option<String> { url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned()) };
    if let Some(error) = query_param("error").filter(|s| !s.is_empty()) {
        let description = query_param("error_description")
            .filter(|s| !s.is_empty())
            .unwrap_or(error);
        return Err(Error::msg(description));
    }
    let state = query_param("state");
    if state.as_deref() != Some(authorization.state.as_str()) {
        return Err(Error::msg("The redirect URL belongs to a different sign-in"));
    }
    let Some(code) = query_param("code").filter(|s| !s.is_empty()) else {
        return Err(Error::msg("The redirect URL does not contain an authorization code"));
    };
    let iss = query_param("iss").filter(|s| !s.is_empty());
    Ok(McpAuthorizationResponse {
        code,
        state: authorization.state.clone(),
        iss,
    })
}
