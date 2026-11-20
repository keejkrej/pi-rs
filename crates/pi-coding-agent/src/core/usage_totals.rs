//! Port of packages/coding-agent/src/core/usage-totals.ts

use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_ai::{Message, Usage, UsageCost};
use serde::{Deserialize, Serialize};

use crate::core::session_manager::SessionEntry;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cost: f64,
}

pub fn create_usage_totals() -> UsageTotals {
    UsageTotals::default()
}

pub fn add_usage_to_totals(totals: &mut UsageTotals, usage: &Usage) {
    totals.input += usage.input;
    totals.output += usage.output;
    totals.cache_read += usage.cache_read;
    totals.cache_write += usage.cache_write;
    totals.cost += usage.cost.total;
}

/// Sum of two usages, keeping the optional token splits when either side reports them.
pub fn combine_usage(first: &Usage, second: &Usage) -> Usage {
    let cache_write_1h = if first.cache_write_1h.is_some() || second.cache_write_1h.is_some() {
        Some(first.cache_write_1h.unwrap_or(0) + second.cache_write_1h.unwrap_or(0))
    } else {
        None
    };
    let reasoning = if first.reasoning.is_some() || second.reasoning.is_some() {
        Some(first.reasoning.unwrap_or(0) + second.reasoning.unwrap_or(0))
    } else {
        None
    };

    Usage {
        input: first.input + second.input,
        output: first.output + second.output,
        cache_read: first.cache_read + second.cache_read,
        cache_write: first.cache_write + second.cache_write,
        cache_write_1h,
        reasoning,
        total_tokens: first.total_tokens + second.total_tokens,
        cost: UsageCost {
            input: first.cost.input + second.cost.input,
            output: first.cost.output + second.cost.output,
            cache_read: first.cost.cache_read + second.cost.cache_read,
            cache_write: first.cost.cache_write + second.cost.cache_write,
            total: first.cost.total + second.cost.total,
            // A sum is provider-reported only when every part is.
            total_from_provider: Some(
                first.cost.total_from_provider == Some(true) && second.cost.total_from_provider == Some(true),
            ),
        },
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCostBreakdownEntry {
    pub key: String,
    pub cost: f64,
    pub tokens: i64,
}

/// Group model-attributed usage by model and all other usage into a separate bucket.
pub fn get_usage_cost_breakdown(entries: &[SessionEntry]) -> Vec<UsageCostBreakdownEntry> {
    let mut totals_by_key: IndexMap<String, UsageTotals> = IndexMap::new();

    for entry in entries {
        let extracted: Option<(String, Usage)> = match entry {
            SessionEntry::Message(msg_entry) => match &msg_entry.message {
                AgentMessage::Message(Message::Assistant(asst)) => {
                    let model_name = asst.response_model.as_deref().unwrap_or(&asst.model);
                    Some((format!("{}/{}", asst.provider, model_name), asst.usage))
                }
                AgentMessage::Message(Message::ToolResult(tr)) => {
                    tr.usage.map(|usage| ("Tools/summaries".to_owned(), usage))
                }
                _ => None,
            },
            SessionEntry::Usage(usage_entry) => Some((
                format!("{}/{}", usage_entry.provider, usage_entry.model),
                usage_entry.usage,
            )),
            SessionEntry::BranchSummary(bs) => bs.usage.map(|usage| ("Tools/summaries".to_owned(), usage)),
            SessionEntry::Compaction(comp) => comp.usage.map(|usage| ("Tools/summaries".to_owned(), usage)),
            _ => None,
        };

        let Some((key, usage)) = extracted else {
            continue;
        };
        let totals = totals_by_key.entry(key).or_insert_with(create_usage_totals);
        add_usage_to_totals(totals, &usage);
    }

    let mut out: Vec<UsageCostBreakdownEntry> = totals_by_key
        .into_iter()
        .map(|(key, totals)| UsageCostBreakdownEntry {
            key,
            cost: totals.cost,
            tokens: totals.input + totals.output + totals.cache_read + totals.cache_write,
        })
        .filter(|entry| entry.cost > 0.0 || entry.tokens > 0)
        .collect();

    out.sort_by(|a, b| b.cost.partial_cmp(&a.cost).unwrap_or(std::cmp::Ordering::Equal));
    out
}
