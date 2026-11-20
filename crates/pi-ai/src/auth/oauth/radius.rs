//! Port of packages/ai/src/auth/oauth/radius.ts

use std::sync::Arc;

use serde_json::Value;

use pi_js::abort::AbortSignal;
use pi_js::fetch::{Body, Headers, Request, Response, fetch};
use pi_js::{Error, Result};

use crate::auth::oauth::callback_server::{OAuthCallbackServerOptions, start_oauth_callback_server};
use crate::auth::oauth::device_code::{OAuthDeviceCodePollResult, poll_oauth_device_code_flow};
use crate::auth::oauth::pkce::generate_pkce;
use crate::auth::oauth::trusted_url::trusted_http_url;
use crate::auth::types::{
    AuthEvent, AuthPrompt, AuthSelectOption, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential,
    ProviderAuthInteraction,
};

const CALLBACK_HOST: &str = "127.0.0.1";
const CALLBACK_PORT: u16 = 1456;
const CALLBACK_PATH: &str = "/oauth/callback";
const REDIRECT_URI: &str = "http://127.0.0.1:1456/oauth/callback";
const TOKEN_EXPIRY_SKEW_MS: i64 = 60_000;
const LOGIN_METHOD_BROWSER: &str = "browser";
const LOGIN_METHOD_DEVICE_CODE: &str = "device-code";
const OAUTH_CLIENT_ID: &str = "pi-gateway";
const OAUTH_SCOPE: &str = "gateway offline_access";
const OAUTH_DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

#[derive(Clone, Debug)]
pub struct RadiusOAuthOptions {
    pub name: String,
    pub gateway: String,
}

use crate::providers::radius_config::normalize_radius_gateway_url;

fn gateway_url(gateway: &str, path: &str) -> Result<String> {
    let base =
        url::Url::parse(gateway).map_err(|e| Error::msg(format!("Invalid Radius gateway URL {gateway}: {e}")))?;
    base.join(path)
        .map(|u| u.to_string())
        .map_err(|e| Error::msg(e.to_string()))
}

struct OAuthResponseError {
    oauth_error: Option<String>,
    error: Error,
}

async fn read_oauth_response_error(response: Response, message: &str) -> OAuthResponseError {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let mut oauth_error: Option<String> = None;
    let mut description: Option<String> = None;

    if !text.is_empty() {
        if let Ok(Value::Object(map)) = pi_js::json::parse(&text) {
            oauth_error = map.get("error").and_then(Value::as_str).map(str::to_owned);
            description = map.get("error_description").and_then(Value::as_str).map(str::to_owned);
        } else {
            description = Some(text);
        }
    }

    let detail = match (&oauth_error, &description) {
        (Some(err), Some(desc)) if !desc.is_empty() => format!("{err}: {desc}"),
        (Some(err), _) => err.clone(),
        (None, Some(desc)) if !desc.is_empty() => desc.clone(),
        _ => status.to_string(),
    };

    OAuthResponseError {
        oauth_error,
        error: Error::msg(format!("{message}: {detail}")),
    }
}

async fn load_radius_oauth_discovery(gateway: &str, signal: &AbortSignal) -> Result<String> {
    let target = gateway_url(gateway, "/v1/oauth")?;
    let mut headers = Headers::new();
    headers.set("accept", "application/json");
    let req = Request::get(&target).with_headers(headers).signal(signal.clone());
    let response = fetch(req).await?;

    if !response.ok() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::msg(format!(
            "Could not load Radius OAuth config from {gateway}: {status} {text}"
        )));
    }

    let discovery: Value = response.json().await?;
    let Some(endpoint) = discovery
        .get("authorizationEndpoint")
        .and_then(Value::as_str)
        .and_then(trusted_http_url)
    else {
        return Err(Error::msg(format!("Invalid Radius OAuth config from {gateway}")));
    };
    Ok(endpoint)
}

async fn request_oauth_token(
    gateway: &str,
    params: &[(&str, &str)],
    signal: &AbortSignal,
) -> std::result::Result<OAuthCredential, OAuthResponseError> {
    let target = match gateway_url(gateway, "/v1/oauth/token") {
        Ok(u) => u,
        Err(e) => {
            return Err(OAuthResponseError {
                oauth_error: None,
                error: e,
            });
        }
    };

    let body = {
        let mut ser = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in params {
            ser.append_pair(k, v);
        }
        ser.finish()
    };

    let mut headers = Headers::new();
    headers.set("accept", "application/json");
    headers.set("content-type", "application/x-www-form-urlencoded");

    let req = Request::post(&target)
        .with_headers(headers)
        .body(Body::from(body))
        .signal(signal.clone());

    let response = match fetch(req).await {
        Ok(resp) => resp,
        Err(err) => {
            if signal.aborted() {
                return Err(OAuthResponseError {
                    oauth_error: None,
                    error: Error::msg("Login cancelled"),
                });
            }
            return Err(OAuthResponseError {
                oauth_error: None,
                error: err,
            });
        }
    };

    if !response.ok() {
        return Err(read_oauth_response_error(response, "Radius OAuth token request failed").await);
    }

    let data: Value = response.json().await.map_err(|e| OAuthResponseError {
        oauth_error: None,
        error: e,
    })?;
    let obj = data.as_object();
    let access = obj.and_then(|m| m.get("access_token")).and_then(Value::as_str);
    let refresh = obj.and_then(|m| m.get("refresh_token")).and_then(Value::as_str);
    let expires_in = obj.and_then(|m| m.get("expires_in")).and_then(Value::as_f64);
    let scope = obj.and_then(|m| m.get("scope")).and_then(Value::as_str);

    let (Some(access), Some(refresh), Some(expires_in)) = (access, refresh, expires_in) else {
        return Err(OAuthResponseError {
            oauth_error: None,
            error: Error::msg("Invalid Radius OAuth token response"),
        });
    };

    let expires = pi_js::time::now_ms() + (expires_in * 1000.0) as i64 - TOKEN_EXPIRY_SKEW_MS;
    let mut cred = OAuthCredential::new(refresh, access, expires);
    if let Some(s) = scope {
        cred.set_extra("scope", Value::String(s.to_owned()));
    }
    Ok(cred)
}

async fn login_with_browser(
    gateway: &str,
    authorization_endpoint: &str,
    interaction: &ProviderAuthInteraction,
) -> Result<OAuthCredential> {
    let pkce = generate_pkce().await?;
    let state = pi_js::crypto::random_uuid();

    let mut authorize_url = url::Url::parse(authorization_endpoint).map_err(|e| Error::msg(e.to_string()))?;
    authorize_url
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", OAUTH_CLIENT_ID)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", OAUTH_SCOPE)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("handoff", "url")
        .append_pair("state", &state);

    let gateway_owned = gateway.to_owned();
    let verifier_owned = pkce.verifier.clone();
    let sig_owned = interaction.signal.clone();

    let callback = start_oauth_callback_server(OAuthCallbackServerOptions {
        provider_name: "Radius".to_owned(),
        host: CALLBACK_HOST.to_owned(),
        port: CALLBACK_PORT,
        path: CALLBACK_PATH.to_owned(),
        redirect_host: None,
        state: Some(state),
        complete: Box::new(move |code| {
            Box::pin(async move {
                request_oauth_token(
                    &gateway_owned,
                    &[
                        ("grant_type", "authorization_code"),
                        ("client_id", OAUTH_CLIENT_ID),
                        ("redirect_uri", REDIRECT_URI),
                        ("code", &code),
                        ("code_verifier", &verifier_owned),
                    ],
                    &sig_owned,
                )
                .await
                .map_err(|e| e.error)
            })
        }),
        signal: Some(interaction.signal.clone()),
        timeout_ms: None,
    })
    .await?;

    interaction.notify(AuthEvent::Progress {
        message: format!("Listening for OAuth callback on {REDIRECT_URI}"),
    });
    interaction.notify(AuthEvent::AuthUrl {
        url: authorize_url.to_string(),
        instructions: Some("Continue in your browser.".to_owned()),
    });

    let wait_res = callback.wait().await;
    callback.close();
    let maybe_cred = wait_res?;
    maybe_cred.ok_or_else(|| Error::msg("OAuth callback did not complete."))
}

struct DeviceAuthorizationResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: f64,
    interval: Option<f64>,
}

async fn request_device_authorization(gateway: &str, signal: &AbortSignal) -> Result<DeviceAuthorizationResponse> {
    let target = gateway_url(gateway, "/v1/oauth/device")?;
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", OAUTH_CLIENT_ID)
        .append_pair("scope", OAUTH_SCOPE)
        .finish();
    let mut headers = Headers::new();
    headers.set("accept", "application/json");
    headers.set("content-type", "application/x-www-form-urlencoded");

    let req = Request::post(&target)
        .with_headers(headers)
        .body(Body::from(body))
        .signal(signal.clone());
    let response = match fetch(req).await {
        Ok(resp) => resp,
        Err(err) => {
            if signal.aborted() {
                return Err(Error::msg("Login cancelled"));
            }
            return Err(err);
        }
    };

    if !response.ok() {
        return Err(
            read_oauth_response_error(response, "Radius OAuth device authorization failed")
                .await
                .error,
        );
    }

    let data: Value = response.json().await?;
    let obj = data.as_object();
    let device_code = obj
        .and_then(|m| m.get("device_code"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let user_code = obj
        .and_then(|m| m.get("user_code"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let verification_uri = obj
        .and_then(|m| m.get("verification_uri"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let expires_in = obj
        .and_then(|m| m.get("expires_in"))
        .and_then(Value::as_f64)
        .filter(|n| *n != 0.0);
    let interval = obj.and_then(|m| m.get("interval")).and_then(Value::as_f64);

    let (Some(device_code), Some(user_code), Some(verification_uri), Some(expires_in)) =
        (device_code, user_code, verification_uri, expires_in)
    else {
        return Err(Error::msg(
            "Radius OAuth device authorization response is missing required fields",
        ));
    };
    let Some(trusted_uri) = trusted_http_url(verification_uri) else {
        return Err(Error::msg(
            "Untrusted verification_uri in Radius OAuth device authorization response",
        ));
    };

    Ok(DeviceAuthorizationResponse {
        device_code: device_code.to_owned(),
        user_code: user_code.to_owned(),
        verification_uri: trusted_uri,
        expires_in,
        interval,
    })
}

async fn login_with_device_code(gateway: &str, interaction: &ProviderAuthInteraction) -> Result<OAuthCredential> {
    let device = request_device_authorization(gateway, &interaction.signal).await?;
    interaction.notify(AuthEvent::DeviceCode {
        user_code: device.user_code.clone(),
        verification_uri: device.verification_uri.clone(),
        open_browser_on_confirm: None,
        copy_code_on_confirm: None,
        interval_seconds: device.interval,
        expires_in_seconds: Some(device.expires_in),
    });

    poll_oauth_device_code_flow(
        device.interval,
        Some(device.expires_in),
        false,
        &interaction.signal,
        || async {
            match request_oauth_token(
                gateway,
                &[
                    ("grant_type", OAUTH_DEVICE_CODE_GRANT_TYPE),
                    ("client_id", OAUTH_CLIENT_ID),
                    ("device_code", &device.device_code),
                ],
                &interaction.signal,
            )
            .await
            {
                Ok(credentials) => Ok(OAuthDeviceCodePollResult::Complete { value: credentials }),
                Err(err) => match err.oauth_error.as_deref() {
                    Some("authorization_pending") => Ok(OAuthDeviceCodePollResult::Pending),
                    Some("slow_down") => Ok(OAuthDeviceCodePollResult::SlowDown { interval_seconds: None }),
                    Some("expired_token") => Ok(OAuthDeviceCodePollResult::Failed {
                        message: "Device authorization expired.".to_owned(),
                    }),
                    Some("access_denied") => Ok(OAuthDeviceCodePollResult::Failed {
                        message: "Device authorization was denied.".to_owned(),
                    }),
                    _ => Err(err.error),
                },
            }
        },
    )
    .await
}

struct RadiusOAuthImpl {
    name: String,
    gateway: String,
}

#[async_trait::async_trait]
impl OAuthAuth for RadiusOAuthImpl {
    fn name(&self) -> &str {
        &self.name
    }

    async fn login(
        &self,
        interaction: ProviderAuthInteraction,
        _options: Option<LoginOptions>,
    ) -> Result<OAuthCredential> {
        let login_method = interaction
            .prompt(AuthPrompt::select(
                format!("Sign in to {}:", self.name),
                vec![
                    AuthSelectOption {
                        id: LOGIN_METHOD_BROWSER.to_owned(),
                        label: "Sign in with browser (recommended)".to_owned(),
                        description: None,
                    },
                    AuthSelectOption {
                        id: LOGIN_METHOD_DEVICE_CODE.to_owned(),
                        label: "Sign in with device code (when signing in from another device)".to_owned(),
                        description: None,
                    },
                ],
            ))
            .await?;

        if login_method == LOGIN_METHOD_DEVICE_CODE {
            return login_with_device_code(&self.gateway, &interaction).await;
        }
        if login_method == LOGIN_METHOD_BROWSER {
            let authorization_endpoint = load_radius_oauth_discovery(&self.gateway, &interaction.signal).await?;
            return login_with_browser(&self.gateway, &authorization_endpoint, &interaction).await;
        }
        Err(Error::msg(format!(
            "Unknown {} sign-in method: {login_method}",
            self.name
        )))
    }

    async fn refresh(&self, credential: OAuthCredential, signal: AbortSignal) -> Result<OAuthCredential> {
        request_oauth_token(
            &self.gateway,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", OAUTH_CLIENT_ID),
                ("refresh_token", &credential.refresh),
            ],
            &signal,
        )
        .await
        .map_err(|e| e.error)
    }

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access),
            headers: None,
            base_url: None,
        })
    }
}

pub fn create_radius_oauth(options: RadiusOAuthOptions) -> Arc<dyn OAuthAuth> {
    Arc::new(RadiusOAuthImpl {
        name: options.name,
        gateway: normalize_radius_gateway_url(&options.gateway),
    })
}
