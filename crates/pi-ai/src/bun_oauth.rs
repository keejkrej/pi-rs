//! Port of packages/ai/src/bun-oauth.ts

use std::sync::Arc;

use crate::auth::oauth::anthropic::anthropic_oauth;
use crate::auth::oauth::github_copilot::github_copilot_oauth;
use crate::auth::oauth::kimi_coding::kimi_coding_oauth;
use crate::auth::oauth::load::{OAuthFlowLoaders, register_bundled_oauth_flow_loaders};
use crate::auth::oauth::meta::meta_oauth;
use crate::auth::oauth::openai_chatgpt::openai_chatgpt_oauth;
use crate::auth::oauth::openai_codex::openai_codex_oauth;
use crate::auth::oauth::openrouter::openrouter_oauth;
use crate::auth::oauth::radius::create_radius_oauth;
use crate::auth::oauth::xai::xai_oauth;

/// Register OAuth flows statically embedded in standalone binaries.
pub fn register_bun_oauth_flows() {
    register_bundled_oauth_flow_loaders(OAuthFlowLoaders {
        anthropic: Arc::new(|| Box::pin(async { Ok(anthropic_oauth()) })),
        openai_codex: Arc::new(|| Box::pin(async { Ok(openai_codex_oauth()) })),
        openai_chatgpt: Arc::new(|| Box::pin(async { Ok(openai_chatgpt_oauth()) })),
        github_copilot: Arc::new(|| Box::pin(async { Ok(github_copilot_oauth()) })),
        openrouter: Arc::new(|| Box::pin(async { Ok(openrouter_oauth()) })),
        kimi_coding: Arc::new(|| Box::pin(async { Ok(kimi_coding_oauth()) })),
        meta: Arc::new(|| Box::pin(async { Ok(meta_oauth()) })),
        xai: Arc::new(|| Box::pin(async { Ok(xai_oauth()) })),
        radius: Arc::new(|options| Box::pin(async move { Ok(create_radius_oauth(options)) })),
    });
}
