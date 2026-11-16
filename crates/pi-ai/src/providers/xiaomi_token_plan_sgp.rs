//! Port of packages/ai/src/providers/xiaomi-token-plan-sgp.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::xiaomi_token_plan_sgp_models::xiaomi_token_plan_sgp_models;

pub fn xiaomi_token_plan_sgp_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "xiaomi-token-plan-sgp",
        "Xiaomi Token Plan SGP",
        "https://token-plan-sgp.xiaomimimo.com/v1",
        "Xiaomi Token Plan SGP API key",
        &["XIAOMI_TOKEN_PLAN_SGP_API_KEY"],
        xiaomi_token_plan_sgp_models(),
        openai_completions_api(),
    )
}
