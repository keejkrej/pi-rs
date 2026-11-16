//! Port of packages/ai/src/providers/xiaomi-token-plan-cn.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::xiaomi_token_plan_cn_models::xiaomi_token_plan_cn_models;

pub fn xiaomi_token_plan_cn_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "xiaomi-token-plan-cn",
        "Xiaomi Token Plan CN",
        "https://token-plan-cn.xiaomimimo.com/v1",
        "Xiaomi Token Plan CN API key",
        &["XIAOMI_TOKEN_PLAN_CN_API_KEY"],
        xiaomi_token_plan_cn_models(),
        openai_completions_api(),
    )
}
