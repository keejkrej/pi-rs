//! Port of packages/ai/src/providers/huggingface.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::huggingface_models::huggingface_models;

pub fn huggingface_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "huggingface",
        "Hugging Face",
        "https://router.huggingface.co/v1",
        "Hugging Face token",
        &["HF_TOKEN"],
        huggingface_models(),
        openai_completions_api(),
    )
}
