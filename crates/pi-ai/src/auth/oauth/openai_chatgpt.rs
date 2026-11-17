//! Port of packages/ai/src/auth/oauth/openai-chatgpt.ts

use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request as HyperRequest, Response as HyperResponse, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Map, Value};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use pi_js::abort::{AbortController, AbortSignal};
use pi_js::fetch::{Body, Headers, Request, fetch};
use pi_js::{Error, Result};

use crate::auth::types::{
    AuthEvent, AuthPrompt, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction,
};
use crate::utils::oauth_page::{oauth_error_html, oauth_success_html};
use crate::utils::provider_env::get_provider_env_value;

const DYNAMIC_CLIENT_ID: &str = "dynamic_agent_client";
const AGENT_NAME_HINT: &str = "Pi";
const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const RESOURCE: &str = "https://api.openai.com/v1";
const CALLBACK_PORT: u16 = 1455;
const CALLBACK_PATH: &str = "/auth/callback";
const REDIRECT_URI: &str = "http://127.0.0.1:1455/auth/callback";
const DIRECT_TOKEN_SCOPE: &str = "chatgpt.tokens.use.direct";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const EXPIRY_MARGIN_MS: i64 = 3 * 60 * 1000;

fn callback_host() -> String {
    get_provider_env_value("PI_OAUTH_CALLBACK_HOST", None).unwrap_or_else(|| "127.0.0.1".to_owned())
}

#[derive(Clone, Debug)]
struct AuthorizationResult {
    code: String,
    client_id: String,
}

fn random_value() -> String {
    pi_js::b64::encode_url(&pi_js::crypto::random_bytes(32))
}

fn first_query_param(url: &url::Url, target_key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(k, _)| k == target_key)
        .map(|(_, v)| v.into_owned())
}

fn authorization_result_from_callback(url: &url::Url, expected_state: &str) -> Result<AuthorizationResult> {
    let code = first_query_param(url, "code")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::msg("Missing authorization code"))?;
    let state = first_query_param(url, "state")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::msg("Missing OAuth state"))?;
    if state != expected_state {
        return Err(Error::msg("OAuth state mismatch"));
    }
    let client_id = first_query_param(url, "client_id")
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::msg("OpenAI OAuth registration callback did not contain an issued client ID"))?;
    Ok(AuthorizationResult { code, client_id })
}

fn authorization_result_from_manual_input(input: &str, expected_state: &str) -> Result<AuthorizationResult> {
    let url = url::Url::parse(input.trim()).map_err(|_| Error::msg("Paste the full callback URL from the browser"))?;
    let expected = url::Url::parse(REDIRECT_URI).map_err(|_| Error::msg("Invalid redirect URI constant"))?;
    if url.origin() != expected.origin() || url.path() != expected.path() {
        return Err(Error::msg(format!(
            "The pasted callback URL must start with {REDIRECT_URI}"
        )));
    }
    if let Some(error) = first_query_param(&url, "error").filter(|s| !s.is_empty()) {
        return Err(Error::msg(format!("ChatGPT authorization failed: {error}")));
    }
    authorization_result_from_callback(&url, expected_state)
}

fn html_response(status: StatusCode, body: String) -> HyperResponse<Full<Bytes>> {
    HyperResponse::builder()
        .status(status)
        .header("Content-Type", "text/html; charset=utf-8")
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_else(|_| HyperResponse::new(Full::new(Bytes::new())))
}

struct ChatGptCallbackServer {
    result_rx: Option<oneshot::Receiver<Result<AuthorizationResult>>>,
    shutdown: CancellationToken,
}

impl ChatGptCallbackServer {
    fn close_all(&self) {
        self.shutdown.cancel();
    }
}

struct CallbackInner {
    expected_state: String,
    settled: AtomicBool,
    tx: StdMutex<Option<oneshot::Sender<Result<AuthorizationResult>>>>,
}

impl CallbackInner {
    fn finish(&self, res: Result<AuthorizationResult>) {
        if self.settled.swap(true, Ordering::SeqCst) {
            return;
        }
        let sender = match self.tx.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(tx) = sender {
            let _ = tx.send(res);
        }
    }
}

async fn handle_chatgpt_callback(
    req: HyperRequest<Incoming>,
    inner: Arc<CallbackInner>,
) -> std::result::Result<HyperResponse<Full<Bytes>>, Infallible> {
    let base = match url::Url::parse(REDIRECT_URI) {
        Ok(u) => u,
        Err(_) => {
            return Ok(html_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                oauth_error_html("Internal error while processing the callback.", None),
            ));
        }
    };
    let path_and_query = req.uri().path_and_query().map(|pq| pq.as_str()).unwrap_or("");
    let Ok(url) = base.join(path_and_query) else {
        return Ok(html_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            oauth_error_html("Internal error while processing the callback.", None),
        ));
    };

    if url.path() != CALLBACK_PATH {
        return Ok(html_response(
            StatusCode::NOT_FOUND,
            oauth_error_html("Callback route not found.", None),
        ));
    }

    if let Some(error) = first_query_param(&url, "error").filter(|s| !s.is_empty()) {
        inner.finish(Err(Error::msg(format!("ChatGPT authorization failed: {error}"))));
        return Ok(html_response(
            StatusCode::BAD_REQUEST,
            oauth_error_html("ChatGPT was not connected.", Some(&format!("Error: {error}"))),
        ));
    }

    match authorization_result_from_callback(&url, &inner.expected_state) {
        Ok(auth_res) => {
            inner.finish(Ok(auth_res));
            Ok(html_response(
                StatusCode::OK,
                oauth_success_html("ChatGPT authentication completed. You can close this window."),
            ))
        }
        Err(err) => Ok(html_response(
            StatusCode::BAD_REQUEST,
            oauth_error_html(&err.to_string(), None),
        )),
    }
}

async fn start_callback_server(expected_state: &str) -> Result<ChatGptCallbackServer> {
    let host = callback_host();
    let listener = TcpListener::bind((host.as_str(), CALLBACK_PORT))
        .await
        .map_err(|e| Error::msg(format!("Failed to listen on {host}:{CALLBACK_PORT}: {e}")))?;

    let (tx, rx) = oneshot::channel();
    let shutdown = CancellationToken::new();
    let inner = Arc::new(CallbackInner {
        expected_state: expected_state.to_owned(),
        settled: AtomicBool::new(false),
        tx: StdMutex::new(Some(tx)),
    });

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
            let conn_inner = Arc::clone(&inner);
            let conn_shutdown = accept_shutdown.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let svc = service_fn(move |req| {
                    let st = Arc::clone(&conn_inner);
                    handle_chatgpt_callback(req, st)
                });
                let conn = http1::Builder::new().serve_connection(io, svc);
                tokio::select! {
                    _ = conn => {}
                    _ = conn_shutdown.cancelled() => {}
                }
            });
        }
    });

    Ok(ChatGptCallbackServer {
        result_rx: Some(rx),
        shutdown,
    })
}

async fn request_token(pairs: &[(&str, &str)], signal: &AbortSignal) -> Result<Map<String, Value>> {
    let body_str = {
        let mut ser = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in pairs {
            ser.append_pair(k, v);
        }
        ser.finish()
    };

    let mut headers = Headers::new();
    headers.set("accept", "application/json");
    headers.set("content-type", "application/x-www-form-urlencoded");

    let req = Request::post(TOKEN_URL)
        .with_headers(headers)
        .body(Body::from(body_str))
        .signal(signal.clone());
    let response = fetch(req).await?;

    if !response.ok() {
        let status = response.status();
        let status_text = response.status_text().to_owned();
        let response_body = response.text().await.unwrap_or_default();
        let detail = if response_body.is_empty() {
            status_text
        } else {
            response_body
        };
        return Err(Error::msg(format!(
            "OpenAI OAuth token request failed ({status}): {detail}"
        )));
    }

    let data: Value = response.json().await?;
    let Value::Object(map) = data else {
        return Err(Error::msg("OpenAI OAuth token response must be an object"));
    };
    Ok(map)
}

fn require_token_string(token: &Map<String, Value>, field: &str) -> Result<String> {
    match token.get(field).and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => Ok(s.to_owned()),
        _ => Err(Error::msg(format!("OpenAI OAuth token response has invalid {field}"))),
    }
}

fn credential_from_token_response(token: &Map<String, Value>, client_id: &str) -> Result<OAuthCredential> {
    let access = require_token_string(token, "access_token")?;
    let refresh = require_token_string(token, "refresh_token")?;
    let scope = require_token_string(token, "scope")?;
    let Some(expires_in) = token
        .get("expires_in")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v > 0.0)
    else {
        return Err(Error::msg("OpenAI OAuth token response has invalid expires_in"));
    };

    let scopes: Vec<String> = scope.trim().split_whitespace().map(str::to_owned).collect();
    if !scopes.iter().any(|s| s == DIRECT_TOKEN_SCOPE) {
        return Err(Error::msg(format!(
            "OpenAI OAuth grant did not include {DIRECT_TOKEN_SCOPE}"
        )));
    }

    let expires = pi_js::time::now_ms() + (expires_in * 1000.0) as i64 - EXPIRY_MARGIN_MS;
    let mut cred = OAuthCredential::new(refresh, access, expires);
    cred.set_extra("clientId", Value::String(client_id.to_owned()));
    cred.set_extra("scopes", Value::Array(scopes.into_iter().map(Value::String).collect()));
    Ok(cred)
}

async fn exchange_authorization_code(
    code: &str,
    verifier: &str,
    client_id: &str,
    signal: &AbortSignal,
) -> Result<OAuthCredential> {
    let token = request_token(
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", REDIRECT_URI),
            ("resource", RESOURCE),
        ],
        signal,
    )
    .await?;

    let has_id_token = token
        .get("id_token")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty());
    if !has_id_token {
        return Err(Error::msg("OpenAI OAuth token response did not contain an ID token"));
    }
    credential_from_token_response(&token, client_id)
}

async fn refresh_access_token(credential: &OAuthCredential, signal: &AbortSignal) -> Result<OAuthCredential> {
    let Some(client_id) = credential
        .get_extra_str("clientId")
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Err(Error::msg(
            "Stored OpenAI OAuth credential does not contain an issued client ID; reconnect ChatGPT",
        ));
    };
    let token = request_token(
        &[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", &credential.refresh),
            ("resource", RESOURCE),
        ],
        signal,
    )
    .await?;
    credential_from_token_response(&token, client_id)
}

fn is_uuid_string(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (idx, &b) in bytes.iter().enumerate() {
        if matches!(idx, 8 | 13 | 18 | 23) {
            if b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

fn agent_host_id(device_id: Option<&str>) -> Result<String> {
    let Some(id) = device_id.filter(|s| is_uuid_string(s)) else {
        return Err(Error::msg(
            "Sign in with ChatGPT requires a device ID (UUID) for this installation",
        ));
    };
    Ok(format!("urn:uuid:{}", id.to_ascii_lowercase()))
}

async fn login_openai_chatgpt(
    interaction: ProviderAuthInteraction,
    options: Option<LoginOptions>,
) -> Result<OAuthCredential> {
    let device_id = options.and_then(|o| o.get_device_id).map(|f| f());
    let host_id = agent_host_id(device_id.as_deref())?;
    let pkce = crate::auth::oauth::pkce::generate_pkce().await?;
    let verifier = pkce.verifier;
    let challenge = pkce.challenge;
    let state = random_value();
    let nonce = random_value();

    let mut callback = match start_callback_server(&state).await {
        Ok(cb) => Some(cb),
        Err(err) => {
            interaction.notify(AuthEvent::Info {
                message: format!("Could not listen on {REDIRECT_URI}; paste the final redirect URL to continue. {err}"),
                links: None,
            });
            None
        }
    };

    let mut auth_url = url::Url::parse(AUTHORIZE_URL).map_err(|e| Error::msg(e.to_string()))?;
    auth_url
        .query_pairs_mut()
        .append_pair("client_id", DYNAMIC_CLIENT_ID)
        .append_pair("agent_name_hint", AGENT_NAME_HINT)
        .append_pair("ext_agent_host_id", &host_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("resource", RESOURCE)
        .append_pair("scope", SCOPE)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("nonce", &nonce);

    interaction.notify(AuthEvent::AuthUrl {
        url: auth_url.to_string(),
        instructions: Some(
            "Complete sign-in in your browser. If the callback does not complete, paste the final redirect URL here."
                .to_owned(),
        ),
    });

    let manual_abort = AbortController::new();
    let manual_signal = AbortSignal::any(&[manual_abort.signal(), interaction.signal.clone()]);
    let expected_state_for_manual = state.clone();
    let manual_interaction = interaction.clone();

    let manual_fut = async move {
        let input = manual_interaction
            .prompt(
                AuthPrompt::manual_code(
                    "Complete login in your browser, or paste the final redirect URL here:",
                    Some(REDIRECT_URI.to_owned()),
                )
                .with_signal(manual_signal),
            )
            .await?;
        authorization_result_from_manual_input(&input, &expected_state_for_manual)
    };

    let result: Result<AuthorizationResult> = if let Some(ref mut cb) = callback {
        let rx = cb.result_rx.take();
        let cb_fut = async move {
            match rx {
                Some(rx) => match rx.await {
                    Ok(res) => res,
                    Err(_) => Err(Error::msg("OAuth callback server closed")),
                },
                None => Err(Error::msg("OAuth callback receiver missing")),
            }
        };
        tokio::select! {
            res = cb_fut => res,
            res = manual_fut => res,
        }
    } else {
        manual_fut.await
    };

    manual_abort.abort(None);
    if let Some(cb) = callback.as_ref() {
        cb.close_all();
    }

    let auth_res = match result {
        Ok(res) => res,
        Err(err) => {
            if interaction.signal.aborted() {
                return Err(Error::msg("Login cancelled"));
            }
            return Err(err);
        }
    };

    interaction.notify(AuthEvent::Progress {
        message: "Exchanging authorization code for tokens...".to_owned(),
    });
    match exchange_authorization_code(&auth_res.code, &verifier, &auth_res.client_id, &interaction.signal).await {
        Ok(cred) => Ok(cred),
        Err(err) => {
            if interaction.signal.aborted() {
                return Err(Error::msg("Login cancelled"));
            }
            Err(err)
        }
    }
}

pub struct OpenAIChatGPTOAuth;

#[async_trait::async_trait]
impl OAuthAuth for OpenAIChatGPTOAuth {
    fn name(&self) -> &str {
        "OpenAI (ChatGPT subscription)"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    fn login_label(&self) -> Option<&str> {
        Some("Sign in with ChatGPT")
    }

    async fn login(
        &self,
        interaction: ProviderAuthInteraction,
        options: Option<LoginOptions>,
    ) -> Result<OAuthCredential> {
        login_openai_chatgpt(interaction, options).await
    }

    async fn refresh(&self, credential: OAuthCredential, signal: AbortSignal) -> Result<OAuthCredential> {
        refresh_access_token(&credential, &signal).await
    }

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access),
            headers: None,
            base_url: None,
        })
    }
}

pub fn openai_chatgpt_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(OpenAIChatGPTOAuth)
}
