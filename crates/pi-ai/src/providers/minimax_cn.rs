//! Port of packages/ai/src/providers/minimax-cn.ts

use std::sync::Arc;

use crate::api::anthropic_messages_lazy::anthropic_messages_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::minimax_cn_models::minimax_cn_models;

pub fn minimax_cn_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "minimax-cn",
        "MiniMax CN",
        "https://api.minimaxi.com/anthropic",
        "MiniMax CN API key",
        &["MINIMAX_CN_API_KEY"],
        minimax_cn_models(),
        anthropic_messages_api(),
    )
}
