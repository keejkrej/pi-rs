//! Port of packages/agent/src/agent.ts

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use indexmap::{IndexMap, IndexSet};
use pi_ai::{
    AssistantContentBlock, FauxAssistantMessageContent, ImageContent, Message, Model, ModelCost, StopReason,
    SystemMessage, SystemMessageContent, ThinkingBudgets, Transport, UserContentBlock, UserMessage, UserMessageContent,
    compat, create_initial_system_message, faux_assistant_message, faux_text, get_current_system_message,
    get_current_system_prompt, to_tool_declaration,
};
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::time::now_ms;
use pi_js::{BoxFuture, Error, Result, Unsubscribe};
use tokio::sync::oneshot;

use crate::agent_loop::{run_agent_loop_async, run_agent_loop_continue_async};
use crate::stream_fn::get_default_stream_fn;
use crate::types::{
    AfterToolCallFn, AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, AgentState, AgentTool, BeforeToolCallFn,
    ConvertToLlmFn, FinishTurnFn, GetApiKeyFn, OnPayloadFn, PrepareNextTurnFn, PrepareNextTurnWithContextFn,
    PrepareRequestFn, QueueMode, StreamFn, ThinkingLevel, ToolExecutionMode, TransformContextFn,
};

/// Input accepted by [`Agent::prompt`].
#[derive(Clone, Debug)]
pub enum PromptInput {
    Text {
        text: String,
        images: Option<Vec<ImageContent>>,
    },
    Message(AgentMessage),
    Messages(Vec<AgentMessage>),
}

impl From<&str> for PromptInput {
    fn from(text: &str) -> Self {
        Self::Text {
            text: text.to_string(),
            images: None,
        }
    }
}

impl From<String> for PromptInput {
    fn from(text: String) -> Self {
        Self::Text { text, images: None }
    }
}

impl From<AgentMessage> for PromptInput {
    fn from(message: AgentMessage) -> Self {
        Self::Message(message)
    }
}

impl From<Vec<AgentMessage>> for PromptInput {
    fn from(messages: Vec<AgentMessage>) -> Self {
        Self::Messages(messages)
    }
}

/// Optional partial initial state passed inside [`AgentOptions`].
#[derive(Clone, Debug, Default)]
pub struct AgentInitialState {
    pub system_prompt: Option<String>,
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
    pub tools: Option<Vec<AgentTool>>,
    pub messages: Option<Vec<AgentMessage>>,
}

/// Configuration options passed to [`Agent::new`].
#[derive(Clone, Default)]
pub struct AgentOptions {
    pub initial_state: Option<AgentInitialState>,
    pub convert_to_llm: Option<ConvertToLlmFn>,
    pub transform_context: Option<TransformContextFn>,
    pub steering_mode: Option<QueueMode>,
    pub follow_up_mode: Option<QueueMode>,
    pub stream_fn: Option<StreamFn>,
    pub session_id: Option<String>,
    pub get_api_key: Option<GetApiKeyFn>,
    pub on_payload: Option<OnPayloadFn>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub transport: Option<Transport>,
    pub max_retry_delay_ms: Option<u64>,
    pub tool_execution: Option<ToolExecutionMode>,
    pub before_tool_call: Option<BeforeToolCallFn>,
    pub after_tool_call: Option<AfterToolCallFn>,
    pub finish_turn: Option<FinishTurnFn>,
    pub prepare_next_turn: Option<PrepareNextTurnFn>,
    pub prepare_next_turn_with_context: Option<PrepareNextTurnWithContextFn>,
    pub prepare_request: Option<PrepareRequestFn>,
}

/// FIFO message queue supporting `"all"` and `"one-at-a-time"` draining modes.
#[derive(Clone, Debug, Default)]
pub struct PendingMessageQueue {
    items: VecDeque<AgentMessage>,
    mode: QueueMode,
}

impl PendingMessageQueue {
    pub fn new(mode: QueueMode) -> Self {
        Self {
            items: VecDeque::new(),
            mode,
        }
    }

    pub fn mode(&self) -> QueueMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: QueueMode) {
        self.mode = mode;
    }

    pub fn push(&mut self, message: AgentMessage) {
        self.items.push_back(message);
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn peek(&self) -> Vec<AgentMessage> {
        match self.mode {
            QueueMode::OneAtATime => self.items.front().cloned().into_iter().collect(),
            QueueMode::All => self.items.iter().cloned().collect(),
        }
    }

    pub fn dequeue(&mut self) -> Vec<AgentMessage> {
        match self.mode {
            QueueMode::OneAtATime => self.items.pop_front().into_iter().collect(),
            QueueMode::All => self.items.drain(..).collect(),
        }
    }
}

type AgentListener = Arc<dyn Fn(AgentEvent, AbortSignal) -> BoxFuture<()> + Send + Sync>;

struct AgentInnerState {
    state: AgentState,
    listeners: IndexMap<u64, AgentListener>,
    next_listener_id: u64,
    convert_to_llm: ConvertToLlmFn,
    transform_context: Option<TransformContextFn>,
    steering_queue: PendingMessageQueue,
    follow_up_queue: PendingMessageQueue,
    abort_controller: Option<Arc<AbortController>>,
    stream_fn: Option<StreamFn>,
    session_id: Option<String>,
    get_api_key: Option<GetApiKeyFn>,
    on_payload: Option<OnPayloadFn>,
    thinking_budgets: Option<ThinkingBudgets>,
    transport: Transport,
    max_retry_delay_ms: Option<u64>,
    tool_execution: ToolExecutionMode,
    before_tool_call: Option<BeforeToolCallFn>,
    after_tool_call: Option<AfterToolCallFn>,
    finish_turn: Option<FinishTurnFn>,
    prepare_next_turn: Option<PrepareNextTurnFn>,
    prepare_next_turn_with_context: Option<PrepareNextTurnWithContextFn>,
    prepare_request: Option<PrepareRequestFn>,
    idle_waiters: Vec<oneshot::Sender<()>>,
}

struct AgentInner {
    lock: Mutex<AgentInnerState>,
}

/// High-level stateful conversational agent wrapping [`agent_loop`] and [`agent_loop_continue`].
///
/// Cloning an `Agent` produces a handle pointing to the same underlying state and queues.
#[derive(Clone)]
pub struct Agent {
    inner: Arc<AgentInner>,
}

impl Default for Agent {
    fn default() -> Self {
        Self::new(AgentOptions::default())
    }
}

impl Agent {
    /// Creates a new [`Agent`] instance initialized with `options`.
    pub fn new(options: AgentOptions) -> Self {
        let init = options.initial_state.unwrap_or_default();
        let default_model = init.model.unwrap_or_else(default_fallback_model);
        let tools = init.tools.unwrap_or_default();
        let mut messages = init.messages.unwrap_or_default();
        let tool_decls = to_tool_declarations(&tools);
        if messages.first().and_then(AgentMessage::role) != Some("system")
            && let Some(initial_sys) = create_initial_system_message(init.system_prompt.as_deref(), Some(&tool_decls))
        {
            messages.insert(0, AgentMessage::from(initial_sys));
        }
        let system_prompt = sync_system_prompt(&messages);
        let state = AgentState {
            system_prompt,
            model: default_model,
            thinking_level: init.thinking_level.unwrap_or(ThinkingLevel::Off),
            tools,
            messages,
            is_streaming: false,
            stream_message: None,
            pending_tool_calls: IndexSet::new(),
            error: None,
        };

        let convert_to_llm = options
            .convert_to_llm
            .unwrap_or_else(|| Arc::new(default_convert_to_llm));

        let inner = AgentInnerState {
            state,
            listeners: IndexMap::new(),
            next_listener_id: 1,
            convert_to_llm,
            transform_context: options.transform_context,
            steering_queue: PendingMessageQueue::new(options.steering_mode.unwrap_or(QueueMode::OneAtATime)),
            follow_up_queue: PendingMessageQueue::new(options.follow_up_mode.unwrap_or(QueueMode::OneAtATime)),
            abort_controller: None,
            stream_fn: options.stream_fn,
            session_id: options.session_id,
            get_api_key: options.get_api_key,
            on_payload: options.on_payload,
            thinking_budgets: options.thinking_budgets,
            transport: options.transport.unwrap_or(Transport::Sse),
            max_retry_delay_ms: options.max_retry_delay_ms,
            tool_execution: options.tool_execution.unwrap_or(ToolExecutionMode::Parallel),
            before_tool_call: options.before_tool_call,
            after_tool_call: options.after_tool_call,
            finish_turn: options.finish_turn,
            prepare_next_turn: options.prepare_next_turn,
            prepare_next_turn_with_context: options.prepare_next_turn_with_context,
            prepare_request: options.prepare_request,
            idle_waiters: Vec::new(),
        };

        Self {
            inner: Arc::new(AgentInner {
                lock: Mutex::new(inner),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, AgentInnerState> {
        self.inner.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Returns a cloned snapshot of the current [`AgentState`].
    pub fn state(&self) -> AgentState {
        self.lock().state.clone()
    }

    pub fn session_id(&self) -> Option<String> {
        self.lock().session_id.clone()
    }

    pub fn set_session_id(&self, value: Option<String>) {
        self.lock().session_id = value;
    }

    pub fn thinking_budgets(&self) -> Option<ThinkingBudgets> {
        self.lock().thinking_budgets.clone()
    }

    pub fn set_thinking_budgets(&self, value: Option<ThinkingBudgets>) {
        self.lock().thinking_budgets = value;
    }

    pub fn transport(&self) -> Transport {
        self.lock().transport
    }

    pub fn set_transport(&self, value: Transport) {
        self.lock().transport = value;
    }

    pub fn max_retry_delay_ms(&self) -> Option<u64> {
        self.lock().max_retry_delay_ms
    }

    pub fn set_max_retry_delay_ms(&self, value: Option<u64>) {
        self.lock().max_retry_delay_ms = value;
    }

    pub fn tool_execution(&self) -> ToolExecutionMode {
        self.lock().tool_execution
    }

    pub fn set_tool_execution(&self, value: ToolExecutionMode) {
        self.lock().tool_execution = value;
    }

    pub fn stream_fn(&self) -> Option<StreamFn> {
        self.lock().stream_fn.clone()
    }

    pub fn set_stream_fn(&self, value: Option<StreamFn>) {
        self.lock().stream_fn = value;
    }

    pub fn get_api_key(&self) -> Option<GetApiKeyFn> {
        self.lock().get_api_key.clone()
    }

    pub fn set_get_api_key(&self, value: Option<GetApiKeyFn>) {
        self.lock().get_api_key = value;
    }

    pub fn on_payload(&self) -> Option<OnPayloadFn> {
        self.lock().on_payload.clone()
    }

    pub fn set_on_payload(&self, value: Option<OnPayloadFn>) {
        self.lock().on_payload = value;
    }

    pub fn before_tool_call(&self) -> Option<BeforeToolCallFn> {
        self.lock().before_tool_call.clone()
    }

    pub fn set_before_tool_call(&self, value: Option<BeforeToolCallFn>) {
        self.lock().before_tool_call = value;
    }

    pub fn after_tool_call(&self) -> Option<AfterToolCallFn> {
        self.lock().after_tool_call.clone()
    }

    pub fn set_after_tool_call(&self, value: Option<AfterToolCallFn>) {
        self.lock().after_tool_call = value;
    }

    pub fn finish_turn(&self) -> Option<FinishTurnFn> {
        self.lock().finish_turn.clone()
    }

    pub fn set_finish_turn(&self, value: Option<FinishTurnFn>) {
        self.lock().finish_turn = value;
    }

    pub fn prepare_next_turn(&self) -> Option<PrepareNextTurnFn> {
        self.lock().prepare_next_turn.clone()
    }

    pub fn set_prepare_next_turn(&self, value: Option<PrepareNextTurnFn>) {
        self.lock().prepare_next_turn = value;
    }

    pub fn prepare_next_turn_with_context(&self) -> Option<PrepareNextTurnWithContextFn> {
        self.lock().prepare_next_turn_with_context.clone()
    }

    pub fn set_prepare_next_turn_with_context(&self, value: Option<PrepareNextTurnWithContextFn>) {
        self.lock().prepare_next_turn_with_context = value;
    }

    pub fn prepare_request(&self) -> Option<PrepareRequestFn> {
        self.lock().prepare_request.clone()
    }

    pub fn set_prepare_request(&self, value: Option<PrepareRequestFn>) {
        self.lock().prepare_request = value;
    }

    pub fn set_system_prompt(&self, prompt: impl Into<String>) {
        let prompt = prompt.into();
        let mut guard = self.lock();
        if let Some(AgentMessage::Message(Message::System(sys))) = guard.state.messages.first_mut() {
            sys.content = SystemMessageContent::Text(prompt);
        } else {
            let tool_decls = to_tool_declarations(&guard.state.tools);
            if let Some(sys) = create_initial_system_message(Some(&prompt), Some(&tool_decls)) {
                guard.state.messages.insert(0, AgentMessage::from(sys));
            }
        }
        guard.state.system_prompt = sync_system_prompt(&guard.state.messages);
    }

    pub fn set_model(&self, model: Model) {
        self.lock().state.model = model;
    }

    pub fn set_thinking_level(&self, level: ThinkingLevel) {
        self.lock().state.thinking_level = level;
    }

    pub fn set_tools(&self, tools: Vec<AgentTool>) {
        self.lock().state.tools = tools;
    }

    pub fn replace_messages(&self, messages: Vec<AgentMessage>) {
        let mut guard = self.lock();
        guard.state.system_prompt = sync_system_prompt(&messages);
        guard.state.messages = messages;
    }

    pub fn append_message(&self, message: AgentMessage) {
        let mut guard = self.lock();
        guard.state.messages.push(message);
        guard.state.system_prompt = sync_system_prompt(&guard.state.messages);
    }

    pub fn clear_messages(&self) {
        let mut guard = self.lock();
        guard.state.messages.clear();
        guard.state.system_prompt.clear();
    }

    pub fn steering_mode(&self) -> QueueMode {
        self.lock().steering_queue.mode()
    }

    pub fn set_steering_mode(&self, mode: QueueMode) {
        self.lock().steering_queue.set_mode(mode);
    }

    pub fn follow_up_mode(&self) -> QueueMode {
        self.lock().follow_up_queue.mode()
    }

    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        self.lock().follow_up_queue.set_mode(mode);
    }

    pub fn steer(&self, message: AgentMessage) {
        self.lock().steering_queue.push(message);
    }

    pub fn follow_up(&self, message: AgentMessage) {
        self.lock().follow_up_queue.push(message);
    }

    pub fn clear_steering_queue(&self) {
        self.lock().steering_queue.clear();
    }

    pub fn clear_follow_up_queue(&self) {
        self.lock().follow_up_queue.clear();
    }

    pub fn clear_all_queues(&self) {
        let mut guard = self.lock();
        guard.steering_queue.clear();
        guard.follow_up_queue.clear();
    }

    pub fn has_queued_messages(&self) -> bool {
        let guard = self.lock();
        !guard.steering_queue.is_empty() || !guard.follow_up_queue.is_empty()
    }

    /// Previews the messages that would be dequeued for the next turn without consuming them.
    pub fn peek_queued_messages(&self) -> Vec<AgentMessage> {
        let guard = self.lock();
        let steering = guard.steering_queue.peek();
        if !steering.is_empty() {
            steering
        } else {
            guard.follow_up_queue.peek()
        }
    }

    /// Returns the active [`AbortSignal`] for the current run, if any.
    pub fn signal(&self) -> Option<AbortSignal> {
        self.lock().abort_controller.as_ref().map(|c| c.signal())
    }

    /// Registers a synchronous event listener. Call [`Unsubscribe::call`] to detach it.
    pub fn subscribe(&self, listener: impl Fn(&AgentEvent) + Send + Sync + 'static) -> Unsubscribe {
        self.subscribe_async(move |event, _signal| {
            listener(&event);
            Box::pin(async {})
        })
    }

    /// Registers an asynchronous event listener receiving `(event, abort_signal)`.
    ///
    /// Listeners are awaited in subscription order, and the agent remains in streaming
    /// state until all awaited listeners for `agent_end` finish.
    pub fn subscribe_async(
        &self,
        listener: impl Fn(AgentEvent, AbortSignal) -> BoxFuture<()> + Send + Sync + 'static,
    ) -> Unsubscribe {
        let id = {
            let mut guard = self.lock();
            let id = guard.next_listener_id;
            guard.next_listener_id += 1;
            guard.listeners.insert(id, Arc::new(listener));
            id
        };
        let weak = Arc::downgrade(&self.inner);
        Unsubscribe::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner
                    .lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .listeners
                    .shift_remove(&id);
            }
        })
    }

    /// Signals abort on the active prompt/continue run, if any.
    pub fn abort(&self) {
        let controller = self.lock().abort_controller.clone();
        if let Some(c) = controller {
            c.abort(None);
        }
    }

    /// Waits until no prompt/continue run is streaming.
    pub async fn wait_for_idle(&self) {
        let rx = {
            let mut guard = self.lock();
            if !guard.state.is_streaming {
                return;
            }
            let (tx, rx) = oneshot::channel();
            guard.idle_waiters.push(tx);
            rx
        };
        let _ = rx.await;
    }

    /// Resets conversation messages while preserving the replayed prompt/tool baseline, and clears queues.
    pub fn reset(&self) -> Result<()> {
        let mut guard = self.lock();
        if guard.state.is_streaming {
            return Err(Error::msg(
                "Agent is already processing. Wait for completion before resetting.",
            ));
        }
        let baseline = current_system_message(&guard.state.messages);
        guard.state.messages = baseline.map(|m| vec![AgentMessage::from(m)]).unwrap_or_default();
        guard.state.system_prompt = sync_system_prompt(&guard.state.messages);
        guard.state.is_streaming = false;
        guard.state.stream_message = None;
        guard.state.pending_tool_calls.clear();
        guard.state.error = None;
        guard.steering_queue.clear();
        guard.follow_up_queue.clear();
        Ok(())
    }

    /// Sends a user text prompt, message, or batch of messages to the agent loop.
    pub async fn prompt(&self, input: impl Into<PromptInput>) -> Result<()> {
        if self.lock().state.is_streaming {
            return Err(Error::msg(
                "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion.",
            ));
        }

        let msgs = match input.into() {
            PromptInput::Text { text, images } => {
                let mut content = vec![UserContentBlock::Text(faux_text(&text))];
                if let Some(imgs) = images {
                    for img in imgs {
                        content.push(UserContentBlock::Image(img));
                    }
                }
                let user_msg = Message::User(UserMessage {
                    content: UserMessageContent::Blocks(content),
                    timestamp: now_ms(),
                });
                vec![AgentMessage::from(user_msg)]
            }
            PromptInput::Message(msg) => vec![msg],
            PromptInput::Messages(msgs) => msgs,
        };

        self.run_loop_internal(Some(msgs), false).await
    }

    /// Convenience helper for sending plain text + optional images.
    pub async fn prompt_text(&self, text: impl Into<String>, images: Option<Vec<ImageContent>>) -> Result<()> {
        self.prompt(PromptInput::Text {
            text: text.into(),
            images,
        })
        .await
    }

    /// Convenience helper for sending a single [`AgentMessage`].
    pub async fn prompt_message(&self, message: AgentMessage) -> Result<()> {
        self.prompt(PromptInput::Message(message)).await
    }

    /// Convenience helper for sending multiple [`AgentMessage`]s.
    pub async fn prompt_messages(&self, messages: Vec<AgentMessage>) -> Result<()> {
        self.prompt(PromptInput::Messages(messages)).await
    }

    /// Continues execution from the current message history or queued steering/follow-up messages.
    pub async fn continue_run(&self) -> Result<()> {
        enum ContinuePlan {
            RunWithPrompts {
                prompts: Vec<AgentMessage>,
                skip_initial_steering_poll: bool,
            },
            ContinueContext,
        }

        let plan = {
            let mut guard = self.lock();
            if guard.state.is_streaming {
                return Err(Error::msg(
                    "Agent is already processing. Wait for completion before continuing.",
                ));
            }
            let Some(last) = guard.state.messages.last() else {
                return Err(Error::msg("No messages to continue from"));
            };
            if guard.state.messages.iter().all(|m| m.role() == Some("system")) {
                return Err(Error::msg("No messages to continue from"));
            }
            if last.role() == Some("assistant") {
                let steering = guard.steering_queue.dequeue();
                if !steering.is_empty() {
                    ContinuePlan::RunWithPrompts {
                        prompts: steering,
                        skip_initial_steering_poll: true,
                    }
                } else {
                    let follow_up = guard.follow_up_queue.dequeue();
                    if !follow_up.is_empty() {
                        ContinuePlan::RunWithPrompts {
                            prompts: follow_up,
                            skip_initial_steering_poll: false,
                        }
                    } else {
                        return Err(Error::msg("Cannot continue from message role: assistant"));
                    }
                }
            } else {
                ContinuePlan::ContinueContext
            }
        };

        match plan {
            ContinuePlan::RunWithPrompts {
                prompts,
                skip_initial_steering_poll,
            } => self.run_loop_internal(Some(prompts), skip_initial_steering_poll).await,
            ContinuePlan::ContinueContext => self.run_loop_internal(None, false).await,
        }
    }

    async fn run_loop_internal(
        &self,
        messages: Option<Vec<AgentMessage>>,
        skip_initial_steering_poll: bool,
    ) -> Result<()> {
        let controller = Arc::new(AbortController::new());
        let signal = controller.signal();

        let (context, config, effective_stream_fn, model) = {
            let mut guard = self.lock();
            if guard.state.is_streaming {
                return Err(Error::msg(
                    "Agent is already processing. Wait for completion before starting a new run.",
                ));
            }
            guard.state.is_streaming = true;
            guard.state.stream_message = None;
            guard.state.error = None;
            guard.abort_controller = Some(Arc::clone(&controller));

            let model = guard.state.model.clone();
            let context = AgentContext {
                messages: guard.state.messages.clone(),
                tools: Some(guard.state.tools.clone()),
            };

            let inner_for_steering = Arc::clone(&self.inner);
            let skip_flag = Arc::new(AtomicBool::new(skip_initial_steering_poll));
            let get_steering = Arc::new(move || {
                let inner = Arc::clone(&inner_for_steering);
                let skip = Arc::clone(&skip_flag);
                Box::pin(async move {
                    if skip.swap(false, Ordering::SeqCst) {
                        return Ok(Vec::new());
                    }
                    Ok(inner
                        .lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .steering_queue
                        .dequeue())
                }) as BoxFuture<Result<Vec<AgentMessage>>>
            });

            let inner_for_follow_up = Arc::clone(&self.inner);
            let get_follow_up = Arc::new(move || {
                let inner = Arc::clone(&inner_for_follow_up);
                Box::pin(async move {
                    Ok(inner
                        .lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .follow_up_queue
                        .dequeue())
                }) as BoxFuture<Result<Vec<AgentMessage>>>
            });

            let effective_stream_fn = guard.stream_fn.clone().or_else(get_default_stream_fn);

            let mut cfg = AgentLoopConfig::new(model.clone(), guard.convert_to_llm.clone());
            cfg.reasoning = if guard.state.thinking_level == ThinkingLevel::Off {
                None
            } else {
                Some(guard.state.thinking_level)
            };
            cfg.session_id = guard.session_id.clone();
            cfg.transport = Some(guard.transport);
            cfg.thinking_budgets = guard.thinking_budgets.clone();
            cfg.max_retry_delay_ms = guard.max_retry_delay_ms;
            cfg.on_payload = guard.on_payload.clone();
            cfg.tool_execution = Some(guard.tool_execution);
            cfg.transform_context = guard.transform_context.clone();
            cfg.get_api_key = guard.get_api_key.clone();
            cfg.get_steering_messages = Some(get_steering);
            cfg.get_follow_up_messages = Some(get_follow_up);
            cfg.before_tool_call = guard.before_tool_call.clone();
            cfg.after_tool_call = guard.after_tool_call.clone();
            cfg.finish_turn = guard.finish_turn.clone();
            cfg.prepare_next_turn = guard.prepare_next_turn.clone();
            cfg.prepare_next_turn_with_context = guard.prepare_next_turn_with_context.clone();
            cfg.prepare_request = guard.prepare_request.clone();
            cfg.stream_fn = effective_stream_fn.clone();

            (context, cfg, effective_stream_fn, model)
        };

        let _idle_guard = IdleGuard {
            inner: Arc::clone(&self.inner),
        };

        let agent = self.clone();
        let emit_signal = signal.clone();
        let emit = Arc::new(move |event: AgentEvent| {
            let agent = agent.clone();
            let signal = emit_signal.clone();
            Box::pin(async move {
                agent.process_event(event, &signal).await;
            }) as BoxFuture<()>
        });

        let outcome = match messages {
            Some(msgs) => {
                run_agent_loop_async(msgs, context, config, emit, Some(signal.clone()), effective_stream_fn).await
            }
            None => {
                run_agent_loop_continue_async(context, config, emit, Some(signal.clone()), effective_stream_fn).await
            }
        };

        if let Err(err) = outcome {
            self.handle_run_failure(&model, err.message(), signal.aborted(), &signal)
                .await;
        }

        Ok(())
    }

    async fn handle_run_failure(&self, model: &Model, error_text: String, aborted: bool, signal: &AbortSignal) {
        let mut msg = faux_assistant_message(FauxAssistantMessageContent::Blocks(Vec::new()), None);
        msg.content = vec![AssistantContentBlock::Text(faux_text(""))];
        msg.api = model.api.clone();
        msg.provider = model.provider.clone();
        msg.model = model.id.clone();
        msg.stop_reason = if aborted {
            StopReason::Aborted
        } else {
            StopReason::Error
        };
        msg.error_message = Some(error_text);
        msg.timestamp = now_ms();

        let agent_msg = AgentMessage::from(msg);
        self.process_event(
            AgentEvent::MessageStart {
                message: agent_msg.clone(),
            },
            signal,
        )
        .await;
        self.process_event(
            AgentEvent::MessageEnd {
                message: agent_msg.clone(),
            },
            signal,
        )
        .await;
        self.process_event(
            AgentEvent::TurnEnd {
                message: agent_msg.clone(),
                tool_results: Vec::new(),
            },
            signal,
        )
        .await;
        self.process_event(
            AgentEvent::AgentEnd {
                messages: vec![agent_msg],
            },
            signal,
        )
        .await;
    }

    async fn process_event(&self, event: AgentEvent, signal: &AbortSignal) {
        let listeners: Vec<AgentListener> = {
            let mut guard = self.lock();
            match &event {
                AgentEvent::MessageStart { message } | AgentEvent::MessageUpdate { message, .. } => {
                    guard.state.stream_message = Some(message.clone());
                }
                AgentEvent::MessageEnd { message } => {
                    guard.state.stream_message = None;
                    guard.state.messages.push(message.clone());
                    guard.state.system_prompt = sync_system_prompt(&guard.state.messages);
                }
                AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                    guard.state.pending_tool_calls.insert(tool_call_id.clone());
                }
                AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                    guard.state.pending_tool_calls.shift_remove(tool_call_id);
                }
                AgentEvent::TurnEnd { message, .. } => {
                    if let Some(assistant) = message.as_assistant()
                        && let Some(err_msg) = &assistant.error_message
                    {
                        guard.state.error = Some(err_msg.clone());
                    }
                }
                AgentEvent::AgentEnd { .. } => {
                    // Keep `is_streaming = true` until all `agent_end` listeners settle;
                    // `IdleGuard::drop` clears streaming state and notifies `wait_for_idle`.
                    guard.state.stream_message = None;
                }
                AgentEvent::AgentStart | AgentEvent::TurnStart | AgentEvent::ToolExecutionUpdate { .. } => {}
            }
            guard.listeners.values().cloned().collect()
        };

        for listener in listeners {
            listener(event.clone(), signal.clone()).await;
        }
    }
}

struct IdleGuard {
    inner: Arc<AgentInner>,
}

impl Drop for IdleGuard {
    fn drop(&mut self) {
        let waiters = {
            let mut guard = self.inner.lock.lock().unwrap_or_else(|e| e.into_inner());
            guard.state.is_streaming = false;
            guard.state.stream_message = None;
            guard.state.pending_tool_calls.clear();
            guard.abort_controller = None;
            std::mem::take(&mut guard.idle_waiters)
        };
        for tx in waiters {
            let _ = tx.send(());
        }
    }
}

fn default_convert_to_llm(messages: Vec<AgentMessage>) -> BoxFuture<Result<Vec<Message>>> {
    Box::pin(async move { Ok(messages.into_iter().filter_map(AgentMessage::into_message).collect()) })
}

fn sync_system_prompt(messages: &[AgentMessage]) -> String {
    let llm_messages: Vec<Message> = messages.iter().filter_map(|m| m.as_message().cloned()).collect();
    get_current_system_prompt(&llm_messages)
}

fn current_system_message(messages: &[AgentMessage]) -> Option<SystemMessage> {
    let llm_messages: Vec<Message> = messages.iter().filter_map(|m| m.as_message().cloned()).collect();
    get_current_system_message(&llm_messages)
}

fn to_tool_declarations(tools: &[AgentTool]) -> Vec<pi_ai::Tool> {
    tools
        .iter()
        .map(AgentTool::to_tool)
        .map(|t| to_tool_declaration(&t))
        .collect()
}

fn default_fallback_model() -> Model {
    compat::get_model("google", "gemini-2.5-flash-lite-preview-06-17").unwrap_or_else(|| Model {
        r#type: None,
        id: "unknown".to_owned(),
        name: "unknown".to_owned(),
        api: "unknown".to_owned(),
        provider: "unknown".to_owned(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: Vec::new(),
        input_limits: None,
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: 0,
        max_tokens: 0,
        headers: None,
        sampling_params: None,
        compat: None,
    })
}
