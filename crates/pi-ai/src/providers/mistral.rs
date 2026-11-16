//! Port of packages/ai/src/providers/mistral.ts

use std::sync::Arc;

use crate::api::mistral_conversations_lazy::mistral_conversations_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider, create_provider};
use crate::providers::mistral_models::mistral_models;
use crate::types::{AnyModel, ApiStreamOptions, Model, ProviderStreams, SimpleStreamOptions, TranscriptContext};
use crate::utils::event_stream::AssistantMessageEventStream;

const MISTRAL_ID: &str = "mistral";
const MISTRAL_NAME: &str = "Mistral";
const MISTRAL_BASE_URL: &str = "https://api.mistral.ai";

struct MistralDirectFallback {
    auth: ProviderAuth,
    models: Vec<Model>,
    streams: ProviderStreams,
}

impl Provider for MistralDirectFallback {
    fn id(&self) -> &str {
        MISTRAL_ID
    }

    fn name(&self) -> &str {
        MISTRAL_NAME
    }

    fn base_url(&self) -> Option<&str> {
        Some(MISTRAL_BASE_URL)
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        self.models.clone()
    }

    fn get_all_models(&self) -> Option<Vec<AnyModel>> {
        Some(self.models.iter().cloned().map(AnyModel::Chat).collect())
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.streams.stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.streams.stream_simple(model, context, options)
    }
}

pub fn mistral_provider() -> Arc<dyn Provider + Send + Sync> {
    let auth = ProviderAuth {
        api_key: Some(env_api_key_auth("Mistral API key", &["MISTRAL_API_KEY"])),
        oauth: None,
    };
    let models: Vec<Model> = mistral_models().values().cloned().collect();
    let streams = mistral_conversations_api();

    match create_provider(CreateProviderOptions {
        id: MISTRAL_ID.to_owned(),
        name: Some(MISTRAL_NAME.to_owned()),
        base_url: Some(MISTRAL_BASE_URL.to_owned()),
        headers: None,
        auth: auth.clone(),
        models: models.iter().cloned().map(AnyModel::Chat).collect(),
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(streams.clone())),
        images: None,
        classifiers: None,
    }) {
        Ok(provider) => provider,
        Err(_) => Arc::new(MistralDirectFallback { auth, models, streams }),
    }
}
