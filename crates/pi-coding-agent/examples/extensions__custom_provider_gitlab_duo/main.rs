//! Port of packages/coding-agent/examples/extensions/custom-provider-gitlab-duo/index.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use indexmap::IndexMap;
use pi_ai::api::{anthropic_messages, openai_responses};
use pi_ai::types::{
    AnthropicMessagesCompat, AssistantMessage, AssistantMessageEvent, Model, ModelCompat, ModelCost, ModelInput,
    ModelThinkingLevel, SimpleStreamOptions, StopReason, ThinkingLevelMap, TranscriptContext, Usage,
};
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use pi_ai::{OAuthAuthInfo, OAuthCredentials, OAuthLoginCallbacks, OAuthPrompt};
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, ProviderChatModelConfig, ProviderConfig,
    ProviderModelConfig, ProviderOAuthConfig,
};
use pi_js::abort::AbortSignal;
use pi_js::b64::encode_url;
use pi_js::crypto::{random_bytes, random_uuid, sha256};
use pi_js::fetch::{Body, Headers, Request, fetch};
use pi_js::time::now_ms;
use serde::Deserialize;

const GITLAB_COM_URL: &str = "https://gitlab.com";
const AI_GATEWAY_URL: &str = "https://cloud.gitlab.com";
const ANTHROPIC_PROXY_URL: &str = "https://cloud.gitlab.com/ai/v1/proxy/anthropic/";
const OPENAI_PROXY_URL: &str = "https://cloud.gitlab.com/ai/v1/proxy/openai/v1";

const BUNDLED_CLIENT_ID: &str = "da4edff2e6ebd2bc3208611e2768bc1c1dd7be791dc5ff26ca34ca9ee44f7d4b";
const OAUTH_SCOPES: &[&str] = &["api"];
const REDIRECT_URI: &str = "http://127.0.0.1:8080/callback";
const DIRECT_ACCESS_TTL: i64 = 25 * 60 * 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Anthropic,
    OpenAI,
}

#[derive(Clone, Debug)]
pub struct GitLabModel {
    pub id: &'static str,
    pub name: &'static str,
    pub backend: Backend,
    pub base_url: &'static str,
    pub reasoning: bool,
    pub xhigh_mapped: bool,
    pub cost: ModelCost,
    pub context_window: i64,
    pub max_tokens: i64,
}

pub fn models() -> Vec<GitLabModel> {
    vec![
        GitLabModel {
            id: "claude-opus-4-8",
            name: "Claude Opus 4.8",
            backend: Backend::Anthropic,
            base_url: ANTHROPIC_PROXY_URL,
            reasoning: true,
            xhigh_mapped: true,
            cost: ModelCost {
                input: 5.0,
                output: 25.0,
                cache_read: 0.5,
                cache_write: 6.25,
                tiers: None,
            },
            context_window: 1_000_000,
            max_tokens: 128_000,
        },
        GitLabModel {
            id: "claude-sonnet-4-6",
            name: "Claude Sonnet 4.6",
            backend: Backend::Anthropic,
            base_url: ANTHROPIC_PROXY_URL,
            reasoning: true,
            xhigh_mapped: true,
            cost: ModelCost {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 3.75,
                tiers: None,
            },
            context_window: 1_000_000,
            max_tokens: 64_000,
        },
        GitLabModel {
            id: "claude-opus-4-5-20251101",
            name: "Claude Opus 4.5",
            backend: Backend::Anthropic,
            base_url: ANTHROPIC_PROXY_URL,
            reasoning: true,
            xhigh_mapped: true,
            cost: ModelCost {
                input: 15.0,
                output: 75.0,
                cache_read: 1.5,
                cache_write: 18.75,
                tiers: None,
            },
            context_window: 200_000,
            max_tokens: 32_000,
        },
        GitLabModel {
            id: "claude-sonnet-4-5-20250929",
            name: "Claude Sonnet 4.5",
            backend: Backend::Anthropic,
            base_url: ANTHROPIC_PROXY_URL,
            reasoning: true,
            xhigh_mapped: true,
            cost: ModelCost {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 3.75,
                tiers: None,
            },
            context_window: 200_000,
            max_tokens: 16_384,
        },
        GitLabModel {
            id: "claude-haiku-4-5-20251001",
            name: "Claude Haiku 4.5",
            backend: Backend::Anthropic,
            base_url: ANTHROPIC_PROXY_URL,
            reasoning: true,
            xhigh_mapped: true,
            cost: ModelCost {
                input: 1.0,
                output: 5.0,
                cache_read: 0.1,
                cache_write: 1.25,
                tiers: None,
            },
            context_window: 200_000,
            max_tokens: 8_192,
        },
        GitLabModel {
            id: "gpt-5.5-2026-04-23",
            name: "GPT-5.5",
            backend: Backend::OpenAI,
            base_url: OPENAI_PROXY_URL,
            reasoning: true,
            xhigh_mapped: false,
            cost: ModelCost {
                input: 5.0,
                output: 30.0,
                cache_read: 0.5,
                cache_write: 0.0,
                tiers: None,
            },
            context_window: 272_000,
            max_tokens: 128_000,
        },
        GitLabModel {
            id: "gpt-5.1-2025-11-13",
            name: "GPT-5.1",
            backend: Backend::OpenAI,
            base_url: OPENAI_PROXY_URL,
            reasoning: true,
            xhigh_mapped: false,
            cost: ModelCost {
                input: 2.5,
                output: 10.0,
                cache_read: 0.0,
                cache_write: 0.0,
                tiers: None,
            },
            context_window: 128_000,
            max_tokens: 16_384,
        },
        GitLabModel {
            id: "gpt-5-mini-2025-08-07",
            name: "GPT-5 Mini",
            backend: Backend::OpenAI,
            base_url: OPENAI_PROXY_URL,
            reasoning: true,
            xhigh_mapped: false,
            cost: ModelCost {
                input: 0.15,
                output: 0.6,
                cache_read: 0.0,
                cache_write: 0.0,
                tiers: None,
            },
            context_window: 128_000,
            max_tokens: 16_384,
        },
        GitLabModel {
            id: "gpt-5-codex",
            name: "GPT-5 Codex",
            backend: Backend::OpenAI,
            base_url: OPENAI_PROXY_URL,
            reasoning: true,
            xhigh_mapped: false,
            cost: ModelCost {
                input: 2.5,
                output: 10.0,
                cache_read: 0.0,
                cache_write: 0.0,
                tiers: None,
            },
            context_window: 128_000,
            max_tokens: 16_384,
        },
    ]
}

#[derive(Clone, Debug)]
struct DirectAccessToken {
    token: String,
    headers: IndexMap<String, String>,
    expires_at: i64,
}

#[derive(Debug, Deserialize)]
struct DirectAccessResponse {
    token: String,
    headers: IndexMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
    created_at: i64,
}

static DIRECT_ACCESS_CACHE: Mutex<Option<DirectAccessToken>> = Mutex::new(None);

async fn get_direct_access_token(gitlab_access_token: &str) -> pi_js::Result<DirectAccessToken> {
    let now = now_ms();
    {
        let guard = DIRECT_ACCESS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = guard.as_ref()
            && cached.expires_at > now
        {
            return Ok(cached.clone());
        }
    }

    let mut headers = Headers::new();
    headers.set("Authorization", &format!("Bearer {gitlab_access_token}"));
    headers.set("Content-Type", "application/json");
    let body = pi_js::json::try_stringify(&serde_json::json!({
        "feature_flags": { "DuoAgentPlatformNext": true }
    }))?;
    let response = fetch(Request {
        method: "POST".to_string(),
        url: format!("{GITLAB_COM_URL}/api/v4/ai/third_party_agents/direct_access"),
        headers,
        body: Some(Body::Text(body)),
        signal: None,
    })
    .await?;

    if !response.ok() {
        let status = response.status();
        let error_text = response.text().await?;
        if status == 403 {
            return Err(pi_js::Error::msg(format!(
                "GitLab Duo access denied. Ensure GitLab Duo is enabled for your account. Error: {error_text}"
            )));
        }
        return Err(pi_js::Error::msg(format!(
            "Failed to get direct access token: {status} {error_text}"
        )));
    }

    let text = response.text().await?;
    let data: DirectAccessResponse = pi_js::json::parse_as(&text)?;
    let entry = DirectAccessToken {
        token: data.token,
        headers: data.headers,
        expires_at: now + DIRECT_ACCESS_TTL,
    };
    {
        let mut guard = DIRECT_ACCESS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(entry.clone());
    }
    Ok(entry)
}

fn invalidate_direct_access_token() {
    let mut guard = DIRECT_ACCESS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    *guard = None;
}

fn generate_pkce() -> (String, String) {
    let bytes = random_bytes(32);
    let verifier = encode_url(&bytes);
    let challenge = encode_url(&sha256(verifier.as_bytes()));
    (verifier, challenge)
}

async fn login_gitlab(callbacks: Arc<dyn OAuthLoginCallbacks>) -> pi_js::Result<OAuthCredentials> {
    let (verifier, challenge) = generate_pkce();
    let scopes = OAUTH_SCOPES.join(" ");
    let state = random_uuid();
    let auth_query = form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", BUNDLED_CLIENT_ID)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("response_type", "code")
        .append_pair("scope", &scopes)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .finish();

    callbacks.on_auth(OAuthAuthInfo {
        url: format!("{GITLAB_COM_URL}/oauth/authorize?{auth_query}"),
        instructions: None,
    });
    let callback_url = callbacks
        .on_prompt(OAuthPrompt {
            message: "Paste the callback URL:".to_string(),
            placeholder: None,
            allow_empty: None,
        })
        .await?;

    let parsed = url::Url::parse(&callback_url).map_err(|e| pi_js::Error::msg(e.to_string()))?;
    let code = parsed
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned())
        .ok_or_else(|| pi_js::Error::msg("No authorization code found in callback URL"))?;

    let mut headers = Headers::new();
    headers.set("Content-Type", "application/x-www-form-urlencoded");
    let form = form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", BUNDLED_CLIENT_ID)
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", &code)
        .append_pair("code_verifier", &verifier)
        .append_pair("redirect_uri", REDIRECT_URI)
        .finish();
    let token_response = fetch(Request {
        method: "POST".to_string(),
        url: format!("{GITLAB_COM_URL}/oauth/token"),
        headers,
        body: Some(Body::Text(form)),
        signal: None,
    })
    .await?;

    if !token_response.ok() {
        let error_text = token_response.text().await?;
        return Err(pi_js::Error::msg(format!("Token exchange failed: {error_text}")));
    }

    let text = token_response.text().await?;
    let data: OAuthTokenResponse = pi_js::json::parse_as(&text)?;
    invalidate_direct_access_token();
    Ok(OAuthCredentials {
        refresh: data.refresh_token,
        access: data.access_token,
        expires: (data.created_at + data.expires_in) * 1000 - 5 * 60 * 1000,
        extra: IndexMap::new(),
    })
}

async fn refresh_gitlab_token(credentials: OAuthCredentials, signal: AbortSignal) -> pi_js::Result<OAuthCredentials> {
    let mut headers = Headers::new();
    headers.set("Content-Type", "application/x-www-form-urlencoded");
    let form = form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", BUNDLED_CLIENT_ID)
        .append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", &credentials.refresh)
        .finish();
    let response = fetch(Request {
        method: "POST".to_string(),
        url: format!("{GITLAB_COM_URL}/oauth/token"),
        headers,
        body: Some(Body::Text(form)),
        signal: Some(signal),
    })
    .await?;

    if !response.ok() {
        let error_text = response.text().await?;
        return Err(pi_js::Error::msg(format!("Token refresh failed: {error_text}")));
    }
    let text = response.text().await?;
    let data: OAuthTokenResponse = pi_js::json::parse_as(&text)?;
    invalidate_direct_access_token();
    Ok(OAuthCredentials {
        refresh: data.refresh_token,
        access: data.access_token,
        expires: (data.created_at + data.expires_in) * 1000 - 5 * 60 * 1000,
        extra: IndexMap::new(),
    })
}

pub fn stream_git_lab_duo(
    model: Model,
    context: TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let stream_clone = stream.clone();

    tokio::spawn(async move {
        let res: pi_js::Result<()> = async {
            let gitlab_access_token = options
                .as_ref()
                .and_then(|o| o.base.api_key.as_deref())
                .filter(|k| !k.is_empty())
                .ok_or_else(|| pi_js::Error::msg("No GitLab access token. Run /login gitlab-duo or set GITLAB_TOKEN"))?
                .to_string();

            let cfg = models()
                .into_iter()
                .find(|m| m.id == model.id)
                .ok_or_else(|| pi_js::Error::msg(format!("Unknown model: {}", model.id)))?;

            let direct_access = get_direct_access_token(&gitlab_access_token).await?;
            let mut model_with_base_url = model.clone();
            model_with_base_url.base_url = cfg.base_url.to_string();

            let mut merged_headers = IndexMap::new();
            for (k, v) in direct_access.headers {
                merged_headers.insert(k, Some(v));
            }
            merged_headers.insert(
                "Authorization".to_string(),
                Some(format!("Bearer {}", direct_access.token)),
            );

            let mut stream_options = options.unwrap_or_default();
            stream_options.base.api_key = Some("gitlab-duo".to_string());
            stream_options.base.headers = Some(merged_headers);

            let mut inner_stream = match cfg.backend {
                Backend::Anthropic => {
                    let mut compat: AnthropicMessagesCompat = match model_with_base_url.compat {
                        Some(ModelCompat::AnthropicMessages(c)) => c,
                        _ => AnthropicMessagesCompat::default(),
                    };
                    compat.force_adaptive_thinking = Some(true);
                    model_with_base_url.compat = Some(ModelCompat::AnthropicMessages(compat));
                    anthropic_messages::provider_streams().stream_simple(
                        &model_with_base_url,
                        &context,
                        Some(stream_options),
                    )
                }
                Backend::OpenAI => openai_responses::provider_streams().stream_simple(
                    &model_with_base_url,
                    &context,
                    Some(stream_options),
                ),
            };

            while let Some(event) = inner_stream.next().await {
                stream_clone.push(event);
            }
            stream_clone.end(inner_stream.try_result());
            Ok(())
        }
        .await;

        if let Err(err) = res {
            let error_msg = AssistantMessage {
                content: Vec::new(),
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                response_model: None,
                response_id: None,
                provider_thinking_level: None,
                thinking_level: None,
                diagnostics: None,
                usage: Usage::default(),
                stop_reason: StopReason::Error,
                deferred: None,
                error_message: Some(err.to_string()),
                raw_stop_reason: None,
                end_turn: None,
                duration_ms: None,
                timestamp: now_ms(),
            };
            stream_clone.push(AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: error_msg.clone(),
            });
            stream_clone.end(Some(error_msg));
        }
    });

    stream
}

pub struct GitLabDuoExtension;

#[async_trait]
impl Extension for GitLabDuoExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let model_configs: Vec<ProviderModelConfig> = models()
            .into_iter()
            .map(|m| {
                let thinking_level_map = if m.xhigh_mapped {
                    let mut map: ThinkingLevelMap = IndexMap::new();
                    map.insert(ModelThinkingLevel::XHigh, Some("max".to_string()));
                    Some(map)
                } else {
                    None
                };
                ProviderModelConfig::Chat(ProviderChatModelConfig {
                    id: m.id.to_string(),
                    name: m.name.to_string(),
                    api: None,
                    base_url: None,
                    input: vec![ModelInput::Text, ModelInput::Image],
                    input_limits: None,
                    cost: m.cost,
                    headers: None,
                    reasoning: m.reasoning,
                    thinking_level_map,
                    prompt_cache: None,
                    context_window: m.context_window,
                    max_tokens: m.max_tokens,
                    sampling_params: None,
                    sampling_params_by_thinking_level: None,
                    compat: None,
                })
            })
            .collect();

        pi.register_provider(
            "gitlab-duo",
            ProviderConfig {
                name: None,
                base_url: Some(AI_GATEWAY_URL.to_string()),
                api_key: Some("$GITLAB_TOKEN".to_string()),
                api: Some("gitlab-duo-api".to_string()),
                models: Some(model_configs),
                oauth: Some(ProviderOAuthConfig {
                    name: "GitLab Duo".to_string(),
                    is_subscription: None,
                    uses_callback_server: None,
                    login: Arc::new(|callbacks| Box::pin(login_gitlab(callbacks))),
                    refresh_token: Arc::new(|credentials, signal| Box::pin(refresh_gitlab_token(credentials, signal))),
                    get_api_key: Arc::new(|cred| cred.access.clone()),
                    modify_models: None,
                }),
                stream_simple: Some(Arc::new(stream_git_lab_duo)),
                ..Default::default()
            },
        )?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(GitLabDuoExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
