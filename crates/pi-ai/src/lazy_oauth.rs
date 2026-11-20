//! Port of packages/ai/src/lazy-oauth.ts

use std::sync::Arc;

use crate::auth::oauth::load::{OAuthFlowLoaders, register_bundled_oauth_flow_loaders};

/// Registers all bundled OAuth flow loaders (`anthropic`, `openai-codex`, `openai-chatgpt`,
/// `github-copilot`, `openrouter`, `kimi-coding`, `meta`, `xai`, `radius`).
pub fn register_lazy_oauth_flows() {
    register_bundled_oauth_flow_loaders(OAuthFlowLoaders {
        anthropic: Arc::new(|| Box::pin(async { Ok(crate::auth::oauth::anthropic::anthropic_oauth()) })),
        openai_codex: Arc::new(|| Box::pin(async { Ok(crate::auth::oauth::openai_codex::openai_codex_oauth()) })),
        openai_chatgpt: Arc::new(|| Box::pin(async { Ok(crate::auth::oauth::openai_chatgpt::openai_chatgpt_oauth()) })),
        github_copilot: Arc::new(|| Box::pin(async { Ok(crate::auth::oauth::github_copilot::github_copilot_oauth()) })),
        openrouter: Arc::new(|| Box::pin(async { Ok(crate::auth::oauth::openrouter::openrouter_oauth()) })),
        kimi_coding: Arc::new(|| Box::pin(async { Ok(crate::auth::oauth::kimi_coding::kimi_coding_oauth()) })),
        meta: Arc::new(|| Box::pin(async { Ok(crate::auth::oauth::meta::meta_oauth()) })),
        xai: Arc::new(|| Box::pin(async { Ok(crate::auth::oauth::xai::xai_oauth()) })),
        radius: Arc::new(|options| {
            Box::pin(async move { Ok(crate::auth::oauth::radius::create_radius_oauth(options)) })
        }),
    });
}
