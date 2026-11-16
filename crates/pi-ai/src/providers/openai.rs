//! Port of packages/ai/src/providers/openai.ts

use std::sync::Arc;

use pi_js::Result;

use crate::api::openai_responses_lazy::openai_responses_api;
use crate::auth::helpers::{env_api_key_auth, lazy_oauth};
use crate::auth::oauth::load::load_openai_chatgpt_oauth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider, create_provider};
use crate::providers::openai_models::openai_models;

pub fn openai_provider() -> Result<Arc<dyn Provider + Send + Sync>> {
    create_provider(CreateProviderOptions {
        id: "openai".to_string(),
        name: Some("OpenAI".to_string()),
        base_url: Some("https://api.openai.com/v1".to_string()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("OpenAI API key", &["OPENAI_API_KEY"])),
            oauth: Some(lazy_oauth(
                "OpenAI (ChatGPT subscription)",
                true,
                Some("Sign in with ChatGPT"),
                Arc::new(|| Box::pin(load_openai_chatgpt_oauth())),
            )),
        },
        models: openai_models(),
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(openai_responses_api())),
        images: None,
        classifiers: None,
    })
}
