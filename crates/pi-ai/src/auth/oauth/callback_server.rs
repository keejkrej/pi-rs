//! Port of packages/ai/src/auth/oauth/callback-server.ts

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio_util::sync::CancellationToken;

use pi_js::abort::{AbortController, AbortSignal};
use pi_js::{BoxFuture, Error, Result};

use crate::auth::types::{AuthPrompt, ProviderAuthInteraction};
use crate::utils::oauth_page::{oauth_error_html, oauth_success_html};

pub type CallbackCompleteFn<T> = Box<dyn FnOnce(String) -> BoxFuture<Result<T>> + Send>;

pub struct OAuthCallbackServerOptions<T> {
    /// Provider name used on the browser page, for example `OpenAI`.
    pub provider_name: String,
    /// Address to listen on.
    pub host: String,
    /// Port to listen on; `0` picks a free port.
    pub port: u16,
    pub path: String,
    /// Host in `redirect_uri` when it differs from `host`, for example `localhost`.
    pub redirect_host: Option<String>,
    /// Expected `state` parameter. Omit when the provider does not send one.
    pub state: Option<String>,
    /// Finishes the sign-in with the received code before the browser page is sent.
    pub complete: CallbackCompleteFn<T>,
    pub signal: Option<AbortSignal>,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallbackOrManualInput<T> {
    Callback(T),
    Manual(String),
}

struct SharedState<T> {
    provider_name: String,
    path: String,
    expected_state: Option<String>,
    complete: StdMutex<Option<CallbackCompleteFn<T>>>,
    claimed: AtomicBool,
    settled: AtomicBool,
    wait_tx: StdMutex<Option<oneshot::Sender<Result<Option<T>>>>>,
    shutdown: CancellationToken,
}

impl<T: Send + 'static> SharedState<T> {
    fn finish(&self, outcome: Result<Option<T>>) {
        if self.settled.swap(true, Ordering::SeqCst) {
            return;
        }
        let tx = match self.wait_tx.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(sender) = tx {
            let _ = sender.send(outcome);
        }
    }
}

pub struct OAuthCallbackServer<T> {
    pub redirect_uri: String,
    state: Arc<SharedState<T>>,
    wait_rx: AsyncMutex<Option<oneshot::Receiver<Result<Option<T>>>>>,
}

impl<T: Send + 'static> OAuthCallbackServer<T> {
    /// Resolves with the result of `complete`, or `None` after `cancel()`.
    pub async fn wait(&self) -> Result<Option<T>> {
        let rx = {
            let mut guard = self.wait_rx.lock().await;
            guard.take()
        };
        let Some(receiver) = rx else {
            return Err(Error::msg("OAuth callback server wait() already awaited"));
        };
        match receiver.await {
            Ok(res) => res,
            Err(_) => Err(Error::msg("OAuth callback server closed")),
        }
    }

    /// Stop waiting for the browser unless a callback is already being completed.
    pub fn cancel(&self) {
        if !self.state.claimed.load(Ordering::SeqCst) {
            self.state.finish(Ok(None));
        }
    }

    /// Stop listening and close active connections immediately.
    pub fn close(&self) {
        self.state.finish(Err(Error::msg("OAuth callback server closed")));
        self.state.shutdown.cancel();
    }
}

fn html_response(status: StatusCode, html: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "text/html; charset=utf-8")
        .header("cache-control", "no-store")
        .body(Full::new(Bytes::from(html)))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
}

fn first_query_param(query: Option<&str>, target_key: &str) -> Option<String> {
    let q = query?;
    for (k, v) in url::form_urlencoded::parse(q.as_bytes()) {
        if k == target_key {
            return Some(v.into_owned());
        }
    }
    None
}

async fn handle_request<T: Send + 'static>(
    req: Request<Incoming>,
    state: Arc<SharedState<T>>,
) -> std::result::Result<Response<Full<Bytes>>, Infallible> {
    if req.method() != Method::GET || req.uri().path() != state.path {
        return Ok(html_response(
            StatusCode::NOT_FOUND,
            oauth_error_html("Callback route not found.", None),
        ));
    }

    let query = req.uri().query();
    if let Some(expected) = state.expected_state.as_deref() {
        let got = first_query_param(query, "state");
        if got.as_deref() != Some(expected) {
            return Ok(html_response(
                StatusCode::BAD_REQUEST,
                oauth_error_html("State mismatch.", None),
            ));
        }
    }

    if state.claimed.load(Ordering::SeqCst) || state.settled.load(Ordering::SeqCst) {
        return Ok(html_response(
            StatusCode::CONFLICT,
            oauth_error_html("This sign-in has already been handled.", None),
        ));
    }

    if let Some(err_param) = first_query_param(query, "error").filter(|s| !s.is_empty()) {
        let description = first_query_param(query, "error_description")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| err_param.clone());
        let title = format!("{} authorization failed.", state.provider_name);
        state.finish(Err(Error::msg(format!(
            "{} authorization failed: {description}",
            state.provider_name
        ))));
        return Ok(html_response(
            StatusCode::BAD_REQUEST,
            oauth_error_html(&title, Some(&description)),
        ));
    }

    let Some(code) = first_query_param(query, "code").filter(|s| !s.is_empty()) else {
        return Ok(html_response(
            StatusCode::BAD_REQUEST,
            oauth_error_html("Missing authorization code.", None),
        ));
    };

    if state.claimed.swap(true, Ordering::SeqCst) || state.settled.load(Ordering::SeqCst) {
        return Ok(html_response(
            StatusCode::CONFLICT,
            oauth_error_html("This sign-in has already been handled.", None),
        ));
    }

    let complete_fn = match state.complete.lock() {
        Ok(mut guard) => guard.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    };
    let Some(complete) = complete_fn else {
        return Ok(html_response(
            StatusCode::CONFLICT,
            oauth_error_html("This sign-in has already been handled.", None),
        ));
    };

    match complete(code).await {
        Ok(value) => {
            let success_msg = format!("Signed in to {}. You may now close this page.", state.provider_name);
            state.finish(Ok(Some(value)));
            Ok(html_response(StatusCode::OK, oauth_success_html(&success_msg)))
        }
        Err(err) => {
            let msg = err.to_string();
            let title = format!("{} sign-in failed.", state.provider_name);
            state.finish(Err(err));
            Ok(html_response(
                StatusCode::BAD_GATEWAY,
                oauth_error_html(&title, Some(&msg)),
            ))
        }
    }
}

pub async fn start_oauth_callback_server<T: Send + 'static>(
    options: OAuthCallbackServerOptions<T>,
) -> Result<OAuthCallbackServer<T>> {
    if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Err(Error::msg("Login cancelled"));
    }

    let listener = TcpListener::bind((options.host.as_str(), options.port))
        .await
        .map_err(|e| Error::msg(format!("Failed to bind OAuth callback server: {e}")))?;

    let addr: SocketAddr = listener
        .local_addr()
        .map_err(|e| Error::msg(format!("OAuth callback server did not bind to TCP: {e}")))?;

    let (wait_tx, wait_rx) = oneshot::channel();
    let shutdown = CancellationToken::new();

    let state = Arc::new(SharedState {
        provider_name: options.provider_name.clone(),
        path: options.path.clone(),
        expected_state: options.state,
        complete: StdMutex::new(Some(options.complete)),
        claimed: AtomicBool::new(false),
        settled: AtomicBool::new(false),
        wait_tx: StdMutex::new(Some(wait_tx)),
        shutdown: shutdown.clone(),
    });

    // Abort/timeout monitor task.
    {
        let monitor_state = Arc::clone(&state);
        let signal = options.signal;
        let timeout_ms = options.timeout_ms;
        let provider_name = options.provider_name;
        let monitor_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let abort_fut = async {
                if let Some(sig) = signal.as_ref() {
                    sig.cancelled().await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            let timeout_fut = async {
                if let Some(ms) = timeout_ms {
                    let _ = pi_js::time::sleep(ms, None).await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            tokio::select! {
                _ = abort_fut => {
                    monitor_state.finish(Err(Error::msg("Login cancelled")));
                }
                _ = timeout_fut => {
                    monitor_state.finish(Err(Error::msg(format!("{provider_name} sign-in timed out"))));
                }
                _ = monitor_shutdown.cancelled() => {}
            }
        });
    }

    // Accept loop task.
    {
        let accept_state = Arc::clone(&state);
        let accept_shutdown = shutdown.clone();
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    res = listener.accept() => res,
                    _ = accept_shutdown.cancelled() => break,
                };
                let Ok((stream, _)) = accepted else {
                    break;
                };
                let conn_state = Arc::clone(&accept_state);
                let conn_shutdown = accept_shutdown.clone();
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let svc = service_fn(move |req| {
                        let st = Arc::clone(&conn_state);
                        handle_request(req, st)
                    });
                    let conn = http1::Builder::new().serve_connection(io, svc);
                    tokio::select! {
                        _ = conn => {}
                        _ = conn_shutdown.cancelled() => {}
                    }
                });
            }
        });
    }

    let redirect_host = options.redirect_host.as_deref().unwrap_or(&options.host);
    let formatted_host = if redirect_host.contains(':') && !redirect_host.starts_with('[') {
        format!("[{redirect_host}]")
    } else {
        redirect_host.to_owned()
    };
    let redirect_uri = format!("http://{formatted_host}:{}{}", addr.port(), options.path);

    Ok(OAuthCallbackServer {
        redirect_uri,
        state,
        wait_rx: AsyncMutex::new(Some(wait_rx)),
    })
}

/// Wait for the browser callback, or for the user to paste the code or redirect URL.
pub async fn wait_for_callback_or_manual_input<T: Send + 'static>(
    interaction: &ProviderAuthInteraction,
    callback: Option<&OAuthCallbackServer<T>>,
    message: &str,
    placeholder: &str,
) -> Result<CallbackOrManualInput<T>> {
    let manual_abort = AbortController::new();
    let manual_abort_signal = manual_abort.signal();
    let prompt_signal = AbortSignal::any(&[manual_abort_signal.clone(), interaction.signal.clone()]);
    let prompt = AuthPrompt::manual_code(message, Some(placeholder.to_owned())).with_signal(prompt_signal);

    let Some(cb) = callback else {
        let input = interaction.prompt(prompt).await?;
        return Ok(CallbackOrManualInput::Manual(input));
    };

    let cb_settled = Arc::new(AtomicBool::new(false));
    let cb_settled_for_manual = Arc::clone(&cb_settled);

    let manual_fut = async move {
        let res = interaction.prompt(prompt).await;
        if !cb_settled_for_manual.load(Ordering::SeqCst) {
            cb.cancel();
        }
        res
    };

    let cb_fut = async move {
        let res = cb.wait().await;
        cb_settled.store(true, Ordering::SeqCst);
        manual_abort.abort(None);
        res
    };

    let (cb_res, manual_res) = tokio::join!(cb_fut, manual_fut);

    match cb_res {
        Ok(Some(value)) => match manual_res {
            Err(err) if !manual_abort_signal.aborted() => Err(err),
            _ => Ok(CallbackOrManualInput::Callback(value)),
        },
        Ok(None) => {
            let input = manual_res?;
            Ok(CallbackOrManualInput::Manual(input))
        }
        Err(err) => {
            if let Err(manual_err) = manual_res {
                if !manual_abort_signal.aborted() {
                    return Err(manual_err);
                }
            }
            Err(err)
        }
    }
}
