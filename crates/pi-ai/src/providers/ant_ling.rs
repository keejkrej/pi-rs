//! Port of packages/ai/src/providers/ant-ling.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::ant_ling_models::ant_ling_models;

pub fn ant_ling_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "ant-ling",
        "Ant Ling",
        "https://api.ant-ling.com/v1",
        "Ant Ling API key",
        &["ANT_LING_API_KEY"],
        ant_ling_models(),
        openai_completions_api(),
    )
}
