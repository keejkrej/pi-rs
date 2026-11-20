//! Port of packages/ai/src/auth/oauth/meta.ts

use std::sync::Arc;

use serde_json::{Map, Value};

use pi_js::abort::AbortSignal;
use pi_js::fetch::{Body, Headers, Request, Response, fetch};
use pi_js::{Error, Result};

use crate::auth::oauth::device_code::{OAuthDeviceCodePollResult, poll_oauth_device_code_flow};
use crate::auth::oauth::trusted_url::trusted_http_url;
use crate::auth::types::{AuthEvent, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction};

const CLIENT_ID: &str = "1031625952748946";
const AUTH_HOST: &str = "https://auth.meta.com";
const DEVICE_AUTHORIZATION_URL: &str = "https://auth.meta.com/oidc/device/authorization/";
const DEVICE_TOKEN_URL: &str = "https://auth.meta.com/oidc/device/token/";
const API_KEY_MINT_URL: &str = "https://api.meta.ai/muse-code/key";
const API_KEY_LIFETIME_MS: i64 = 24 * 60 * 60 * 1000;
const REQUEST_TIMEOUT_MS: u64 = 30 * 1000;

struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval_seconds: Option<f64>,
    expires_in_seconds: Option<f64>,
}

fn request_signal(signal: &AbortSignal) -> AbortSignal {
    AbortSignal::any(&[AbortSignal::timeout(REQUEST_TIMEOUT_MS), signal.clone()])
}

async fn read_json_object(response: Response) -> Option<Map<String, Value>> {
    let parsed: Value = response.json().await.ok()?;
    match parsed {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn error_detail(json: Option<&Map<String, Value>>) -> String {
    let Some(map) = json else {
        return String::new();
    };
    for key in ["error_description", "detail", "message", "error"] {
        if let Some(val) = map.get(key).and_then(Value::as_str) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return format!(": {trimmed}");
            }
        }
    }
    String::new()
}

fn positive_number(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64).filter(|v| v.is_finite() && *v > 0.0)
}

async fn start_device_authorization(signal: &AbortSignal) -> Result<DeviceAuthorization> {
    let _ = AUTH_HOST;
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", CLIENT_ID)
        .finish();
    let mut headers = Headers::new();
    headers.set("Content-Type", "application/x-www-form-urlencoded");
    headers.set("Accept", "application/json");

    let req = Request::post(DEVICE_AUTHORIZATION_URL)
        .with_headers(headers)
        .body(Body::from(body))
        .signal(request_signal(signal));
    let response = fetch(req).await?;
    let status = response.status();
    let ok = response.ok();
    let json = read_json_object(response).await;
    if !ok {
        return Err(Error::msg(format!(
            "Meta device authorization failed with status {status}{}",
            error_detail(json.as_ref())
        )));
    }

    let obj = json.as_ref();
    let device_code = obj
        .and_then(|j| j.get("device_code"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let user_code = obj
        .and_then(|j| j.get("user_code"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let verification_uri = obj
        .and_then(|j| j.get("verification_uri_complete"))
        .and_then(Value::as_str)
        .and_then(trusted_http_url)
        .or_else(|| {
            obj.and_then(|j| j.get("verification_uri"))
                .and_then(Value::as_str)
                .and_then(trusted_http_url)
        });

    let (Some(device_code), Some(user_code), Some(verification_uri)) = (device_code, user_code, verification_uri)
    else {
        let json_str = obj
            .map(|m| pi_js::json::stringify(&Value::Object(m.clone())))
            .unwrap_or_else(|| "null".to_owned());
        return Err(Error::msg(format!(
            "Invalid Meta device authorization response: {json_str}"
        )));
    };

    Ok(DeviceAuthorization {
        device_code: device_code.to_owned(),
        user_code: user_code.to_owned(),
        verification_uri,
        interval_seconds: positive_number(obj.and_then(|j| j.get("interval"))),
        expires_in_seconds: positive_number(obj.and_then(|j| j.get("expires_in"))),
    })
}

async fn poll_for_identity_token(device: &DeviceAuthorization, signal: &AbortSignal) -> Result<String> {
    poll_oauth_device_code_flow(
        device.interval_seconds,
        device.expires_in_seconds,
        true,
        signal,
        || async {
            let body = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("grant_type", "urn:ietf:params:oauth:grant-type:device_code")
                .append_pair("device_code", &device.device_code)
                .append_pair("client_id", CLIENT_ID)
                .finish();
            let mut headers = Headers::new();
            headers.set("Content-Type", "application/x-www-form-urlencoded");
            headers.set("Accept", "application/json");

            let req = Request::post(DEVICE_TOKEN_URL)
                .with_headers(headers)
                .body(Body::from(body))
                .signal(request_signal(signal));
            let response = fetch(req).await?;
            let status = response.status();
            let ok = response.ok();
            let json = read_json_object(response).await;

            if ok {
                if let Some(access_token) = json
                    .as_ref()
                    .and_then(|j| j.get("access_token"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    return Ok(OAuthDeviceCodePollResult::Complete {
                        value: access_token.to_owned(),
                    });
                }
            }

            let err_str = json.as_ref().and_then(|j| j.get("error")).and_then(Value::as_str);
            match err_str {
                Some("authorization_pending") => Ok(OAuthDeviceCodePollResult::Pending),
                Some("slow_down") => Ok(OAuthDeviceCodePollResult::SlowDown {
                    interval_seconds: positive_number(json.as_ref().and_then(|j| j.get("interval"))),
                }),
                Some("access_denied") => Ok(OAuthDeviceCodePollResult::Failed {
                    message: "Meta login was denied.".to_owned(),
                }),
                Some("expired_token") => Ok(OAuthDeviceCodePollResult::Failed {
                    message: "Meta device authorization expired. Please restart login.".to_owned(),
                }),
                _ => Ok(OAuthDeviceCodePollResult::Failed {
                    message: format!(
                        "Meta device token request failed with status {status}{}",
                        error_detail(json.as_ref())
                    ),
                }),
            }
        },
    )
    .await
}

async fn mint_api_key(identity_token: &str, signal: &AbortSignal) -> Result<OAuthCredential> {
    let mut headers = Headers::new();
    headers.set("Accept", "application/json");
    headers.set("Authorization", &format!("Bearer {identity_token}"));
    headers.set("Content-Type", "application/json");
    headers.set("x-api-version", "1.0.0");

    let req = Request::post(API_KEY_MINT_URL)
        .with_headers(headers)
        .body(Body::from("{}"))
        .signal(request_signal(signal));
    let response = fetch(req).await?;
    let status = response.status();
    let ok = response.ok();
    let json = read_json_object(response).await;

    if status == 401 || status == 403 {
        return Err(Error::msg(format!(
            "Meta session expired (status {status}). Run `/login meta` to sign in again.{}",
            error_detail(json.as_ref())
        )));
    }
    if !ok {
        return Err(Error::msg(format!(
            "Meta API key mint failed with status {status}{}",
            error_detail(json.as_ref())
        )));
    }

    let api_key = json
        .as_ref()
        .and_then(|j| j.get("api_key"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let Some(key) = api_key else {
        let action_url = json
            .as_ref()
            .and_then(|j| j.get("action_url"))
            .and_then(Value::as_str)
            .and_then(trusted_http_url);
        let suffix = action_url
            .map(|u| format!(" Complete setup at {u}"))
            .unwrap_or_default();
        return Err(Error::msg(format!("Meta did not issue an API key.{suffix}")));
    };

    Ok(OAuthCredential::new(
        identity_token,
        key,
        pi_js::time::now_ms() + API_KEY_LIFETIME_MS,
    ))
}

async fn login_meta(interaction: ProviderAuthInteraction) -> Result<OAuthCredential> {
    let result: Result<OAuthCredential> = async {
        let device = start_device_authorization(&interaction.signal).await?;
        interaction.notify(AuthEvent::DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri.clone(),
            open_browser_on_confirm: None,
            copy_code_on_confirm: None,
            interval_seconds: device.interval_seconds,
            expires_in_seconds: device.expires_in_seconds,
        });
        let identity_token = poll_for_identity_token(&device, &interaction.signal).await?;
        interaction.notify(AuthEvent::Progress {
            message: "Enabling Meta Model API access...".to_owned(),
        });
        mint_api_key(&identity_token, &interaction.signal).await
    }
    .await;

    if interaction.signal.aborted() {
        return Err(Error::msg("Login cancelled"));
    }
    result
}

pub struct MetaOAuth;

#[async_trait::async_trait]
impl OAuthAuth for MetaOAuth {
    fn name(&self) -> &str {
        "Meta (Muse subscription)"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    fn login_label(&self) -> Option<&str> {
        Some("Sign in with Meta")
    }

    async fn login(
        &self,
        interaction: ProviderAuthInteraction,
        _options: Option<LoginOptions>,
    ) -> Result<OAuthCredential> {
        login_meta(interaction).await
    }

    async fn refresh(&self, credential: OAuthCredential, signal: AbortSignal) -> Result<OAuthCredential> {
        mint_api_key(&credential.refresh, &signal).await
    }

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access),
            headers: None,
            base_url: None,
        })
    }
}

pub fn meta_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(MetaOAuth)
}
