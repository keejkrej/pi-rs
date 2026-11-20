//! Port of packages/coding-agent/src/core/virtual-models.ts

use std::sync::{Arc, LazyLock};

use indexmap::IndexSet;
use pi_agent_core::AgentMessage;
use pi_ai::api::lazy::lazy_stream;
use pi_ai::models::{Provider, RefreshModelsContext};
use pi_ai::types::{
    AnyModel, ApiStreamOptions, AssistantImages, AssistantMessage, ClassifierContext, ClassifierModel,
    ClassifierOptions, ClassifierResult, DeferredCancelOptions, DeferredFetchOptions, DeferredHandle, ImageModel,
    ImagesContext, ImagesOptions, Message, Model, ModelCost, ModelInput, ModelThinkingLevel, ProviderHeaders,
    SimpleStreamOptions, StopReason, ThinkingLevelMap, TranscriptContext,
};
use pi_ai::{
    ApiKeyAuth, ApiKeyAuthInput, AssistantMessageEventStream, AuthResult, Credential, ModelAuth, ProviderAuth,
    is_model_type,
};
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::session_manager::SessionEntry;

/// API id of virtual catalog entries. Requests for it fail unless routed first.
pub const VIRTUAL_MODEL_API: &str = "pi-virtual";

/// Custom entry type that stores router state on the session branch.
pub const VIRTUAL_MODEL_STATE_ENTRY: &str = "pi.virtual-model-state";

const THINKING_LEVELS: &[ModelThinkingLevel] = &[
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::XHigh,
    ModelThinkingLevel::Max,
];

/// Data of a `pi.virtual-model-state` custom entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualModelStateData {
    pub provider: String,
    pub model_id: String,
    #[serde(default)]
    pub state: Value,
}

/// Why a request is being routed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelRouteReason {
    User,
    Continuation,
    Retry,
    Direct,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelRoutePrevious {
    pub model: Model,
    pub thinking_level: Option<ModelThinkingLevel>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelRouteFailed {
    pub model: Model,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub message: AssistantMessage,
}

#[derive(Clone, Debug)]
pub struct ModelRouteRequest {
    pub model: Model,
    pub thinking_level: ModelThinkingLevel,
    pub reason: ModelRouteReason,
    pub previous: Option<ModelRoutePrevious>,
    pub failed: Option<ModelRouteFailed>,
    pub state: Option<Value>,
    pub messages: Vec<Message>,
    pub signal: Option<AbortSignal>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelRoute {
    pub model: Model,
    pub thinking_level: ModelThinkingLevel,
    pub state: Option<Value>,
}

#[async_trait::async_trait]
pub trait VirtualModelRouter: Send + Sync {
    async fn route(&self, request: ModelRouteRequest) -> Result<ModelRoute>;
}

#[derive(Clone)]
pub struct VirtualModelDefinition {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub thinking_levels: Option<Vec<ModelThinkingLevel>>,
    pub context_window: Option<i64>,
    pub max_tokens: Option<i64>,
    pub input: Option<Vec<ModelInput>>,
    pub router: Arc<dyn VirtualModelRouter>,
}

impl std::fmt::Debug for VirtualModelDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualModelDefinition")
            .field("provider", &self.provider)
            .field("id", &self.id)
            .field("name", &self.name)
            .field("thinking_levels", &self.thinking_levels)
            .field("context_window", &self.context_window)
            .field("max_tokens", &self.max_tokens)
            .field("input", &self.input)
            .finish()
    }
}

#[derive(Clone, Debug, Default)]
pub struct VirtualModelDescriptor {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub thinking_levels: Option<Vec<ModelThinkingLevel>>,
    pub context_window: Option<i64>,
    pub max_tokens: Option<i64>,
    pub input: Option<Vec<ModelInput>>,
}

impl From<&VirtualModelDefinition> for VirtualModelDescriptor {
    fn from(def: &VirtualModelDefinition) -> Self {
        Self {
            provider: def.provider.clone(),
            id: def.id.clone(),
            name: def.name.clone(),
            thinking_levels: def.thinking_levels.clone(),
            context_window: def.context_window,
            max_tokens: def.max_tokens,
            input: def.input.clone(),
        }
    }
}

/// Whether a model or message names a virtual model.
pub fn is_virtual_model(model: &Model) -> bool {
    model.api == VIRTUAL_MODEL_API
}

/// Whether an `AnyModel` names a virtual model.
pub fn is_virtual_any_model(model: &AnyModel) -> bool {
    model.api() == VIRTUAL_MODEL_API
}

/// Whether an `AssistantMessage` names a virtual model.
pub fn is_virtual_assistant_message(message: &AssistantMessage) -> bool {
    message.api == VIRTUAL_MODEL_API
}

/// Latest successful response. Failed or aborted requests, including failed routing, are skipped.
pub fn find_latest_response(messages: &[AgentMessage]) -> Option<&AssistantMessage> {
    for message in messages.iter().rev() {
        if let Some(Message::Assistant(assistant)) = message.as_message()
            && assistant.stop_reason != StopReason::Error
            && assistant.stop_reason != StopReason::Aborted
        {
            return Some(assistant);
        }
    }
    None
}

/// The model selection a session branch records.
pub fn get_branch_selection<'a, F>(branch: &'a [SessionEntry], get_model: F) -> Option<(String, String)>
where
    F: Fn(&str, &str) -> Option<Model>,
{
    for (idx, entry) in branch.iter().enumerate().rev() {
        match entry {
            SessionEntry::ModelChange(change) => {
                return Some((change.provider.clone(), change.model_id.clone()));
            }
            SessionEntry::Message(msg_entry) => {
                if let Some(Message::Assistant(assistant)) = msg_entry.message.as_message()
                    && !is_virtual_assistant_message(assistant)
                {
                    let response = (assistant.provider.clone(), assistant.model.clone());
                    if let Some((change_provider, change_id)) = find_last_model_change(branch, idx)
                        && let Some(model) = get_model(&change_provider, &change_id)
                        && is_virtual_model(&model)
                    {
                        return Some((change_provider, change_id));
                    }
                    return Some(response);
                }
            }
            _ => {}
        }
    }
    None
}

fn find_last_model_change(branch: &[SessionEntry], before: usize) -> Option<(String, String)> {
    for entry in branch[..before].iter().rev() {
        if let SessionEntry::ModelChange(change) = entry {
            return Some((change.provider.clone(), change.model_id.clone()));
        }
    }
    None
}

/// Latest router state a session branch stores for a virtual model.
pub fn get_virtual_model_state(branch: &[SessionEntry], provider: &str, model_id: &str) -> Option<Value> {
    for entry in branch.iter().rev() {
        let SessionEntry::Custom(custom) = entry else {
            continue;
        };
        if custom.custom_type != VIRTUAL_MODEL_STATE_ENTRY {
            continue;
        }
        let Some(ref raw) = custom.data else {
            continue;
        };
        if let Ok(parsed) = serde_json::from_value::<VirtualModelStateData>(raw.clone())
            && parsed.provider == provider
            && parsed.model_id == model_id
        {
            return Some(parsed.state);
        }
    }
    None
}

/// Build the catalog entry of a virtual model.
pub fn create_virtual_model(definition: &VirtualModelDescriptor) -> Model {
    let levels = definition
        .thinking_levels
        .clone()
        .unwrap_or_else(|| vec![ModelThinkingLevel::Off]);
    let mut thinking_level_map: ThinkingLevelMap = ThinkingLevelMap::new();
    for &level in THINKING_LEVELS {
        let mapped = if levels.contains(&level) {
            Some(level.as_str().to_owned())
        } else {
            None
        };
        thinking_level_map.insert(level, mapped);
    }

    Model {
        r#type: None,
        id: definition.id.clone(),
        name: definition.name.clone(),
        api: VIRTUAL_MODEL_API.to_owned(),
        provider: definition.provider.clone(),
        base_url: String::new(),
        reasoning: levels.iter().any(|&level| level != ModelThinkingLevel::Off),
        thinking_level_map: Some(thinking_level_map),
        prompt_cache: None,
        input: definition
            .input
            .clone()
            .unwrap_or_else(|| vec![ModelInput::Text, ModelInput::Image]),
        input_limits: None,
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: definition.context_window.unwrap_or(0),
        max_tokens: definition.max_tokens.unwrap_or(0),
        headers: None,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

fn unrouted_stream(model: &Model) -> AssistantMessageEventStream {
    let provider = model.provider.clone();
    let id = model.id.clone();
    lazy_stream(model, async move {
        Err(Error::msg(format!(
            "Virtual model {provider}/{id} must be routed before streaming"
        )))
    })
}

struct VirtualApiKeyAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for VirtualApiKeyAuth {
    fn name(&self) -> &str {
        "Virtual model"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        Ok(Some(AuthResult {
            auth: ModelAuth::default(),
            env: None,
            source: Some("virtual".to_owned()),
        }))
    }
}

static VIRTUAL_PROVIDER_AUTH: LazyLock<ProviderAuth> = LazyLock::new(|| ProviderAuth {
    api_key: Some(Arc::new(VirtualApiKeyAuth)),
    oauth: None,
});

struct VirtualOnlyProvider {
    provider_id: String,
    virtual_models: Vec<Model>,
}

impl Provider for VirtualOnlyProvider {
    fn id(&self) -> &str {
        &self.provider_id
    }

    fn name(&self) -> &str {
        &self.provider_id
    }

    fn auth(&self) -> &ProviderAuth {
        &VIRTUAL_PROVIDER_AUTH
    }

    fn get_models(&self) -> Vec<Model> {
        self.virtual_models.clone()
    }

    fn stream(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        unrouted_stream(model)
    }

    fn stream_simple(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        unrouted_stream(model)
    }
}

struct VirtualWrappedProvider {
    inner: Arc<dyn Provider + Send + Sync>,
    virtual_models: Vec<Model>,
    virtual_ids: IndexSet<String>,
}

impl VirtualWrappedProvider {
    fn physical_chat(&self, models: &[Model]) -> Vec<Model> {
        models
            .iter()
            .filter(|model| !is_virtual_model(model) && !self.virtual_ids.contains(&model.id))
            .cloned()
            .collect()
    }

    fn physical_any(&self, models: &[AnyModel]) -> Vec<AnyModel> {
        models
            .iter()
            .filter(|model| {
                !is_virtual_any_model(model)
                    && !(is_model_type(model, pi_ai::ModelType::Chat) && self.virtual_ids.contains(model.id()))
            })
            .cloned()
            .collect()
    }
}

impl Provider for VirtualWrappedProvider {
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
        let mut out = self.physical_chat(&self.inner.get_models());
        out.extend(self.virtual_models.iter().cloned());
        out
    }

    fn get_all_models(&self) -> Option<Vec<AnyModel>> {
        let base_all = self
            .inner
            .get_all_models()
            .unwrap_or_else(|| self.inner.get_models().into_iter().map(AnyModel::Chat).collect());
        let mut out = self.physical_any(&base_all);
        out.extend(self.virtual_models.iter().cloned().map(AnyModel::Chat));
        Some(out)
    }

    fn refresh_models(&self, context: RefreshModelsContext) -> Option<BoxFuture<Result<()>>> {
        self.inner.refresh_models(context)
    }

    fn filter_models(&self, models: &[Model], credential: Option<&Credential>) -> Option<Vec<Model>> {
        let real = self.physical_chat(models);
        let mut out = self.inner.filter_models(&real, credential).unwrap_or(real);
        out.extend(models.iter().filter(|m| is_virtual_model(m)).cloned());
        Some(out)
    }

    fn filter_all_models(&self, models: &[AnyModel], credential: Option<&Credential>) -> Option<Vec<AnyModel>> {
        let real = self.physical_any(models);
        let mut out = self.inner.filter_all_models(&real, credential)?;
        out.extend(models.iter().filter(|m| is_virtual_any_model(m)).cloned());
        Some(out)
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        if is_virtual_model(model) {
            unrouted_stream(model)
        } else {
            self.inner.stream(model, context, options)
        }
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        if is_virtual_model(model) {
            unrouted_stream(model)
        } else {
            self.inner.stream_simple(model, context, options)
        }
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

/// Add virtual models to a provider's catalog.
pub fn with_virtual_models(
    provider_id: &str,
    provider: Option<Arc<dyn Provider + Send + Sync>>,
    virtual_models: Vec<Model>,
) -> Arc<dyn Provider + Send + Sync> {
    let Some(base) = provider else {
        return Arc::new(VirtualOnlyProvider {
            provider_id: provider_id.to_owned(),
            virtual_models,
        });
    };
    let virtual_ids = virtual_models.iter().map(|m| m.id.clone()).collect();
    Arc::new(VirtualWrappedProvider {
        inner: base,
        virtual_models,
        virtual_ids,
    })
}
