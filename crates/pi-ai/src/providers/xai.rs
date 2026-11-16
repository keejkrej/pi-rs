//! Port of packages/ai/src/providers/xai.ts

use std::sync::Arc;

use crate::api::openai_responses_lazy::openai_responses_api;
use crate::auth::helpers::{env_api_key_auth, lazy_oauth};
use crate::auth::oauth::load::load_xai_oauth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider};
use crate::providers::all::build_provider;
use crate::providers::xai_models::xai_models;
use crate::types::AnyModel;

pub fn xai_provider() -> Arc<dyn Provider + Send + Sync> {
    build_provider(CreateProviderOptions {
        id: "xai".to_owned(),
        name: Some("xAI".to_owned()),
        base_url: Some("https://api.x.ai/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("xAI API key", &["XAI_API_KEY"])),
            oauth: Some(lazy_oauth(
                "xAI (Grok/X subscription)",
                true,
                Some("Sign in with SuperGrok or X Premium"),
                Arc::new(|| Box::pin(load_xai_oauth())),
            )),
        },
        models: xai_models().values().cloned().map(AnyModel::Chat).collect(),
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(openai_responses_api())),
        images: None,
        classifiers: None,
    })
}
