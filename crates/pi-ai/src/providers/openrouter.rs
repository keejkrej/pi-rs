//! Port of packages/ai/src/providers/openrouter.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_js::abort::AbortSignal;
use pi_js::fetch::{Headers, Request};
use pi_js::{Error, Result};
use serde_json::Value;

use crate::api::anthropic_messages_lazy::anthropic_messages_api;
use crate::api::openai_completions_lazy::openai_completions_api;
use crate::api::openrouter_images_lazy::openrouter_images_api;
use crate::api::typesafe_system_one_lazy::typesafe_system_one_api;
use crate::auth::helpers::{env_api_key_auth, lazy_oauth};
use crate::auth::oauth::load::load_openrouter_oauth;
use crate::auth::types::{Credential, ProviderAuth};
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider, RefreshModelsContext};
use crate::providers::all::build_provider;
use crate::providers::openrouter_api::{OpenRouterModel, open_router_cost};
use crate::providers::openrouter_models::{openrouter_classifier_models, openrouter_image_models, openrouter_models};
use crate::types::{AnyModel, ModelType};
use crate::utils::model_operations::is_model_type;

const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
const OPENROUTER_USER_MODELS_TIMEOUT_MS: u64 = 15_000;

/// Restrict an OpenRouter catalog to the chat models the key may use, with the key's limits and
/// prices from `GET /models/user`. Image and classifier models are returned unchanged. A missing
/// limit keeps the catalog value. When the endpoint rejects the key (`401`/`403`) or does not
/// exist (`404`), the catalog is returned unchanged.
pub async fn personalize_openrouter_catalog(
    catalog: &[AnyModel],
    context: &RefreshModelsContext,
) -> Result<Vec<AnyModel>> {
    let key = match context.credential.as_ref() {
        Some(Credential::OAuth(oauth)) => oauth.access.as_str(),
        Some(Credential::ApiKey(api_key)) => api_key.key.as_deref().unwrap_or(""),
        None => "",
    };
    if key.is_empty() {
        return Ok(catalog.to_vec());
    }

    let base_url = context
        .base_url
        .as_deref()
        .unwrap_or(OPENROUTER_BASE_URL)
        .trim_end_matches('/');
    let url = format!("{base_url}/models/user");

    let mut headers = Headers::new();
    headers.set("accept", "application/json");
    headers.set("authorization", &format!("Bearer {key}"));

    let signal = AbortSignal::any(&[
        context.signal.clone(),
        AbortSignal::timeout(OPENROUTER_USER_MODELS_TIMEOUT_MS),
    ]);
    let request = Request::get(url).with_headers(headers).signal(signal);
    let response = pi_js::fetch::fetch(request).await?;

    if matches!(response.status(), 401 | 403 | 404) {
        return Ok(catalog.to_vec());
    }
    if !response.ok() {
        return Err(Error::msg(format!(
            "OpenRouter model list request failed: {}",
            response.status()
        )));
    }

    let raw = response.text().await?;
    let body = pi_js::json::parse(&raw)?;
    let Some(entries) = body.as_object().and_then(|o| o.get("data")).and_then(Value::as_array) else {
        return Err(Error::msg("Invalid OpenRouter model list response"));
    };

    let mut allowed: IndexMap<String, OpenRouterModel> = IndexMap::new();
    for entry in entries {
        let Some(obj) = entry.as_object() else {
            continue;
        };
        if obj.get("id").and_then(Value::as_str).is_none() {
            continue;
        }
        if let Ok(parsed) = serde_json::from_value::<OpenRouterModel>(entry.clone()) {
            allowed.insert(parsed.id.clone(), parsed);
        }
    }

    let mut out = Vec::with_capacity(catalog.len());
    for model in catalog {
        if !is_model_type(model, ModelType::Chat) {
            out.push(model.clone());
            continue;
        }
        let AnyModel::Chat(chat) = model else {
            out.push(model.clone());
            continue;
        };
        let Some(entry) = allowed.get(&chat.id) else {
            continue;
        };
        let mut updated = chat.clone();
        updated.context_window = entry
            .top_provider
            .as_ref()
            .and_then(|tp| tp.context_length)
            .filter(|&n| n != 0)
            .or_else(|| entry.context_length.filter(|&n| n != 0))
            .unwrap_or(chat.context_window);
        updated.max_tokens = entry
            .top_provider
            .as_ref()
            .and_then(|tp| tp.max_completion_tokens)
            .filter(|&n| n != 0)
            .unwrap_or(chat.max_tokens);
        if let Some(pricing) = entry.pricing.as_ref() {
            updated.cost = open_router_cost(Some(pricing));
        }
        out.push(AnyModel::Chat(updated));
    }

    Ok(out)
}

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
        base_url: Some(OPENROUTER_BASE_URL.to_owned()),
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
