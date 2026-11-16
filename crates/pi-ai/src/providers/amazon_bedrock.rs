//! Port of packages/ai/src/providers/amazon-bedrock.ts

use std::sync::Arc;

use pi_js::Result;

use crate::api::bedrock_converse_stream_lazy::bedrock_converse_stream_api;
use crate::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthEvent, AuthInfoLink, AuthPrompt, AuthResult, AuthSelectOption,
    ModelAuth, ProviderAuth, ProviderAuthInteraction,
};
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider, create_provider};
use crate::providers::amazon_bedrock_models::amazon_bedrock_models;
use crate::types::{AnyModel, ProviderEnv};

/// Bedrock accepts a bearer token or the AWS SDK's default credential chain.
/// The login flow can store a token/profile choice; resolve also detects ambient
/// AWS credentials without copying them into pi's credential store.
#[derive(Clone, Copy, Debug, Default)]
pub struct BedrockApiKeyAuth;

async fn read_env(
    ctx: &Arc<dyn crate::auth::types::AuthContext>,
    signal: &pi_js::abort::AbortSignal,
    name: &str,
) -> Result<Option<String>> {
    signal.throw_if_aborted()?;
    let value = ctx.env(name).await;
    signal.throw_if_aborted()?;
    Ok(value.filter(|s| !s.is_empty()))
}

#[async_trait::async_trait]
impl ApiKeyAuth for BedrockApiKeyAuth {
    fn name(&self) -> &str {
        "AWS credentials or bearer token"
    }

    fn supports_login(&self) -> bool {
        true
    }

    async fn login(&self, interaction: ProviderAuthInteraction) -> Result<ApiKeyCredential> {
        interaction.signal.throw_if_aborted()?;
        let method = interaction
            .prompt(AuthPrompt::select(
                "Select Amazon Bedrock authentication method:",
                vec![
                    AuthSelectOption {
                        id: "bearer-token".to_string(),
                        label: "Bearer token".to_string(),
                        description: None,
                    },
                    AuthSelectOption {
                        id: "aws-profile".to_string(),
                        label: "AWS profile".to_string(),
                        description: None,
                    },
                    AuthSelectOption {
                        id: "credential-chain".to_string(),
                        label: "Existing AWS credential chain".to_string(),
                        description: None,
                    },
                ],
            ))
            .await?;
        interaction.signal.throw_if_aborted()?;

        if method == "bearer-token" {
            let key = interaction
                .prompt(AuthPrompt::secret("Enter Amazon Bedrock bearer token", None))
                .await?;
            return Ok(ApiKeyCredential {
                key: Some(key),
                env: None,
            });
        }

        interaction.notify(AuthEvent::Info {
            message: "Amazon Bedrock supports AWS profiles, IAM credentials, and role-based credentials.".to_string(),
            links: Some(vec![AuthInfoLink {
                label: Some("AWS credential provider chain".to_string()),
                url: "https://docs.aws.amazon.com/sdkref/latest/guide/standardized-credentials.html".to_string(),
            }]),
        });

        if method == "aws-profile" {
            let profile = interaction
                .prompt(AuthPrompt::text("Enter AWS profile name", None))
                .await?;
            let mut env = ProviderEnv::new();
            env.insert("AWS_PROFILE".to_string(), profile);
            return Ok(ApiKeyCredential {
                key: None,
                env: Some(env),
            });
        }

        if method != "credential-chain" {
            return Err(pi_js::Error::msg(format!(
                "Unknown Amazon Bedrock auth method: {method}"
            )));
        }

        let _ = interaction
            .prompt(AuthPrompt::text(
                "Configure AWS credentials, then press Enter to continue",
                None,
            ))
            .await?;
        Ok(ApiKeyCredential::default())
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        let ApiKeyAuthInput {
            ctx,
            credential,
            signal,
        } = input;

        if let Some(cred) = &credential
            && let Some(key) = cred.key.as_ref().filter(|s| !s.is_empty())
        {
            return Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some(key.clone()),
                    headers: None,
                    base_url: None,
                },
                env: cred.env.clone(),
                source: Some("stored credential".to_string()),
            }));
        }

        if read_env(&ctx, &signal, "AWS_BEARER_TOKEN_BEDROCK").await?.is_some() {
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: None,
                source: Some("AWS_BEARER_TOKEN_BEDROCK".to_string()),
            }));
        }

        let stored_profile = credential
            .as_ref()
            .and_then(|c| c.env.as_ref())
            .and_then(|e| e.get("AWS_PROFILE"))
            .filter(|s| !s.is_empty())
            .cloned();
        if stored_profile.is_some() {
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: credential.and_then(|c| c.env),
                source: Some("stored credential".to_string()),
            }));
        }
        if read_env(&ctx, &signal, "AWS_PROFILE").await?.is_some() {
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: credential.and_then(|c| c.env),
                source: Some("AWS_PROFILE".to_string()),
            }));
        }

        let access_key = read_env(&ctx, &signal, "AWS_ACCESS_KEY_ID").await?;
        let secret_key = read_env(&ctx, &signal, "AWS_SECRET_ACCESS_KEY").await?;
        if access_key.is_some() && secret_key.is_some() {
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: None,
                source: Some("AWS access keys".to_string()),
            }));
        }

        if read_env(&ctx, &signal, "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
            .await?
            .is_some()
        {
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: None,
                source: Some("ECS task role".to_string()),
            }));
        }

        if read_env(&ctx, &signal, "AWS_CONTAINER_CREDENTIALS_FULL_URI")
            .await?
            .is_some()
        {
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: None,
                source: Some("ECS task role".to_string()),
            }));
        }

        if read_env(&ctx, &signal, "AWS_WEB_IDENTITY_TOKEN_FILE").await?.is_some() {
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: None,
                source: Some("web identity token".to_string()),
            }));
        }

        Ok(None)
    }
}

pub fn try_amazon_bedrock_provider() -> Result<Arc<dyn Provider + Send + Sync>> {
    let models = amazon_bedrock_models().values().cloned().map(AnyModel::Chat).collect();
    create_provider(CreateProviderOptions {
        id: "amazon-bedrock".to_string(),
        name: Some("Amazon Bedrock".to_string()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(BedrockApiKeyAuth)),
            oauth: None,
        },
        models,
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(bedrock_converse_stream_api())),
        images: None,
        classifiers: None,
    })
}

pub fn amazon_bedrock_provider() -> Arc<dyn Provider + Send + Sync> {
    match try_amazon_bedrock_provider() {
        Ok(provider) => provider,
        Err(err) => panic!("Failed to create amazon-bedrock provider: {err}"),
    }
}
