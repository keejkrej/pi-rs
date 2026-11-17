//! Context compaction for long sessions.
//!
//! Pure functions for compaction logic. The session manager handles I/O,
//! and after compaction the session is reloaded.
//!
//! Port of `packages/coding-agent/src/core/compaction/compaction.ts`.

use std::sync::Arc;

use indexmap::IndexSet;
use pi_agent_core::{AgentMessage, StreamFn, ThinkingLevel};
use pi_ai::compat::complete_simple;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, CacheRetention, Context, Message, Model, ProviderEnv, ProviderHeaders,
    RetryCallbacks, RetryPolicy, SimpleStreamOptions, StopReason, TranscriptContext, Usage, UserContentBlock,
    UserMessage, UserMessageContent, content_text, get_current_system_message, get_system_message_text,
    normalize_context, retry_assistant_call, uuidv7, without_initial_system_message,
};
use pi_js::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::messages::convert_to_llm;
use crate::core::session_manager::{
    CompactionEntry, ProjectedSessionEntry, SessionEntry, SessionProjection, build_session_projection,
    session_entry_to_context_messages,
};
use crate::core::usage_totals::combine_usage;

use super::utils::{
    FileOperations, SUMMARIZATION_SYSTEM_PROMPT, compute_file_lists, create_file_ops, extract_file_ops_from_message,
    format_file_operations, serialize_conversation,
};

// ============================================================================
// File Operation Tracking
// ============================================================================

/// Details stored in `CompactionEntry.details` for file tracking.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

/// Extract file operations from messages and previous compaction entries.
fn extract_file_operations(
    messages: &[AgentMessage],
    entries: &[SessionEntry],
    prev_compaction_index: Option<usize>,
) -> FileOperations {
    let mut file_ops = create_file_ops();

    // Collect from previous compaction's details (if pi-generated)
    if let Some(idx) = prev_compaction_index
        && let Some(SessionEntry::Compaction(prev_compaction)) = entries.get(idx)
        && !prev_compaction.from_hook.unwrap_or(false)
        && let Some(details_val) = &prev_compaction.details
        && let Ok(details) = serde_json::from_value::<CompactionDetails>(details_val.clone())
    {
        for f in details.read_files {
            file_ops.read.insert(f);
        }
        for f in details.modified_files {
            file_ops.edited.insert(f);
        }
    }

    for msg in messages {
        extract_file_ops_from_message(msg, &mut file_ops);
    }

    file_ops
}

// ============================================================================
// Message Extraction
// ============================================================================

/// Extract LLM-summarizable messages from a projected session entry.
fn get_messages_from_projected_entry_for_compaction(entry: &ProjectedSessionEntry) -> Vec<AgentMessage> {
    if matches!(entry.source_entry, SessionEntry::Compaction(_)) {
        return Vec::new();
    }
    // System messages are prompt state, not conversation; the compaction entry carries their replay.
    entry
        .messages
        .iter()
        .filter(|message| message.role() != Some("system"))
        .cloned()
        .collect()
}

/// Result from `compact()` — `SessionManager` adds id/parentId when saving.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResult<T = Value> {
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_tokens_after: Option<i64>,
    /// Usage from the LLM call(s) that generated this summary, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Extension-specific data (e.g. `CompactionDetails`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<T>,
}

// ============================================================================
// Types
// ============================================================================

/// Resolved compaction thresholds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: i64,
    pub keep_recent_tokens: i64,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        DEFAULT_COMPACTION_SETTINGS
    }
}

pub const DEFAULT_COMPACTION_SETTINGS: CompactionSettings = CompactionSettings {
    enabled: true,
    reserve_tokens: 16384,
    keep_recent_tokens: 20000,
};

// ============================================================================
// Token Calculation
// ============================================================================

/// Calculate total context tokens from usage.
/// Uses the native `total_tokens` field when non-zero, falling back to summing components.
pub fn calculate_context_tokens(usage: &Usage) -> i64 {
    if usage.total_tokens != 0 {
        usage.total_tokens
    } else {
        usage.input + usage.output + usage.cache_read + usage.cache_write
    }
}

/// Get usage from an assistant message if valid (not aborted/error and non-zero tokens).
fn get_assistant_usage(msg: &AgentMessage) -> Option<&Usage> {
    let AgentMessage::Message(Message::Assistant(assistant)) = msg else {
        return None;
    };
    if assistant.stop_reason != StopReason::Aborted
        && assistant.stop_reason != StopReason::Error
        && calculate_context_tokens(&assistant.usage) > 0
    {
        Some(&assistant.usage)
    } else {
        None
    }
}

/// Find the last valid assistant message usage from session entries.
pub fn get_last_assistant_usage(entries: &[SessionEntry]) -> Option<Usage> {
    for entry in entries.iter().rev() {
        if let SessionEntry::Message(msg_entry) = entry
            && let Some(usage) = get_assistant_usage(&msg_entry.message)
        {
            return Some(*usage);
        }
    }
    None
}

/// Result of estimating total context token usage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsageEstimate {
    pub tokens: i64,
    pub usage_tokens: i64,
    pub trailing_tokens: i64,
    pub last_usage_index: Option<usize>,
}

fn get_last_assistant_usage_info(messages: &[AgentMessage]) -> Option<(&Usage, usize)> {
    for (i, msg) in messages.iter().enumerate().rev() {
        if let Some(usage) = get_assistant_usage(msg) {
            return Some((usage, i));
        }
    }
    None
}

/// Estimate context tokens from messages, using the last assistant usage when available.
/// If there are messages after the last usage, estimate their tokens with `estimate_tokens`.
pub fn estimate_context_tokens(messages: &[AgentMessage]) -> ContextUsageEstimate {
    let Some((usage, usage_index)) = get_last_assistant_usage_info(messages) else {
        let mut estimated = 0i64;
        for message in messages {
            estimated += estimate_tokens(message);
        }
        return ContextUsageEstimate {
            tokens: estimated,
            usage_tokens: 0,
            trailing_tokens: estimated,
            last_usage_index: None,
        };
    };

    let usage_tokens = calculate_context_tokens(usage);
    let mut trailing_tokens = 0i64;
    for message in &messages[usage_index + 1..] {
        trailing_tokens += estimate_tokens(message);
    }

    ContextUsageEstimate {
        tokens: usage_tokens + trailing_tokens,
        usage_tokens,
        trailing_tokens,
        last_usage_index: Some(usage_index),
    }
}

/// Estimate projected context without trusting usage captured before a later edit or compaction.
pub fn estimate_projected_context_tokens(
    projection: &SessionProjection,
    branch_entries: &[SessionEntry],
) -> ContextUsageEstimate {
    let estimate = estimate_context_tokens(&projection.messages);
    if let Some(last_usage_index) = estimate.last_usage_index {
        let mut projected_message_index = 0usize;
        let mut usage_entry_id: Option<&str> = None;
        for entry in &projection.entries {
            let next_message_index = projected_message_index + entry.messages.len();
            if last_usage_index < next_message_index {
                usage_entry_id = Some(entry.source_entry.id());
                break;
            }
            projected_message_index = next_message_index;
        }

        let usage_entry_index = usage_entry_id
            .and_then(|id| branch_entries.iter().position(|entry| entry.id() == id))
            .map(|idx| idx as isize)
            .unwrap_or(-1);
        let mut latest_invalidating_entry_index: isize = -1;
        for (i, entry) in branch_entries.iter().enumerate().rev() {
            if matches!(entry, SessionEntry::ContextEdit(_) | SessionEntry::Compaction(_)) {
                latest_invalidating_entry_index = i as isize;
                break;
            }
        }
        if usage_entry_index > latest_invalidating_entry_index {
            return estimate;
        }
    }

    let sys_messages: Vec<&Message> = projection
        .messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::Message(msg @ Message::System(_)) => Some(msg),
            _ => None,
        })
        .collect();
    let current_system = get_current_system_message(&sys_messages);
    let mut tokens = current_system
        .map(|sys| estimate_tokens(&AgentMessage::Message(Message::System(sys.clone()))))
        .unwrap_or(0);
    for message in &projection.messages {
        if message.role() != Some("system") {
            tokens += estimate_tokens(message);
        }
    }
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}

/// Check if compaction should trigger based on context usage.
pub fn should_compact(context_tokens: i64, context_window: i64, settings: &CompactionSettings) -> bool {
    if !settings.enabled {
        return false;
    }
    context_tokens > context_window - settings.reserve_tokens
}

// ============================================================================
// Cut Point Detection
// ============================================================================

const ESTIMATED_IMAGE_CHARS: usize = 4800;

fn estimate_user_blocks_chars(blocks: &[UserContentBlock]) -> usize {
    let mut chars = 0usize;
    for block in blocks {
        match block {
            UserContentBlock::Text(text) => {
                chars += pi_js::str16::len(&text.text);
            }
            UserContentBlock::Image(_) => {
                chars += ESTIMATED_IMAGE_CHARS;
            }
        }
    }
    chars
}

fn estimate_json_content_chars(content: &Value) -> usize {
    match content {
        Value::String(s) => pi_js::str16::len(s),
        Value::Array(items) => {
            let mut chars = 0usize;
            for item in items {
                let Some(block_type) = item.get("type").and_then(Value::as_str) else {
                    continue;
                };
                if block_type == "text" {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        chars += pi_js::str16::len(text);
                    }
                } else if block_type == "image" {
                    chars += ESTIMATED_IMAGE_CHARS;
                }
            }
            chars
        }
        _ => 0,
    }
}

/// Estimate token count for a message using the `chars / 4` ceiling heuristic.
pub fn estimate_tokens(message: &AgentMessage) -> i64 {
    let chars: usize = match message {
        AgentMessage::Message(msg) => match msg {
            Message::System(system) => {
                let mut total = match &system.content {
                    pi_ai::SystemMessageContent::Text(s) => pi_js::str16::len(s),
                    pi_ai::SystemMessageContent::Blocks(blocks) => {
                        blocks.iter().map(|b| pi_js::str16::len(&b.text)).sum()
                    }
                };
                if let Some(sections) = &system.sections {
                    for section in sections.values().flatten() {
                        total += pi_js::str16::len(section);
                    }
                }
                if let Some(tools_added) = &system.tools_added {
                    total += pi_js::str16::len(&pi_js::json::stringify(tools_added));
                }
                total
            }
            Message::User(user) => match &user.content {
                UserMessageContent::Text(s) => pi_js::str16::len(s),
                UserMessageContent::Blocks(blocks) => estimate_user_blocks_chars(blocks),
            },
            Message::Assistant(assistant) => {
                let mut total = 0usize;
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Text(text) => {
                            total += pi_js::str16::len(&text.text);
                        }
                        AssistantContentBlock::Thinking(thinking) => {
                            total += pi_js::str16::len(&thinking.thinking);
                        }
                        AssistantContentBlock::ToolCall(tc) => {
                            total +=
                                pi_js::str16::len(&tc.name) + pi_js::str16::len(&pi_js::json::stringify(&tc.arguments));
                        }
                    }
                }
                total
            }
            Message::ToolResult(tool_result) => estimate_user_blocks_chars(&tool_result.content),
        },
        AgentMessage::Custom(value) => {
            let role = value.get("role").and_then(Value::as_str).unwrap_or("");
            match role {
                "custom" => value.get("content").map(estimate_json_content_chars).unwrap_or(0),
                "bashExecution" => {
                    let cmd_len = value
                        .get("command")
                        .and_then(Value::as_str)
                        .map(pi_js::str16::len)
                        .unwrap_or(0);
                    let out_len = value
                        .get("output")
                        .and_then(Value::as_str)
                        .map(pi_js::str16::len)
                        .unwrap_or(0);
                    cmd_len + out_len
                }
                "branchSummary" | "compactionSummary" => value
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(pi_js::str16::len)
                    .unwrap_or(0),
                _ => 0,
            }
        }
    };

    chars.div_ceil(4) as i64
}

fn is_cut_point_message(message: &AgentMessage) -> bool {
    matches!(
        message.role(),
        Some("user" | "assistant" | "bashExecution" | "custom" | "branchSummary" | "compactionSummary")
    )
}

fn is_turn_start_message(message: &AgentMessage) -> bool {
    matches!(
        message.role(),
        Some("user" | "bashExecution" | "custom" | "branchSummary" | "compactionSummary")
    )
}

fn is_turn_start_entry(entry: &SessionEntry) -> bool {
    if matches!(entry, SessionEntry::Compaction(_)) {
        return false;
    }
    session_entry_to_context_messages(entry)
        .iter()
        .any(is_turn_start_message)
}

fn find_valid_cut_points(entries: &[SessionEntry], start_index: usize, end_index: usize) -> Vec<usize> {
    let mut cut_points = Vec::new();
    let end = end_index.min(entries.len());
    for (i, entry) in entries.iter().enumerate().take(end).skip(start_index) {
        if matches!(entry, SessionEntry::Compaction(_)) {
            continue;
        }
        if session_entry_to_context_messages(entry)
            .iter()
            .any(is_cut_point_message)
        {
            cut_points.push(i);
        }
    }
    cut_points
}

/// Find the context-visible user-role message that starts the turn containing `entry_index`.
/// Returns `None` if no turn start is found before the index (TS returns `-1`).
pub fn find_turn_start_index(entries: &[SessionEntry], entry_index: usize, start_index: usize) -> Option<usize> {
    if entries.is_empty() || start_index > entry_index {
        return None;
    }
    let end = entry_index.min(entries.len() - 1);
    for i in (start_index..=end).rev() {
        if is_turn_start_entry(&entries[i]) {
            return Some(i);
        }
    }
    None
}

/// Cut point metadata computed by [`find_cut_point`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutPointResult {
    /// Index of first entry to keep.
    pub first_kept_entry_index: usize,
    /// Index of user message that starts the turn being split, or `None` if not splitting.
    pub turn_start_index: Option<usize>,
    /// Whether this cut splits a turn (cut point is not a user message).
    pub is_split_turn: bool,
}

/// Find the cut point in session entries that keeps approximately `keep_recent_tokens`.
pub fn find_cut_point(
    entries: &[SessionEntry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: i64,
) -> CutPointResult {
    let cut_points = find_valid_cut_points(entries, start_index, end_index);
    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: None,
            is_split_turn: false,
        };
    }

    let mut accumulated_tokens = 0i64;
    let mut cut_index = cut_points[0];
    let end = end_index.min(entries.len());

    for i in (start_index..end).rev() {
        let message_tokens: i64 = session_entry_to_context_messages(&entries[i])
            .iter()
            .map(estimate_tokens)
            .sum();
        if message_tokens == 0 {
            continue;
        }
        accumulated_tokens += message_tokens;
        if accumulated_tokens >= keep_recent_tokens {
            cut_index = cut_points
                .iter()
                .copied()
                .find(|&candidate| candidate >= i)
                .unwrap_or_else(|| cut_points[cut_points.len() - 1]);
            break;
        }
    }

    // Scan backwards from cutIndex to include adjacent metadata entries that do not affect context.
    while cut_index > start_index {
        let prev_entry = &entries[cut_index - 1];
        if matches!(prev_entry, SessionEntry::Compaction(_))
            || !session_entry_to_context_messages(prev_entry).is_empty()
        {
            break;
        }
        cut_index -= 1;
    }

    let starts_turn = is_turn_start_entry(&entries[cut_index]);
    let turn_start_index = if starts_turn {
        None
    } else {
        find_turn_start_index(entries, cut_index, start_index)
    };

    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index.is_some(),
    }
}

// ============================================================================
// Summarization
// ============================================================================

const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

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

const UPDATE_SUMMARIZATION_INSTRUCTIONS: &str = "Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed
- UPDATE \"Next Steps\" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

fn update_summarization_prompt() -> String {
    format!(
        "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.\n\n{UPDATE_SUMMARIZATION_INSTRUCTIONS}"
    )
}

/// Returns an error message when a summarization response cannot safely be persisted.
/// A length stop contains partial text and must not become a session checkpoint.
pub fn get_summarization_failure(response: &AssistantMessage, label: &str) -> Option<String> {
    if response.stop_reason == StopReason::Error {
        let msg = response
            .error_message
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("Unknown error");
        return Some(format!("{label} failed: {msg}"));
    }
    if response.stop_reason == StopReason::Length {
        return Some(format!(
            "{label} failed: generation hit the token cap and the summary is incomplete"
        ));
    }
    None
}

fn create_summarization_options(
    model: &Model,
    max_tokens: i64,
    api_key: Option<String>,
    headers: Option<ProviderHeaders>,
    env: Option<ProviderEnv>,
    signal: Option<pi_js::abort::AbortSignal>,
    thinking_level: Option<ThinkingLevel>,
    session_id: Option<String>,
) -> SimpleStreamOptions {
    let mut options = SimpleStreamOptions::default();
    options.base.max_tokens = Some(max_tokens);
    options.base.signal = signal;
    options.base.api_key = api_key;
    options.base.headers = headers;
    options.base.env = env;
    options.base.session_id = session_id;
    if model.reasoning
        && let Some(level) = thinking_level
    {
        options.reasoning = level.to_ai_thinking_level();
    }
    options
}

/// Adapter so callers of [`complete_summarization`] can pass either `&Context` or `&TranscriptContext`.
pub trait SummarizationContext {
    fn to_context(&self) -> Context;
}

impl SummarizationContext for Context {
    fn to_context(&self) -> Context {
        self.clone()
    }
}

impl SummarizationContext for TranscriptContext {
    fn to_context(&self) -> Context {
        match self.messages.first() {
            Some(Message::System(sys)) => Context {
                system_prompt: Some(get_system_message_text(sys)),
                messages: without_initial_system_message(&self.messages),
                tools: sys.tools_added.clone(),
            },
            _ => Context {
                system_prompt: None,
                messages: self.messages.clone(),
                tools: None,
            },
        }
    }
}

/// Shared choke point for every compaction/branch-summary summarization call. Wraps the
/// single LLM call in `retry_assistant_call` so transient stream drops honor the configured retry policy.
pub async fn complete_summarization<C: SummarizationContext + ?Sized>(
    model: &Model,
    context: &C,
    mut options: SimpleStreamOptions,
    stream_fn: Option<&StreamFn>,
    retry: Option<&RetryPolicy>,
    callbacks: Option<&dyn RetryCallbacks>,
) -> Result<AssistantMessage> {
    options.base.cache_retention = Some(CacheRetention::None);
    if options.base.session_id.is_none() {
        options.base.session_id = Some(uuidv7(None)?);
    }

    let ctx = context.to_context();
    let _normalized: TranscriptContext = normalize_context(&ctx);
    let signal = options.base.signal.clone();
    let produce = || {
        let model = model.clone();
        let ctx = ctx.clone();
        let options = options.clone();
        let stream_fn = stream_fn.cloned();
        async move {
            if let Some(sf) = stream_fn {
                let stream = sf(model, ctx, Some(options)).await?;
                Ok(stream.result().await)
            } else {
                Ok(complete_simple(&model, &ctx, Some(options)).await)
            }
        }
    };

    retry_assistant_call(produce, retry, signal.as_ref(), callbacks).await
}

/// Optional settings for [`generate_summary`] and [`generate_summary_with_usage`].
#[derive(Clone, Default)]
pub struct GenerateSummaryOptions {
    pub api_key: Option<String>,
    pub headers: Option<ProviderHeaders>,
    pub signal: Option<pi_js::abort::AbortSignal>,
    pub custom_instructions: Option<String>,
    pub previous_summary: Option<String>,
    pub thinking_level: Option<ThinkingLevel>,
    pub stream_fn: Option<StreamFn>,
    pub env: Option<ProviderEnv>,
    pub retry: Option<RetryPolicy>,
    pub callbacks: Option<Arc<dyn RetryCallbacks>>,
    pub session_id: Option<String>,
}

/// Text and token/cost usage returned by [`generate_summary_with_usage`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryOutput {
    pub text: String,
    pub usage: Usage,
}

fn clamp_summary_max_tokens(budget: i64, model_max_tokens: i64) -> i64 {
    if model_max_tokens > 0 {
        budget.min(model_max_tokens)
    } else {
        budget
    }
}

/// Build the provider context for a standalone summary request.
fn build_summarization_context(prompt_text: String) -> Context {
    Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text(prompt_text),
            timestamp: pi_js::time::now_ms(),
        })],
        tools: None,
    }
}

/// Generate a summary of the conversation using the LLM.
/// If `previous_summary` is provided, uses the update prompt to merge.
pub async fn generate_summary(
    current_messages: &[AgentMessage],
    model: &Model,
    reserve_tokens: i64,
    options: Option<GenerateSummaryOptions>,
) -> Result<String> {
    Ok(
        generate_summary_with_usage(current_messages, model, reserve_tokens, options)
            .await?
            .text,
    )
}

/// Generate or update a conversation summary and return its provider usage.
pub async fn generate_summary_with_usage(
    current_messages: &[AgentMessage],
    model: &Model,
    reserve_tokens: i64,
    options: Option<GenerateSummaryOptions>,
) -> Result<SummaryOutput> {
    let opts = options.unwrap_or_default();
    let max_tokens = clamp_summary_max_tokens(((0.8 * reserve_tokens as f64).floor()) as i64, model.max_tokens);

    let mut base_prompt = if opts.previous_summary.is_some() {
        update_summarization_prompt()
    } else {
        SUMMARIZATION_PROMPT.to_owned()
    };
    if let Some(custom) = &opts.custom_instructions
        && !custom.is_empty()
    {
        base_prompt.push_str("\n\nAdditional focus: ");
        base_prompt.push_str(custom);
    }

    let llm_messages = convert_to_llm(current_messages);
    let conversation_text = serialize_conversation(&llm_messages);

    let mut prompt_text = format!("<conversation>\n{conversation_text}\n</conversation>\n\n");
    if let Some(prev) = &opts.previous_summary
        && !prev.is_empty()
    {
        prompt_text.push_str(&format!("<previous-summary>\n{prev}\n</previous-summary>\n\n"));
    }
    prompt_text.push_str(&base_prompt);

    let completion_options = create_summarization_options(
        model,
        max_tokens,
        opts.api_key,
        opts.headers,
        opts.env,
        opts.signal,
        opts.thinking_level,
        opts.session_id,
    );

    let ctx = build_summarization_context(prompt_text);
    let response = complete_summarization(
        model,
        &ctx,
        completion_options,
        opts.stream_fn.as_ref(),
        opts.retry.as_ref(),
        opts.callbacks.as_deref(),
    )
    .await?;

    if response.stop_reason == StopReason::Aborted {
        return Err(Error::Abort(pi_js::abort::AbortReason::abort()));
    }
    if let Some(failure) = get_summarization_failure(&response, "Summarization") {
        return Err(Error::msg(failure));
    }
    if response
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
    {
        return Err(Error::msg("Summarization attempted to call a tool"));
    }

    Ok(SummaryOutput {
        text: content_text(&response.content, None),
        usage: response.usage,
    })
}

// ============================================================================
// Compaction Preparation (for extensions)
// ============================================================================

/// Prepared inputs needed to summarize older turns and retain recent ones.
#[derive(Clone, Debug, PartialEq)]
pub struct CompactionPreparation {
    /// ID of first entry to keep.
    pub first_kept_entry_id: String,
    /// Messages that will be summarized and discarded.
    pub messages_to_summarize: Vec<AgentMessage>,
    /// Messages that will be turned into turn prefix summary (if splitting).
    pub turn_prefix_messages: Vec<AgentMessage>,
    /// Whether this is a split turn (cut point in middle of turn).
    pub is_split_turn: bool,
    pub tokens_before: i64,
    /// Summary from previous compaction, for iterative update.
    pub previous_summary: Option<String>,
    /// File operations extracted from `messages_to_summarize`.
    pub file_ops: FileOperations,
    /// Compaction settings from `settings.jsonl`.
    pub settings: CompactionSettings,
}

fn is_projected_turn_start(entry: &ProjectedSessionEntry) -> bool {
    if matches!(entry.source_entry, SessionEntry::Compaction(_)) {
        return false;
    }
    entry.messages.iter().any(is_turn_start_message)
}

fn find_projected_turn_start_index(
    entries: &[ProjectedSessionEntry],
    entry_index: usize,
    start_index: usize,
) -> Option<usize> {
    if entries.is_empty() || start_index > entry_index {
        return None;
    }
    let end = entry_index.min(entries.len() - 1);
    for i in (start_index..=end).rev() {
        if is_projected_turn_start(&entries[i]) {
            return Some(i);
        }
    }
    None
}

fn find_projected_cut_point(
    entries: &[ProjectedSessionEntry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: i64,
) -> CutPointResult {
    let end = end_index.min(entries.len());
    let mut cut_points = Vec::new();
    for (i, entry) in entries.iter().enumerate().take(end).skip(start_index) {
        if !matches!(entry.source_entry, SessionEntry::Compaction(_)) && entry.messages.iter().any(is_cut_point_message)
        {
            cut_points.push(i);
        }
    }
    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: None,
            is_split_turn: false,
        };
    }

    let mut accumulated_tokens = 0i64;
    let mut exceeded_budget = false;
    let mut cut_index = cut_points[0];
    for i in (start_index..end).rev() {
        let message_tokens: i64 = entries[i].messages.iter().map(estimate_tokens).sum();
        if message_tokens == 0 {
            continue;
        }
        accumulated_tokens += message_tokens;
        if accumulated_tokens >= keep_recent_tokens {
            exceeded_budget = true;
            cut_index = cut_points
                .iter()
                .copied()
                .find(|&candidate| candidate >= i)
                .unwrap_or_else(|| cut_points[cut_points.len() - 1]);
            break;
        }
    }

    // A recovery attempt and its omission edits are context-invisible after the last
    // visible input. Advance only for a closed suffix containing an omitted assistant
    // attempt; arbitrary metadata must not move the cut past unsent input.
    let suffix = &entries[cut_index + 1..end];
    let is_intrinsically_visible = |entry: &ProjectedSessionEntry| -> bool {
        !matches!(entry.source_entry, SessionEntry::ContextEdit(_))
            && !session_entry_to_context_messages(&entry.source_entry).is_empty()
    };
    let is_omitted =
        |entry: &ProjectedSessionEntry| -> bool { is_intrinsically_visible(entry) && entry.messages.is_empty() };
    let omitted_suffix_ids: IndexSet<&str> = suffix
        .iter()
        .filter(|entry| is_omitted(entry))
        .map(|entry| entry.source_entry.id())
        .collect();
    let has_external_replacement = suffix.iter().any(|entry| {
        if let SessionEntry::ContextEdit(edit) = &entry.source_entry {
            edit.replacement.is_some() && !omitted_suffix_ids.contains(edit.target_id.as_str())
        } else {
            false
        }
    });
    let is_recovery_omission_suffix = exceeded_budget
        && !has_external_replacement
        && suffix.iter().any(|entry| {
            matches!(
                &entry.source_entry,
                SessionEntry::Message(m) if m.message.role() == Some("assistant")
            ) && is_omitted(entry)
        })
        && suffix.iter().all(|entry| {
            !matches!(entry.source_entry, SessionEntry::Compaction(_))
                && (!is_intrinsically_visible(entry) || is_omitted(entry))
        });
    if is_recovery_omission_suffix {
        cut_index += 1;
    }

    while cut_index > start_index {
        let previous = &entries[cut_index - 1];
        if matches!(previous.source_entry, SessionEntry::Compaction(_)) || !previous.messages.is_empty() {
            break;
        }
        cut_index -= 1;
    }

    let starts_turn = is_projected_turn_start(&entries[cut_index]);
    let turn_start_index = if starts_turn {
        None
    } else {
        find_projected_turn_start_index(entries, cut_index, start_index)
    };
    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index.is_some(),
    }
}

pub fn prepare_compaction(
    path_entries: &[SessionEntry],
    settings: &CompactionSettings,
) -> Option<CompactionPreparation> {
    if matches!(path_entries.last(), Some(SessionEntry::Compaction(_))) {
        return None;
    }

    let projection = build_session_projection(path_entries, None, None);
    let projected_entries = &projection.entries;
    let source_entries: Vec<SessionEntry> = projected_entries
        .iter()
        .map(|entry| entry.source_entry.clone())
        .collect();

    // The newest compaction is projected first. Older compaction entries can still
    // occur in its retained raw range, but their projected contribution is empty.
    let prev_compaction_index = projected_entries
        .iter()
        .position(|entry| matches!(entry.source_entry, SessionEntry::Compaction(_)) && !entry.messages.is_empty());

    let mut previous_summary: Option<String> = None;
    let mut boundary_start = 0usize;
    if let Some(idx) = prev_compaction_index
        && let SessionEntry::Compaction(CompactionEntry { summary, .. }) = &projected_entries[idx].source_entry
    {
        previous_summary = Some(summary.clone());
        boundary_start = idx + 1;
    }

    let boundary_end = projected_entries.len();
    let tokens_before = estimate_projected_context_tokens(&projection, path_entries).tokens;
    let cut_point = find_projected_cut_point(
        projected_entries,
        boundary_start,
        boundary_end,
        settings.keep_recent_tokens,
    );

    let first_kept_entry = &projected_entries.get(cut_point.first_kept_entry_index)?.source_entry;
    let first_kept_entry_id = first_kept_entry.id().to_owned();
    if first_kept_entry_id.is_empty() {
        return None;
    }

    let history_end = if cut_point.is_split_turn {
        cut_point.turn_start_index.unwrap_or(cut_point.first_kept_entry_index)
    } else {
        cut_point.first_kept_entry_index
    };

    let messages_to_summarize: Vec<AgentMessage> = projected_entries[boundary_start..history_end]
        .iter()
        .flat_map(get_messages_from_projected_entry_for_compaction)
        .collect();
    let turn_prefix_messages: Vec<AgentMessage> = if cut_point.is_split_turn {
        let turn_start = cut_point.turn_start_index.unwrap_or(cut_point.first_kept_entry_index);
        projected_entries[turn_start..cut_point.first_kept_entry_index]
            .iter()
            .flat_map(get_messages_from_projected_entry_for_compaction)
            .collect()
    } else {
        Vec::new()
    };

    if messages_to_summarize.is_empty() && turn_prefix_messages.is_empty() {
        return None;
    }

    let mut file_ops = extract_file_operations(&messages_to_summarize, &source_entries, prev_compaction_index);
    if cut_point.is_split_turn {
        for msg in &turn_prefix_messages {
            extract_file_ops_from_message(msg, &mut file_ops);
        }
    }

    Some(CompactionPreparation {
        first_kept_entry_id,
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn: cut_point.is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        settings: *settings,
    })
}

// ============================================================================
// Main Compaction Function
// ============================================================================

const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = "The messages above are earlier context from an ongoing conversation. Later messages are stored separately and do not need to be reconstructed.

Create a concise checkpoint of the user's request and the progress shown above. This checkpoint will be placed before the later messages so the conversation can continue with the necessary context.

## Original Request
[What did the user ask for?]

## Progress So Far
- [Key decisions and work completed in these messages]

## Context Needed to Continue
- [Information from these messages needed to understand the later work]

Only summarize information explicitly present above. Do not infer or recreate later messages.";

/// Optional settings for [`compact`].
#[derive(Clone, Default)]
pub struct CompactOptions {
    pub api_key: Option<String>,
    pub headers: Option<ProviderHeaders>,
    pub custom_instructions: Option<String>,
    pub signal: Option<pi_js::abort::AbortSignal>,
    pub thinking_level: Option<ThinkingLevel>,
    pub stream_fn: Option<StreamFn>,
    pub env: Option<ProviderEnv>,
    pub retry: Option<RetryPolicy>,
    pub callbacks: Option<Arc<dyn RetryCallbacks>>,
    pub session_id: Option<String>,
}

/// Generate summaries for compaction using prepared data.
/// Returns `CompactionResult<CompactionDetails>` — `SessionManager` adds id/parentId when saving.
pub async fn compact(
    preparation: &CompactionPreparation,
    model: &Model,
    options: Option<CompactOptions>,
) -> Result<CompactionResult<CompactionDetails>> {
    let opts = options.unwrap_or_default();
    let mut summary: String;
    let summary_usage: Usage;

    if preparation.is_split_turn && !preparation.turn_prefix_messages.is_empty() {
        let mut history_text = preparation
            .previous_summary
            .clone()
            .unwrap_or_else(|| "No prior history.".to_owned());
        let mut history_usage: Option<Usage> = None;

        if !preparation.messages_to_summarize.is_empty() {
            let history_result = generate_summary_with_usage(
                &preparation.messages_to_summarize,
                model,
                preparation.settings.reserve_tokens,
                Some(GenerateSummaryOptions {
                    api_key: opts.api_key.clone(),
                    headers: opts.headers.clone(),
                    signal: opts.signal.clone(),
                    custom_instructions: opts.custom_instructions.clone(),
                    previous_summary: preparation.previous_summary.clone(),
                    thinking_level: opts.thinking_level,
                    stream_fn: opts.stream_fn.clone(),
                    env: opts.env.clone(),
                    retry: opts.retry.clone(),
                    callbacks: opts.callbacks.clone(),
                    session_id: opts.session_id.clone(),
                }),
            )
            .await?;
            history_text = history_result.text;
            history_usage = Some(history_result.usage);
        }

        let turn_prefix_result = generate_turn_prefix_summary(
            &preparation.turn_prefix_messages,
            model,
            preparation.settings.reserve_tokens,
            opts.api_key,
            opts.headers,
            opts.env,
            opts.signal,
            opts.thinking_level,
            opts.stream_fn,
            opts.retry,
            opts.callbacks,
            opts.session_id,
            opts.custom_instructions,
        )
        .await?;

        summary = format!(
            "{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{}",
            turn_prefix_result.text
        );
        summary_usage = match history_usage {
            Some(prev) => combine_usage(&prev, &turn_prefix_result.usage),
            None => turn_prefix_result.usage,
        };
    } else {
        let result = generate_summary_with_usage(
            &preparation.messages_to_summarize,
            model,
            preparation.settings.reserve_tokens,
            Some(GenerateSummaryOptions {
                api_key: opts.api_key,
                headers: opts.headers,
                signal: opts.signal,
                custom_instructions: opts.custom_instructions,
                previous_summary: preparation.previous_summary.clone(),
                thinking_level: opts.thinking_level,
                stream_fn: opts.stream_fn,
                env: opts.env,
                retry: opts.retry,
                callbacks: opts.callbacks,
                session_id: opts.session_id,
            }),
        )
        .await?;
        summary = result.text;
        summary_usage = result.usage;
    }

    let lists = compute_file_lists(&preparation.file_ops);
    summary.push_str(&format_file_operations(&lists.read_files, &lists.modified_files));

    if preparation.first_kept_entry_id.is_empty() {
        return Err(Error::msg("First kept entry has no UUID - session may need migration"));
    }

    Ok(CompactionResult {
        summary,
        first_kept_entry_id: preparation.first_kept_entry_id.clone(),
        tokens_before: preparation.tokens_before,
        estimated_tokens_after: None,
        usage: Some(summary_usage),
        details: Some(CompactionDetails {
            read_files: lists.read_files,
            modified_files: lists.modified_files,
        }),
    })
}

/// Generate a summary for a turn prefix (when splitting a turn).
#[allow(clippy::too_many_arguments)]
async fn generate_turn_prefix_summary(
    messages: &[AgentMessage],
    model: &Model,
    reserve_tokens: i64,
    api_key: Option<String>,
    headers: Option<ProviderHeaders>,
    env: Option<ProviderEnv>,
    signal: Option<pi_js::abort::AbortSignal>,
    thinking_level: Option<ThinkingLevel>,
    stream_fn: Option<StreamFn>,
    retry: Option<RetryPolicy>,
    callbacks: Option<Arc<dyn RetryCallbacks>>,
    session_id: Option<String>,
    custom_instructions: Option<String>,
) -> Result<SummaryOutput> {
    let max_tokens = clamp_summary_max_tokens(((0.5 * reserve_tokens as f64).floor()) as i64, model.max_tokens);
    let llm_messages = convert_to_llm(messages);
    let conversation_text = serialize_conversation(&llm_messages);
    let mut prompt_text =
        format!("# Conversation\n{conversation_text}\n\n# Instructions\n{TURN_PREFIX_SUMMARIZATION_PROMPT}");
    if let Some(custom) = &custom_instructions
        && !custom.is_empty()
    {
        prompt_text.push_str("\n\nAdditional focus: ");
        prompt_text.push_str(custom);
    }

    let options = create_summarization_options(
        model,
        max_tokens,
        api_key,
        headers,
        env,
        signal,
        thinking_level,
        session_id,
    );
    let ctx = build_summarization_context(prompt_text);
    let response = complete_summarization(
        model,
        &ctx,
        options,
        stream_fn.as_ref(),
        retry.as_ref(),
        callbacks.as_deref(),
    )
    .await?;

    if response.stop_reason == StopReason::Aborted {
        return Err(Error::Abort(pi_js::abort::AbortReason::abort()));
    }
    if let Some(failure) = get_summarization_failure(&response, "Turn prefix summarization") {
        return Err(Error::msg(failure));
    }
    if response
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
    {
        return Err(Error::msg("Turn prefix summarization attempted to call a tool"));
    }

    Ok(SummaryOutput {
        text: content_text(&response.content, None),
        usage: response.usage,
    })
}
