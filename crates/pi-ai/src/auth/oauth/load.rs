//! Port of packages/ai/src/auth/oauth/load.ts

use std::sync::{Arc, Mutex, OnceLock};

use pi_js::{BoxFuture, Result};

use crate::auth::oauth::anthropic::anthropic_oauth;
use crate::auth::oauth::github_copilot::github_copilot_oauth;
use crate::auth::oauth::kimi_coding::kimi_coding_oauth;
use crate::auth::oauth::meta::meta_oauth;
use crate::auth::oauth::openai_chatgpt::openai_chatgpt_oauth;
use crate::auth::oauth::openai_codex::openai_codex_oauth;
use crate::auth::oauth::openrouter::openrouter_oauth;
use crate::auth::oauth::radius::{RadiusOAuthOptions, create_radius_oauth};
use crate::auth::oauth::xai::xai_oauth;
use crate::auth::types::OAuthAuth;

#[derive(Clone)]
pub struct OAuthFlowLoaders {
    pub anthropic: Arc<dyn Fn() -> BoxFuture<Result<Arc<dyn OAuthAuth>>> + Send + Sync>,
    pub openai_codex: Arc<dyn Fn() -> BoxFuture<Result<Arc<dyn OAuthAuth>>> + Send + Sync>,
    pub openai_chatgpt: Arc<dyn Fn() -> BoxFuture<Result<Arc<dyn OAuthAuth>>> + Send + Sync>,
    pub github_copilot: Arc<dyn Fn() -> BoxFuture<Result<Arc<dyn OAuthAuth>>> + Send + Sync>,
    pub openrouter: Arc<dyn Fn() -> BoxFuture<Result<Arc<dyn OAuthAuth>>> + Send + Sync>,
    pub kimi_coding: Arc<dyn Fn() -> BoxFuture<Result<Arc<dyn OAuthAuth>>> + Send + Sync>,
    pub meta: Arc<dyn Fn() -> BoxFuture<Result<Arc<dyn OAuthAuth>>> + Send + Sync>,
    pub xai: Arc<dyn Fn() -> BoxFuture<Result<Arc<dyn OAuthAuth>>> + Send + Sync>,
    pub radius: Arc<dyn Fn(RadiusOAuthOptions) -> BoxFuture<Result<Arc<dyn OAuthAuth>>> + Send + Sync>,
}

static BUNDLED_LOADERS: OnceLock<Mutex<Option<OAuthFlowLoaders>>> = OnceLock::new();

fn loaders_slot() -> &'static Mutex<Option<OAuthFlowLoaders>> {
    BUNDLED_LOADERS.get_or_init(|| Mutex::new(None))
}

fn current_loaders() -> Option<OAuthFlowLoaders> {
    let guard = match loaders_slot().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.clone()
}

/// Registers statically bundled OAuth flows for standalone binaries.
pub fn register_bundled_oauth_flow_loaders(loaders: OAuthFlowLoaders) {
    let mut guard = match loaders_slot().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = Some(loaders);
}

pub async fn load_anthropic_oauth() -> Result<Arc<dyn OAuthAuth>> {
    if let Some(loaders) = current_loaders() {
        return (loaders.anthropic)().await;
    }
    Ok(anthropic_oauth())
}

pub async fn load_openai_codex_oauth() -> Result<Arc<dyn OAuthAuth>> {
    if let Some(loaders) = current_loaders() {
        return (loaders.openai_codex)().await;
    }
    Ok(openai_codex_oauth())
}

pub async fn load_openai_chatgpt_oauth() -> Result<Arc<dyn OAuthAuth>> {
    if let Some(loaders) = current_loaders() {
        return (loaders.openai_chatgpt)().await;
    }
    Ok(openai_chatgpt_oauth())
}

pub async fn load_github_copilot_oauth() -> Result<Arc<dyn OAuthAuth>> {
    if let Some(loaders) = current_loaders() {
        return (loaders.github_copilot)().await;
    }
    Ok(github_copilot_oauth())
}

pub async fn load_openrouter_oauth() -> Result<Arc<dyn OAuthAuth>> {
    if let Some(loaders) = current_loaders() {
        return (loaders.openrouter)().await;
    }
    Ok(openrouter_oauth())
}

pub async fn load_kimi_coding_oauth() -> Result<Arc<dyn OAuthAuth>> {
    if let Some(loaders) = current_loaders() {
        return (loaders.kimi_coding)().await;
    }
    Ok(kimi_coding_oauth())
}

pub async fn load_meta_oauth() -> Result<Arc<dyn OAuthAuth>> {
    if let Some(loaders) = current_loaders() {
        return (loaders.meta)().await;
    }
    Ok(meta_oauth())
}

pub async fn load_xai_oauth() -> Result<Arc<dyn OAuthAuth>> {
    if let Some(loaders) = current_loaders() {
        return (loaders.xai)().await;
    }
    Ok(xai_oauth())
}

pub async fn load_radius_oauth(options: RadiusOAuthOptions) -> Result<Arc<dyn OAuthAuth>> {
    if let Some(loaders) = current_loaders() {
        return (loaders.radius)(options).await;
    }
    Ok(create_radius_oauth(options))
}
