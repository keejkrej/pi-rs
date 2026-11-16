//! Port of packages/ai/src/providers/xiaomi-token-plan-ams.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::xiaomi_token_plan_ams_models::xiaomi_token_plan_ams_models;

pub fn xiaomi_token_plan_ams_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "xiaomi-token-plan-ams",
        "Xiaomi Token Plan AMS",
        "https://token-plan-ams.xiaomimimo.com/v1",
        "Xiaomi Token Plan AMS API key",
        &["XIAOMI_TOKEN_PLAN_AMS_API_KEY"],
        xiaomi_token_plan_ams_models(),
        openai_completions_api(),
    )
}
