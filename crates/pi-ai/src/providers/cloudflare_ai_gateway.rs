//! Port of packages/ai/src/providers/cloudflare-ai-gateway.ts

use std::sync::Arc;

use indexmap::IndexMap;

use crate::api::anthropic_messages_lazy::anthropic_messages_api;
use crate::api::openai_completions_lazy::openai_completions_api;
use crate::api::openai_responses_lazy::openai_responses_api;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider};
use crate::providers::all::build_provider;
use crate::providers::cloudflare_ai_gateway_models::cloudflare_ai_gateway_models;
use crate::providers::cloudflare_auth::cloudflare_ai_gateway_auth;
use crate::providers::cloudflare_stream::cloudflare_streams;
use crate::types::AnyModel;

pub fn cloudflare_ai_gateway_provider() -> Arc<dyn Provider + Send + Sync> {
    let models: Vec<AnyModel> = cloudflare_ai_gateway_models()
        .values()
        .cloned()
        .map(AnyModel::Chat)
        .collect();

    // Pin all three APIs: models.dev drops and restores `workers-ai/*` (openai-completions) over time.
    let mut apis = IndexMap::new();
    apis.insert(
        "anthropic-messages".to_owned(),
        cloudflare_streams(anthropic_messages_api()),
    );
    apis.insert(
        "openai-completions".to_owned(),
        cloudflare_streams(openai_completions_api()),
    );
    apis.insert(
        "openai-responses".to_owned(),
        cloudflare_streams(openai_responses_api()),
    );

    build_provider(CreateProviderOptions {
        id: "cloudflare-ai-gateway".to_owned(),
        name: Some("Cloudflare AI Gateway".to_owned()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(cloudflare_ai_gateway_auth()),
            oauth: None,
        },
        models,
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::ByApi(apis)),
        images: None,
        classifiers: None,
    })
}
