//! Port of packages/ai/src/providers/zai.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::zai_models::zai_models;

pub fn zai_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "zai",
        "Z.AI",
        "https://api.z.ai/api/coding/paas/v4",
        "Z.AI API key",
        &["ZAI_API_KEY"],
        zai_models(),
        openai_completions_api(),
    )
}
