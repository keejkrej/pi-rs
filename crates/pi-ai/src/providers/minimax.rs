//! Port of packages/ai/src/providers/minimax.ts

use std::sync::Arc;

use crate::api::anthropic_messages_lazy::anthropic_messages_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::minimax_models::minimax_models;

pub fn minimax_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "minimax",
        "MiniMax",
        "https://api.minimax.io/anthropic",
        "MiniMax API key",
        &["MINIMAX_API_KEY"],
        minimax_models(),
        anthropic_messages_api(),
    )
}
