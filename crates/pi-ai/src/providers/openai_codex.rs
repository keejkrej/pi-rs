//! Port of packages/ai/src/providers/openai-codex.ts

use std::sync::Arc;

use pi_js::Result;

use crate::api::openai_codex_responses_lazy::openai_codex_responses_api;
use crate::auth::helpers::lazy_oauth;
use crate::auth::oauth::load::load_openai_codex_oauth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider, create_provider};
use crate::providers::openai_codex_models::openai_codex_models;

pub fn openai_codex_provider() -> Result<Arc<dyn Provider + Send + Sync>> {
    create_provider(CreateProviderOptions {
        id: "openai-codex".to_string(),
        name: Some("OpenAI Codex".to_string()),
        base_url: Some("https://chatgpt.com/backend-api".to_string()),
        headers: None,
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(lazy_oauth(
                "OpenAI (ChatGPT Plus/Pro)",
                true,
                None::<&str>,
                Arc::new(|| Box::pin(load_openai_codex_oauth())),
            )),
        },
        models: openai_codex_models(),
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(openai_codex_responses_api())),
        images: None,
        classifiers: None,
    })
}
