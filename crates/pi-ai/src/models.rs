//! Port of packages/ai/src/models.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;

use pi_js::abort::{AbortController, AbortSignal};
use pi_js::{BoxFuture, Error, Result};

use crate::auth::resolve::AuthResolutionOverrides;
use crate::auth::types::{
    AuthCheck, AuthContext, AuthInteraction, AuthOperationOptions, AuthResult, AuthType, Credential, CredentialStore,
    LoginOptions, ProviderAuth,
};
use crate::models_store::{ModelsStore, ModelsStoreEntry};
use crate::types::{
    AnyModel, ApiStreamOptions, AssistantImages, AssistantMessage, ClassifierContext, ClassifierModel,
    ClassifierOptions, ClassifierResult, Context, DeferredCancelOptions, DeferredFetchOptions, DeferredHandle,
    ImageModel, ImagesContext, ImagesOptions, Model, ModelThinkingLevel, ModelType, ProviderClassifier, ProviderEnv,
    ProviderHeaders, ProviderImages, ProviderStreams, SimpleStreamOptions, TranscriptContext, Usage, UsageCost,
};
use crate::utils::event_stream::AssistantMessageEventStream;

pub use crate::utils::model_operations::{get_model_type, is_model_type};
pub use crate::utils::models_error::{ModelsError, ModelsErrorCode};

const KNOWN_MODEL_TYPES: [&str; 3] = ["chat", "image", "classifier"];
const EXTENDED_THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

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
///
/// PORT: TS intersection. `base` is the stream options; `transform_headers` is the Models-only field.
/// `TApi` is erased.
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
///
/// PORT: `Provider<TApi>` is erased to this trait. Optional TS methods return `None` when absent
/// so callers can tell "not implemented" from an empty result. `generate_images` and `classify`
/// resolve to the message (they never reject); failures are error results.
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
    auth_context: Arc<dyn AuthContext + Send + Sync>,
    refresh_generations: Mutex<IndexMap<String, i64>>,
    refresh_controllers: Mutex<IndexMap<String, AbortController>>,
    /// PORT: JS `publicationChains` promise chain. The tokio mutex is the per-provider serialization lock (§4.4).
    publication_chains: Mutex<IndexMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

struct ProviderRefresh {
    generation: i64,
    controller: AbortController,
}

struct AuthenticatedProvider {
    provider: Arc<dyn Provider + Send + Sync>,
    credential: Option<Credential>,
    auth: AuthCheck,
}

/// PORT: TS `applyAuth` also spreads the remaining request options. This extracts the auth overlay;
/// the caller keeps the other option fields.
struct AppliedRequest {
    request_model: AnyModel,
    api_key: Option<String>,
    headers: Option<ProviderHeaders>,
    env: Option<ProviderEnv>,
}

impl Models {
    /// Upsert/replace by provider.id. Provider ids are unique.
    pub fn set_provider(&self, provider: Arc<dyn Provider + Send + Sync>) {
        todo!("port: Models::set_provider")
    }

    pub fn delete_provider(&self, id: &str) {
        todo!("port: Models::delete_provider")
    }

    pub fn clear_providers(&self) {
        todo!("port: Models::clear_providers")
    }

    pub fn get_providers(&self) -> Vec<Arc<dyn Provider + Send + Sync>> {
        todo!("port: Models::get_providers")
    }

    pub fn get_provider(&self, id: &str) -> Option<Arc<dyn Provider + Send + Sync>> {
        todo!("port: Models::get_provider")
    }

    /// Sync read of last-known chat models from one provider or all providers.
    /// Best-effort: a provider whose `getModels()` throws yields no models.
    ///
    /// `provider` `None` reads every provider.
    pub fn get_models(&self, provider: Option<&str>) -> Vec<Model> {
        todo!("port: Models::get_models")
    }

    /// Sync read of last-known models of every type from one provider or all providers.
    pub fn get_all_models(&self, provider: Option<&str>) -> Vec<AnyModel> {
        todo!("port: Models::get_all_models")
    }

    /// Sync read of last-known models of one type from one provider or all providers.
    ///
    /// PORT: `ModelTypeMap[TType]` is erased to [`AnyModel`]. Narrow with [`is_model_type`].
    pub fn get_models_of_type(&self, r#type: ModelType, provider: Option<&str>) -> Vec<AnyModel> {
        todo!("port: Models::get_models_of_type")
    }

    /// Sync runtime chat model lookup against last-known lists.
    pub fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
        todo!("port: Models::get_model")
    }

    /// Sync runtime lookup of a model of one type against last-known lists.
    pub fn get_model_of_type(&self, r#type: ModelType, provider: &str, id: &str) -> Option<AnyModel> {
        todo!("port: Models::get_model_of_type")
    }

    /// Refresh selected configured dynamic providers concurrently (all when `providers` is omitted).
    ///
    /// `options` `None` is `{}`.
    pub async fn refresh(&self, options: Option<ModelsRefreshOptions>) -> Result<ModelsRefreshResult> {
        todo!("port: Models::refresh")
    }

    /// Check whether a provider has complete auth configuration without refreshing OAuth.
    pub async fn check_auth(
        &self,
        provider_id: &str,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<AuthCheck>> {
        todo!("port: Models::check_auth")
    }

    /// Return chat models whose providers have complete auth configuration.
    ///
    /// `provider_id` `None` checks every provider.
    pub async fn get_available(
        &self,
        provider_id: Option<&str>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Vec<Model>> {
        todo!("port: Models::get_available")
    }

    /// Return models of one type whose providers have complete auth configuration.
    pub async fn get_available_of_type(
        &self,
        r#type: ModelType,
        provider_id: Option<&str>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Vec<AnyModel>> {
        todo!("port: Models::get_available_of_type")
    }

    /// Return models of every type whose providers have complete auth configuration.
    pub async fn get_all_available(
        &self,
        provider_id: Option<&str>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Vec<AnyModel>> {
        todo!("port: Models::get_all_available")
    }

    /// `getAuth(providerId, overrides?)`.
    pub async fn get_auth_provider_id(
        &self,
        provider_id: &str,
        overrides: Option<AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>> {
        todo!("port: Models::get_auth_provider_id")
    }

    /// `getAuth(model, overrides?)`.
    pub async fn get_auth_model(
        &self,
        model: &AnyModel,
        overrides: Option<AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>> {
        todo!("port: Models::get_auth_model")
    }

    /// Run a provider-owned login flow and persist its returned credential.
    pub async fn login(
        &self,
        provider_id: &str,
        r#type: AuthType,
        interaction: AuthInteraction,
        options: Option<LoginOptions>,
    ) -> Result<Credential> {
        todo!("port: Models::login")
    }

    /// Remove the stored credential for a provider.
    pub async fn logout(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<()> {
        todo!("port: Models::logout")
    }

    pub fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        todo!("port: Models::stream")
    }

    pub async fn complete(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsApiStreamOptions>,
    ) -> AssistantMessage {
        todo!("port: Models::complete")
    }

    pub fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsSimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        todo!("port: Models::stream_simple")
    }

    pub async fn complete_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsSimpleStreamOptions>,
    ) -> AssistantMessage {
        todo!("port: Models::complete_simple")
    }

    pub fn stream_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<ModelsDeferredFetchOptions>,
    ) -> AssistantMessageEventStream {
        todo!("port: Models::stream_deferred")
    }

    pub async fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<ModelsDeferredFetchOptions>,
    ) -> AssistantMessage {
        todo!("port: Models::fetch_deferred")
    }

    pub async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<ModelsDeferredCancelOptions>,
    ) -> Result<()> {
        todo!("port: Models::cancel_deferred")
    }

    /// Generate images through the owning provider. Never rejects.
    pub async fn generate_images(
        &self,
        model: &ImageModel,
        context: &ImagesContext,
        options: Option<ModelsImagesOptions>,
    ) -> AssistantImages {
        todo!("port: Models::generate_images")
    }

    /// Classify structured state through the owning provider. Never rejects.
    pub async fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<ModelsClassifierOptions>,
    ) -> ClassifierResult {
        todo!("port: Models::classify")
    }

    fn supersede_provider_refresh(&self, provider_id: &str) -> i64 {
        todo!("port: Models::supersede_provider_refresh")
    }

    fn begin_provider_refresh(&self, provider_id: &str) -> ProviderRefresh {
        todo!("port: Models::begin_provider_refresh")
    }

    async fn publish_provider_models(
        &self,
        provider_id: &str,
        generation: i64,
        signal: &AbortSignal,
        publication: ModelsPublication,
    ) -> Result<bool> {
        todo!("port: Models::publish_provider_models")
    }

    async fn run_provider_refresh_phase(
        &self,
        provider: Arc<dyn Provider + Send + Sync>,
        credential: Option<&Credential>,
        allow_network: bool,
        force: Option<bool>,
        generation: i64,
        signal: &AbortSignal,
    ) -> Result<()> {
        todo!("port: Models::run_provider_refresh_phase")
    }

    async fn resolve_refresh_credential(
        &self,
        provider: &dyn Provider,
        stored: Option<&Credential>,
        signal: &AbortSignal,
    ) -> Result<Option<Credential>> {
        todo!("port: Models::resolve_refresh_credential")
    }

    async fn read_credential(&self, provider_id: &str, signal: &AbortSignal) -> Result<Option<Credential>> {
        todo!("port: Models::read_credential")
    }

    async fn check_provider_auth(
        &self,
        provider: &dyn Provider,
        credential: Option<&Credential>,
        signal: &AbortSignal,
    ) -> Result<Option<AuthCheck>> {
        todo!("port: Models::check_provider_auth")
    }

    async fn get_authenticated_providers(
        &self,
        provider_id: Option<&str>,
        signal: &AbortSignal,
    ) -> Result<Vec<AuthenticatedProvider>> {
        todo!("port: Models::get_authenticated_providers")
    }

    fn require_provider(&self, model: &AnyModel) -> Result<Arc<dyn Provider + Send + Sync>> {
        todo!("port: Models::require_provider")
    }

    fn require_chat_provider(&self, model: &Model) -> Result<Arc<dyn Provider + Send + Sync>> {
        todo!("port: Models::require_chat_provider")
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
        todo!("port: Models::apply_auth")
    }
}

#[derive(Clone, Default)]
pub struct CreateModelsOptions {
    pub credentials: Option<Arc<dyn CredentialStore + Send + Sync>>,
    pub models_store: Option<Arc<dyn ModelsStore + Send + Sync>>,
    pub auth_context: Option<Arc<dyn AuthContext + Send + Sync>>,
}

/// `options` `None` is `{}`.
pub fn create_models(options: Option<CreateModelsOptions>) -> MutableModels {
    todo!("port: create_models")
}

/// PORT: `ProviderStreams | Partial<Record<TApi, ProviderStreams>>`. A single implementation versus a map keyed by `model.api`.
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
    state: Mutex<CreatedProviderState>,
}

impl Provider for CreatedProvider {
    fn id(&self) -> &str {
        todo!("port: CreatedProvider::id")
    }

    fn name(&self) -> &str {
        todo!("port: CreatedProvider::name")
    }

    fn base_url(&self) -> Option<&str> {
        todo!("port: CreatedProvider::base_url")
    }

    fn headers(&self) -> Option<&ProviderHeaders> {
        todo!("port: CreatedProvider::headers")
    }

    fn auth(&self) -> &ProviderAuth {
        todo!("port: CreatedProvider::auth")
    }

    fn get_models(&self) -> Vec<Model> {
        todo!("port: CreatedProvider::get_models")
    }

    fn get_all_models(&self) -> Option<Vec<AnyModel>> {
        todo!("port: CreatedProvider::get_all_models")
    }

    fn refresh_models(&self, context: RefreshModelsContext) -> Option<BoxFuture<Result<()>>> {
        todo!("port: CreatedProvider::refresh_models")
    }

    fn filter_models(&self, models: &[Model], credential: Option<&Credential>) -> Option<Vec<Model>> {
        todo!("port: CreatedProvider::filter_models")
    }

    fn filter_all_models(&self, models: &[AnyModel], credential: Option<&Credential>) -> Option<Vec<AnyModel>> {
        todo!("port: CreatedProvider::filter_all_models")
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        todo!("port: CreatedProvider::stream")
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        todo!("port: CreatedProvider::stream_simple")
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        todo!("port: CreatedProvider::fetch_deferred")
    }

    fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredCancelOptions>,
    ) -> Option<BoxFuture<Result<()>>> {
        todo!("port: CreatedProvider::cancel_deferred")
    }

    fn generate_images(
        &self,
        model: &ImageModel,
        context: &ImagesContext,
        options: Option<ImagesOptions>,
    ) -> Option<BoxFuture<AssistantImages>> {
        todo!("port: CreatedProvider::generate_images")
    }

    fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<ClassifierOptions>,
    ) -> Option<BoxFuture<ClassifierResult>> {
        todo!("port: CreatedProvider::classify")
    }
}

/// Builds a provider from parts. At least one concrete implementation across
/// `api` / `images` / `classifiers` is required; empty maps are rejected.
pub fn create_provider(input: CreateProviderOptions) -> Result<Arc<dyn Provider + Send + Sync>> {
    todo!("port: create_provider")
}

/// Runtime-checked narrowing for dynamically looked-up models.
/// Non-chat models never match, even when their api id equals `api`.
///
/// PORT: the TS type predicate is erased. `api` is the api id string.
pub fn has_api(model: &AnyModel, api: &str) -> bool {
    todo!("port: has_api")
}

// Anthropic charges 2x base input for 1h cache writes.
pub fn calculate_cost(model: &AnyModel, usage: &mut Usage) -> UsageCost {
    todo!("port: calculate_cost")
}

pub fn get_supported_thinking_levels(model: &Model) -> Vec<ModelThinkingLevel> {
    todo!("port: get_supported_thinking_levels")
}

pub fn clamp_thinking_level(model: &Model, level: ModelThinkingLevel) -> ModelThinkingLevel {
    todo!("port: clamp_thinking_level")
}

/// Check if two models are equal by comparing their type, id, and provider.
/// Returns false if either model is null or undefined.
pub fn models_are_equal(a: Option<&AnyModel>, b: Option<&AnyModel>) -> bool {
    todo!("port: models_are_equal")
}

fn has_known_model_type(model: &AnyModel) -> bool {
    todo!("port: has_known_model_type")
}

fn with_known_model_types(entry: ModelsStoreEntry) -> ModelsStoreEntry {
    todo!("port: with_known_model_types")
}

fn merge_headers(
    base: Option<&ProviderHeaders>,
    override_headers: Option<&ProviderHeaders>,
) -> Option<ProviderHeaders> {
    todo!("port: merge_headers")
}
