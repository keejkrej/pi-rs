//! Port of packages/coding-agent/src/cli/auth-logout.ts

use pi_ai::{AuthOperationOptions, AuthType, CredentialInfo};
use pi_js::abort::AbortSignal;
use pi_tui::replace_control_characters;

use crate::cli::auth_terminal::AuthOutput;
use crate::config::APP_NAME;
use crate::core::model_runtime::{CredentialSynchronizationError, ModelRuntime};
use crate::core::provider_matching::find_matching_provider_arcs;

/// Dependencies of `run_auth_logout`, injected so tests can capture the output.
pub struct AuthLogoutOptions<'a> {
    pub model_runtime: &'a ModelRuntime,
    pub terminal: &'a dyn AuthOutput,
    pub signal: AbortSignal,
}

fn auth_logout_usage() -> String {
    format!("{} auth logout <provider>", *APP_NAME)
}

/// Run `pi auth logout <provider>`: remove the credential `pi auth login` or `/login` stored, and return the exit code.
/// Environment variables and `models.json` config stay, as with `/logout`.
pub async fn run_auth_logout<S: AsRef<str>>(args: &[S], options: AuthLogoutOptions<'_>) -> i32 {
    let model_runtime = options.model_runtime;
    let terminal = options.terminal;
    let signal = options.signal;

    let Some(provider_ref) = args.first().map(AsRef::as_ref) else {
        terminal.error(&format!("Usage: {}", auth_logout_usage()));
        return 1;
    };
    if provider_ref.starts_with('-') || args.len() > 1 {
        terminal.error(&format!("Usage: {}", auth_logout_usage()));
        return 1;
    }

    let stored: Vec<CredentialInfo> = match model_runtime
        .list_credentials(Some(AuthOperationOptions {
            signal: Some(signal.clone()),
        }))
        .await
    {
        Ok(creds) => creds,
        Err(err) => {
            terminal.error(&format!("Could not read stored credentials: {err}"));
            return 1;
        }
    };

    let providers = model_runtime.get_providers();
    let provider = find_matching_provider_arcs(&providers, provider_ref).into_iter().next();
    let target_id = provider.map(|p| p.id()).unwrap_or(provider_ref);
    // Providers registered by extensions are not loaded here, so their stored credentials match by id alone.
    let credential = stored.iter().find(|candidate| candidate.provider_id == target_id);
    if provider.is_none() && credential.is_none() {
        terminal.error(&format!(
            "Unknown provider \"{}\".",
            replace_control_characters(provider_ref)
        ));
        let mut stored_ids: Vec<&str> = stored.iter().map(|c| c.provider_id.as_str()).collect();
        stored_ids.sort_unstable();
        let list = if stored_ids.is_empty() {
            "none".to_string()
        } else {
            stored_ids.join(", ")
        };
        terminal.error(&format!("Stored credentials: {list}"));
        return 1;
    }

    let Some(credential) = credential else {
        let provider_name = provider.map(|p| p.name()).unwrap_or(provider_ref);
        terminal.write(&format!(
            "No stored credentials for {provider_name}. Environment variables and models.json config are unchanged."
        ));
        return 0;
    };

    let provider_name = provider.map(|p| p.name()).unwrap_or(provider_ref);
    if let Err(err) = model_runtime
        .logout(
            &credential.provider_id,
            Some(AuthOperationOptions { signal: Some(signal) }),
        )
        .await
    {
        if err.downcast_ref::<CredentialSynchronizationError>().is_some() {
            terminal.error(&format!(
                "Credentials removed for {provider_name}, but local model state could not be synchronized: {err}"
            ));
        } else {
            terminal.error(&format!("Logout failed: {err}"));
        }
        return 1;
    }

    terminal.write(&if credential.credential_type == AuthType::OAuth {
        format!("Logged out of {provider_name}.")
    } else {
        format!(
            "Removed stored API key for {provider_name}. Environment variables and models.json config are unchanged."
        )
    });
    0
}
