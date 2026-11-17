//! Port of packages/coding-agent/src/core/model-runtime.ts

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::{IndexMap, IndexSet};
use pi_ai::api::lazy::lazy_stream;
use pi_ai::auth::resolve::AuthResolutionOverrides;
use pi_ai::models::{
    CreateModelsOptions, ModelsApiStreamOptions, ModelsClassifierOptions, ModelsDeferredCancelOptions,
    ModelsDeferredFetchOptions, ModelsImagesOptions, ModelsRefreshOptions, ModelsRefreshResult,
    ModelsSimpleStreamOptions, MutableModels, Provider, create_models,
};
use pi_ai::models_store::ModelsStore;
use pi_ai::providers::all::{builtin_providers, get_builtin_model_data_generated_at};
use pi_ai::providers::radius::{RadiusProviderOptions, radius_provider_with_options};
use pi_ai::types::{
    AnyModel, AssistantImages, ClassifierContext, ClassifierModel, ClassifierResult, Context, DeferredHandle,
    ImageModel, ImagesContext, Message, Model, ModelThinkingLevel, ModelType, ProviderEnv, ProviderHeaders,
    SimpleStreamOptions, TranscriptContext,
};
use pi_ai::utils::model_operations::{
    assert_chat_model, assert_classifier_model, assert_image_model, classifier_error_result, image_error_result,
};
use pi_ai::utils::models_error::{ModelsError, ModelsErrorCode};
use pi_ai::{
    AssistantMessage, AssistantMessageEventStream, AuthCheck, AuthInteraction, AuthOperationOptions, AuthResult,
    AuthType, Credential, CredentialInfo, CredentialStore, LoginOptions, clamp_thinking_level, normalize_context,
};
use pi_js::abort::{AbortController, AbortReason, AbortSignal};
use pi_js::{Error, Result};

use crate::config::get_agent_dir;
use crate::core::auth_storage::AuthStorage as DefaultAuthStorage;
use crate::core::model_config::ModelConfig;
use crate::core::models_store::{FileModelsStore, InMemoryCodingAgentModelsStore};
use crate::core::provider_composer::{
    AuthStatus, AuthStatusSource, CompatibilityRequestConfig, ProviderConfigInput, compose_model_provider,
    configured_request_auth_status, resolve_compatibility_request_config, resolve_configured_model_headers,
    validate_extension_provider,
};
use crate::core::remote_catalog_provider::with_remote_catalog;
use crate::core::runtime_credentials::RuntimeCredentials;
use crate::core::virtual_models::{
    ModelRoute, ModelRouteFailed, ModelRoutePrevious, ModelRouteReason, ModelRouteRequest, VirtualModelDefinition,
    VirtualModelDescriptor, VirtualModelRouter, create_virtual_model, is_virtual_model, with_virtual_models,
};
use crate::utils::abort::operation_signal;
use crate::utils::paths::normalize_path;

#[derive(Clone)]
struct RegisteredVirtualModel {
    model: Model,
    router: Arc<dyn VirtualModelRouter>,
}

#[derive(Clone, Default)]
struct ModelRuntimeSnapshot {
    all: Vec<Model>,
    available: Vec<Model>,
    configured_providers: IndexSet<String>,
    stored_providers: IndexSet<String>,
    auth: IndexMap<String, Option<AuthCheck>>,
}

#[derive(Clone, Default)]
pub struct CreateModelRuntimeOptions {
    pub credentials: Option<Arc<dyn CredentialStore + Send + Sync>>,
    pub auth_path: Option<String>,
    /// `None` defaults to `<agentDir>/models.json`; `Some(None)` disables loading `models.json`.
    pub models_path: Option<Option<String>>,
    pub models_store: Option<Arc<dyn ModelsStore + Send + Sync>>,
    pub models_store_path: Option<String>,
    /// Allow `create()` to refresh model catalogs over the network. Defaults to `false`.
    pub allow_model_network: Option<bool>,
    pub model_refresh_timeout_ms: Option<u64>,
    pub catalog_base_url: Option<String>,
    pub signal: Option<AbortSignal>,
    /// Skip initial catalog and availability refresh. Static models remain available.
    pub refresh_on_create: Option<bool>,
}

/// Alias matching camelCase-to-PascalCase expectation (`ModelRuntimeCreateOptions`).
pub type ModelRuntimeCreateOptions = CreateModelRuntimeOptions;

#[derive(Clone, Debug, Default)]
pub struct ModelRuntimeAuthOverrides {
    pub api_key: Option<String>,
    pub env: Option<IndexMap<String, String>>,
    pub min_oauth_validity_ms: Option<i64>,
    pub signal: Option<AbortSignal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CredentialSynchronizationOperation {
    Login,
    Logout,
    SetRuntimeApiKey,
    RemoveRuntimeApiKey,
}

impl CredentialSynchronizationOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Logout => "logout",
            Self::SetRuntimeApiKey => "setRuntimeApiKey",
            Self::RemoveRuntimeApiKey => "removeRuntimeApiKey",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Credential {op_str} committed for {provider_id}, but local synchronization failed: {cause}")]
pub struct CredentialSynchronizationError {
    pub provider_id: String,
    pub operation: CredentialSynchronizationOperation,
    pub op_str: &'static str,
    pub credential: Option<Credential>,
    #[source]
    pub cause: Error,
}

impl CredentialSynchronizationError {
    pub fn new(
        provider_id: impl Into<String>,
        operation: CredentialSynchronizationOperation,
        credential: Option<Credential>,
        cause: Error,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            operation,
            op_str: operation.as_str(),
            credential,
            cause,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResolveModelOptions {
    pub reason: ModelRouteReason,
    pub thinking_level: ModelThinkingLevel,
    pub signal: Option<AbortSignal>,
    pub failed: Option<AssistantMessage>,
    pub state: Option<serde_json::Value>,
}

impl Default for ResolveModelOptions {
    fn default() -> Self {
        Self {
            reason: ModelRouteReason::default(),
            thinking_level: ModelThinkingLevel::Off,
            signal: None,
            failed: None,
            state: None,
        }
    }
}

impl Default for ModelRouteReason {
    fn default() -> Self {
        Self::Direct
    }
}

pub fn merge_headers(
    base: Option<&ProviderHeaders>,
    override_headers: Option<&ProviderHeaders>,
) -> Option<ProviderHeaders> {
    if base.is_none() && override_headers.is_none() {
        return None;
    }
    let mut merged: ProviderHeaders = base.cloned().unwrap_or_default();
    if let Some(ovr) = override_headers {
        for (name, value) in ovr {
            let lower = name.to_ascii_lowercase();
            let to_remove: Vec<String> = merged
                .keys()
                .filter(|k| k.to_ascii_lowercase() == lower)
                .cloned()
                .collect();
            for k in to_remove {
                merged.shift_remove(&k);
            }
            merged.insert(name.clone(), value.clone());
        }
    }
    Some(merged)
}

fn string_headers_to_provider(headers: Option<&IndexMap<String, String>>) -> Option<ProviderHeaders> {
    headers.map(|h| h.iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect())
}

fn abort_error(signal: &AbortSignal) -> Error {
    match signal.reason() {
        Some(reason) => Error::Abort(reason),
        None => Error::Abort(AbortReason::abort()),
    }
}

fn strip_trailing_v1(url: &str) -> String {
    if let Some(s) = url.strip_suffix("/v1/") {
        s.to_owned()
    } else if let Some(s) = url.strip_suffix("/v1") {
        s.to_owned()
    } else {
        url.to_owned()
    }
}

fn find_latest_message_response(messages: &[Message]) -> Option<&AssistantMessage> {
    for msg in messages.iter().rev() {
        if let Message::Assistant(assistant) = msg
            && assistant.stop_reason != pi_ai::StopReason::Error
            && assistant.stop_reason != pi_ai::StopReason::Aborted
        {
            return Some(assistant);
        }
    }
    None
}

struct MutableState {
    builtins: IndexMap<String, Arc<dyn Provider + Send + Sync>>,
    native_extension_providers: IndexMap<String, Arc<dyn Provider + Send + Sync>>,
    extension_providers: IndexMap<String, ProviderConfigInput>,
    virtual_models: IndexMap<String, IndexMap<String, RegisteredVirtualModel>>,
    composition_errors: IndexMap<String, String>,
    config: ModelConfig,
    snapshot: ModelRuntimeSnapshot,
    availability_refresh_seq: u64,
    availability_completed_seq: u64,
    availability_in_flight: usize,
    availability_error_seq: u64,
    provider_availability_seq: IndexMap<String, u64>,
    availability_error: Option<String>,
}

struct ModelRuntimeInner {
    models: MutableModels,
    credentials: Arc<RuntimeCredentials>,
    default_builtins: IndexMap<String, Arc<dyn Provider + Send + Sync>>,
    models_path: Option<String>,
    model_network_enabled: bool,
    state: Mutex<MutableState>,
    availability_event_tx: tokio::sync::watch::Sender<u64>,
    credential_operations: Mutex<IndexMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

struct AvailabilityFlightGuard<'a>(&'a ModelRuntimeInner);

impl Drop for AvailabilityFlightGuard<'_> {
    fn drop(&mut self) {
        let mut st = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.availability_in_flight = st.availability_in_flight.saturating_sub(1);
        self.0.availability_event_tx.send_modify(|v| *v += 1);
    }
}

/// Configured `pi-ai` `Models` collection used by coding-agent and SDK consumers.
#[derive(Clone)]
pub struct ModelRuntime {
    inner: Arc<ModelRuntimeInner>,
}

impl ModelRuntime {
    fn from_parts(
        credentials: Arc<RuntimeCredentials>,
        config: ModelConfig,
        models_path: Option<String>,
        models_store: Arc<dyn ModelsStore + Send + Sync>,
        providers: Vec<Arc<dyn Provider + Send + Sync>>,
        model_network_enabled: bool,
    ) -> Self {
        pi_ai::compat::register_built_in_api_providers();
        let mut default_builtins = IndexMap::new();
        let mut builtins = IndexMap::new();
        for p in providers {
            let id = p.id().to_owned();
            default_builtins.insert(id.clone(), Arc::clone(&p));
            builtins.insert(id, p);
        }

        let cred_store: Arc<dyn CredentialStore + Send + Sync> = credentials.clone();
        let models = create_models(Some(CreateModelsOptions {
            credentials: Some(cred_store),
            models_store: Some(models_store),
            auth_context: None,
        }));
        let (availability_event_tx, _) = tokio::sync::watch::channel(0u64);

        let runtime = Self {
            inner: Arc::new(ModelRuntimeInner {
                models,
                credentials,
                default_builtins,
                models_path,
                model_network_enabled,
                state: Mutex::new(MutableState {
                    builtins,
                    native_extension_providers: IndexMap::new(),
                    extension_providers: IndexMap::new(),
                    virtual_models: IndexMap::new(),
                    composition_errors: IndexMap::new(),
                    config,
                    snapshot: ModelRuntimeSnapshot::default(),
                    availability_refresh_seq: 0,
                    availability_completed_seq: 0,
                    availability_in_flight: 0,
                    availability_error_seq: 0,
                    provider_availability_seq: IndexMap::new(),
                    availability_error: None,
                }),
                availability_event_tx,
                credential_operations: Mutex::new(IndexMap::new()),
            }),
        };
        runtime.rebuild_providers();
        runtime
    }

    pub fn in_memory() -> Self {
        let base_credentials: Arc<dyn CredentialStore + Send + Sync> = Arc::new(DefaultAuthStorage::in_memory(None));
        let credentials = Arc::new(RuntimeCredentials::new(base_credentials));
        Self::from_parts(
            credentials,
            ModelConfig::default(),
            None,
            Arc::new(InMemoryCodingAgentModelsStore::new()),
            Vec::new(),
            false,
        )
    }

    pub async fn create(options: CreateModelRuntimeOptions) -> Result<Self> {
        let base_credentials: Arc<dyn CredentialStore + Send + Sync> = match options.credentials {
            Some(c) => c,
            None => Arc::new(DefaultAuthStorage::create(options.auth_path.as_deref())),
        };
        let credentials = Arc::new(RuntimeCredentials::new(base_credentials));

        let models_path = match options.models_path {
            Some(None) => None,
            Some(Some(p)) => Some(normalize_path(&p, Default::default())),
            None => Some(pi_js::path::join(&[&get_agent_dir(), "models.json"])),
        };

        let config = ModelConfig::load(models_path.as_deref()).await;
        let models_store: Arc<dyn ModelsStore + Send + Sync> = match options.models_store {
            Some(s) => s,
            None => match (&models_path, &options.models_store_path) {
                (Some(mp), store_path_opt) => {
                    let resolved_store = match store_path_opt {
                        Some(p) => p.clone(),
                        None => pi_js::path::join(&[&pi_js::path::dirname(mp), "models-store.json"]),
                    };
                    Arc::new(FileModelsStore::new(Some(&resolved_store)))
                }
                (None, Some(store_path)) => Arc::new(FileModelsStore::new(Some(store_path))),
                (None, None) => Arc::new(InMemoryCodingAgentModelsStore::new()),
            },
        };

        let builtin_generated_at = get_builtin_model_data_generated_at();
        let catalog_base_url = options.catalog_base_url.as_deref();
        let providers: Vec<Arc<dyn Provider + Send + Sync>> = builtin_providers()
            .into_iter()
            .map(|p| {
                if p.id() == "radius" {
                    p
                } else {
                    with_remote_catalog(p, catalog_base_url, builtin_generated_at)
                }
            })
            .collect();

        let network_enabled = pi_js::env::var("PI_OFFLINE").is_none();
        let runtime = Self::from_parts(
            credentials,
            config,
            models_path,
            models_store,
            providers,
            network_enabled,
        );
        runtime.configure_radius_providers();
        runtime.rebuild_providers();

        let refresh_from_network = runtime.inner.model_network_enabled && options.allow_model_network == Some(true);
        let timeout_controller = if refresh_from_network {
            options.model_refresh_timeout_ms.map(|ms| {
                let ctrl = AbortController::new();
                let sig = ctrl.signal();
                let timeout_sig = AbortSignal::timeout(ms);
                let _ = (ctrl, timeout_sig, sig);
                AbortSignal::timeout(ms)
            })
        } else {
            None
        };

        let effective_signal = match (options.signal, timeout_controller) {
            (Some(s), Some(t)) => Some(AbortSignal::any(&[s, t])),
            (Some(s), None) => Some(s),
            (None, Some(t)) => Some(t),
            (None, None) => None,
        };

        if options.refresh_on_create != Some(false) {
            let _ = runtime
                .refresh(Some(ModelsRefreshOptions {
                    allow_network: Some(refresh_from_network),
                    signal: effective_signal,
                    ..Default::default()
                }))
                .await?;
        }

        Ok(runtime)
    }

    fn configure_radius_providers(&self) {
        let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.builtins.clear();
        for (id, provider) in &self.inner.default_builtins {
            st.builtins.insert(id.clone(), Arc::clone(provider));
        }
        for provider_id in st.config.get_provider_ids() {
            if let Some(cfg) = st.config.get_provider(&provider_id)
                && cfg.oauth.as_deref() == Some("radius")
                && let Some(ref base_url) = cfg.base_url
            {
                let radius = radius_provider_with_options(RadiusProviderOptions {
                    id: Some(provider_id.clone()),
                    name: Some(cfg.name.clone().unwrap_or_else(|| provider_id.clone())),
                    gateway: Some(strip_trailing_v1(base_url)),
                });
                st.builtins.insert(provider_id, radius);
            }
        }
    }

    fn provider_ids_locked(st: &MutableState) -> Vec<String> {
        let mut ids = IndexSet::new();
        for k in st.builtins.keys() {
            ids.insert(k.clone());
        }
        for k in st.native_extension_providers.keys() {
            ids.insert(k.clone());
        }
        for k in st.config.get_provider_ids() {
            ids.insert(k);
        }
        for k in st.extension_providers.keys() {
            ids.insert(k.clone());
        }
        for k in st.virtual_models.keys() {
            ids.insert(k.clone());
        }
        ids.into_iter().collect()
    }

    fn compose_provider_locked(st: &mut MutableState, provider_id: &str) -> Option<Arc<dyn Provider + Send + Sync>> {
        let base = st
            .native_extension_providers
            .get(provider_id)
            .cloned()
            .or_else(|| st.builtins.get(provider_id).cloned());
        let extension = st.extension_providers.get(provider_id).cloned();
        let has_config = st.config.get_provider(provider_id).is_some();
        if !has_config && extension.is_none() {
            st.composition_errors.shift_remove(provider_id);
            return base;
        }
        match compose_model_provider(provider_id, base.clone(), &st.config, extension.as_ref()) {
            Ok(provider) => {
                st.composition_errors.shift_remove(provider_id);
                Some(provider)
            }
            Err(err) => {
                st.composition_errors.insert(provider_id.to_owned(), err.to_string());
                base
            }
        }
    }

    fn recompose_provider_locked(st: &mut MutableState, models: &MutableModels, provider_id: &str) -> bool {
        let provider = Self::compose_provider_locked(st, provider_id);
        let has_physical = provider.is_some();
        let virtual_models: Vec<Model> = st
            .virtual_models
            .get(provider_id)
            .map(|map| map.values().map(|entry| entry.model.clone()).collect())
            .unwrap_or_default();
        if !virtual_models.is_empty() {
            models.set_provider(with_virtual_models(provider_id, provider, virtual_models));
        } else if let Some(p) = provider {
            models.set_provider(p);
        } else {
            models.delete_provider(provider_id);
        }
        has_physical
    }

    fn update_model_snapshot_locked(st: &mut MutableState, models: &MutableModels) {
        let all = models.get_models(None);
        let available = all
            .iter()
            .filter(|m| st.snapshot.configured_providers.contains(&m.provider))
            .cloned()
            .collect();
        st.snapshot.all = all;
        st.snapshot.available = available;
    }

    fn rebuild_providers(&self) {
        let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.inner.models.clear_providers();
        st.composition_errors.clear();
        for provider_id in Self::provider_ids_locked(&st) {
            Self::recompose_provider_locked(&mut st, &self.inner.models, &provider_id);
        }
        Self::update_model_snapshot_locked(&mut st, &self.inner.models);
    }

    async fn run_availability_refresh(&self, seq: u64, error_seq: u64, signal: &AbortSignal) -> Result<bool> {
        let providers = self.inner.models.get_providers();
        let available_fut = self.inner.models.get_available(
            None,
            Some(AuthOperationOptions {
                signal: Some(signal.clone()),
            }),
        );
        let checks_fut = futures::future::try_join_all(providers.iter().map(|provider| {
            let pid = provider.id().to_owned();
            let sig = signal.clone();
            let models = self.inner.models.clone();
            async move {
                let auth = models
                    .check_auth(&pid, Some(AuthOperationOptions { signal: Some(sig) }))
                    .await?;
                Ok::<(String, Option<AuthCheck>), Error>((pid, auth))
            }
        }));
        let creds_fut = self.inner.credentials.list(Some(AuthOperationOptions {
            signal: Some(signal.clone()),
        }));

        let (available, checks, credentials) = futures::future::try_join3(available_fut, checks_fut, creds_fut).await?;

        let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        if seq != st.availability_refresh_seq {
            return Ok(false);
        }
        let mut auth = IndexMap::new();
        let mut configured_providers = IndexSet::new();
        for (pid, check) in checks {
            if check.is_some() {
                configured_providers.insert(pid.clone());
            }
            auth.insert(pid, check);
        }
        let stored_providers = credentials.into_iter().map(|c| c.provider_id).collect();
        let all = self.inner.models.get_models(None);
        st.snapshot = ModelRuntimeSnapshot {
            all,
            available,
            configured_providers,
            stored_providers,
            auth,
        };
        st.availability_completed_seq = st.availability_completed_seq.max(seq);
        if error_seq == st.availability_error_seq {
            st.availability_error = None;
        }
        Ok(true)
    }

    async fn queue_availability_refresh(&self, signal: Option<&AbortSignal>) -> Result<()> {
        let effective_signal = operation_signal(signal.cloned());
        effective_signal.throw_if_aborted()?;
        loop {
            let (seq, error_seq) = {
                let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                st.availability_refresh_seq += 1;
                let seq = st.availability_refresh_seq;
                for p_seq in st.provider_availability_seq.values_mut() {
                    *p_seq += 1;
                }
                st.availability_error_seq += 1;
                let error_seq = st.availability_error_seq;
                st.availability_in_flight += 1;
                (seq, error_seq)
            };
            let flight = AvailabilityFlightGuard(&self.inner);
            let res = self.run_availability_refresh(seq, error_seq, &effective_signal).await;
            drop(flight);

            match res {
                Ok(true) => return Ok(()),
                Ok(false) => {
                    let mut rx = self.inner.availability_event_tx.subscribe();
                    loop {
                        {
                            let st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                            rx.mark_unchanged();
                            if st.availability_completed_seq >= seq {
                                return Ok(());
                            }
                            if st.availability_in_flight == 0 {
                                break;
                            }
                        }
                        tokio::select! {
                            biased;
                            _ = effective_signal.cancelled() => return Err(abort_error(&effective_signal)),
                            changed = rx.changed() => {
                                if changed.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    if !effective_signal.aborted() {
                        let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                        if error_seq == st.availability_error_seq {
                            st.availability_error = Some(err.to_string());
                        }
                    }
                    return Err(err);
                }
            }
        }
    }

    async fn refresh_provider_availability(&self, provider_id: &str, signal: &AbortSignal) -> Result<()> {
        let (provider_seq, error_seq) = {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.availability_refresh_seq += 1;
            let next_p = st.provider_availability_seq.get(provider_id).copied().unwrap_or(0) + 1;
            st.provider_availability_seq.insert(provider_id.to_owned(), next_p);
            st.availability_error_seq += 1;
            (next_p, st.availability_error_seq)
        };

        let opts = Some(AuthOperationOptions {
            signal: Some(signal.clone()),
        });
        let res: Result<()> = async {
            let (available, auth, credential) = futures::future::try_join3(
                self.inner.models.get_available(Some(provider_id), opts.clone()),
                self.inner.models.check_auth(provider_id, opts.clone()),
                self.inner.credentials.read(provider_id, opts),
            )
            .await?;
            signal.throw_if_aborted()?;

            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            if st.provider_availability_seq.get(provider_id).copied() != Some(provider_seq) {
                return Ok(());
            }
            if let Some(a) = auth {
                st.snapshot.configured_providers.insert(provider_id.to_owned());
                st.snapshot.auth.insert(provider_id.to_owned(), Some(a));
            } else {
                st.snapshot.configured_providers.shift_remove(provider_id);
                st.snapshot.auth.shift_remove(provider_id);
            }
            if credential.is_some() {
                st.snapshot.stored_providers.insert(provider_id.to_owned());
            } else {
                st.snapshot.stored_providers.shift_remove(provider_id);
            }
            let all = self.inner.models.get_models(None);
            let mut available_by_id: IndexMap<String, Model> = IndexMap::new();
            for m in st
                .snapshot
                .available
                .iter()
                .filter(|m| m.provider != provider_id)
                .chain(available.iter())
            {
                available_by_id.insert(format!("{}\0{}", m.provider, m.id), m.clone());
            }
            let ordered_available = all
                .iter()
                .filter_map(|m| available_by_id.get(&format!("{}\0{}", m.provider, m.id)).cloned())
                .collect();
            st.snapshot.all = all;
            st.snapshot.available = ordered_available;
            if error_seq == st.availability_error_seq {
                st.availability_error = None;
            }
            Ok(())
        }
        .await;

        if let Err(ref err) = res
            && !signal.aborted()
        {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            if st.provider_availability_seq.get(provider_id).copied() == Some(provider_seq)
                && error_seq == st.availability_error_seq
            {
                st.availability_error = Some(err.to_string());
            }
        }
        res
    }

    pub fn get_providers(&self) -> Vec<Arc<dyn Provider + Send + Sync>> {
        self.inner.models.get_providers()
    }

    pub fn get_provider(&self, provider_id: &str) -> Option<Arc<dyn Provider + Send + Sync>> {
        self.inner.models.get_provider(provider_id)
    }

    pub fn get_models(&self, provider_id: Option<&str>) -> Vec<Model> {
        self.inner.models.get_models(provider_id)
    }

    pub fn get_model(&self, provider_id: &str, model_id: &str) -> Option<Model> {
        self.inner.models.get_model(provider_id, model_id)
    }

    pub fn get_models_of_type(&self, model_type: ModelType, provider_id: Option<&str>) -> Vec<AnyModel> {
        self.inner.models.get_models_of_type(model_type, provider_id)
    }

    pub fn get_model_of_type(&self, model_type: ModelType, provider_id: &str, model_id: &str) -> Option<AnyModel> {
        self.inner.models.get_model_of_type(model_type, provider_id, model_id)
    }

    pub fn get_all_models(&self, provider_id: Option<&str>) -> Vec<AnyModel> {
        self.inner.models.get_all_models(provider_id)
    }

    pub async fn get_available_of_type(
        &self,
        model_type: ModelType,
        provider_id: Option<&str>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Vec<AnyModel>> {
        self.inner
            .models
            .get_available_of_type(model_type, provider_id, options)
            .await
    }

    pub async fn get_all_available(
        &self,
        provider_id: Option<&str>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Vec<AnyModel>> {
        self.inner.models.get_all_available(provider_id, options).await
    }

    pub async fn check_auth(
        &self,
        provider_id: &str,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<AuthCheck>> {
        self.inner.models.check_auth(provider_id, options).await
    }

    pub async fn get_available(
        &self,
        provider_id: Option<&str>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Vec<Model>> {
        if let Some(pid) = provider_id {
            let error_seq = {
                let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                st.availability_error_seq += 1;
                st.availability_error_seq
            };
            let aborted = options
                .as_ref()
                .and_then(|o| o.signal.as_ref())
                .is_some_and(AbortSignal::aborted);
            let res = self.inner.models.get_available(Some(pid), options).await;
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            match res {
                Ok(avail) => {
                    if error_seq == st.availability_error_seq {
                        st.availability_error = None;
                    }
                    return Ok(avail);
                }
                Err(err) => {
                    if error_seq == st.availability_error_seq && !aborted {
                        st.availability_error = Some(err.to_string());
                    }
                    return Err(err);
                }
            }
        }
        let signal = options.and_then(|o| o.signal);
        self.queue_availability_refresh(signal.as_ref()).await?;
        Ok(self.get_available_snapshot())
    }

    pub fn get_available_snapshot(&self) -> Vec<Model> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .available
            .clone()
    }

    pub fn get_error(&self) -> Option<String> {
        let st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut errors = Vec::new();
        if let Some(cfg_err) = st.config.get_error() {
            errors.push(cfg_err.to_owned());
        }
        for (provider_id, err) in &st.composition_errors {
            errors.push(format!("Provider \"{provider_id}\": {err}"));
        }
        if let Some(ref avail_err) = st.availability_error {
            errors.push(format!("Availability refresh: {avail_err}"));
        }
        if errors.is_empty() {
            None
        } else {
            Some(errors.join("\n\n"))
        }
    }

    pub fn get_registered_provider_config(&self, provider_id: &str) -> Option<ProviderConfigInput> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extension_providers
            .get(provider_id)
            .cloned()
    }

    pub fn get_registered_provider_ids(&self) -> Vec<String> {
        let st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut ids = IndexSet::new();
        for k in st.extension_providers.keys() {
            ids.insert(k.clone());
        }
        for k in st.native_extension_providers.keys() {
            ids.insert(k.clone());
        }
        ids.into_iter().collect()
    }

    pub fn get_registered_native_provider(&self, provider_id: &str) -> Option<Arc<dyn Provider + Send + Sync>> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .native_extension_providers
            .get(provider_id)
            .cloned()
    }

    pub fn get_compatibility_request_config(&self, model: &Model) -> Result<CompatibilityRequestConfig> {
        let st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        let any_model = AnyModel::Chat(model.clone());
        resolve_compatibility_request_config(
            &any_model,
            st.config.get_provider(&model.provider),
            st.extension_providers.get(&model.provider),
        )
    }

    pub fn is_using_oauth(&self, provider_id: &str) -> bool {
        let st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.snapshot
            .auth
            .get(provider_id)
            .and_then(|c| c.as_ref())
            .is_some_and(|c| c.auth_type == AuthType::OAuth)
    }

    pub fn is_using_subscription(&self, provider_id: &str) -> bool {
        self.is_using_oauth(provider_id)
            && self
                .inner
                .models
                .get_provider(provider_id)
                .and_then(|p| p.auth().oauth.clone())
                .is_some_and(|o| o.is_subscription())
    }

    pub fn has_configured_auth(&self, provider_id: &str) -> bool {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .configured_providers
            .contains(provider_id)
    }

    pub async fn get_auth_provider_id(
        &self,
        provider_id: &str,
        overrides: Option<ModelRuntimeAuthOverrides>,
    ) -> Result<Option<AuthResult>> {
        let ai_overrides = overrides.map(|o| AuthResolutionOverrides {
            api_key: o.api_key,
            env: o.env,
            min_oauth_validity_ms: o.min_oauth_validity_ms,
            signal: o.signal,
        });
        self.inner.models.get_auth_provider_id(provider_id, ai_overrides).await
    }

    pub async fn get_auth_model(
        &self,
        model: &AnyModel,
        overrides: Option<ModelRuntimeAuthOverrides>,
    ) -> Result<Option<AuthResult>> {
        let ovr_env = overrides.as_ref().and_then(|o| o.env.clone());
        let ai_overrides = overrides.map(|o| AuthResolutionOverrides {
            api_key: o.api_key,
            env: o.env,
            min_oauth_validity_ms: o.min_oauth_validity_ms,
            signal: o.signal,
        });
        let Some(mut resolution) = self.inner.models.get_auth_model(model, ai_overrides).await? else {
            return Ok(None);
        };
        let (cfg_provider, ext_provider) = {
            let st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            (
                st.config.get_provider(model.provider()).cloned(),
                st.extension_providers.get(model.provider()).cloned(),
            )
        };
        let mut combined_env = resolution.env.clone().unwrap_or_default();
        if let Some(env) = ovr_env {
            for (k, v) in env {
                combined_env.insert(k, v);
            }
        }
        let configured_headers = resolve_configured_model_headers(
            model,
            cfg_provider.as_ref(),
            ext_provider.as_ref(),
            if combined_env.is_empty() {
                None
            } else {
                Some(&combined_env)
            },
        )?;
        let provider_headers = string_headers_to_provider(configured_headers.as_ref());
        resolution.auth.headers = merge_headers(resolution.auth.headers.as_ref(), provider_headers.as_ref());
        Ok(Some(resolution))
    }

    async fn enqueue_credential_operation<T, F, Fut>(
        &self,
        provider_id: &str,
        signal: &AbortSignal,
        task: F,
    ) -> Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        signal.throw_if_aborted()?;
        let mutex = {
            let mut ops = self
                .inner
                .credential_operations
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            ops.entry(provider_id.to_owned())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = tokio::select! {
            biased;
            _ = signal.cancelled() => return Err(abort_error(signal)),
            g = mutex.lock() => g,
        };
        signal.throw_if_aborted()?;
        task().await
    }

    async fn synchronize_credential_state(
        &self,
        provider_id: &str,
        operation: CredentialSynchronizationOperation,
        credential: Option<Credential>,
        signal: &AbortSignal,
    ) -> Result<()> {
        let sync_res: Result<()> = async {
            signal.throw_if_aborted()?;
            {
                let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                Self::recompose_provider_locked(&mut st, &self.inner.models, provider_id);
                if let Some(comp_err) = st.composition_errors.get(provider_id).cloned() {
                    return Err(Error::msg(comp_err));
                }
            }
            let result = self
                .inner
                .models
                .refresh(Some(ModelsRefreshOptions {
                    allow_network: Some(false),
                    providers: Some(vec![provider_id.to_owned()]),
                    force: None,
                    signal: Some(signal.clone()),
                }))
                .await?;
            if result.aborted {
                signal.throw_if_aborted()?;
            }
            if let Some(err) = result
                .errors
                .into_iter()
                .find(|(k, _)| k == provider_id)
                .map(|(_, v)| v)
            {
                return Err(err);
            }
            {
                let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                Self::update_model_snapshot_locked(&mut st, &self.inner.models);
            }
            self.refresh_provider_availability(provider_id, signal).await?;
            Ok(())
        }
        .await;

        sync_res.map_err(|cause| {
            Error::typed(
                "CredentialSynchronizationError",
                CredentialSynchronizationError::new(provider_id, operation, credential, cause),
            )
        })
    }

    pub async fn set_runtime_api_key(
        &self,
        provider_id: &str,
        api_key: &str,
        options: Option<AuthOperationOptions>,
    ) -> Result<()> {
        let signal = operation_signal(options.and_then(|o| o.signal));
        self.enqueue_credential_operation(provider_id, &signal, || async {
            self.inner.credentials.set_runtime_api_key(provider_id, api_key);
            self.synchronize_credential_state(
                provider_id,
                CredentialSynchronizationOperation::SetRuntimeApiKey,
                Some(Credential::ApiKey(pi_ai::ApiKeyCredential {
                    key: Some(api_key.to_owned()),
                    env: None,
                })),
                &signal,
            )
            .await
        })
        .await
    }

    pub async fn remove_runtime_api_key(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<()> {
        let signal = operation_signal(options.and_then(|o| o.signal));
        self.enqueue_credential_operation(provider_id, &signal, || async {
            self.inner.credentials.remove_runtime_api_key(provider_id);
            self.synchronize_credential_state(
                provider_id,
                CredentialSynchronizationOperation::RemoveRuntimeApiKey,
                None,
                &signal,
            )
            .await
        })
        .await
    }

    pub async fn list_credentials(&self, options: Option<AuthOperationOptions>) -> Result<Vec<CredentialInfo>> {
        self.inner.credentials.list(options).await
    }

    pub fn get_provider_auth_status(&self, provider_id: &str) -> AuthStatus {
        if self.inner.credentials.has_runtime_api_key(provider_id) {
            return AuthStatus {
                configured: true,
                source: Some(AuthStatusSource::Runtime),
                label: None,
            };
        }
        let st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.snapshot.stored_providers.contains(provider_id) {
            return AuthStatus {
                configured: true,
                source: Some(AuthStatusSource::Stored),
                label: None,
            };
        }
        if let Some(configured) = configured_request_auth_status(
            st.config.get_provider(provider_id),
            st.extension_providers.get(provider_id),
        ) {
            return configured;
        }
        match st.snapshot.auth.get(provider_id).and_then(|c| c.as_ref()) {
            Some(check) => AuthStatus {
                configured: true,
                source: Some(AuthStatusSource::Environment),
                label: check.source.clone(),
            },
            None => AuthStatus {
                configured: false,
                source: None,
                label: None,
            },
        }
    }

    async fn prepare_request_inner(
        &self,
        model: &AnyModel,
        req_api_key: Option<String>,
        req_env: Option<ProviderEnv>,
        req_headers: Option<ProviderHeaders>,
        req_signal: Option<AbortSignal>,
        transform_headers: Option<&pi_ai::models::TransformHeaders>,
    ) -> Result<(
        Arc<dyn Provider + Send + Sync>,
        AnyModel,
        Option<String>,
        Option<ProviderHeaders>,
        Option<ProviderEnv>,
    )> {
        let provider_id = model.provider();
        let provider = self.inner.models.get_provider(provider_id).ok_or_else(|| {
            Error::from(ModelsError::new(
                ModelsErrorCode::Provider,
                format!("Unknown provider: {provider_id}"),
            ))
        })?;
        let resolution = self
            .get_auth_model(
                model,
                Some(ModelRuntimeAuthOverrides {
                    api_key: req_api_key.clone(),
                    env: req_env.clone(),
                    min_oauth_validity_ms: None,
                    signal: req_signal,
                }),
            )
            .await?
            .ok_or_else(|| {
                Error::from(ModelsError::new(
                    ModelsErrorCode::Auth,
                    format!("Provider is not configured: {provider_id}"),
                ))
            })?;

        let mut headers = merge_headers(resolution.auth.headers.as_ref(), req_headers.as_ref());
        if let Some(transform) = transform_headers {
            headers = Some(transform(headers.unwrap_or_default()).await?);
        }
        let env = match (resolution.env, req_env) {
            (None, None) => None,
            (Some(re), None) => Some(re),
            (None, Some(oe)) => Some(oe),
            (Some(mut re), Some(oe)) => {
                for (k, v) in oe {
                    re.insert(k, v);
                }
                Some(re)
            }
        };

        let request_model = match resolution.auth.base_url {
            Some(url) => match model.clone() {
                AnyModel::Chat(mut m) => {
                    m.base_url = url;
                    AnyModel::Chat(m)
                }
                AnyModel::Image(mut m) => {
                    m.base_url = url;
                    AnyModel::Image(m)
                }
                AnyModel::Classifier(mut m) => {
                    m.base_url = url;
                    AnyModel::Classifier(m)
                }
                AnyModel::Unknown(mut v) => {
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert("baseUrl".to_owned(), serde_json::Value::String(url));
                    }
                    AnyModel::Unknown(v)
                }
            },
            None => model.clone(),
        };

        let applied_api_key = req_api_key.or(resolution.auth.api_key);
        Ok((provider, request_model, applied_api_key, headers, env))
    }

    pub fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        let transcript: TranscriptContext = normalize_context(context);
        let runtime = self.clone();
        let model_owned = model.clone();
        lazy_stream(model, async move {
            let any_model = AnyModel::Chat(model_owned.clone());
            assert_chat_model(&any_model)?;
            let mut req_opts = options.as_ref().map(|o| o.base.clone()).unwrap_or_default();
            let transform = options.as_ref().and_then(|o| o.transform_headers.as_ref());
            let (provider, req_model, api_key, headers, env) = runtime
                .prepare_request_inner(
                    &any_model,
                    req_opts.base.api_key.clone(),
                    req_opts.base.env.clone(),
                    req_opts.base.headers.clone(),
                    req_opts.base.signal.clone(),
                    transform,
                )
                .await?;
            let final_model = req_model.into_chat().unwrap_or(model_owned);
            req_opts.base.api_key = api_key;
            req_opts.base.headers = headers;
            req_opts.base.env = env;
            Ok(provider.stream(&final_model, &transcript, Some(req_opts)))
        })
    }

    pub async fn complete(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsApiStreamOptions>,
    ) -> AssistantMessage {
        self.stream(model, context, options).result().await
    }

    pub fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsSimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        let transcript: TranscriptContext = normalize_context(context);
        let runtime = self.clone();
        let model_owned = model.clone();
        let context_owned = context.clone();

        if is_virtual_model(model) {
            return lazy_stream(model, async move {
                let thinking_level = options
                    .as_ref()
                    .and_then(|o| o.base.reasoning)
                    .map(ModelThinkingLevel::from)
                    .unwrap_or(ModelThinkingLevel::Off);
                let signal = options.as_ref().and_then(|o| o.base.base.signal.clone());
                let route = runtime
                    .resolve_model(
                        &model_owned,
                        &transcript.messages,
                        ResolveModelOptions {
                            reason: ModelRouteReason::Direct,
                            thinking_level,
                            signal,
                            failed: None,
                            state: None,
                        },
                    )
                    .await?;
                let mut next_opts = options.unwrap_or_else(|| ModelsSimpleStreamOptions {
                    base: SimpleStreamOptions::default(),
                    transform_headers: None,
                });
                let limit = route.model.max_tokens;
                if let Some(req_max) = next_opts.base.base.max_tokens
                    && limit > 0
                {
                    next_opts.base.base.max_tokens = Some(req_max.min(limit));
                }
                next_opts.base.reasoning = route.thinking_level.as_thinking_level();
                if route.model.provider != model_owned.provider {
                    next_opts.base.base.api_key = None;
                    next_opts.base.base.headers = None;
                    next_opts.base.base.env = None;
                }
                Ok(runtime.stream_simple(&route.model, &context_owned, Some(next_opts)))
            });
        }

        lazy_stream(model, async move {
            let any_model = AnyModel::Chat(model_owned.clone());
            assert_chat_model(&any_model)?;
            let mut req_opts = options.as_ref().map(|o| o.base.clone()).unwrap_or_default();
            let transform = options.as_ref().and_then(|o| o.transform_headers.as_ref());
            let (provider, req_model, api_key, headers, env) = runtime
                .prepare_request_inner(
                    &any_model,
                    req_opts.base.api_key.clone(),
                    req_opts.base.env.clone(),
                    req_opts.base.headers.clone(),
                    req_opts.base.signal.clone(),
                    transform,
                )
                .await?;
            let final_model = req_model.into_chat().unwrap_or(model_owned);
            req_opts.base.api_key = api_key;
            req_opts.base.headers = headers;
            req_opts.base.env = env;
            Ok(provider.stream_simple(&final_model, &transcript, Some(req_opts)))
        })
    }

    pub async fn complete_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsSimpleStreamOptions>,
    ) -> AssistantMessage {
        self.stream_simple(model, context, options).result().await
    }

    pub fn stream_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<ModelsDeferredFetchOptions>,
    ) -> AssistantMessageEventStream {
        let runtime = self.clone();
        let model_owned = model.clone();
        let handle_owned = handle.clone();
        lazy_stream(model, async move {
            let any_model = AnyModel::Chat(model_owned.clone());
            assert_chat_model(&any_model)?;
            let mut req_opts = options.as_ref().map(|o| o.base.clone()).unwrap_or_default();
            let transform = options.as_ref().and_then(|o| o.transform_headers.as_ref());
            let (provider, req_model, api_key, headers, env) = runtime
                .prepare_request_inner(
                    &any_model,
                    req_opts.base.api_key.clone(),
                    req_opts.base.env.clone(),
                    req_opts.base.headers.clone(),
                    req_opts.base.signal.clone(),
                    transform,
                )
                .await?;
            let final_model = req_model.into_chat().unwrap_or(model_owned.clone());
            req_opts.base.api_key = api_key;
            req_opts.base.headers = headers;
            req_opts.base.env = env;
            provider
                .fetch_deferred(&final_model, &handle_owned, Some(req_opts))
                .ok_or_else(|| {
                    Error::from(ModelsError::new(
                        ModelsErrorCode::Provider,
                        format!("Provider {} does not support deferred responses", model_owned.provider),
                    ))
                })
        })
    }

    pub async fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<ModelsDeferredFetchOptions>,
    ) -> AssistantMessage {
        self.stream_deferred(model, handle, options).result().await
    }

    pub async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<ModelsDeferredCancelOptions>,
    ) -> Result<()> {
        let any_model = AnyModel::Chat(model.clone());
        assert_chat_model(&any_model)?;
        let mut req_opts = options.as_ref().map(|o| o.base.clone()).unwrap_or_default();
        let transform = options.as_ref().and_then(|o| o.transform_headers.as_ref());
        let (provider, req_model, api_key, headers, env) = self
            .prepare_request_inner(
                &any_model,
                req_opts.api_key.clone(),
                req_opts.env.clone(),
                req_opts.headers.clone(),
                req_opts.signal.clone(),
                transform,
            )
            .await?;
        let final_model = req_model.into_chat().unwrap_or_else(|| model.clone());
        req_opts.api_key = api_key;
        req_opts.headers = headers;
        req_opts.env = env;
        let fut = provider
            .cancel_deferred(&final_model, handle, Some(req_opts))
            .ok_or_else(|| {
                Error::from(ModelsError::new(
                    ModelsErrorCode::Provider,
                    format!("Provider {} does not support deferred responses", model.provider),
                ))
            })?;
        fut.await
    }

    pub async fn generate_images(
        &self,
        model: &ImageModel,
        context: &ImagesContext,
        options: Option<ModelsImagesOptions>,
    ) -> AssistantImages {
        let aborted = options
            .as_ref()
            .and_then(|o| o.base.base.signal.as_ref())
            .is_some_and(AbortSignal::aborted);
        let any_model = AnyModel::Image(model.clone());
        let res: Result<AssistantImages> = async {
            assert_image_model(&any_model)?;
            let mut req_opts = options.as_ref().map(|o| o.base.clone()).unwrap_or_default();
            let transform = options.as_ref().and_then(|o| o.transform_headers.as_ref());
            let (provider, req_model, api_key, headers, env) = self
                .prepare_request_inner(
                    &any_model,
                    req_opts.base.api_key.clone(),
                    req_opts.base.env.clone(),
                    req_opts.base.headers.clone(),
                    req_opts.base.signal.clone(),
                    transform,
                )
                .await?;
            let final_model = req_model.into_image().unwrap_or_else(|| model.clone());
            req_opts.base.api_key = api_key;
            req_opts.base.headers = headers;
            req_opts.base.env = env;
            let fut = provider
                .generate_images(&final_model, context, Some(req_opts))
                .ok_or_else(|| {
                    Error::from(ModelsError::new(
                        ModelsErrorCode::Provider,
                        format!("Provider {} does not support image generation", model.provider),
                    ))
                })?;
            Ok(fut.await)
        }
        .await;

        match res {
            Ok(images) => images,
            Err(err) => image_error_result(model, &err, aborted),
        }
    }

    pub async fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<ModelsClassifierOptions>,
    ) -> ClassifierResult {
        let aborted = options
            .as_ref()
            .and_then(|o| o.base.base.signal.as_ref())
            .is_some_and(AbortSignal::aborted);
        let any_model = AnyModel::Classifier(model.clone());
        let res: Result<ClassifierResult> = async {
            assert_classifier_model(&any_model)?;
            let mut req_opts = options.as_ref().map(|o| o.base.clone()).unwrap_or_default();
            let transform = options.as_ref().and_then(|o| o.transform_headers.as_ref());
            let (provider, req_model, api_key, headers, env) = self
                .prepare_request_inner(
                    &any_model,
                    req_opts.base.api_key.clone(),
                    req_opts.base.env.clone(),
                    req_opts.base.headers.clone(),
                    req_opts.base.signal.clone(),
                    transform,
                )
                .await?;
            let final_model = req_model.into_classifier().unwrap_or_else(|| model.clone());
            req_opts.base.api_key = api_key;
            req_opts.base.headers = headers;
            req_opts.base.env = env;
            let fut = provider
                .classify(&final_model, context, Some(req_opts))
                .ok_or_else(|| {
                    Error::from(ModelsError::new(
                        ModelsErrorCode::Provider,
                        format!("Provider {} does not support classification", model.provider),
                    ))
                })?;
            Ok(fut.await)
        }
        .await;

        match res {
            Ok(result) => result,
            Err(err) => classifier_error_result(model, &err, aborted),
        }
    }

    pub async fn login(
        &self,
        provider_id: &str,
        auth_type: AuthType,
        interaction: Arc<dyn AuthInteraction>,
        options: Option<LoginOptions>,
    ) -> Result<Credential> {
        let signal = operation_signal(interaction.signal().cloned());
        self.enqueue_credential_operation(provider_id, &signal, || async {
            let credential = self
                .inner
                .models
                .login(provider_id, auth_type, interaction, options)
                .await?;
            self.synchronize_credential_state(
                provider_id,
                CredentialSynchronizationOperation::Login,
                Some(credential.clone()),
                &signal,
            )
            .await?;
            Ok(credential)
        })
        .await
    }

    pub async fn logout(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<()> {
        let signal = operation_signal(options.and_then(|o| o.signal));
        self.enqueue_credential_operation(provider_id, &signal, || async {
            self.inner
                .models
                .logout(
                    provider_id,
                    Some(AuthOperationOptions {
                        signal: Some(signal.clone()),
                    }),
                )
                .await?;
            self.synchronize_credential_state(provider_id, CredentialSynchronizationOperation::Logout, None, &signal)
                .await
        })
        .await
    }

    pub async fn refresh(&self, options: Option<ModelsRefreshOptions>) -> Result<ModelsRefreshResult> {
        let opts = options.unwrap_or_default();
        let loaded_config = ModelConfig::load(self.inner.models_path.as_deref()).await;
        {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.config = loaded_config;
        }
        self.configure_radius_providers();

        let provider_filter: Option<IndexSet<String>> = opts.providers.as_ref().map(|p| p.iter().cloned().collect());
        if let Some(ref filter) = provider_filter {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            for pid in filter {
                Self::recompose_provider_locked(&mut st, &self.inner.models, pid);
            }
            Self::update_model_snapshot_locked(&mut st, &self.inner.models);
        } else {
            self.rebuild_providers();
        }

        let caller_signal = opts.signal.clone();
        let refresh_opts = ModelsRefreshOptions {
            allow_network: Some(opts.allow_network.unwrap_or(self.inner.model_network_enabled)),
            providers: opts.providers.clone(),
            force: opts.force,
            signal: caller_signal.clone(),
        };
        let result = self.inner.models.refresh(Some(refresh_opts)).await?;
        let mut errors = result.errors;

        {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            Self::update_model_snapshot_locked(&mut st, &self.inner.models);
        }

        if let Some(filter) = provider_filter {
            let sig = operation_signal(caller_signal.clone());
            let per_provider = futures::future::join_all(filter.into_iter().map(|pid| {
                let sig_clone = sig.clone();
                async move {
                    let res = self.refresh_provider_availability(&pid, &sig_clone).await;
                    (pid, res)
                }
            }))
            .await;
            for (pid, res) in per_provider {
                if let Err(err) = res
                    && !caller_signal.as_ref().is_some_and(AbortSignal::aborted)
                {
                    errors.insert(pid, err);
                }
            }
        } else {
            let _ = self.queue_availability_refresh(caller_signal.as_ref()).await;
        }

        Ok(ModelsRefreshResult {
            aborted: result.aborted || caller_signal.as_ref().is_some_and(AbortSignal::aborted),
            errors,
        })
    }

    fn spawn_offline_refresh(&self) {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let runtime = self.clone();
            handle.spawn(async move {
                let _ = runtime
                    .refresh(Some(ModelsRefreshOptions {
                        allow_network: Some(false),
                        ..Default::default()
                    }))
                    .await;
            });
        }
    }

    fn mark_provisionally_configured_locked(
        st: &mut MutableState,
        provider_id: &str,
        configured_status: Option<&AuthStatus>,
        auth_type: AuthType,
    ) {
        if !st.snapshot.stored_providers.contains(provider_id) && !configured_status.is_some_and(|s| s.configured) {
            return;
        }
        st.snapshot.configured_providers.insert(provider_id.to_owned());
        if !st.snapshot.auth.get(provider_id).and_then(|c| c.as_ref()).is_some() {
            st.snapshot.auth.insert(
                provider_id.to_owned(),
                Some(AuthCheck {
                    source: Some("configured provider".to_owned()),
                    auth_type,
                }),
            );
        }
        st.snapshot.available = st
            .snapshot
            .all
            .iter()
            .filter(|m| st.snapshot.configured_providers.contains(&m.provider))
            .cloned()
            .collect();
    }

    pub fn register_native_provider(&self, provider: Arc<dyn Provider + Send + Sync>) -> Result<()> {
        let id = provider.id().to_owned();
        if id.trim().is_empty() {
            return Err(Error::msg("Provider id must not be empty."));
        }
        {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            let auth_type = if provider.auth().oauth.is_some() && provider.auth().api_key.is_none() {
                AuthType::OAuth
            } else {
                AuthType::ApiKey
            };
            st.extension_providers.shift_remove(&id);
            st.native_extension_providers.insert(id.clone(), provider);
            Self::recompose_provider_locked(&mut st, &self.inner.models, &id);
            Self::update_model_snapshot_locked(&mut st, &self.inner.models);
            let configured_status = configured_request_auth_status(st.config.get_provider(&id), None);
            Self::mark_provisionally_configured_locked(&mut st, &id, configured_status.as_ref(), auth_type);
        }
        self.spawn_offline_refresh();
        Ok(())
    }

    pub fn register_provider(&self, provider_id: &str, config: ProviderConfigInput) -> Result<()> {
        {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            let base = st.builtins.get(provider_id).map(|b| &**b as &dyn Provider);
            validate_extension_provider(provider_id, base, st.config.get_provider(provider_id), &config)?;
            st.native_extension_providers.shift_remove(provider_id);
            let mut effective = st.extension_providers.get(provider_id).cloned().unwrap_or_default();
            if config.name.is_some() {
                effective.name = config.name;
            }
            if config.base_url.is_some() {
                effective.base_url = config.base_url;
            }
            if config.api_key.is_some() {
                effective.api_key = config.api_key;
            }
            if config.api.is_some() {
                effective.api = config.api;
            }
            if config.stream_simple.is_some() {
                effective.stream_simple = config.stream_simple;
            }
            if config.images.is_some() {
                effective.images = config.images;
            }
            if config.classifiers.is_some() {
                effective.classifiers = config.classifiers;
            }
            if config.headers.is_some() {
                effective.headers = config.headers;
            }
            if config.auth_header.is_some() {
                effective.auth_header = config.auth_header;
            }
            if config.oauth.is_some() {
                effective.oauth = config.oauth;
            }
            if config.models.is_some() {
                effective.models = config.models;
            }
            if config.refresh_models.is_some() {
                effective.refresh_models = config.refresh_models;
            }

            let auth_type = if effective.oauth.is_some() && effective.api_key.is_none() {
                AuthType::OAuth
            } else {
                AuthType::ApiKey
            };
            let configured_status =
                configured_request_auth_status(st.config.get_provider(provider_id), Some(&effective));
            st.extension_providers.insert(provider_id.to_owned(), effective);
            Self::recompose_provider_locked(&mut st, &self.inner.models, provider_id);
            Self::update_model_snapshot_locked(&mut st, &self.inner.models);
            Self::mark_provisionally_configured_locked(&mut st, provider_id, configured_status.as_ref(), auth_type);
        }
        self.spawn_offline_refresh();
        Ok(())
    }

    pub fn unregister_provider(&self, provider_id: &str) {
        {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.extension_providers.shift_remove(provider_id);
            st.native_extension_providers.shift_remove(provider_id);
            Self::recompose_provider_locked(&mut st, &self.inner.models, provider_id);
            Self::update_model_snapshot_locked(&mut st, &self.inner.models);
        }
        self.spawn_offline_refresh();
    }

    pub fn register_virtual_model(&self, definition: VirtualModelDefinition) -> Result<()> {
        let provider_id = definition.provider.clone();
        let id = definition.id.clone();
        if provider_id.trim().is_empty() || id.trim().is_empty() {
            return Err(Error::msg("Virtual model provider and id must not be empty."));
        }
        if let Some(existing) = self.inner.models.get_model(&provider_id, &id)
            && !is_virtual_model(&existing)
        {
            return Err(Error::msg(format!(
                "Virtual model {provider_id}/{id} conflicts with a physical model."
            )));
        }

        let descriptor = VirtualModelDescriptor::from(&definition);
        let model = create_virtual_model(&descriptor);
        let entry = RegisteredVirtualModel {
            model,
            router: definition.router,
        };
        {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            let models_map = st.virtual_models.entry(provider_id.clone()).or_default();
            models_map.insert(id, entry);
            let has_physical = Self::recompose_provider_locked(&mut st, &self.inner.models, &provider_id);
            if !has_physical && !st.snapshot.configured_providers.contains(&provider_id) {
                st.snapshot.auth.insert(
                    provider_id.clone(),
                    Some(AuthCheck {
                        source: Some("virtual".to_owned()),
                        auth_type: AuthType::ApiKey,
                    }),
                );
                st.snapshot.configured_providers.insert(provider_id);
            }
            Self::update_model_snapshot_locked(&mut st, &self.inner.models);
        }
        self.spawn_offline_refresh();
        Ok(())
    }

    pub fn unregister_virtual_model(&self, provider_id: &str, id: &str) {
        let removed = {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            let removed = match st.virtual_models.get_mut(provider_id) {
                Some(map) => {
                    let was_present = map.shift_remove(id).is_some();
                    if map.is_empty() {
                        st.virtual_models.shift_remove(provider_id);
                    }
                    was_present
                }
                None => false,
            };
            if removed {
                Self::recompose_provider_locked(&mut st, &self.inner.models, provider_id);
                Self::update_model_snapshot_locked(&mut st, &self.inner.models);
            }
            removed
        };
        if removed {
            self.spawn_offline_refresh();
        }
    }

    pub async fn resolve_model(
        &self,
        model: &Model,
        messages: &[Message],
        options: ResolveModelOptions,
    ) -> Result<ModelRoute> {
        let name = format!("Virtual model {}/{}", model.provider, model.id);
        let router = {
            let st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.virtual_models
                .get(&model.provider)
                .and_then(|map| map.get(&model.id))
                .map(|entry| Arc::clone(&entry.router))
        }
        .ok_or_else(|| Error::msg(format!("{name} is not registered.")))?;

        let latest = find_latest_message_response(messages);
        let previous = latest.and_then(|msg| {
            self.get_physical_model(&msg.provider, &msg.model)
                .map(|phys| ModelRoutePrevious {
                    model: phys,
                    thinking_level: msg.thinking_level,
                })
        });
        let failed = options.failed.as_ref().and_then(|msg| {
            self.get_physical_model(&msg.provider, &msg.model)
                .map(|phys| ModelRouteFailed {
                    model: phys,
                    thinking_level: msg.thinking_level,
                    message: msg.clone(),
                })
        });

        let route = router
            .route(ModelRouteRequest {
                model: model.clone(),
                thinking_level: options.thinking_level,
                reason: options.reason,
                previous,
                failed,
                state: options.state,
                messages: messages.to_vec(),
                signal: options.signal,
            })
            .await?;

        let routed = format!("{name} routed to {}/{}", route.model.provider, route.model.id);
        let target = self
            .get_physical_model(&route.model.provider, &route.model.id)
            .ok_or_else(|| Error::msg(format!("{routed}, which is not a physical model.")))?;
        if !self.has_configured_auth(&target.provider) {
            return Err(Error::msg(format!("{routed}, which has no credentials.")));
        }
        let clamped = clamp_thinking_level(&target, route.thinking_level);
        Ok(ModelRoute {
            model: target,
            thinking_level: clamped,
            state: route.state,
        })
    }

    pub fn get_physical_model(&self, provider_id: &str, model_id: &str) -> Option<Model> {
        self.inner
            .models
            .get_model(provider_id, model_id)
            .filter(|m| !is_virtual_model(m))
    }
}
