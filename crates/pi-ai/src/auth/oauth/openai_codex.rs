//! Port of packages/ai/src/auth/oauth/openai-codex.ts

use std::sync::Arc;

use serde_json::{Map, Value};

use pi_js::abort::AbortSignal;
use pi_js::fetch::{Body, Headers, Request, Response, fetch};
use pi_js::{Error, Result};

use crate::auth::oauth::callback_server::{
    CallbackOrManualInput, OAuthCallbackServerOptions, start_oauth_callback_server, wait_for_callback_or_manual_input,
};
use crate::auth::oauth::device_code::{OAuthDeviceCodePollResult, poll_oauth_device_code_flow};
use crate::auth::oauth::pkce::generate_pkce;
use crate::auth::types::{
    AuthEvent, AuthPrompt, AuthSelectOption, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential,
    ProviderAuthInteraction,
};
use crate::utils::provider_env::get_provider_env_value;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTH_BASE_URL: &str = "https://auth.openai.com";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const DEVICE_USER_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const DEVICE_VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const DEVICE_CODE_TIMEOUT_SECONDS: f64 = 15.0 * 60.0;
const OPENAI_CODEX_BROWSER_LOGIN_METHOD: &str = "browser";
const OPENAI_CODEX_DEVICE_CODE_LOGIN_METHOD: &str = "device_code";
const SCOPE: &str = "openid profile email offline_access";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";

struct OAuthToken {
    access: String,
    refresh: String,
    expires: i64,
}

struct DeviceAuthInfo {
    device_auth_id: String,
    user_code: String,
    interval_seconds: f64,
}

struct DeviceTokenSuccess {
    authorization_code: String,
    code_verifier: String,
}

fn callback_host() -> String {
    get_provider_env_value("PI_OAUTH_CALLBACK_HOST", None).unwrap_or_else(|| "127.0.0.1".to_owned())
}

fn create_state() -> String {
    hex::encode(pi_js::crypto::random_bytes(16))
}

fn parse_authorization_input(input: &str) -> (Option<String>, Option<String>) {
    let value = input.trim();
    if value.is_empty() {
        return (None, None);
    }

    if let Ok(parsed) = url::Url::parse(value) {
        let mut code = None;
        let mut state = None;
        for (k, v) in parsed.query_pairs() {
            if k == "code" && code.is_none() {
                code = Some(v.into_owned());
            } else if k == "state" && state.is_none() {
                state = Some(v.into_owned());
            }
        }
        return (code, state);
    }

    if let Some((code, state)) = value.split_once('#') {
        return (Some(code.to_owned()), Some(state.to_owned()));
    }

    if value.contains("code=") {
        let mut code = None;
        let mut state = None;
        for (k, v) in url::form_urlencoded::parse(value.as_bytes()) {
            if k == "code" && code.is_none() {
                code = Some(v.into_owned());
            } else if k == "state" && state.is_none() {
                state = Some(v.into_owned());
            }
        }
        return (code, state);
    }

    (Some(value.to_owned()), None)
}

fn decode_jwt(token: &str) -> Option<Map<String, Value>> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _sig = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let mut standard = payload.replace('-', "+").replace('_', "/");
    let rem = standard.len() % 4;
    if rem == 2 {
        standard.push_str("==");
    } else if rem == 3 {
        standard.push('=');
    } else if rem == 1 {
        return None;
    }

    let decoded = pi_js::b64::atob(&standard).ok()?;
    let Value::Object(map) = pi_js::json::parse(&decoded).ok()? else {
        return None;
    };
    Some(map)
}

async fn fetch_with_login_cancellation(req: Request, signal: &AbortSignal) -> Result<Response> {
    match fetch(req).await {
        Ok(resp) => Ok(resp),
        Err(err) => {
            if signal.aborted() {
                return Err(Error::msg("Login cancelled"));
            }
            Err(err)
        }
    }
}

async fn read_token_response(response: Response, operation: &str) -> Result<OAuthToken> {
    if !response.ok() {
        let status = response.status();
        let status_text = response.status_text().to_owned();
        let text = response.text().await.unwrap_or_default();
        let detail = if text.is_empty() { status_text } else { text };
        return Err(Error::msg(format!(
            "OpenAI Codex token {operation} failed ({status}): {detail}"
        )));
    }

    let raw_json: Value = response.json().await?;
    let obj = raw_json.as_object();
    let access_token = obj
        .and_then(|j| j.get("access_token"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let refresh_token = obj
        .and_then(|j| j.get("refresh_token"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let expires_in = obj.and_then(|j| j.get("expires_in")).and_then(Value::as_f64);

    let (Some(access), Some(refresh), Some(expires_secs)) = (access_token, refresh_token, expires_in) else {
        return Err(Error::msg(format!(
            "OpenAI Codex token {operation} response missing fields: {}",
            pi_js::json::stringify(&raw_json)
        )));
    };

    Ok(OAuthToken {
        access: access.to_owned(),
        refresh: refresh.to_owned(),
        expires: pi_js::time::now_ms() + (expires_secs * 1000.0) as i64,
    })
}

async fn exchange_authorization_code(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    signal: &AbortSignal,
) -> Result<OAuthToken> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("code", code)
        .append_pair("code_verifier", verifier)
        .append_pair("redirect_uri", redirect_uri)
        .finish();

    let mut headers = Headers::new();
    headers.set("Content-Type", "application/x-www-form-urlencoded");

    let req = Request::post(TOKEN_URL)
        .with_headers(headers)
        .body(Body::from(body))
        .signal(signal.clone());
    let response = fetch_with_login_cancellation(req, signal).await?;
    read_token_response(response, "exchange").await
}

async fn refresh_access_token(refresh_token: &str, signal: &AbortSignal) -> Result<OAuthToken> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", refresh_token)
        .append_pair("client_id", CLIENT_ID)
        .finish();

    let mut headers = Headers::new();
    headers.set("Content-Type", "application/x-www-form-urlencoded");

    let req = Request::post(TOKEN_URL)
        .with_headers(headers)
        .body(Body::from(body))
        .signal(signal.clone());
    let response = fetch(req)
        .await
        .map_err(|err| Error::msg(format!("OpenAI Codex token refresh error: {err}")))?;
    read_token_response(response, "refresh").await
}

async fn start_openai_codex_device_auth(signal: &AbortSignal) -> Result<DeviceAuthInfo> {
    let _ = AUTH_BASE_URL;
    let mut headers = Headers::new();
    headers.set("Content-Type", "application/json");
    let body = pi_js::json::stringify(&serde_json::json!({ "client_id": CLIENT_ID }));

    let req = Request::post(DEVICE_USER_CODE_URL)
        .with_headers(headers)
        .body(Body::from(body))
        .signal(signal.clone());
    let response = fetch_with_login_cancellation(req, signal).await?;

    if !response.ok() {
        let status = response.status();
        if status == 404 {
            return Err(Error::msg(
                "OpenAI Codex device code login is not enabled for this server. Use browser login or verify the server URL.",
            ));
        }
        let body_text = response.text().await.unwrap_or_default();
        let suffix = if body_text.is_empty() {
            String::new()
        } else {
            format!(": {body_text}")
        };
        return Err(Error::msg(format!(
            "OpenAI Codex device code request failed with status {status}{suffix}"
        )));
    }

    let raw_json: Value = response.json().await?;
    let obj = raw_json.as_object();
    let device_auth_id = obj
        .and_then(|j| j.get("device_auth_id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let user_code = obj
        .and_then(|j| j.get("user_code"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let interval_seconds = match obj.and_then(|j| j.get("interval")) {
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        Some(v) => v.as_f64(),
        None => None,
    };

    let (Some(device_auth_id), Some(user_code), Some(interval_secs)) = (
        device_auth_id,
        user_code,
        interval_seconds.filter(|n| n.is_finite() && *n >= 0.0),
    ) else {
        return Err(Error::msg(format!(
            "Invalid OpenAI Codex device code response: {}",
            pi_js::json::stringify(&raw_json)
        )));
    };

    Ok(DeviceAuthInfo {
        device_auth_id: device_auth_id.to_owned(),
        user_code: user_code.to_owned(),
        interval_seconds: interval_secs,
    })
}

async fn poll_openai_codex_device_auth(device: &DeviceAuthInfo, signal: &AbortSignal) -> Result<DeviceTokenSuccess> {
    poll_oauth_device_code_flow(
        Some(device.interval_seconds),
        Some(DEVICE_CODE_TIMEOUT_SECONDS),
        false,
        signal,
        || async {
            let mut headers = Headers::new();
            headers.set("Content-Type", "application/json");
            let body = pi_js::json::stringify(&serde_json::json!({
                "device_auth_id": device.device_auth_id,
                "user_code": device.user_code,
            }));

            let req = Request::post(DEVICE_TOKEN_URL)
                .with_headers(headers)
                .body(Body::from(body))
                .signal(signal.clone());
            let response = fetch_with_login_cancellation(req, signal).await?;
            let status = response.status();

            if response.ok() {
                let raw_json: Value = response.json().await?;
                let obj = raw_json.as_object();
                let auth_code = obj
                    .and_then(|j| j.get("authorization_code"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty());
                let code_verifier = obj
                    .and_then(|j| j.get("code_verifier"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty());

                return match (auth_code, code_verifier) {
                    (Some(authorization_code), Some(code_verifier)) => Ok(OAuthDeviceCodePollResult::Complete {
                        value: DeviceTokenSuccess {
                            authorization_code: authorization_code.to_owned(),
                            code_verifier: code_verifier.to_owned(),
                        },
                    }),
                    _ => Ok(OAuthDeviceCodePollResult::Failed {
                        message: format!(
                            "Invalid OpenAI Codex device auth token response: {}",
                            pi_js::json::stringify(&raw_json)
                        ),
                    }),
                };
            }

            if status == 403 || status == 404 {
                return Ok(OAuthDeviceCodePollResult::Pending);
            }

            let response_body = response.text().await.unwrap_or_default();
            let error_code = pi_js::json::parse(&response_body).ok().and_then(|v| {
                let err = v.get("error")?;
                if let Some(s) = err.as_str() {
                    Some(s.to_owned())
                } else {
                    err.get("code").and_then(Value::as_str).map(str::to_owned)
                }
            });

            if error_code.as_deref() == Some("deviceauth_authorization_pending") {
                return Ok(OAuthDeviceCodePollResult::Pending);
            }
            if error_code.as_deref() == Some("slow_down") {
                return Ok(OAuthDeviceCodePollResult::SlowDown { interval_seconds: None });
            }

            let suffix = if response_body.is_empty() {
                String::new()
            } else {
                format!(": {response_body}")
            };
            Ok(OAuthDeviceCodePollResult::Failed {
                message: format!("OpenAI Codex device auth failed with status {status}{suffix}"),
            })
        },
    )
    .await
}

async fn create_authorization_flow(originator: &str) -> Result<(String, String, String)> {
    let pkce = generate_pkce().await?;
    let state = create_state();

    let mut parsed = url::Url::parse(AUTHORIZE_URL).map_err(|e| Error::msg(e.to_string()))?;
    parsed
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", SCOPE)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", originator);

    Ok((pkce.verifier, state, parsed.to_string()))
}

fn get_account_id(access_token: &str) -> Option<String> {
    let payload = decode_jwt(access_token)?;
    let account_id = payload
        .get(JWT_CLAIM_PATH)?
        .as_object()?
        .get("chatgpt_account_id")?
        .as_str()?;
    if account_id.is_empty() {
        None
    } else {
        Some(account_id.to_owned())
    }
}

fn credentials_from_token(token: OAuthToken) -> Result<OAuthCredential> {
    let Some(account_id) = get_account_id(&token.access) else {
        return Err(Error::msg("Failed to extract accountId from token"));
    };
    let mut cred = OAuthCredential::new(token.refresh, token.access, token.expires);
    cred.set_extra("accountId", Value::String(account_id));
    Ok(cred)
}

async fn exchange_authorization_code_for_credentials(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    signal: &AbortSignal,
) -> Result<OAuthCredential> {
    credentials_from_token(exchange_authorization_code(code, verifier, redirect_uri, signal).await?)
}

async fn login_openai_codex_device_code(interaction: &ProviderAuthInteraction) -> Result<OAuthCredential> {
    let device = start_openai_codex_device_auth(&interaction.signal).await?;
    interaction.notify(AuthEvent::DeviceCode {
        user_code: device.user_code.clone(),
        verification_uri: DEVICE_VERIFICATION_URI.to_owned(),
        interval_seconds: Some(device.interval_seconds),
        expires_in_seconds: Some(DEVICE_CODE_TIMEOUT_SECONDS),
    });
    let code = poll_openai_codex_device_auth(&device, &interaction.signal).await?;
    exchange_authorization_code_for_credentials(
        &code.authorization_code,
        &code.code_verifier,
        DEVICE_REDIRECT_URI,
        &interaction.signal,
    )
    .await
}

async fn login_openai_codex(interaction: &ProviderAuthInteraction) -> Result<OAuthCredential> {
    let (verifier, state, url) = create_authorization_flow("pi").await?;
    let callback = start_oauth_callback_server(OAuthCallbackServerOptions {
        provider_name: "OpenAI".to_owned(),
        host: callback_host(),
        port: 1455,
        path: "/auth/callback".to_owned(),
        redirect_host: None,
        state: Some(state.clone()),
        complete: Box::new(|code| Box::pin(async move { Ok(code) })),
        signal: Some(interaction.signal.clone()),
        timeout_ms: None,
    })
    .await
    .ok();

    interaction.notify(AuthEvent::AuthUrl {
        url,
        instructions: Some("A browser window should open. Complete login to finish.".to_owned()),
    });

    let result = async {
        let wait_res = wait_for_callback_or_manual_input(
            interaction,
            callback.as_ref(),
            "Complete login in your browser, or paste the authorization code / redirect URL here:",
            REDIRECT_URI,
        )
        .await?;

        let code = match wait_res {
            CallbackOrManualInput::Callback(c) => Some(c),
            CallbackOrManualInput::Manual(input) => {
                let (parsed_code, parsed_state) = parse_authorization_input(&input);
                if let Some(ref st) = parsed_state {
                    if st != &state {
                        return Err(Error::msg("State mismatch"));
                    }
                }
                parsed_code
            }
        };

        let Some(code) = code.filter(|s| !s.is_empty()) else {
            return Err(Error::msg("Missing authorization code"));
        };
        exchange_authorization_code_for_credentials(&code, &verifier, REDIRECT_URI, &interaction.signal).await
    }
    .await;

    if let Some(cb) = callback.as_ref() {
        cb.close();
    }
    result
}

pub struct OpenAICodexOAuth;

#[async_trait::async_trait]
impl OAuthAuth for OpenAICodexOAuth {
    fn name(&self) -> &str {
        "OpenAI (ChatGPT Plus/Pro)"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    async fn login(
        &self,
        interaction: ProviderAuthInteraction,
        _options: Option<LoginOptions>,
    ) -> Result<OAuthCredential> {
        let method = interaction
            .prompt(AuthPrompt::select(
                "Select OpenAI Codex login method:",
                vec![
                    AuthSelectOption {
                        id: OPENAI_CODEX_BROWSER_LOGIN_METHOD.to_owned(),
                        label: "Browser login (default)".to_owned(),
                        description: None,
                    },
                    AuthSelectOption {
                        id: OPENAI_CODEX_DEVICE_CODE_LOGIN_METHOD.to_owned(),
                        label: "Device code login (headless)".to_owned(),
                        description: None,
                    },
                ],
            ))
            .await?;

        if method == OPENAI_CODEX_DEVICE_CODE_LOGIN_METHOD {
            return login_openai_codex_device_code(&interaction).await;
        }
        if method != OPENAI_CODEX_BROWSER_LOGIN_METHOD {
            return Err(Error::msg(format!("Unknown OpenAI Codex login method: {method}")));
        }
        login_openai_codex(&interaction).await
    }

    async fn refresh(&self, credential: OAuthCredential, signal: AbortSignal) -> Result<OAuthCredential> {
        credentials_from_token(refresh_access_token(&credential.refresh, &signal).await?)
    }

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access),
            headers: None,
            base_url: None,
        })
    }
}

pub fn openai_codex_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(OpenAICodexOAuth)
}
