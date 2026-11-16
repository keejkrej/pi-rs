//! Port of packages/ai/src/providers/meta.ts

use std::sync::Arc;

use crate::api::openai_responses_lazy::openai_responses_api;
use crate::auth::helpers::{env_api_key_auth, lazy_oauth};
use crate::auth::oauth::load::load_meta_oauth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider};
use crate::providers::all::build_provider;
use crate::providers::meta_models::meta_models;
use crate::types::AnyModel;

pub fn meta_provider() -> Arc<dyn Provider + Send + Sync> {
    build_provider(CreateProviderOptions {
        id: "meta".to_owned(),
        name: Some("Meta".to_owned()),
        base_url: Some("https://api.meta.ai/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Meta Model API key", &["META_API_KEY"])),
            oauth: Some(lazy_oauth(
                "Meta (Muse subscription)",
                true,
                Some("Sign in with Meta"),
                Arc::new(|| Box::pin(load_meta_oauth())),
            )),
        },
        models: meta_models().values().cloned().map(AnyModel::Chat).collect(),
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(openai_responses_api())),
        images: None,
        classifiers: None,
    })
}
