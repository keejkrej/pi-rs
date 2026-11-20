//! Port of packages/ai/src/providers/kimi-coding.ts

use std::sync::Arc;

use crate::api::openai_completions_lazy::openai_completions_api;
use crate::auth::helpers::{env_api_key_auth, lazy_oauth};
use crate::auth::oauth::load::load_kimi_coding_oauth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider};
use crate::providers::all::build_provider;
use crate::providers::kimi_coding_models::kimi_coding_models;
use crate::types::AnyModel;

pub fn kimi_coding_provider() -> Arc<dyn Provider + Send + Sync> {
    build_provider(CreateProviderOptions {
        id: "kimi-coding".to_owned(),
        name: Some("Kimi For Coding".to_owned()),
        base_url: Some("https://api.kimi.com/coding/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Kimi API key", &["KIMI_API_KEY"])),
            oauth: Some(lazy_oauth(
                "Kimi Code (subscription)",
                true,
                Some("Sign in with Kimi Code"),
                Arc::new(|| Box::pin(load_kimi_coding_oauth())),
            )),
        },
        models: kimi_coding_models().values().cloned().map(AnyModel::Chat).collect(),
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(openai_completions_api())),
        images: None,
        classifiers: None,
    })
}
