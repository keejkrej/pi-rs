//! Port of packages/ai/src/api/strict-tool-limits.ts

use std::sync::{LazyLock, Mutex, Once};

use indexmap::IndexSet;

use crate::session_resources::register_session_resource_cleanup;

/// Claude rejects (on the Anthropic API and on Bedrock) a request whose strict tools exceed its limits with one of these
/// messages. Observed limits: 20 strict tools, and 16 optional or union-typed parameters across all strict tools.
const STRICT_TOOL_LIMIT_ERROR_MESSAGES: &[&str] = &[
    "The compiled grammar is too large",
    "Too many strict tools (",
    "Schemas contains too many parameters with union types (",
];

/// Sessions whose strict tools Claude rejected for exceeding its limits. Their later requests send `strict: "prefer"`
/// tools non-strict right away: the tool set rarely changes between turns, so trying strict again would only repeat
/// the rejection, and switching back to strict later would rewrite the tool definitions and miss the prompt cache.
/// Shared by the Anthropic and Bedrock providers, which run the same Claude limits.
const MAX_SESSIONS_OVER_STRICT_TOOL_LIMITS: usize = 10_000;

static SESSIONS_OVER_STRICT_TOOL_LIMITS: LazyLock<Mutex<IndexSet<String>>> =
    LazyLock::new(|| Mutex::new(IndexSet::new()));
static CLEANUP_REGISTERED: Once = Once::new();

fn ensure_cleanup_registered() {
    CLEANUP_REGISTERED.call_once(|| {
        let _ = register_session_resource_cleanup(|session_id| {
            let mut guard = SESSIONS_OVER_STRICT_TOOL_LIMITS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match session_id {
                Some(id) => {
                    guard.shift_remove(id);
                }
                None => guard.clear(),
            }
        });
    });
}

/// Returns true when a provider error message says the request's strict tools exceed the provider's limits.
pub fn is_strict_tool_limit_error_message(message: &str) -> bool {
    STRICT_TOOL_LIMIT_ERROR_MESSAGES
        .iter()
        .any(|limit_message| message.contains(limit_message))
}

/// Records that a session's strict tools exceeded Claude's limits; does nothing without a session. Keeps at most
/// `MAX_SESSIONS_OVER_STRICT_TOOL_LIMITS` sessions.
///
/// Entries are normally freed by `cleanup_session_resources(session_id)` when a session ends. The cap bounds memory for
/// library users that never call it; a dropped session just pays one more rejection.
pub fn mark_strict_tool_limits_exceeded(session_id: Option<&str>) {
    ensure_cleanup_registered();
    let Some(id) = session_id else {
        return;
    };
    let mut guard = SESSIONS_OVER_STRICT_TOOL_LIMITS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.insert(id.to_owned());
    while guard.len() > MAX_SESSIONS_OVER_STRICT_TOOL_LIMITS {
        guard.shift_remove_index(0);
    }
}

/// Returns true when an earlier request of this session had its strict tools rejected for exceeding Claude's limits.
pub fn has_exceeded_strict_tool_limits(session_id: Option<&str>) -> bool {
    ensure_cleanup_registered();
    let Some(id) = session_id else {
        return false;
    };
    let guard = SESSIONS_OVER_STRICT_TOOL_LIMITS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.contains(id)
}
