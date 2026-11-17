//! Port of packages/ai/src/providers/cerebras.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::cerebras_models::cerebras_models;

pub fn cerebras_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "cerebras",
        "Cerebras",
        "https://api.cerebras.ai/v1",
        "Cerebras API key",
        &["CEREBRAS_API_KEY"],
        cerebras_models(),
        openai_completions_api(),
    )
}
