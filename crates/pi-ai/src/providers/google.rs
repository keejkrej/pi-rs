//! Port of packages/ai/src/providers/google.ts

use std::sync::Arc;

use crate::api::google_generative_ai_lazy::google_generative_ai_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider, create_provider};
use crate::providers::google_models::google_models;
use crate::types::{AnyModel, ApiStreamOptions, Model, ProviderStreams, SimpleStreamOptions, TranscriptContext};
use crate::utils::event_stream::AssistantMessageEventStream;

const GOOGLE_ID: &str = "google";
const GOOGLE_NAME: &str = "Google";
const GOOGLE_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

struct GoogleDirectFallback {
    auth: ProviderAuth,
    models: Vec<Model>,
    streams: ProviderStreams,
}

impl Provider for GoogleDirectFallback {
    fn id(&self) -> &str {
        GOOGLE_ID
    }

    fn name(&self) -> &str {
        GOOGLE_NAME
    }

    fn base_url(&self) -> Option<&str> {
        Some(GOOGLE_BASE_URL)
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

pub fn google_provider() -> Arc<dyn Provider + Send + Sync> {
    let auth = ProviderAuth {
        api_key: Some(env_api_key_auth("Gemini API key", &["GEMINI_API_KEY"])),
        oauth: None,
    };
    let models: Vec<Model> = google_models().values().cloned().collect();
    let streams = google_generative_ai_api();

    match create_provider(CreateProviderOptions {
        id: GOOGLE_ID.to_owned(),
        name: Some(GOOGLE_NAME.to_owned()),
        base_url: Some(GOOGLE_BASE_URL.to_owned()),
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
        Err(_) => Arc::new(GoogleDirectFallback { auth, models, streams }),
    }
}
