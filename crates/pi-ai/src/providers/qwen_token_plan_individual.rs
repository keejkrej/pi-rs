//! Port of packages/ai/src/providers/qwen-token-plan-individual.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::qwen_token_plan_individual_models::qwen_token_plan_individual_models;

pub fn qwen_token_plan_individual_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "qwen-token-plan-individual",
        "Qwen Token Plan Individual",
        "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
        "Qwen Token Plan Individual API key",
        &["QWEN_TOKEN_PLAN_API_KEY"],
        qwen_token_plan_individual_models(),
        openai_completions_api(),
    )
}
