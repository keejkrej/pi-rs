//! Port of packages/durable/src/harness/compaction.ts

use std::sync::{Arc, LazyLock, Mutex};

use indexmap::{IndexMap, IndexSet};
use pi_ai::models::ModelsSimpleStreamOptions;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, CacheRetention, Context as PiAiContext, Message, ModelThinkingLevel,
    StopReason, SystemMessage, SystemMessageContent, TextContent, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_ai::utils::estimate::{calculate_context_tokens, estimate_message_tokens};
use pi_ai::utils::retry::{RetryPolicy as PiAiRetryPolicy, is_retryable_assistant_error, retry_delay_ms};
use pi_chord::Context;
use serde::{Deserialize, Serialize};

use crate::entries::COMPACTION_ENTRY;
use crate::harness::context::order_tool_results;
use crate::harness::generation::to_simple_stream_options;
use crate::harness::inbox::QueueModes;
use crate::harness::live::{
    DurableRetryState, LIVE_DOC, add_compaction_status, compaction_status_mut, remove_compaction_status,
};
use crate::harness::submissions::admit_submission;
use crate::harness::types::{
    BeforeCompactDecision, BeforeCompactInput, CompactionHooks, CompactionReason, CompactionResult, ContextView,
    ConversationStreamOptions, ModelRef, SubmissionDraft,
};
use crate::harness::usage::{UsageBucket, record_usage};
use crate::session::transaction::Tx;
use crate::tasks::{Task, TaskDefinition, TaskRuntimeHandle, define_task};
use crate::types::{
    ConversationId, EntryDraft, EntryDraftHead, EntryId, JsonValue, NextTaskState, RunningTask, TaskId, TaskOptions,
    TaskOutcome, TaskOutcomeError, TaskOwnership,
};

/// Longest tool result text a serialized summary source keeps.
const TOOL_RESULT_MAX_CHARS: usize = 2000;

const SUMMARY_PREFIX: &str =
    "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
const SUMMARY_SUFFIX: &str = "\n</summary>";

const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work. If the conversation starts with an earlier summary, preserve its information and fold the newer messages into it.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or \"(none)\" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionInput {
    pub reason: CompactionReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// The pinned summarization request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryRequest {
    pub attempt: u32,
    pub model: ModelRef,
    pub thinking_level: ModelThinkingLevel,
    pub stream_options: ConversationStreamOptions,
    pub max_tokens: u64,
    /// Newest entry of the context the range was selected from.
    pub tail: EntryId,
    /// First entry kept verbatim; the summary's `head`.
    pub first_kept: EntryId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum CompactionCheckpoint {
    Select,
    Summarize {
        #[serde(flatten)]
        request: SummaryRequest,
    },
    Retry {
        until: i64,
        #[serde(flatten)]
        request: SummaryRequest,
    },
}

type Runtime = TaskRuntimeHandle<CompactionInput, CompactionCheckpoint, CompactionResult, CompactionHooks>;

/// Built-in compaction task (spec §8.7): select an old prefix of the model context, summarize it, and place a
/// summary entry whose `head` is the first kept entry. A compaction the generation owns blocks it and appends
/// directly; a conversation-owned one places its summary through a write submission.
pub static COMPACTION_TASK: LazyLock<Task<CompactionInput, CompactionCheckpoint, CompactionResult, CompactionHooks>> =
    LazyLock::new(|| {
        let mut phases = IndexMap::new();
        phases.insert(
            "select".to_owned(),
            Arc::new(
                |task: RunningTask<CompactionInput, CompactionCheckpoint, CompactionResult>,
                 runtime: Runtime,
                 context: Context| {
                    Box::pin(async move { phase_select(task, runtime, context).await })
                },
            )
                as crate::tasks::TypedPhaseHandler<
                    CompactionInput,
                    CompactionCheckpoint,
                    CompactionResult,
                    CompactionHooks,
                >,
        );
        phases.insert(
            "summarize".to_owned(),
            Arc::new(
                |task: RunningTask<CompactionInput, CompactionCheckpoint, CompactionResult>,
                 runtime: Runtime,
                 context: Context| {
                    Box::pin(async move { phase_summarize(task, runtime, context).await })
                },
            ),
        );
        phases.insert(
            "retry".to_owned(),
            Arc::new(
                |task: RunningTask<CompactionInput, CompactionCheckpoint, CompactionResult>,
                 runtime: Runtime,
                 context: Context| {
                    Box::pin(async move { phase_retry(task, runtime, context).await })
                },
            ),
        );

        define_task(TaskDefinition {
            name: "pi.compaction".to_owned(),
            version: 1,
            initial: Arc::new(|_input| Ok(CompactionCheckpoint::Select)),
            phases,
            abort: Arc::new(|_task, runtime, context| Box::pin(async move { phase_abort(runtime, context).await })),
            migrate: None,
            _hooks: std::marker::PhantomData,
        })
    });

async fn phase_select(
    task: RunningTask<CompactionInput, CompactionCheckpoint, CompactionResult>,
    runtime: Runtime,
    context: Context,
) -> pi_js::Result<()> {
    let conversation_id = runtime.conversation_id()?;
    let agent = runtime.agent(context.clone()).await?;
    let settings = runtime.settings()?;
    let ref_model = agent.model.clone();
    let models = runtime.models()?;
    let model = ref_model
        .as_ref()
        .and_then(|r| models.get_model(&r.provider, &r.model_id));
    let (Some(ref_model), Some(model)) = (ref_model, model) else {
        return fail_no_model(&runtime, ref_model.as_ref(), context).await;
    };

    let policy = settings.compaction;
    let view = runtime.context(conversation_id, context.clone(), None).await?;
    let Some(cut) = select_cut(&view, policy.keep_recent_tokens) else {
        return complete_empty(&runtime, context).await;
    };
    let first_kept = view.entries[cut].id;
    let compaction_input = BeforeCompactInput {
        reason: task.input.reason,
        entries: view.entries[..cut].to_vec(),
        messages: summarized_messages(&view, cut),
        first_kept,
        instructions: task.input.instructions.clone(),
    };

    let decision_state = Arc::new(Mutex::new(None::<BeforeCompactDecision>));
    runtime
        .hooks(COMPACTION_TASK.name())?
        .each(|hooks, api| {
            let state = Arc::clone(&decision_state);
            let handler = hooks.before_compact.clone();
            let input_for_hook = compaction_input.clone();
            let ctx = context.clone();
            async move {
                if state.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                    return Ok(());
                }
                let Some(before_compact) = handler else {
                    return Ok(());
                };
                if let Some(decision) = before_compact(input_for_hook, api, ctx).await? {
                    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                    if guard.is_none() {
                        *guard = Some(decision);
                    }
                }
                Ok(())
            }
        })
        .await?;

    let decision = decision_state.lock().unwrap_or_else(|e| e.into_inner()).take();
    match decision {
        Some(BeforeCompactDecision::Decline) => return complete_empty(&runtime, context).await,
        Some(BeforeCompactDecision::Summary(summary)) => {
            return place(&runtime, first_kept, summary, context).await;
        }
        None => {}
    }

    let max_budget = (0.8 * policy.reserve_tokens as f64).floor() as u64;
    let max_tokens = if model.max_tokens > 0 {
        max_budget.min(model.max_tokens as u64)
    } else {
        max_budget
    };
    let tail = view.entries.iter().fold(first_kept, |acc, entry| acc.max(entry.id));
    let request = SummaryRequest {
        attempt: 1,
        model: ref_model,
        thinking_level: agent.thinking_level,
        stream_options: settings.stream,
        max_tokens,
        tail,
        first_kept,
    };

    runtime
        .commit(
            move |_tx, _current| {
                Box::pin(async move {
                    Ok(Some(NextTaskState::Running {
                        checkpoint: CompactionCheckpoint::Summarize { request },
                    }))
                })
            },
            context,
        )
        .await
}

async fn phase_summarize(
    task: RunningTask<CompactionInput, CompactionCheckpoint, CompactionResult>,
    runtime: Runtime,
    context: Context,
) -> pi_js::Result<()> {
    let CompactionCheckpoint::Summarize { request } = task.checkpoint else {
        return Err(pi_js::Error::msg("Invalid compaction checkpoint in summarize phase"));
    };
    let models = runtime.models()?;
    let Some(model) = models.get_model(&request.model.provider, &request.model.model_id) else {
        return fail_no_model(&runtime, Some(&request.model), context).await;
    };

    let conversation_id = runtime.conversation_id()?;
    let view = runtime
        .context(conversation_id, context.clone(), Some(request.tail))
        .await?;
    let cut = view
        .entries
        .iter()
        .position(|entry| entry.id == request.first_kept)
        .unwrap_or(0);
    let now = runtime.now()?;
    let prompt_text = summary_prompt(&summarized_messages(&view, cut), task.input.instructions.as_deref());
    let messages = vec![
        Message::System(SystemMessage {
            content: SystemMessageContent::Text(SUMMARIZATION_SYSTEM_PROMPT.to_owned()),
            sections: None,
            tools_added: None,
            tools_removed: None,
            timestamp: now,
        }),
        Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(prompt_text))]),
            timestamp: now,
        }),
    ];

    let signal = runtime.signal()?;
    let options = to_simple_stream_options(
        &request.stream_options,
        request.thinking_level,
        signal.clone(),
        Some(request.max_tokens as i64),
        Some(CacheRetention::None),
        false,
    );
    let pi_context = PiAiContext {
        system_prompt: None,
        messages,
        tools: None,
    };
    let message = models
        .complete_simple(
            &model,
            &pi_context,
            Some(ModelsSimpleStreamOptions {
                base: options,
                transform_headers: None,
            }),
        )
        .await;
    signal.throw_if_aborted()?;

    let summary = summary_text(&message);
    let settings = runtime.settings()?;
    let policy = settings.retry;
    let should_retry = message.stop_reason == StopReason::Error
        && is_retryable_assistant_error(&message)
        && policy.enabled
        && request.attempt <= policy.max_retries;
    let until = if should_retry {
        let ai_policy = PiAiRetryPolicy {
            enabled: policy.enabled,
            max_retries: policy.max_retries,
            base_delay_ms: policy.base_delay_ms,
            max_agent_delay_ms: policy.max_agent_delay_ms,
        };
        runtime.now()? + retry_delay_ms(&ai_policy, request.attempt) as i64
    } else {
        0
    };

    let task_id = runtime.task_id()?.erased();
    let now_commit = runtime.now()?;
    let queue_modes = QueueModes::from(&settings);
    runtime
        .commit(
            move |tx, current| {
                Box::pin(async move {
                    let usage_key = format!("{}/{}", message.provider, message.model);
                    record_usage(&tx, conversation_id, UsageBucket::Models, &usage_key, &message.usage).await?;
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    let mut live = live_draft.get()?;
                    if let Some(text) = summary {
                        let next = place_summary(
                            &tx,
                            conversation_id,
                            task_id,
                            &current,
                            &mut live,
                            request.first_kept,
                            text,
                            now_commit,
                            queue_modes,
                        )
                        .await?;
                        live_draft.replace(live)?;
                        return Ok(Some(next));
                    }
                    if should_retry {
                        if let Some(status) = compaction_status_mut(&mut live, task_id) {
                            status.retry = Some(DurableRetryState {
                                at: until,
                                error: message.error_message.clone().unwrap_or_default(),
                            });
                        }
                        live_draft.replace(live)?;
                        return Ok(Some(NextTaskState::Running {
                            checkpoint: CompactionCheckpoint::Retry { until, request },
                        }));
                    }
                    remove_compaction_status(&mut live, task_id);
                    live_draft.replace(live)?;
                    let msg = summary_failure(&message);
                    let mut detail_map = serde_json::Map::new();
                    detail_map.insert("reason".to_owned(), JsonValue::String("model_error".to_owned()));
                    Ok(Some(NextTaskState::Terminal {
                        outcome: TaskOutcome::Failed {
                            error: TaskOutcomeError {
                                message: msg,
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

async fn phase_retry(
    task: RunningTask<CompactionInput, CompactionCheckpoint, CompactionResult>,
    runtime: Runtime,
    context: Context,
) -> pi_js::Result<()> {
    let CompactionCheckpoint::Retry { until, mut request } = task.checkpoint else {
        return Err(pi_js::Error::msg("Invalid compaction checkpoint in retry phase"));
    };
    runtime.sleep(until, context.clone()).await?;
    let next_attempt = request.attempt + 1;
    request.attempt = next_attempt;

    let conversation_id = runtime.conversation_id()?;
    let task_id = runtime.task_id()?.erased();
    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    live_draft.update(|live| {
                        if let Some(status) = compaction_status_mut(live, task_id) {
                            status.attempt = next_attempt;
                            status.retry = None;
                        }
                    })?;
                    Ok(Some(NextTaskState::Running {
                        checkpoint: CompactionCheckpoint::Summarize { request },
                    }))
                })
            },
            context,
        )
        .await
}

async fn phase_abort(runtime: Runtime, context: Context) -> pi_js::Result<()> {
    let conversation_id = runtime.conversation_id()?;
    let task_id = runtime.task_id()?.erased();
    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    live_draft.update(|live| {
                        remove_compaction_status(live, task_id);
                    })?;
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

/// Create a compaction task with its status in this commit. `owner` is the generation that waits for it
/// (a blocking compaction); without one it is conversation-owned, and `background` unless it is manual.
pub async fn create_compaction(
    tx: &Tx,
    conversation_id: ConversationId,
    input: CompactionInput,
    owner: Option<TaskId>,
) -> pi_js::Result<TaskId<CompactionResult>> {
    let ownership = match owner {
        None => TaskOwnership::Conversation,
        Some(task_id) => TaskOwnership::Task { task_id },
    };
    let background = owner.is_none() && input.reason != CompactionReason::Manual;
    let task_id = tx
        .create_task(
            &*COMPACTION_TASK,
            &input,
            TaskOptions {
                ownership,
                conversation_id: Some(conversation_id),
                background,
            },
        )
        .await?;
    let status = crate::harness::live::CompactionStatus {
        task_id,
        reason: input.reason,
        blocking: owner.is_some(),
        attempt: 1,
        retry: None,
    };
    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
    live_draft.update(|live| {
        add_compaction_status(live, status);
    })?;
    Ok(task_id)
}

/// Index in `view.entries` of the first entry a summary keeps, or `None` when there is nothing to compact (spec §8.7).
/// Walks back from the tail until `keep_recent_tokens` are kept, then cuts at the first candidate at or after that
/// entry: an entry whose contribution starts with a user or assistant message, never a tool result, and never a user
/// entry that a result of the preceding assistant's calls still follows.
pub fn select_cut(view: &ContextView, keep_recent_tokens: u64) -> Option<usize> {
    let contributions = &view.contributions;
    let start = if view.head.is_none() { 0 } else { 1 };
    let mut candidates: Vec<usize> = Vec::new();
    for index in start..contributions.len() {
        if is_candidate(contributions, index) {
            candidates.push(index);
        }
    }
    let mut kept = 0i64;
    let mut cut: Option<usize> = None;
    for index in (start..contributions.len()).rev() {
        for message in &contributions[index] {
            kept += estimate_message_tokens(message);
        }
        if (kept as u64) < keep_recent_tokens {
            continue;
        }
        cut = candidates
            .iter()
            .copied()
            .find(|&candidate| candidate >= index)
            .or_else(|| candidates.last().copied());
        break;
    }
    let cut = cut?;
    if (start..cut).any(|index| !contributions[index].is_empty()) {
        Some(cut)
    } else {
        None
    }
}

fn is_candidate(contributions: &[Vec<Message>], index: usize) -> bool {
    let Some(first) = contributions[index].first() else {
        return false;
    };
    if matches!(first, Message::Assistant(_)) {
        return true;
    }
    if !matches!(first, Message::User(_)) {
        return false;
    }
    // A result of the preceding assistant's calls that follows this entry, before the next assistant, belongs before it.
    let mut calls: IndexSet<&str> = IndexSet::new();
    for before in (0..index).rev() {
        let Some(assistant) = contributions[before].iter().rev().find_map(|msg| match msg {
            Message::Assistant(a) => Some(a),
            _ => None,
        }) else {
            continue;
        };
        for block in &assistant.content {
            if let AssistantContentBlock::ToolCall(tc) = block {
                calls.insert(tc.id.as_str());
            }
        }
        break;
    }
    if calls.is_empty() {
        return true;
    }
    for after in index..contributions.len() {
        for (position, message) in contributions[after].iter().enumerate() {
            if matches!(message, Message::Assistant(_)) && (after > index || position > 0) {
                return true;
            }
            if let Message::ToolResult(tr) = message
                && calls.contains(tr.tool_call_id.as_str())
            {
                return false;
            }
        }
    }
    true
}

/// Model messages of the entries before `cut`: the head marker first, ordered like model context (spec §2.1).
pub fn summarized_messages(view: &ContextView, cut: usize) -> Vec<Message> {
    let flat: Vec<Message> = view.contributions[..cut].iter().flatten().cloned().collect();
    order_tool_results(&flat)
}

/// Size of a request over `view` followed by `extra` (spec §8.3): the usage of the newest assistant appended
/// after the head marker, whose request included the marker, plus estimates of the messages after it; without one,
/// estimates of every message.
pub fn estimate_context(view: &ContextView, extra: &[Message]) -> i64 {
    let mut measured: Option<&AssistantMessage> = None;
    let after = view.head.as_ref().map(|h| h.id.as_i64());
    for index in (0..view.entries.len()).rev() {
        if let Some(after_id) = after
            && view.entries[index].id.as_i64() <= after_id
        {
            continue;
        }
        measured = view.contributions[index]
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::Assistant(a) if calculate_context_tokens(&a.usage) > 0 => Some(a),
                _ => None,
            });
        if measured.is_some() {
            break;
        }
    }
    let from = match measured {
        None => 0,
        Some(target) => view
            .messages
            .iter()
            .rposition(|message| matches!(message, Message::Assistant(a) if a == target))
            .map_or(0, |idx| idx + 1),
    };
    let mut tokens = measured.map_or(0, |m| calculate_context_tokens(&m.usage));
    for message in &view.messages[from..] {
        tokens += estimate_message_tokens(message);
    }
    for message in extra {
        tokens += estimate_message_tokens(message);
    }
    tokens
}

/// The summary of a clean `stop` with text and no tool call; anything else is not a summary.
fn summary_text(message: &AssistantMessage) -> Option<String> {
    if message.stop_reason != StopReason::Stop
        || message
            .content
            .iter()
            .any(|content| matches!(content, AssistantContentBlock::ToolCall(_)))
    {
        return None;
    }
    let parts: Vec<&str> = message
        .content
        .iter()
        .filter_map(|content| match content {
            AssistantContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    let joined = parts.join("\n");
    let text = pi_js::str16::trim(&joined);
    if text.is_empty() { None } else { Some(text.to_owned()) }
}

fn summary_failure(message: &AssistantMessage) -> String {
    if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
        let detail = message.error_message.as_deref().unwrap_or(message.stop_reason.as_str());
        return format!("Summarization failed: {detail}");
    }
    if message.stop_reason == StopReason::Length {
        return "Summarization hit the token limit; the summary is incomplete".to_owned();
    }
    if message
        .content
        .iter()
        .any(|content| matches!(content, AssistantContentBlock::ToolCall(_)))
    {
        return "Summarization attempted to call a tool".to_owned();
    }
    "Summarization produced no text".to_owned()
}

/// The summarizer's user message: the serialized conversation, the prompt, and any instructions.
fn summary_prompt(messages: &[Message], instructions: Option<&str>) -> String {
    let focus = match instructions {
        None => String::new(),
        Some(text) => format!("\n\nAdditional focus: {text}"),
    };
    format!(
        "<conversation>\n{}\n</conversation>\n\n{}{}",
        serialize_conversation(messages),
        SUMMARIZATION_PROMPT,
        focus
    )
}

/// Messages as plain text, so the summarizer reads a transcript instead of continuing it. System messages are omitted.
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for message in messages {
        match message {
            Message::User(user) => {
                let text = user_content_text(&user.content);
                if !text.is_empty() {
                    parts.push(format!("[User]: {text}"));
                }
            }
            Message::Assistant(assistant) => {
                let mut thinking = Vec::new();
                let mut text = Vec::new();
                let mut calls = Vec::new();
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Thinking(t) => thinking.push(t.thinking.as_str()),
                        AssistantContentBlock::Text(t) => text.push(t.text.as_str()),
                        AssistantContentBlock::ToolCall(tc) => {
                            let args = tc
                                .arguments
                                .iter()
                                .map(|(key, value)| format!("{key}={}", pi_js::json::stringify(value)))
                                .collect::<Vec<_>>()
                                .join(", ");
                            calls.push(format!("{}({args})", tc.name));
                        }
                    }
                }
                if !thinking.is_empty() {
                    parts.push(format!("[Assistant thinking]: {}", thinking.join("\n")));
                }
                if !text.is_empty() {
                    parts.push(format!("[Assistant]: {}", text.join("\n")));
                }
                if !calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", calls.join("; ")));
                }
            }
            Message::ToolResult(tr) => {
                let text = blocks_content_text(&tr.content);
                if !text.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_chars(&text, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
            Message::System(_) => {}
        }
    }
    parts.join("\n\n")
}

fn user_content_text(content: &UserMessageContent) -> String {
    match content {
        UserMessageContent::Text(s) => s.clone(),
        UserMessageContent::Blocks(blocks) => blocks_content_text(blocks),
    }
}

fn blocks_content_text(blocks: &[UserContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let total = pi_js::str16::len(text);
    if total <= max_chars {
        return text.to_owned();
    }
    let head = pi_js::str16::slice(text, 0, Some(max_chars as i64));
    format!("{head}\n\n[... {} more characters truncated]", total - max_chars)
}

async fn place(runtime: &Runtime, first_kept: EntryId, summary: String, context: Context) -> pi_js::Result<()> {
    let conversation_id = runtime.conversation_id()?;
    let task_id = runtime.task_id()?.erased();
    let now = runtime.now()?;
    let queue_modes = QueueModes::from(&runtime.settings()?);
    runtime
        .commit(
            move |tx, current| {
                Box::pin(async move {
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    let mut live = live_draft.get()?;
                    let next = place_summary(
                        &tx,
                        conversation_id,
                        task_id,
                        &current,
                        &mut live,
                        first_kept,
                        summary,
                        now,
                        queue_modes,
                    )
                    .await?;
                    live_draft.replace(live)?;
                    Ok(Some(next))
                })
            },
            context,
        )
        .await
}

#[allow(clippy::too_many_arguments)]
async fn place_summary(
    tx: &Tx,
    conversation_id: ConversationId,
    task_id: TaskId,
    current: &RunningTask<CompactionInput, CompactionCheckpoint, CompactionResult>,
    live: &mut crate::harness::live::LiveState,
    first_kept: EntryId,
    summary: String,
    now: i64,
    queue_modes: QueueModes,
) -> pi_js::Result<NextTaskState<CompactionCheckpoint, CompactionResult>> {
    remove_compaction_status(live, task_id);
    let text = format!("{SUMMARY_PREFIX}{summary}{SUMMARY_SUFFIX}");
    let message = Message::User(UserMessage {
        content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
        timestamp: now,
    });
    let data = serde_json::to_value(crate::entries::CompactionEntryData {
        reason: current.input.reason,
    })
    .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
    let entry = EntryDraft {
        kind: COMPACTION_ENTRY.kind().to_owned(),
        model: Some(vec![message]),
        data: Some(data),
        head: Some(EntryDraftHead::Entry(first_kept)),
        edits: None,
    };

    let result = if current.owner.is_none() {
        let submission_id = admit_submission(
            tx,
            conversation_id,
            SubmissionDraft::Write {
                request_id: Some(format!("compaction:{task_id}")),
                entry,
            },
            now,
            queue_modes,
        )
        .await?;
        CompactionResult {
            entry_id: None,
            submission_id: Some(submission_id),
        }
    } else {
        let appended = tx.append_entry(conversation_id, entry).await?;
        CompactionResult {
            entry_id: Some(appended.id),
            submission_id: None,
        }
    };

    Ok(NextTaskState::Terminal {
        outcome: TaskOutcome::Completed { result },
    })
}

async fn complete_empty(runtime: &Runtime, context: Context) -> pi_js::Result<()> {
    let conversation_id = runtime.conversation_id()?;
    let task_id = runtime.task_id()?.erased();
    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    live_draft.update(|live| {
                        remove_compaction_status(live, task_id);
                    })?;
                    Ok(Some(NextTaskState::Terminal {
                        outcome: TaskOutcome::Completed {
                            result: CompactionResult::default(),
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
    let task_id = runtime.task_id()?.erased();
    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    live_draft.update(|live| {
                        remove_compaction_status(live, task_id);
                    })?;
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
