//! Port of packages/ai/src/providers/together.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::together_models::together_models;

pub fn together_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "together",
        "Together",
        "https://api.together.ai/v1",
        "Together API key",
        &["TOGETHER_API_KEY"],
        together_models(),
        openai_completions_api(),
    )
}
