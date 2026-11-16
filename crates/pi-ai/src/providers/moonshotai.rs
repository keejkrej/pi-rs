//! Port of packages/ai/src/providers/moonshotai.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::moonshotai_models::moonshotai_models;

pub fn moonshotai_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "moonshotai",
        "Moonshot AI",
        "https://api.moonshot.ai/v1",
        "Moonshot AI API key",
        &["MOONSHOT_API_KEY"],
        moonshotai_models(),
        openai_completions_api(),
    )
}
