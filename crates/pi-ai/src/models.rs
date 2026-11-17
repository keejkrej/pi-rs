//! Port of packages/ai/src/models.ts

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::{IndexMap, IndexSet};
use tokio::sync::{Mutex as AsyncMutex, Notify};

use pi_js::abort::{AbortController, AbortReason, AbortSignal};
use pi_js::{BoxFuture, Error, Result};

use crate::api::lazy::lazy_stream;
use crate::auth::context::default_provider_auth_context;
use crate::auth::credential_store::InMemoryCredentialStore;
use crate::auth::resolve::{AuthResolutionOverrides, resolve_provider_auth};
use crate::auth::types::{
    ApiKeyAuthInput, AuthCheck, AuthContext, AuthInteraction, AuthOperationOptions, AuthResult, AuthType, Credential,
    CredentialStore, LoginOptions, ModifyCredentialFn, ProviderAuth, ProviderAuthInteraction,
};
use crate::models_store::{InMemoryModelsStore, ModelsStore, ModelsStoreEntry, ModelsStoreOperationOptions};
use crate::types::{
    AnyModel, ApiStreamOptions, AssistantImages, AssistantMessage, ClassifierContext, ClassifierModel,
    ClassifierOptions, ClassifierResult, Context, DeferredCancelOptions, DeferredFetchOptions, DeferredHandle,
    ImageModel, ImagesContext, ImagesOptions, Model, ModelCostRates, ModelThinkingLevel, ModelType, ProviderClassifier,
    ProviderEnv, ProviderHeaders, ProviderImages, ProviderStreams, SimpleStreamOptions, TranscriptContext, Usage,
    UsageCost,
};
use crate::utils::abort::{operation_signal, race_with_abort_signal};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::model_operations::{
    assert_chat_model, assert_classifier_model, assert_image_model, classifier_error_result, image_error_result,
};
use crate::utils::transcript::normalize_context;

pub use crate::utils::model_operations::{get_model_type, is_model_type};
pub use crate::utils::models_error::{ModelsError, ModelsErrorCode};

const EXTENDED_THINKING_LEVELS: [ModelThinkingLevel; 7] = [
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::XHigh,
    ModelThinkingLevel::Max,
];

pub type TransformHeaders = Arc<dyn Fn(ProviderHeaders) -> BoxFuture<Result<ProviderHeaders>> + Send + Sync>;
pub type FilterModelsFn = Arc<dyn Fn(&[Model], Option<&Credential>) -> Vec<Model> + Send + Sync>;
pub type FilterAllModelsFn = Arc<dyn Fn(&[AnyModel], Option<&Credential>) -> Vec<AnyModel> + Send + Sync>;
pub type ModelsPublicationUpdate = Arc<dyn Fn() + Send + Sync>;

/// Provider-selected persisted catalog. Omit to leave storage unchanged; null deletes it.
///
/// PORT: `persist?: ModelsStoreEntry | null` distinguishes omit, null, and a value.
/// `None` leaves storage unchanged, `Some(None)` deletes it, `Some(Some(entry))` writes it.
#[derive(Clone)]
pub struct ModelsPublication {
    pub persist: Option<Option<ModelsStoreEntry>>,
    /// Optional synchronous update of provider-private in-memory catalog state.
    pub update: Option<ModelsPublicationUpdate>,
}

pub type PublishModels = Arc<dyn Fn(ModelsPublication) -> BoxFuture<Result<bool>> + Send + Sync>;

pub struct RefreshModelsContext {
    /// Effective configured credential. OAuth credentials are refreshed before network access.
    pub credential: Option<Credential>,
    /// Immutable provider-scoped catalog snapshot captured before this refresh phase.
    pub stored: Option<ModelsStoreEntry>,
    /// Generation-checked publication. Persistence policy remains provider-owned;
    /// the update runs synchronously only after the selected persistence mutation.
    pub publish: PublishModels,
    /// False during offline/cache-only initialization.
    pub allow_network: bool,
    /// Bypass provider freshness checks and fetch immediately when network access is allowed.
    pub force: Option<bool>,
    /// Always present, including when the public refresh caller omits its optional signal.
    pub signal: AbortSignal,
}

pub type FetchModels = Arc<dyn Fn(RefreshModelsContext) -> BoxFuture<Result<Vec<AnyModel>>> + Send + Sync>;

#[derive(Clone, Debug, Default)]
pub struct ModelsRefreshOptions {
    pub allow_network: Option<bool>,
    /// Restrict refresh to these provider IDs. Unknown and static providers are ignored.
    pub providers: Option<Vec<String>>,
    /// Bypass provider freshness checks and fetch immediately when network access is allowed.
    pub force: Option<bool>,
    pub signal: Option<AbortSignal>,
}

pub struct ModelsRefreshResult {
    pub aborted: bool,
    pub errors: IndexMap<String, Error>,
}

/// Transform fully assembled model/auth/request headers before provider dispatch.
#[derive(Clone, Default)]
pub struct ModelsRequestTransforms {
    pub transform_headers: Option<TransformHeaders>,
}

/// `ApiStreamOptions<TApi> & ModelsRequestTransforms`.
#[derive(Clone)]
pub struct ModelsApiStreamOptions {
    pub base: ApiStreamOptions,
    pub transform_headers: Option<TransformHeaders>,
}

/// `SimpleStreamOptions & ModelsRequestTransforms`.
#[derive(Clone)]
pub struct ModelsSimpleStreamOptions {
    pub base: SimpleStreamOptions,
    pub transform_headers: Option<TransformHeaders>,
}

/// `DeferredFetchOptions & ModelsRequestTransforms`.
#[derive(Clone)]
pub struct ModelsDeferredFetchOptions {
    pub base: DeferredFetchOptions,
    pub transform_headers: Option<TransformHeaders>,
}

/// `DeferredCancelOptions & ModelsRequestTransforms`.
#[derive(Clone)]
pub struct ModelsDeferredCancelOptions {
    pub base: DeferredCancelOptions,
    pub transform_headers: Option<TransformHeaders>,
}

/// `ImagesOptions & ModelsRequestTransforms`.
#[derive(Clone)]
pub struct ModelsImagesOptions {
    pub base: ImagesOptions,
    pub transform_headers: Option<TransformHeaders>,
}

/// `ClassifierOptions & ModelsRequestTransforms`.
#[derive(Clone)]
pub struct ModelsClassifierOptions {
    pub base: ClassifierOptions,
    pub transform_headers: Option<TransformHeaders>,
}

/// A provider is the concrete runtime unit. It owns id/name/base metadata,
/// auth methods, model listing, and the operations its models support
/// (streaming, image generation, classification).
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn base_url(&self) -> Option<&str> {
        None
    }
    fn headers(&self) -> Option<&ProviderHeaders> {
        None
    }
    fn auth(&self) -> &ProviderAuth;

    /// Current known chat models, sync. Must not throw; `Models` treats a throwing
    /// implementation as having no models.
    fn get_models(&self) -> Vec<Model>;

    /// `None` means `getAllModels` was omitted. `Models` then uses [`Provider::get_models`].
    fn get_all_models(&self) -> Option<Vec<AnyModel>> {
        None
    }

    /// `None` means `refreshModels` was omitted (static provider).
    fn refresh_models(&self, context: RefreshModelsContext) -> Option<BoxFuture<Result<()>>> {
        let _ = context;
        None
    }

    /// `None` means `filterModels` was omitted.
    fn filter_models(&self, models: &[Model], credential: Option<&Credential>) -> Option<Vec<Model>> {
        let _ = (models, credential);
        None
    }

    /// `None` means `filterAllModels` was omitted.
    fn filter_all_models(&self, models: &[AnyModel], credential: Option<&Credential>) -> Option<Vec<AnyModel>> {
        let _ = (models, credential);
        None
    }

    /// Stream a normalized transcript. `Models` normalizes the caller's `Context` before dispatching here.
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream;

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream;

    /// `None` means `fetchDeferred` was omitted.
    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        let _ = (model, handle, options);
        None
    }

    /// `None` means `cancelDeferred` was omitted.
    fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredCancelOptions>,
    ) -> Option<BoxFuture<Result<()>>> {
        let _ = (model, handle, options);
        None
    }

    /// `None` means `generateImages` was omitted. The future does not reject.
    fn generate_images(
        &self,
        model: &ImageModel,
        context: &ImagesContext,
        options: Option<ImagesOptions>,
    ) -> Option<BoxFuture<AssistantImages>> {
        let _ = (model, context, options);
        None
    }

    /// `None` means `classify` was omitted. The future does not reject.
    fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<ClassifierOptions>,
    ) -> Option<BoxFuture<ClassifierResult>> {
        let _ = (model, context, options);
        None
    }
}

/// Runtime collection of providers plus auth application and request convenience.
///
/// PORT: TS `Models` and `MutableModels` are one handle. `ModelsImpl` is not exported.
/// `pub type MutableModels = Models`.
#[derive(Clone)]
pub struct Models {
    inner: Arc<ModelsInner>,
}

pub type MutableModels = Models;

struct ModelsInner {
    providers: Mutex<IndexMap<String, Arc<dyn Provider + Send + Sync>>>,
    credentials: Arc<dyn CredentialStore + Send + Sync>,
    models_store: Arc<dyn ModelsStore + Send + Sync>,
    auth_context: Arc<dyn AuthContext>,
    refresh_generations: Mutex<IndexMap<String, i64>>,
    refresh_controllers: Mutex<IndexMap<String, Arc<AbortController>>>,
    publication_chains: Mutex<IndexMap<String, Arc<AsyncMutex<()>>>>,
}

struct ProviderRefresh {
    generation: i64,
    controller: Arc<AbortController>,
}

struct AuthenticatedProvider {
    provider: Arc<dyn Provider + Send + Sync>,
    credential: Option<Credential>,
    #[allow(dead_code)]
    auth: AuthCheck,
}

struct AppliedRequest {
    request_model: AnyModel,
    api_key: Option<String>,
    headers: Option<ProviderHeaders>,
    env: Option<ProviderEnv>,
}

fn abort_error(signal: &AbortSignal) -> Error {
    Error::Abort(signal.reason().unwrap_or_else(AbortReason::abort))
}

async fn race_plain<F, T>(operation: F, signal: &AbortSignal) -> Result<T>
where
    F: Future<Output = T>,
{
    signal.throw_if_aborted()?;
    tokio::select! {
        biased;
        _ = signal.cancelled() => Err(abort_error(signal)),
        res = operation => Ok(res),
    }
}

impl Models {
    /// Create a new [`Models`] registry with optional credential/model stores and auth context.
    pub fn new(options: Option<CreateModelsOptions>) -> Self {
        create_models(options)
    }

    /// Upsert/replace by provider.id. Provider ids are unique.
    pub fn set_provider(&self, provider: Arc<dyn Provider + Send + Sync>) {
        let id = provider.id().to_owned();
        self.supersede_provider_refresh(&id);
        let mut guard = self
            .inner
            .providers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.insert(id, provider);
    }

    pub fn delete_provider(&self, id: &str) {
        self.supersede_provider_refresh(id);
        let mut guard = self
            .inner
            .providers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.shift_remove(id);
    }

    pub fn clear_providers(&self) {
        let ids: Vec<String> = {
            let providers = self
                .inner
                .providers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let controllers = self
                .inner
                .refresh_controllers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut set: IndexSet<String> = providers.keys().cloned().collect();
            for k in controllers.keys() {
                set.insert(k.clone());
            }
            set.into_iter().collect()
        };
        for id in ids {
            self.supersede_provider_refresh(&id);
        }
        let mut guard = self
            .inner
            .providers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.clear();
    }

    pub fn get_providers(&self) -> Vec<Arc<dyn Provider + Send + Sync>> {
        let guard = self
            .inner
            .providers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.values().cloned().collect()
    }

    pub fn get_provider(&self, id: &str) -> Option<Arc<dyn Provider + Send + Sync>> {
        let guard = self
            .inner
            .providers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.get(id).cloned()
    }

    /// Sync read of last-known chat models from one provider or all providers.
    pub fn get_models(&self, provider: Option<&str>) -> Vec<Model> {
        if let Some(provider_id) = provider {
            return self
                .get_provider(provider_id)
                .map(|p| p.get_models())
                .unwrap_or_default();
        }
        self.get_providers().into_iter().flat_map(|p| p.get_models()).collect()
    }

    /// Sync read of last-known models of every type from one provider or all providers.
    pub fn get_all_models(&self, provider: Option<&str>) -> Vec<AnyModel> {
        let all_for_provider = |p: Arc<dyn Provider + Send + Sync>| -> Vec<AnyModel> {
            p.get_all_models()
                .unwrap_or_else(|| p.get_models().into_iter().map(AnyModel::Chat).collect())
        };
        if let Some(provider_id) = provider {
            return self.get_provider(provider_id).map(all_for_provider).unwrap_or_default();
        }
        self.get_providers().into_iter().flat_map(all_for_provider).collect()
    }

    /// Sync read of last-known models of one type from one provider or all providers.
    pub fn get_models_of_type(&self, r#type: ModelType, provider: Option<&str>) -> Vec<AnyModel> {
        self.get_all_models(provider)
            .into_iter()
            .filter(|m| is_model_type(m, r#type))
            .collect()
    }

    /// Sync runtime chat model lookup against last-known lists.
    pub fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
        self.get_models(Some(provider)).into_iter().find(|m| m.id == id)
    }

    /// Sync runtime lookup of a model of one type against last-known lists.
    pub fn get_model_of_type(&self, r#type: ModelType, provider: &str, id: &str) -> Option<AnyModel> {
        self.get_models_of_type(r#type, Some(provider))
            .into_iter()
            .find(|m| m.id() == id)
    }

    /// Refresh selected configured dynamic providers concurrently (all when `providers` is omitted).
    pub async fn refresh(&self, options: Option<ModelsRefreshOptions>) -> Result<ModelsRefreshResult> {
        let opts = options.unwrap_or_default();
        let allow_network = opts.allow_network.unwrap_or(true);
        let force = opts.force;
        let caller_signal = operation_signal(opts.signal.as_ref());
        let errors = Arc::new(Mutex::new(IndexMap::new()));

        if caller_signal.aborted() {
            return Ok(ModelsRefreshResult {
                aborted: true,
                errors: IndexMap::new(),
            });
        }

        let selected: Option<IndexSet<String>> = opts.providers.map(|list| list.into_iter().collect());
        let candidate_providers: Vec<Arc<dyn Provider + Send + Sync>> = self
            .get_providers()
            .into_iter()
            .filter(|p| selected.as_ref().is_none_or(|s| s.contains(p.id())))
            .collect();

        let refresh_all = futures::future::join_all(candidate_providers.into_iter().map(|provider| {
            let models_handle = self.clone();
            let caller_sig = caller_signal.clone();
            let errs = Arc::clone(&errors);
            async move {
                let provider_id = provider.id().to_owned();
                let ProviderRefresh { generation, controller } = models_handle.begin_provider_refresh(&provider_id);
                let combined_signal = AbortSignal::any(&[caller_sig, controller.signal()]);

                let op = async {
                    let (stored_credential, credential_error) =
                        match models_handle.read_credential(&provider_id, &combined_signal).await {
                            Ok(cred) => (cred, None),
                            Err(err) => (None, Some(err)),
                        };

                    // Restore cached provider state before auth resolution or network access.
                    let is_refreshable = models_handle
                        .run_provider_refresh_phase(
                            Arc::clone(&provider),
                            stored_credential.as_ref(),
                            false,
                            None,
                            generation,
                            &combined_signal,
                        )
                        .await?;
                    if !is_refreshable {
                        return Ok(());
                    }

                    if let Some(err) = credential_error {
                        return Err(err);
                    }
                    if !allow_network || combined_signal.aborted() {
                        return Ok(());
                    }

                    let credential = models_handle
                        .resolve_refresh_credential(provider.as_ref(), stored_credential.as_ref(), &combined_signal)
                        .await?;
                    let Some(cred) = credential else {
                        return Ok(());
                    };
                    models_handle
                        .run_provider_refresh_phase(provider, Some(&cred), true, force, generation, &combined_signal)
                        .await?;
                    Ok(())
                };

                if let Err(err) = race_with_abort_signal(op, &combined_signal).await
                    && !combined_signal.aborted()
                {
                    let mut guard = errs.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.insert(provider_id.clone(), err);
                }

                let mut ctrls = models_handle
                    .inner
                    .refresh_controllers
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if ctrls.get(&provider_id).is_some_and(|c| Arc::ptr_eq(c, &controller)) {
                    ctrls.shift_remove(&provider_id);
                }
            }
        }));

        let _ = race_plain(refresh_all, &caller_signal).await;

        let mut err_guard = errors.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let out_errors = std::mem::take(&mut *err_guard);
        Ok(ModelsRefreshResult {
            aborted: caller_signal.aborted(),
            errors: out_errors,
        })
    }

    /// Check whether a provider has complete auth configuration without refreshing OAuth.
    pub async fn check_auth(
        &self,
        provider_id: &str,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<AuthCheck>> {
        let signal = operation_signal(options.as_ref().and_then(|o| o.signal.as_ref()));
        let sig_for_task = signal.clone();
        let pid = provider_id.to_owned();
        let check = async move {
            sig_for_task.throw_if_aborted()?;
            let Some(provider) = self.get_provider(&pid) else {
                return Ok(None);
            };
            let cred = self.read_credential(&pid, &sig_for_task).await?;
            self.check_provider_auth(provider.as_ref(), cred.as_ref(), &sig_for_task)
                .await
        };
        race_with_abort_signal(check, &signal).await
    }

    /// Return chat models whose providers have complete auth configuration.
    pub async fn get_available(
        &self,
        provider_id: Option<&str>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Vec<Model>> {
        let signal = operation_signal(options.as_ref().and_then(|o| o.signal.as_ref()));
        let sig_for_task = signal.clone();
        let available = async move {
            let providers = self.get_authenticated_providers(provider_id, &sig_for_task).await?;
            let mut out = Vec::new();
            for entry in providers {
                let models = entry.provider.get_models();
                let filtered = entry
                    .provider
                    .filter_models(&models, entry.credential.as_ref())
                    .unwrap_or(models);
                out.extend(filtered);
            }
            Ok(out)
        };
        race_with_abort_signal(available, &signal).await
    }

    /// Return models of one type whose providers have complete auth configuration.
    pub async fn get_available_of_type(
        &self,
        r#type: ModelType,
        provider_id: Option<&str>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Vec<AnyModel>> {
        let all = self.get_all_available(provider_id, options).await?;
        Ok(all.into_iter().filter(|m| is_model_type(m, r#type)).collect())
    }

    /// Return models of every type whose providers have complete auth configuration.
    pub async fn get_all_available(
        &self,
        provider_id: Option<&str>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Vec<AnyModel>> {
        let signal = operation_signal(options.as_ref().and_then(|o| o.signal.as_ref()));
        let sig_for_task = signal.clone();
        let available = async move {
            let providers = self.get_authenticated_providers(provider_id, &sig_for_task).await?;
            let mut out = Vec::new();
            for entry in providers {
                let models = entry
                    .provider
                    .get_all_models()
                    .unwrap_or_else(|| entry.provider.get_models().into_iter().map(AnyModel::Chat).collect());
                if let Some(filtered) = entry.provider.filter_all_models(&models, entry.credential.as_ref()) {
                    out.extend(filtered);
                } else if let Some(filtered_chat) = entry
                    .provider
                    .filter_models(&entry.provider.get_models(), entry.credential.as_ref())
                {
                    let available_ids: IndexSet<String> = filtered_chat.into_iter().map(|m| m.id).collect();
                    for m in models {
                        if !is_model_type(&m, ModelType::Chat) || available_ids.contains(m.id()) {
                            out.push(m);
                        }
                    }
                } else {
                    out.extend(models);
                }
            }
            Ok(out)
        };
        race_with_abort_signal(available, &signal).await
    }

    /// `getAuth(providerId, overrides?)`.
    pub async fn get_auth_provider_id(
        &self,
        provider_id: &str,
        overrides: Option<AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>> {
        let signal = operation_signal(overrides.as_ref().and_then(|o| o.signal.as_ref()));
        let Some(provider) = self.get_provider(provider_id) else {
            return Ok(None);
        };
        let mut next_overrides = overrides.unwrap_or_default();
        next_overrides.signal = Some(signal);
        resolve_provider_auth(
            provider.id(),
            provider.auth(),
            self.inner.credentials.as_ref(),
            Arc::clone(&self.inner.auth_context),
            Some(next_overrides),
        )
        .await
    }

    /// `getAuth(model, overrides?)`.
    pub async fn get_auth_model(
        &self,
        model: &AnyModel,
        overrides: Option<AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>> {
        let Some(mut result) = self.get_auth_provider_id(model.provider(), overrides).await? else {
            return Ok(None);
        };
        if let Some(model_headers) = model.headers() {
            let override_headers: ProviderHeaders = model_headers
                .iter()
                .map(|(k, v)| (k.clone(), Some(v.clone())))
                .collect();
            result.auth.headers = merge_headers(result.auth.headers.as_ref(), Some(&override_headers));
        }
        Ok(Some(result))
    }

    /// Run a provider-owned login flow and persist its returned credential.
    pub async fn login(
        &self,
        provider_id: &str,
        r#type: AuthType,
        interaction: Arc<dyn AuthInteraction>,
        options: Option<LoginOptions>,
    ) -> Result<Credential> {
        let signal = operation_signal(interaction.signal());
        signal.throw_if_aborted()?;

        let provider = self.get_provider(provider_id).ok_or_else(|| {
            Error::from(ModelsError::new(
                ModelsErrorCode::Provider,
                format!("Unknown provider: {provider_id}"),
            ))
        })?;

        let provider_interaction = ProviderAuthInteraction::new(interaction, signal.clone());
        let auth = provider.auth();

        let credential = match r#type {
            AuthType::OAuth => {
                let oauth = auth.oauth.as_ref().ok_or_else(|| {
                    Error::from(ModelsError::new(
                        ModelsErrorCode::Auth,
                        format!("{} does not support oauth login", provider.name()),
                    ))
                })?;
                let oauth_cred = race_with_abort_signal(oauth.login(provider_interaction, options), &signal).await?;
                Credential::OAuth(oauth_cred)
            }
            AuthType::ApiKey => {
                let api_key = auth.api_key.as_ref().filter(|a| a.supports_login()).ok_or_else(|| {
                    Error::from(ModelsError::new(
                        ModelsErrorCode::Auth,
                        format!("{} does not support api_key login", provider.name()),
                    ))
                })?;
                let key_cred = race_with_abort_signal(api_key.login(provider_interaction), &signal).await?;
                Credential::ApiKey(key_cred)
            }
        };

        let mutation_started = Arc::new(AtomicBool::new(false));
        let started_notify = Arc::new(Notify::new());

        let cred_for_store = credential.clone();
        let started_flag = Arc::clone(&mutation_started);
        let started_signal = Arc::clone(&started_notify);
        let modify_fn: ModifyCredentialFn = Box::new(move |_current| {
            started_flag.store(true, Ordering::SeqCst);
            started_signal.notify_waiters();
            Box::pin(async move { Ok(Some(cred_for_store)) })
        });

        let credentials = Arc::clone(&self.inner.credentials);
        let pid_owned = provider_id.to_owned();
        let signal_for_modify = signal.clone();
        let mut modify_handle = tokio::spawn(async move {
            credentials
                .modify(
                    &pid_owned,
                    modify_fn,
                    Some(AuthOperationOptions {
                        signal: Some(signal_for_modify),
                    }),
                )
                .await
        });

        let modify_result = tokio::select! {
            res = &mut modify_handle => {
                res.map_err(|e| Error::msg(format!("Credential store modify task panicked: {e}")))?
            }
            _ = started_notify.notified() => {
                modify_handle
                    .await
                    .map_err(|e| Error::msg(format!("Credential store modify task panicked: {e}")))?
            }
            _ = signal.cancelled(), if !mutation_started.load(Ordering::SeqCst) => {
                return Err(abort_error(&signal));
            }
        };

        if let Err(err) = modify_result {
            signal.throw_if_aborted()?;
            return Err(ModelsError::with_cause(
                ModelsErrorCode::Auth,
                format!("Credential store modify failed for {provider_id}"),
                err,
            )
            .into());
        }

        Ok(credential)
    }

    /// Remove the stored credential for a provider.
    pub async fn logout(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<()> {
        let signal = operation_signal(options.as_ref().and_then(|o| o.signal.as_ref()));
        signal.throw_if_aborted()?;
        if let Err(err) = self
            .inner
            .credentials
            .delete(
                provider_id,
                Some(AuthOperationOptions {
                    signal: Some(signal.clone()),
                }),
            )
            .await
        {
            signal.throw_if_aborted()?;
            return Err(ModelsError::with_cause(
                ModelsErrorCode::Auth,
                format!("Credential store delete failed for {provider_id}"),
                err,
            )
            .into());
        }
        Ok(())
    }

    pub fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        let transcript = normalize_context(context);
        let models_handle = self.clone();
        let model_owned = model.clone();
        lazy_stream(model, async move {
            let any_model = AnyModel::Chat(model_owned.clone());
            let provider = models_handle.require_chat_provider(&model_owned)?;
            let (api_key, env, headers, signal, transform_headers) = match options.as_ref() {
                Some(o) => (
                    o.base.base.api_key.as_deref(),
                    o.base.base.env.as_ref(),
                    o.base.base.headers.as_ref(),
                    o.base.base.signal.as_ref(),
                    o.transform_headers.as_ref(),
                ),
                None => (None, None, None, None, None),
            };
            let applied = models_handle
                .apply_auth(&any_model, api_key, env, headers, signal, transform_headers)
                .await?;
            let request_model = applied.request_model.into_chat().unwrap_or(model_owned);
            let mut request_options = options.map(|o| o.base).unwrap_or_default();
            request_options.base.api_key = applied.api_key;
            request_options.base.headers = applied.headers;
            request_options.base.env = applied.env;
            Ok(provider.stream(&request_model, &transcript, Some(request_options)))
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
        let transcript = normalize_context(context);
        let models_handle = self.clone();
        let model_owned = model.clone();
        lazy_stream(model, async move {
            let any_model = AnyModel::Chat(model_owned.clone());
            let provider = models_handle.require_chat_provider(&model_owned)?;
            let (api_key, env, headers, signal, transform_headers) = match options.as_ref() {
                Some(o) => (
                    o.base.base.api_key.as_deref(),
                    o.base.base.env.as_ref(),
                    o.base.base.headers.as_ref(),
                    o.base.base.signal.as_ref(),
                    o.transform_headers.as_ref(),
                ),
                None => (None, None, None, None, None),
            };
            let applied = models_handle
                .apply_auth(&any_model, api_key, env, headers, signal, transform_headers)
                .await?;
            let request_model = applied.request_model.into_chat().unwrap_or(model_owned);
            let mut request_options = options.map(|o| o.base).unwrap_or_default();
            request_options.base.api_key = applied.api_key;
            request_options.base.headers = applied.headers;
            request_options.base.env = applied.env;
            Ok(provider.stream_simple(&request_model, &transcript, Some(request_options)))
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
        let models_handle = self.clone();
        let model_owned = model.clone();
        let handle_owned = handle.clone();
        lazy_stream(model, async move {
            let any_model = AnyModel::Chat(model_owned.clone());
            let provider = models_handle.require_chat_provider(&model_owned)?;
            let (api_key, env, headers, signal, transform_headers) = match options.as_ref() {
                Some(o) => (
                    o.base.base.api_key.as_deref(),
                    o.base.base.env.as_ref(),
                    o.base.base.headers.as_ref(),
                    o.base.base.signal.as_ref(),
                    o.transform_headers.as_ref(),
                ),
                None => (None, None, None, None, None),
            };
            let applied = models_handle
                .apply_auth(&any_model, api_key, env, headers, signal, transform_headers)
                .await?;
            let request_model = applied.request_model.into_chat().unwrap_or(model_owned.clone());
            let mut request_options = options.map(|o| o.base).unwrap_or_default();
            request_options.base.api_key = applied.api_key;
            request_options.base.headers = applied.headers;
            request_options.base.env = applied.env;
            provider
                .fetch_deferred(&request_model, &handle_owned, Some(request_options))
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
        let provider = self.require_chat_provider(model)?;
        let (api_key, env, headers, signal, transform_headers) = match options.as_ref() {
            Some(o) => (
                o.base.api_key.as_deref(),
                o.base.env.as_ref(),
                o.base.headers.as_ref(),
                o.base.signal.as_ref(),
                o.transform_headers.as_ref(),
            ),
            None => (None, None, None, None, None),
        };
        let applied = self
            .apply_auth(&any_model, api_key, env, headers, signal, transform_headers)
            .await?;
        let request_model = applied.request_model.into_chat().unwrap_or_else(|| model.clone());
        let mut request_options = options.map(|o| o.base).unwrap_or_default();
        request_options.api_key = applied.api_key;
        request_options.headers = applied.headers;
        request_options.env = applied.env;
        let fut = provider
            .cancel_deferred(&request_model, handle, Some(request_options))
            .ok_or_else(|| {
                Error::from(ModelsError::new(
                    ModelsErrorCode::Provider,
                    format!("Provider {} does not support deferred responses", model.provider),
                ))
            })?;
        fut.await
    }

    /// Generate images through the owning provider. Never rejects.
    pub async fn generate_images(
        &self,
        model: &ImageModel,
        context: &ImagesContext,
        options: Option<ModelsImagesOptions>,
    ) -> AssistantImages {
        let aborted = options
            .as_ref()
            .and_then(|o| o.base.base.signal.as_ref())
            .is_some_and(|s| s.aborted());
        let any_model = AnyModel::Image(model.clone());
        let res: Result<AssistantImages> = async {
            assert_image_model(&any_model)?;
            let provider = self.require_provider(&any_model)?;
            let (api_key, env, headers, signal, transform_headers) = match options.as_ref() {
                Some(o) => (
                    o.base.base.api_key.as_deref(),
                    o.base.base.env.as_ref(),
                    o.base.base.headers.as_ref(),
                    o.base.base.signal.as_ref(),
                    o.transform_headers.as_ref(),
                ),
                None => (None, None, None, None, None),
            };
            let applied = self
                .apply_auth(&any_model, api_key, env, headers, signal, transform_headers)
                .await?;
            let request_model = applied.request_model.into_image().unwrap_or_else(|| model.clone());
            let mut request_options = options.map(|o| o.base).unwrap_or_default();
            request_options.base.api_key = applied.api_key;
            request_options.base.headers = applied.headers;
            request_options.base.env = applied.env;
            let fut = provider
                .generate_images(&request_model, context, Some(request_options))
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

    /// Classify structured state through the owning provider. Never rejects.
    pub async fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<ModelsClassifierOptions>,
    ) -> ClassifierResult {
        let aborted = options
            .as_ref()
            .and_then(|o| o.base.base.signal.as_ref())
            .is_some_and(|s| s.aborted());
        let any_model = AnyModel::Classifier(model.clone());
        let res: Result<ClassifierResult> = async {
            assert_classifier_model(&any_model)?;
            let provider = self.require_provider(&any_model)?;
            let (api_key, env, headers, signal, transform_headers) = match options.as_ref() {
                Some(o) => (
                    o.base.base.api_key.as_deref(),
                    o.base.base.env.as_ref(),
                    o.base.base.headers.as_ref(),
                    o.base.base.signal.as_ref(),
                    o.transform_headers.as_ref(),
                ),
                None => (None, None, None, None, None),
            };
            let applied = self
                .apply_auth(&any_model, api_key, env, headers, signal, transform_headers)
                .await?;
            let request_model = applied.request_model.into_classifier().unwrap_or_else(|| model.clone());
            let mut request_options = options.map(|o| o.base).unwrap_or_default();
            request_options.base.api_key = applied.api_key;
            request_options.base.headers = applied.headers;
            request_options.base.env = applied.env;
            let fut = provider
                .classify(&request_model, context, Some(request_options))
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
            Ok(out) => out,
            Err(err) => classifier_error_result(model, &err, aborted),
        }
    }

    fn supersede_provider_refresh(&self, provider_id: &str) -> i64 {
        let generation = {
            let mut gens = self
                .inner
                .refresh_generations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let next = gens.get(provider_id).copied().unwrap_or(0) + 1;
            gens.insert(provider_id.to_owned(), next);
            next
        };
        let previous = {
            let mut ctrls = self
                .inner
                .refresh_controllers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            ctrls.shift_remove(provider_id)
        };
        if let Some(prev) = previous {
            prev.abort(None);
        }
        generation
    }

    fn begin_provider_refresh(&self, provider_id: &str) -> ProviderRefresh {
        let generation = self.supersede_provider_refresh(provider_id);
        let controller = Arc::new(AbortController::new());
        {
            let mut ctrls = self
                .inner
                .refresh_controllers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            ctrls.insert(provider_id.to_owned(), Arc::clone(&controller));
        }
        ProviderRefresh { generation, controller }
    }

    fn is_current_generation(&self, provider_id: &str, generation: i64) -> bool {
        let gens = self
            .inner
            .refresh_generations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        gens.get(provider_id).copied() == Some(generation)
    }

    async fn publish_provider_models(
        &self,
        provider_id: &str,
        generation: i64,
        signal: &AbortSignal,
        publication: ModelsPublication,
    ) -> Result<bool> {
        let chain_mutex = {
            let mut chains = self
                .inner
                .publication_chains
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            Arc::clone(
                chains
                    .entry(provider_id.to_owned())
                    .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
            )
        };

        let models_handle = self.clone();
        let pid = provider_id.to_owned();
        let sig = signal.clone();

        race_with_abort_signal(
            async move {
                let _guard = chain_mutex.lock().await;
                if sig.aborted() || !models_handle.is_current_generation(&pid, generation) {
                    return Ok(false);
                }
                match publication.persist {
                    Some(None) => {
                        models_handle
                            .inner
                            .models_store
                            .delete(
                                &pid,
                                Some(ModelsStoreOperationOptions {
                                    signal: Some(sig.clone()),
                                }),
                            )
                            .await?;
                    }
                    Some(Some(entry)) => {
                        models_handle
                            .inner
                            .models_store
                            .write(
                                &pid,
                                entry,
                                Some(ModelsStoreOperationOptions {
                                    signal: Some(sig.clone()),
                                }),
                            )
                            .await?;
                    }
                    None => {}
                }
                if sig.aborted() || !models_handle.is_current_generation(&pid, generation) {
                    return Ok(false);
                }
                if let Some(update) = publication.update {
                    update();
                }
                Ok(true)
            },
            signal,
        )
        .await
    }

    async fn run_provider_refresh_phase(
        &self,
        provider: Arc<dyn Provider + Send + Sync>,
        credential: Option<&Credential>,
        allow_network: bool,
        force: Option<bool>,
        generation: i64,
        signal: &AbortSignal,
    ) -> Result<bool> {
        let stored = self
            .inner
            .models_store
            .read(
                provider.id(),
                Some(ModelsStoreOperationOptions {
                    signal: Some(signal.clone()),
                }),
            )
            .await?
            .map(with_known_model_types);

        let models_handle = self.clone();
        let pid = provider.id().to_owned();
        let sig = signal.clone();
        let publish: PublishModels = Arc::new(move |publication| {
            let mh = models_handle.clone();
            let p = pid.clone();
            let s = sig.clone();
            Box::pin(async move { mh.publish_provider_models(&p, generation, &s, publication).await })
        });

        let ctx = RefreshModelsContext {
            credential: credential.cloned(),
            stored,
            publish,
            allow_network,
            force: if allow_network { force } else { None },
            signal: signal.clone(),
        };

        if let Some(fut) = provider.refresh_models(ctx) {
            fut.await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn resolve_refresh_credential(
        &self,
        provider: &dyn Provider,
        stored: Option<&Credential>,
        signal: &AbortSignal,
    ) -> Result<Option<Credential>> {
        if let Some(Credential::OAuth(oauth_cred)) = stored {
            let Some(oauth) = provider.auth().oauth.as_ref() else {
                return Ok(None);
            };
            if pi_js::time::now_ms() < oauth_cred.expires {
                return Ok(Some(Credential::OAuth(oauth_cred.clone())));
            }
            if signal.aborted() {
                return Ok(None);
            }
            let oauth_arc = Arc::clone(oauth);
            let sig = signal.clone();
            let modify_fn: ModifyCredentialFn = Box::new(move |current| {
                Box::pin(async move {
                    let Some(Credential::OAuth(cur_oauth)) = current else {
                        return Ok(None);
                    };
                    if pi_js::time::now_ms() < cur_oauth.expires {
                        return Ok(None);
                    }
                    let refreshed = oauth_arc.refresh(cur_oauth, sig).await?;
                    Ok(Some(Credential::OAuth(refreshed)))
                })
            });
            let post = self
                .inner
                .credentials
                .modify(
                    provider.id(),
                    modify_fn,
                    Some(AuthOperationOptions {
                        signal: Some(signal.clone()),
                    }),
                )
                .await?;
            return Ok(match post {
                Some(Credential::OAuth(o)) => Some(Credential::OAuth(o)),
                _ => None,
            });
        }

        let Some(api_key) = provider.auth().api_key.as_ref() else {
            return Ok(None);
        };
        let credential = match stored {
            Some(Credential::ApiKey(k)) => Some(k.clone()),
            _ => None,
        };
        let result = api_key
            .resolve(ApiKeyAuthInput {
                ctx: Arc::clone(&self.inner.auth_context),
                credential,
                signal: signal.clone(),
            })
            .await?;
        Ok(result.map(|r| {
            Credential::ApiKey(crate::auth::types::ApiKeyCredential {
                key: r.auth.api_key,
                env: r.env,
            })
        }))
    }

    async fn read_credential(&self, provider_id: &str, signal: &AbortSignal) -> Result<Option<Credential>> {
        self.inner
            .credentials
            .read(
                provider_id,
                Some(AuthOperationOptions {
                    signal: Some(signal.clone()),
                }),
            )
            .await
            .map_err(|err| {
                Error::from(ModelsError::with_cause(
                    ModelsErrorCode::Auth,
                    format!("Credential store read failed for {provider_id}"),
                    err,
                ))
            })
    }

    async fn check_provider_auth(
        &self,
        provider: &dyn Provider,
        credential: Option<&Credential>,
        signal: &AbortSignal,
    ) -> Result<Option<AuthCheck>> {
        if matches!(credential, Some(Credential::OAuth(_))) {
            return Ok(if provider.auth().oauth.is_some() {
                Some(AuthCheck {
                    source: Some("OAuth".to_owned()),
                    auth_type: AuthType::OAuth,
                })
            } else {
                None
            });
        }
        let Some(api_key) = provider.auth().api_key.as_ref() else {
            return Ok(None);
        };
        let api_cred = match credential {
            Some(Credential::ApiKey(c)) => Some(c.clone()),
            _ => None,
        };
        if api_key.supports_check() {
            return api_key
                .check(ApiKeyAuthInput {
                    ctx: Arc::clone(&self.inner.auth_context),
                    credential: api_cred,
                    signal: signal.clone(),
                })
                .await
                .map_err(|err| {
                    Error::from(ModelsError::with_cause(
                        ModelsErrorCode::Auth,
                        format!("API key auth check failed for provider {}", provider.id()),
                        err,
                    ))
                });
        }
        let resolution = resolve_provider_auth(
            provider.id(),
            provider.auth(),
            self.inner.credentials.as_ref(),
            Arc::clone(&self.inner.auth_context),
            Some(AuthResolutionOverrides {
                signal: Some(signal.clone()),
                ..Default::default()
            }),
        )
        .await?;
        Ok(resolution.map(|r| AuthCheck {
            source: Some(r.source.unwrap_or_else(|| "API key".to_owned())),
            auth_type: AuthType::ApiKey,
        }))
    }

    async fn get_authenticated_providers(
        &self,
        provider_id: Option<&str>,
        signal: &AbortSignal,
    ) -> Result<Vec<AuthenticatedProvider>> {
        signal.throw_if_aborted()?;
        let providers: Vec<Arc<dyn Provider + Send + Sync>> = match provider_id {
            Some(id) => self.get_provider(id).into_iter().collect(),
            None => self.get_providers(),
        };
        let checks = futures::future::try_join_all(providers.into_iter().map(|provider| async move {
            let credential = self.read_credential(provider.id(), signal).await?;
            let auth = self
                .check_provider_auth(provider.as_ref(), credential.as_ref(), signal)
                .await?;
            Ok::<_, Error>(auth.map(|auth| AuthenticatedProvider {
                provider,
                credential,
                auth,
            }))
        }))
        .await?;
        Ok(checks.into_iter().flatten().collect())
    }

    fn require_provider(&self, model: &AnyModel) -> Result<Arc<dyn Provider + Send + Sync>> {
        self.get_provider(model.provider()).ok_or_else(|| {
            Error::from(ModelsError::new(
                ModelsErrorCode::Provider,
                format!("Unknown provider: {}", model.provider()),
            ))
        })
    }

    fn require_chat_provider(&self, model: &Model) -> Result<Arc<dyn Provider + Send + Sync>> {
        let any = AnyModel::Chat(model.clone());
        assert_chat_model(&any)?;
        self.require_provider(&any)
    }

    async fn apply_auth(
        &self,
        model: &AnyModel,
        api_key: Option<&str>,
        env: Option<&ProviderEnv>,
        headers: Option<&ProviderHeaders>,
        signal: Option<&AbortSignal>,
        transform_headers: Option<&TransformHeaders>,
    ) -> Result<AppliedRequest> {
        let _provider = self.require_provider(model)?;
        let resolution = self
            .get_auth_model(
                model,
                Some(AuthResolutionOverrides {
                    api_key: api_key.map(ToOwned::to_owned),
                    env: env.cloned(),
                    signal: signal.cloned(),
                    min_oauth_validity_ms: None,
                }),
            )
            .await?
            .ok_or_else(|| {
                Error::from(ModelsError::new(
                    ModelsErrorCode::Auth,
                    format!("Provider is not configured: {}", model.provider()),
                ))
            })?;

        let resolved_api_key = api_key.map(ToOwned::to_owned).or(resolution.auth.api_key);
        let mut merged_headers = merge_headers(resolution.auth.headers.as_ref(), headers);
        if let Some(transform) = transform_headers {
            merged_headers = Some(transform(merged_headers.unwrap_or_default()).await?);
        }

        let merged_env = if resolution.env.is_some() || env.is_some() {
            let mut out = resolution.env.unwrap_or_default();
            if let Some(e) = env {
                for (k, v) in e {
                    out.insert(k.clone(), v.clone());
                }
            }
            Some(out)
        } else {
            None
        };

        let request_model = match (resolution.auth.base_url, model) {
            (Some(base_url), AnyModel::Chat(m)) => {
                let mut cloned = m.clone();
                cloned.base_url = base_url;
                AnyModel::Chat(cloned)
            }
            (Some(base_url), AnyModel::Image(m)) => {
                let mut cloned = m.clone();
                cloned.base_url = base_url;
                AnyModel::Image(cloned)
            }
            (Some(base_url), AnyModel::Classifier(m)) => {
                let mut cloned = m.clone();
                cloned.base_url = base_url;
                AnyModel::Classifier(cloned)
            }
            (_, other) => other.clone(),
        };

        Ok(AppliedRequest {
            request_model,
            api_key: resolved_api_key,
            headers: merged_headers,
            env: merged_env,
        })
    }
}

#[derive(Clone, Default)]
pub struct CreateModelsOptions {
    pub credentials: Option<Arc<dyn CredentialStore + Send + Sync>>,
    pub models_store: Option<Arc<dyn ModelsStore + Send + Sync>>,
    pub auth_context: Option<Arc<dyn AuthContext>>,
}

/// `options` `None` is `{}`.
pub fn create_models(options: Option<CreateModelsOptions>) -> MutableModels {
    let opts = options.unwrap_or_default();
    Models {
        inner: Arc::new(ModelsInner {
            providers: Mutex::new(IndexMap::new()),
            credentials: opts
                .credentials
                .unwrap_or_else(|| Arc::new(InMemoryCredentialStore::new())),
            models_store: opts
                .models_store
                .unwrap_or_else(|| Arc::new(InMemoryModelsStore::new())),
            auth_context: opts.auth_context.unwrap_or_else(default_provider_auth_context),
            refresh_generations: Mutex::new(IndexMap::new()),
            refresh_controllers: Mutex::new(IndexMap::new()),
            publication_chains: Mutex::new(IndexMap::new()),
        }),
    }
}

/// PORT: `ProviderStreams | Partial<Record<TApi, ProviderStreams>>`.
pub enum CreateProviderApi {
    Single(ProviderStreams),
    ByApi(IndexMap<String, ProviderStreams>),
}

/// PORT: `TApi` is erased. `models` is every model type (`AnyModel`).
pub struct CreateProviderOptions {
    pub id: String,
    /// Display name. Default: `id`.
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub headers: Option<ProviderHeaders>,
    /// Required. Every provider has auth semantics, even ambient/keyless ones.
    pub auth: ProviderAuth,
    /// Static baseline models of every type (empty for purely dynamic providers).
    pub models: Vec<AnyModel>,
    pub fetch_models: Option<FetchModels>,
    pub filter_models: Option<FilterModelsFn>,
    pub filter_all_models: Option<FilterAllModelsFn>,
    /// Chat implementation: one for every chat model, or a map keyed by `model.api`.
    pub api: Option<CreateProviderApi>,
    /// Image-generation implementations keyed by `model.api`.
    pub images: Option<IndexMap<String, ProviderImages>>,
    /// Classifier implementations keyed by `model.api`.
    pub classifiers: Option<IndexMap<String, ProviderClassifier>>,
}

struct CreatedProviderState {
    baseline_models: Vec<AnyModel>,
    dynamic_models: Vec<AnyModel>,
}

struct CreatedProvider {
    id: String,
    name: String,
    base_url: Option<String>,
    headers: Option<ProviderHeaders>,
    auth: ProviderAuth,
    fetch_models: Option<FetchModels>,
    filter_models: Option<FilterModelsFn>,
    filter_all_models: Option<FilterAllModelsFn>,
    api: Option<CreateProviderApi>,
    images: Option<IndexMap<String, ProviderImages>>,
    classifiers: Option<IndexMap<String, ProviderClassifier>>,
    state: Arc<Mutex<CreatedProviderState>>,
    has_fetch_deferred: bool,
    has_cancel_deferred: bool,
}

impl CreatedProvider {
    fn current_models(&self) -> Vec<AnyModel> {
        let guard = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut merged = guard.baseline_models.clone();
        for model in &guard.dynamic_models {
            let model_type = get_model_type(model);
            let model_id = model.id();
            if let Some(pos) = merged
                .iter()
                .position(|entry| get_model_type(entry) == model_type && entry.id() == model_id)
            {
                merged[pos] = model.clone();
            } else {
                merged.push(model.clone());
            }
        }
        merged
    }

    fn api_for(&self, api_id: &str) -> Option<&ProviderStreams> {
        match self.api.as_ref()? {
            CreateProviderApi::Single(s) => Some(s),
            CreateProviderApi::ByApi(map) => map.get(api_id),
        }
    }
}

impl Provider for CreatedProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    fn headers(&self) -> Option<&ProviderHeaders> {
        self.headers.as_ref()
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        self.current_models()
            .into_iter()
            .filter_map(|m| m.into_chat())
            .collect()
    }

    fn get_all_models(&self) -> Option<Vec<AnyModel>> {
        Some(self.current_models())
    }

    fn refresh_models(&self, context: RefreshModelsContext) -> Option<BoxFuture<Result<()>>> {
        let fetch = Arc::clone(self.fetch_models.as_ref()?);
        let state = Arc::clone(&self.state);
        let provider_id = self.id.clone();

        Some(Box::pin(async move {
            if let Some(stored) = context.stored.as_ref() {
                let restored: Vec<AnyModel> = stored
                    .models
                    .iter()
                    .filter(|m| m.provider() == provider_id)
                    .cloned()
                    .collect();
                let state_for_update = Arc::clone(&state);
                let restored_for_update = restored;
                let published = (context.publish)(ModelsPublication {
                    persist: None,
                    update: Some(Arc::new(move || {
                        let mut guard = state_for_update.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                        guard.dynamic_models = restored_for_update.clone();
                    })),
                })
                .await?;
                if !published {
                    return Ok(());
                }
            }

            if !context.allow_network || context.signal.aborted() {
                return Ok(());
            }

            let ctx_for_fetch = RefreshModelsContext {
                credential: context.credential.clone(),
                stored: context.stored.clone(),
                publish: Arc::clone(&context.publish),
                allow_network: context.allow_network,
                force: context.force,
                signal: context.signal.clone(),
            };
            let fetched = fetch(ctx_for_fetch).await?;
            if context.signal.aborted() {
                return Ok(());
            }

            let refreshed: Vec<AnyModel> = fetched.into_iter().filter(has_known_model_type).collect();
            let entry = ModelsStoreEntry {
                models: refreshed.clone(),
                last_modified: None,
                checked_at: Some(pi_js::time::now_ms()),
                etag: None,
            };
            (context.publish)(ModelsPublication {
                persist: Some(Some(entry)),
                update: Some(Arc::new(move || {
                    let mut guard = state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.dynamic_models = refreshed.clone();
                })),
            })
            .await?;

            Ok(())
        }))
    }

    fn filter_models(&self, models: &[Model], credential: Option<&Credential>) -> Option<Vec<Model>> {
        self.filter_models.as_ref().map(|f| f(models, credential))
    }

    fn filter_all_models(&self, models: &[AnyModel], credential: Option<&Credential>) -> Option<Vec<AnyModel>> {
        self.filter_all_models.as_ref().map(|f| f(models, credential))
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        if let Some(streams) = self.api_for(&model.api) {
            return streams.stream(model, context, options);
        }
        let provider_id = self.id.clone();
        let model_api = model.api.clone();
        lazy_stream(model, async move {
            Err(Error::from(ModelsError::new(
                ModelsErrorCode::Stream,
                format!("Provider {provider_id} has no API implementation for \"{model_api}\""),
            )))
        })
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        if let Some(streams) = self.api_for(&model.api) {
            return streams.stream_simple(model, context, options);
        }
        let provider_id = self.id.clone();
        let model_api = model.api.clone();
        lazy_stream(model, async move {
            Err(Error::from(ModelsError::new(
                ModelsErrorCode::Stream,
                format!("Provider {provider_id} has no API implementation for \"{model_api}\""),
            )))
        })
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        if !self.has_fetch_deferred {
            return None;
        }
        if let Some(fetch_fn) = self.api_for(&model.api).and_then(|s| s.fetch_deferred.as_ref()) {
            return Some(fetch_fn(model, handle, options));
        }
        let provider_id = self.id.clone();
        let model_api = model.api.clone();
        Some(lazy_stream(model, async move {
            Err(Error::from(ModelsError::new(
                ModelsErrorCode::Provider,
                format!("Provider {provider_id} does not support deferred responses for \"{model_api}\""),
            )))
        }))
    }

    fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredCancelOptions>,
    ) -> Option<BoxFuture<Result<()>>> {
        if !self.has_cancel_deferred {
            return None;
        }
        if let Some(cancel_fn) = self.api_for(&model.api).and_then(|s| s.cancel_deferred.as_ref()) {
            return Some(cancel_fn(model, handle, options));
        }
        let provider_id = self.id.clone();
        let model_api = model.api.clone();
        Some(Box::pin(async move {
            Err(Error::from(ModelsError::new(
                ModelsErrorCode::Provider,
                format!("Provider {provider_id} cannot cancel deferred responses for \"{model_api}\""),
            )))
        }))
    }

    fn generate_images(
        &self,
        model: &ImageModel,
        context: &ImagesContext,
        options: Option<ImagesOptions>,
    ) -> Option<BoxFuture<AssistantImages>> {
        let images_map = self.images.as_ref()?;
        if images_map.is_empty() {
            return None;
        }
        if let Some(impl_) = images_map.get(&model.api) {
            return Some(impl_.generate_images(model, context, options));
        }
        let provider_id = self.id.clone();
        let model_owned = model.clone();
        Some(Box::pin(async move {
            image_error_result(
                &model_owned,
                &Error::from(ModelsError::new(
                    ModelsErrorCode::Provider,
                    format!(
                        "Provider {provider_id} has no image generation implementation for \"{}\"",
                        model_owned.api
                    ),
                )),
                false,
            )
        }))
    }

    fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<ClassifierOptions>,
    ) -> Option<BoxFuture<ClassifierResult>> {
        let classifiers_map = self.classifiers.as_ref()?;
        if classifiers_map.is_empty() {
            return None;
        }
        if let Some(impl_) = classifiers_map.get(&model.api) {
            return Some(impl_.classify(model, context, options));
        }
        let provider_id = self.id.clone();
        let model_owned = model.clone();
        Some(Box::pin(async move {
            classifier_error_result(
                &model_owned,
                &Error::from(ModelsError::new(
                    ModelsErrorCode::Provider,
                    format!(
                        "Provider {provider_id} has no classifier implementation for \"{}\"",
                        model_owned.api
                    ),
                )),
                false,
            )
        }))
    }
}

/// Builds a provider from parts. At least one concrete implementation across
/// `api` / `images` / `classifiers` is required; empty maps are rejected.
pub fn create_provider(input: CreateProviderOptions) -> Result<Arc<dyn Provider + Send + Sync>> {
    let stream_count = match input.api.as_ref() {
        Some(CreateProviderApi::Single(_)) => 1,
        Some(CreateProviderApi::ByApi(map)) => map.len(),
        None => 0,
    };
    let image_count = input.images.as_ref().map_or(0, |m| m.len());
    let classifier_count = input.classifiers.as_ref().map_or(0, |m| m.len());

    if stream_count == 0 && image_count == 0 && classifier_count == 0 {
        return Err(Error::msg(format!(
            "Provider {}: at least one of \"api\", \"images\", or \"classifiers\" is required.",
            input.id
        )));
    }

    let (has_fetch_deferred, has_cancel_deferred) = match input.api.as_ref() {
        Some(CreateProviderApi::Single(s)) => (s.fetch_deferred.is_some(), s.cancel_deferred.is_some()),
        Some(CreateProviderApi::ByApi(map)) => (
            map.values().any(|s| s.fetch_deferred.is_some()),
            map.values().any(|s| s.cancel_deferred.is_some()),
        ),
        None => (false, false),
    };

    let name = input.name.unwrap_or_else(|| input.id.clone());
    Ok(Arc::new(CreatedProvider {
        id: input.id,
        name,
        base_url: input.base_url,
        headers: input.headers,
        auth: input.auth,
        fetch_models: input.fetch_models,
        filter_models: input.filter_models,
        filter_all_models: input.filter_all_models,
        api: input.api,
        images: input.images,
        classifiers: input.classifiers,
        state: Arc::new(Mutex::new(CreatedProviderState {
            baseline_models: input.models,
            dynamic_models: Vec::new(),
        })),
        has_fetch_deferred,
        has_cancel_deferred,
    }))
}

/// Runtime-checked narrowing for dynamically looked-up models.
/// Non-chat models never match, even when their api id equals `api`.
pub fn has_api(model: &AnyModel, api: &str) -> bool {
    is_model_type(model, ModelType::Chat) && model.api() == api
}

// Anthropic charges 2x base input for 1h cache writes.
pub fn calculate_cost(model: &AnyModel, usage: &mut Usage) -> UsageCost {
    let Some(cost) = model.cost() else {
        return usage.cost;
    };
    let input_tokens = usage.input + usage.cache_read + usage.cache_write;
    let mut rates: ModelCostRates = cost.rates();
    let mut matched_threshold: i64 = -1;
    if let Some(tiers) = cost.tiers.as_ref() {
        for tier in tiers {
            if input_tokens > tier.input_tokens_above && tier.input_tokens_above > matched_threshold {
                rates = ModelCostRates {
                    input: tier.input,
                    output: tier.output,
                    cache_read: tier.cache_read,
                    cache_write: tier.cache_write,
                };
                matched_threshold = tier.input_tokens_above;
            }
        }
    }

    let long_write = usage.cache_write_1h.unwrap_or(0);
    let short_write = usage.cache_write - long_write;
    usage.cost.input = (rates.input / 1_000_000.0) * usage.input as f64;
    usage.cost.output = (rates.output / 1_000_000.0) * usage.output as f64;
    usage.cost.cache_read = (rates.cache_read / 1_000_000.0) * usage.cache_read as f64;
    usage.cost.cache_write =
        (rates.cache_write * short_write as f64 + rates.input * 2.0 * long_write as f64) / 1_000_000.0;
    usage.cost.total = usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
    usage.cost
}

pub fn get_supported_thinking_levels(model: &Model) -> Vec<ModelThinkingLevel> {
    if !model.reasoning {
        return vec![ModelThinkingLevel::Off];
    }

    EXTENDED_THINKING_LEVELS
        .iter()
        .copied()
        .filter(|&level| {
            let mapped = model.thinking_level_map.as_ref().and_then(|m| m.get(&level));
            match mapped {
                Some(None) => false,
                None if matches!(level, ModelThinkingLevel::XHigh | ModelThinkingLevel::Max) => false,
                _ => true,
            }
        })
        .collect()
}

pub fn clamp_thinking_level(model: &Model, level: ModelThinkingLevel) -> ModelThinkingLevel {
    let available = get_supported_thinking_levels(model);
    if available.contains(&level) {
        return level;
    }

    let Some(requested_index) = EXTENDED_THINKING_LEVELS.iter().position(|&l| l == level) else {
        return available.first().copied().unwrap_or(ModelThinkingLevel::Off);
    };

    for &candidate in &EXTENDED_THINKING_LEVELS[requested_index..] {
        if available.contains(&candidate) {
            return candidate;
        }
    }
    for &candidate in EXTENDED_THINKING_LEVELS[..requested_index].iter().rev() {
        if available.contains(&candidate) {
            return candidate;
        }
    }
    available.first().copied().unwrap_or(ModelThinkingLevel::Off)
}

/// Check if two models are equal by comparing their type, id, and provider.
pub fn models_are_equal(a: Option<&AnyModel>, b: Option<&AnyModel>) -> bool {
    let (Some(a), Some(b)) = (a, b) else {
        return false;
    };
    get_model_type(a) == get_model_type(b) && a.id() == b.id() && a.provider() == b.provider()
}

fn has_known_model_type(model: &AnyModel) -> bool {
    !matches!(model, AnyModel::Unknown(_))
}

fn with_known_model_types(mut entry: ModelsStoreEntry) -> ModelsStoreEntry {
    entry.models.retain(has_known_model_type);
    entry
}

fn merge_headers(
    base: Option<&ProviderHeaders>,
    override_headers: Option<&ProviderHeaders>,
) -> Option<ProviderHeaders> {
    if base.is_none() && override_headers.is_none() {
        return None;
    }
    let mut merged = base.cloned().unwrap_or_default();
    if let Some(ovr) = override_headers {
        for (name, value) in ovr {
            let lower = name.to_ascii_lowercase();
            merged.retain(|existing_name, _| existing_name.to_ascii_lowercase() != lower);
            merged.insert(name.clone(), value.clone());
        }
    }
    Some(merged)
}
