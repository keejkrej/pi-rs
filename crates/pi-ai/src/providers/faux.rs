//! Port of packages/ai/src/providers/faux.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex, MutexGuard};

use indexmap::IndexMap;

use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result};

use crate::models::Provider;
use crate::types::{
    AssistantMessage, DeferredCancelOptions, DeferredFetchOptions, DeferredHandle, ImageContent, JsonObject, Message,
    Model, ModelCostRates, ModelInput, ModelInputLimits, SimpleStreamOptions, StopReason, StreamOptions, TextContent,
    ThinkingContent, ToolCall, ToolResultMessage, TranscriptContext, Usage,
};
use crate::utils::event_stream::AssistantMessageEventStream;

const DEFAULT_API: &str = "faux";
const DEFAULT_PROVIDER: &str = "faux";
const DEFAULT_MODEL_ID: &str = "faux-1";
const DEFAULT_MODEL_NAME: &str = "Faux Model";
const DEFAULT_BASE_URL: &str = "http://localhost:0";
const DEFAULT_MIN_TOKEN_SIZE: i64 = 3;
const DEFAULT_MAX_TOKEN_SIZE: i64 = 5;
// Default model literal in `createFauxCore` when `options.models` is empty.
const DEFAULT_CONTEXT_WINDOW: i64 = 128_000;
const DEFAULT_MAX_TOKENS: i64 = 16_384;

// DEFAULT_USAGE: input/output/cacheRead/cacheWrite/totalTokens are 0.
// cost input/output/cacheRead/cacheWrite/total are 0.
fn default_usage() -> Usage {
    todo!("port: default_usage")
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

/// `TextContent | ThinkingContent | ToolCall`.
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

#[derive(Clone, Debug)]
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
        self.inner.lock().unwrap()
    }

    /// `state.callCount`.
    pub fn call_count(&self) -> i64 {
        self.inner.lock().unwrap().call_count
    }

    /// `state.deferredFetchCount`.
    pub fn deferred_fetch_count(&self) -> i64 {
        self.inner.lock().unwrap().deferred_fetch_count
    }

    /// `state.cancelledDeferred`.
    pub fn cancelled_deferred(&self) -> Vec<DeferredHandle> {
        self.inner.lock().unwrap().cancelled_deferred.clone()
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
        todo!("port: FauxCore::stream")
    }

    pub fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        todo!("port: FauxCore::stream_simple")
    }

    pub fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredFetchOptions>,
    ) -> AssistantMessageEventStream {
        todo!("port: FauxCore::fetch_deferred")
    }

    pub async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredCancelOptions>,
    ) -> Result<()> {
        todo!("port: FauxCore::cancel_deferred")
    }

    /// `getModel()` with no id. Returns the first model.
    pub fn get_model(&self) -> Model {
        todo!("port: FauxCore::get_model")
    }

    /// `getModel(modelId)`.
    pub fn get_model_model_id(&self, model_id: &str) -> Option<Model> {
        todo!("port: FauxCore::get_model_model_id")
    }

    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        todo!("port: FauxCore::set_responses")
    }

    pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
        todo!("port: FauxCore::append_responses")
    }

    pub fn get_pending_response_count(&self) -> i64 {
        self.inner.pending_responses.lock().unwrap().len() as i64
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
    todo!("port: faux_text")
}

pub fn faux_thinking(thinking: &str) -> ThinkingContent {
    todo!("port: faux_thinking")
}

pub fn faux_tool_call(name: &str, arguments: JsonObject, options: Option<FauxToolCallOptions>) -> ToolCall {
    todo!("port: faux_tool_call")
}

pub fn faux_assistant_message(
    content: FauxAssistantMessageContent,
    options: Option<FauxAssistantMessageOptions>,
) -> AssistantMessage {
    todo!("port: faux_assistant_message")
}

pub fn create_faux_core(options: RegisterFauxProviderOptions) -> FauxCore {
    todo!("port: create_faux_core")
}

/// Faux provider for tests built on explicit `Models` collections.
///
/// `options` `None` is `{}`. Does not call a real provider.
///
/// ```ts
/// const faux = fauxProvider();
/// const models = createModels();
/// models.setProvider(faux.provider);
/// faux.setResponses([fauxAssistantMessage("hi")]);
/// ```
pub fn faux_provider(options: Option<RegisterFauxProviderOptions>) -> FauxProviderHandle {
    todo!("port: faux_provider")
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
    todo!("port: normalize_faux_assistant_content")
}

fn estimate_tokens(text: &str) -> i64 {
    todo!("port: estimate_tokens")
}

fn random_id(prefix: &str) -> String {
    todo!("port: random_id")
}

fn content_to_text(content: &FauxPlainContent) -> String {
    todo!("port: content_to_text")
}

fn assistant_content_to_text(content: &[FauxContentBlock]) -> String {
    todo!("port: assistant_content_to_text")
}

fn tool_result_to_text(message: &ToolResultMessage) -> String {
    todo!("port: tool_result_to_text")
}

fn message_to_text(message: &Message) -> String {
    todo!("port: message_to_text")
}

fn serialize_context(context: &TranscriptContext) -> String {
    todo!("port: serialize_context")
}

fn common_prefix_length(a: &str, b: &str) -> i64 {
    todo!("port: common_prefix_length")
}

fn with_usage_estimate(
    message: AssistantMessage,
    context: &TranscriptContext,
    options: Option<&StreamOptions>,
    prompt_cache: &mut IndexMap<String, String>,
) -> AssistantMessage {
    todo!("port: with_usage_estimate")
}

fn split_string_by_token_size(text: &str, min_token_size: i64, max_token_size: i64) -> Vec<String> {
    todo!("port: split_string_by_token_size")
}

fn clone_message(message: &AssistantMessage, api: &str, provider: &str, model_id: &str) -> AssistantMessage {
    todo!("port: clone_message")
}

fn create_deferred_message(model: &Model, handle: &DeferredHandle) -> AssistantMessage {
    todo!("port: create_deferred_message")
}

fn create_error_message(error: &pi_js::Error, api: &str, provider: &str, model_id: &str) -> AssistantMessage {
    todo!("port: create_error_message")
}

fn create_aborted_message(partial: AssistantMessage) -> AssistantMessage {
    todo!("port: create_aborted_message")
}

async fn schedule_chunk(chunk: &str, tokens_per_second: Option<f64>) {
    todo!("port: schedule_chunk")
}

async fn stream_with_deltas(
    stream: &AssistantMessageEventStream,
    message: &AssistantMessage,
    min_token_size: i64,
    max_token_size: i64,
    tokens_per_second: Option<f64>,
    signal: Option<&AbortSignal>,
) {
    todo!("port: stream_with_deltas")
}
