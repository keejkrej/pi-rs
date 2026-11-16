//! Port of packages/ai/src/providers/zai-coding-cn.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::zai_coding_cn_models::zai_coding_cn_models;

pub fn zai_coding_cn_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "zai-coding-cn",
        "Z.AI Coding CN",
        "https://open.bigmodel.cn/api/coding/paas/v4",
        "Z.AI Coding CN API key",
        &["ZAI_CODING_CN_API_KEY"],
        zai_coding_cn_models(),
        openai_completions_api(),
    )
}
