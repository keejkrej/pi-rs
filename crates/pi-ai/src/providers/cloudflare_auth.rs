//! Port of packages/ai/src/providers/cloudflare-auth.ts

use std::sync::Arc;

use pi_js::Result;
use pi_js::abort::AbortSignal;

use crate::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthContext, AuthPrompt, AuthResult, ModelAuth,
    ProviderAuthInteraction,
};
use crate::types::{ProviderEnv, ProviderHeaders};

const CLOUDFLARE_API_KEY: &str = "CLOUDFLARE_API_KEY";
const CLOUDFLARE_ACCOUNT_ID: &str = "CLOUDFLARE_ACCOUNT_ID";
const CLOUDFLARE_GATEWAY_ID: &str = "CLOUDFLARE_GATEWAY_ID";

#[derive(Clone, Copy, PartialEq, Eq)]
enum CloudflareAuthKind {
    WorkersAi,
    AiGateway,
}

async fn resolve_value(
    name: &str,
    ctx: &dyn AuthContext,
    credential: Option<&ApiKeyCredential>,
    signal: &AbortSignal,
) -> Result<Option<String>> {
    // Per-field merge: prefer credential value, fall back to ambient env so a key-only
    // credential still picks up account / gateway IDs from the environment.
    let from_credential = credential.and_then(|cred| {
        if name == CLOUDFLARE_API_KEY {
            cred.key.as_deref()
        } else {
            cred.env.as_ref().and_then(|env| env.get(name).map(String::as_str))
        }
    });
    if let Some(val) = from_credential {
        return Ok(Some(val.to_owned()));
    }
    signal.throw_if_aborted()?;
    let value = ctx.env(name).await;
    signal.throw_if_aborted()?;
    Ok(value)
}

async fn resolve_cloudflare_env(
    kind: CloudflareAuthKind,
    input: &ApiKeyAuthInput,
) -> Result<Option<(String, ProviderEnv, String)>> {
    let cred = input.credential.as_ref();
    let api_key = resolve_value(CLOUDFLARE_API_KEY, input.ctx.as_ref(), cred, &input.signal).await?;
    let account_id = resolve_value(CLOUDFLARE_ACCOUNT_ID, input.ctx.as_ref(), cred, &input.signal).await?;
    let gateway_id = match kind {
        CloudflareAuthKind::AiGateway => {
            resolve_value(CLOUDFLARE_GATEWAY_ID, input.ctx.as_ref(), cred, &input.signal).await?
        }
        CloudflareAuthKind::WorkersAi => None,
    };

    let Some(api_key) = api_key.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let Some(account_id) = account_id.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if kind == CloudflareAuthKind::AiGateway && gateway_id.as_deref().is_none_or(str::is_empty) {
        return Ok(None);
    }

    let mut env = ProviderEnv::new();
    env.insert(CLOUDFLARE_ACCOUNT_ID.to_owned(), account_id);
    if let Some(gw) = gateway_id.filter(|s| !s.is_empty()) {
        env.insert(CLOUDFLARE_GATEWAY_ID.to_owned(), gw);
    }

    let source = if cred.is_some() {
        "stored credential".to_owned()
    } else {
        CLOUDFLARE_API_KEY.to_owned()
    };

    Ok(Some((api_key, env, source)))
}

struct CloudflareAuth {
    kind: CloudflareAuthKind,
}

#[async_trait::async_trait]
impl ApiKeyAuth for CloudflareAuth {
    fn name(&self) -> &str {
        "Cloudflare API key"
    }

    fn supports_login(&self) -> bool {
        true
    }

    async fn login(&self, interaction: ProviderAuthInteraction) -> Result<ApiKeyCredential> {
        interaction.signal.throw_if_aborted()?;
        let key = interaction
            .prompt(AuthPrompt::secret("Enter Cloudflare API key", None))
            .await?;
        interaction.signal.throw_if_aborted()?;
        let account_id = interaction
            .prompt(AuthPrompt::text("Enter Cloudflare account ID", None))
            .await?;
        interaction.signal.throw_if_aborted()?;

        let mut env = ProviderEnv::new();
        env.insert(CLOUDFLARE_ACCOUNT_ID.to_owned(), account_id);

        if self.kind == CloudflareAuthKind::AiGateway {
            let gateway_id = interaction
                .prompt(AuthPrompt::text("Enter Cloudflare AI Gateway ID", None))
                .await?;
            interaction.signal.throw_if_aborted()?;
            env.insert(CLOUDFLARE_GATEWAY_ID.to_owned(), gateway_id);
        }

        Ok(ApiKeyCredential {
            key: Some(key),
            env: Some(env),
        })
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        let Some((api_key, env, source)) = resolve_cloudflare_env(self.kind, &input).await? else {
            return Ok(None);
        };
        let auth = match self.kind {
            CloudflareAuthKind::WorkersAi => ModelAuth {
                api_key: Some(api_key),
                headers: None,
                base_url: None,
            },
            CloudflareAuthKind::AiGateway => {
                let mut headers = ProviderHeaders::new();
                headers.insert("cf-aig-authorization".to_owned(), Some(format!("Bearer {api_key}")));
                headers.insert("Authorization".to_owned(), None);
                headers.insert("x-api-key".to_owned(), None);
                ModelAuth {
                    api_key: None,
                    headers: Some(headers),
                    base_url: None,
                }
            }
        };
        Ok(Some(AuthResult {
            auth,
            env: Some(env),
            source: Some(source),
        }))
    }
}

pub fn cloudflare_workers_ai_auth() -> Arc<dyn ApiKeyAuth> {
    Arc::new(CloudflareAuth {
        kind: CloudflareAuthKind::WorkersAi,
    })
}

pub fn cloudflare_ai_gateway_auth() -> Arc<dyn ApiKeyAuth> {
    Arc::new(CloudflareAuth {
        kind: CloudflareAuthKind::AiGateway,
    })
}
