//! Port of packages/ai/src/providers/groq.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::models::Provider;
use crate::providers::all::build_env_provider;
use crate::providers::groq_models::groq_models;

pub fn groq_provider() -> Arc<dyn Provider + Send + Sync> {
    build_env_provider(
        "groq",
        "Groq",
        "https://api.groq.com/openai/v1",
        "Groq API key",
        &["GROQ_API_KEY"],
        groq_models(),
        openai_completions_api(),
    )
}
