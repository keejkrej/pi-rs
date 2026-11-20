//! Port of packages/ai/src/auth/oauth/kimi-coding.ts

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::{Map, Value};

use pi_js::abort::AbortSignal;
use pi_js::fetch::{Body, Headers, Request, Response, fetch};
use pi_js::{Error, Result};

use crate::auth::oauth::device_code::{OAuthDeviceCodePollResult, poll_oauth_device_code_flow};
use crate::auth::oauth::trusted_url::trusted_http_url;
use crate::auth::types::{AuthEvent, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction};
use crate::utils::provider_env::get_provider_env_value;

const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const DEFAULT_OAUTH_HOST: &str = "https://auth.kimi.com";
const DEVICE_CODE_TIMEOUT_SECONDS: f64 = 15.0 * 60.0;
const DEFAULT_POLL_INTERVAL_SECONDS: f64 = 5.0;
const REQUEST_TIMEOUT_MS: u64 = 30 * 1000;
const REFRESH_MAX_RETRIES: u32 = 3;

struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri_complete: String,
    interval_seconds: f64,
    expires_in_seconds: f64,
}

fn get_oauth_host() -> String {
    let raw = get_provider_env_value("KIMI_CODE_OAUTH_HOST", None)
        .or_else(|| get_provider_env_value("KIMI_OAUTH_HOST", None))
        .unwrap_or_else(|| DEFAULT_OAUTH_HOST.to_owned());
    raw.trim_end_matches('/').to_owned()
}

fn request_signal(signal: &AbortSignal) -> AbortSignal {
    AbortSignal::any(&[AbortSignal::timeout(REQUEST_TIMEOUT_MS), signal.clone()])
}

fn form_url_encode(fields: &[(&str, &str)]) -> String {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in fields {
        ser.append_pair(k, v);
    }
    ser.finish()
}

async fn read_json_object(response: Response) -> Option<Map<String, Value>> {
    let parsed: Value = response.json().await.ok()?;
    match parsed {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

async fn start_device_authorization(oauth_host: &str, signal: &AbortSignal) -> Result<DeviceAuthorization> {
    let mut headers = Headers::new();
    headers.set("Content-Type", "application/x-www-form-urlencoded");
    headers.set("Accept", "application/json");

    let req = Request::post(format!("{oauth_host}/api/oauth/device_authorization"))
        .with_headers(headers)
        .body(Body::from(form_url_encode(&[("client_id", CLIENT_ID)])))
        .signal(request_signal(signal));
    let response = fetch(req).await?;

    if !response.ok() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let suffix = if text.is_empty() {
            String::new()
        } else {
            format!(": {text}")
        };
        return Err(Error::msg(format!(
            "Kimi Code device authorization failed with status {status}{suffix}"
        )));
    }

    let json = read_json_object(response).await;
    let obj_ref = json.as_ref();
    let device_code = obj_ref.and_then(|j| j.get("device_code")).and_then(Value::as_str);
    let user_code = obj_ref.and_then(|j| j.get("user_code")).and_then(Value::as_str);
    let verification_uri = obj_ref
        .and_then(|j| j.get("verification_uri"))
        .and_then(Value::as_str)
        .and_then(trusted_http_url);
    let verification_uri_complete = obj_ref
        .and_then(|j| j.get("verification_uri_complete"))
        .and_then(Value::as_str)
        .and_then(trusted_http_url);

    let (Some(device_code), Some(user_code), Some(_verification_uri), Some(verification_uri_complete)) =
        (device_code, user_code, verification_uri, verification_uri_complete)
    else {
        return Err(Error::msg(format!(
            "Invalid Kimi Code device authorization response: {}",
            json.as_ref()
                .map(|m| pi_js::json::stringify(&Value::Object(m.clone())))
                .unwrap_or_else(|| "null".to_owned())
        )));
    };

    let interval = obj_ref.and_then(|j| j.get("interval")).and_then(Value::as_f64);
    let expires_in = obj_ref.and_then(|j| j.get("expires_in")).and_then(Value::as_f64);

    Ok(DeviceAuthorization {
        device_code: device_code.to_owned(),
        user_code: user_code.to_owned(),
        verification_uri_complete,
        interval_seconds: interval
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS),
        expires_in_seconds: expires_in
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(DEVICE_CODE_TIMEOUT_SECONDS),
    })
}

fn parse_token_response(json: Option<&Map<String, Value>>, operation: &str) -> Result<OAuthCredential> {
    let access_token = json
        .and_then(|j| j.get("access_token"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let refresh_token = json
        .and_then(|j| j.get("refresh_token"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let expires_in = json
        .and_then(|j| j.get("expires_in"))
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v > 0.0);

    let (Some(access), Some(refresh), Some(expires_secs)) = (access_token, refresh_token, expires_in) else {
        let json_str = json
            .map(|m| pi_js::json::stringify(&Value::Object(m.clone())))
            .unwrap_or_else(|| "null".to_owned());
        return Err(Error::msg(format!(
            "Kimi Code token {operation} response missing fields: {json_str}"
        )));
    };

    let expires = pi_js::time::now_ms() + (expires_secs * 1000.0) as i64;
    Ok(OAuthCredential::new(refresh, access, expires))
}

async fn poll_for_token(
    oauth_host: &str,
    device: &DeviceAuthorization,
    signal: &AbortSignal,
) -> Result<OAuthCredential> {
    poll_oauth_device_code_flow(
        Some(device.interval_seconds),
        Some(device.expires_in_seconds),
        true,
        signal,
        || async {
            let mut headers = Headers::new();
            headers.set("Content-Type", "application/x-www-form-urlencoded");
            headers.set("Accept", "application/json");

            let req = Request::post(format!("{oauth_host}/api/oauth/token"))
                .with_headers(headers)
                .body(Body::from(form_url_encode(&[
                    ("client_id", CLIENT_ID),
                    ("device_code", &device.device_code),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])))
                .signal(request_signal(signal));

            let response = fetch(req).await?;
            let status = response.status();
            let ok = response.ok();
            if status >= 500 {
                let text = response.text().await.unwrap_or_default();
                let suffix = if text.is_empty() {
                    String::new()
                } else {
                    format!(": {text}")
                };
                return Ok(OAuthDeviceCodePollResult::Failed {
                    message: format!("Kimi Code device token request failed with status {status}{suffix}"),
                });
            }

            let json = read_json_object(response).await;
            if ok
                && json
                    .as_ref()
                    .and_then(|j| j.get("access_token"))
                    .and_then(Value::as_str)
                    .is_some()
            {
                return match parse_token_response(json.as_ref(), "poll") {
                    Ok(value) => Ok(OAuthDeviceCodePollResult::Complete { value }),
                    Err(err) => Ok(OAuthDeviceCodePollResult::Failed {
                        message: err.to_string(),
                    }),
                };
            }

            let error = json.as_ref().and_then(|j| j.get("error")).and_then(Value::as_str);
            let description = json
                .as_ref()
                .and_then(|j| j.get("error_description"))
                .and_then(Value::as_str)
                .map(|d| format!(": {d}"))
                .unwrap_or_default();

            match error {
                Some("authorization_pending") => Ok(OAuthDeviceCodePollResult::Pending),
                Some("slow_down") => {
                    let interval_seconds = json
                        .as_ref()
                        .and_then(|j| j.get("interval"))
                        .and_then(Value::as_f64)
                        .filter(|v| *v > 0.0);
                    Ok(OAuthDeviceCodePollResult::SlowDown { interval_seconds })
                }
                Some("expired_token") => Ok(OAuthDeviceCodePollResult::Failed {
                    message: "Kimi Code device authorization expired. Please restart login.".to_owned(),
                }),
                Some("access_denied") => Ok(OAuthDeviceCodePollResult::Failed {
                    message: "Kimi Code login was denied.".to_owned(),
                }),
                Some(err_code) => Ok(OAuthDeviceCodePollResult::Failed {
                    message: format!(
                        "Kimi Code device token request failed (status {status}): {err_code}{description}"
                    ),
                }),
                None => Ok(OAuthDeviceCodePollResult::Failed {
                    message: format!("Kimi Code device token request failed (status {status})"),
                }),
            }
        },
    )
    .await
}

fn is_retryable_refresh_failure(status: u16) -> bool {
    status == 429 || status >= 500
}

async fn refresh_token(oauth_host: &str, refresh_token_value: &str, signal: &AbortSignal) -> Result<OAuthCredential> {
    let mut last_error: Option<Error> = None;

    for attempt in 0..=REFRESH_MAX_RETRIES {
        if attempt > 0 {
            let delay_ms = 1000 * 2u64.pow(attempt - 1);
            crate::utils::sleep::sleep(delay_ms, signal).await?;
        }
        if signal.aborted() {
            return Err(Error::msg("Kimi Code token refresh aborted"));
        }

        let mut headers = Headers::new();
        headers.set("Content-Type", "application/x-www-form-urlencoded");
        headers.set("Accept", "application/json");

        let req = Request::post(format!("{oauth_host}/api/oauth/token"))
            .with_headers(headers)
            .body(Body::from(form_url_encode(&[
                ("client_id", CLIENT_ID),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token_value),
            ])))
            .signal(request_signal(signal));

        let response = match fetch(req).await {
            Ok(resp) => resp,
            Err(err) => {
                last_error = Some(err);
                continue;
            }
        };

        let status = response.status();
        let ok = response.ok();
        let json = read_json_object(response).await;
        if ok {
            return parse_token_response(json.as_ref(), "refresh");
        }

        let is_invalid_grant =
            json.as_ref().and_then(|j| j.get("error")).and_then(Value::as_str) == Some("invalid_grant");
        if status == 401 || status == 403 || is_invalid_grant {
            let description = json
                .as_ref()
                .and_then(|j| j.get("error_description"))
                .and_then(Value::as_str)
                .map(|d| format!(": {d}"))
                .unwrap_or_default();
            return Err(Error::msg(format!(
                "Kimi Code token refresh unauthorized (status {status}){description}"
            )));
        }

        if is_retryable_refresh_failure(status) && attempt < REFRESH_MAX_RETRIES {
            last_error = Some(Error::msg(format!(
                "Kimi Code token refresh failed with status {status}"
            )));
            continue;
        }

        let text = json
            .as_ref()
            .map(|m| pi_js::json::stringify(&Value::Object(m.clone())))
            .unwrap_or_else(|| "null".to_owned());
        return Err(Error::msg(format!(
            "Kimi Code token refresh failed with status {status}: {text}"
        )));
    }

    Err(last_error.unwrap_or_else(|| Error::msg("Kimi Code token refresh failed")))
}

async fn login_kimi_coding(interaction: ProviderAuthInteraction) -> Result<OAuthCredential> {
    let oauth_host = get_oauth_host();
    let device = start_device_authorization(&oauth_host, &interaction.signal).await?;
    interaction.notify(AuthEvent::DeviceCode {
        user_code: device.user_code.clone(),
        verification_uri: device.verification_uri_complete.clone(),
        open_browser_on_confirm: None,
        copy_code_on_confirm: None,
        interval_seconds: Some(device.interval_seconds),
        expires_in_seconds: Some(device.expires_in_seconds),
    });
    poll_for_token(&oauth_host, &device, &interaction.signal).await
}

pub struct KimiCodingOAuth;

#[async_trait::async_trait]
impl OAuthAuth for KimiCodingOAuth {
    fn name(&self) -> &str {
        "Kimi Code (subscription)"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    fn login_label(&self) -> Option<&str> {
        Some("Sign in with Kimi Code")
    }

    async fn login(
        &self,
        interaction: ProviderAuthInteraction,
        _options: Option<LoginOptions>,
    ) -> Result<OAuthCredential> {
        login_kimi_coding(interaction).await
    }

    async fn refresh(&self, credential: OAuthCredential, signal: AbortSignal) -> Result<OAuthCredential> {
        refresh_token(&get_oauth_host(), &credential.refresh, &signal).await
    }

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth> {
        let mut headers = IndexMap::new();
        headers.insert(
            "Authorization".to_owned(),
            Some(format!("Bearer {}", credential.access)),
        );
        Ok(ModelAuth {
            api_key: None,
            headers: Some(headers),
            base_url: None,
        })
    }
}

pub fn kimi_coding_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(KimiCodingOAuth)
}
