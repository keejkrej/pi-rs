//! Port of packages/ai/src/auth/oauth/openrouter.ts

use std::sync::Arc;

use serde_json::{Map, Value};

use pi_js::abort::AbortSignal;
use pi_js::fetch::{Body, Headers, Request, fetch};
use pi_js::{Error, Result};

use crate::auth::oauth::callback_server::{
    CallbackOrManualInput, OAuthCallbackServerOptions, start_oauth_callback_server, wait_for_callback_or_manual_input,
};
use crate::auth::oauth::pkce::generate_pkce;
use crate::auth::types::{AuthEvent, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction};
use crate::utils::provider_env::get_provider_env_value;

const AUTHORIZE_URL: &str = "https://openrouter.ai/auth";
const TOKEN_URL: &str = "https://openrouter.ai/api/v1/auth/keys";
const LOGIN_TIMEOUT_MS: u64 = 5 * 60 * 1000;
const TOKEN_EXCHANGE_TIMEOUT_MS: u64 = 30_000;
const JS_MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

fn callback_host() -> String {
    get_provider_env_value("PI_OAUTH_CALLBACK_HOST", None).unwrap_or_else(|| "127.0.0.1".to_owned())
}

fn parse_authorization_input(input: &str) -> Option<String> {
    let value = input.trim();
    if value.is_empty() {
        return None;
    }

    if let Ok(parsed) = url::Url::parse(value) {
        return parsed
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.into_owned());
    }

    if value.contains("code=") {
        return url::form_urlencoded::parse(value.as_bytes())
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.into_owned());
    }

    Some(value.to_owned())
}

fn error_detail(body: &Map<String, Value>) -> Option<String> {
    if let Some(s) = body.get("error_description").and_then(Value::as_str) {
        return Some(s.to_owned());
    }
    if let Some(s) = body.get("message").and_then(Value::as_str) {
        return Some(s.to_owned());
    }
    if let Some(s) = body.get("error").and_then(Value::as_str) {
        return Some(s.to_owned());
    }
    if let Some(msg) = body
        .get("error")
        .and_then(Value::as_object)
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
    {
        return Some(msg.to_owned());
    }
    None
}

async fn exchange_authorization_code(code: &str, verifier: &str, signal: &AbortSignal) -> Result<OAuthCredential> {
    if signal.aborted() {
        return Err(Error::msg("Login cancelled"));
    }

    let timeout_sig = AbortSignal::timeout(TOKEN_EXCHANGE_TIMEOUT_MS);
    let combined = AbortSignal::any(&[signal.clone(), timeout_sig.clone()]);

    let mut headers = Headers::new();
    headers.set("accept", "application/json");
    headers.set("content-type", "application/json");

    let payload = pi_js::json::stringify(&serde_json::json!({
        "code": code,
        "code_verifier": verifier,
        "code_challenge_method": "S256",
    }));

    let req = Request::post(TOKEN_URL)
        .with_headers(headers)
        .body(Body::from(payload))
        .signal(combined);

    let response = match fetch(req).await {
        Ok(r) => r,
        Err(err) => {
            if signal.aborted() {
                return Err(Error::msg("Login cancelled"));
            }
            if timeout_sig.aborted() {
                return Err(Error::msg("OpenRouter OAuth token exchange timed out"));
            }
            return Err(err);
        }
    };

    let status = response.status();
    let ok = response.ok();
    let parsed_json: std::result::Result<Value, Error> = response.json().await;
    let body: Map<String, Value> = match parsed_json {
        Ok(Value::Object(map)) => map,
        Ok(_) => Map::new(),
        Err(err) => {
            if signal.aborted() {
                return Err(Error::msg("Login cancelled"));
            }
            if timeout_sig.aborted() {
                return Err(Error::msg("OpenRouter OAuth token exchange timed out"));
            }
            if ok {
                return Err(Error::msg("OpenRouter OAuth returned invalid JSON"));
            }
            let _ = err;
            Map::new()
        }
    };

    if !ok {
        let detail = error_detail(&body);
        let suffix = detail.map(|d| format!(": {d}")).unwrap_or_default();
        return Err(Error::msg(format!(
            "OpenRouter OAuth key exchange failed (HTTP {status}){suffix}"
        )));
    }

    let Some(key) = body.get("key").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
        return Err(Error::msg("OpenRouter OAuth response carries no \"key\""));
    };

    Ok(OAuthCredential::new("", key, JS_MAX_SAFE_INTEGER))
}

async fn login_openrouter(interaction: ProviderAuthInteraction) -> Result<OAuthCredential> {
    let pkce = generate_pkce().await?;
    let verifier = pkce.verifier;
    let challenge = pkce.challenge;

    let verifier_for_cb = verifier.clone();
    let signal_for_cb = interaction.signal.clone();

    let callback = start_oauth_callback_server(OAuthCallbackServerOptions {
        provider_name: "OpenRouter".to_owned(),
        host: callback_host(),
        port: 0,
        path: format!("/oauth/callback/{}", pi_js::crypto::random_uuid()),
        redirect_host: None,
        state: None,
        complete: Box::new(move |code| {
            Box::pin(async move { exchange_authorization_code(&code, &verifier_for_cb, &signal_for_cb).await })
        }),
        signal: Some(interaction.signal.clone()),
        timeout_ms: Some(LOGIN_TIMEOUT_MS),
    })
    .await?;

    let result = async {
        let mut authorize_url = url::Url::parse(AUTHORIZE_URL).map_err(|e| Error::msg(e.to_string()))?;
        authorize_url
            .query_pairs_mut()
            .append_pair("callback_url", &callback.redirect_uri)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");

        interaction.notify(AuthEvent::Progress {
            message: format!("Listening for OpenRouter OAuth callback on {}", callback.redirect_uri),
        });
        interaction.notify(AuthEvent::AuthUrl {
            url: authorize_url.to_string(),
            instructions: Some(
                "Complete sign-in in your browser. If the browser is on another machine, paste the final redirect URL here."
                    .to_owned(),
            ),
        });

        let wait_res = wait_for_callback_or_manual_input(
            &interaction,
            Some(&callback),
            "Complete sign-in in your browser, or paste the authorization code / redirect URL here:",
            &callback.redirect_uri,
        )
        .await?;

        match wait_res {
            CallbackOrManualInput::Callback(cred) => Ok(cred),
            CallbackOrManualInput::Manual(input) => {
                let Some(code) = parse_authorization_input(&input).filter(|s| !s.is_empty()) else {
                    return Err(Error::msg("Missing authorization code"));
                };
                interaction.notify(AuthEvent::Progress {
                    message: "Exchanging authorization code for an API key...".to_owned(),
                });
                exchange_authorization_code(&code, &verifier, &interaction.signal).await
            }
        }
    }
    .await;

    callback.close();
    result
}

pub struct OpenRouterOAuth;

#[async_trait::async_trait]
impl OAuthAuth for OpenRouterOAuth {
    fn name(&self) -> &str {
        "OpenRouter OAuth"
    }

    fn login_label(&self) -> Option<&str> {
        Some("Sign in with OpenRouter")
    }

    async fn login(
        &self,
        interaction: ProviderAuthInteraction,
        _options: Option<LoginOptions>,
    ) -> Result<OAuthCredential> {
        login_openrouter(interaction).await
    }

    async fn refresh(&self, credential: OAuthCredential, _signal: AbortSignal) -> Result<OAuthCredential> {
        Ok(credential)
    }

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access),
            headers: None,
            base_url: None,
        })
    }
}

pub fn openrouter_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(OpenRouterOAuth)
}
