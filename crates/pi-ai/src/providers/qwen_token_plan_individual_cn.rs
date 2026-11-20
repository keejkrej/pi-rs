//! Port of packages/ai/src/providers/qwen-token-plan-individual-cn.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::qwen_token_plan_individual_cn_models::qwen_token_plan_individual_cn_models;

pub fn qwen_token_plan_individual_cn_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "qwen-token-plan-individual-cn",
        "Qwen Token Plan Individual CN",
        "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
        "Qwen Token Plan Individual CN API key",
        &["QWEN_TOKEN_PLAN_CN_API_KEY"],
        qwen_token_plan_individual_cn_models(),
        openai_completions_api(),
    )
}
