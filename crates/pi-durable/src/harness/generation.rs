//! Port of packages/durable/src/harness/generation.ts

use std::sync::{Arc, LazyLock, Mutex};

use futures::StreamExt;
use indexmap::{IndexMap, IndexSet};
use pi_ai::models::{ModelsDeferredCancelOptions, ModelsDeferredFetchOptions, ModelsSimpleStreamOptions};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, CacheRetention, Context as PiAiContext,
    DeferredCancelOptions, DeferredFetchOptions, DeferredHandle, DeferredOption, DeferredWindow, Message, Model,
    ModelThinkingLevel, SimpleStreamOptions, StopReason, StreamOptions, ThinkingLevel, ToolCall, UserMessage,
};
use pi_ai::utils::overflow::is_context_overflow;
use pi_ai::utils::retry::{RetryPolicy as PiAiRetryPolicy, is_retryable_assistant_error, retry_delay_ms};
use pi_ai::utils::transcript::get_current_tools;
use pi_chord::Context;
use pi_chord::json::copy_json;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::entries::{ASSISTANT_ENTRY, RESET_ENTRY, SYSTEM_ENTRY, TypedEntryDraft, USER_ENTRY};
use crate::env::ExecutionEnv;
use crate::harness::agent::add_tools;
use crate::harness::compaction::{CompactionInput, create_compaction, estimate_context, select_cut};
use crate::harness::inbox::{BoundaryAt, QueueModes, apply_boundary, prepare_boundary};
use crate::harness::json::{JsonKey, assign_json};
use crate::harness::live::{
    DurableRetryState, LIVE_DOC, LiveDeferred, LiveGeneration, LiveRun, LiveState, ToolSlot, ToolSlotStatus, end_run,
};
use crate::harness::prompt::{plan_system_entries, render_sections, replay_sections};
use crate::harness::tool::{TOOL_TASK, ToolTaskInput, ToolTaskResult, append_tool_result, harness_error};
use crate::harness::types::{
    CompactionPolicy, CompactionReason, CompactionResult, ContextView, ConversationStreamOptions, DocumentReader,
    GenerationHooks, ModelRef, PromptInput, StreamDeferredOption, ToolControl, ToolExecutionMode, UserInput,
};
use crate::harness::usage::{UsageBucket, record_usage_tx};
use crate::session::transaction::{Transaction, Tx};
use crate::tasks::{Task, TaskDefinition, TaskRuntimeHandle, define_task};
use crate::types::{
    ConversationId, EntryDraftHead, EntryId, EntryQuery, JoinPolicy, JsonValue, NextTaskState, RunningTask, SelfHead,
    SubmissionId, SubmissionSettlement, TaskId, TaskOptions, TaskOutcome, TaskOutcomeError, TaskOwnership, TypedEntry,
};

const PARTIAL_THROTTLE_MS: u64 = 100;
const DEFAULT_POLL_AFTER_MS: i64 = 5000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationInput {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum GenerationCheckpoint {
    #[serde(rename_all = "camelCase")]
    Prepare {
        attempt: u32,
        /// The blocking compaction this generation waited for; it starts no other compaction (spec §8.3).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compacted: Option<TaskId<CompactionResult>>,
        /// Error text of the overflow that started `compacted`; checked once when `prepare` resumes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        overflow: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Request {
        attempt: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compacted: Option<TaskId<CompactionResult>>,
        model: ModelRef,
        thinking_level: ModelThinkingLevel,
        /// The settings' request options when preparation committed; a resend after recovery uses them unchanged.
        stream_options: ConversationStreamOptions,
        /// Newest entry included in the request.
        cutoff: EntryId,
    },
    #[serde(rename_all = "camelCase")]
    Retry {
        attempt: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compacted: Option<TaskId<CompactionResult>>,
        until: i64,
    },
    #[serde(rename_all = "camelCase")]
    Poll {
        attempt: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compacted: Option<TaskId<CompactionResult>>,
        model: ModelRef,
        cutoff: EntryId,
        handle: DeferredHandle,
        poll_at: i64,
    },
    /// Waiting on the round's tool tasks, which the generation owns (spec §8.5).
    #[serde(rename_all = "camelCase")]
    Tools {
        assistant: EntryId,
        tools: Vec<TaskId<ToolTaskResult>>,
        pending: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationResult {
    pub entry_id: EntryId,
}

type Runtime = TaskRuntimeHandle<GenerationInput, GenerationCheckpoint, GenerationResult, GenerationHooks>;

/// What classification needs from the request that produced a message.
struct RequestContext {
    attempt: u32,
    compacted: Option<TaskId<CompactionResult>>,
    model: ModelRef,
    cutoff: EntryId,
    messages: Option<Vec<Message>>,
    poll_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThresholdDecision {
    Blocking,
    Background,
}

/// Shared conversion from [`ConversationStreamOptions`] + [`ModelThinkingLevel`] to pi-ai [`SimpleStreamOptions`].
pub(crate) fn to_simple_stream_options(
    stream_options: &ConversationStreamOptions,
    thinking_level: ModelThinkingLevel,
    signal: pi_js::abort::AbortSignal,
    max_tokens: Option<i64>,
    cache_retention_override: Option<CacheRetention>,
    include_deferred: bool,
) -> SimpleStreamOptions {
    let base = StreamOptions {
        signal: Some(signal),
        transport: stream_options.transport,
        timeout_ms: stream_options.timeout_ms,
        max_retries: stream_options.max_retries,
        max_retry_delay_ms: stream_options.max_retry_delay_ms,
        headers: stream_options.headers.clone(),
        metadata: stream_options.metadata.clone(),
        cache_retention: cache_retention_override.or(stream_options.cache_retention),
        max_tokens,
        ..Default::default()
    };
    let reasoning = match thinking_level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(ThinkingLevel::High),
        ModelThinkingLevel::XHigh => Some(ThinkingLevel::XHigh),
        ModelThinkingLevel::Max => Some(ThinkingLevel::Max),
    };
    let deferred = if include_deferred {
        stream_options.deferred.map(|d| match d {
            StreamDeferredOption::Bool(b) => DeferredOption::Bool(b),
            StreamDeferredOption::Window { window } => DeferredOption::Window {
                window: window.map(|w| match w {
                    crate::harness::types::DeferredWindow::M15 => DeferredWindow::M15,
                    crate::harness::types::DeferredWindow::H1 => DeferredWindow::H1,
                    crate::harness::types::DeferredWindow::H24 => DeferredWindow::H24,
                }),
            },
        })
    } else {
        None
    };
    SimpleStreamOptions {
        base,
        tool_choice: None,
        reasoning,
        deferred,
        thinking_budgets: None,
    }
}

/// Built-in generation task: prepares the positional system prompt and tool loadout, requests or polls the model,
/// retries, and classifies the response. The run's inputs live in `pi.live.run`.
pub static GENERATION_TASK: LazyLock<Task<GenerationInput, GenerationCheckpoint, GenerationResult, GenerationHooks>> =
    LazyLock::new(|| {
        let mut phases = IndexMap::new();
        phases.insert(
            "prepare".to_owned(),
            Arc::new(
                |task: RunningTask<GenerationInput, GenerationCheckpoint, GenerationResult>,
                 runtime: Runtime,
                 context: Context| {
                    Box::pin(async move { phase_prepare(task, runtime, context).await })
                },
            )
                as crate::tasks::TypedPhaseHandler<
                    GenerationInput,
                    GenerationCheckpoint,
                    GenerationResult,
                    GenerationHooks,
                >,
        );
        phases.insert(
            "request".to_owned(),
            Arc::new(
                |task: RunningTask<GenerationInput, GenerationCheckpoint, GenerationResult>,
                 runtime: Runtime,
                 context: Context| {
                    Box::pin(async move { phase_request(task, runtime, context).await })
                },
            ),
        );
        phases.insert(
            "retry".to_owned(),
            Arc::new(
                |task: RunningTask<GenerationInput, GenerationCheckpoint, GenerationResult>,
                 runtime: Runtime,
                 context: Context| {
                    Box::pin(async move { phase_retry(task, runtime, context).await })
                },
            ),
        );
        phases.insert(
            "poll".to_owned(),
            Arc::new(
                |task: RunningTask<GenerationInput, GenerationCheckpoint, GenerationResult>,
                 runtime: Runtime,
                 context: Context| {
                    Box::pin(async move { phase_poll(task, runtime, context).await })
                },
            ),
        );
        phases.insert(
            "tools".to_owned(),
            Arc::new(
                |task: RunningTask<GenerationInput, GenerationCheckpoint, GenerationResult>,
                 runtime: Runtime,
                 context: Context| {
                    Box::pin(async move { phase_tools(task, runtime, context).await })
                },
            ),
        );

        define_task(TaskDefinition {
            name: "pi.generation".to_owned(),
            version: 1,
            initial: Arc::new(|_input| {
                Ok(GenerationCheckpoint::Prepare {
                    attempt: 1,
                    compacted: None,
                    overflow: None,
                })
            }),
            phases,
            abort: Arc::new(|task, runtime, context| {
                Box::pin(async move { phase_abort(task, runtime, context).await })
            }),
            migrate: None,
            _hooks: std::marker::PhantomData,
        })
    });

async fn phase_prepare(
    task: RunningTask<GenerationInput, GenerationCheckpoint, GenerationResult>,
    runtime: Runtime,
    context: Context,
) -> pi_js::Result<()> {
    let GenerationCheckpoint::Prepare {
        attempt,
        compacted,
        overflow,
    } = task.checkpoint
    else {
        return Err(pi_js::Error::msg("Invalid generation checkpoint in prepare phase"));
    };

    let conversation_id = runtime.conversation_id()?;
    let agent = runtime.agent(context.clone()).await?;
    let settings = runtime.settings()?;
    let models = runtime.models()?;
    let resolved = agent
        .model
        .as_ref()
        .and_then(|r| models.get_model(&r.provider, &r.model_id));
    let (Some(model_ref), Some(resolved)) = (agent.model.clone(), resolved) else {
        return fail_no_model(&runtime, agent.model.as_ref(), context).await;
    };

    if let (Some(comp_id), Some(overflow_msg)) = (compacted, overflow) {
        let outcomes = runtime.outcomes(&[comp_id], context.clone()).await?;
        let completed_with_entry = matches!(
            outcomes.first(),
            Some(TaskOutcome::Completed { result }) if result.entry_id.is_some()
        );
        if !completed_with_entry {
            return fail_model_error(&runtime, overflow_msg, context).await;
        }
    }

    let view = runtime.context(conversation_id, context.clone(), None).await?;
    let shown = replay_sections(&view.messages);
    let env: Option<Arc<dyn ExecutionEnv>> = match runtime.env(context.clone()).await {
        Ok(env) => env,
        Err(err) => {
            if context.abort_signal().is_some_and(|s| s.aborted()) {
                return Err(err);
            }
            runtime.report(err);
            None
        }
    };

    let read: DocumentReader = runtime.read()?;
    let prompt_input = PromptInput {
        conversation_id,
        agent: agent.clone(),
        env,
        shown: shown.clone(),
        read,
    };
    let rt_for_report = runtime.clone();
    let report = move |err: pi_js::Error| rt_for_report.report(err);
    let desired = render_sections(&agent.sections, &prompt_input, &shown, &report, context.clone()).await?;
    let pi_tools: Vec<_> = agent.tools.iter().map(|t| t.as_pi_tool()).collect();
    let entries = plan_system_entries(&view, &desired, &pi_tools, runtime.now()?);

    let threshold = if compacted.is_none() {
        threshold_compaction(&view, &entries, resolved.context_window, &settings.compaction)
    } else {
        None
    };

    if threshold == Some(ThresholdDecision::Blocking) {
        let gen_task_id = runtime.task_id()?.erased();
        runtime
            .commit(
                move |tx, _current| {
                    Box::pin(async move {
                        let child = create_compaction(
                            &tx,
                            conversation_id,
                            CompactionInput {
                                reason: CompactionReason::Threshold,
                                instructions: None,
                            },
                            Some(gen_task_id),
                        )
                        .await?;
                        Ok(Some(NextTaskState::Waiting {
                            checkpoint: GenerationCheckpoint::Prepare {
                                attempt,
                                compacted: Some(child),
                                overflow: None,
                            },
                            on: vec![child.erased()],
                            policy: JoinPolicy::AllSettled,
                        }))
                    })
                },
                context,
            )
            .await?;
        return Ok(());
    }

    let stream_options = settings.stream;
    let thinking_level = agent.thinking_level;
    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let latest = tx
                        .scan_entries(
                            EntryQuery {
                                conversation_id,
                                min_entry_id: None,
                                max_entry_id: None,
                            },
                            1,
                            None,
                        )
                        .await?;
                    let mut cutoff = latest.items.first().map(|e| e.id);
                    for entry in entries {
                        let appended = tx.append_typed_entry(&SYSTEM_ENTRY, conversation_id, entry).await?;
                        cutoff = Some(appended.record.id);
                    }
                    let Some(cutoff) = cutoff else {
                        return Err(pi_js::Error::msg(format!(
                            "Conversation {conversation_id} has no entries to send"
                        )));
                    };
                    if threshold == Some(ThresholdDecision::Background) {
                        let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                        let live = live_draft.get()?;
                        if live.compactions.is_none() {
                            create_compaction(
                                &tx,
                                conversation_id,
                                CompactionInput {
                                    reason: CompactionReason::Threshold,
                                    instructions: None,
                                },
                                None,
                            )
                            .await?;
                        }
                    }
                    Ok(Some(NextTaskState::Running {
                        checkpoint: GenerationCheckpoint::Request {
                            attempt,
                            compacted,
                            model: model_ref,
                            thinking_level,
                            stream_options,
                            cutoff,
                        },
                    }))
                })
            },
            context,
        )
        .await
}

async fn phase_request(
    task: RunningTask<GenerationInput, GenerationCheckpoint, GenerationResult>,
    runtime: Runtime,
    context: Context,
) -> pi_js::Result<()> {
    let GenerationCheckpoint::Request {
        attempt,
        compacted,
        model: ref_model,
        thinking_level,
        stream_options,
        cutoff,
    } = task.checkpoint
    else {
        return Err(pi_js::Error::msg("Invalid generation checkpoint in request phase"));
    };

    let conversation_id = runtime.conversation_id()?;
    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    let mut live = live_draft.get()?;
                    convert_partial(tx.as_transaction(), &mut live, conversation_id).await?;
                    live.generation = Some(LiveGeneration {
                        attempt,
                        message: None,
                        retry: None,
                        deferred: None,
                    });
                    live_draft.replace(live)?;
                    Ok(None)
                })
            },
            context.clone(),
        )
        .await?;

    let models = runtime.models()?;
    let Some(model) = models.get_model(&ref_model.provider, &ref_model.model_id) else {
        return fail_no_model(&runtime, Some(&ref_model), context).await;
    };
    let view = runtime.context(conversation_id, context.clone(), Some(cutoff)).await?;

    let messages_state = Arc::new(Mutex::new(view.messages.clone()));
    runtime
        .hooks(GENERATION_TASK.name())?
        .each(|hooks, api| {
            let state = Arc::clone(&messages_state);
            let handler = hooks.before_request.clone();
            let ctx = context.clone();
            async move {
                let Some(before_request) = handler else {
                    return Ok(());
                };
                let cur = state.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some(replaced) = before_request(cur, api, ctx).await? {
                    *state.lock().unwrap_or_else(|e| e.into_inner()) = replaced;
                }
                Ok(())
            }
        })
        .await?;
    let messages = std::mem::take(&mut *messages_state.lock().unwrap_or_else(|e| e.into_inner()));

    let options = to_simple_stream_options(&stream_options, thinking_level, runtime.signal()?, None, None, true);
    let message = stream_response(&runtime, &model, messages, options, attempt, context.clone()).await?;
    let request = RequestContext {
        attempt,
        compacted,
        model: ref_model,
        cutoff,
        messages: Some(view.messages),
        poll_at: None,
    };
    classify(&runtime, request, message, context).await
}

async fn phase_retry(
    task: RunningTask<GenerationInput, GenerationCheckpoint, GenerationResult>,
    runtime: Runtime,
    context: Context,
) -> pi_js::Result<()> {
    let GenerationCheckpoint::Retry {
        attempt,
        compacted,
        until,
    } = task.checkpoint
    else {
        return Err(pi_js::Error::msg("Invalid generation checkpoint in retry phase"));
    };

    runtime.sleep(until, context.clone()).await?;
    let conversation_id = runtime.conversation_id()?;
    let next_attempt = attempt + 1;
    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    live_draft.update(|live| {
                        live.generation = Some(LiveGeneration {
                            attempt: next_attempt,
                            message: None,
                            retry: None,
                            deferred: None,
                        });
                    })?;
                    Ok(Some(NextTaskState::Running {
                        checkpoint: GenerationCheckpoint::Prepare {
                            attempt: next_attempt,
                            compacted,
                            overflow: None,
                        },
                    }))
                })
            },
            context,
        )
        .await
}

async fn phase_poll(
    task: RunningTask<GenerationInput, GenerationCheckpoint, GenerationResult>,
    runtime: Runtime,
    context: Context,
) -> pi_js::Result<()> {
    let GenerationCheckpoint::Poll {
        attempt,
        compacted,
        model: ref_model,
        cutoff,
        handle,
        poll_at,
    } = task.checkpoint
    else {
        return Err(pi_js::Error::msg("Invalid generation checkpoint in poll phase"));
    };

    let models = runtime.models()?;
    let Some(model) = models.get_model(&ref_model.provider, &ref_model.model_id) else {
        return fail_no_model(&runtime, Some(&ref_model), context).await;
    };
    runtime.sleep(poll_at, context.clone()).await?;
    let fetch_opts = ModelsDeferredFetchOptions {
        base: DeferredFetchOptions {
            base: pi_ai::types::ProviderRequestOptions {
                signal: Some(runtime.signal()?),
                ..Default::default()
            },
            wait: None,
        },
        transform_headers: None,
    };
    let message = models.fetch_deferred(&model, &handle, Some(fetch_opts)).await;
    let request = RequestContext {
        attempt,
        compacted,
        model: ref_model,
        cutoff,
        messages: None,
        poll_at: Some(poll_at),
    };
    classify(&runtime, request, message, context).await
}

async fn phase_tools(
    task: RunningTask<GenerationInput, GenerationCheckpoint, GenerationResult>,
    runtime: Runtime,
    context: Context,
) -> pi_js::Result<()> {
    let GenerationCheckpoint::Tools {
        assistant,
        mut tools,
        pending,
    } = task.checkpoint
    else {
        return Err(pi_js::Error::msg("Invalid generation checkpoint in tools phase"));
    };

    let mut pending_iter = pending.into_iter();
    let Some(next_call) = pending_iter.next() else {
        return finish_tool_round(&runtime, assistant, tools, context).await;
    };
    let rest: Vec<String> = pending_iter.collect();

    let conversation_id = runtime.conversation_id()?;
    let gen_task_id = runtime.task_id()?.erased();
    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    let mut live = live_draft.get()?;
                    let call_id_for_slot = next_call.clone();
                    let task_id = create_tool_task(&tx, gen_task_id, assistant, next_call).await?;
                    if let Some(slot) = live.tools.as_deref_mut().and_then(|list| {
                        list.iter_mut()
                            .find(|s| s.call_id == call_id_for_slot && s.task_id.is_none())
                    }) {
                        slot.task_id = Some(task_id.erased());
                    }
                    live_draft.replace(live)?;
                    tools.push(task_id);
                    Ok(Some(NextTaskState::Waiting {
                        checkpoint: GenerationCheckpoint::Tools {
                            assistant,
                            tools,
                            pending: rest,
                        },
                        on: vec![task_id.erased()],
                        policy: JoinPolicy::AllSettled,
                    }))
                })
            },
            context,
        )
        .await
}

async fn phase_abort(
    task: RunningTask<GenerationInput, GenerationCheckpoint, GenerationResult>,
    runtime: Runtime,
    context: Context,
) -> pi_js::Result<()> {
    let checkpoint = task.checkpoint;
    if let GenerationCheckpoint::Poll {
        model: ref_model,
        handle,
        ..
    } = &checkpoint
        && let Some(model) = runtime.models()?.get_model(&ref_model.provider, &ref_model.model_id)
    {
        let cancel_opts = ModelsDeferredCancelOptions {
            base: DeferredCancelOptions {
                signal: Some(runtime.signal()?),
                ..Default::default()
            },
            transform_headers: None,
        };
        if let Err(error) = runtime
            .models()?
            .cancel_deferred(&model, handle, Some(cancel_opts))
            .await
        {
            runtime.report(error);
        }
    }

    let conversation_id = runtime.conversation_id()?;
    let gen_task_id = runtime.task_id()?.erased();
    let now = runtime.now()?;
    let unstarted = match checkpoint {
        GenerationCheckpoint::Tools { assistant, pending, .. } => {
            read_calls(&runtime, assistant, &pending, context.clone()).await?
        }
        _ => Vec::new(),
    };

    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    let mut live = live_draft.get()?;
                    convert_partial(tx.as_transaction(), &mut live, conversation_id).await?;
                    for call in unstarted {
                        let result = harness_error("aborted", format!("Tool {} was aborted", call.name));
                        append_tool_result(&tx, conversation_id, &call, &result, now).await?;
                    }
                    end_run(
                        tx.as_transaction(),
                        &mut live,
                        gen_task_id,
                        &SubmissionSettlement::Unanswered {
                            reason: "aborted".to_owned(),
                            detail: None,
                        },
                    )?;
                    live_draft.replace(live)?;
                    Ok(Some(NextTaskState::Terminal {
                        outcome: TaskOutcome::Aborted {
                            reason: None,
                            result: None,
                        },
                    }))
                })
            },
            context,
        )
        .await
}

async fn read_calls(
    runtime: &Runtime,
    assistant: EntryId,
    call_ids: &[String],
    context: Context,
) -> pi_js::Result<Vec<ToolCall>> {
    if call_ids.is_empty() {
        return Ok(Vec::new());
    }
    let entry = runtime.typed_entry(&ASSISTANT_ENTRY, assistant, context).await?;
    let message = entry
        .as_ref()
        .and_then(|typed| typed.record.model.as_deref())
        .and_then(|msgs| msgs.first());
    let calls: Vec<ToolCall> = match message {
        Some(Message::Assistant(a)) => a
            .content
            .iter()
            .filter_map(|block| match block {
                AssistantContentBlock::ToolCall(tc) => Some(tc.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let mut selected = Vec::with_capacity(call_ids.len());
    for id in call_ids {
        if let Some(call) = calls.iter().find(|c| &c.id == id) {
            selected.push(call.clone());
        }
    }
    Ok(selected)
}

async fn create_tool_task(
    tx: &Tx,
    owner: TaskId,
    assistant: EntryId,
    call_id: String,
) -> pi_js::Result<TaskId<ToolTaskResult>> {
    tx.create_task(
        &*TOOL_TASK,
        &ToolTaskInput { assistant, call_id },
        TaskOptions {
            ownership: TaskOwnership::Task { task_id: owner },
            conversation_id: None,
            background: false,
        },
    )
    .await
}

fn threshold_compaction(
    view: &ContextView,
    planned: &[TypedEntryDraft<()>],
    context_window: i64,
    policy: &CompactionPolicy,
) -> Option<ThresholdDecision> {
    if !policy.enabled || context_window <= 0 {
        return None;
    }
    let extra: Vec<Message> = planned
        .iter()
        .flat_map(|entry| entry.model.as_deref().unwrap_or(&[]).iter().cloned())
        .collect();
    let tokens = estimate_context(view, &extra);
    let blocking = context_window.saturating_sub(policy.reserve_tokens);
    let background = blocking.saturating_sub(policy.background_tokens as i64);
    let over = if tokens > blocking {
        Some(ThresholdDecision::Blocking)
    } else if policy.background_tokens > 0 && tokens > background {
        Some(ThresholdDecision::Background)
    } else {
        None
    }?;
    select_cut(view, policy.keep_recent_tokens)?;
    Some(over)
}

async fn fail_model_error(runtime: &Runtime, text: String, context: Context) -> pi_js::Result<()> {
    let conversation_id = runtime.conversation_id()?;
    let gen_task_id = runtime.task_id()?.erased();
    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    let mut live = live_draft.get()?;
                    end_run(
                        tx.as_transaction(),
                        &mut live,
                        gen_task_id,
                        &SubmissionSettlement::Unanswered {
                            reason: "model_error".to_owned(),
                            detail: Some(JsonValue::String(text.clone())),
                        },
                    )?;
                    live_draft.replace(live)?;
                    let mut detail_map = serde_json::Map::new();
                    detail_map.insert("reason".to_owned(), JsonValue::String("model_error".to_owned()));
                    Ok(Some(NextTaskState::Terminal {
                        outcome: TaskOutcome::Failed {
                            error: TaskOutcomeError {
                                message: text,
                                detail: Some(JsonValue::Object(detail_map)),
                            },
                            result: None,
                        },
                    }))
                })
            },
            context,
        )
        .await
}

async fn fail_no_model(runtime: &Runtime, ref_model: Option<&ModelRef>, context: Context) -> pi_js::Result<()> {
    let message = match ref_model {
        None => "No model is configured".to_owned(),
        Some(r) => format!("Model {}/{} is not available", r.provider, r.model_id),
    };
    let conversation_id = runtime.conversation_id()?;
    let gen_task_id = runtime.task_id()?.erased();
    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    let mut live = live_draft.get()?;
                    end_run(
                        tx.as_transaction(),
                        &mut live,
                        gen_task_id,
                        &SubmissionSettlement::Unanswered {
                            reason: "no_model".to_owned(),
                            detail: None,
                        },
                    )?;
                    live_draft.replace(live)?;
                    let mut detail_map = serde_json::Map::new();
                    detail_map.insert("reason".to_owned(), JsonValue::String("no_model".to_owned()));
                    Ok(Some(NextTaskState::Terminal {
                        outcome: TaskOutcome::Failed {
                            error: TaskOutcomeError {
                                message,
                                detail: Some(JsonValue::Object(detail_map)),
                            },
                            result: None,
                        },
                    }))
                })
            },
            context,
        )
        .await
}

/// Append a committed partial left by an interrupted, aborted, faulted, or orphaned attempt as an aborted
/// assistant entry; the caller replaces or removes `generation`.
pub async fn convert_partial(
    tx: &Transaction,
    live: &mut LiveState,
    conversation_id: ConversationId,
) -> pi_js::Result<()> {
    let Some(partial) = live.generation.as_ref().and_then(|g| g.message.as_ref()) else {
        return Ok(());
    };
    let mut aborted = partial.clone();
    aborted.stop_reason = StopReason::Aborted;
    append_assistant_tx(tx, conversation_id, aborted).await?;
    Ok(())
}

struct PartialThrottleState {
    pending: Option<AssistantMessage>,
    timer: Option<JoinHandle<()>>,
    in_flight: Option<JoinHandle<()>>,
    stopped: bool,
}

#[derive(Clone)]
struct PartialFlusher {
    runtime: Runtime,
    attempt: u32,
    context: Context,
    state: Arc<Mutex<PartialThrottleState>>,
}

impl PartialFlusher {
    fn new(runtime: Runtime, attempt: u32, context: Context) -> Self {
        Self {
            runtime,
            attempt,
            context,
            state: Arc::new(Mutex::new(PartialThrottleState {
                pending: None,
                timer: None,
                in_flight: None,
                stopped: false,
            })),
        }
    }

    fn push(&self, partial: AssistantMessage) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.stopped {
            return;
        }
        guard.pending = Some(partial);
        if guard.timer.is_none() && guard.in_flight.is_none() {
            guard.timer = Some(Self::spawn_timer(self.clone()));
        }
    }

    fn spawn_timer(this: Self) -> JoinHandle<()> {
        tokio::spawn(async move {
            let _ = pi_js::time::sleep(PARTIAL_THROTTLE_MS, None).await;
            let partial = {
                let mut guard = this.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.timer = None;
                if guard.stopped {
                    return;
                }
                let Some(partial) = guard.pending.take() else {
                    return;
                };
                let handle = Self::spawn_flush(this.clone(), partial);
                guard.in_flight = Some(handle);
                true
            };
            let _ = partial;
        })
    }

    fn spawn_flush(this: Self, partial: AssistantMessage) -> JoinHandle<()> {
        tokio::spawn(async move {
            let res: pi_js::Result<()> = async {
                let raw =
                    serde_json::to_value(&partial).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
                let message = copy_json(&raw, None)?;
                let conversation_id = this.runtime.conversation_id()?;
                let attempt = this.attempt;
                this.runtime
                    .commit(
                        move |tx, _current| {
                            Box::pin(async move {
                                let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                                let root = live_draft.overlay()?;
                                if root.get("generation")?.is_undefined() {
                                    let mut gen_map = serde_json::Map::new();
                                    gen_map.insert("attempt".to_owned(), JsonValue::from(attempt));
                                    root.set("generation", JsonValue::Object(gen_map))?;
                                }
                                let gen_val = root.get("generation")?;
                                if let Some(gen_ov) = gen_val.as_overlay() {
                                    assign_json(gen_ov, JsonKey::Prop("message"), &message)?;
                                }
                                Ok(None)
                            })
                        },
                        this.context.clone(),
                    )
                    .await
            }
            .await;

            if let Err(err) = res
                && let Ok(sig) = this.runtime.signal()
                && !sig.aborted()
            {
                this.runtime.report(err);
            }

            let mut guard = this.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.in_flight = None;
            if guard.pending.is_some() && !guard.stopped {
                guard.timer = Some(Self::spawn_timer(this.clone()));
            }
        })
    }

    async fn stop(&self) {
        let (timer, in_flight) = {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.stopped = true;
            (guard.timer.take(), guard.in_flight.take())
        };
        if let Some(handle) = timer {
            handle.abort();
        }
        if let Some(handle) = in_flight {
            let _ = handle.await;
        }
    }
}

/// Stream one request and return the terminal message. Partials commit as trailing writes at most every 100 ms
/// with one commit in flight; cleanup stops the throttle and awaits that commit, so no stale partial lands after
/// the outcome.
async fn stream_response(
    runtime: &Runtime,
    model: &Model,
    messages: Vec<Message>,
    options: SimpleStreamOptions,
    attempt: u32,
    context: Context,
) -> pi_js::Result<AssistantMessage> {
    let flusher = PartialFlusher::new(runtime.clone(), attempt, context);
    let mut events = runtime.models()?.stream_simple(
        model,
        &PiAiContext {
            system_prompt: None,
            messages,
            tools: None,
        },
        Some(ModelsSimpleStreamOptions {
            base: options,
            transform_headers: None,
        }),
    );

    while let Some(event) = events.next().await {
        let partial = match &event {
            AssistantMessageEvent::Start { partial }
            | AssistantMessageEvent::TextStart { partial, .. }
            | AssistantMessageEvent::TextDelta { partial, .. }
            | AssistantMessageEvent::TextEnd { partial, .. }
            | AssistantMessageEvent::ThinkingStart { partial, .. }
            | AssistantMessageEvent::ThinkingDelta { partial, .. }
            | AssistantMessageEvent::ThinkingEnd { partial, .. }
            | AssistantMessageEvent::ToolCallStart { partial, .. }
            | AssistantMessageEvent::ToolCallDelta { partial, .. }
            | AssistantMessageEvent::ToolCallEnd { partial, .. } => Some(partial),
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => None,
        };
        if let Some(partial) = partial
            && !partial.content.is_empty()
        {
            flusher.push(partial.clone());
        }
    }

    let result = events.result().await;
    flusher.stop().await;
    Ok(result)
}

async fn classify(
    runtime: &Runtime,
    request: RequestContext,
    message: AssistantMessage,
    context: Context,
) -> pi_js::Result<()> {
    runtime.signal()?.throw_if_aborted()?;
    let conversation_id = runtime.conversation_id()?;
    let attempt = request.attempt;
    let compacted = request.compacted;
    let ref_model = request.model.clone();
    let cutoff = request.cutoff;

    if message.stop_reason == StopReason::Deferred
        && let Some(handle) = message.deferred.clone()
    {
        let poll_after = handle.poll_after_ms.unwrap_or(DEFAULT_POLL_AFTER_MS);
        let now_poll = runtime.now()? + poll_after;
        let poll_at = match request.poll_at {
            Some(prev) => now_poll.max(prev + 1),
            None => now_poll,
        };
        runtime
            .commit(
                move |tx, _current| {
                    Box::pin(async move {
                        let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                        live_draft.update(|live| {
                            live.generation = Some(LiveGeneration {
                                attempt,
                                message: None,
                                retry: None,
                                deferred: Some(LiveDeferred { poll_at }),
                            });
                        })?;
                        Ok(Some(NextTaskState::Running {
                            checkpoint: GenerationCheckpoint::Poll {
                                attempt,
                                compacted,
                                model: ref_model,
                                cutoff,
                                handle,
                                poll_at,
                            },
                        }))
                    })
                },
                context,
            )
            .await?;
        return Ok(());
    }

    runtime
        .hooks(GENERATION_TASK.name())?
        .each(|hooks, api| {
            let handler = hooks.after_response.clone();
            let msg = message.clone();
            let ctx = context.clone();
            async move {
                if let Some(after_response) = handler {
                    after_response(msg, api, ctx).await?;
                }
                Ok(())
            }
        })
        .await?;

    let calls: Vec<ToolCall> = message
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::ToolCall(tc) => Some(tc.clone()),
            _ => None,
        })
        .collect();

    if message.stop_reason == StopReason::ToolUse && !calls.is_empty() {
        return start_tool_round(runtime, request, message, calls, context).await;
    }
    if matches!(
        message.stop_reason,
        StopReason::Stop | StopReason::Length | StopReason::ToolUse
    ) {
        return answer(runtime, message, context).await;
    }

    let settings = runtime.settings()?;
    let overflow = message.stop_reason == StopReason::Error && is_context_overflow(&message, None);
    let gen_task_id = runtime.task_id()?.erased();
    if overflow && compacted.is_none() && settings.compaction.enabled {
        let policy = settings.compaction;
        let view = runtime.context(conversation_id, context.clone(), Some(cutoff)).await?;
        if select_cut(&view, policy.keep_recent_tokens).is_some() {
            let text = message
                .error_message
                .clone()
                .unwrap_or_else(|| "Context overflow".to_owned());
            runtime
                .commit(
                    move |tx, _current| {
                        Box::pin(async move {
                            let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                            let mut live = live_draft.get()?;
                            append_assistant(&tx, conversation_id, message).await?;
                            live.generation = None;
                            live_draft.replace(live)?;
                            let child = create_compaction(
                                &tx,
                                conversation_id,
                                CompactionInput {
                                    reason: CompactionReason::Overflow,
                                    instructions: None,
                                },
                                Some(gen_task_id),
                            )
                            .await?;
                            Ok(Some(NextTaskState::Waiting {
                                checkpoint: GenerationCheckpoint::Prepare {
                                    attempt,
                                    compacted: Some(child),
                                    overflow: Some(text),
                                },
                                on: vec![child.erased()],
                                policy: JoinPolicy::AllSettled,
                            }))
                        })
                    },
                    context,
                )
                .await?;
            return Ok(());
        }
    }

    let retry_policy = settings.retry;
    let should_retry = message.stop_reason == StopReason::Error
        && !overflow
        && is_retryable_assistant_error(&message)
        && retry_policy.enabled
        && attempt <= retry_policy.max_retries;
    let until = if should_retry {
        let ai_policy = PiAiRetryPolicy {
            enabled: retry_policy.enabled,
            max_retries: retry_policy.max_retries,
            base_delay_ms: retry_policy.base_delay_ms,
            max_agent_delay_ms: retry_policy.max_agent_delay_ms,
        };
        runtime.now()? + retry_delay_ms(&ai_policy, attempt) as i64
    } else {
        0
    };

    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    let mut live = live_draft.get()?;
                    let err_msg = message.error_message.clone();
                    let stop_str = message.stop_reason.as_str().to_owned();
                    append_assistant(&tx, conversation_id, message).await?;
                    if should_retry {
                        live.generation = Some(LiveGeneration {
                            attempt,
                            message: None,
                            retry: Some(DurableRetryState {
                                at: until,
                                error: err_msg.unwrap_or_default(),
                            }),
                            deferred: None,
                        });
                        live_draft.replace(live)?;
                        return Ok(Some(NextTaskState::Running {
                            checkpoint: GenerationCheckpoint::Retry {
                                attempt,
                                compacted,
                                until,
                            },
                        }));
                    }
                    let text = err_msg.unwrap_or_else(|| format!("Model response ended with stop reason {stop_str}"));
                    end_run(
                        tx.as_transaction(),
                        &mut live,
                        gen_task_id,
                        &SubmissionSettlement::Unanswered {
                            reason: "model_error".to_owned(),
                            detail: Some(JsonValue::String(text.clone())),
                        },
                    )?;
                    live_draft.replace(live)?;
                    let mut detail_map = serde_json::Map::new();
                    detail_map.insert("reason".to_owned(), JsonValue::String("model_error".to_owned()));
                    Ok(Some(NextTaskState::Terminal {
                        outcome: TaskOutcome::Failed {
                            error: TaskOutcomeError {
                                message: text,
                                detail: Some(JsonValue::Object(detail_map)),
                            },
                            result: None,
                        },
                    }))
                })
            },
            context,
        )
        .await
}

async fn answer(runtime: &Runtime, message: AssistantMessage, context: Context) -> pi_js::Result<()> {
    let cont_state = Arc::new(Mutex::new(None::<UserInput>));
    runtime
        .hooks(GENERATION_TASK.name())?
        .each(|hooks, api| {
            let state = Arc::clone(&cont_state);
            let handler = hooks.on_yield.clone();
            let msg = message.clone();
            let ctx = context.clone();
            async move {
                if state.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                    return Ok(());
                }
                let Some(on_yield) = handler else {
                    return Ok(());
                };
                if let Some(next_input) = on_yield(msg, api, ctx).await? {
                    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                    if guard.is_none() {
                        *guard = Some(next_input);
                    }
                }
                Ok(())
            }
        })
        .await?;
    let continuation = cont_state.lock().unwrap_or_else(|e| e.into_inner()).take();

    let conversation_id = runtime.conversation_id()?;
    let gen_task_id = runtime.task_id()?.erased();
    let now = runtime.now()?;
    let queue_modes = QueueModes::from(&runtime.settings()?);
    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let mut boundary = prepare_boundary(&tx, conversation_id, queue_modes).await?;
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    let mut live = live_draft.get()?;
                    let entry = append_assistant(&tx, conversation_id, message).await?;
                    let entry_id = entry.record.id;
                    let result = NextTaskState::Terminal {
                        outcome: TaskOutcome::Completed {
                            result: GenerationResult { entry_id },
                        },
                    };
                    let boundary_res = apply_boundary(&tx, &mut boundary, BoundaryAt::Final, now).await?;
                    if let Some(cont) = continuation
                        && boundary_res.users.is_empty()
                        && !boundary_res.reset
                    {
                        let user = Message::User(UserMessage {
                            content: cont,
                            timestamp: now,
                        });
                        tx.append_typed_entry(
                            &USER_ENTRY,
                            conversation_id,
                            TypedEntryDraft {
                                model: Some(vec![user]),
                                data: (),
                                head: None,
                                edits: None,
                            },
                        )
                        .await?;
                        let next_gen = create_generation(&tx, conversation_id).await?;
                        hand_over(&mut live, gen_task_id, next_gen.erased());
                        live.generation = None;
                        live_draft.replace(live)?;
                        return Ok(Some(result));
                    }
                    end_run(
                        tx.as_transaction(),
                        &mut live,
                        gen_task_id,
                        &SubmissionSettlement::Done { answer: entry_id },
                    )?;
                    if !boundary_res.users.is_empty() {
                        start_run(&tx, conversation_id, &mut live, boundary_res.users).await?;
                    }
                    live_draft.replace(live)?;
                    Ok(Some(result))
                })
            },
            context,
        )
        .await
}

async fn start_tool_round(
    runtime: &Runtime,
    request: RequestContext,
    message: AssistantMessage,
    calls: Vec<ToolCall>,
    context: Context,
) -> pi_js::Result<()> {
    let conversation_id = runtime.conversation_id()?;
    let messages = match request.messages {
        Some(msgs) => msgs,
        None => {
            runtime
                .context(conversation_id, context.clone(), Some(request.cutoff))
                .await?
                .messages
        }
    };
    let offered: IndexSet<String> = get_current_tools(&messages).into_iter().map(|tool| tool.name).collect();
    let tools = runtime.agent(context.clone()).await?.tools;
    let settings = runtime.settings()?;
    let sequential = settings.tool_execution == ToolExecutionMode::Sequential
        || calls.iter().any(|call| {
            offered.contains(&call.name)
                && tools
                    .iter()
                    .find(|t| t.name == call.name)
                    .and_then(|t| t.execution_mode)
                    == Some(ToolExecutionMode::Sequential)
        });

    let gen_task_id = runtime.task_id()?.erased();
    let now = runtime.now()?;
    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    let mut live = live_draft.get()?;
                    let entry = append_assistant(&tx, conversation_id, message).await?;
                    let entry_id = entry.record.id;

                    let mut slots: Vec<ToolSlot> = Vec::with_capacity(calls.len());
                    let mut started_tasks: Vec<TaskId<ToolTaskResult>> = Vec::new();
                    let mut pending: Vec<String> = Vec::new();

                    for call in calls {
                        if !offered.contains(&call.name) {
                            let unavailable =
                                harness_error("tool_unavailable", format!("Tool {} is not available", call.name));
                            let res_entry = append_tool_result(&tx, conversation_id, &call, &unavailable, now).await?;
                            slots.push(ToolSlot {
                                call_id: call.id,
                                name: call.name,
                                task_id: None,
                                status: ToolSlotStatus::Done,
                                output: None,
                                dropped_bytes: None,
                                dropped_lines: None,
                                details: None,
                                diagnostics: None,
                                entry: Some(res_entry.record.id),
                            });
                            continue;
                        }
                        if sequential && !started_tasks.is_empty() {
                            pending.push(call.id.clone());
                            slots.push(ToolSlot {
                                call_id: call.id,
                                name: call.name,
                                task_id: None,
                                status: ToolSlotStatus::Pending,
                                output: None,
                                dropped_bytes: None,
                                dropped_lines: None,
                                details: None,
                                diagnostics: None,
                                entry: None,
                            });
                            continue;
                        }
                        let task_id = create_tool_task(&tx, gen_task_id, entry_id, call.id.clone()).await?;
                        started_tasks.push(task_id);
                        slots.push(ToolSlot {
                            call_id: call.id,
                            name: call.name,
                            task_id: Some(task_id.erased()),
                            status: ToolSlotStatus::Pending,
                            output: None,
                            dropped_bytes: None,
                            dropped_lines: None,
                            details: None,
                            diagnostics: None,
                            entry: None,
                        });
                    }

                    live.generation = None;
                    live.tools = Some(slots);
                    live_draft.replace(live)?;

                    let wait_on: Vec<TaskId> = started_tasks.iter().map(|id| id.erased()).collect();
                    Ok(Some(NextTaskState::Waiting {
                        checkpoint: GenerationCheckpoint::Tools {
                            assistant: entry_id,
                            tools: started_tasks,
                            pending,
                        },
                        on: wait_on,
                        policy: JoinPolicy::AllSettled,
                    }))
                })
            },
            context,
        )
        .await
}

async fn finish_tool_round(
    runtime: &Runtime,
    assistant: EntryId,
    tools: Vec<TaskId<ToolTaskResult>>,
    context: Context,
) -> pi_js::Result<()> {
    let conversation_id = runtime.conversation_id()?;
    let outcomes = runtime.outcomes(&tools, context.clone()).await?;
    let mut controls: IndexMap<TaskId, Option<ToolControl>> = IndexMap::with_capacity(tools.len());
    for (id, outcome) in tools.into_iter().zip(outcomes.into_iter()) {
        let control = match outcome {
            TaskOutcome::Completed { result } => result.control,
            _ => None,
        };
        controls.insert(id.erased(), control);
    }

    let live_snapshot = runtime
        .read()?
        .snapshot_conversation(&*LIVE_DOC, conversation_id, context.clone())
        .await?;
    let slots = live_snapshot.and_then(|s| s.tools).unwrap_or_default();
    let results: Vec<EntryId> = slots.iter().filter_map(|slot| slot.entry).collect();

    runtime
        .hooks(GENERATION_TASK.name())?
        .each(|hooks, api| {
            let handler = hooks.after_tools.clone();
            let res_ids = results.clone();
            let ctx = context.clone();
            async move {
                if let Some(after_tools) = handler {
                    after_tools(assistant, res_ids, api, ctx).await?;
                }
                Ok(())
            }
        })
        .await?;

    let terminate = !slots.is_empty()
        && slots.iter().all(|slot| {
            slot.task_id
                .and_then(|id| controls.get(&id))
                .and_then(|c| c.as_ref())
                .and_then(|c| c.terminate)
                == Some(true)
        });

    let added: Vec<String> = controls
        .values()
        .flatten()
        .flat_map(|c| c.add_tools.iter().flatten().cloned())
        .collect();
    let handoff = controls.values().rev().flatten().find_map(|c| c.handoff.clone());

    let gen_task_id = runtime.task_id()?.erased();
    let now = runtime.now()?;
    let queue_modes = QueueModes::from(&runtime.settings()?);
    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let mut boundary = prepare_boundary(&tx, conversation_id, queue_modes).await?;
                    if !added.is_empty() {
                        add_tools(&tx, conversation_id, &added).await?;
                    }
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    let mut live = live_draft.get()?;

                    if terminate || handoff.is_some() {
                        if let Some(handoff_text) = handoff {
                            let message = Message::User(UserMessage {
                                content: UserInput::Text(handoff_text),
                                timestamp: now,
                            });
                            let entry = tx
                                .append_typed_entry(
                                    &RESET_ENTRY,
                                    conversation_id,
                                    TypedEntryDraft {
                                        model: Some(vec![message]),
                                        data: (),
                                        head: Some(EntryDraftHead::SelfId(SelfHead)),
                                        edits: None,
                                    },
                                )
                                .await?;
                            boundary.head = Some(entry.record.id);
                        }
                        let boundary_res = apply_boundary(&tx, &mut boundary, BoundaryAt::Final, now).await?;
                        end_run(
                            tx.as_transaction(),
                            &mut live,
                            gen_task_id,
                            &SubmissionSettlement::Done { answer: assistant },
                        )?;
                        if !boundary_res.users.is_empty() {
                            start_run(&tx, conversation_id, &mut live, boundary_res.users).await?;
                        }
                    } else {
                        let boundary_res = apply_boundary(&tx, &mut boundary, BoundaryAt::PostTools, now).await?;
                        if boundary_res.reset {
                            end_run(
                                tx.as_transaction(),
                                &mut live,
                                gen_task_id,
                                &SubmissionSettlement::Unanswered {
                                    reason: "reset".to_owned(),
                                    detail: None,
                                },
                            )?;
                            if !boundary_res.users.is_empty() {
                                start_run(&tx, conversation_id, &mut live, boundary_res.users).await?;
                            }
                        } else {
                            live.tools = None;
                            if let Some(ref mut run) = live.run
                                && run.task_id == gen_task_id
                            {
                                run.inputs.extend(boundary_res.users);
                            }
                            let next_gen = create_generation(&tx, conversation_id).await?;
                            hand_over(&mut live, gen_task_id, next_gen.erased());
                        }
                    }

                    live_draft.replace(live)?;
                    Ok(Some(NextTaskState::Terminal {
                        outcome: TaskOutcome::Completed {
                            result: GenerationResult { entry_id: assistant },
                        },
                    }))
                })
            },
            context,
        )
        .await
}

async fn append_assistant(
    tx: &Tx,
    conversation_id: ConversationId,
    message: AssistantMessage,
) -> pi_js::Result<TypedEntry<()>> {
    append_assistant_tx(tx.as_transaction(), conversation_id, message).await
}

/// Append a provider result and add its usage to `pi.usage` in the same commit.
/// REMINDER: every built-in writer of assistant entries goes through here, so the usage ledger stays complete.
async fn append_assistant_tx(
    tx: &Transaction,
    conversation_id: ConversationId,
    message: AssistantMessage,
) -> pi_js::Result<TypedEntry<()>> {
    let usage_key = format!("{}/{}", message.provider, message.model);
    record_usage_tx(tx, conversation_id, UsageBucket::Models, &usage_key, &message.usage).await?;
    tx.append_typed_entry(
        &ASSISTANT_ENTRY,
        conversation_id,
        TypedEntryDraft {
            model: Some(vec![Message::Assistant(message)]),
            data: (),
            head: None,
            edits: None,
        },
    )
    .await
}

/// Start a run for `inputs`, placed input submissions: a new generation takes `pi.live.run`.
pub async fn start_run(
    tx: &Tx,
    conversation_id: ConversationId,
    live: &mut LiveState,
    inputs: Vec<SubmissionId>,
) -> pi_js::Result<()> {
    let task_id = create_generation(tx, conversation_id).await?.erased();
    live.run = Some(LiveRun { task_id, inputs });
    Ok(())
}

async fn create_generation(tx: &Tx, conversation_id: ConversationId) -> pi_js::Result<TaskId<GenerationResult>> {
    tx.create_task(
        &*GENERATION_TASK,
        &GenerationInput {},
        TaskOptions {
            ownership: TaskOwnership::Conversation,
            conversation_id: Some(conversation_id),
            background: false,
        },
    )
    .await
}

/// Hand run control from `from` to `to`; the run's inputs move with it.
pub fn hand_over(live: &mut LiveState, from: TaskId, to: TaskId) {
    if let Some(ref mut run) = live.run
        && run.task_id == from
    {
        run.task_id = to;
    }
}
