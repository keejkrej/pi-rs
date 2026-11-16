//! Port of packages/ai/src/providers/openrouter.ts

use std::sync::Arc;

use indexmap::IndexMap;

use crate::api::anthropic_messages_lazy::anthropic_messages_api;
use crate::api::openai_completions_lazy::openai_completions_api;
use crate::api::openrouter_images_lazy::openrouter_images_api;
use crate::api::typesafe_system_one_lazy::typesafe_system_one_api;
use crate::auth::helpers::{env_api_key_auth, lazy_oauth};
use crate::auth::oauth::load::load_openrouter_oauth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider};
use crate::providers::all::build_provider;
use crate::providers::openrouter_models::{openrouter_classifier_models, openrouter_image_models, openrouter_models};
use crate::types::AnyModel;

pub fn openrouter_provider() -> Arc<dyn Provider + Send + Sync> {
    let mut models: Vec<AnyModel> = openrouter_models().values().cloned().map(AnyModel::Chat).collect();
    models.extend(openrouter_image_models().values().cloned().map(AnyModel::Image));
    models.extend(
        openrouter_classifier_models()
            .values()
            .cloned()
            .map(AnyModel::Classifier),
    );

    let mut apis = IndexMap::new();
    apis.insert("anthropic-messages".to_owned(), anthropic_messages_api());
    apis.insert("openai-completions".to_owned(), openai_completions_api());

    let mut images = IndexMap::new();
    images.insert("openrouter-images".to_owned(), openrouter_images_api());

    let mut classifiers = IndexMap::new();
    // OpenRouter serves TypeSafe's System One protocol at /api/v1/systemone.
    classifiers.insert("typesafe-system-one".to_owned(), typesafe_system_one_api());

    build_provider(CreateProviderOptions {
        id: "openrouter".to_owned(),
        name: Some("OpenRouter".to_owned()),
        base_url: Some("https://openrouter.ai/api/v1".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("OpenRouter API key", &["OPENROUTER_API_KEY"])),
            oauth: Some(lazy_oauth(
                "OpenRouter OAuth",
                false,
                Some("Sign in with OpenRouter"),
                Arc::new(|| Box::pin(load_openrouter_oauth())),
            )),
        },
        models,
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::ByApi(apis)),
        images: Some(images),
        classifiers: Some(classifiers),
    })
}
