//! Port of packages/durable/src/harness/context.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, TextContent, ToolCall, ToolResultMessage,
    UserContentBlock,
};
use pi_chord::Context;

use crate::harness::types::ContextView;
use crate::session::session::SessionImpl;
use crate::types::{ContextEdit, ConversationId, Cursor, EntryId, EntryQuery, EntryRecord, Storage};

const SCAN_PAGE_SIZE: usize = 256;
const MISSING_RESULT_TEXT: &str = "Tool result unavailable: history ends before this call completed.";

fn is_excluded_stop_reason(reason: StopReason) -> bool {
    matches!(reason, StopReason::Aborted | StopReason::Error | StopReason::Deferred)
}

/// Head marker and newest visible entry that fix one committed context range.
#[derive(Clone, Debug, PartialEq)]
pub struct ContextBounds {
    pub head: Option<EntryRecord>,
    pub tail: EntryId,
}

/// Capture the bounds of the current context, or of the context cut off at the visible entry `at`,
/// with two O(1) reads. Run this on the Session line; entries at or below the tail are immutable,
/// so `derive_context()` can then scan them off the line.
pub async fn capture_context_bounds(
    storage: &dyn Storage,
    conversation_id: ConversationId,
    context: &Context,
    at: Option<EntryId>,
) -> pi_js::Result<Option<ContextBounds>> {
    let tail = match at {
        None => {
            let page = storage
                .scan_entries(
                    EntryQuery {
                        conversation_id,
                        min_entry_id: None,
                        max_entry_id: None,
                    },
                    1,
                    None,
                    context.clone(),
                )
                .await?;
            let Some(first) = page.items.into_iter().next() else {
                return Ok(None);
            };
            first.id
        }
        Some(target) => {
            if storage
                .entry_in_conversation(conversation_id, target, context.clone())
                .await?
                .is_none()
            {
                return Err(pi_js::Error::msg(format!(
                    "Entry {target} is not visible from conversation {conversation_id}"
                )));
            }
            target
        }
    };
    let head = storage
        .find_latest_head_marker(conversation_id, Some(tail), context.clone())
        .await?;
    Ok(Some(ContextBounds { head, tail }))
}

/// Committed context of one conversation: bounds captured on the Session line, entries derived off it.
pub async fn read_context(
    session: &Arc<SessionImpl>,
    storage: &Arc<dyn Storage>,
    conversation_id: ConversationId,
    context: Context,
    at: Option<EntryId>,
) -> pi_js::Result<ContextView> {
    let storage_for_line = Arc::clone(storage);
    let ctx_for_line = context.clone();
    let bounds = session
        .read_on_line(move || async move {
            capture_context_bounds(storage_for_line.as_ref(), conversation_id, &ctx_for_line, at).await
        })
        .await?;
    derive_context(storage.as_ref(), conversation_id, bounds, &context).await
}

/// Derive the active transcript and model context of one conversation within captured bounds.
///
/// `H` = newest visible head marker; the range runs from `H.head` (or transcript start) through
/// the tail. Per target, the newest edit in the range wins. Context entries are `H` followed by
/// the range's non-head entries.
pub async fn derive_context(
    storage: &dyn Storage,
    conversation_id: ConversationId,
    bounds: Option<ContextBounds>,
    context: &Context,
) -> pi_js::Result<ContextView> {
    let Some(bounds) = bounds else {
        return Ok(ContextView {
            head: None,
            entries: Vec::new(),
            contributions: Vec::new(),
            messages: Vec::new(),
        });
    };
    let head = bounds.head.clone();
    let range = scan_range(storage, conversation_id, &bounds, context).await?;
    let mut edits: IndexMap<EntryId, ContextEdit> = IndexMap::new();
    // Edits of every entry in the range count, including older head markers that `select_active()` drops.
    for entry in &range {
        if let Some(entry_edits) = &entry.edits {
            for edit in entry_edits {
                edits.insert(edit.target(), edit.clone());
            }
        }
    }

    let entries = select_active(head.clone(), range);
    let mut contributions: Vec<Vec<Message>> = Vec::with_capacity(entries.len());
    let mut flat_messages: Vec<Message> = Vec::new();
    for entry in &entries {
        let contributed: &[Message] = match edits.get(&entry.id) {
            Some(ContextEdit::Omit { .. }) => &[],
            Some(ContextEdit::Replace { messages, .. }) => messages.as_slice(),
            None => entry.model.as_deref().unwrap_or(&[]),
        };
        let filtered: Vec<Message> = contributed
            .iter()
            .filter(|msg| match msg {
                Message::Assistant(AssistantMessage { stop_reason, .. }) => !is_excluded_stop_reason(*stop_reason),
                _ => true,
            })
            .cloned()
            .collect();
        flat_messages.extend(filtered.iter().cloned());
        contributions.push(filtered);
    }

    let messages = order_tool_results(&flat_messages);
    Ok(ContextView {
        head,
        entries,
        contributions,
        messages,
    })
}

/// The raw active entries within captured bounds, without deriving model context.
pub async fn active_entries(
    storage: &dyn Storage,
    conversation_id: ConversationId,
    bounds: Option<ContextBounds>,
    context: &Context,
) -> pi_js::Result<Vec<EntryRecord>> {
    let Some(bounds) = bounds else {
        return Ok(Vec::new());
    };
    let head = bounds.head.clone();
    let range = scan_range(storage, conversation_id, &bounds, context).await?;
    Ok(select_active(head, range))
}

/// Visible entries from the head marker's head, or transcript start, through the tail, oldest first.
async fn scan_range(
    storage: &dyn Storage,
    conversation_id: ConversationId,
    bounds: &ContextBounds,
    context: &Context,
) -> pi_js::Result<Vec<EntryRecord>> {
    let min_entry_id = bounds.head.as_ref().and_then(|h| h.head);
    let mut range = Vec::new();
    let mut cursor: Option<Cursor> = None;
    loop {
        let page = storage
            .scan_entries(
                EntryQuery {
                    conversation_id,
                    min_entry_id,
                    max_entry_id: Some(bounds.tail),
                },
                SCAN_PAGE_SIZE,
                cursor,
                context.clone(),
            )
            .await?;
        range.extend(page.items);
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    range.reverse();
    Ok(range)
}

/// The head marker followed by the range's non-head entries, or the whole range without a marker.
fn select_active(head: Option<EntryRecord>, range: Vec<EntryRecord>) -> Vec<EntryRecord> {
    let Some(head_entry) = head else {
        return range;
    };
    let mut selected = Vec::with_capacity(1 + range.len());
    selected.push(head_entry);
    selected.extend(range.into_iter().filter(|entry| entry.head.is_none()));
    selected
}

/// Place each assistant's tool results directly after it in call order.
/// Results are taken from the messages before the next assistant; a missing result is synthesized
/// and unmatched results are dropped.
pub fn order_tool_results(messages: &[Message]) -> Vec<Message> {
    let mut ordered = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate() {
        if matches!(message, Message::ToolResult(_)) {
            continue;
        }
        ordered.push(message.clone());
        let Message::Assistant(assistant) = message else {
            continue;
        };
        let calls: Vec<&ToolCall> = assistant
            .content
            .iter()
            .filter_map(|block| match block {
                AssistantContentBlock::ToolCall(call) => Some(call),
                _ => None,
            })
            .collect();
        if calls.is_empty() {
            continue;
        }
        let mut results: IndexMap<&str, usize> = IndexMap::new();
        for (next, candidate) in messages.iter().enumerate().skip(index + 1) {
            if matches!(candidate, Message::Assistant(_)) {
                break;
            }
            if let Message::ToolResult(tr) = candidate {
                results.entry(tr.tool_call_id.as_str()).or_insert(next);
            }
        }
        for call in calls {
            match results.get(call.id.as_str()) {
                Some(&result_index) => ordered.push(messages[result_index].clone()),
                None => ordered.push(missing_result(call, assistant.timestamp)),
            }
        }
    }
    ordered
}

fn missing_result(call: &ToolCall, timestamp: i64) -> Message {
    Message::ToolResult(ToolResultMessage {
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        content: vec![UserContentBlock::Text(TextContent::new(MISSING_RESULT_TEXT))],
        details: Some(serde_json::json!({ "reason": "missing_result" })),
        usage: None,
        nested_calls: None,
        is_error: true,
        timestamp,
    })
}
