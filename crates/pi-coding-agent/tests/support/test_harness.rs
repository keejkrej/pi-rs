//! Port of packages/coding-agent/test/test-harness.ts

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_agent_core::{Agent, AgentEvent, AgentInitialState, AgentMessage, AgentOptions, AgentTool, StreamFn};
use pi_ai::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, Context, JsonObject,
    Model, ModelCost, ModelInput, SimpleStreamOptions, StopReason, TextContent, ThinkingContent, ToolCall,
    TranscriptContext, Usage, UsageCost, create_assistant_message_event_stream,
};
use pi_coding_agent::core::agent_session::{AgentSession, AgentSessionConfig, AgentSessionEvent};
use pi_coding_agent::core::auth_storage::{AuthStorage, make_api_key_credential};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::{Settings, SettingsManager};
use pi_js::{Error, Result};

use super::model_runtime_test_utils::{
    ModelRegistry, ModelRuntime, ProviderChatModelConfig, ProviderConfigInput, ProviderModelConfig,
    create_in_memory_model_registry, get_model_runtime,
};
use super::utilities::{
    CreateTestResourceLoaderOptions, TestExtensionInput, TestExtensionRunner, TestResourceLoader,
    create_test_extensions_result, create_test_resource_loader,
};

const FAUX_PROVIDER: &str = "faux";
const FAUX_MODEL_ID: &str = "faux-1";
const FAUX_API: &str = "anthropic-messages";

static TOOL_CALL_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn faux_model() -> Model {
    Model {
        r#type: None,
        id: FAUX_MODEL_ID.to_owned(),
        name: "Faux Model".to_owned(),
        api: FAUX_API.to_owned(),
        provider: FAUX_PROVIDER.to_owned(),
        base_url: "http://localhost:0".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text, ModelInput::Image],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 16_384,
        headers: None,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

#[derive(Clone, Debug)]
pub struct FauxToolCallSpec {
    pub id: Option<String>,
    pub name: String,
    pub args: JsonObject,
}

#[derive(Clone, Debug, Default)]
pub struct FauxUsageOverrides {
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cache_read: Option<i64>,
    pub cache_write: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cost: Option<UsageCost>,
}

#[derive(Clone, Debug, Default)]
pub struct FauxModelOverride {
    pub provider: Option<String>,
    pub id: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct FauxResponse {
    pub text: Option<String>,
    pub tool_calls: Option<Vec<FauxToolCallSpec>>,
    pub thinking: Option<String>,
    pub stop_reason: Option<StopReason>,
    pub error: Option<String>,
    pub usage: Option<FauxUsageOverrides>,
    pub delay_ms: Option<u64>,
    pub model: Option<FauxModelOverride>,
}

#[derive(Clone, Debug)]
pub enum FauxResponseInput {
    Text(String),
    Spec(FauxResponse),
}

impl From<&str> for FauxResponseInput {
    fn from(s: &str) -> Self {
        Self::Text(s.to_owned())
    }
}

impl From<String> for FauxResponseInput {
    fn from(s: String) -> Self {
        Self::Text(s)
    }
}

impl From<FauxResponse> for FauxResponseInput {
    fn from(r: FauxResponse) -> Self {
        Self::Spec(r)
    }
}

fn normalize_response(input: &FauxResponseInput) -> FauxResponse {
    match input {
        FauxResponseInput::Text(s) => FauxResponse {
            text: Some(s.clone()),
            ..Default::default()
        },
        FauxResponseInput::Spec(r) => r.clone(),
    }
}

fn build_usage(partial: Option<&FauxUsageOverrides>) -> Usage {
    let input = partial.and_then(|u| u.input).unwrap_or(100);
    let output = partial.and_then(|u| u.output).unwrap_or(50);
    let cache_read = partial.and_then(|u| u.cache_read).unwrap_or(0);
    let cache_write = partial.and_then(|u| u.cache_write).unwrap_or(0);
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: partial
            .and_then(|u| u.total_tokens)
            .unwrap_or(input + output + cache_read + cache_write),
        cost: partial.and_then(|u| u.cost).unwrap_or_default(),
    }
}

fn build_assistant_message(resp: &FauxResponse) -> AssistantMessage {
    let mut content: Vec<AssistantContentBlock> = Vec::new();

    if let Some(ref thinking) = resp.thinking
        && !thinking.is_empty()
    {
        content.push(AssistantContentBlock::Thinking(ThinkingContent::new(thinking)));
    }
    if let Some(ref text) = resp.text {
        content.push(AssistantContentBlock::Text(TextContent::new(text)));
    }
    if let Some(ref tool_calls) = resp.tool_calls {
        for tc in tool_calls {
            let id = tc.id.clone().unwrap_or_else(|| {
                let n = TOOL_CALL_ID_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
                format!("faux_tc_{n}")
            });
            content.push(AssistantContentBlock::ToolCall(ToolCall::new(
                id,
                &tc.name,
                tc.args.clone(),
            )));
        }
    }

    if content.is_empty() && resp.error.is_none() {
        content.push(AssistantContentBlock::Text(TextContent::new("")));
    }

    let stop_reason = if let Some(sr) = resp.stop_reason {
        sr
    } else if resp.error.is_some() {
        StopReason::Error
    } else if resp.tool_calls.as_ref().is_some_and(|t| !t.is_empty()) {
        StopReason::ToolUse
    } else {
        StopReason::Stop
    };

    AssistantMessage {
        content,
        api: FAUX_API.to_owned(),
        provider: resp
            .model
            .as_ref()
            .and_then(|m| m.provider.clone())
            .unwrap_or_else(|| FAUX_PROVIDER.to_owned()),
        model: resp
            .model
            .as_ref()
            .and_then(|m| m.id.clone())
            .unwrap_or_else(|| FAUX_MODEL_ID.to_owned()),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: build_usage(resp.usage.as_ref()),
        stop_reason,
        deferred: None,
        error_message: resp.error.clone(),
        raw_stop_reason: None,
        end_turn: None,
        duration_ms: None,
        timestamp: pi_js::time::now_ms(),
    }
}

fn chunk_string(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    let mut chunks = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let size = 3 + (pi_js::crypto::math_random() * 3.0).floor() as usize;
        let end = (i + size).min(chars.len());
        chunks.push(chars[i..end].iter().collect());
        i = end;
    }
    chunks
}

fn stream_with_deltas(stream: &AssistantMessageEventStream, message: AssistantMessage) {
    let mut partial = AssistantMessage {
        content: Vec::new(),
        stop_reason: StopReason::Pending,
        ..message.clone()
    };
    stream.push(AssistantMessageEvent::Start {
        partial: partial.clone(),
    });

    for (i, block) in message.content.iter().enumerate() {
        match block {
            AssistantContentBlock::Thinking(th) => {
                partial
                    .content
                    .push(AssistantContentBlock::Thinking(ThinkingContent::new("")));
                stream.push(AssistantMessageEvent::ThinkingStart {
                    content_index: i,
                    partial: partial.clone(),
                });
                for chunk in chunk_string(&th.thinking) {
                    if let Some(AssistantContentBlock::Thinking(p)) = partial.content.get_mut(i) {
                        p.thinking.push_str(&chunk);
                    }
                    stream.push(AssistantMessageEvent::ThinkingDelta {
                        content_index: i,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                stream.push(AssistantMessageEvent::ThinkingEnd {
                    content_index: i,
                    content: th.thinking.clone(),
                    partial: partial.clone(),
                });
            }
            AssistantContentBlock::Text(t) => {
                partial.content.push(AssistantContentBlock::Text(TextContent::new("")));
                stream.push(AssistantMessageEvent::TextStart {
                    content_index: i,
                    partial: partial.clone(),
                });
                for chunk in chunk_string(&t.text) {
                    if let Some(AssistantContentBlock::Text(p)) = partial.content.get_mut(i) {
                        p.text.push_str(&chunk);
                    }
                    stream.push(AssistantMessageEvent::TextDelta {
                        content_index: i,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                stream.push(AssistantMessageEvent::TextEnd {
                    content_index: i,
                    content: t.text.clone(),
                    partial: partial.clone(),
                });
            }
            AssistantContentBlock::ToolCall(tc) => {
                let args_json = pi_js::json::stringify(&tc.arguments);
                partial.content.push(AssistantContentBlock::ToolCall(ToolCall::new(
                    &tc.id,
                    &tc.name,
                    JsonObject::new(),
                )));
                stream.push(AssistantMessageEvent::ToolCallStart {
                    content_index: i,
                    partial: partial.clone(),
                });
                for chunk in chunk_string(&args_json) {
                    stream.push(AssistantMessageEvent::ToolCallDelta {
                        content_index: i,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                if let Some(AssistantContentBlock::ToolCall(p)) = partial.content.get_mut(i) {
                    p.arguments = tc.arguments.clone();
                }
                stream.push(AssistantMessageEvent::ToolCallEnd {
                    content_index: i,
                    tool_call: tc.clone(),
                    partial: partial.clone(),
                });
            }
        }
    }

    if message.stop_reason == StopReason::Pending {
        let err = AssistantMessage {
            stop_reason: StopReason::Error,
            error_message: Some("Faux response ended without a stop reason".to_owned()),
            ..message
        };
        stream.push(AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: err,
        });
        return;
    }
    if message.stop_reason == StopReason::Error || message.stop_reason == StopReason::Aborted {
        stream.push(AssistantMessageEvent::Error {
            reason: message.stop_reason,
            error: message,
        });
        return;
    }
    stream.push(AssistantMessageEvent::Done {
        reason: message.stop_reason,
        message,
    });
}

#[derive(Clone, Debug, Default)]
pub struct FauxStreamFnState {
    pub call_count: usize,
    pub contexts: Vec<Context>,
}

pub fn create_faux_stream_fn(responses: Vec<FauxResponseInput>) -> Result<(StreamFn, Arc<Mutex<FauxStreamFnState>>)> {
    if responses.is_empty() {
        return Err(Error::msg("createFauxStreamFn requires at least one response"));
    }

    let state = Arc::new(Mutex::new(FauxStreamFnState::default()));
    let state_clone = state.clone();
    let responses = Arc::new(responses);

    let stream_fn: StreamFn = Arc::new(
        move |_model: Model, context: Context, _options: Option<SimpleStreamOptions>| {
            let state = state_clone.clone();
            let responses = responses.clone();
            Box::pin(async move {
                let index = {
                    let mut guard = state.lock().unwrap_or_else(PoisonError::into_inner);
                    let idx = guard.call_count % responses.len();
                    guard.call_count += 1;
                    guard.contexts.push(context);
                    idx
                };
                let resp = normalize_response(&responses[index]);
                let message = build_assistant_message(&resp);
                let stream = create_assistant_message_event_stream();
                let stream_clone = stream.clone();
                if let Some(delay) = resp.delay_ms.filter(|&d| d > 0) {
                    tokio::spawn(async move {
                        let _ = pi_js::time::sleep(delay, None).await;
                        stream_with_deltas(&stream_clone, message);
                    });
                } else {
                    tokio::spawn(async move {
                        tokio::task::yield_now().await;
                        stream_with_deltas(&stream_clone, message);
                    });
                }
                Ok(stream)
            })
        },
    );

    Ok((stream_fn, state))
}

#[derive(Clone, Default)]
pub struct HarnessOptions {
    pub responses: Option<Vec<FauxResponseInput>>,
    pub model: Option<Model>,
    pub context_window: Option<i64>,
    pub settings: Option<Settings>,
    pub system_prompt: Option<String>,
    pub tools: Option<Vec<AgentTool>>,
    pub base_tools_override: Option<IndexMap<String, AgentTool>>,
    pub resource_loader: Option<TestResourceLoader>,
    pub extension_factories: Option<Vec<TestExtensionInput>>,
}

#[derive(Clone, Debug)]
pub enum HarnessSessionEvent {
    Agent(AgentEvent),
    AutoRetryStart {
        attempt: usize,
        delay_ms: u64,
        error_message: String,
    },
    AutoRetryEnd {
        success: bool,
        attempt: usize,
    },
}

#[derive(Clone)]
pub struct HarnessSession {
    agent_session: AgentSession,
    pub extension_runner: Option<TestExtensionRunner>,
}

impl HarnessSession {
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.agent_session.messages()
    }

    pub async fn prompt(&self, text: &str) -> Result<()> {
        self.agent_session.prompt(text, None).await
    }

    pub fn dispose(&self) {
        self.agent_session.dispose();
    }
}

pub struct Harness {
    pub session: HarnessSession,
    pub agent: Agent,
    pub session_manager: SessionManager,
    pub settings_manager: SettingsManager,
    pub model_registry: ModelRegistry,
    pub model_runtime: ModelRuntime,
    pub faux_state: Arc<Mutex<FauxStreamFnState>>,
    pub events: Arc<Mutex<Vec<HarnessSessionEvent>>>,
    pub temp_dir: String,
    _unsub: pi_js::Unsubscribe,
    _session_unsub: pi_js::Unsubscribe,
}

impl Harness {
    pub fn faux(&self) -> FauxStreamFnState {
        self.faux_state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn events_snapshot(&self) -> Vec<HarnessSessionEvent> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn agent_events(&self) -> Vec<AgentEvent> {
        self.events_snapshot()
            .into_iter()
            .filter_map(|e| match e {
                HarnessSessionEvent::Agent(ae) => Some(ae),
                _ => None,
            })
            .collect()
    }

    pub fn cleanup(&self) {
        self.session.dispose();
        if pi_js::fs::exists(&self.temp_dir) {
            let _ = pi_js::fs::rm(&self.temp_dir, true, true);
        }
    }
}

async fn create_harness_with_resource_loader(
    options: HarnessOptions,
    resource_loader: TestResourceLoader,
    temp_dir: String,
) -> Result<Harness> {
    let mut model = options.model.unwrap_or_else(faux_model);
    if let Some(cw) = options.context_window {
        model.context_window = cw;
    }

    let responses = options.responses.unwrap_or_else(|| vec![FauxResponseInput::from("ok")]);
    let (stream_fn, faux_state) = create_faux_stream_fn(responses)?;

    let mut tools = options.tools.unwrap_or_default();
    let base_tools_override = if let Some(override_map) = options.base_tools_override {
        for (_, t) in &override_map {
            if !tools.iter().any(|existing| existing.name == t.name) {
                tools.push(t.clone());
            }
        }
        Some(override_map)
    } else {
        Some(
            tools
                .iter()
                .map(|t| (t.name.clone(), t.clone()))
                .collect::<IndexMap<String, AgentTool>>(),
        )
    };
    let active_tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();

    let agent = Agent::new(AgentOptions {
        get_api_key: Some(Arc::new(|_| Box::pin(async { Ok(Some("faux-key".to_owned())) }))),
        initial_state: Some(AgentInitialState {
            model: Some(model.clone()),
            system_prompt: Some(
                options
                    .system_prompt
                    .unwrap_or_else(|| "You are a test assistant.".to_owned()),
            ),
            tools: Some(tools),
            ..Default::default()
        }),
        stream_fn: Some(stream_fn),
        ..Default::default()
    });

    let session_manager = SessionManager::in_memory(Some(&temp_dir), None, None)?;
    let settings_manager = SettingsManager::create(&temp_dir, Some(&temp_dir), None);
    if let Some(ref settings) = options.settings {
        settings_manager.apply_overrides(settings);
    }

    let mut auth_data = IndexMap::new();
    auth_data.insert(
        model.provider.clone(),
        make_api_key_credential(Some("faux-key".to_owned()), None),
    );
    let auth_storage = Arc::new(AuthStorage::in_memory(Some(auth_data)));
    let model_registry = create_in_memory_model_registry(auth_storage).await?;
    model_registry.register_provider(
        &model.provider,
        ProviderConfigInput {
            base_url: Some(model.base_url.clone()),
            api: Some(model.api.clone()),
            models: Some(vec![ProviderModelConfig::Chat(ProviderChatModelConfig {
                id: model.id.clone(),
                name: model.name.clone(),
                api: Some(model.api.clone()),
                base_url: Some(model.base_url.clone()),
                input: model.input.clone(),
                input_limits: model.input_limits,
                cost: model.cost.clone(),
                headers: model.headers.clone(),
                reasoning: model.reasoning,
                thinking_level_map: model.thinking_level_map.clone(),
                prompt_cache: model.prompt_cache.clone(),
                context_window: model.context_window,
                max_tokens: model.max_tokens,
                sampling_params: model.sampling_params.clone(),
                sampling_params_by_thinking_level: model.sampling_params_by_thinking_level.clone(),
                compat: model.compat.clone(),
            })]),
            ..Default::default()
        },
    )?;
    let model_runtime = get_model_runtime(&model_registry);

    let events = Arc::new(Mutex::new(Vec::new()));
    let events_sub = events.clone();
    let unsub = agent.subscribe(move |ev| {
        events_sub
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(HarnessSessionEvent::Agent(ev.clone()));
    });

    let extension_runner = if resource_loader.get_extensions().extensions.is_empty() {
        None
    } else {
        Some(resource_loader.get_extensions().runner.clone())
    };

    let agent_session = AgentSession::new(AgentSessionConfig {
        agent: agent.clone(),
        session_manager: session_manager.clone(),
        settings_manager: settings_manager.clone(),
        cwd: temp_dir.clone(),
        scoped_models: None,
        resource_loader: Arc::new(resource_loader),
        custom_tools: None,
        model_runtime: model_runtime.clone(),
        cache_warmer: None,
        initial_active_tool_names: Some(active_tool_names),
        uses_default_tools: Some(false),
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override,
        extension_runner_ref: None,
        session_start_event: None,
        initial_model: Some(model),
        initial_thinking_level: None,
        default_tool_modifiers: None,
    });
    let session_events = events.clone();
    let session_unsub = agent_session.subscribe(move |ev| match ev {
        AgentSessionEvent::AutoRetryStart {
            attempt,
            delay_ms,
            error_message,
            ..
        } => {
            session_events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(HarnessSessionEvent::AutoRetryStart {
                    attempt: attempt as usize,
                    delay_ms,
                    error_message,
                });
        }
        AgentSessionEvent::AutoRetryEnd { success, attempt, .. } => {
            session_events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(HarnessSessionEvent::AutoRetryEnd {
                    success,
                    attempt: attempt as usize,
                });
        }
        _ => {}
    });

    let session = HarnessSession {
        agent_session,
        extension_runner,
    };

    Ok(Harness {
        session,
        agent,
        session_manager,
        settings_manager,
        model_registry,
        model_runtime,
        faux_state,
        events,
        temp_dir,
        _unsub: unsub,
        _session_unsub: session_unsub,
    })
}

pub async fn create_harness(options: Option<HarnessOptions>) -> Result<Harness> {
    let opts = options.unwrap_or_default();
    if opts.extension_factories.as_ref().is_some_and(|v| !v.is_empty()) {
        return Err(Error::msg(
            "createHarness does not support extensionFactories. Use createHarnessWithExtensions().",
        ));
    }
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-harness-"]))?;
    let resource_loader = opts
        .resource_loader
        .clone()
        .unwrap_or_else(|| create_test_resource_loader(None));
    create_harness_with_resource_loader(opts, resource_loader, temp_dir).await
}

pub async fn create_harness_with_extensions(options: Option<HarnessOptions>) -> Result<Harness> {
    let mut opts = options.unwrap_or_default();
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-harness-"]))?;
    let factories = opts.extension_factories.take().unwrap_or_default();
    let extensions_result = create_test_extensions_result(factories, Some(&temp_dir)).await?;
    let resource_loader = opts.resource_loader.clone().unwrap_or_else(|| {
        create_test_resource_loader(Some(CreateTestResourceLoaderOptions {
            extensions_result: Some(extensions_result),
        }))
    });
    create_harness_with_resource_loader(opts, resource_loader, temp_dir).await
}

pub fn context_from_faux(ctx: &Context) -> &TranscriptContext {
    // Helper keeping TranscriptContext re-export accessible when needed.
    let _ = std::marker::PhantomData::<TranscriptContext>;
    let _ = ctx;
    unreachable!("use ctx.messages directly")
}
