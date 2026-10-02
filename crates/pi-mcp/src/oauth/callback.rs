//! Port of packages/mcp/src/oauth/callback.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;

use pi_js::Result;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthCallback {
    pub code: String,
    pub state: String,
    pub iss: Option<String>,
}

/// Outcome shown on the browser page after the redirect.
///
/// `Ok` is `{ ok: true }`. `Err` is `{ ok: false, message, details? }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OAuthCallbackPage {
    Ok,
    Err { message: String, details: Option<String> },
}

#[derive(Clone, Default)]
pub struct OAuthCallbackServerOptions {
    /// Address to listen on. Default: `127.0.0.1`.
    pub host: Option<String>,
    /// Host name in `redirectUrl`, for example `localhost` for a client registered with it while
    /// listening on `127.0.0.1`. Default: `host`.
    pub redirect_host: Option<String>,
    pub port: Option<i64>,
    pub path: Option<String>,
    pub timeout_ms: Option<i64>,
    /// Render the browser page as HTML. Default: a plain-text message.
    pub render_page: Option<Arc<dyn Fn(&OAuthCallbackPage) -> String + Send + Sync>>,
}

#[derive(Clone)]
pub struct OAuthCallbackServer {
    inner: Arc<OAuthCallbackServerInner>,
}

struct OAuthCallbackServerInner {
    redirect_url: String,
    path: String,
    timeout_ms: i64,
    render_page: Option<Arc<dyn Fn(&OAuthCallbackPage) -> String + Send + Sync>>,
    pending: Mutex<IndexMap<String, PendingOAuthCallback>>,
    shutdown: tokio::sync::watch::Sender<bool>,
    serve_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

struct PendingOAuthCallback {
    tx: tokio::sync::oneshot::Sender<Result<OAuthCallback>>,
    timer: pi_js::time::Timeout,
}

fn plain_text(page: &OAuthCallbackPage) -> String {
    todo!("port: plain_text")
}

impl OAuthCallbackServer {
    pub fn redirect_url(&self) -> &str {
        &self.inner.redirect_url
    }

    pub async fn listen(options: Option<OAuthCallbackServerOptions>) -> Result<Self> {
        todo!("port: OAuthCallbackServer::listen")
    }

    pub async fn wait_for_callback(&self, state: &str) -> Result<OAuthCallback> {
        todo!("port: OAuthCallbackServer::wait_for_callback")
    }

    pub async fn close(&self) -> Result<()> {
        todo!("port: OAuthCallbackServer::close")
    }
}
