//! Session prompt-cache miss and waste tracking.
//!
//! Port of `packages/coding-agent/src/core/cache-stats.ts`.

use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_ai::{AssistantMessage, Message};
use serde::{Deserialize, Serialize};

use crate::core::session_manager::SessionEntry;

/// Prompt-cache TTL: idle gaps longer than this are worth mentioning as the
/// likely cause of a miss. Anthropic's default cache TTL is 5 minutes.
pub const CACHE_TTL_MS: i64 = 5 * 60 * 1000;

/// Per-turn misses at or below this are cache breakpoint granularity noise.
const NOISE_FLOOR_TOKENS: i64 = 1024;

/// A counted cache miss on a single assistant message.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheMiss {
    /// Prompt tokens that were in the previous turn's prompt but not read from cache.
    pub missed_tokens: i64,
    /// Extra dollars paid vs. a full cache hit; 0 when pricing is unknown.
    pub missed_cost: f64,
    /// Milliseconds since the previous request (which last refreshed the cache).
    pub idle_ms: i64,
    /// True when the model changed relative to the previous request.
    pub model_changed: bool,
}

/// Cumulative cache waste totals across a session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheWasteTotals {
    pub missed_tokens: i64,
    pub missed_cost: f64,
    /// Number of counted misses (turns above the noise floor).
    pub miss_count: usize,
}

/// Minimal pricing lookup, satisfied by `ModelRuntime`. Cost is $/million tokens.
// WIRE(@mcp-helper2): implement `ModelPriceSource` for `crate::core::model_runtime::ModelRuntime` once peer helper lands.
pub trait ModelPriceSource: Send + Sync {
    fn get_cache_read_cost(&self, provider: &str, model_id: &str) -> Option<f64>;
}

impl<F> ModelPriceSource for F
where
    F: Fn(&str, &str) -> Option<f64> + Send + Sync,
{
    fn get_cache_read_cost(&self, provider: &str, model_id: &str) -> Option<f64> {
        self(provider, model_id)
    }
}

struct PreviousRequest {
    prompt_tokens: i64,
    model_key: String,
    timestamp: i64,
    reported_cache: bool,
}

struct ScanResult {
    prev: Option<PreviousRequest>,
    totals: CacheWasteTotals,
    misses: IndexMap<usize, CacheMiss>,
}

fn detect_miss(
    prev: Option<&PreviousRequest>,
    message: &AssistantMessage,
    models: &dyn ModelPriceSource,
) -> Option<CacheMiss> {
    let prev = prev?;
    let usage = &message.usage;
    let prompt_tokens = usage.input + usage.cache_read + usage.cache_write;
    if prompt_tokens <= 0 || (usage.cache_read + usage.cache_write == 0 && !prev.reported_cache) {
        return None;
    }

    let missed_tokens = prev.prompt_tokens.min(prompt_tokens) - usage.cache_read;
    if missed_tokens <= NOISE_FLOOR_TOKENS {
        return None;
    }

    let paid_tokens = usage.input + usage.cache_write;
    let paid_per_token = if paid_tokens > 0 {
        (usage.cost.input + usage.cost.cache_write) / (paid_tokens as f64)
    } else {
        0.0
    };
    let read_per_token = if usage.cache_read > 0 {
        usage.cost.cache_read / (usage.cache_read as f64)
    } else {
        models
            .get_cache_read_cost(&message.provider, &message.model)
            .unwrap_or(0.0)
            / 1_000_000.0
    };

    let model_key = format!("{}/{}", message.provider, message.model);
    Some(CacheMiss {
        missed_tokens,
        missed_cost: (missed_tokens as f64) * (paid_per_token - read_per_token).max(0.0),
        idle_ms: (message.timestamp - prev.timestamp).max(0),
        model_changed: model_key != prev.model_key,
    })
}

fn as_previous_request(message: &AssistantMessage, reported_cache: bool) -> Option<PreviousRequest> {
    let usage = &message.usage;
    let prompt_tokens = usage.input + usage.cache_read + usage.cache_write;
    if prompt_tokens <= 0 {
        return None;
    }
    Some(PreviousRequest {
        prompt_tokens,
        model_key: format!("{}/{}", message.provider, message.model),
        timestamp: message.timestamp,
        reported_cache: reported_cache || (usage.cache_read + usage.cache_write) > 0,
    })
}

fn scan(entries: &[SessionEntry], models: &dyn ModelPriceSource) -> ScanResult {
    let mut prev: Option<PreviousRequest> = None;
    let mut totals = CacheWasteTotals::default();
    let mut misses: IndexMap<usize, CacheMiss> = IndexMap::new();

    for (idx, entry) in entries.iter().enumerate() {
        match entry {
            SessionEntry::Compaction(_) | SessionEntry::BranchSummary(_) => {
                prev = None;
            }
            SessionEntry::Usage(usage_entry) if usage_entry.kind == "cache_warm" => {
                let prompt_tokens =
                    usage_entry.usage.input + usage_entry.usage.cache_read + usage_entry.usage.cache_write;
                if prompt_tokens > 0 {
                    prev = Some(PreviousRequest {
                        prompt_tokens,
                        model_key: format!("{}/{}", usage_entry.provider, usage_entry.model),
                        timestamp: pi_js::time::parse_date(&usage_entry.timestamp).unwrap_or(0),
                        reported_cache: true,
                    });
                }
            }
            SessionEntry::Message(msg_entry) => {
                let AgentMessage::Message(Message::Assistant(assistant)) = &msg_entry.message else {
                    continue;
                };
                if let Some(miss) = detect_miss(prev.as_ref(), assistant, models) {
                    totals.missed_tokens += miss.missed_tokens;
                    totals.missed_cost += miss.missed_cost;
                    totals.miss_count += 1;
                    misses.insert(idx, miss);
                }
                let prev_reported = prev.as_ref().is_some_and(|p| p.reported_cache);
                if let Some(next_prev) = as_previous_request(assistant, prev_reported) {
                    prev = Some(next_prev);
                }
            }
            _ => {}
        }
    }

    ScanResult { prev, totals, misses }
}

/// Cumulative cache waste across a session: prompt tokens that should have been
/// cache reads (they were in the previous turn's prompt) but were re-billed.
pub fn compute_cache_waste(entries: &[SessionEntry], models: &dyn ModelPriceSource) -> CacheWasteTotals {
    scan(entries, models).totals
}

/// All counted cache misses across a session, keyed by entry index in `entries`.
pub fn collect_cache_misses(entries: &[SessionEntry], models: &dyn ModelPriceSource) -> IndexMap<usize, CacheMiss> {
    scan(entries, models).misses
}

/// Detect a cache miss on a just-completed assistant message.
/// `entries` must not yet contain `message` (`message_end` fires before persistence).
pub fn detect_cache_miss(
    entries: &[SessionEntry],
    message: &AssistantMessage,
    models: &dyn ModelPriceSource,
) -> Option<CacheMiss> {
    detect_miss(scan(entries, models).prev.as_ref(), message, models)
}
