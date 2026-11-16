//! Port of packages/ai/src/providers/opencode.ts

use std::sync::Arc;

use indexmap::IndexMap;

use crate::api::anthropic_messages_lazy::anthropic_messages_api;
use crate::api::google_generative_ai_lazy::google_generative_ai_api;
use crate::api::openai_completions_lazy::openai_completions_api;
use crate::api::openai_responses_lazy::openai_responses_api;
use crate::api::typesafe_system_one_lazy::typesafe_system_one_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider};
use crate::providers::all::build_provider;
use crate::providers::opencode_headers::with_open_code_session_header;
use crate::providers::opencode_models::{opencode_classifier_models, opencode_models};
use crate::types::AnyModel;

pub fn opencode_provider() -> Arc<dyn Provider + Send + Sync> {
    let mut models: Vec<AnyModel> = opencode_models().values().cloned().map(AnyModel::Chat).collect();
    models.extend(opencode_classifier_models().values().cloned().map(AnyModel::Classifier));

    let mut apis = IndexMap::new();
    apis.insert(
        "anthropic-messages".to_owned(),
        with_open_code_session_header(anthropic_messages_api()),
    );
    apis.insert(
        "google-generative-ai".to_owned(),
        with_open_code_session_header(google_generative_ai_api()),
    );
    apis.insert(
        "openai-completions".to_owned(),
        with_open_code_session_header(openai_completions_api()),
    );
    apis.insert(
        "openai-responses".to_owned(),
        with_open_code_session_header(openai_responses_api()),
    );

    let mut classifiers = IndexMap::new();
    classifiers.insert("typesafe-system-one".to_owned(), typesafe_system_one_api());

    build_provider(CreateProviderOptions {
        id: "opencode".to_owned(),
        name: Some("OpenCode Zen".to_owned()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("OpenCode API key", &["OPENCODE_API_KEY"])),
            oauth: None,
        },
        models,
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::ByApi(apis)),
        images: None,
        classifiers: Some(classifiers),
    })
}
