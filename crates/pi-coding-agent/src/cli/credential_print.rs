//! Port of packages/coding-agent/src/cli/credential-print.ts

use indexmap::IndexMap;
use pi_ai::{CredentialType, Model};
use pi_js::abort::AbortSignal;

use crate::cli::args::Args;
use crate::cli::auth_command::{AuthCommandError, AuthCommandKind, get_auth_credential, validate_auth_command_args};
use crate::core::model_resolver::{ResolveCliModelOptions, resolve_cli_model};
use crate::core::model_runtime::{ModelRuntime, ModelRuntimeGetAuthOptions};

const DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS: i64 = 30 * 60_000;

struct CandidateProvider {
    id: String,
    model: Option<Model>,
}

struct ResolvedCredential {
    provider_id: String,
    value: String,
}

/// Resolve one configured provider credential.
///
/// This intentionally calls `ModelRuntime::get_auth()`, which refreshes and persists
/// OAuth credentials with less than five minutes remaining through the normal request-auth path.
pub async fn resolve_credential_for_print(
    args: &Args,
    model_runtime: &ModelRuntime,
    kind: AuthCommandKind,
    min_expiry_ms: Option<i64>,
    signal: Option<AbortSignal>,
) -> pi_js::Result<String> {
    let (cli_provider, cli_model) = validate_auth_command_args(args, kind)?;
    let listed = model_runtime.list_credentials(signal.clone()).await?;
    let mut credential_types: IndexMap<String, CredentialType> = IndexMap::new();
    for cred in listed {
        credential_types.insert(cred.provider_id, cred.r#type);
    }

    let mut providers: Vec<CandidateProvider> = Vec::new();
    if let Some(cli_provider) = cli_provider.as_deref() {
        let Some(provider) = model_runtime.get_provider(cli_provider) else {
            return Err(AuthCommandError::new(format!(
                "Unknown provider \"{cli_provider}\". Use --list-models to see available providers."
            ))
            .into_error());
        };
        if let Some(cli_model) = cli_model.as_deref() {
            let resolved = resolve_cli_model(ResolveCliModelOptions {
                cli_provider: Some(&provider.id),
                cli_model,
                cli_thinking: None,
                model_runtime,
            });
            if resolved.error.is_some() || resolved.model.is_none() {
                let msg = resolved
                    .error
                    .unwrap_or_else(|| "Unable to resolve the requested provider/model".to_string());
                return Err(AuthCommandError::new(msg).into_error());
            }
            providers.push(CandidateProvider {
                id: provider.id,
                model: resolved.model,
            });
        } else {
            providers.push(CandidateProvider {
                id: provider.id,
                model: None,
            });
        }
    } else {
        let model_query = cli_model.as_deref().unwrap_or("");
        for provider in model_runtime.get_providers() {
            if !credential_types.contains_key(&provider.id) {
                continue;
            }
            let resolved = resolve_cli_model(ResolveCliModelOptions {
                cli_provider: Some(&provider.id),
                cli_model: model_query,
                cli_thinking: None,
                model_runtime,
            });
            if let Some(model) = resolved.model
                && resolved.error.is_none()
                && !resolved
                    .warning
                    .as_deref()
                    .is_some_and(|w| w.contains("Using custom model id"))
            {
                providers.push(CandidateProvider {
                    id: provider.id,
                    model: Some(model),
                });
            }
        }
        if providers.is_empty() {
            return Err(AuthCommandError::new(format!(
                "Model \"{model_query}\" not found. Use --list-models to see available models."
            ))
            .into_error());
        }
    }

    let mut credentials: Vec<ResolvedCredential> = Vec::new();
    for provider in &providers {
        let cred_type = credential_types.get(&provider.id).copied();
        if kind == AuthCommandKind::ApiKey && cred_type == Some(CredentialType::OAuth) {
            continue;
        }
        if kind == AuthCommandKind::BearerToken && cred_type != Some(CredentialType::OAuth) {
            continue;
        }
        let auth_options = ModelRuntimeGetAuthOptions {
            min_oauth_validity_ms: if kind == AuthCommandKind::BearerToken {
                Some(min_expiry_ms.unwrap_or(DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS))
            } else {
                None
            },
            signal: signal.clone(),
        };
        let auth = match &provider.model {
            Some(model) => model_runtime.get_auth_for_model(model, Some(auth_options)).await?,
            None => {
                model_runtime
                    .get_auth_for_provider(&provider.id, Some(auth_options))
                    .await?
            }
        };
        if let Some(value) = get_auth_credential(auth.as_ref()) {
            credentials.push(ResolvedCredential {
                provider_id: provider.id.clone(),
                value,
            });
        }
    }

    if credentials.len() == 1 {
        return Ok(credentials.swap_remove(0).value);
    }
    if credentials.is_empty() {
        let provider_id = providers.first().map(|p| p.id.as_str());
        let cred_type = provider_id.and_then(|id| credential_types.get(id).copied());
        if cli_provider.is_some() && kind == AuthCommandKind::ApiKey && cred_type == Some(CredentialType::OAuth) {
            let pid = provider_id.unwrap_or("");
            return Err(
                AuthCommandError::new(format!("Provider \"{pid}\" is configured with OAuth, not an API key"))
                    .into_error(),
            );
        }
        if cli_provider.is_some() && kind == AuthCommandKind::BearerToken && cred_type != Some(CredentialType::OAuth) {
            let pid = provider_id.unwrap_or("");
            return Err(AuthCommandError::new(format!(
                "Provider \"{pid}\" is not configured with an OAuth bearer token"
            ))
            .into_error());
        }
        let label = if kind == AuthCommandKind::ApiKey {
            "API key"
        } else {
            "OAuth bearer token"
        };
        return Err(AuthCommandError::new(format!("No usable {label} is configured")).into_error());
    }

    let matched: Vec<&str> = credentials.iter().map(|c| c.provider_id.as_str()).collect();
    Err(AuthCommandError::new(format!(
        "Multiple configured providers matched ({}). Specify --provider.",
        matched.join(", ")
    ))
    .into_error())
}
