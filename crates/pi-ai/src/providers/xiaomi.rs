//! Port of packages/ai/src/providers/xiaomi.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::xiaomi_models::xiaomi_models;

pub fn xiaomi_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "xiaomi",
        "Xiaomi",
        "https://api.xiaomimimo.com/v1",
        "Xiaomi API key",
        &["XIAOMI_API_KEY"],
        xiaomi_models(),
        openai_completions_api(),
    )
}
