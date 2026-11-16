//! Port of packages/ai/src/providers/qwen-token-plan-cn.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::qwen_token_plan_cn_models::qwen_token_plan_cn_models;

pub fn qwen_token_plan_cn_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "qwen-token-plan-cn",
        "Qwen Token Plan CN",
        "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
        "Qwen Token Plan CN API key",
        &["QWEN_TOKEN_PLAN_CN_API_KEY"],
        qwen_token_plan_cn_models(),
        openai_completions_api(),
    )
}
