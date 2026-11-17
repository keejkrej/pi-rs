//! Port of packages/ai/src/providers/nvidia.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::nvidia_models::nvidia_models;

pub fn nvidia_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "nvidia",
        "NVIDIA",
        "https://integrate.api.nvidia.com/v1",
        "NVIDIA API key",
        &["NVIDIA_API_KEY"],
        nvidia_models(),
        openai_completions_api(),
    )
}
