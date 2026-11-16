//! Port of packages/agent/src/agent-loop.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt;
use futures::future::join_all;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, EventStream, FauxAssistantMessageContent,
    JsonObject, Message, ModelThinkingLevel, SimpleStreamOptions, StopReason, SystemMessage, SystemMessageContent,
    Tool, ToolCall, ToolReference, ToolResultMessage, ToolStateChanges, UserContentBlock, faux_assistant_message,
    faux_text, get_current_tools, get_tool_state_changes, normalize_context, to_tool_declaration,
    validate_tool_arguments,
};
use pi_js::abort::AbortSignal;
use pi_js::time::now_ms;
use pi_js::{BoxFuture, Error, Result};
use serde_json::Value;

use crate::stream_fn::get_default_stream_fn;
use crate::types::{
    AfterToolCallContext, AfterToolCallFn, AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, AgentTool,
    AgentToolCall, AgentToolCallOutcome, AgentToolResult, AgentToolUpdateCallback, BeforeToolCallContext,
    BeforeToolCallFn, FinishTurnAction, FinishTurnContext, PrepareNextTurnContext, PrepareRequestContext, StreamFn,
    ThinkingLevel, ToolExecutionMode,
};

/// Event stream emitted by [`agent_loop`] and [`agent_loop_continue`].
pub type AgentEventStream = EventStream<AgentEvent, Vec<AgentMessage>>;

/// Synchronous event sink passed to [`run_agent_loop`] and [`run_agent_loop_continue`].
pub type AgentEventSink = Arc<dyn Fn(AgentEvent) + Send + Sync>;

/// Asynchronous event sink used internally so [`Agent`] can await `subscribe_async` listeners.
pub(crate) type AgentAsyncEventSink = Arc<dyn Fn(AgentEvent) -> pi_js::BoxFuture<()> + Send + Sync>;

fn sync_to_async_sink(emit: AgentEventSink) -> AgentAsyncEventSink {
    Arc::new(move |event| {
        let emit = Arc::clone(&emit);
        Box::pin(async move {
            emit(event);
        })
    })
}

/// Parameters for [`run_tool_call`].
#[derive(Clone)]
pub struct RunToolCallOptions {
    pub tools: Vec<AgentTool>,
    pub assistant_message: AssistantMessage,
    pub context: AgentContext,
    pub before_tool_call: Option<BeforeToolCallFn>,
    pub after_tool_call: Option<AfterToolCallFn>,
    pub signal: Option<AbortSignal>,
    pub on_update: Option<AgentToolUpdateCallback>,
}

enum PreparedToolCall {
    Immediate {
        tool_call: ToolCall,
        args: Value,
        outcome: AgentToolCallOutcome,
    },
    Execute {
        tool: AgentTool,
        tool_call: ToolCall,
        args: Value,
    },
}

/// Starts an agent loop with one or more new `prompts`, returning a streaming [`AgentEventStream`].
pub fn agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> AgentEventStream {
    let stream = create_agent_stream();
    let stream_task = stream.clone();
    let model = config.model.clone();

    tokio::spawn(async move {
        let stream_for_emit = stream_task.clone();
        let emit: AgentEventSink = Arc::new(move |event| {
            stream_for_emit.push(event);
        });
        match run_agent_loop(prompts, context, config, emit.clone(), signal.clone(), stream_fn).await {
            Ok(messages) => {
                stream_task.end(Some(messages));
            }
            Err(err) => {
                let failure = make_error_assistant_message(&model, err.message(), signal.as_ref());
                let failure_msg = AgentMessage::from(failure.clone());
                emit(AgentEvent::MessageStart {
                    message: failure_msg.clone(),
                });
                emit(AgentEvent::MessageEnd {
                    message: failure_msg.clone(),
                });
                emit(AgentEvent::TurnEnd {
                    message: failure_msg.clone(),
                    tool_results: Vec::new(),
                });
                let new_messages = vec![failure_msg];
                emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                });
                stream_task.end(Some(new_messages));
            }
        }
    });

    stream
}

/// Continues an agent loop from existing `context` without appending new prompts.
///
/// Fails synchronously when `context.messages` is empty or ends with an assistant message.
pub fn agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Result<AgentEventStream> {
    validate_continue_context(&context)?;

    let stream = create_agent_stream();
    let stream_task = stream.clone();
    let model = config.model.clone();

    tokio::spawn(async move {
        let stream_for_emit = stream_task.clone();
        let emit: AgentEventSink = Arc::new(move |event| {
            stream_for_emit.push(event);
        });
        match run_agent_loop_continue(context, config, emit.clone(), signal.clone(), stream_fn).await {
            Ok(messages) => {
                stream_task.end(Some(messages));
            }
            Err(err) => {
                let failure = make_error_assistant_message(&model, err.message(), signal.as_ref());
                let failure_msg = AgentMessage::from(failure.clone());
                emit(AgentEvent::MessageStart {
                    message: failure_msg.clone(),
                });
                emit(AgentEvent::MessageEnd {
                    message: failure_msg.clone(),
                });
                emit(AgentEvent::TurnEnd {
                    message: failure_msg.clone(),
                    tool_results: Vec::new(),
                });
                let new_messages = vec![failure_msg];
                emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                });
                stream_task.end(Some(new_messages));
            }
        }
    });

    Ok(stream)
}

/// Executes an agent loop starting with `prompts` and delivers lifecycle events to `emit`.
pub async fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Result<Vec<AgentMessage>> {
    run_agent_loop_async(prompts, context, config, sync_to_async_sink(emit), signal, stream_fn).await
}

/// Continues an agent loop from existing `context` and delivers lifecycle events to `emit`.
pub async fn run_agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Result<Vec<AgentMessage>> {
    run_agent_loop_continue_async(context, config, sync_to_async_sink(emit), signal, stream_fn).await
}

pub(crate) async fn run_agent_loop_async(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentAsyncEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Result<Vec<AgentMessage>> {
    let initial_messages = declare_tool_changes(&context, prompts);
    let mut new_messages = initial_messages.clone();
    let mut current_context = context;
    current_context.messages.extend(initial_messages.iter().cloned());

    emit(AgentEvent::AgentStart).await;
    emit(AgentEvent::TurnStart).await;
    for prompt in initial_messages {
        emit(AgentEvent::MessageStart {
            message: prompt.clone(),
        })
        .await;
        emit(AgentEvent::MessageEnd { message: prompt }).await;
    }

    run_loop(current_context, &mut new_messages, config, signal, stream_fn, &emit).await?;
    Ok(new_messages)
}

pub(crate) async fn run_agent_loop_continue_async(
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentAsyncEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Result<Vec<AgentMessage>> {
    validate_continue_context(&context)?;

    let mut new_messages = Vec::new();
    emit(AgentEvent::AgentStart).await;
    emit(AgentEvent::TurnStart).await;

    run_loop(context, &mut new_messages, config, signal, stream_fn, &emit).await?;
    Ok(new_messages)
}

/// Executes one tool call through argument preparation, schema validation, `before_tool_call`,
/// tool execution, and `after_tool_call` hooks.
///
/// This does not emit `tool_execution_*` agent events; nested tool runners handle their own events.
pub async fn run_tool_call(tool_call: AgentToolCall, options: RunToolCallOptions) -> AgentToolCallOutcome {
    let raw_call = tool_call.to_tool_call();
    let prepared = prepare_tool_call(
        &options.tools,
        &options.assistant_message,
        &raw_call,
        &options.context,
        options.before_tool_call.as_ref(),
        options.signal.as_ref(),
    )
    .await;

    match prepared {
        PreparedToolCall::Immediate { outcome, .. } => outcome,
        PreparedToolCall::Execute {
            tool,
            tool_call: valid_call,
            args,
        } => {
            if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
                return aborted_outcome(&valid_call);
            }
            let raw_outcome = execute_prepared_tool_call(
                &tool,
                &valid_call,
                args.clone(),
                options.signal.as_ref(),
                options.on_update.clone(),
            )
            .await;
            finalize_tool_call(
                &options.assistant_message,
                &valid_call,
                args,
                &options.context,
                options.after_tool_call.as_ref(),
                options.signal.as_ref(),
                raw_outcome,
            )
            .await
        }
    }
}

fn create_agent_stream() -> AgentEventStream {
    EventStream::new(
        |event| matches!(event, AgentEvent::AgentEnd { .. }),
        |event| match event {
            AgentEvent::AgentEnd { messages } => messages,
            _ => Vec::new(),
        },
    )
}

fn validate_continue_context(context: &AgentContext) -> Result<()> {
    let Some(last) = context.messages.last() else {
        return Err(Error::msg("Cannot continue: no messages in context"));
    };
    if last.role() == Some("assistant") {
        return Err(Error::msg("Cannot continue from message role: assistant"));
    }
    Ok(())
}

async fn run_loop(
    mut current_context: AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    mut config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
    emit: &AgentAsyncEventSink,
) -> Result<()> {
    let mut first_turn = true;
    let mut last_completed_turn: Option<FinishTurnContext> = None;
    let mut explicit_continuation = false;
    let mut pending_messages = if let Some(get_steering) = &config.get_steering_messages {
        get_steering().await?
    } else {
        Vec::new()
    };

    loop {
        let mut has_more_tool_calls = true;

        while has_more_tool_calls || !pending_messages.is_empty() {
            let mut prepared_messages = Vec::new();
            if !first_turn {
                if let Some(last_turn) = last_completed_turn.take() {
                    prepared_messages =
                        apply_next_turn_refresh(&mut current_context, last_turn, &mut config, signal.as_ref()).await?;
                }
                if pending_messages.is_empty() {
                    if let Some(get_steering) = &config.get_steering_messages {
                        pending_messages = get_steering().await?;
                    }
                }
                emit(AgentEvent::TurnStart).await;
            } else {
                first_turn = false;
            }

            let mut to_append = prepared_messages;
            to_append.extend(pending_messages.drain(..));
            for msg in declare_tool_changes(&current_context, to_append) {
                emit(AgentEvent::MessageStart { message: msg.clone() }).await;
                emit(AgentEvent::MessageEnd { message: msg.clone() }).await;
                current_context.messages.push(msg.clone());
                new_messages.push(msg);
            }

            let assistant_message = stream_assistant_response(
                &mut current_context,
                new_messages,
                &mut config,
                signal.as_ref(),
                stream_fn.as_ref(),
                emit,
            )
            .await?;

            if matches!(assistant_message.stop_reason, StopReason::Error | StopReason::Aborted) {
                if let Some(finish_turn) = &config.finish_turn {
                    let _ = finish_turn(
                        FinishTurnContext {
                            message: assistant_message.clone(),
                            tool_results: Vec::new(),
                            context: current_context.clone(),
                        },
                        signal.clone(),
                    )
                    .await?;
                }
                emit(AgentEvent::TurnEnd {
                    message: AgentMessage::from(assistant_message),
                    tool_results: Vec::new(),
                })
                .await;
                emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                })
                .await;
                return Ok(());
            }

            let tool_calls: Vec<ToolCall> = assistant_message
                .content
                .iter()
                .filter_map(|block| match block {
                    AssistantContentBlock::ToolCall(tc) => Some(tc.clone()),
                    _ => None,
                })
                .collect();

            has_more_tool_calls = false;
            let mut tool_results: Vec<ToolResultMessage> = Vec::new();

            if !tool_calls.is_empty() {
                if assistant_message.stop_reason == StopReason::Length {
                    has_more_tool_calls = true;
                    tool_results = fail_tool_calls_from_truncated_message(&tool_calls, emit).await;
                } else {
                    let exec_res = execute_tool_calls(
                        current_context.tools.as_deref().unwrap_or(&[]),
                        &assistant_message,
                        &tool_calls,
                        &current_context,
                        &config,
                        signal.as_ref(),
                        emit,
                    )
                    .await;
                    tool_results = exec_res.tool_results;
                    has_more_tool_calls = !exec_res.terminated;
                }

                for result in &tool_results {
                    let agent_msg = AgentMessage::from(result.clone());
                    current_context.messages.push(agent_msg.clone());
                    new_messages.push(agent_msg);
                }
            }

            last_completed_turn = Some(FinishTurnContext {
                message: assistant_message.clone(),
                tool_results: tool_results.clone(),
                context: current_context.clone(),
            });
            let mut explicit_cont_next = false;
            if let Some(finish_turn) = &config.finish_turn {
                let decision = finish_turn(
                    FinishTurnContext {
                        message: assistant_message.clone(),
                        tool_results: tool_results.clone(),
                        context: current_context.clone(),
                    },
                    signal.clone(),
                )
                .await?;
                emit(AgentEvent::TurnEnd {
                    message: AgentMessage::from(assistant_message),
                    tool_results,
                })
                .await;
                if let Some(d) = decision {
                    match d.action {
                        FinishTurnAction::End => {
                            emit(AgentEvent::AgentEnd {
                                messages: new_messages.clone(),
                            })
                            .await;
                            return Ok(());
                        }
                        FinishTurnAction::Continue => {
                            explicit_cont_next = true;
                        }
                    }
                }
            } else {
                emit(AgentEvent::TurnEnd {
                    message: AgentMessage::from(assistant_message),
                    tool_results,
                })
                .await;
            }

            explicit_continuation = explicit_cont_next;
            if let Some(get_steering) = &config.get_steering_messages {
                pending_messages = get_steering().await?;
            }
            if has_more_tool_calls || !pending_messages.is_empty() {
                explicit_continuation = false;
            }
        }

        if let Some(get_follow_up) = &config.get_follow_up_messages {
            let follow_up = get_follow_up().await?;
            if !follow_up.is_empty() {
                explicit_continuation = false;
                pending_messages = follow_up;
                continue;
            }
        }

        if explicit_continuation {
            explicit_continuation = false;
            continue;
        }

        break;
    }

    emit(AgentEvent::AgentEnd {
        messages: new_messages.clone(),
    })
    .await;
    Ok(())
}

async fn apply_next_turn_refresh(
    current_context: &mut AgentContext,
    config: &mut AgentLoopConfig,
    signal: Option<&AbortSignal>,
) -> Result<Vec<AgentMessage>> {
    let update = if let Some(with_ctx) = &config.prepare_next_turn_with_context {
        with_ctx(
            PrepareNextTurnContext {
                context: current_context.clone(),
            },
            signal.cloned(),
        )
        .await?
    } else if let Some(prepare) = &config.prepare_next_turn {
        prepare(signal.cloned()).await?
    } else {
        None
    };

    let mut prepared_messages = Vec::new();
    if let Some(u) = update {
        if let Some(ctx) = u.context {
            *current_context = ctx;
        }
        if let Some(msgs) = u.messages {
            prepared_messages = msgs;
        }
        if let Some(model) = u.model {
            config.model = model;
        }
        if let Some(level) = u.thinking_level.or(u.reasoning) {
            config.reasoning = Some(level);
        }
    }
    Ok(prepared_messages)
}

async fn stream_assistant_response(
    current_context: &mut AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    config: &mut AgentLoopConfig,
    signal: Option<&AbortSignal>,
    override_stream_fn: Option<&StreamFn>,
    emit: &AgentAsyncEventSink,
) -> Result<AssistantMessage> {
    if let Some(prepare_request) = &config.prepare_request {
        let initial_level = config.reasoning.unwrap_or(ThinkingLevel::Off);
        let prepared = prepare_request(
            PrepareRequestContext {
                context: current_context.clone(),
                model: config.model.clone(),
                thinking_level: initial_level,
            },
            signal.cloned(),
        )
        .await?;
        if let Some(p) = prepared {
            if let Some(ctx) = p.context {
                *current_context = ctx;
            }
            if let Some(model) = p.model {
                config.model = model;
            }
            if let Some(level) = p.thinking_level {
                config.reasoning = Some(level);
            }
        }
    }

    let request_context = current_context.clone();
    let request_model = config.model.clone();
    let request_thinking_level = config.reasoning.unwrap_or(ThinkingLevel::Off);

    let messages = if let Some(transform) = &config.transform_context {
        transform(request_context.messages, signal.cloned()).await?
    } else {
        request_context.messages
    };

    let llm_messages = (config.convert_to_llm)(messages).await?;
    let transcript = normalize_context(&Context {
        system_prompt: None,
        messages: llm_messages,
        tools: None,
    });
    let llm_context = Context {
        system_prompt: None,
        messages: transcript.messages,
        tools: None,
    };

    let api_key = if let Some(get_api_key) = &config.get_api_key {
        let dynamic = get_api_key(request_model.provider.clone()).await?;
        dynamic.or_else(|| config.api_key.clone())
    } else {
        config.api_key.clone()
    };

    let stream_options = SimpleStreamOptions {
        temperature: config.temperature,
        max_tokens: config.max_tokens,
        api_key,
        cache_retention: config.cache_retention,
        reasoning: request_thinking_level.to_ai_thinking_level(),
        session_id: config.session_id.clone(),
        transport: config.transport,
        thinking_budgets: config.thinking_budgets.clone(),
        max_retry_delay_ms: config.max_retry_delay_ms,
        metadata: config.metadata.clone(),
        headers: config.headers.clone(),
        on_payload: config.on_payload.clone(),
        signal: signal.cloned(),
    };

    let mut response_stream = if let Some(f) = override_stream_fn {
        f(request_model.clone(), llm_context, Some(stream_options)).await?
    } else if let Some(f) = &config.stream_fn {
        f(request_model.clone(), llm_context, Some(stream_options)).await?
    } else if let Some(f) = get_default_stream_fn() {
        f(request_model.clone(), llm_context, Some(stream_options)).await?
    } else {
        pi_ai::compat::stream_simple(&request_model, &llm_context, Some(stream_options))
    };

    let mut partial_message: Option<AssistantMessage> = None;
    let mut added_partial = false;

    while let Some(event) = response_stream.next().await {
        match event {
            AssistantMessageEvent::Start { ref partial } => {
                partial_message = Some(partial.clone());
                let msg = AgentMessage::from(partial.clone());
                if !added_partial {
                    current_context.messages.push(msg.clone());
                    added_partial = true;
                } else if let Some(last) = current_context.messages.last_mut() {
                    *last = msg.clone();
                }
                emit(AgentEvent::MessageStart { message: msg }).await;
            }
            AssistantMessageEvent::TextStart { ref partial, .. }
            | AssistantMessageEvent::TextDelta { ref partial, .. }
            | AssistantMessageEvent::TextEnd { ref partial, .. }
            | AssistantMessageEvent::ThinkingStart { ref partial, .. }
            | AssistantMessageEvent::ThinkingDelta { ref partial, .. }
            | AssistantMessageEvent::ThinkingEnd { ref partial, .. }
            | AssistantMessageEvent::ToolCallStart { ref partial, .. }
            | AssistantMessageEvent::ToolCallDelta { ref partial, .. }
            | AssistantMessageEvent::ToolCallEnd { ref partial, .. } => {
                if partial_message.is_some() {
                    partial_message = Some(partial.clone());
                    let msg = AgentMessage::from(partial.clone());
                    if added_partial && let Some(last) = current_context.messages.last_mut() {
                        *last = msg.clone();
                    }
                    emit(AgentEvent::MessageUpdate {
                        message: msg,
                        assistant_message_event: event,
                    })
                    .await;
                }
            }
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => {
                let mut final_message = response_stream.result().await;
                final_message.thinking_level = Some(request_thinking_level.to_model_thinking_level());
                let final_agent_msg = AgentMessage::from(final_message.clone());
                if added_partial {
                    if let Some(last) = current_context.messages.last_mut() {
                        *last = final_agent_msg.clone();
                    }
                } else {
                    current_context.messages.push(final_agent_msg.clone());
                    emit(AgentEvent::MessageStart {
                        message: final_agent_msg.clone(),
                    })
                    .await;
                }
                new_messages.push(final_agent_msg.clone());
                emit(AgentEvent::MessageEnd {
                    message: final_agent_msg,
                })
                .await;
                return Ok(final_message);
            }
        }
    }

    let mut final_message = response_stream.result().await;
    final_message.thinking_level = Some(request_thinking_level.to_model_thinking_level());
    let final_agent_msg = AgentMessage::from(final_message.clone());
    if added_partial {
        if let Some(last) = current_context.messages.last_mut() {
            *last = final_agent_msg.clone();
        }
    } else {
        current_context.messages.push(final_agent_msg.clone());
        emit(AgentEvent::MessageStart {
            message: final_agent_msg.clone(),
        })
        .await;
    }
    new_messages.push(final_agent_msg.clone());
    emit(AgentEvent::MessageEnd {
        message: final_agent_msg,
    })
    .await;
    Ok(final_message)
}

fn with_tool_changes(mut sys: SystemMessage, changes: ToolStateChanges) -> SystemMessage {
    sys.tools_added = (!changes.tools_added.is_empty()).then_some(changes.tools_added);
    sys.tools_removed = (!changes.tools_removed.is_empty()).then_some(changes.tools_removed);
    sys
}

fn declare_tool_changes(context: &AgentContext, pending_messages: Vec<AgentMessage>) -> Vec<AgentMessage> {
    let system_index = pending_messages.iter().rposition(|msg| msg.role() == Some("system"));
    let pending_sys = system_index.and_then(|idx| match &pending_messages[idx] {
        AgentMessage::Message(Message::System(sys)) => Some(sys.clone()),
        _ => None,
    });

    let no_changes = ToolStateChanges {
        tools_added: Vec::new(),
        tools_removed: Vec::new(),
    };

    let baseline: Vec<AgentMessage> = if let (Some(idx), Some(pending)) = (system_index, pending_sys.as_ref()) {
        pending_messages
            .iter()
            .enumerate()
            .map(|(i, msg)| {
                if i == idx {
                    AgentMessage::from(with_tool_changes(pending.clone(), no_changes.clone()))
                } else {
                    msg.clone()
                }
            })
            .collect()
    } else {
        pending_messages.clone()
    };

    let combined_llm_messages: Vec<Message> = context
        .messages
        .iter()
        .chain(baseline.iter())
        .filter_map(|msg| msg.as_message().cloned())
        .collect();
    let declared_tools: Vec<Tool> = context
        .tools
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(to_tool_declaration)
        .collect();
    let current_tools = get_current_tools(&combined_llm_messages);
    let changes = get_tool_state_changes(&current_tools, &declared_tools);
    let unchanged = changes.tools_added.is_empty() && changes.tools_removed.is_empty();

    if let (Some(idx), Some(pending)) = (system_index, pending_sys.as_ref()) {
        let has_existing = pending.tools_added.as_ref().is_some_and(|v| !v.is_empty())
            || pending.tools_removed.as_ref().is_some_and(|v| !v.is_empty());
        if unchanged && !has_existing {
            return pending_messages;
        }
        return baseline
            .into_iter()
            .enumerate()
            .map(|(i, msg)| {
                if i == idx {
                    AgentMessage::from(with_tool_changes(pending.clone(), changes.clone()))
                } else {
                    msg
                }
            })
            .collect();
    }

    if unchanged {
        return pending_messages;
    }

    let update = AgentMessage::from(with_tool_changes(
        SystemMessage {
            content: SystemMessageContent::Text(String::new()),
            sections: None,
            tools_added: None,
            tools_removed: None,
            timestamp: now_ms(),
        },
        changes,
    ));
    let insert_idx = pending_messages
        .iter()
        .position(|msg| msg.role() != Some("system"))
        .unwrap_or(pending_messages.len());
    let mut out = Vec::with_capacity(pending_messages.len() + 1);
    out.extend(pending_messages[..insert_idx].iter().cloned());
    out.push(update);
    out.extend(pending_messages[insert_idx..].iter().cloned());
    out
}

struct ToolBatchOutput {
    tool_results: Vec<ToolResultMessage>,
    terminated: bool,
}

async fn execute_tool_calls(
    tools: &[AgentTool],
    assistant_message: &AssistantMessage,
    tool_calls: &[ToolCall],
    current_context: &AgentContext,
    config: &AgentLoopConfig,
    signal: Option<&AbortSignal>,
    emit: &AgentAsyncEventSink,
) -> ToolBatchOutput {
    let sequential_mode = config.tool_execution == Some(ToolExecutionMode::Sequential)
        || tool_calls.iter().any(|tc| {
            tools.iter().find(|t| t.name == tc.name).and_then(|t| t.execution_mode)
                == Some(ToolExecutionMode::Sequential)
        });

    if sequential_mode {
        execute_tool_calls_sequential(
            tools,
            assistant_message,
            tool_calls,
            current_context,
            config,
            signal,
            emit,
        )
        .await
    } else {
        execute_tool_calls_parallel(
            tools,
            assistant_message,
            tool_calls,
            current_context,
            config,
            signal,
            emit,
        )
        .await
    }
}

async fn execute_tool_calls_sequential(
    tools: &[AgentTool],
    assistant_message: &AssistantMessage,
    tool_calls: &[ToolCall],
    current_context: &AgentContext,
    config: &AgentLoopConfig,
    signal: Option<&AbortSignal>,
    emit: &AgentAsyncEventSink,
) -> ToolBatchOutput {
    let mut tool_results = Vec::with_capacity(tool_calls.len());
    let mut finalized_count = 0usize;
    let mut all_terminated = true;

    for tc in tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tc.id.clone(),
            tool_name: tc.name.clone(),
            args: json_object_to_value(&tc.arguments),
        })
        .await;

        let prepared = prepare_tool_call(
            tools,
            assistant_message,
            tc,
            current_context,
            config.before_tool_call.as_ref(),
            signal,
        )
        .await;

        let outcome = match prepared {
            PreparedToolCall::Immediate { outcome, .. } => outcome,
            PreparedToolCall::Execute {
                tool,
                tool_call: valid_call,
                args,
            } => {
                let (pump, on_update) = AsyncUpdatePump::start(&valid_call, args.clone(), Arc::clone(emit));
                let raw = execute_prepared_tool_call(&tool, &valid_call, args.clone(), signal, Some(on_update)).await;
                pump.finish().await;
                finalize_tool_call(
                    assistant_message,
                    &valid_call,
                    args,
                    current_context,
                    config.after_tool_call.as_ref(),
                    signal,
                    raw,
                )
                .await
            }
        };

        finalized_count += 1;
        all_terminated &= outcome_requests_termination(&outcome.result);

        let tr = make_tool_result_message(&outcome);
        emit(AgentEvent::ToolExecutionEnd {
            tool_call_id: tc.id.clone(),
            tool_name: tc.name.clone(),
            result: outcome.result,
            is_error: outcome.is_error,
        })
        .await;
        emit(AgentEvent::MessageStart {
            message: AgentMessage::from(tr.clone()),
        })
        .await;
        emit(AgentEvent::MessageEnd {
            message: AgentMessage::from(tr.clone()),
        })
        .await;
        tool_results.push(tr);
    }

    ToolBatchOutput {
        tool_results,
        terminated: finalized_count > 0 && all_terminated,
    }
}

async fn execute_tool_calls_parallel(
    tools: &[AgentTool],
    assistant_message: &AssistantMessage,
    tool_calls: &[ToolCall],
    current_context: &AgentContext,
    config: &AgentLoopConfig,
    signal: Option<&AbortSignal>,
    emit: &AgentAsyncEventSink,
) -> ToolBatchOutput {
    // Phase 1: emit `tool_execution_start` and prepare every call sequentially in source order (#9986).
    let mut prepared_calls = Vec::with_capacity(tool_calls.len());
    for tc in tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tc.id.clone(),
            tool_name: tc.name.clone(),
            args: json_object_to_value(&tc.arguments),
        })
        .await;
        let prepared = prepare_tool_call(
            tools,
            assistant_message,
            tc,
            current_context,
            config.before_tool_call.as_ref(),
            signal,
        )
        .await;
        prepared_calls.push(prepared);
    }

    // Phase 2: execute prepared calls concurrently; emit `tool_execution_end` in completion order.
    let futures: Vec<BoxFuture<(usize, ToolResultMessage, bool)>> = prepared_calls
        .into_iter()
        .enumerate()
        .map(|(idx, prepared)| {
            let assistant_message = assistant_message.clone();
            let current_context = current_context.clone();
            let after_hook = config.after_tool_call.clone();
            let signal = signal.cloned();
            let emit = emit.clone();
            Box::pin(async move {
                let outcome = match prepared {
                    PreparedToolCall::Immediate { outcome, .. } => outcome,
                    PreparedToolCall::Execute {
                        tool,
                        tool_call: valid_call,
                        args,
                    } => {
                        if signal.as_ref().is_some_and(AbortSignal::aborted) {
                            aborted_outcome(&valid_call)
                        } else {
                            let (pump, on_update) = AsyncUpdatePump::start(&valid_call, args.clone(), emit.clone());
                            let raw = execute_prepared_tool_call(
                                &tool,
                                &valid_call,
                                args.clone(),
                                signal.as_ref(),
                                Some(on_update),
                            )
                            .await;
                            pump.finish().await;
                            finalize_tool_call(
                                &assistant_message,
                                &valid_call,
                                args,
                                &current_context,
                                after_hook.as_ref(),
                                signal.as_ref(),
                                raw,
                            )
                            .await
                        }
                    }
                };
                let term = outcome_requests_termination(&outcome.result);
                let tr = make_tool_result_message(&outcome);
                emit(AgentEvent::ToolExecutionEnd {
                    tool_call_id: outcome.tool_call.id,
                    tool_name: outcome.tool_call.name,
                    result: outcome.result,
                    is_error: outcome.is_error,
                })
                .await;
                (idx, tr, term)
            }) as BoxFuture<(usize, ToolResultMessage, bool)>
        })
        .collect();

    let mut settled = join_all(futures).await;
    settled.sort_by_key(|(idx, _, _)| *idx);

    let mut finalized_count = 0usize;
    let mut terminated = true;
    let mut tool_results = Vec::with_capacity(settled.len());
    for (_, tr, term) in settled {
        finalized_count += 1;
        terminated &= term;
        emit(AgentEvent::MessageStart {
            message: AgentMessage::from(tr.clone()),
        })
        .await;
        emit(AgentEvent::MessageEnd {
            message: AgentMessage::from(tr.clone()),
        })
        .await;
        tool_results.push(tr);
    }

    ToolBatchOutput {
        tool_results,
        terminated: finalized_count > 0 && terminated,
    }
}

async fn prepare_tool_call(
    tools: &[AgentTool],
    assistant_message: &AssistantMessage,
    tc: &ToolCall,
    current_context: &AgentContext,
    before_tool_call: Option<&BeforeToolCallFn>,
    signal: Option<&AbortSignal>,
) -> PreparedToolCall {
    let raw_args = json_object_to_value(&tc.arguments);
    if signal.is_some_and(AbortSignal::aborted) {
        return PreparedToolCall::Immediate {
            tool_call: tc.clone(),
            args: raw_args,
            outcome: aborted_outcome(tc),
        };
    }

    let Some(tool) = tools.iter().find(|t| t.name == tc.name).cloned() else {
        return PreparedToolCall::Immediate {
            tool_call: tc.clone(),
            args: raw_args,
            outcome: error_outcome(tc, format!("Tool {} not found", tc.name)),
        };
    };

    let prepared_args = if let Some(prepare) = &tool.prepare_arguments {
        prepare(raw_args)
    } else {
        raw_args
    };

    let mut validation_call = tc.clone();
    if let Value::Object(map) = &prepared_args {
        validation_call.arguments = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }

    let validated_args = match validate_tool_arguments(&tool.to_tool(), &validation_call) {
        Ok(args) => args,
        Err(err) => {
            return PreparedToolCall::Immediate {
                tool_call: validation_call,
                args: prepared_args,
                outcome: error_outcome(tc, err.message()),
            };
        }
    };

    if let Some(before) = before_tool_call {
        let hook_res = before(
            BeforeToolCallContext {
                assistant_message: assistant_message.clone(),
                tool_call: validation_call.clone(),
                args: validated_args.clone(),
                context: current_context.clone(),
            },
            signal.cloned(),
        )
        .await;
        match hook_res {
            Ok(_) if signal.is_some_and(AbortSignal::aborted) => {
                return PreparedToolCall::Immediate {
                    tool_call: validation_call,
                    args: validated_args,
                    outcome: aborted_outcome(tc),
                };
            }
            Ok(Some(res)) if res.block == Some(true) => {
                let reason = res.reason.unwrap_or_else(|| "Tool execution was blocked".to_string());
                let mut outcome = error_outcome(tc, reason);
                if res.terminate == Some(true) {
                    outcome.result.terminate = Some(true);
                }
                return PreparedToolCall::Immediate {
                    tool_call: validation_call,
                    args: validated_args,
                    outcome,
                };
            }
            Ok(Some(res)) => {
                if let Some(updated) = res.args {
                    validated_args = updated;
                }
            }
            Err(err) => {
                return PreparedToolCall::Immediate {
                    tool_call: validation_call,
                    args: validated_args,
                    outcome: error_outcome(tc, err.message()),
                };
            }
            Ok(None) => {}
        }
    }

    PreparedToolCall::Execute {
        tool,
        tool_call: validation_call,
        args: validated_args,
    }
}

async fn execute_prepared_tool_call(
    tool: &AgentTool,
    tc: &ToolCall,
    args: Value,
    signal: Option<&AbortSignal>,
    on_update: Option<AgentToolUpdateCallback>,
) -> AgentToolCallOutcome {
    let accepting_updates = Arc::new(AtomicBool::new(true));
    let gated_update: Option<AgentToolUpdateCallback> = on_update.map(|cb| {
        let gate = Arc::clone(&accepting_updates);
        Arc::new(move |partial_result| {
            if gate.load(Ordering::SeqCst) {
                cb(partial_result);
            }
        }) as AgentToolUpdateCallback
    });

    let exec_result = (tool.execute)(tc.id.clone(), args, signal.cloned(), gated_update).await;
    accepting_updates.store(false, Ordering::SeqCst);

    match exec_result {
        Ok(result) => {
            let is_error = result.is_error == Some(true);
            AgentToolCallOutcome {
                tool_call: AgentToolCall::from(tc),
                result,
                is_error,
            }
        }
        Err(err) => AgentToolCallOutcome {
            tool_call: AgentToolCall::from(tc),
            result: AgentToolResult::text(err.message()),
            is_error: true,
        },
    }
}

async fn finalize_tool_call(
    assistant_message: &AssistantMessage,
    tc: &ToolCall,
    args: Value,
    current_context: &AgentContext,
    after_tool_call: Option<&AfterToolCallFn>,
    signal: Option<&AbortSignal>,
    mut outcome: AgentToolCallOutcome,
) -> AgentToolCallOutcome {
    if let Some(after) = after_tool_call {
        let res = after(
            AfterToolCallContext {
                assistant_message: assistant_message.clone(),
                tool_call: tc.clone(),
                args,
                result: outcome.result.clone(),
                is_error: outcome.is_error,
                context: current_context.clone(),
            },
            signal.cloned(),
        )
        .await;
        match res {
            Ok(Some(overrides)) => {
                let replaced_content = overrides.content.is_some();
                if let Some(content) = overrides.content {
                    outcome.result.content = content;
                }
                if let Some(details) = overrides.details {
                    outcome.result.details = Some(details);
                }
                if let Some(structured_content) = overrides.structured_content {
                    outcome.result.structured_content = Some(structured_content);
                } else if replaced_content {
                    outcome.result.structured_content = None;
                }
                if let Some(usage) = overrides.usage {
                    outcome.result.usage = Some(usage);
                }
                if let Some(terminate) = overrides.terminate {
                    outcome.result.terminate = Some(terminate);
                }
                if let Some(is_error) = overrides.is_error {
                    outcome.is_error = is_error;
                }
            }
            Err(err) => {
                outcome.result = AgentToolResult::text(err.message());
                outcome.is_error = true;
            }
            Ok(None) => {}
        }
    }
    outcome
}

fn outcome_requests_termination(result: &AgentToolResult) -> bool {
    result.terminate == Some(true) || result.control.as_ref().and_then(|c| c.terminate) == Some(true)
}

async fn fail_tool_calls_from_truncated_message(
    tool_calls: &[ToolCall],
    emit: &AgentAsyncEventSink,
) -> Vec<ToolResultMessage> {
    let mut results = Vec::with_capacity(tool_calls.len());
    for tc in tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tc.id.clone(),
            tool_name: tc.name.clone(),
            args: json_object_to_value(&tc.arguments),
        })
        .await;
        let outcome = error_outcome(
            tc,
            format!(
                "Tool call \"{}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.",
                tc.name
            ),
        );
        let tr = make_tool_result_message(&outcome);
        emit(AgentEvent::ToolExecutionEnd {
            tool_call_id: tc.id.clone(),
            tool_name: tc.name.clone(),
            result: outcome.result,
            is_error: true,
        })
        .await;
        emit(AgentEvent::MessageStart {
            message: AgentMessage::from(tr.clone()),
        })
        .await;
        emit(AgentEvent::MessageEnd {
            message: AgentMessage::from(tr.clone()),
        })
        .await;
        results.push(tr);
    }
    results
}

struct AsyncUpdatePump {
    sender: Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<AgentToolResult>>>>,
    task: tokio::task::JoinHandle<()>,
}

impl AsyncUpdatePump {
    fn start(tc: &ToolCall, args: Value, emit: AgentAsyncEventSink) -> (Self, AgentToolUpdateCallback) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentToolResult>();
        let sender = Arc::new(std::sync::Mutex::new(Some(tx)));
        let sender_cb = Arc::clone(&sender);
        let tool_call_id = tc.id.clone();
        let tool_name = tc.name.clone();
        let task = tokio::spawn(async move {
            while let Some(partial_result) = rx.recv().await {
                emit(AgentEvent::ToolExecutionUpdate {
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    args: args.clone(),
                    partial_result,
                })
                .await;
            }
        });
        let cb: AgentToolUpdateCallback = Arc::new(move |partial| {
            let guard = sender_cb.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(tx) = guard.as_ref() {
                let _ = tx.send(partial);
            }
        });
        (Self { sender, task }, cb)
    }

    async fn finish(self) {
        self.sender.lock().unwrap_or_else(|e| e.into_inner()).take();
        let _ = self.task.await;
    }
}

fn aborted_outcome(tc: &ToolCall) -> AgentToolCallOutcome {
    error_outcome(tc, "Operation aborted")
}

fn error_outcome(tc: &ToolCall, message: impl Into<String>) -> AgentToolCallOutcome {
    AgentToolCallOutcome {
        tool_call: AgentToolCall::from(tc),
        result: AgentToolResult::text(message),
        is_error: true,
    }
}

fn make_tool_result_message(outcome: &AgentToolCallOutcome) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: outcome.tool_call.id.clone(),
        tool_name: outcome.tool_call.name.clone(),
        content: outcome.result.content.clone(),
        details: outcome.result.details.clone(),
        is_error: outcome.is_error,
        usage: outcome.result.usage.clone(),
        timestamp: now_ms(),
    }
}

fn make_error_assistant_message(
    model: &pi_ai::Model,
    error_message: String,
    signal: Option<&AbortSignal>,
) -> AssistantMessage {
    let mut msg = faux_assistant_message(FauxAssistantMessageContent::Blocks(Vec::new()), None);
    msg.content = vec![AssistantContentBlock::Text(faux_text(""))];
    msg.api = model.api.clone();
    msg.provider = model.provider.clone();
    msg.model = model.id.clone();
    msg.stop_reason = if signal.is_some_and(AbortSignal::aborted) {
        StopReason::Aborted
    } else {
        StopReason::Error
    };
    msg.error_message = Some(error_message);
    msg.timestamp = now_ms();
    msg
}

fn json_object_to_value(obj: &JsonObject) -> Value {
    let map: serde_json::Map<String, Value> = obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    Value::Object(map)
}
