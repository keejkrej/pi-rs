//! Port of packages/ai/src/providers/anthropic.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_js::Result;

use crate::api::anthropic_messages_lazy::anthropic_messages_api;
use crate::auth::helpers::lazy_oauth;
use crate::auth::oauth::load::load_anthropic_oauth;
use crate::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthPrompt, AuthResult, ModelAuth, ProviderAuth,
    ProviderAuthInteraction,
};
use crate::env_api_keys::{
    ANTHROPIC_API_KEY_ENV, ANTHROPIC_AUTH_TOKEN_ENV, ANTHROPIC_FEDERATION_RULE_ID_ENV,
    ANTHROPIC_IDENTITY_TOKEN_FILE_ENV, ANTHROPIC_OAUTH_TOKEN_ENV, ANTHROPIC_ORGANIZATION_ID_ENV,
    ANTHROPIC_SERVICE_ACCOUNT_ID_ENV, ANTHROPIC_WORKSPACE_ID_ENV,
};
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider, create_provider};
use crate::providers::anthropic_models::anthropic_models;
use crate::types::{ProviderEnv, ProviderHeaders};

struct AnthropicApiKeyAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for AnthropicApiKeyAuth {
    fn name(&self) -> &str {
        "Anthropic API key"
    }

    fn supports_login(&self) -> bool {
        true
    }

    async fn login(&self, interaction: ProviderAuthInteraction) -> Result<ApiKeyCredential> {
        interaction.signal.throw_if_aborted()?;
        let key = interaction
            .prompt(AuthPrompt::secret("Enter Anthropic API key", None))
            .await?;
        interaction.signal.throw_if_aborted()?;
        Ok(ApiKeyCredential {
            key: Some(key),
            env: None,
        })
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        input.signal.throw_if_aborted()?;
        if let Some(key) = input
            .credential
            .as_ref()
            .and_then(|c| c.key.clone())
            .filter(|k| !k.is_empty())
        {
            return Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some(key),
                    headers: None,
                    base_url: None,
                },
                env: input.credential.and_then(|c| c.env),
                source: Some("stored credential".to_string()),
            }));
        }

        if let Some(auth_token) = input.ctx.env(ANTHROPIC_AUTH_TOKEN_ENV).await {
            input.signal.throw_if_aborted()?;
            if !auth_token.is_empty() {
                let mut headers: ProviderHeaders = IndexMap::new();
                headers.insert("Authorization".to_string(), Some(format!("Bearer {auth_token}")));
                return Ok(Some(AuthResult {
                    auth: ModelAuth {
                        api_key: None,
                        headers: Some(headers),
                        base_url: None,
                    },
                    env: None,
                    source: Some(ANTHROPIC_AUTH_TOKEN_ENV.to_string()),
                }));
            }
        }
        input.signal.throw_if_aborted()?;

        for env_var in [ANTHROPIC_OAUTH_TOKEN_ENV, ANTHROPIC_API_KEY_ENV] {
            let api_key = input.ctx.env(env_var).await;
            input.signal.throw_if_aborted()?;
            if let Some(key) = api_key.filter(|k| !k.is_empty()) {
                return Ok(Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some(key),
                        headers: None,
                        base_url: None,
                    },
                    env: None,
                    source: Some(env_var.to_string()),
                }));
            }
        }

        // Workload identity federation: SDK exchanges the identity token itself.
        let mut federation: ProviderEnv = IndexMap::new();
        for env_var in [
            ANTHROPIC_FEDERATION_RULE_ID_ENV,
            ANTHROPIC_ORGANIZATION_ID_ENV,
            ANTHROPIC_IDENTITY_TOKEN_FILE_ENV,
        ] {
            let value = input.ctx.env(env_var).await;
            input.signal.throw_if_aborted()?;
            let Some(val) = value.filter(|v| !v.is_empty()) else {
                return Ok(None);
            };
            federation.insert(env_var.to_string(), val);
        }
        for env_var in [ANTHROPIC_SERVICE_ACCOUNT_ID_ENV, ANTHROPIC_WORKSPACE_ID_ENV] {
            let value = input.ctx.env(env_var).await;
            input.signal.throw_if_aborted()?;
            if let Some(val) = value.filter(|v| !v.is_empty()) {
                federation.insert(env_var.to_string(), val);
            }
        }

        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: None,
                headers: None,
                base_url: None,
            },
            env: Some(federation),
            source: Some("workload identity federation".to_string()),
        }))
    }
}

pub fn anthropic_provider() -> Result<Arc<dyn Provider + Send + Sync>> {
    create_provider(CreateProviderOptions {
        id: "anthropic".to_string(),
        name: Some("Anthropic".to_string()),
        base_url: Some("https://api.anthropic.com".to_string()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(AnthropicApiKeyAuth)),
            oauth: Some(lazy_oauth(
                "Anthropic (Claude Pro/Max)",
                true,
                None::<&str>,
                Arc::new(|| Box::pin(load_anthropic_oauth())),
            )),
        },
        models: anthropic_models(),
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(anthropic_messages_api())),
        images: None,
        classifiers: None,
    })
}
