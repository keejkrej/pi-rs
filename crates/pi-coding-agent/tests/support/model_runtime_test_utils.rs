//! Port of packages/coding-agent/test/model-runtime-test-utils.ts

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_ai::{Credential, CredentialStore, Model, ModelCost, ModelInput, ModelInputLimits};
use pi_coding_agent::core::resolve_config_value::{resolve_config_value, resolve_headers};
use pi_js::Result;
use serde_json::Value;

// WIRE(@mcp-helper2): replace adapter ModelRegistry/ModelRuntime/InMemoryCodingAgentModelsStore with
// `pi_coding_agent::core::{model_registry::ModelRegistry, model_runtime::ModelRuntime, models_store::InMemoryCodingAgentModelsStore}`
// once `core-models-pkg` lands.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiKeyAndHeaders {
    pub ok: bool,
    pub api_key: Option<String>,
    pub headers: Option<IndexMap<String, String>>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ProviderModelDefinition {
    pub id: String,
    pub name: String,
    pub api: String,
    pub reasoning: bool,
    pub input: Vec<ModelInput>,
    pub input_limits: Option<ModelInputLimits>,
    pub cost: ModelCost,
    pub context_window: i64,
    pub max_tokens: i64,
    pub base_url: String,
}

#[derive(Clone, Debug)]
pub struct ProviderRegistrationConfig {
    pub base_url: String,
    pub api: String,
    pub models: Vec<ProviderModelDefinition>,
}

#[derive(Clone, Default)]
pub struct InMemoryCodingAgentModelsStore {
    providers: Arc<Mutex<IndexMap<String, ProviderRegistrationConfig>>>,
}

impl InMemoryCodingAgentModelsStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Clone)]
pub struct ModelRuntime {
    pub credentials: Arc<dyn CredentialStore>,
    pub models_path: Option<String>,
    pub models_store: InMemoryCodingAgentModelsStore,
    pub allow_model_network: bool,
    registered_models: Arc<Mutex<Vec<Model>>>,
}

impl ModelRuntime {
    pub async fn create(
        credentials: Arc<dyn CredentialStore>,
        models_path: Option<String>,
        models_store: Option<InMemoryCodingAgentModelsStore>,
        allow_model_network: bool,
    ) -> Result<Self> {
        Ok(Self {
            credentials,
            models_path,
            models_store: models_store.unwrap_or_default(),
            allow_model_network,
            registered_models: Arc::new(Mutex::new(Vec::new())),
        })
    }

    pub fn registered_models(&self) -> Vec<Model> {
        self.registered_models
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[derive(Clone)]
pub struct ModelRegistry {
    runtime: ModelRuntime,
    load_error: Option<String>,
    provider_api_keys: Arc<Mutex<IndexMap<String, String>>>,
}

impl ModelRegistry {
    pub fn new(runtime: ModelRuntime) -> Self {
        let mut load_error = None;
        let provider_api_keys = Arc::new(Mutex::new(IndexMap::new()));
        if let Some(ref path) = runtime.models_path
            && pi_js::fs::exists(path)
        {
            match pi_js::fs::read_to_string(path).and_then(|raw| pi_js::json::parse(&raw)) {
                Ok(Value::Object(root)) => {
                    if let Some(Value::Object(providers)) = root.get("providers") {
                        let mut models = runtime.registered_models.lock().unwrap_or_else(PoisonError::into_inner);
                        let mut keys = provider_api_keys.lock().unwrap_or_else(PoisonError::into_inner);
                        for (provider_name, pval) in providers {
                            let Some(pobj) = pval.as_object() else { continue };
                            if let Some(ak) = pobj.get("apiKey").and_then(Value::as_str) {
                                keys.insert(provider_name.clone(), ak.to_owned());
                            }
                            let base_url = pobj
                                .get("baseUrl")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned();
                            let default_api = pobj
                                .get("api")
                                .and_then(Value::as_str)
                                .unwrap_or("openai-completions")
                                .to_owned();
                            let prov_headers = pobj.get("headers").and_then(Value::as_object).map(|h| {
                                h.iter()
                                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                                    .collect::<IndexMap<String, String>>()
                            });
                            if let Some(Value::Array(m_arr)) = pobj.get("models") {
                                for mval in m_arr {
                                    let Some(mobj) = mval.as_object() else { continue };
                                    let Some(id) = mobj.get("id").and_then(Value::as_str) else {
                                        continue;
                                    };
                                    let name = mobj.get("name").and_then(Value::as_str).unwrap_or(id).to_owned();
                                    let api = mobj
                                        .get("api")
                                        .and_then(Value::as_str)
                                        .unwrap_or(&default_api)
                                        .to_owned();
                                    let model_headers = mobj.get("headers").and_then(Value::as_object).map(|h| {
                                        h.iter()
                                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                                            .collect::<IndexMap<String, String>>()
                                    });
                                    let merged_headers = match (prov_headers.as_ref(), model_headers.as_ref()) {
                                        (None, None) => None,
                                        (Some(ph), None) => Some(ph.clone()),
                                        (None, Some(mh)) => Some(mh.clone()),
                                        (Some(ph), Some(mh)) => {
                                            let mut m = ph.clone();
                                            for (k, v) in mh {
                                                m.insert(k.clone(), v.clone());
                                            }
                                            Some(m)
                                        }
                                    };
                                    models.push(Model {
                                        r#type: None,
                                        id: id.to_owned(),
                                        name,
                                        api,
                                        provider: provider_name.clone(),
                                        base_url: base_url.clone(),
                                        reasoning: mobj.get("reasoning").and_then(Value::as_bool).unwrap_or(false),
                                        thinking_level_map: None,
                                        prompt_cache: None,
                                        input: vec![ModelInput::Text],
                                        input_limits: None,
                                        cost: ModelCost::default(),
                                        context_window: mobj
                                            .get("contextWindow")
                                            .and_then(Value::as_i64)
                                            .unwrap_or(128_000),
                                        max_tokens: mobj.get("maxTokens").and_then(Value::as_i64).unwrap_or(16_384),
                                        headers: merged_headers,
                                        sampling_params: None,
                                        compat: None,
                                    });
                                }
                            }
                        }
                    }
                }
                Ok(_) => {
                    load_error = Some(format!(
                        "Failed to parse models.json: root must be a JSON object\nFile: {path}"
                    ));
                }
                Err(err) => {
                    load_error = Some(format!("Failed to parse models.json: {err}\nFile: {path}"));
                }
            }
        }
        Self {
            runtime,
            load_error,
            provider_api_keys,
        }
    }

    pub fn get_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn find(&self, provider: &str, model_id: &str) -> Option<Model> {
        self.runtime
            .registered_models()
            .into_iter()
            .find(|m| m.provider == provider && m.id == model_id)
    }

    pub async fn get_api_key_for_provider(&self, provider: &str) -> Option<String> {
        if let Ok(Some(cred)) = self.runtime.credentials.read(provider, None).await {
            match cred {
                Credential::ApiKey(k) => {
                    if let Some(raw) = k.key
                        && let Some(resolved) = resolve_config_value(&raw, None)
                    {
                        return Some(resolved);
                    }
                }
                Credential::OAuth(o) => return Some(o.access),
            }
        }
        let configured = self
            .provider_api_keys
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(provider)
            .cloned();
        configured.and_then(|raw| resolve_config_value(&raw, None))
    }

    pub async fn get_api_key_and_headers(&self, model: &Model) -> Result<ApiKeyAndHeaders> {
        let api_key = self.get_api_key_for_provider(&model.provider).await;
        let headers = resolve_headers(model.headers.as_ref(), None);
        Ok(ApiKeyAndHeaders {
            ok: true,
            api_key,
            headers,
            error: None,
        })
    }

    pub fn register_provider(&self, provider: &str, config: ProviderRegistrationConfig) {
        {
            let mut providers = self
                .runtime
                .models_store
                .providers
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            providers.insert(provider.to_owned(), config.clone());
        }
        let mut models = self
            .runtime
            .registered_models
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        models.retain(|m| m.provider != provider);
        for def in config.models {
            models.push(Model {
                r#type: None,
                id: def.id,
                name: def.name,
                api: def.api,
                provider: provider.to_owned(),
                base_url: def.base_url,
                reasoning: def.reasoning,
                thinking_level_map: None,
                prompt_cache: None,
                input: def.input,
                input_limits: def.input_limits,
                cost: def.cost,
                context_window: def.context_window,
                max_tokens: def.max_tokens,
                headers: None,
                sampling_params: None,
                compat: None,
            });
        }
    }

    pub fn runtime(&self) -> &ModelRuntime {
        &self.runtime
    }
}

/// Load optional models.json configuration without introducing file-backed catalog locks into unit tests.
pub async fn create_model_registry(
    credentials: Arc<dyn CredentialStore>,
    models_path: Option<&str>,
) -> Result<ModelRegistry> {
    let runtime = ModelRuntime::create(
        credentials,
        models_path.map(str::to_owned),
        Some(InMemoryCodingAgentModelsStore::new()),
        false,
    )
    .await?;
    Ok(ModelRegistry::new(runtime))
}

pub async fn create_in_memory_model_registry(credentials: Arc<dyn CredentialStore>) -> Result<ModelRegistry> {
    let runtime = ModelRuntime::create(credentials, None, None, false).await?;
    Ok(ModelRegistry::new(runtime))
}

pub fn get_model_runtime(model_registry: &ModelRegistry) -> ModelRuntime {
    model_registry.runtime().clone()
}
