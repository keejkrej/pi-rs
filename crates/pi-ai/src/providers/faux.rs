//! Port of packages/ai/src/providers/faux.ts

use std::sync::{Arc, Mutex, MutexGuard};

use indexmap::IndexMap;

use pi_js::abort::{AbortReason, AbortSignal};
use pi_js::{BoxFuture, Error, Result};

use crate::auth::types::{ApiKeyAuth, ApiKeyAuthInput, AuthResult, ModelAuth, ProviderAuth};
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider, create_provider};
use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, DeferredCancelOptions, DeferredFetchOptions,
    DeferredHandle, DeferredOption, ImageContent, JsonObject, Message, Model, ModelCost, ModelCostRates, ModelInput,
    ModelInputLimits, ProviderResponse, ProviderStreams, SimpleStreamOptions, StopReason, StreamOptions, TextContent,
    ThinkingContent, ToolCall, ToolResultMessage, TranscriptContext, Usage, UserContentBlock, UserMessageContent,
};
use crate::utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use crate::utils::text::get_system_message_text;

const DEFAULT_API: &str = "faux";
const DEFAULT_PROVIDER: &str = "faux";
const DEFAULT_MODEL_ID: &str = "faux-1";
const DEFAULT_MODEL_NAME: &str = "Faux Model";
const DEFAULT_BASE_URL: &str = "http://localhost:0";
const DEFAULT_MIN_TOKEN_SIZE: i64 = 3;
const DEFAULT_MAX_TOKEN_SIZE: i64 = 5;
const DEFAULT_CONTEXT_WINDOW: i64 = 128_000;
const DEFAULT_MAX_TOKENS: i64 = 16_384;

fn default_usage() -> Usage {
    Usage::default()
}

#[derive(Clone, Debug)]
pub struct FauxModelDefinition {
    pub id: String,
    pub name: Option<String>,
    pub reasoning: Option<bool>,
    pub input: Option<Vec<ModelInput>>,
    pub input_limits: Option<ModelInputLimits>,
    pub cost: Option<ModelCostRates>,
    pub context_window: Option<i64>,
    pub max_tokens: Option<i64>,
}

#[derive(Clone, Debug)]
pub enum FauxContentBlock {
    Text(TextContent),
    Thinking(ThinkingContent),
    ToolCall(ToolCall),
}

/// `string | FauxContentBlock | FauxContentBlock[]`.
#[derive(Clone, Debug)]
pub enum FauxAssistantMessageContent {
    Text(String),
    Block(FauxContentBlock),
    Blocks(Vec<FauxContentBlock>),
}

/// `{ id?: string }` on [`faux_tool_call`]. `None` is `{}`.
#[derive(Clone, Debug, Default)]
pub struct FauxToolCallOptions {
    pub id: Option<String>,
}

/// Options object on [`faux_assistant_message`]. `None` is `{}`.
#[derive(Clone, Debug, Default)]
pub struct FauxAssistantMessageOptions {
    pub stop_reason: Option<StopReason>,
    pub deferred: Option<DeferredHandle>,
    pub error_message: Option<String>,
    pub response_id: Option<String>,
    pub timestamp: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct FauxProviderStateInner {
    pub call_count: i64,
    pub deferred_fetch_count: i64,
    pub cancelled_deferred: Vec<DeferredHandle>,
}

/// Shared counters mutated by the faux stream and read by tests.
#[derive(Clone, Debug)]
pub struct FauxProviderState {
    inner: Arc<Mutex<FauxProviderStateInner>>,
}

impl FauxProviderState {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FauxProviderStateInner {
                call_count: 0,
                deferred_fetch_count: 0,
                cancelled_deferred: Vec::new(),
            })),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, FauxProviderStateInner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// `state.callCount`.
    pub fn call_count(&self) -> i64 {
        self.lock().call_count
    }

    /// `state.deferredFetchCount`.
    pub fn deferred_fetch_count(&self) -> i64 {
        self.lock().deferred_fetch_count
    }

    /// `state.cancelledDeferred`.
    pub fn cancelled_deferred(&self) -> Vec<DeferredHandle> {
        self.lock().cancelled_deferred.clone()
    }
}

pub type FauxResponseFactory = Arc<
    dyn Fn(
            TranscriptContext,
            Option<SimpleStreamOptions>,
            FauxProviderState,
            Model,
        ) -> BoxFuture<Result<AssistantMessage>>
        + Send
        + Sync,
>;

/// `AssistantMessage | FauxResponseFactory`.
#[derive(Clone)]
pub enum FauxResponseStep {
    Message(AssistantMessage),
    Factory(FauxResponseFactory),
}

#[derive(Clone, Debug, Default)]
pub struct FauxDeferredOptions {
    /// Number of fetches that return the original handle before the scripted response becomes ready.
    pub pending_fetches: Option<i64>,
    pub poll_after_ms: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct FauxTokenSize {
    pub min: Option<i64>,
    pub max: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct RegisterFauxProviderOptions {
    pub api: Option<String>,
    pub provider: Option<String>,
    pub models: Option<Vec<FauxModelDefinition>>,
    pub deferred: Option<FauxDeferredOptions>,
    pub tokens_per_second: Option<f64>,
    pub token_size: Option<FauxTokenSize>,
}

#[derive(Clone)]
struct FauxDeferredEntry {
    handle: DeferredHandle,
    step: FauxResponseStep,
    context: TranscriptContext,
    options: Option<SimpleStreamOptions>,
    model: Model,
    pending_fetches: i64,
    cancelled: bool,
    final_message: Option<AssistantMessage>,
}

struct FauxCoreInner {
    api: String,
    provider: String,
    models: Vec<Model>,
    state: FauxProviderState,
    pending_responses: Mutex<Vec<FauxResponseStep>>,
    min_token_size: i64,
    max_token_size: i64,
    tokens_per_second: Option<f64>,
    prompt_cache: Mutex<IndexMap<String, String>>,
    deferred_responses: Mutex<IndexMap<String, FauxDeferredEntry>>,
    pending_fetches: i64,
    poll_after_ms: Option<i64>,
}

/// Return value of [`create_faux_core`].
///
/// PORT: TS infers this object. The non-empty `models` tuple is a [`Vec`] that `create_faux_core` always fills.
#[derive(Clone)]
pub struct FauxCore {
    inner: Arc<FauxCoreInner>,
}

impl FauxCore {
    /// Api id (`core.api`).
    pub fn api(&self) -> &str {
        &self.inner.api
    }

    /// Provider id (`core.provider`).
    pub fn provider(&self) -> &str {
        &self.inner.provider
    }

    pub fn models(&self) -> &[Model] {
        &self.inner.models
    }

    pub fn state(&self) -> FauxProviderState {
        self.inner.state.clone()
    }

    pub fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        let outer = create_assistant_message_event_stream();
        let step = {
            let mut guard = self
                .inner
                .pending_responses
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.is_empty() { None } else { Some(guard.remove(0)) }
        };
        {
            let mut state_guard = self.inner.state.lock();
            state_guard.call_count += 1;
        }

        let inner = Arc::clone(&self.inner);
        let outer_clone = outer.clone();
        let request_model = model.clone();
        let context_owned = context.clone();

        tokio::spawn(async move {
            let result: Result<()> = async {
                if let Some(on_response) = options.as_ref().and_then(|o| o.base.on_response.as_ref()) {
                    on_response(
                        ProviderResponse {
                            status: 200,
                            headers: IndexMap::new(),
                        },
                        &request_model,
                    )
                    .await?;
                }

                let Some(step) = step else {
                    let msg = create_error_message(
                        &Error::msg("No more faux responses queued"),
                        &inner.api,
                        &inner.provider,
                        &request_model.id,
                    );
                    let mut cache = inner
                        .prompt_cache
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let msg = with_usage_estimate(msg, &context_owned, options.as_ref().map(|o| &o.base), &mut cache);
                    outer_clone.push(AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        error: msg.clone(),
                    });
                    outer_clone.end(Some(msg));
                    return Ok(());
                };

                let is_deferred = matches!(
                    options.as_ref().and_then(|o| o.deferred.as_ref()),
                    Some(DeferredOption::Bool(true) | DeferredOption::Window { .. })
                );
                if is_deferred {
                    let handle = DeferredHandle {
                        provider: request_model.provider.clone(),
                        model_id: request_model.id.clone(),
                        api: request_model.api.clone(),
                        id: random_id("deferred"),
                        expires_at: None,
                        poll_after_ms: inner.poll_after_ms,
                        data: None,
                    };
                    let entry = FauxDeferredEntry {
                        handle: handle.clone(),
                        step,
                        context: context_owned,
                        options: options.clone(),
                        model: request_model.clone(),
                        pending_fetches: inner.pending_fetches,
                        cancelled: false,
                        final_message: None,
                    };
                    {
                        let mut deferred_map = inner
                            .deferred_responses
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        deferred_map.insert(handle.id.clone(), entry);
                    }
                    let deferred_msg = create_deferred_message(&request_model, &handle);
                    stream_with_deltas(
                        &outer_clone,
                        &deferred_msg,
                        inner.min_token_size,
                        inner.max_token_size,
                        inner.tokens_per_second,
                        options.as_ref().and_then(|o| o.base.signal.as_ref()),
                    )
                    .await?;
                    return Ok(());
                }

                let resolved =
                    resolve_step_message(&inner, step, &context_owned, options.as_ref(), &request_model).await?;
                stream_with_deltas(
                    &outer_clone,
                    &resolved,
                    inner.min_token_size,
                    inner.max_token_size,
                    inner.tokens_per_second,
                    options.as_ref().and_then(|o| o.base.signal.as_ref()),
                )
                .await?;
                Ok(())
            }
            .await;

            if let Err(err) = result {
                let msg = create_error_message(&err, &inner.api, &inner.provider, &request_model.id);
                outer_clone.push(AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: msg.clone(),
                });
                outer_clone.end(Some(msg));
            }
        });

        outer
    }

    pub fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.stream(model, context, options)
    }

    pub fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredFetchOptions>,
    ) -> AssistantMessageEventStream {
        let outer = create_assistant_message_event_stream();
        {
            let mut state_guard = self.inner.state.lock();
            state_guard.deferred_fetch_count += 1;
        }

        let inner = Arc::clone(&self.inner);
        let outer_clone = outer.clone();
        let request_model = model.clone();
        let handle_owned = handle.clone();

        tokio::spawn(async move {
            let result: Result<()> = async {
                if let Some(on_response) = options.as_ref().and_then(|o| o.base.on_response.as_ref()) {
                    on_response(
                        ProviderResponse {
                            status: 200,
                            headers: IndexMap::new(),
                        },
                        &request_model,
                    )
                    .await?;
                }

                enum FetchAction {
                    StillPending(DeferredHandle),
                    ResolveAndCache {
                        step: FauxResponseStep,
                        context: TranscriptContext,
                        submission_options: Option<SimpleStreamOptions>,
                        entry_model: Model,
                    },
                    Ready(AssistantMessage),
                }

                let action = {
                    let mut deferred_map = inner
                        .deferred_responses
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let Some(entry) = deferred_map.get_mut(&handle_owned.id) else {
                        return Err(Error::msg(format!(
                            "Unknown faux deferred response: {}",
                            handle_owned.id
                        )));
                    };
                    if entry.handle.provider != handle_owned.provider
                        || entry.handle.model_id != handle_owned.model_id
                        || entry.handle.api != handle_owned.api
                    {
                        return Err(Error::msg(format!(
                            "Unknown faux deferred response: {}",
                            handle_owned.id
                        )));
                    }
                    if entry.cancelled {
                        return Err(Error::msg(format!(
                            "Faux deferred response was cancelled: {}",
                            handle_owned.id
                        )));
                    }

                    if entry.pending_fetches > 0 {
                        entry.pending_fetches -= 1;
                        FetchAction::StillPending(entry.handle.clone())
                    } else if let Some(ref final_msg) = entry.final_message {
                        FetchAction::Ready(final_msg.clone())
                    } else {
                        let submission_options = entry.options.clone().map(|mut opts| {
                            opts.deferred = None;
                            opts.base.signal = None;
                            opts.base.on_response = None;
                            opts
                        });
                        FetchAction::ResolveAndCache {
                            step: entry.step.clone(),
                            context: entry.context.clone(),
                            submission_options,
                            entry_model: entry.model.clone(),
                        }
                    }
                };

                match action {
                    FetchAction::StillPending(entry_handle) => {
                        let msg = create_deferred_message(&request_model, &entry_handle);
                        stream_with_deltas(
                            &outer_clone,
                            &msg,
                            inner.min_token_size,
                            inner.max_token_size,
                            inner.tokens_per_second,
                            options.as_ref().and_then(|o| o.base.signal.as_ref()),
                        )
                        .await?;
                    }
                    FetchAction::ResolveAndCache {
                        step,
                        context,
                        submission_options,
                        entry_model,
                    } => {
                        let resolved = match resolve_step_message(
                            &inner,
                            step,
                            &context,
                            submission_options.as_ref(),
                            &entry_model,
                        )
                        .await
                        {
                            Ok(msg) => msg,
                            Err(err) => create_error_message(&err, &inner.api, &inner.provider, &entry_model.id),
                        };
                        {
                            let mut deferred_map = inner
                                .deferred_responses
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            if let Some(entry) = deferred_map.get_mut(&handle_owned.id) {
                                entry.final_message = Some(resolved.clone());
                            }
                        }
                        stream_with_deltas(
                            &outer_clone,
                            &resolved,
                            inner.min_token_size,
                            inner.max_token_size,
                            inner.tokens_per_second,
                            options.as_ref().and_then(|o| o.base.signal.as_ref()),
                        )
                        .await?;
                    }
                    FetchAction::Ready(final_msg) => {
                        stream_with_deltas(
                            &outer_clone,
                            &final_msg,
                            inner.min_token_size,
                            inner.max_token_size,
                            inner.tokens_per_second,
                            options.as_ref().and_then(|o| o.base.signal.as_ref()),
                        )
                        .await?;
                    }
                }
                Ok(())
            }
            .await;

            if let Err(err) = result {
                let msg = create_error_message(&err, &inner.api, &inner.provider, &request_model.id);
                outer_clone.push(AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: msg.clone(),
                });
                outer_clone.end(Some(msg));
            }
        });

        outer
    }

    pub async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredCancelOptions>,
    ) -> Result<()> {
        {
            let mut state_guard = self.inner.state.lock();
            state_guard.cancelled_deferred.push(handle.clone());
        }
        {
            let mut deferred_map = self
                .inner
                .deferred_responses
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(entry) = deferred_map.get_mut(&handle.id) {
                entry.cancelled = true;
            }
        }
        if let Some(on_response) = options.as_ref().and_then(|o| o.on_response.as_ref()) {
            on_response(
                ProviderResponse {
                    status: 200,
                    headers: IndexMap::new(),
                },
                model,
            )
            .await?;
        }
        Ok(())
    }

    /// `getModel()` with no id. Returns the first model.
    pub fn get_model(&self) -> Model {
        self.inner.models[0].clone()
    }

    /// `getModel(modelId)`.
    pub fn get_model_model_id(&self, model_id: &str) -> Option<Model> {
        self.inner.models.iter().find(|m| m.id == model_id).cloned()
    }

    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        let mut guard = self
            .inner
            .pending_responses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = responses;
    }

    pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
        let mut guard = self
            .inner
            .pending_responses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.extend(responses);
    }

    pub fn get_pending_response_count(&self) -> i64 {
        self.inner
            .pending_responses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len() as i64
    }
}

/// Object returned by compat `registerFauxProvider`.
///
/// PORT: compat builds this from [`FauxCore`] plus an `unregister` closure.
#[derive(Clone)]
pub struct FauxProviderRegistration {
    inner: Arc<FauxProviderRegistrationInner>,
}

struct FauxProviderRegistrationInner {
    core: FauxCore,
    unregister_fn: Arc<dyn Fn() + Send + Sync>,
}

impl FauxProviderRegistration {
    pub fn new(core: FauxCore, unregister: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            inner: Arc::new(FauxProviderRegistrationInner {
                core,
                unregister_fn: unregister,
            }),
        }
    }

    pub fn api(&self) -> &str {
        self.inner.core.api()
    }

    pub fn models(&self) -> &[Model] {
        self.inner.core.models()
    }

    pub fn get_model(&self) -> Model {
        self.inner.core.get_model()
    }

    pub fn get_model_model_id(&self, model_id: &str) -> Option<Model> {
        self.inner.core.get_model_model_id(model_id)
    }

    pub fn state(&self) -> FauxProviderState {
        self.inner.core.state()
    }

    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        self.inner.core.set_responses(responses);
    }

    pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
        self.inner.core.append_responses(responses);
    }

    pub fn get_pending_response_count(&self) -> i64 {
        self.inner.core.get_pending_response_count()
    }

    pub fn unregister(&self) {
        (self.inner.unregister_fn)();
    }
}

/// Object returned by [`faux_provider`].
#[derive(Clone)]
pub struct FauxProviderHandle {
    inner: Arc<FauxProviderHandleInner>,
}

struct FauxProviderHandleInner {
    provider: Arc<dyn Provider + Send + Sync>,
    core: FauxCore,
}

impl FauxProviderHandle {
    pub(crate) fn new(provider: Arc<dyn Provider + Send + Sync>, core: FauxCore) -> Self {
        Self {
            inner: Arc::new(FauxProviderHandleInner { provider, core }),
        }
    }

    pub fn provider(&self) -> Arc<dyn Provider + Send + Sync> {
        Arc::clone(&self.inner.provider)
    }

    pub fn api(&self) -> &str {
        self.inner.core.api()
    }

    pub fn models(&self) -> &[Model] {
        self.inner.core.models()
    }

    pub fn get_model(&self) -> Model {
        self.inner.core.get_model()
    }

    pub fn get_model_model_id(&self, model_id: &str) -> Option<Model> {
        self.inner.core.get_model_model_id(model_id)
    }

    pub fn state(&self) -> FauxProviderState {
        self.inner.core.state()
    }

    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        self.inner.core.set_responses(responses);
    }

    pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
        self.inner.core.append_responses(responses);
    }

    pub fn get_pending_response_count(&self) -> i64 {
        self.inner.core.get_pending_response_count()
    }
}

pub fn faux_text(text: &str) -> TextContent {
    TextContent::new(text)
}

pub fn faux_thinking(thinking: &str) -> ThinkingContent {
    ThinkingContent::new(thinking)
}

pub fn faux_tool_call(name: &str, arguments: JsonObject, options: Option<FauxToolCallOptions>) -> ToolCall {
    let id = options.and_then(|o| o.id).unwrap_or_else(|| random_id("tool"));
    ToolCall::new(id, name, arguments)
}

pub fn faux_assistant_message(
    content: FauxAssistantMessageContent,
    options: Option<FauxAssistantMessageOptions>,
) -> AssistantMessage {
    let opts = options.unwrap_or_default();
    let content_blocks: Vec<AssistantContentBlock> = normalize_faux_assistant_content(content)
        .into_iter()
        .map(|block| match block {
            FauxContentBlock::Text(t) => AssistantContentBlock::Text(t),
            FauxContentBlock::Thinking(th) => AssistantContentBlock::Thinking(th),
            FauxContentBlock::ToolCall(tc) => AssistantContentBlock::ToolCall(tc),
        })
        .collect();

    AssistantMessage {
        content: content_blocks,
        api: DEFAULT_API.to_owned(),
        provider: DEFAULT_PROVIDER.to_owned(),
        model: DEFAULT_MODEL_ID.to_owned(),
        response_model: None,
        response_id: opts.response_id,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: default_usage(),
        stop_reason: opts.stop_reason.unwrap_or(StopReason::Stop),
        deferred: opts.deferred,
        error_message: opts.error_message,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: opts.timestamp.unwrap_or_else(pi_js::time::now_ms),
    }
}

pub fn create_faux_core(options: RegisterFauxProviderOptions) -> FauxCore {
    let api = options.api.unwrap_or_else(|| random_id(DEFAULT_API));
    let provider = options.provider.unwrap_or_else(|| DEFAULT_PROVIDER.to_owned());
    let token_min = options
        .token_size
        .as_ref()
        .and_then(|t| t.min)
        .unwrap_or(DEFAULT_MIN_TOKEN_SIZE);
    let token_max = options
        .token_size
        .as_ref()
        .and_then(|t| t.max)
        .unwrap_or(DEFAULT_MAX_TOKEN_SIZE);
    let min_token_size = token_min.min(token_max).max(1);
    let max_token_size = token_max.max(min_token_size);

    let definitions = match options.models {
        Some(defs) if !defs.is_empty() => defs,
        _ => vec![FauxModelDefinition {
            id: DEFAULT_MODEL_ID.to_owned(),
            name: Some(DEFAULT_MODEL_NAME.to_owned()),
            reasoning: Some(false),
            input: Some(vec![ModelInput::Text, ModelInput::Image]),
            input_limits: None,
            cost: Some(ModelCostRates::default()),
            context_window: Some(DEFAULT_CONTEXT_WINDOW),
            max_tokens: Some(DEFAULT_MAX_TOKENS),
        }],
    };

    let models = definitions
        .into_iter()
        .map(|def| {
            let rates = def.cost.unwrap_or_default();
            Model {
                r#type: None,
                id: def.id.clone(),
                name: def.name.unwrap_or(def.id),
                api: api.clone(),
                provider: provider.clone(),
                base_url: DEFAULT_BASE_URL.to_owned(),
                reasoning: def.reasoning.unwrap_or(false),
                thinking_level_map: None,
                prompt_cache: None,
                input: def.input.unwrap_or_else(|| vec![ModelInput::Text, ModelInput::Image]),
                input_limits: def.input_limits,
                cost: ModelCost {
                    input: rates.input,
                    output: rates.output,
                    cache_read: rates.cache_read,
                    cache_write: rates.cache_write,
                    tiers: None,
                },
                context_window: def.context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW),
                max_tokens: def.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
                headers: None,
                sampling_params: None,
                compat: None,
            }
        })
        .collect();

    let pending_fetches = options
        .deferred
        .as_ref()
        .and_then(|d| d.pending_fetches)
        .unwrap_or(0)
        .max(0);
    let poll_after_ms = options.deferred.as_ref().and_then(|d| d.poll_after_ms);

    FauxCore {
        inner: Arc::new(FauxCoreInner {
            api,
            provider,
            models,
            state: FauxProviderState::new(),
            pending_responses: Mutex::new(Vec::new()),
            min_token_size,
            max_token_size,
            tokens_per_second: options.tokens_per_second,
            prompt_cache: Mutex::new(IndexMap::new()),
            deferred_responses: Mutex::new(IndexMap::new()),
            pending_fetches,
            poll_after_ms,
        }),
    }
}

struct FauxApiKeyAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for FauxApiKeyAuth {
    fn name(&self) -> &str {
        "Faux"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        Ok(Some(AuthResult {
            auth: ModelAuth::default(),
            env: None,
            source: None,
        }))
    }
}

struct FauxDirectProvider {
    core: FauxCore,
    auth: ProviderAuth,
}

impl Provider for FauxDirectProvider {
    fn id(&self) -> &str {
        self.core.provider()
    }

    fn name(&self) -> &str {
        self.core.provider()
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        self.core.models().to_vec()
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<crate::types::ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        let simple_opts = options.map(|o| SimpleStreamOptions {
            base: o.base,
            tool_choice: None,
            reasoning: None,
            deferred: None,
            thinking_budgets: None,
        });
        self.core.stream(model, context, simple_opts)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.core.stream_simple(model, context, options)
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        Some(self.core.fetch_deferred(model, handle, options))
    }

    fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredCancelOptions>,
    ) -> Option<BoxFuture<Result<()>>> {
        let core = self.core.clone();
        let model_owned = model.clone();
        let handle_owned = handle.clone();
        Some(Box::pin(async move {
            core.cancel_deferred(&model_owned, &handle_owned, options).await
        }))
    }
}

/// Faux provider for tests built on explicit `Models` collections.
///
/// `options` `None` is `{}`. Does not call a real provider.
pub fn faux_provider(options: Option<RegisterFauxProviderOptions>) -> FauxProviderHandle {
    let core = create_faux_core(options.unwrap_or_default());
    let auth = ProviderAuth {
        api_key: Some(Arc::new(FauxApiKeyAuth)),
        oauth: None,
    };

    let core_for_stream = core.clone();
    let stream_fn: crate::types::StreamFunction = Arc::new(move |model, context, opts| {
        let simple_opts = opts.map(|o| SimpleStreamOptions {
            base: o.base,
            tool_choice: None,
            reasoning: None,
            deferred: None,
            thinking_budgets: None,
        });
        core_for_stream.stream(model, context, simple_opts)
    });

    let core_for_simple = core.clone();
    let stream_simple_fn: crate::types::SimpleStreamFunction =
        Arc::new(move |model, context, opts| core_for_simple.stream_simple(model, context, opts));

    let core_for_fetch = core.clone();
    let fetch_deferred_fn: crate::types::FetchDeferredFunction =
        Arc::new(move |model, handle, opts| core_for_fetch.fetch_deferred(model, handle, opts));

    let core_for_cancel = core.clone();
    let cancel_deferred_fn: crate::types::CancelDeferredFunction = Arc::new(move |model, handle, opts| {
        let core_cloned = core_for_cancel.clone();
        let model_owned = model.clone();
        let handle_owned = handle.clone();
        Box::pin(async move { core_cloned.cancel_deferred(&model_owned, &handle_owned, opts).await })
    });

    let provider = match create_provider(CreateProviderOptions {
        id: core.provider().to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: auth.clone(),
        models: core
            .models()
            .iter()
            .cloned()
            .map(crate::types::AnyModel::Chat)
            .collect(),
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(ProviderStreams {
            stream: stream_fn,
            stream_simple: stream_simple_fn,
            fetch_deferred: Some(fetch_deferred_fn),
            cancel_deferred: Some(cancel_deferred_fn),
        })),
        images: None,
        classifiers: None,
    }) {
        Ok(p) => p,
        Err(_) => Arc::new(FauxDirectProvider {
            core: core.clone(),
            auth,
        }),
    };

    FauxProviderHandle::new(provider, core)
}

#[derive(Clone, Debug)]
enum FauxPlainBlock {
    Text(TextContent),
    Image(ImageContent),
}

enum FauxPlainContent {
    Text(String),
    Blocks(Vec<FauxPlainBlock>),
}

fn normalize_faux_assistant_content(content: FauxAssistantMessageContent) -> Vec<FauxContentBlock> {
    match content {
        FauxAssistantMessageContent::Text(s) => vec![FauxContentBlock::Text(faux_text(&s))],
        FauxAssistantMessageContent::Block(b) => vec![b],
        FauxAssistantMessageContent::Blocks(bs) => bs,
    }
}

fn estimate_tokens(text: &str) -> i64 {
    let utf16_len = pi_js::str16::len(text) as i64;
    (utf16_len + 3) / 4
}

fn random_id(prefix: &str) -> String {
    let ts = pi_js::time::now_ms();
    let rnd: u64 = rand::random();
    format!("{prefix}:{ts}:{rnd:x}")
}

fn content_to_text(content: &FauxPlainContent) -> String {
    match content {
        FauxPlainContent::Text(s) => s.clone(),
        FauxPlainContent::Blocks(blocks) => blocks
            .iter()
            .map(|b| match b {
                FauxPlainBlock::Text(t) => t.text.clone(),
                FauxPlainBlock::Image(img) => {
                    format!("[image:{}:{}]", img.mime_type, pi_js::str16::len(&img.data))
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn assistant_content_to_text(content: &[FauxContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            FauxContentBlock::Text(t) => t.text.clone(),
            FauxContentBlock::Thinking(th) => th.thinking.clone(),
            FauxContentBlock::ToolCall(tc) => {
                format!("{}:{}", tc.name, pi_js::json::stringify(&tc.arguments))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assistant_blocks_to_faux_blocks(content: &[AssistantContentBlock]) -> Vec<FauxContentBlock> {
    content
        .iter()
        .map(|block| match block {
            AssistantContentBlock::Text(t) => FauxContentBlock::Text(t.clone()),
            AssistantContentBlock::Thinking(th) => FauxContentBlock::Thinking(th.clone()),
            AssistantContentBlock::ToolCall(tc) => FauxContentBlock::ToolCall(tc.clone()),
        })
        .collect()
}

fn tool_result_to_text(message: &ToolResultMessage) -> String {
    let mut parts = Vec::with_capacity(1 + message.content.len());
    parts.push(message.tool_name.clone());
    for block in &message.content {
        let plain = match block {
            UserContentBlock::Text(t) => FauxPlainBlock::Text(t.clone()),
            UserContentBlock::Image(i) => FauxPlainBlock::Image(i.clone()),
        };
        parts.push(content_to_text(&FauxPlainContent::Blocks(vec![plain])));
    }
    parts.join("\n")
}

fn message_to_text(message: &Message) -> String {
    match message {
        Message::System(sys) => {
            let mut parts: Vec<String> = Vec::new();
            parts.push(get_system_message_text(sys));
            if let Some(removed) = &sys.tools_removed {
                for tool in removed {
                    parts.push(format!("tool-:{}", pi_js::json::stringify(tool)));
                }
            }
            if let Some(added) = &sys.tools_added {
                for tool in added {
                    parts.push(format!("tool+:{}", pi_js::json::stringify(tool)));
                }
            }
            parts
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        }
        Message::User(user) => {
            let plain = match &user.content {
                UserMessageContent::Text(t) => FauxPlainContent::Text(t.clone()),
                UserMessageContent::Blocks(blocks) => FauxPlainContent::Blocks(
                    blocks
                        .iter()
                        .map(|b| match b {
                            UserContentBlock::Text(t) => FauxPlainBlock::Text(t.clone()),
                            UserContentBlock::Image(i) => FauxPlainBlock::Image(i.clone()),
                        })
                        .collect(),
                ),
            };
            content_to_text(&plain)
        }
        Message::Assistant(asst) => assistant_content_to_text(&assistant_blocks_to_faux_blocks(&asst.content)),
        Message::ToolResult(tr) => tool_result_to_text(tr),
    }
}

fn serialize_context(context: &TranscriptContext) -> String {
    context
        .messages
        .iter()
        .map(|m| format!("{}:{}", m.role(), message_to_text(m)))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn common_prefix_byte_len(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(ca, cb)| ca == cb)
        .map(|(ca, _)| ca.len_utf8())
        .sum()
}

fn common_prefix_length(a: &str, b: &str) -> i64 {
    let prefix_bytes = common_prefix_byte_len(a, b);
    pi_js::str16::len(&a[..prefix_bytes]) as i64
}

fn with_usage_estimate(
    mut message: AssistantMessage,
    context: &TranscriptContext,
    options: Option<&StreamOptions>,
    prompt_cache: &mut IndexMap<String, String>,
) -> AssistantMessage {
    let _ = common_prefix_length;
    let prompt_text = serialize_context(context);
    let prompt_tokens = estimate_tokens(&prompt_text);
    let output_tokens = estimate_tokens(&assistant_content_to_text(&assistant_blocks_to_faux_blocks(
        &message.content,
    )));
    let mut input = prompt_tokens;
    let mut cache_read = 0_i64;
    let mut cache_write = 0_i64;

    if let Some(session_id) = options.and_then(|o| o.session_id.as_ref()) {
        let no_cache = options
            .and_then(|o| o.cache_retention)
            .is_some_and(|r| matches!(r, crate::types::CacheRetention::None));
        if !no_cache {
            if let Some(previous_prompt) = prompt_cache.get(session_id) {
                let prefix_bytes = common_prefix_byte_len(previous_prompt, &prompt_text);
                cache_read = estimate_tokens(&previous_prompt[..prefix_bytes]);
                cache_write = (prompt_tokens - cache_read).max(0);
            } else {
                cache_write = prompt_tokens;
            }
            // Every prompt token is either read from or written to the simulated cache.
            input = 0;
            prompt_cache.insert(session_id.clone(), prompt_text);
        }
    }

    message.usage = Usage {
        input,
        output: output_tokens,
        cache_read,
        cache_write,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: input + output_tokens + cache_read + cache_write,
        cost: crate::types::UsageCost::default(),
    };
    message
}

fn split_string_by_token_size(text: &str, min_token_size: i64, max_token_size: i64) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let min_tok = min_token_size.max(1);
    let max_tok = max_token_size.max(min_tok);
    let span = (max_tok - min_tok + 1) as u64;
    let mut chunks = Vec::new();
    let mut chars = text.chars().peekable();

    while chars.peek().is_some() {
        let token_size = min_tok + (rand::random::<u64>() % span) as i64;
        let char_size = (token_size * 4).max(1) as usize;
        let mut chunk = String::new();
        for _ in 0..char_size {
            match chars.next() {
                Some(ch) => chunk.push(ch),
                None => break,
            }
        }
        chunks.push(chunk);
    }

    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

fn clone_message(message: &AssistantMessage, api: &str, provider: &str, model_id: &str) -> AssistantMessage {
    let mut cloned = message.clone();
    cloned.api = api.to_owned();
    cloned.provider = provider.to_owned();
    cloned.model = model_id.to_owned();
    cloned
}

fn create_deferred_message(model: &Model, handle: &DeferredHandle) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: default_usage(),
        stop_reason: StopReason::Deferred,
        deferred: Some(handle.clone()),
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
    }
}

fn create_error_message(error: &Error, api: &str, provider: &str, model_id: &str) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: api.to_owned(),
        provider: provider.to_owned(),
        model: model_id.to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: default_usage(),
        stop_reason: StopReason::Error,
        deferred: None,
        error_message: Some(error.to_string()),
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
    }
}

fn create_aborted_message(mut partial: AssistantMessage) -> AssistantMessage {
    partial.stop_reason = StopReason::Aborted;
    partial.error_message = Some("Request was aborted".to_owned());
    partial.timestamp = pi_js::time::now_ms();
    partial
}

async fn schedule_chunk(chunk: &str, tokens_per_second: Option<f64>, signal: Option<&AbortSignal>) {
    match tokens_per_second {
        Some(tps) if tps > 0.0 => {
            let delay_ms = ((estimate_tokens(chunk) as f64 / tps) * 1000.0).max(0.0) as u64;
            let _ = pi_js::time::sleep(delay_ms, signal).await;
        }
        _ => {
            tokio::task::yield_now().await;
        }
    }
}

async fn resolve_step_message(
    inner: &FauxCoreInner,
    step: FauxResponseStep,
    context: &TranscriptContext,
    options: Option<&SimpleStreamOptions>,
    request_model: &Model,
) -> Result<AssistantMessage> {
    let resolved = match step {
        FauxResponseStep::Message(msg) => msg,
        FauxResponseStep::Factory(factory) => {
            factory(
                context.clone(),
                options.cloned(),
                inner.state.clone(),
                request_model.clone(),
            )
            .await?
        }
    };
    let cloned = clone_message(&resolved, &inner.api, &inner.provider, &request_model.id);
    let mut cache = inner
        .prompt_cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Ok(with_usage_estimate(
        cloned,
        context,
        options.map(|o| &o.base),
        &mut cache,
    ))
}

async fn stream_with_deltas(
    stream: &AssistantMessageEventStream,
    message: &AssistantMessage,
    min_token_size: i64,
    max_token_size: i64,
    tokens_per_second: Option<f64>,
    signal: Option<&AbortSignal>,
) -> Result<()> {
    let mut partial = message.clone();
    partial.content = Vec::new();
    partial.stop_reason = StopReason::Pending;

    if signal.is_some_and(|s| s.aborted()) {
        let aborted = create_aborted_message(partial);
        stream.push(AssistantMessageEvent::Error {
            reason: StopReason::Aborted,
            error: aborted.clone(),
        });
        stream.end(Some(aborted));
        return Ok(());
    }

    stream.push(AssistantMessageEvent::Start {
        partial: partial.clone(),
    });

    for (index, block) in message.content.iter().enumerate() {
        if signal.is_some_and(|s| s.aborted()) {
            let aborted = create_aborted_message(partial);
            stream.push(AssistantMessageEvent::Error {
                reason: StopReason::Aborted,
                error: aborted.clone(),
            });
            stream.end(Some(aborted));
            return Ok(());
        }

        match block {
            AssistantContentBlock::Thinking(thinking_block) => {
                partial
                    .content
                    .push(AssistantContentBlock::Thinking(ThinkingContent::new("")));
                stream.push(AssistantMessageEvent::ThinkingStart {
                    content_index: index,
                    partial: partial.clone(),
                });
                for chunk in split_string_by_token_size(&thinking_block.thinking, min_token_size, max_token_size) {
                    schedule_chunk(&chunk, tokens_per_second, signal).await;
                    if signal.is_some_and(|s| s.aborted()) {
                        let aborted = create_aborted_message(partial);
                        stream.push(AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: aborted.clone(),
                        });
                        stream.end(Some(aborted));
                        return Ok(());
                    }
                    if let Some(AssistantContentBlock::Thinking(th)) = partial.content.get_mut(index) {
                        th.thinking.push_str(&chunk);
                    }
                    stream.push(AssistantMessageEvent::ThinkingDelta {
                        content_index: index,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                if let Some(AssistantContentBlock::Thinking(th)) = partial.content.get_mut(index) {
                    *th = thinking_block.clone();
                }
                stream.push(AssistantMessageEvent::ThinkingEnd {
                    content_index: index,
                    content: thinking_block.thinking.clone(),
                    partial: partial.clone(),
                });
            }
            AssistantContentBlock::Text(text_block) => {
                partial.content.push(AssistantContentBlock::Text(TextContent::new("")));
                stream.push(AssistantMessageEvent::TextStart {
                    content_index: index,
                    partial: partial.clone(),
                });
                for chunk in split_string_by_token_size(&text_block.text, min_token_size, max_token_size) {
                    schedule_chunk(&chunk, tokens_per_second, signal).await;
                    if signal.is_some_and(|s| s.aborted()) {
                        let aborted = create_aborted_message(partial);
                        stream.push(AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: aborted.clone(),
                        });
                        stream.end(Some(aborted));
                        return Ok(());
                    }
                    if let Some(AssistantContentBlock::Text(t)) = partial.content.get_mut(index) {
                        t.text.push_str(&chunk);
                    }
                    stream.push(AssistantMessageEvent::TextDelta {
                        content_index: index,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                if let Some(AssistantContentBlock::Text(t)) = partial.content.get_mut(index) {
                    *t = text_block.clone();
                }
                stream.push(AssistantMessageEvent::TextEnd {
                    content_index: index,
                    content: text_block.text.clone(),
                    partial: partial.clone(),
                });
            }
            AssistantContentBlock::ToolCall(tc_block) => {
                partial.content.push(AssistantContentBlock::ToolCall(ToolCall::new(
                    tc_block.id.clone(),
                    tc_block.name.clone(),
                    JsonObject::new(),
                )));
                stream.push(AssistantMessageEvent::ToolCallStart {
                    content_index: index,
                    partial: partial.clone(),
                });
                let serialized_args = pi_js::json::stringify(&tc_block.arguments);
                for chunk in split_string_by_token_size(&serialized_args, min_token_size, max_token_size) {
                    schedule_chunk(&chunk, tokens_per_second, signal).await;
                    if signal.is_some_and(|s| s.aborted()) {
                        let aborted = create_aborted_message(partial);
                        stream.push(AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: aborted.clone(),
                        });
                        stream.end(Some(aborted));
                        return Ok(());
                    }
                    stream.push(AssistantMessageEvent::ToolCallDelta {
                        content_index: index,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                if let Some(AssistantContentBlock::ToolCall(tc)) = partial.content.get_mut(index) {
                    *tc = tc_block.clone();
                }
                stream.push(AssistantMessageEvent::ToolCallEnd {
                    content_index: index,
                    tool_call: tc_block.clone(),
                    partial: partial.clone(),
                });
            }
        }
    }

    match message.stop_reason {
        StopReason::Pending => {
            let _ = AbortReason::abort();
            return Err(Error::msg("Faux response ended without a stop reason"));
        }
        StopReason::Error | StopReason::Aborted => {
            stream.push(AssistantMessageEvent::Error {
                reason: message.stop_reason,
                error: message.clone(),
            });
            stream.end(Some(message.clone()));
        }
        reason => {
            stream.push(AssistantMessageEvent::Done {
                reason,
                message: message.clone(),
            });
            stream.end(Some(message.clone()));
        }
    }
    Ok(())
}
