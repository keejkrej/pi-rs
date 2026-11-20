//! Port of packages/mcp/src/oauth/callback.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;
use indexmap::IndexMap;

use pi_js::{Error, Result};

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
    paths: Vec<String>,
    timeout_ms: i64,
    render_page: Option<Arc<dyn Fn(&OAuthCallbackPage) -> String + Send + Sync>>,
    pending: Mutex<IndexMap<String, PendingOAuthCallback>>,
    shutdown: tokio::sync::watch::Sender<bool>,
    serve_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

struct PendingOAuthCallback {
    tx: tokio::sync::oneshot::Sender<Result<OAuthCallback>>,
    timer: pi_js::time::Timeout,
    path: Option<String>,
}

fn plain_text(page: &OAuthCallbackPage) -> String {
    match page {
        OAuthCallbackPage::Ok => "Authorization complete. You may close this window.".to_string(),
        OAuthCallbackPage::Err { message, details } => match details.as_deref().filter(|d| !d.is_empty()) {
            Some(d) => format!("{message}\n\n{d}"),
            None => message.clone(),
        },
    }
}

impl OAuthCallbackServer {
    pub fn redirect_url(&self) -> &str {
        &self.inner.redirect_url
    }

    pub async fn listen(options: Option<OAuthCallbackServerOptions>) -> Result<Self> {
        Self::listen_with_extra_paths(options, Vec::new()).await
    }

    // PORT: Additive constructor with `extra_paths` so `OAuthCallbackServerOptions` keeps all
    // existing struct-literal callers compiling.
    pub async fn listen_with_extra_paths(
        options: Option<OAuthCallbackServerOptions>,
        extra_paths: Vec<String>,
    ) -> Result<Self> {
        let options = options.unwrap_or_default();
        let host = options.host.unwrap_or_else(|| "127.0.0.1".to_string());
        let redirect_host = options.redirect_host.unwrap_or_else(|| host.clone());
        let port = u16::try_from(options.port.unwrap_or(0).max(0)).map_err(|_| Error::msg("Invalid port"))?;
        let path = options.path.unwrap_or_else(|| "/callback".to_string());
        let timeout_ms = options.timeout_ms.unwrap_or(5 * 60_000).max(0);

        let listener = tokio::net::TcpListener::bind((host.as_str(), port))
            .await
            .map_err(|e| Error::from_io(e, "listen", None, None))?;
        let addr = listener
            .local_addr()
            .map_err(|e| Error::from_io(e, "getsockname", None, None))?;
        let formatted_host = if redirect_host.contains(':') {
            format!("[{redirect_host}]")
        } else {
            redirect_host
        };
        let redirect_url = format!("http://{formatted_host}:{}{path}", addr.port());
        let mut paths = vec![path];
        paths.extend(extra_paths);

        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
        let server = Self {
            inner: Arc::new(OAuthCallbackServerInner {
                redirect_url,
                paths,
                timeout_ms,
                render_page: options.render_page,
                pending: Mutex::new(IndexMap::new()),
                shutdown: shutdown_tx,
                serve_task: Mutex::new(None),
            }),
        };

        let srv_for_loop = server.clone();
        let handle = tokio::spawn(async move {
            let mut conns = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    Some(_) = conns.join_next(), if !conns.is_empty() => {}
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else {
                            break;
                        };
                        let srv = srv_for_loop.clone();
                        let mut conn_shutdown = shutdown_rx.clone();
                        conns.spawn(async move {
                            let io = hyper_util::rt::TokioIo::new(stream);
                            let service = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                                let srv = srv.clone();
                                async move {
                                    Ok::<_, std::convert::Infallible>(srv.handle_request(req.uri().to_string()))
                                }
                            });
                            let conn = hyper::server::conn::http1::Builder::new().serve_connection(io, service);
                            tokio::pin!(conn);
                            tokio::select! {
                                _ = &mut conn => {}
                                _ = conn_shutdown.changed() => {
                                    conn.as_mut().graceful_shutdown();
                                    let _ = conn.await;
                                }
                            }
                        });
                    }
                }
            }
            // Wait briefly for in-flight HTTP responses to flush, then drop unused keep-alive /
            // preconnect sockets so `close()` returns immediately (#11311).
            if !conns.is_empty() {
                let _ = tokio::time::timeout(Duration::from_millis(50), async {
                    while conns.join_next().await.is_some() {}
                })
                .await;
                conns.shutdown().await;
            }
        });
        *server.inner.serve_task.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
        Ok(server)
    }

    pub async fn wait_for_callback(&self, state: &str) -> Result<OAuthCallback> {
        self.wait_for_callback_on_path(state, None).await
    }

    /// Wait for the authorization response with `state`. With `path`, a response on another path fails,
    /// so a server-specific redirect URI can tell authorization servers apart (RFC 9700 section 4.4.2.2).
    pub async fn wait_for_callback_on_path(&self, state: &str, path: Option<&str>) -> Result<OAuthCallback> {
        let rx = {
            let mut pending = self.inner.pending.lock().unwrap_or_else(|e| e.into_inner());
            if pending.contains_key(state) {
                return Err(Error::msg("OAuth state is already pending"));
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            let inner = Arc::clone(&self.inner);
            let key = state.to_string();
            let timer = pi_js::time::set_timeout(self.inner.timeout_ms as u64, move || {
                let removed = inner
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .shift_remove(&key);
                if let Some(entry) = removed {
                    let _ = entry.tx.send(Err(Error::msg("OAuth callback timed out")));
                }
            });
            pending.insert(
                state.to_string(),
                PendingOAuthCallback {
                    tx,
                    timer,
                    path: path.map(str::to_string),
                },
            );
            rx
        };
        rx.await
            .unwrap_or_else(|_| Err(Error::msg("OAuth callback server closed")))
    }

    pub async fn close(&self) -> Result<()> {
        let entries: Vec<PendingOAuthCallback> = {
            let mut pending = self.inner.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.drain(..).map(|(_, v)| v).collect()
        };
        for entry in entries {
            entry.timer.clear();
            let _ = entry.tx.send(Err(Error::msg("OAuth callback server closed")));
        }
        let _ = self.inner.shutdown.send(true);
        let handle = self.inner.serve_task.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = handle {
            let _ = h.await;
        }
        Ok(())
    }

    fn reply(&self, status: u16, page: &OAuthCallbackPage) -> Response<Full<Bytes>> {
        if let Some(render) = &self.inner.render_page {
            let html = render(page);
            Response::builder()
                .status(status)
                .header("content-type", "text/html; charset=utf-8")
                .header("cache-control", "no-store")
                .body(Full::new(Bytes::from(html)))
                .expect("valid response")
        } else {
            Response::builder()
                .status(status)
                .header("content-type", "text/plain; charset=utf-8")
                .body(Full::new(Bytes::from(plain_text(page))))
                .expect("valid response")
        }
    }

    fn handle_request(&self, raw_uri: String) -> Response<Full<Bytes>> {
        let base = url::Url::parse(&self.inner.redirect_url)
            .unwrap_or_else(|_| url::Url::parse("http://127.0.0.1/").expect("valid fallback url"));
        let Ok(url) = base.join(&raw_uri) else {
            return self.reply(
                400,
                &OAuthCallbackPage::Err {
                    message: "Invalid request URL".to_string(),
                    details: None,
                },
            );
        };
        if !self.inner.paths.iter().any(|p| p == url.path()) {
            return self.reply(
                404,
                &OAuthCallbackPage::Err {
                    message: "Not found".to_string(),
                    details: None,
                },
            );
        }

        let query_param =
            |name: &str| -> Option<String> { url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned()) };
        let state = query_param("state").filter(|s| !s.is_empty());
        let entry = state.as_ref().and_then(|s| {
            self.inner
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .shift_remove(s)
        });
        let (Some(state), Some(entry)) = (state, entry) else {
            return self.reply(
                400,
                &OAuthCallbackPage::Err {
                    message: "Invalid or expired OAuth state".to_string(),
                    details: None,
                },
            );
        };
        entry.timer.clear();

        if let Some(ref expected_path) = entry.path
            && url.path() != expected_path
        {
            let _ = entry.tx.send(Err(Error::msg(
                "The authorization response arrived on another redirect URI",
            )));
            return self.reply(
                400,
                &OAuthCallbackPage::Err {
                    message: "Unexpected redirect URI".to_string(),
                    details: None,
                },
            );
        }

        if let Some(error) = query_param("error").filter(|s| !s.is_empty()) {
            let description = query_param("error_description").unwrap_or(error);
            let _ = entry.tx.send(Err(Error::msg(description.clone())));
            return self.reply(
                200,
                &OAuthCallbackPage::Err {
                    message: "Authorization failed. You may close this window.".to_string(),
                    details: Some(description),
                },
            );
        }

        let Some(code) = query_param("code").filter(|s| !s.is_empty()) else {
            let _ = entry
                .tx
                .send(Err(Error::msg("OAuth callback did not include an authorization code")));
            return self.reply(
                400,
                &OAuthCallbackPage::Err {
                    message: "Missing authorization code".to_string(),
                    details: None,
                },
            );
        };

        let iss = query_param("iss").filter(|s| !s.is_empty());
        let _ = entry.tx.send(Ok(OAuthCallback { code, state, iss }));
        self.reply(200, &OAuthCallbackPage::Ok)
    }
}
