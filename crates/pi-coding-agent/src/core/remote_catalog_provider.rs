//! Port of packages/coding-agent/src/core/remote-catalog-provider.ts

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_ai::models::{ModelsPublication, Provider, RefreshModelsContext};
use pi_ai::models_store::ModelsStoreEntry;
use pi_ai::types::{
    AnyModel, ApiStreamOptions, AssistantImages, ClassifierContext, ClassifierModel, ClassifierOptions,
    ClassifierResult, DeferredCancelOptions, DeferredFetchOptions, DeferredHandle, ImageModel, ImagesContext,
    ImagesOptions, Model, ModelType, ProviderHeaders, SimpleStreamOptions, TranscriptContext,
};
use pi_ai::{AssistantMessageEventStream, Credential, ProviderAuth, get_model_type};
use pi_js::fetch::{Headers, Request};
use pi_js::{BoxFuture, Error, Result};
use serde_json::Value;

use crate::config::VERSION;
use crate::utils::management_http::{FetchRetryOptions, fetch_with_retry};
use crate::utils::pi_user_agent::get_pi_user_agent;

const DEFAULT_CATALOG_BASE_URL: &str = "https://pi.dev";
const REMOTE_CATALOG_ATTEMPT_TIMEOUT_MS: u64 = 4_000;
pub const REMOTE_CATALOG_REFRESH_INTERVAL_MS: i64 = 4 * 60 * 60 * 1000;

/// Model types this client can consume. Sent as `?types=` so the catalog server
/// returns the full-type shard instead of the chat-only one served to clients
/// that predate model types. A server that ignores the parameter still returns
/// the chat-only shard, which this client handles unchanged.
pub const REMOTE_CATALOG_MODEL_TYPES: &[ModelType] = &[ModelType::Chat, ModelType::Image, ModelType::Classifier];

fn is_supported_model_type(obj: &serde_json::Map<String, Value>) -> bool {
    match obj.get("type") {
        None => true,
        Some(Value::String(s)) => REMOTE_CATALOG_MODEL_TYPES.iter().any(|t| t.as_str() == s),
        Some(_) => false,
    }
}

fn merge_models(baseline: Vec<AnyModel>, dynamic: &[AnyModel]) -> Vec<AnyModel> {
    let mut merged: IndexMap<String, AnyModel> = IndexMap::new();
    for model in baseline.into_iter().chain(dynamic.iter().cloned()) {
        let key = format!("{}\0{}", get_model_type(&model).as_str(), model.id());
        merged.insert(key, model);
    }
    merged.into_values().collect()
}

fn merge_chat_models(baseline: Vec<Model>, dynamic: &[AnyModel]) -> Vec<Model> {
    let mut merged: IndexMap<String, Model> = IndexMap::new();
    for model in baseline {
        merged.insert(format!("chat\0{}", model.id), model);
    }
    for model in dynamic {
        if let Some(chat) = model.as_chat() {
            merged.insert(format!("chat\0{}", chat.id), chat.clone());
        }
    }
    merged.into_values().collect()
}

fn parse_catalog(provider_id: &str, value: Value) -> Result<Vec<AnyModel>> {
    let entries: Vec<Value> = match value {
        Value::Array(arr) => arr,
        Value::Object(mut obj) => match obj.get("models") {
            Some(Value::Array(_)) => match obj.shift_remove("models") {
                Some(Value::Array(arr)) => arr,
                _ => unreachable!(),
            },
            _ => obj.into_values().collect(),
        },
        _ => {
            return Err(Error::msg(format!(
                "Invalid model catalog for provider \"{provider_id}\""
            )));
        }
    };

    let mut out = Vec::new();
    for entry in entries {
        let Value::Object(mut map) = entry else {
            continue;
        };
        if !map.contains_key("id") || !is_supported_model_type(&map) {
            continue;
        }
        map.insert("provider".to_owned(), Value::String(provider_id.to_owned()));
        let model: AnyModel = serde_json::from_value(Value::Object(map))
            .map_err(|err| Error::msg(format!("Invalid model catalog entry for \"{provider_id}\": {err}")))?;
        out.push(model);
    }
    Ok(out)
}

fn remote_models(entry: Option<&ModelsStoreEntry>, local_generated_at: Option<i64>) -> Vec<AnyModel> {
    let Some(entry) = entry else {
        return Vec::new();
    };
    if let Some(local_ts) = local_generated_at
        && entry.last_modified.is_none_or(|lm| lm <= local_ts)
    {
        return Vec::new();
    }
    entry.models.clone()
}

fn encode_uri_component(s: &str) -> String {
    const ASCII_SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'!')
        .remove(b'~')
        .remove(b'*')
        .remove(b'\'')
        .remove(b'(')
        .remove(b')');
    percent_encoding::utf8_percent_encode(s, ASCII_SET).to_string()
}

struct RemoteCatalogProvider {
    inner: Arc<dyn Provider + Send + Sync>,
    catalog_base_url: String,
    local_generated_at: Option<i64>,
    dynamic_models: Arc<Mutex<Vec<AnyModel>>>,
}

impl Provider for RemoteCatalogProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn base_url(&self) -> Option<&str> {
        self.inner.base_url()
    }

    fn headers(&self) -> Option<&ProviderHeaders> {
        self.inner.headers()
    }

    fn auth(&self) -> &ProviderAuth {
        self.inner.auth()
    }

    fn get_models(&self) -> Vec<Model> {
        let dynamic = self
            .dynamic_models
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        merge_chat_models(self.inner.get_models(), &dynamic)
    }

    fn get_all_models(&self) -> Option<Vec<AnyModel>> {
        let baseline = self
            .inner
            .get_all_models()
            .unwrap_or_else(|| self.inner.get_models().into_iter().map(AnyModel::Chat).collect());
        let dynamic = self
            .dynamic_models
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        Some(merge_models(baseline, &dynamic))
    }

    fn refresh_models(&self, context: RefreshModelsContext) -> Option<BoxFuture<Result<()>>> {
        let provider_id = self.inner.id().to_owned();
        let catalog_base_url = self.catalog_base_url.clone();
        let local_generated_at = self.local_generated_at;
        let dynamic_models = self.dynamic_models.clone();

        Some(Box::pin(async move {
            let stored = context.stored;
            let restored: Vec<AnyModel> = remote_models(stored.as_ref(), local_generated_at)
                .into_iter()
                .filter(|m| m.provider() == provider_id)
                .collect();

            let dynamic_for_restore = dynamic_models.clone();
            let restored_clone = restored.clone();
            let restored_ok = (context.publish)(ModelsPublication {
                persist: None,
                update: Some(Arc::new(move || {
                    *dynamic_for_restore.lock().unwrap_or_else(PoisonError::into_inner) = restored_clone.clone();
                })),
            })
            .await?;
            if !restored_ok {
                return Ok(());
            }
            if !context.allow_network || context.signal.aborted() {
                return Ok(());
            }

            if !context.force.unwrap_or(false)
                && let Some(ref entry) = stored
                && let Some(checked_at) = entry.checked_at
                && entry.last_modified.is_some()
                && pi_js::time::now_ms() - checked_at < REMOTE_CATALOG_REFRESH_INTERVAL_MS
            {
                return Ok(());
            }

            // Only revalidate when a cached body backs the validator, so a 304 can never
            // leave the overlay empty.
            let validator = stored
                .as_ref()
                .filter(|entry| !entry.models.is_empty())
                .and_then(|entry| entry.etag.clone());

            let path = format!("/api/models/providers/{}", encode_uri_component(&provider_id));
            let mut url = url::Url::parse(&catalog_base_url)
                .and_then(|base| base.join(&path))
                .map_err(|err| Error::msg(format!("Invalid catalog URL: {err}")))?;
            let types_param = REMOTE_CATALOG_MODEL_TYPES
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(",");
            url.query_pairs_mut().append_pair("types", &types_param);

            let mut headers = Headers::new();
            headers.set("accept", "application/json");
            headers.set("User-Agent", &get_pi_user_agent(*VERSION));
            if let Some(ref etag) = validator {
                headers.set("if-none-match", etag);
            }

            let req = Request::get(url.to_string())
                .with_headers(headers)
                .signal(context.signal.clone());

            let response = fetch_with_retry(
                req,
                FetchRetryOptions {
                    attempt_timeout_ms: Some(REMOTE_CATALOG_ATTEMPT_TIMEOUT_MS),
                    ..Default::default()
                },
            )
            .await?;

            if context.signal.aborted() {
                return Ok(());
            }

            let checked_at = pi_js::time::now_ms();
            // Unchanged: dynamicModels already holds the stored overlay, so only the
            // freshness window moves.
            if response.status() == 304
                && let Some(mut entry) = stored
            {
                entry.checked_at = Some(checked_at);
                let _ = (context.publish)(ModelsPublication {
                    persist: Some(Some(entry)),
                    update: None,
                })
                .await?;
                return Ok(());
            }

            if response.status() == 404 || response.status() == 501 {
                let mut entry = stored.unwrap_or_default();
                entry.checked_at = Some(checked_at);
                entry.last_modified = Some(0);
                entry.etag = None;
                let _ = (context.publish)(ModelsPublication {
                    persist: Some(Some(entry)),
                    update: None,
                })
                .await?;
                return Ok(());
            }

            if !response.ok() {
                // Transient failure: the cached body and its validator stay valid, so keep the
                // etag and let the next refresh revalidate instead of downloading the catalog.
                let mut entry = stored.unwrap_or_default();
                entry.checked_at = Some(checked_at);
                let _ = (context.publish)(ModelsPublication {
                    persist: Some(Some(entry)),
                    update: None,
                })
                .await?;
                return Err(Error::msg(format!(
                    "Model catalog request failed for {provider_id}: {}",
                    response.status()
                )));
            }

            let last_modified = response
                .headers()
                .get("last-modified")
                .as_deref()
                .and_then(pi_js::time::parse_date)
                .unwrap_or(0);
            let etag = response.headers().get("etag");
            let json = response.json().await?;
            let refreshed = parse_catalog(&provider_id, json)?;
            if context.signal.aborted() {
                return Ok(());
            }

            let entry = ModelsStoreEntry {
                models: refreshed,
                last_modified: Some(last_modified),
                checked_at: Some(checked_at),
                etag,
            };
            let published = remote_models(Some(&entry), local_generated_at);
            let dynamic_for_publish = dynamic_models;
            let _ = (context.publish)(ModelsPublication {
                persist: Some(Some(entry)),
                update: Some(Arc::new(move || {
                    *dynamic_for_publish.lock().unwrap_or_else(PoisonError::into_inner) = published.clone();
                })),
            })
            .await?;
            Ok(())
        }))
    }

    fn filter_models(&self, models: &[Model], credential: Option<&Credential>) -> Option<Vec<Model>> {
        self.inner.filter_models(models, credential)
    }

    fn filter_all_models(&self, models: &[AnyModel], credential: Option<&Credential>) -> Option<Vec<AnyModel>> {
        self.inner.filter_all_models(models, credential)
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.inner.stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.inner.stream_simple(model, context, options)
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        self.inner.fetch_deferred(model, handle, options)
    }

    fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredCancelOptions>,
    ) -> Option<BoxFuture<Result<()>>> {
        self.inner.cancel_deferred(model, handle, options)
    }

    fn generate_images(
        &self,
        model: &ImageModel,
        context: &ImagesContext,
        options: Option<ImagesOptions>,
    ) -> Option<BoxFuture<AssistantImages>> {
        self.inner.generate_images(model, context, options)
    }

    fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<ClassifierOptions>,
    ) -> Option<BoxFuture<ClassifierResult>> {
        self.inner.classify(model, context, options)
    }
}

/// Add a persisted pi.dev catalog overlay to a static built-in provider.
pub fn with_remote_catalog(
    provider: Arc<dyn Provider + Send + Sync>,
    catalog_base_url: Option<&str>,
    local_generated_at: Option<i64>,
) -> Arc<dyn Provider + Send + Sync> {
    Arc::new(RemoteCatalogProvider {
        inner: provider,
        catalog_base_url: catalog_base_url.unwrap_or(DEFAULT_CATALOG_BASE_URL).to_owned(),
        local_generated_at,
        dynamic_models: Arc::new(Mutex::new(Vec::new())),
    })
}
