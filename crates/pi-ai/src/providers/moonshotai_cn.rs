//! Port of packages/ai/src/providers/moonshotai-cn.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::moonshotai_cn_models::moonshotai_cn_models;

pub fn moonshotai_cn_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "moonshotai-cn",
        "Moonshot AI CN",
        "https://api.moonshot.cn/v1",
        "Moonshot AI API key",
        &["MOONSHOT_API_KEY"],
        moonshotai_cn_models(),
        openai_completions_api(),
    )
}
