//! Port of packages/ai/src/providers/baseten.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::baseten_models::baseten_models;

pub fn baseten_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "baseten",
        "Baseten",
        "https://inference.baseten.co/v1",
        "Baseten API key",
        &["BASETEN_API_KEY"],
        baseten_models(),
        openai_completions_api(),
    )
}
