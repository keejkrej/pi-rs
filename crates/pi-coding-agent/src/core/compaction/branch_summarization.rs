//! Branch summarization for tree navigation.
//!
//! When navigating to a different point in the session tree, this generates
//! a summary of the branch being left so context isn't lost.
//!
//! Port of `packages/coding-agent/src/core/compaction/branch-summarization.ts`.

use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};
use pi_agent_core::{AgentMessage, StreamFn, ThinkingLevel};
use pi_ai::{
    AssistantContentBlock, Context, Message, Model, ProviderEnv, ProviderHeaders, RetryCallbacks, RetryPolicy,
    SimpleStreamOptions, StopReason, Usage, UserMessage, UserMessageContent, content_text,
};
use serde::{Deserialize, Serialize};

use crate::core::messages::{convert_to_llm, create_branch_summary_message, create_compaction_summary_message};
use crate::core::session_manager::{ContextEditEntry, SessionEntry, SessionManager, project_context_entry};

use super::compaction::{complete_summarization, estimate_tokens, get_summarization_failure};
use super::utils::{
    FileOperations, SUMMARIZATION_SYSTEM_PROMPT, compute_file_lists, create_file_ops, extract_file_ops_from_message,
    format_file_operations, serialize_conversation,
};

// ============================================================================
// Types
// ============================================================================

/// Result of generating a branch summary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_files: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_files: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aborted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Details stored in `BranchSummaryEntry.details` for file tracking.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

/// Messages and cumulative file operations prepared for branch summarization.
#[derive(Clone, Debug, PartialEq)]
pub struct BranchPreparation {
    /// Messages extracted for summarization, in chronological order.
    pub messages: Vec<AgentMessage>,
    /// File operations extracted from tool calls.
    pub file_ops: FileOperations,
    /// Total estimated tokens in `messages`.
    pub total_tokens: i64,
}

/// Entries along a branch and their deepest common ancestor with the target branch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectEntriesResult {
    /// Entries to summarize, in chronological order.
    pub entries: Vec<SessionEntry>,
    /// Common ancestor between old and new position, if any.
    pub common_ancestor_id: Option<String>,
}

/// Read-only view of session tree traversal required by [`collect_entries_for_branch_summary`].
pub trait ReadonlySessionManager {
    fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry>;
    fn get_entry(&self, id: &str) -> Option<SessionEntry>;
}

impl ReadonlySessionManager for SessionManager {
    fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry> {
        SessionManager::get_branch(self, from_id)
    }

    fn get_entry(&self, id: &str) -> Option<SessionEntry> {
        SessionManager::get_entry(self, id)
    }
}

/// Options for [`generate_branch_summary`].
#[derive(Clone)]
pub struct GenerateBranchSummaryOptions {
    /// Model to use for summarization.
    pub model: Model,
    /// API key for the model.
    pub api_key: Option<String>,
    /// Request headers for the model.
    pub headers: Option<ProviderHeaders>,
    /// Provider-scoped environment values for the model.
    pub env: Option<ProviderEnv>,
    /// Abort signal for cancellation.
    pub signal: Option<pi_js::abort::AbortSignal>,
    /// Thinking level for reasoning-capable models.
    pub thinking_level: Option<ThinkingLevel>,
    /// Optional custom instructions for summarization.
    pub custom_instructions: Option<String>,
    /// If true, `custom_instructions` replaces the default prompt instead of being appended.
    pub replace_instructions: Option<bool>,
    /// Tokens reserved when selecting branch history (default 16384).
    pub reserve_tokens: Option<i64>,
    /// Optional session stream function.
    pub stream_fn: Option<StreamFn>,
    /// Retry policy for transient summarization errors.
    pub retry: Option<RetryPolicy>,
    /// Optional callbacks for retry reporting.
    pub callbacks: Option<Arc<dyn RetryCallbacks>>,
}

impl GenerateBranchSummaryOptions {
    /// Construct options with only the required `model` and sensible defaults.
    pub fn new(model: Model) -> Self {
        Self {
            model,
            api_key: None,
            headers: None,
            env: None,
            signal: None,
            thinking_level: None,
            custom_instructions: None,
            replace_instructions: None,
            reserve_tokens: None,
            stream_fn: None,
            retry: None,
            callbacks: None,
        }
    }
}

// ============================================================================
// Entry Collection
// ============================================================================

/// Collect entries that should be summarized when navigating from one position to another.
///
/// Walks from `old_leaf_id` back to the common ancestor with `target_id`, collecting entries
/// along the way. Does NOT stop at compaction boundaries — those are included and their
/// summaries become context.
pub fn collect_entries_for_branch_summary<S: ReadonlySessionManager + ?Sized>(
    session: &S,
    old_leaf_id: Option<&str>,
    target_id: &str,
) -> CollectEntriesResult {
    let Some(old_leaf) = old_leaf_id else {
        return CollectEntriesResult {
            entries: Vec::new(),
            common_ancestor_id: None,
        };
    };

    let old_path: IndexSet<String> = session
        .get_branch(Some(old_leaf))
        .into_iter()
        .map(|entry| entry.id().to_owned())
        .collect();
    let target_path = session.get_branch(Some(target_id));

    // target_path is root-first, so iterate backwards to find deepest common ancestor
    let mut common_ancestor_id: Option<String> = None;
    for entry in target_path.iter().rev() {
        let id = entry.id();
        if old_path.contains(id) {
            common_ancestor_id = Some(id.to_owned());
            break;
        }
    }

    // Collect entries from old leaf back to common ancestor
    let mut entries: Vec<SessionEntry> = Vec::new();
    let mut current: Option<String> = Some(old_leaf.to_owned());
    while let Some(cur_id) = current {
        if common_ancestor_id.as_deref() == Some(cur_id.as_str()) {
            break;
        }
        let Some(entry) = session.get_entry(&cur_id) else {
            break;
        };
        current = entry.parent_id().map(ToOwned::to_owned);
        entries.push(entry);
    }

    entries.reverse();

    CollectEntriesResult {
        entries,
        common_ancestor_id,
    }
}

// ============================================================================
// Entry to Message Conversion
// ============================================================================

fn get_message_from_entry(entry: &SessionEntry, edit: Option<&ContextEditEntry>) -> Option<AgentMessage> {
    match entry {
        SessionEntry::Message(msg_entry) => {
            if msg_entry.message.role() == Some("toolResult") {
                return None;
            }
            if let Some(edit) = edit {
                return project_context_entry(entry, Some(edit)).into_iter().next();
            }
            Some(msg_entry.message.clone())
        }
        SessionEntry::CustomMessage(custom) => {
            if let Some(edit) = edit {
                return project_context_entry(entry, Some(edit)).into_iter().next();
            }
            Some(project_or_create_custom(custom))
        }
        SessionEntry::BranchSummary(bs) => {
            Some(create_branch_summary_message(bs.summary.clone(), Some(bs.from_id.clone()), &bs.timestamp).into())
        }
        SessionEntry::Compaction(c) => {
            Some(create_compaction_summary_message(c.summary.clone(), c.tokens_before, &c.timestamp).into())
        }
        SessionEntry::ThinkingLevelChange(_)
        | SessionEntry::ModelChange(_)
        | SessionEntry::Custom(_)
        | SessionEntry::Label(_)
        | SessionEntry::SessionInfo(_)
        | SessionEntry::ContextEdit(_)
        | SessionEntry::Usage(_) => None,
    }
}

fn project_or_create_custom(custom: &crate::core::session_manager::CustomMessageEntry) -> AgentMessage {
    crate::core::messages::create_custom_message(
        custom.custom_type.clone(),
        custom.content.clone(),
        custom.display,
        custom.details.clone(),
        &custom.timestamp,
    )
    .into()
}

/// Prepare entries for summarization with a token budget (`0` = no limit).
///
/// Walks entries from NEWEST to OLDEST, adding messages until hitting the token budget.
pub fn prepare_branch_entries(entries: &[SessionEntry], token_budget: i64) -> BranchPreparation {
    let mut messages: Vec<AgentMessage> = Vec::new();
    let mut file_ops = create_file_ops();
    let mut total_tokens = 0i64;

    // First pass: collect cumulative file ops from existing branch_summary entries.
    for entry in entries {
        if let SessionEntry::BranchSummary(bs) = entry
            && !bs.from_hook.unwrap_or(false)
            && let Some(details_val) = &bs.details
            && let Ok(details) = serde_json::from_value::<BranchSummaryDetails>(details_val.clone())
        {
            for f in details.read_files {
                file_ops.read.insert(f);
            }
            for f in details.modified_files {
                file_ops.edited.insert(f);
            }
        }
    }

    // Context edits hide or replace earlier entries in model context; honor the latest edit per target.
    let mut edits: IndexMap<&str, &ContextEditEntry> = IndexMap::new();
    for entry in entries {
        if let SessionEntry::ContextEdit(edit) = entry {
            edits.insert(edit.target_id.as_str(), edit);
        }
    }

    // Second pass: walk from newest to oldest, adding messages until token budget.
    for entry in entries.iter().rev() {
        let Some(message) = get_message_from_entry(entry, edits.get(entry.id()).copied()) else {
            continue;
        };

        extract_file_ops_from_message(&message, &mut file_ops);

        let tokens = estimate_tokens(&message);
        if token_budget > 0 && total_tokens + tokens > token_budget {
            if matches!(entry, SessionEntry::Compaction(_) | SessionEntry::BranchSummary(_))
                && (total_tokens as f64) < (token_budget as f64) * 0.9
            {
                messages.insert(0, message);
                total_tokens += tokens;
            }
            break;
        }

        messages.insert(0, message);
        total_tokens += tokens;
    }

    BranchPreparation {
        messages,
        file_ops,
        total_tokens,
    }
}

// ============================================================================
// Summary Generation
// ============================================================================

const BRANCH_SUMMARY_PREAMBLE: &str = "The user explored a different conversation branch before returning here.
Summary of that exploration:

";

const BRANCH_SUMMARY_PROMPT: &str =
    "Create a structured summary of this conversation branch for context when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Work that was started but not finished]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

/// Generate a summary of abandoned branch entries.
pub async fn generate_branch_summary(
    entries: &[SessionEntry],
    options: GenerateBranchSummaryOptions,
) -> BranchSummaryResult {
    let reserve_tokens = options.reserve_tokens.unwrap_or(16384);
    let context_window = if options.model.context_window != 0 {
        options.model.context_window
    } else {
        128000
    };
    let token_budget = context_window - reserve_tokens;

    let preparation = prepare_branch_entries(entries, token_budget);
    if preparation.messages.is_empty() {
        return BranchSummaryResult {
            summary: Some("No content to summarize".to_owned()),
            usage: None,
            read_files: None,
            modified_files: None,
            aborted: None,
            error: None,
        };
    }

    let llm_messages = convert_to_llm(&preparation.messages);
    let conversation_text = serialize_conversation(&llm_messages);

    let instructions = match (
        options.replace_instructions.unwrap_or(false),
        options.custom_instructions.as_deref(),
    ) {
        (true, Some(custom)) if !custom.is_empty() => custom.to_owned(),
        (_, Some(custom)) if !custom.is_empty() => format!("{BRANCH_SUMMARY_PROMPT}\n\nAdditional focus: {custom}"),
        _ => BRANCH_SUMMARY_PROMPT.to_owned(),
    };
    let prompt_text = format!("<conversation>\n{conversation_text}\n</conversation>\n\n{instructions}");

    let context = Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text(prompt_text),
            timestamp: pi_js::time::now_ms(),
        })],
        tools: None,
    };

    let max_tokens = if options.model.max_tokens > 0 {
        4096.min(options.model.max_tokens)
    } else {
        4096
    };

    let mut request_options = SimpleStreamOptions::default();
    request_options.base.api_key = options.api_key;
    request_options.base.headers = options.headers;
    request_options.base.env = options.env;
    request_options.base.signal = options.signal;
    request_options.base.max_tokens = Some(max_tokens);
    if options.model.reasoning
        && let Some(level) = options.thinking_level
    {
        request_options.reasoning = level.to_ai_thinking_level();
    }

    let response = match complete_summarization(
        &options.model,
        &context,
        request_options,
        options.stream_fn.as_ref(),
        options.retry.as_ref(),
        options.callbacks.as_deref(),
    )
    .await
    {
        Ok(resp) => resp,
        Err(err) => {
            if err.is_abort() {
                return BranchSummaryResult {
                    summary: None,
                    usage: None,
                    read_files: None,
                    modified_files: None,
                    aborted: Some(true),
                    error: None,
                };
            }
            return BranchSummaryResult {
                summary: None,
                usage: None,
                read_files: None,
                modified_files: None,
                aborted: None,
                error: Some(err.to_string()),
            };
        }
    };

    if response.stop_reason == StopReason::Aborted {
        return BranchSummaryResult {
            summary: None,
            usage: None,
            read_files: None,
            modified_files: None,
            aborted: Some(true),
            error: None,
        };
    }
    if let Some(failure) = get_summarization_failure(&response, "Branch summarization") {
        return BranchSummaryResult {
            summary: None,
            usage: None,
            read_files: None,
            modified_files: None,
            aborted: None,
            error: Some(failure),
        };
    }
    if response
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
    {
        return BranchSummaryResult {
            summary: None,
            usage: None,
            read_files: None,
            modified_files: None,
            aborted: None,
            error: Some("Branch summarization attempted to call a tool".to_owned()),
        };
    }

    let lists = compute_file_lists(&preparation.file_ops);
    let mut summary = format!("{BRANCH_SUMMARY_PREAMBLE}{}", content_text(&response.content, None));
    summary.push_str(&format_file_operations(&lists.read_files, &lists.modified_files));

    BranchSummaryResult {
        summary: Some(if summary.is_empty() {
            "No summary generated".to_owned()
        } else {
            summary
        }),
        usage: Some(response.usage),
        read_files: Some(lists.read_files),
        modified_files: Some(lists.modified_files),
        aborted: None,
        error: None,
    }
}
