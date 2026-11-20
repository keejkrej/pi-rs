//! Port of packages/ai/src/auth/resolve.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pi_js::Result;
use pi_js::abort::{AbortController, AbortSignal};

use crate::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthContext, AuthOperationOptions, AuthResult, Credential,
    CredentialStore, OAuthAuth, OAuthCredential, ProviderAuth,
};
use crate::types::ProviderEnv;
use crate::utils::abort::{operation_signal, race_with_abort_signal};

pub use crate::utils::models_error::{ModelsError, ModelsErrorCode};

const DEFAULT_OAUTH_MINIMUM_VALIDITY_MS: i64 = 5 * 60 * 1000;
const DEFAULT_OAUTH_REFRESH_TIMEOUT_MS: u64 = 15_000;

#[derive(Clone, Debug, Default)]
pub struct AuthResolutionOverrides {
    pub api_key: Option<String>,
    pub env: Option<ProviderEnv>,
    /// Require this much remaining OAuth-token validity; defaults to five minutes.
    pub min_oauth_validity_ms: Option<i64>,
    pub signal: Option<AbortSignal>,
}

struct OverlayEnvAuthContext {
    base: Arc<dyn AuthContext>,
    env: ProviderEnv,
}

#[async_trait::async_trait]
impl AuthContext for OverlayEnvAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        if let Some(value) = self.env.get(name).filter(|s| !s.is_empty()) {
            return Some(value.clone());
        }
        self.base.env(name).await
    }

    async fn file_exists(&self, path: &str) -> bool {
        self.base.file_exists(path).await
    }
}

fn overlay_env_auth_context(base: Arc<dyn AuthContext>, env: ProviderEnv) -> Arc<dyn AuthContext> {
    Arc::new(OverlayEnvAuthContext { base, env })
}

/// Auth resolution shared by all operations in a `Models` collection.
/// A stored credential owns the provider: ambient/env is consulted only when
/// nothing is stored. No silent env fallback after a failed refresh or for a
/// credential type without a matching handler.
pub async fn resolve_provider_auth(
    provider_id: &str,
    auth: &ProviderAuth,
    credentials: &dyn CredentialStore,
    auth_context: Arc<dyn AuthContext>,
    overrides: Option<AuthResolutionOverrides>,
) -> Result<Option<AuthResult>> {
    let signal = operation_signal(overrides.as_ref().and_then(|o| o.signal.as_ref()));
    race_with_abort_signal(
        resolve_provider_auth_with_signal(provider_id, auth, credentials, auth_context, overrides, &signal),
        &signal,
    )
    .await
}

async fn resolve_provider_auth_with_signal(
    provider_id: &str,
    auth: &ProviderAuth,
    credentials: &dyn CredentialStore,
    auth_context: Arc<dyn AuthContext>,
    overrides: Option<AuthResolutionOverrides>,
    signal: &AbortSignal,
) -> Result<Option<AuthResult>> {
    signal.throw_if_aborted()?;

    let overrides_env = overrides.as_ref().and_then(|o| o.env.clone());
    let overrides_api_key = overrides.as_ref().and_then(|o| o.api_key.clone());
    let min_oauth_validity_ms = overrides.as_ref().and_then(|o| o.min_oauth_validity_ms);

    let request_auth_context = match overrides_env.clone() {
        Some(env) => overlay_env_auth_context(auth_context, env),
        None => auth_context,
    };

    if let Some(api_key) = overrides_api_key {
        if let Some(api_key_auth) = auth.api_key.as_deref() {
            return resolve_api_key(
                request_auth_context,
                api_key_auth,
                provider_id,
                Some(ApiKeyCredential {
                    key: Some(api_key),
                    env: overrides_env,
                }),
                signal.clone(),
            )
            .await;
        }
    }

    let stored = read_credential(credentials, provider_id, signal).await?;
    if let Some(stored_cred) = stored {
        match stored_cred {
            Credential::OAuth(oauth_cred) => {
                if let Some(oauth_auth) = auth.oauth.as_ref() {
                    return resolve_stored_oauth(
                        credentials,
                        provider_id,
                        Arc::clone(oauth_auth),
                        oauth_cred,
                        signal,
                        min_oauth_validity_ms,
                    )
                    .await;
                }
                return Ok(None);
            }
            Credential::ApiKey(mut api_cred) => {
                if let Some(api_key_auth) = auth.api_key.as_deref() {
                    if let Some(ovr_env) = overrides_env {
                        let mut merged = api_cred.env.unwrap_or_default();
                        for (k, v) in ovr_env {
                            merged.insert(k, v);
                        }
                        api_cred.env = Some(merged);
                    }
                    return resolve_api_key(
                        request_auth_context,
                        api_key_auth,
                        provider_id,
                        Some(api_cred),
                        signal.clone(),
                    )
                    .await;
                }
                return Ok(None);
            }
        }
    }

    // Ambient (env vars, AWS profiles, ADC files).
    if let Some(api_key_auth) = auth.api_key.as_deref() {
        resolve_api_key(request_auth_context, api_key_auth, provider_id, None, signal.clone()).await
    } else {
        Ok(None)
    }
}

fn expires_soon(credential: &OAuthCredential, minimum_validity_ms: i64) -> bool {
    pi_js::time::now_ms().saturating_add(minimum_validity_ms) >= credential.expires
}

/// Refreshes an OAuth credential inside the store lock if `needs_refresh` holds.
///
/// Cancelling `signal` aborts only waiting for the store lock, never an in-flight `oauth.refresh`
/// so rotated refresh tokens are always persisted.
pub async fn refresh_stored_oauth_credential<F>(
    credentials: &dyn CredentialStore,
    provider_id: &str,
    oauth: Arc<dyn OAuthAuth>,
    needs_refresh: F,
    signal: &AbortSignal,
) -> Result<Option<OAuthCredential>>
where
    F: Fn(&OAuthCredential) -> bool + Send + Sync + 'static,
{
    signal.throw_if_aborted()?;

    let lock_wait = Arc::new(AbortController::new());
    let entered = Arc::new(AtomicBool::new(false));
    let lock_wait_cb = Arc::clone(&lock_wait);
    let entered_cb = Arc::clone(&entered);
    let _abort_guard = signal.on_abort(move |reason| {
        if !entered_cb.load(Ordering::SeqCst) {
            lock_wait_cb.abort(Some(reason.clone()));
        }
    });
    if signal.aborted() && !entered.load(Ordering::SeqCst) {
        lock_wait.abort(signal.reason());
    }

    let oauth_for_modify = Arc::clone(&oauth);
    let provider_id_owned = provider_id.to_owned();
    let oauth_refresh_failed = Arc::new(AtomicBool::new(false));
    let oauth_refresh_failed_flag = Arc::clone(&oauth_refresh_failed);
    let entered_modify = Arc::clone(&entered);

    let post = credentials
        .modify(
            provider_id,
            Box::new(move |current| {
                entered_modify.store(true, Ordering::SeqCst);
                Box::pin(async move {
                    let Some(Credential::OAuth(cur_oauth)) = current else {
                        return Ok(None);
                    };
                    if !needs_refresh(&cur_oauth) {
                        return Ok(None);
                    }
                    let refresh_signal = AbortSignal::timeout(DEFAULT_OAUTH_REFRESH_TIMEOUT_MS);
                    match oauth_for_modify.refresh(cur_oauth, refresh_signal).await {
                        Ok(refreshed) => Ok(Some(Credential::OAuth(refreshed))),
                        Err(err) => {
                            oauth_refresh_failed_flag.store(true, Ordering::SeqCst);
                            Err(ModelsError::with_cause(
                                ModelsErrorCode::OAuth,
                                format!("OAuth refresh failed for {provider_id_owned}"),
                                err,
                            )
                            .into())
                        }
                    }
                })
            }),
            Some(AuthOperationOptions {
                signal: Some(lock_wait.signal()),
            }),
        )
        .await
        .map_err(|err| {
            if oauth_refresh_failed.load(Ordering::SeqCst) {
                err
            } else if let Err(abort_err) = signal.throw_if_aborted() {
                abort_err
            } else {
                ModelsError::with_cause(
                    ModelsErrorCode::Auth,
                    format!("Credential store modify failed for {provider_id}"),
                    err,
                )
                .into()
            }
        })?;

    match post {
        Some(Credential::OAuth(cred)) => Ok(Some(cred)),
        _ => Ok(None),
    }
}

/// OAuth resolution with double-checked locking: tokens with less than five
/// minutes remaining lock, re-check expiry under the lock, refresh once
/// globally, and persist the rotated credential before release.
async fn resolve_stored_oauth(
    credentials: &dyn CredentialStore,
    provider_id: &str,
    oauth: Arc<dyn OAuthAuth>,
    stored: OAuthCredential,
    signal: &AbortSignal,
    min_oauth_validity_ms: Option<i64>,
) -> Result<Option<AuthResult>> {
    let minimum_validity_ms = DEFAULT_OAUTH_MINIMUM_VALIDITY_MS.max(min_oauth_validity_ms.unwrap_or(0));
    let mut credential = stored;

    if expires_soon(&credential, minimum_validity_ms) {
        let Some(updated_oauth) = refresh_stored_oauth_credential(
            credentials,
            provider_id,
            Arc::clone(&oauth),
            move |cur_oauth| expires_soon(cur_oauth, minimum_validity_ms),
            signal,
        )
        .await?
        else {
            return Ok(None);
        };
        credential = updated_oauth;
        signal.throw_if_aborted()?;

        if min_oauth_validity_ms.is_some() && expires_soon(&credential, minimum_validity_ms) {
            return Err(ModelsError::new(
                ModelsErrorCode::OAuth,
                format!("OAuth refresh returned a token that expires too soon for {provider_id}"),
            )
            .into());
        }
    }

    match oauth.to_auth(credential).await {
        Ok(auth) => Ok(Some(AuthResult {
            auth,
            env: None,
            source: Some("OAuth".to_owned()),
        })),
        Err(err) => Err(ModelsError::with_cause(
            ModelsErrorCode::OAuth,
            format!("OAuth auth derivation failed for {provider_id}"),
            err,
        )
        .into()),
    }
}

async fn resolve_api_key(
    auth_context: Arc<dyn AuthContext>,
    api_key: &dyn ApiKeyAuth,
    provider_id: &str,
    credential: Option<ApiKeyCredential>,
    signal: AbortSignal,
) -> Result<Option<AuthResult>> {
    api_key
        .resolve(ApiKeyAuthInput {
            ctx: auth_context,
            credential,
            signal,
        })
        .await
        .map_err(|err| {
            ModelsError::with_cause(
                ModelsErrorCode::Auth,
                format!("API key auth failed for provider {provider_id}"),
                err,
            )
            .into()
        })
}

async fn read_credential(
    credentials: &dyn CredentialStore,
    provider_id: &str,
    signal: &AbortSignal,
) -> Result<Option<Credential>> {
    credentials
        .read(
            provider_id,
            Some(AuthOperationOptions {
                signal: Some(signal.clone()),
            }),
        )
        .await
        .map_err(|err| {
            ModelsError::with_cause(
                ModelsErrorCode::Auth,
                format!("Credential store read failed for {provider_id}"),
                err,
            )
            .into()
        })
}
