//! Port of packages/ai/src/providers/fireworks.ts

use std::sync::Arc;

use indexmap::IndexMap;

use crate::api::anthropic_messages_lazy::anthropic_messages_api;
use crate::api::openai_completions_lazy::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider};
use crate::providers::all::build_provider;
use crate::providers::fireworks_models::fireworks_models;
use crate::types::AnyModel;

pub fn fireworks_provider() -> Arc<dyn Provider + Send + Sync> {
    let mut apis = IndexMap::new();
    apis.insert("anthropic-messages".to_owned(), anthropic_messages_api());
    apis.insert("openai-completions".to_owned(), openai_completions_api());

    build_provider(CreateProviderOptions {
        id: "fireworks".to_owned(),
        name: Some("Fireworks".to_owned()),
        base_url: Some("https://api.fireworks.ai/inference".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Fireworks API key", &["FIREWORKS_API_KEY"])),
            oauth: None,
        },
        models: fireworks_models().values().cloned().map(AnyModel::Chat).collect(),
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::ByApi(apis)),
        images: None,
        classifiers: None,
    })
}
