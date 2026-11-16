//! Port of packages/ai/src/providers/azure-openai-responses.ts

use std::sync::Arc;

use pi_js::Result;

use crate::api::azure_openai_responses_lazy::azure_openai_responses_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider, create_provider};
use crate::providers::azure_openai_responses_models::azure_openai_responses_models;

pub fn azure_openai_responses_provider() -> Result<Arc<dyn Provider + Send + Sync>> {
    create_provider(CreateProviderOptions {
        id: "azure-openai-responses".to_string(),
        name: Some("Azure OpenAI".to_string()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Azure OpenAI API key", &["AZURE_OPENAI_API_KEY"])),
            oauth: None,
        },
        models: azure_openai_responses_models(),
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(azure_openai_responses_api())),
        images: None,
        classifiers: None,
    })
}
