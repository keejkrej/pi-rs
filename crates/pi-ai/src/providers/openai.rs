//! Port of packages/ai/src/providers/openai.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_js::Result;

use crate::api::openai_decisions_lazy::openai_decisions_api;
use crate::api::openai_responses_lazy::openai_responses_api;
use crate::auth::helpers::{env_api_key_auth, lazy_oauth};
use crate::auth::oauth::load::load_openai_chatgpt_oauth;
use crate::auth::types::{Credential, ProviderAuth};
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider, create_provider};
use crate::providers::openai_models::{OPENAI_CLASSIFIER_MODELS, openai_models};
use crate::types::{AnyModel, ModelType};
use crate::utils::model_operations::is_model_type;

pub fn openai_provider() -> Result<Arc<dyn Provider + Send + Sync>> {
    let mut models = openai_models();
    models.extend(OPENAI_CLASSIFIER_MODELS.values().cloned().map(AnyModel::Classifier));

    let mut classifiers = IndexMap::new();
    classifiers.insert("openai-decisions".to_owned(), openai_decisions_api());

    create_provider(CreateProviderOptions {
        id: "openai".to_string(),
        name: Some("OpenAI".to_string()),
        base_url: Some("https://api.openai.com/v1".to_string()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("OpenAI API key", &["OPENAI_API_KEY"])),
            oauth: Some(lazy_oauth(
                "OpenAI (ChatGPT subscription)",
                true,
                Some("Sign in with ChatGPT"),
                Arc::new(|| Box::pin(load_openai_chatgpt_oauth())),
            )),
        },
        models,
        fetch_models: None,
        filter_models: None,
        // Sign in with ChatGPT tokens only reach the Responses API; the Decisions API rejects them.
        filter_all_models: Some(Arc::new(|all_models: &[AnyModel], credential: Option<&Credential>| {
            if matches!(credential, Some(Credential::OAuth(_))) {
                all_models
                    .iter()
                    .filter(|model| !is_model_type(model, ModelType::Classifier))
                    .cloned()
                    .collect()
            } else {
                all_models.to_vec()
            }
        })),
        api: Some(CreateProviderApi::Single(openai_responses_api())),
        images: None,
        classifiers: Some(classifiers),
    })
}
