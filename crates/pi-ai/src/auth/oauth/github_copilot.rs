//! Port of packages/ai/src/auth/oauth/github-copilot.ts

use std::sync::{Arc, OnceLock};

use indexmap::IndexSet;
use serde_json::Value;

use pi_js::abort::AbortSignal;
use pi_js::fetch::{Body, Headers, Request, Response, fetch};
use pi_js::{Error, Result};

use crate::auth::oauth::device_code::{OAuthDeviceCodePollResult, poll_oauth_device_code_flow};
use crate::auth::types::{
    AuthEvent, AuthPrompt, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction,
};

const CLIENT_ID_B64: &str = "SXYxLmI1MDdhMDhjODdlY2ZlOTg=";
const COPILOT_HEADERS: &[(&str, &str)] = &[
    ("User-Agent", "GitHubCopilotChat/0.35.0"),
    ("Editor-Version", "vscode/1.107.0"),
    ("Editor-Plugin-Version", "copilot-chat/0.35.0"),
    ("Copilot-Integration-Id", "vscode-chat"),
];
const COPILOT_API_VERSION: &str = "2026-06-01";
const DEFAULT_COPILOT_API_BASE: &str = "https://api.individual.githubcopilot.com";

fn client_id() -> Result<String> {
    pi_js::b64::atob(CLIENT_ID_B64)
}

fn known_github_copilot_model_ids() -> &'static IndexSet<String> {
    static IDS: OnceLock<IndexSet<String>> = OnceLock::new();
    IDS.get_or_init(|| {
        crate::providers::github_copilot_models::github_copilot_models()
            .keys()
            .cloned()
            .collect()
    })
}

#[derive(Clone, Debug)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: Option<f64>,
    expires_in: f64,
}

#[derive(Clone, Copy)]
struct RetryPolicy {
    max_retries: u32,
    max_elapsed_ms: u64,
}

pub fn normalize_github_copilot_domain(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    let parsed = url::Url::parse(&candidate).ok()?;
    parsed.host_str().map(str::to_owned)
}

fn get_urls(domain: &str) -> (String, String, String) {
    (
        format!("https://{domain}/login/device/code"),
        format!("https://{domain}/login/oauth/access_token"),
        format!("https://api.{domain}/copilot_internal/v2/token"),
    )
}

pub fn get_base_url_from_github_copilot_token(token: &str) -> Option<String> {
    let (_, after) = token.split_once("proxy-ep=")?;
    let proxy_host = after.split(';').next().unwrap_or("");
    if proxy_host.is_empty() {
        return None;
    }
    let api_host = if let Some(rest) = proxy_host.strip_prefix("proxy.") {
        format!("api.{rest}")
    } else {
        proxy_host.to_owned()
    };
    Some(format!("https://{api_host}"))
}

pub fn get_github_copilot_base_url(token: Option<&str>, enterprise_domain: Option<&str>) -> String {
    if let Some(tok) = token {
        if let Some(url) = get_base_url_from_github_copilot_token(tok) {
            return url;
        }
    }
    if let Some(domain) = enterprise_domain.filter(|s| !s.is_empty()) {
        return format!("https://copilot-api.{domain}");
    }
    DEFAULT_COPILOT_API_BASE.to_owned()
}

struct ParsedCopilotCatalog {
    available_model_ids: Vec<String>,
    policy_model_ids: Vec<String>,
}

fn parse_github_copilot_model_catalog(raw: &Value, allow_policy_fallback: bool) -> Result<ParsedCopilotCatalog> {
    let Some(data) = raw.get("data").and_then(Value::as_array) else {
        return Err(Error::msg("Invalid Copilot models response"));
    };

    struct AccountModel {
        id: String,
        picker_enabled: bool,
        policy_state: Option<String>,
    }

    let mut account_models = Vec::new();
    for raw_item in data {
        let Some(item) = raw_item.as_object() else {
            continue;
        };
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        let supports_tool_calls = item
            .get("capabilities")
            .and_then(|c| c.get("supports"))
            .and_then(|s| s.get("tool_calls"))
            .and_then(Value::as_bool);
        if supports_tool_calls == Some(false) {
            continue;
        }
        account_models.push(AccountModel {
            id: id.to_owned(),
            picker_enabled: item.get("model_picker_enabled").and_then(Value::as_bool) == Some(true),
            policy_state: item
                .get("policy")
                .and_then(|p| p.get("state"))
                .and_then(Value::as_str)
                .map(str::to_owned),
        });
    }

    let picker_model_ids: Vec<String> = account_models
        .iter()
        .filter(|m| m.picker_enabled && m.policy_state.as_deref() != Some("disabled"))
        .map(|m| m.id.clone())
        .collect();

    let use_policy_fallback = allow_policy_fallback && picker_model_ids.is_empty();
    let available_model_ids = if !picker_model_ids.is_empty() || !allow_policy_fallback {
        picker_model_ids
    } else {
        account_models
            .iter()
            .filter(|m| m.policy_state.as_deref() == Some("enabled"))
            .map(|m| m.id.clone())
            .collect()
    };

    let known_ids = known_github_copilot_model_ids();
    let policy_model_ids = account_models
        .iter()
        .filter(|m| {
            m.policy_state.as_deref() == Some("unconfigured")
                && known_ids.contains(&m.id)
                && (m.picker_enabled || use_policy_fallback)
        })
        .map(|m| m.id.clone())
        .collect();

    Ok(ParsedCopilotCatalog {
        available_model_ids,
        policy_model_ids,
    })
}

async fn fetch_with_rate_limit_retry<F>(
    build_req: F,
    signal: &AbortSignal,
    retry_policy: RetryPolicy,
) -> Result<Response>
where
    F: Fn() -> Request,
{
    let retry_budget_signal = if retry_policy.max_retries > 0 && retry_policy.max_elapsed_ms > 0 {
        Some(AbortSignal::timeout(retry_policy.max_elapsed_ms))
    } else {
        None
    };
    let request_signal = match retry_budget_signal.as_ref() {
        Some(budget) => AbortSignal::any(&[signal.clone(), budget.clone()]),
        None => signal.clone(),
    };
    let retry_deadline_ms = retry_budget_signal
        .as_ref()
        .map(|_| (pi_js::time::now_ms() as f64) + retry_policy.max_elapsed_ms as f64);

    let mut retry = 0u32;
    loop {
        let attempt_signal = AbortSignal::any(&[request_signal.clone(), AbortSignal::timeout(5000)]);
        let req = build_req().signal(attempt_signal);
        let response = fetch(req).await?;
        if response.status() != 429 || retry == retry_policy.max_retries {
            return Ok(response);
        }

        let mut delay_ms = 500.0 * 2_f64.powi(retry as i32);
        if let Some(retry_after) = response.headers().get("retry-after") {
            let parsed_delay = if let Ok(seconds) = retry_after.trim().parse::<f64>() {
                seconds * 1000.0
            } else if let Some(target_ms) = pi_js::time::parse_date(&retry_after) {
                (target_ms as f64) - (pi_js::time::now_ms() as f64)
            } else {
                f64::NAN
            };
            if !parsed_delay.is_finite() {
                return Ok(response);
            }
            delay_ms = parsed_delay;
        }
        delay_ms = delay_ms.max(0.0);
        if let Some(deadline) = retry_deadline_ms {
            if delay_ms >= deadline - (pi_js::time::now_ms() as f64) {
                return Ok(response);
            }
        }
        drop(response);
        crate::utils::sleep::sleep(delay_ms as u64, &request_signal).await?;
        retry += 1;
    }
}

async fn fetch_github_copilot_models(
    copilot_token: &str,
    enterprise_domain: Option<&str>,
    signal: &AbortSignal,
    retry_policy: RetryPolicy,
) -> Result<ParsedCopilotCatalog> {
    let base_url = get_github_copilot_base_url(Some(copilot_token), enterprise_domain);
    let allow_policy_fallback = base_url == DEFAULT_COPILOT_API_BASE;
    let url = format!("{base_url}/models");
    let bearer = format!("Bearer {copilot_token}");

    let response = fetch_with_rate_limit_retry(
        || {
            let mut headers = Headers::new();
            headers.set("Accept", "application/json");
            headers.set("Authorization", &bearer);
            for (k, v) in COPILOT_HEADERS {
                headers.set(k, v);
            }
            headers.set("X-GitHub-Api-Version", COPILOT_API_VERSION);
            Request::get(&url).with_headers(headers)
        },
        signal,
        retry_policy,
    )
    .await?;

    if !response.ok() {
        let status = response.status();
        let status_text = response.status_text().to_owned();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::msg(format!("{status} {status_text}: {text}")));
    }

    let raw: Value = response.json().await?;
    parse_github_copilot_model_catalog(&raw, allow_policy_fallback)
}

async fn fetch_json(req: Request) -> Result<Value> {
    let response = fetch(req).await?;
    if !response.ok() {
        let status = response.status();
        let status_text = response.status_text().to_owned();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::msg(format!("{status} {status_text}: {text}")));
    }
    response.json().await
}

async fn start_device_flow(domain: &str, signal: &AbortSignal) -> Result<DeviceCodeResponse> {
    let (device_code_url, _, _) = get_urls(domain);
    let cid = client_id()?;
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &cid)
        .append_pair("scope", "read:user")
        .finish();

    let mut headers = Headers::new();
    headers.set("Accept", "application/json");
    headers.set("Content-Type", "application/x-www-form-urlencoded");
    headers.set("User-Agent", "GitHubCopilotChat/0.35.0");

    let req = Request::post(&device_code_url)
        .with_headers(headers)
        .body(Body::from(body))
        .signal(signal.clone());
    let data = fetch_json(req).await?;
    let Some(obj) = data.as_object() else {
        return Err(Error::msg("Invalid device code response"));
    };

    let device_code = obj.get("device_code").and_then(Value::as_str);
    let user_code = obj.get("user_code").and_then(Value::as_str);
    let verification_uri = obj.get("verification_uri").and_then(Value::as_str);
    let interval_raw = obj.get("interval");
    let interval = match interval_raw {
        None => None,
        Some(v) => Some(
            v.as_f64()
                .ok_or_else(|| Error::msg("Invalid device code response fields"))?,
        ),
    };
    let expires_in = obj.get("expires_in").and_then(Value::as_f64);

    let (Some(device_code), Some(user_code), Some(verification_uri), Some(expires_in)) =
        (device_code, user_code, verification_uri, expires_in)
    else {
        return Err(Error::msg("Invalid device code response fields"));
    };

    let parsed_uri = url::Url::parse(verification_uri)
        .map_err(|_| Error::msg("Untrusted verification_uri in device code response"))?;
    if parsed_uri.scheme() != "https" && parsed_uri.scheme() != "http" {
        return Err(Error::msg("Untrusted verification_uri in device code response"));
    }

    Ok(DeviceCodeResponse {
        device_code: device_code.to_owned(),
        user_code: user_code.to_owned(),
        verification_uri: parsed_uri.to_string(),
        interval,
        expires_in,
    })
}

async fn poll_for_github_access_token(
    domain: &str,
    device: &DeviceCodeResponse,
    signal: &AbortSignal,
) -> Result<String> {
    let (_, access_token_url, _) = get_urls(domain);
    let cid = client_id()?;

    poll_oauth_device_code_flow(device.interval, Some(device.expires_in), true, signal, || async {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("client_id", &cid)
            .append_pair("device_code", &device.device_code)
            .append_pair("grant_type", "urn:ietf:params:oauth:grant-type:device_code")
            .finish();

        let mut headers = Headers::new();
        headers.set("Accept", "application/json");
        headers.set("Content-Type", "application/x-www-form-urlencoded");
        headers.set("User-Agent", "GitHubCopilotChat/0.35.0");

        let req = Request::post(&access_token_url)
            .with_headers(headers)
            .body(Body::from(body))
            .signal(signal.clone());
        let raw = fetch_json(req).await?;

        if let Some(access_token) = raw.get("access_token").and_then(Value::as_str) {
            return Ok(OAuthDeviceCodePollResult::Complete {
                value: access_token.to_owned(),
            });
        }

        if let Some(error) = raw.get("error").and_then(Value::as_str) {
            if error == "authorization_pending" {
                return Ok(OAuthDeviceCodePollResult::Pending);
            }
            if error == "slow_down" {
                let interval_seconds = raw.get("interval").and_then(Value::as_f64);
                return Ok(OAuthDeviceCodePollResult::SlowDown { interval_seconds });
            }
            let description = raw.get("error_description").and_then(Value::as_str);
            let suffix = description.map(|d| format!(": {d}")).unwrap_or_default();
            return Ok(OAuthDeviceCodePollResult::Failed {
                message: format!("Device flow failed: {error}{suffix}"),
            });
        }

        Ok(OAuthDeviceCodePollResult::Failed {
            message: "Invalid device token response".to_owned(),
        })
    })
    .await
}

async fn refresh_github_copilot_access_token(
    refresh_token: &str,
    enterprise_domain: Option<&str>,
    signal: &AbortSignal,
) -> Result<OAuthCredential> {
    let domain = enterprise_domain.filter(|s| !s.is_empty()).unwrap_or("github.com");
    let (_, _, copilot_token_url) = get_urls(domain);

    let mut headers = Headers::new();
    headers.set("Accept", "application/json");
    headers.set("Authorization", &format!("Bearer {refresh_token}"));
    for (k, v) in COPILOT_HEADERS {
        headers.set(k, v);
    }

    let req = Request::get(&copilot_token_url)
        .with_headers(headers)
        .signal(signal.clone());
    let raw = fetch_json(req).await?;
    let Some(obj) = raw.as_object() else {
        return Err(Error::msg("Invalid Copilot token response"));
    };

    let Some(token) = obj.get("token").and_then(Value::as_str) else {
        return Err(Error::msg("Invalid Copilot token response fields"));
    };
    let Some(expires_at) = obj.get("expires_at").and_then(Value::as_f64) else {
        return Err(Error::msg("Invalid Copilot token response fields"));
    };

    let expires = (expires_at * 1000.0) as i64 - 5 * 60 * 1000;
    let mut cred = OAuthCredential::new(refresh_token, token, expires);
    if let Some(ent) = enterprise_domain {
        cred.set_extra("enterpriseUrl", Value::String(ent.to_owned()));
    }
    Ok(cred)
}

async fn refresh_github_copilot_token(
    refresh_token: &str,
    enterprise_domain: Option<&str>,
    signal: &AbortSignal,
) -> Result<OAuthCredential> {
    let mut cred = refresh_github_copilot_access_token(refresh_token, enterprise_domain, signal).await?;
    let catalog = fetch_github_copilot_models(
        &cred.access,
        enterprise_domain,
        signal,
        RetryPolicy {
            max_retries: 0,
            max_elapsed_ms: 0,
        },
    )
    .await?;
    cred.set_extra(
        "availableModelIds",
        Value::Array(catalog.available_model_ids.into_iter().map(Value::String).collect()),
    );
    Ok(cred)
}

async fn enable_github_copilot_model(
    token: &str,
    model_id: &str,
    enterprise_domain: Option<&str>,
    signal: &AbortSignal,
) -> Result<bool> {
    let base_url = get_github_copilot_base_url(Some(token), enterprise_domain);
    let url = format!("{base_url}/models/{model_id}/policy");
    let bearer = format!("Bearer {token}");
    let body = pi_js::json::stringify(&serde_json::json!({ "state": "enabled" }));

    let response = match fetch_with_rate_limit_retry(
        || {
            let mut headers = Headers::new();
            headers.set("Content-Type", "application/json");
            headers.set("Authorization", &bearer);
            for (k, v) in COPILOT_HEADERS {
                headers.set(k, v);
            }
            headers.set("openai-intent", "chat-policy");
            headers.set("x-interaction-type", "chat-policy");
            Request::post(&url).with_headers(headers).body(Body::from(body.clone()))
        },
        signal,
        RetryPolicy {
            max_retries: 2,
            max_elapsed_ms: 5000,
        },
    )
    .await
    {
        Ok(resp) => resp,
        Err(err) => {
            if signal.aborted() {
                return Err(err);
            }
            return Ok(false);
        }
    };

    if response.status() == 429 {
        let status = response.status();
        let status_text = response.status_text().to_owned();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::msg(format!("{status} {status_text}: {text}")));
    }
    Ok(response.ok())
}

async fn enable_github_copilot_models(
    token: &str,
    model_ids: &[String],
    enterprise_domain: Option<&str>,
    signal: &AbortSignal,
) -> Result<Vec<String>> {
    let mut enabled = Vec::new();
    for model_id in model_ids {
        match enable_github_copilot_model(token, model_id, enterprise_domain, signal).await {
            Ok(true) => enabled.push(model_id.clone()),
            Ok(false) => {}
            Err(err) => {
                if signal.aborted() {
                    return Err(err);
                }
                break;
            }
        }
    }
    Ok(enabled)
}

async fn login_github_copilot(interaction: ProviderAuthInteraction) -> Result<OAuthCredential> {
    let input = interaction
        .prompt(AuthPrompt::text(
            "GitHub Enterprise URL/domain (blank for github.com)",
            Some("company.ghe.com".to_owned()),
        ))
        .await?;
    if interaction.signal.aborted() {
        return Err(Error::msg("Login cancelled"));
    }

    let trimmed = input.trim();
    let enterprise_domain = normalize_github_copilot_domain(&input);
    if !trimmed.is_empty() && enterprise_domain.is_none() {
        return Err(Error::msg("Invalid GitHub Enterprise URL/domain"));
    }
    let domain = enterprise_domain.as_deref().unwrap_or("github.com");

    let device = start_device_flow(domain, &interaction.signal).await?;
    interaction.notify(AuthEvent::DeviceCode {
        user_code: device.user_code.clone(),
        verification_uri: device.verification_uri.clone(),
        interval_seconds: device.interval,
        expires_in_seconds: Some(device.expires_in),
    });

    let github_access_token = poll_for_github_access_token(domain, &device, &interaction.signal).await?;
    let mut cred =
        refresh_github_copilot_access_token(&github_access_token, enterprise_domain.as_deref(), &interaction.signal)
            .await?;

    let models = fetch_github_copilot_models(
        &cred.access,
        enterprise_domain.as_deref(),
        &interaction.signal,
        RetryPolicy {
            max_retries: 2,
            max_elapsed_ms: 5000,
        },
    )
    .await?;

    let mut enabled_model_ids = Vec::new();
    if !models.policy_model_ids.is_empty() {
        interaction.notify(AuthEvent::Progress {
            message: "Enabling models...".to_owned(),
        });
        enabled_model_ids = enable_github_copilot_models(
            &cred.access,
            &models.policy_model_ids,
            enterprise_domain.as_deref(),
            &interaction.signal,
        )
        .await?;
    }

    let mut merged: IndexSet<String> = IndexSet::new();
    for id in models.available_model_ids {
        merged.insert(id);
    }
    for id in enabled_model_ids {
        merged.insert(id);
    }
    cred.set_extra(
        "availableModelIds",
        Value::Array(merged.into_iter().map(Value::String).collect()),
    );
    Ok(cred)
}

fn copilot_enterprise_domain(credential: &OAuthCredential) -> Option<String> {
    let raw = credential.get_extra_str("enterpriseUrl")?;
    if raw.is_empty() {
        return None;
    }
    normalize_github_copilot_domain(raw)
}

pub struct GitHubCopilotOAuth;

#[async_trait::async_trait]
impl OAuthAuth for GitHubCopilotOAuth {
    fn name(&self) -> &str {
        "GitHub Copilot"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    async fn login(
        &self,
        interaction: ProviderAuthInteraction,
        _options: Option<LoginOptions>,
    ) -> Result<OAuthCredential> {
        login_github_copilot(interaction).await
    }

    async fn refresh(&self, credential: OAuthCredential, signal: AbortSignal) -> Result<OAuthCredential> {
        let domain = copilot_enterprise_domain(&credential);
        refresh_github_copilot_token(&credential.refresh, domain.as_deref(), &signal).await
    }

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth> {
        let domain = copilot_enterprise_domain(&credential);
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            headers: None,
            base_url: Some(get_github_copilot_base_url(Some(&credential.access), domain.as_deref())),
        })
    }
}

pub fn github_copilot_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(GitHubCopilotOAuth)
}
