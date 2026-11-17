//! Port of packages/ai/src/auth/helpers.ts

use std::sync::Arc;

use tokio::sync::OnceCell;

use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result};

use crate::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthPrompt, AuthResult, LoginOptions, ModelAuth, OAuthAuth,
    OAuthCredential, ProviderAuthInteraction,
};

struct EnvApiKeyAuthImpl {
    name: String,
    env_vars: Vec<String>,
}

#[async_trait::async_trait]
impl ApiKeyAuth for EnvApiKeyAuthImpl {
    fn name(&self) -> &str {
        &self.name
    }

    fn supports_login(&self) -> bool {
        true
    }

    async fn login(&self, interaction: ProviderAuthInteraction) -> Result<ApiKeyCredential> {
        interaction.signal.throw_if_aborted()?;
        let key = interaction
            .prompt(AuthPrompt::secret(format!("Enter {}", self.name), None))
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
                source: Some("stored credential".to_owned()),
            }));
        }

        for env_var in &self.env_vars {
            let value = input.ctx.env(env_var).await;
            input.signal.throw_if_aborted()?;
            if let Some(val) = value.filter(|v| !v.is_empty()) {
                return Ok(Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some(val),
                        headers: None,
                        base_url: None,
                    },
                    env: None,
                    source: Some(env_var.clone()),
                }));
            }
        }

        Ok(None)
    }
}

/// Standard api-key auth: a stored credential key wins, otherwise the first
/// set env var resolves. Includes a `login` that prompts for the key.
pub fn env_api_key_auth(name: impl Into<String>, env_vars: &[&str]) -> Arc<dyn ApiKeyAuth> {
    Arc::new(EnvApiKeyAuthImpl {
        name: name.into(),
        env_vars: env_vars.iter().map(|s| (*s).to_owned()).collect(),
    })
}

pub type LoadOAuthAuthFn = Arc<dyn Fn() -> BoxFuture<Result<Arc<dyn OAuthAuth>>> + Send + Sync>;

struct LazyOAuthImpl {
    name: String,
    is_subscription: bool,
    login_label: Option<String>,
    load: LoadOAuthAuthFn,
    loaded: OnceCell<Arc<dyn OAuthAuth>>,
}

impl LazyOAuthImpl {
    async fn get_loaded(&self) -> Result<&Arc<dyn OAuthAuth>> {
        self.loaded.get_or_try_init(|| (self.load)()).await
    }
}

#[async_trait::async_trait]
impl OAuthAuth for LazyOAuthImpl {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_subscription(&self) -> bool {
        self.is_subscription
    }

    fn login_label(&self) -> Option<&str> {
        self.login_label.as_deref()
    }

    async fn login(
        &self,
        interaction: ProviderAuthInteraction,
        options: Option<LoginOptions>,
    ) -> Result<OAuthCredential> {
        self.get_loaded().await?.login(interaction, options).await
    }

    async fn refresh(&self, credential: OAuthCredential, signal: AbortSignal) -> Result<OAuthCredential> {
        self.get_loaded().await?.refresh(credential, signal).await
    }

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth> {
        self.get_loaded().await?.to_auth(credential).await
    }
}

/// Wraps a lazily constructed/loaded `OAuthAuth` so provider definitions can
/// advertise OAuth metadata without eagerly loading flow state.
pub fn lazy_oauth(
    name: impl Into<String>,
    is_subscription: bool,
    login_label: Option<impl Into<String>>,
    load: LoadOAuthAuthFn,
) -> Arc<dyn OAuthAuth> {
    Arc::new(LazyOAuthImpl {
        name: name.into(),
        is_subscription,
        login_label: login_label.map(Into::into),
        load,
        loaded: OnceCell::new(),
    })
}
