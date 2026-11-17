//! Port of packages/coding-agent/src/cli/auth-check.ts

use std::sync::Arc;

use pi_ai::{AuthType, Credential, CredentialStore};
use serde::{Deserialize, Serialize};

use crate::cli::args::Args;
use crate::cli::auth_command::{AuthCommandError, AuthCommandKind, get_auth_credential, validate_auth_command_args};
use crate::core::model_resolver::{ResolveCliModelOptions, resolve_cli_model};
use crate::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use crate::core::models_store::InMemoryCodingAgentModelsStore;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthCheckStatus {
    Ready,
    NotReady,
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthCheckReason {
    ProviderNotFound,
    CredentialsNotConfigured,
    CredentialNotAvailable,
    InvalidState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthCheckResult {
    pub status: AuthCheckStatus,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<AuthCheckReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_type: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AuthCheckOptions {
    pub refresh: bool,
}

pub async fn check_provider_auth(
    args: &Args,
    model_runtime: &ModelRuntime,
    options: Option<AuthCheckOptions>,
) -> pi_js::Result<AuthCheckResult> {
    let refresh = options.map(|o| o.refresh).unwrap_or(false);
    let (cli_provider, cli_model) = validate_auth_command_args(args, AuthCommandKind::Check)?;
    let mut provider = cli_provider.clone();

    if let Some(cli_model) = cli_model.as_deref() {
        let resolved = resolve_cli_model(ResolveCliModelOptions {
            cli_provider: cli_provider.as_deref(),
            cli_model,
            cli_thinking: None,
            model_runtime,
        });
        if resolved.error.is_some() || resolved.model.is_none() {
            let msg = resolved
                .error
                .unwrap_or_else(|| format!("Unable to resolve model \"{cli_model}\""));
            return Err(AuthCommandError::new(msg).into_error());
        }
        provider = resolved.model.map(|m| m.provider);
    }

    let Some(provider) = provider else {
        return Err(AuthCommandError::new("Unable to resolve an auth provider").into_error());
    };

    if model_runtime.get_error().is_some() {
        return Ok(AuthCheckResult {
            status: AuthCheckStatus::Invalid,
            provider,
            reason: Some(AuthCheckReason::InvalidState),
            auth_type: None,
        });
    }

    if model_runtime.get_provider(&provider).is_none() {
        return Ok(AuthCheckResult {
            status: AuthCheckStatus::NotReady,
            provider,
            reason: Some(AuthCheckReason::ProviderNotFound),
            auth_type: None,
        });
    }

    match model_runtime.check_auth(&provider, None).await {
        Ok(Some(auth)) => {
            if refresh {
                match model_runtime.get_auth_provider_id(&provider, None).await {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        return Ok(AuthCheckResult {
                            status: AuthCheckStatus::NotReady,
                            provider,
                            reason: Some(AuthCheckReason::CredentialsNotConfigured),
                            auth_type: None,
                        });
                    }
                    Err(_) => {
                        return Ok(AuthCheckResult {
                            status: AuthCheckStatus::Invalid,
                            provider,
                            reason: Some(AuthCheckReason::InvalidState),
                            auth_type: None,
                        });
                    }
                }
            }
            let auth_type = match auth.auth_type {
                AuthType::ApiKey => "api_key".to_string(),
                AuthType::OAuth => "oauth".to_string(),
            };
            Ok(AuthCheckResult {
                status: AuthCheckStatus::Ready,
                provider,
                reason: None,
                auth_type: Some(auth_type),
            })
        }
        Ok(None) => Ok(AuthCheckResult {
            status: AuthCheckStatus::NotReady,
            provider,
            reason: Some(AuthCheckReason::CredentialsNotConfigured),
            auth_type: None,
        }),
        Err(_) => Ok(AuthCheckResult {
            status: AuthCheckStatus::Invalid,
            provider,
            reason: Some(AuthCheckReason::InvalidState),
            auth_type: None,
        }),
    }
}

pub async fn get_provider_credential(
    provider_id: &str,
    model_runtime: &ModelRuntime,
    credentials: &dyn CredentialStore,
    options: AuthCheckOptions,
) -> pi_js::Result<Option<String>> {
    let credential = credentials.read(provider_id, None).await?;
    if !options.refresh
        && let Some(Credential::OAuth(oauth)) = credential
    {
        return Ok(Some(oauth.access));
    }
    let auth = model_runtime.get_auth_provider_id(provider_id, None).await?;
    Ok(get_auth_credential(auth.as_ref()))
}

pub async fn create_auth_check_model_runtime(credentials: Arc<dyn CredentialStore>) -> pi_js::Result<ModelRuntime> {
    ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(credentials),
        models_store: Some(Arc::new(InMemoryCodingAgentModelsStore::default())),
        allow_model_network: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
}
