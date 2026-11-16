//! Port of packages/ai/src/providers/github-copilot.ts

use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};
use serde_json::Value;

use crate::api::anthropic_messages_lazy::anthropic_messages_api;
use crate::api::openai_completions_lazy::openai_completions_api;
use crate::api::openai_responses_lazy::openai_responses_api;
use crate::auth::helpers::{env_api_key_auth, lazy_oauth};
use crate::auth::oauth::load::load_github_copilot_oauth;
use crate::auth::types::{Credential, ProviderAuth};
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider};
use crate::providers::all::build_provider;
use crate::providers::github_copilot_models::github_copilot_models;
use crate::types::{AnyModel, Model};

pub fn github_copilot_provider() -> Arc<dyn Provider + Send + Sync> {
    let models: Vec<AnyModel> = github_copilot_models().values().cloned().map(AnyModel::Chat).collect();

    let mut apis = IndexMap::new();
    apis.insert("anthropic-messages".to_owned(), anthropic_messages_api());
    apis.insert("openai-completions".to_owned(), openai_completions_api());
    apis.insert("openai-responses".to_owned(), openai_responses_api());

    build_provider(CreateProviderOptions {
        id: "github-copilot".to_owned(),
        name: Some("GitHub Copilot".to_owned()),
        base_url: Some("https://api.individual.githubcopilot.com".to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("GitHub Copilot token", &["COPILOT_GITHUB_TOKEN"])),
            oauth: Some(lazy_oauth(
                "GitHub Copilot",
                true,
                Option::<&str>::None,
                Arc::new(|| Box::pin(load_github_copilot_oauth())),
            )),
        },
        models,
        fetch_models: None,
        filter_models: Some(Arc::new(|models: &[Model], credential: Option<&Credential>| {
            let Some(Credential::OAuth(oauth)) = credential else {
                return models.to_vec();
            };
            let Some(Value::Array(items)) = oauth.extra.get("availableModelIds") else {
                return models.to_vec();
            };
            let mut available = IndexSet::with_capacity(items.len());
            for item in items {
                let Some(id) = item.as_str() else {
                    return models.to_vec();
                };
                available.insert(id);
            }
            models
                .iter()
                .filter(|model| available.contains(model.id.as_str()))
                .cloned()
                .collect()
        })),
        filter_all_models: None,
        api: Some(CreateProviderApi::ByApi(apis)),
        images: None,
        classifiers: None,
    })
}
