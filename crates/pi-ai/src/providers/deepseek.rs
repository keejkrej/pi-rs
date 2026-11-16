//! Port of packages/ai/src/providers/deepseek.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::deepseek_models::deepseek_models;

pub fn deepseek_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "deepseek",
        "DeepSeek",
        "https://api.deepseek.com",
        "DeepSeek API key",
        &["DEEPSEEK_API_KEY"],
        deepseek_models(),
        openai_completions_api(),
    )
}
