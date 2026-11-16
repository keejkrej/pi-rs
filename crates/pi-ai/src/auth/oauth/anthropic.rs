//! Port of packages/ai/src/auth/oauth/anthropic.ts

use std::sync::Arc;

use serde::Deserialize;

use pi_js::abort::AbortSignal;
use pi_js::fetch::{Body, Headers, Request, fetch};
use pi_js::{Error, Result};

use crate::auth::oauth::callback_server::{
    CallbackOrManualInput, OAuthCallbackServerOptions, start_oauth_callback_server, wait_for_callback_or_manual_input,
};
use crate::auth::oauth::pkce::generate_pkce;
use crate::auth::types::{
    AuthEvent, AuthPrompt, AuthSelectOption, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential,
    ProviderAuthInteraction,
};
use crate::utils::provider_env::get_provider_env_value;

const CLIENT_ID_B64: &str = "OWQxYzI1MGEtZTYxYi00NGQ5LTg4ZWQtNTk0NGQxOTYyZjVl";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CALLBACK_PORT: u16 = 53692;
const CALLBACK_PATH: &str = "/callback";
const REDIRECT_URI: &str = "http://localhost:53692/callback";
const COPY_CODE_REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
const ANTHROPIC_BROWSER_LOGIN_METHOD: &str = "browser";
const ANTHROPIC_COPY_CODE_LOGIN_METHOD: &str = "copy_code";
const SCOPES: &str =
    "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

fn client_id() -> Result<String> {
    pi_js::b64::atob(CLIENT_ID_B64)
}

fn callback_host() -> String {
    get_provider_env_value("PI_OAUTH_CALLBACK_HOST", None).unwrap_or_else(|| "127.0.0.1".to_owned())
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

async fn post_json(target_url: &str, body_json: String, signal: &AbortSignal) -> Result<String> {
    let mut headers = Headers::new();
    headers.set("Content-Type", "application/json");
    headers.set("Accept", "application/json");

    let req_signal = AbortSignal::any(&[signal.clone(), AbortSignal::timeout(30_000)]);
    let req = Request::post(target_url)
        .with_headers(headers)
        .body(Body::from(body_json))
        .signal(req_signal);
    let response = fetch(req).await?;

    let status = response.status();
    let ok = response.ok();
    let response_body = response.text().await?;
    if !ok {
        return Err(Error::msg(format!(
            "HTTP request failed. status={status}; url={target_url}; body={response_body}"
        )));
    }
    Ok(response_body)
}

#[derive(Deserialize)]
struct AnthropicTokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: f64,
}

async fn exchange_authorization_code(
    code: &str,
    state: &str,
    verifier: &str,
    redirect_uri: &str,
    signal: &AbortSignal,
) -> Result<OAuthCredential> {
    let payload = pi_js::json::stringify(&serde_json::json!({
        "grant_type": "authorization_code",
        "client_id": client_id()?,
        "code": code,
        "state": state,
        "redirect_uri": redirect_uri,
        "code_verifier": verifier,
    }));

    let response_body = post_json(TOKEN_URL, payload, signal).await.map_err(|err| {
        Error::msg(format!(
            "Token exchange request failed. url={TOKEN_URL}; redirect_uri={redirect_uri}; response_type=authorization_code; details={err}"
        ))
    })?;

    let token_data: AnthropicTokenResponse = pi_js::json::parse_as(&response_body).map_err(|err| {
        Error::msg(format!(
            "Token exchange returned invalid JSON. url={TOKEN_URL}; body={response_body}; details={err}"
        ))
    })?;

    let expires = pi_js::time::now_ms() + (token_data.expires_in * 1000.0) as i64 - 5 * 60 * 1000;
    Ok(OAuthCredential::new(
        token_data.refresh_token,
        token_data.access_token,
        expires,
    ))
}

fn build_authorize_url(challenge: &str, state: &str, redirect_uri: &str) -> Result<String> {
    let mut parsed = url::Url::parse(AUTHORIZE_URL).map_err(|e| Error::msg(e.to_string()))?;
    parsed
        .query_pairs_mut()
        .append_pair("code", "true")
        .append_pair("client_id", &client_id()?)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", SCOPES)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state);
    Ok(parsed.to_string())
}

async fn login_anthropic(interaction: &ProviderAuthInteraction) -> Result<OAuthCredential> {
    let pkce = generate_pkce().await?;
    let verifier = pkce.verifier;
    let challenge = pkce.challenge;

    let callback = start_oauth_callback_server(OAuthCallbackServerOptions {
        provider_name: "Anthropic".to_owned(),
        host: callback_host(),
        port: CALLBACK_PORT,
        path: CALLBACK_PATH.to_owned(),
        redirect_host: None,
        state: Some(verifier.clone()),
        complete: Box::new(|code| Box::pin(async move { Ok(code) })),
        signal: Some(interaction.signal.clone()),
        timeout_ms: None,
    })
    .await
    .ok();

    let result = async {
        let auth_url = build_authorize_url(&challenge, &verifier, REDIRECT_URI)?;
        interaction.notify(AuthEvent::AuthUrl {
            url: auth_url,
            instructions: Some(
                "Complete login in your browser. If the browser is on another machine, paste the final redirect URL here."
                    .to_owned(),
            ),
        });

        let wait_res = wait_for_callback_or_manual_input(
            interaction,
            callback.as_ref(),
            "Complete login in your browser, or paste the authorization code / redirect URL here:",
            REDIRECT_URI,
        )
        .await?;

        let (code, state) = match wait_res {
            CallbackOrManualInput::Callback(c) => (Some(c), verifier.clone()),
            CallbackOrManualInput::Manual(input) => {
                let (parsed_code, parsed_state) = parse_authorization_input(&input);
                if let Some(ref st) = parsed_state {
                    if st != &verifier {
                        return Err(Error::msg("OAuth state mismatch"));
                    }
                }
                (parsed_code, parsed_state.unwrap_or_else(|| verifier.clone()))
            }
        };

        let Some(code) = code.filter(|s| !s.is_empty()) else {
            return Err(Error::msg("Missing authorization code"));
        };
        interaction.notify(AuthEvent::Progress {
            message: "Exchanging authorization code for tokens...".to_owned(),
        });
        exchange_authorization_code(&code, &state, &verifier, REDIRECT_URI, &interaction.signal).await
    }
    .await;

    if let Some(cb) = callback.as_ref() {
        cb.close();
    }
    result
}

async fn login_anthropic_copy_code(interaction: &ProviderAuthInteraction) -> Result<OAuthCredential> {
    let pkce = generate_pkce().await?;
    let auth_url = build_authorize_url(&pkce.challenge, &pkce.verifier, COPY_CODE_REDIRECT_URI)?;
    interaction.notify(AuthEvent::AuthUrl {
        url: auth_url,
        instructions: Some(
            "Complete login in your browser, then copy the code Anthropic shows and paste it here.".to_owned(),
        ),
    });

    let input = interaction
        .prompt(
            AuthPrompt::manual_code(
                "Paste the code Anthropic shows after you sign in:",
                Some("code#state".to_owned()),
            )
            .with_signal(interaction.signal.clone()),
        )
        .await?;

    let (parsed_code, parsed_state) = parse_authorization_input(&input);
    if let Some(ref st) = parsed_state {
        if st != &pkce.verifier {
            return Err(Error::msg("OAuth state mismatch"));
        }
    }
    let Some(code) = parsed_code.filter(|s| !s.is_empty()) else {
        return Err(Error::msg("Missing authorization code"));
    };
    interaction.notify(AuthEvent::Progress {
        message: "Exchanging authorization code for tokens...".to_owned(),
    });
    let state = parsed_state.unwrap_or_else(|| pkce.verifier.clone());
    exchange_authorization_code(
        &code,
        &state,
        &pkce.verifier,
        COPY_CODE_REDIRECT_URI,
        &interaction.signal,
    )
    .await
}

async fn refresh_anthropic_token(refresh_token: &str, signal: &AbortSignal) -> Result<OAuthCredential> {
    let payload = pi_js::json::stringify(&serde_json::json!({
        "grant_type": "refresh_token",
        "client_id": client_id()?,
        "refresh_token": refresh_token,
    }));

    let response_body = post_json(TOKEN_URL, payload, signal).await.map_err(|err| {
        Error::msg(format!(
            "Anthropic token refresh request failed. url={TOKEN_URL}; details={err}"
        ))
    })?;

    let data: AnthropicTokenResponse = pi_js::json::parse_as(&response_body).map_err(|err| {
        Error::msg(format!(
            "Anthropic token refresh returned invalid JSON. url={TOKEN_URL}; body={response_body}; details={err}"
        ))
    })?;

    let expires = pi_js::time::now_ms() + (data.expires_in * 1000.0) as i64 - 5 * 60 * 1000;
    Ok(OAuthCredential::new(data.refresh_token, data.access_token, expires))
}

pub struct AnthropicOAuth;

#[async_trait::async_trait]
impl OAuthAuth for AnthropicOAuth {
    fn name(&self) -> &str {
        "Anthropic (Claude Pro/Max)"
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
                "Select Anthropic login method:",
                vec![
                    AuthSelectOption {
                        id: ANTHROPIC_BROWSER_LOGIN_METHOD.to_owned(),
                        label: "Browser login (default)".to_owned(),
                        description: None,
                    },
                    AuthSelectOption {
                        id: ANTHROPIC_COPY_CODE_LOGIN_METHOD.to_owned(),
                        label: "Copy code login (headless)".to_owned(),
                        description: None,
                    },
                ],
            ))
            .await?;

        if method == ANTHROPIC_COPY_CODE_LOGIN_METHOD {
            return login_anthropic_copy_code(&interaction).await;
        }
        if method != ANTHROPIC_BROWSER_LOGIN_METHOD {
            return Err(Error::msg(format!("Unknown Anthropic login method: {method}")));
        }
        login_anthropic(&interaction).await
    }

    async fn refresh(&self, credential: OAuthCredential, signal: AbortSignal) -> Result<OAuthCredential> {
        refresh_anthropic_token(&credential.refresh, &signal).await
    }

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access),
            headers: None,
            base_url: None,
        })
    }
}

pub fn anthropic_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(AnthropicOAuth)
}
