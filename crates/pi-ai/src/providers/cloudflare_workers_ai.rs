//! Port of packages/ai/src/providers/cloudflare-workers-ai.ts

use std::sync::Arc;

use indexmap::IndexMap;

use crate::api::cloudflare_workers_ai_system_one_lazy::cloudflare_workers_ai_system_one_api;
use crate::api::openai_completions_lazy::openai_completions_api;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider};
use crate::providers::all::build_provider;
use crate::providers::cloudflare_auth::cloudflare_workers_ai_auth;
use crate::providers::cloudflare_stream::{cloudflare_classifier, cloudflare_streams};
use crate::providers::cloudflare_workers_ai_models::{
    cloudflare_workers_ai_classifier_models, cloudflare_workers_ai_models,
};
use crate::types::AnyModel;

pub fn cloudflare_workers_ai_provider() -> Arc<dyn Provider + Send + Sync> {
    let mut models: Vec<AnyModel> = cloudflare_workers_ai_models()
        .values()
        .cloned()
        .map(AnyModel::Chat)
        .collect();
    models.extend(
        cloudflare_workers_ai_classifier_models()
            .values()
            .cloned()
            .map(AnyModel::Classifier),
    );

    let mut classifiers = IndexMap::new();
    classifiers.insert(
        "cloudflare-workers-ai-system-one".to_owned(),
        cloudflare_classifier(cloudflare_workers_ai_system_one_api()),
    );

    build_provider(CreateProviderOptions {
        id: "cloudflare-workers-ai".to_owned(),
        name: Some("Cloudflare Workers AI".to_owned()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(cloudflare_workers_ai_auth()),
            oauth: None,
        },
        models,
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(cloudflare_streams(openai_completions_api()))),
        images: None,
        classifiers: Some(classifiers),
    })
}
