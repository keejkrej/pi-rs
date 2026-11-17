//! Port of packages/ai/src/providers/vercel-ai-gateway.ts

use std::sync::Arc;

use indexmap::IndexMap;

use crate::api::anthropic_messages_lazy::anthropic_messages_api;
use crate::api::typesafe_system_one_lazy::typesafe_system_one_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider};
use crate::providers::all::build_provider;
use crate::providers::vercel_ai_gateway_models::{vercel_ai_gateway_classifier_models, vercel_ai_gateway_models};
use crate::types::AnyModel;

pub fn vercel_ai_gateway_provider() -> Arc<dyn Provider + Send + Sync> {
    let mut models: Vec<AnyModel> = vercel_ai_gateway_models()
        .values()
        .cloned()
        .map(AnyModel::Chat)
        .collect();
    models.extend(
        vercel_ai_gateway_classifier_models()
            .values()
            .cloned()
            .map(AnyModel::Classifier),
    );

    let mut classifiers = IndexMap::new();
    classifiers.insert("typesafe-system-one".to_owned(), typesafe_system_one_api());

    build_provider(CreateProviderOptions {
        id: "vercel-ai-gateway".to_owned(),
        name: Some("Vercel AI Gateway".to_owned()),
        base_url: Some("https://ai-gateway.vercel.sh".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Vercel AI Gateway API key", &["AI_GATEWAY_API_KEY"])),
            oauth: None,
        },
        models,
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(anthropic_messages_api())),
        images: None,
        classifiers: Some(classifiers),
    })
}
