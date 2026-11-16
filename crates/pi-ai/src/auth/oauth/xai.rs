//! Port of packages/ai/src/auth/oauth/xai.ts

use std::sync::Arc;

use serde_json::{Map, Value};

use pi_js::abort::AbortSignal;
use pi_js::fetch::{Body, Headers, Request, fetch};
use pi_js::{Error, Result};

use crate::auth::oauth::device_code::{OAuthDeviceCodePollResult, poll_oauth_device_code_flow};
use crate::auth::types::{AuthEvent, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction};

const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const XAI_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const XAI_DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const XAI_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const REFRESH_SKEW_MS: i64 = 5 * 60 * 1000;
const DEFAULT_TOKEN_LIFETIME_SECONDS: f64 = 3600.0;

struct OAuthHttpResponse {
    ok: bool,
    status: u16,
    body: Map<String, Value>,
}

struct XaiDeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    interval_seconds: Option<f64>,
    expires_in_seconds: f64,
}

fn required_string(body: &Map<String, Value>, field: &str) -> Result<String> {
    match body.get(field).and_then(Value::as_str) {
        Some(s) if !s.is_empty() => Ok(s.to_owned()),
        _ => Err(Error::msg(format!("Invalid xAI OAuth response field: {field}"))),
    }
}

fn positive_number(body: &Map<String, Value>, field: &str) -> Result<f64> {
    match body.get(field).and_then(Value::as_f64) {
        Some(n) if n.is_finite() && n > 0.0 => Ok(n),
        _ => Err(Error::msg(format!("Invalid xAI OAuth response field: {field}"))),
    }
}

fn validate_verification_uri(raw: &str) -> Result<String> {
    let url = url::Url::parse(raw).map_err(|_| Error::msg("Untrusted verification URI in xAI OAuth response"))?;
    if url.scheme() != "https" {
        return Err(Error::msg("Untrusted verification URI in xAI OAuth response"));
    }
    Ok(url.to_string())
}

async fn post_form(target_url: &str, fields: &[(&str, &str)], signal: &AbortSignal) -> Result<OAuthHttpResponse> {
    let body_str = {
        let mut ser = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in fields {
            ser.append_pair(k, v);
        }
        ser.finish()
    };

    let mut headers = Headers::new();
    headers.set("Accept", "application/json");
    headers.set("Content-Type", "application/x-www-form-urlencoded");

    let req = Request::post(target_url)
        .with_headers(headers)
        .body(Body::from(body_str))
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

    let ok = response.ok();
    let status = response.status();
    let body = match response.json().await {
        Ok(Value::Object(map)) => map,
        Ok(_) => Map::new(),
        Err(_) => {
            if signal.aborted() {
                return Err(Error::msg("Login cancelled"));
            }
            return Err(Error::msg(format!("xAI OAuth returned invalid JSON (HTTP {status})")));
        }
    };

    Ok(OAuthHttpResponse { ok, status, body })
}

fn request_failure(action: &str, response: &OAuthHttpResponse) -> Error {
    let error = response.body.get("error").and_then(Value::as_str);
    let description = response.body.get("error_description").and_then(Value::as_str);
    let mut parts = Vec::new();
    if let Some(e) = error.filter(|s| !s.is_empty()) {
        parts.push(e);
    }
    if let Some(d) = description.filter(|s| !s.is_empty()) {
        parts.push(d);
    }
    let detail = parts.join(": ");
    let suffix = if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    };
    Error::msg(format!("xAI OAuth {action} failed (HTTP {}){suffix}", response.status))
}

fn parse_device_code(body: &Map<String, Value>) -> Result<XaiDeviceCode> {
    let interval_seconds = body
        .get("interval")
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n > 0.0);

    let verification_uri_complete = match body.get("verification_uri_complete").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => Some(validate_verification_uri(s)?),
        _ => None,
    };

    Ok(XaiDeviceCode {
        device_code: required_string(body, "device_code")?,
        user_code: required_string(body, "user_code")?,
        verification_uri: validate_verification_uri(&required_string(body, "verification_uri")?)?,
        verification_uri_complete,
        interval_seconds,
        expires_in_seconds: positive_number(body, "expires_in")?,
    })
}

fn credentials_from_token_response(
    body: &Map<String, Value>,
    previous_refresh: Option<&str>,
) -> Result<OAuthCredential> {
    let access = required_string(body, "access_token")?;
    let refresh = if !body.contains_key("refresh_token") {
        match previous_refresh {
            Some(prev) => prev.to_owned(),
            None => required_string(body, "refresh_token")?,
        }
    } else {
        required_string(body, "refresh_token")?
    };
    let expires_in_seconds = if !body.contains_key("expires_in") {
        DEFAULT_TOKEN_LIFETIME_SECONDS
    } else {
        positive_number(body, "expires_in")?
    };
    let expires = pi_js::time::now_ms() + (expires_in_seconds * 1000.0) as i64 - REFRESH_SKEW_MS;
    Ok(OAuthCredential::new(refresh, access, expires))
}

async fn request_device_code(signal: &AbortSignal) -> Result<XaiDeviceCode> {
    let response = post_form(
        XAI_DEVICE_CODE_URL,
        &[("client_id", XAI_CLIENT_ID), ("scope", XAI_SCOPE), ("referrer", "pi")],
        signal,
    )
    .await?;
    if !response.ok {
        return Err(request_failure("device authorization", &response));
    }
    parse_device_code(&response.body)
}

async fn poll_for_tokens(device: &XaiDeviceCode, signal: &AbortSignal) -> Result<OAuthCredential> {
    poll_oauth_device_code_flow(
        device.interval_seconds,
        Some(device.expires_in_seconds),
        true,
        signal,
        || async {
            let response = post_form(
                XAI_TOKEN_URL,
                &[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("client_id", XAI_CLIENT_ID),
                    ("device_code", &device.device_code),
                ],
                signal,
            )
            .await?;

            if response.ok {
                return Ok(OAuthDeviceCodePollResult::Complete {
                    value: credentials_from_token_response(&response.body, None)?,
                });
            }

            let error = response.body.get("error").and_then(Value::as_str);
            match error {
                Some("authorization_pending") => Ok(OAuthDeviceCodePollResult::Pending),
                Some("slow_down") => Ok(OAuthDeviceCodePollResult::SlowDown {
                    interval_seconds: response.body.get("interval").and_then(Value::as_f64),
                }),
                Some("access_denied") | Some("authorization_denied") => Ok(OAuthDeviceCodePollResult::Failed {
                    message: "xAI device authorization was denied".to_owned(),
                }),
                Some("expired_token") => Ok(OAuthDeviceCodePollResult::Failed {
                    message: "xAI device code expired".to_owned(),
                }),
                _ => Ok(OAuthDeviceCodePollResult::Failed {
                    message: request_failure("device token polling", &response).to_string(),
                }),
            }
        },
    )
    .await
}

async fn login_xai(interaction: ProviderAuthInteraction) -> Result<OAuthCredential> {
    let device = request_device_code(&interaction.signal).await?;
    interaction.notify(AuthEvent::DeviceCode {
        user_code: device.user_code.clone(),
        verification_uri: device
            .verification_uri_complete
            .clone()
            .unwrap_or_else(|| device.verification_uri.clone()),
        interval_seconds: device.interval_seconds,
        expires_in_seconds: Some(device.expires_in_seconds),
    });
    poll_for_tokens(&device, &interaction.signal).await
}

async fn refresh_xai_token(refresh_token: &str, signal: &AbortSignal) -> Result<OAuthCredential> {
    let response = post_form(
        XAI_TOKEN_URL,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", XAI_CLIENT_ID),
            ("refresh_token", refresh_token),
        ],
        signal,
    )
    .await?;
    if !response.ok {
        return Err(request_failure("token refresh", &response));
    }
    credentials_from_token_response(&response.body, Some(refresh_token))
}

pub struct XaiOAuth;

#[async_trait::async_trait]
impl OAuthAuth for XaiOAuth {
    fn name(&self) -> &str {
        "xAI (Grok/X subscription)"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    fn login_label(&self) -> Option<&str> {
        Some("Sign in with SuperGrok or X Premium")
    }

    async fn login(
        &self,
        interaction: ProviderAuthInteraction,
        _options: Option<LoginOptions>,
    ) -> Result<OAuthCredential> {
        login_xai(interaction).await
    }

    async fn refresh(&self, credential: OAuthCredential, signal: AbortSignal) -> Result<OAuthCredential> {
        refresh_xai_token(&credential.refresh, &signal).await
    }

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access),
            headers: None,
            base_url: None,
        })
    }
}

pub fn xai_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(XaiOAuth)
}
