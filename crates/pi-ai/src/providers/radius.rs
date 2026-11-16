//! Port of packages/ai/src/providers/radius.ts

use std::sync::{Arc, Mutex};

use pi_js::{BoxFuture, Result};

use crate::api::pi_messages_lazy::pi_messages_api;
use crate::auth::helpers::{env_api_key_auth, lazy_oauth};
use crate::auth::oauth::load::load_radius_oauth;
use crate::auth::oauth::radius::RadiusOAuthOptions;
use crate::auth::types::{Credential, ProviderAuth};
use crate::models::{ModelsPublication, Provider, RefreshModelsContext};
use crate::models_store::ModelsStoreEntry;
use crate::providers::radius_config::{
    DEFAULT_RADIUS_GATEWAY, get_radius_models, get_radius_models_from_config, load_radius_gateway_config,
    normalize_radius_gateway_url,
};
use crate::providers::radius_models::radius_models;
use crate::types::{AnyModel, ApiStreamOptions, Model, ProviderStreams, SimpleStreamOptions, TranscriptContext};
use crate::utils::event_stream::AssistantMessageEventStream;

#[derive(Clone, Debug, Default)]
pub struct RadiusProviderOptions {
    pub id: Option<String>,
    pub name: Option<String>,
    pub gateway: Option<String>,
}

struct RadiusProvider {
    id: String,
    name: String,
    gateway: String,
    auth: ProviderAuth,
    baseline_models: Vec<Model>,
    dynamic_models: Arc<Mutex<Vec<Model>>>,
    streams: ProviderStreams,
}

impl Provider for RadiusProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        let mut merged = self.baseline_models.clone();
        let dynamic = match self.dynamic_models.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        };
        for model in dynamic {
            if let Some(pos) = merged.iter().position(|entry| entry.id == model.id) {
                merged[pos] = model;
            } else {
                merged.push(model);
            }
        }
        merged
    }

    fn refresh_models(&self, context: RefreshModelsContext) -> Option<BoxFuture<Result<()>>> {
        let id = self.id.clone();
        let gateway = self.gateway.clone();
        let dynamic_models = Arc::clone(&self.dynamic_models);

        Some(Box::pin(async move {
            let stored = context.stored;
            if let Some(ref entry) = stored {
                let restored: Vec<Model> = entry
                    .models
                    .iter()
                    .filter_map(|model| match model {
                        AnyModel::Chat(chat) if chat.provider == id => Some(chat.clone()),
                        _ => None,
                    })
                    .collect();
                let target = Arc::clone(&dynamic_models);
                let published = (context.publish)(ModelsPublication {
                    persist: None,
                    update: Some(Arc::new(move || {
                        let mut guard = match target.lock() {
                            Ok(g) => g,
                            Err(p) => p.into_inner(),
                        };
                        *guard = restored.clone();
                    })),
                })
                .await?;
                if !published {
                    return Ok(());
                }
            }

            // Import catalogs cached by the pre-ModelsStore Radius implementation.
            if stored.is_none() {
                if let Some(Credential::OAuth(ref oauth)) = context.credential {
                    let legacy = get_radius_models(&id, Some(oauth));
                    if !legacy.is_empty() {
                        let persist_entry = ModelsStoreEntry {
                            models: legacy.iter().cloned().map(AnyModel::Chat).collect(),
                            checked_at: Some(pi_js::time::now_ms()),
                            last_modified: None,
                            etag: None,
                        };
                        let target = Arc::clone(&dynamic_models);
                        let published = (context.publish)(ModelsPublication {
                            persist: Some(Some(persist_entry)),
                            update: Some(Arc::new(move || {
                                let mut guard = match target.lock() {
                                    Ok(g) => g,
                                    Err(p) => p.into_inner(),
                                };
                                *guard = legacy.clone();
                            })),
                        })
                        .await?;
                        if !published {
                            return Ok(());
                        }
                    }
                }
            }

            if !context.allow_network || context.signal.aborted() {
                return Ok(());
            }

            let api_key = match &context.credential {
                Some(Credential::OAuth(oauth)) => Some(oauth.access.as_str()),
                Some(Credential::ApiKey(cred)) => cred.key.as_deref(),
                None => None,
            };

            let config = load_radius_gateway_config(&gateway, api_key, Some(&context.signal)).await?;
            if context.signal.aborted() {
                return Ok(());
            }

            let refreshed = get_radius_models_from_config(&id, &config);
            let persist_entry = ModelsStoreEntry {
                models: refreshed.iter().cloned().map(AnyModel::Chat).collect(),
                checked_at: Some(pi_js::time::now_ms()),
                last_modified: None,
                etag: None,
            };
            let target = Arc::clone(&dynamic_models);
            let _ = (context.publish)(ModelsPublication {
                persist: Some(Some(persist_entry)),
                update: Some(Arc::new(move || {
                    let mut guard = match target.lock() {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
                    *guard = refreshed.clone();
                })),
            })
            .await?;

            Ok(())
        }))
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        (self.streams.stream)(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        (self.streams.stream_simple)(model, context, options)
    }
}

/// Radius gateway provider with a persisted, dynamically refreshed catalog (default options).
pub fn radius_provider() -> Arc<dyn Provider + Send + Sync> {
    radius_provider_with_options(RadiusProviderOptions::default())
}

/// Radius gateway provider configured with custom id, name, or gateway URL.
pub fn radius_provider_with_options(options: RadiusProviderOptions) -> Arc<dyn Provider + Send + Sync> {
    let id = match options.id {
        Some(v) => v,
        None => "radius".to_owned(),
    };
    let name = match options.name {
        Some(v) => v,
        None => "Radius".to_owned(),
    };
    let gateway_raw = match options.gateway {
        Some(v) => v,
        None => DEFAULT_RADIUS_GATEWAY.to_owned(),
    };
    let gateway = normalize_radius_gateway_url(&gateway_raw);

    let baseline_models: Vec<Model> = if gateway == normalize_radius_gateway_url(DEFAULT_RADIUS_GATEWAY) {
        radius_models()
            .values()
            .cloned()
            .map(|mut model| {
                model.provider = id.clone();
                model
            })
            .collect()
    } else {
        Vec::new()
    };

    let dynamic_models = Arc::new(Mutex::new(get_radius_models(&id, None)));
    let streams = pi_messages_api();

    let oauth_name = name.clone();
    let oauth_gateway = gateway.clone();
    let auth = ProviderAuth {
        api_key: Some(env_api_key_auth("Radius API key", &["RADIUS_API_KEY"])),
        oauth: Some(lazy_oauth(
            name.clone(),
            false,
            Option::<&str>::None,
            Arc::new(move || {
                let opts = RadiusOAuthOptions {
                    name: oauth_name.clone(),
                    gateway: oauth_gateway.clone(),
                };
                Box::pin(load_radius_oauth(opts))
            }),
        )),
    };

    Arc::new(RadiusProvider {
        id,
        name,
        gateway,
        auth,
        baseline_models,
        dynamic_models,
        streams,
    })
}
