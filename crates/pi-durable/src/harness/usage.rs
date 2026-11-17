//! Port of packages/durable/src/harness/usage.ts

use std::sync::LazyLock;

use pi_ai::types::Usage;

use crate::documents::{ConversationDocToken, DocDefinitionInput, define_conversation_doc};
pub use crate::harness::types::UsageState;
use crate::session::transaction::{Transaction, Tx};
use crate::types::{ConversationId, LatestFork};

/// Ledger of one conversation's own spend: its entries and compaction summarization attempts.
pub static USAGE_DOC: LazyLock<ConversationDocToken<UsageState>> = LazyLock::new(|| {
    define_conversation_doc(
        LatestFork::Initial,
        DocDefinitionInput {
            kind: "pi.usage",
            version: 1,
            initial: UsageState::default,
            migrate: None,
            checkpoint_when: Some(|_, _, _| true),
        },
    )
    .expect("valid built-in pi.usage doc definition")
});

/// Bucket within [`UsageState`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageBucket {
    Models,
    Tools,
}

/// Add `usage` to one bucket of the conversation's `pi.usage` in the commit recording the response.
pub async fn record_usage(
    tx: &Tx,
    conversation_id: ConversationId,
    bucket: UsageBucket,
    key: &str,
    usage: &Usage,
) -> pi_js::Result<()> {
    record_usage_tx(tx.as_transaction(), conversation_id, bucket, key, usage).await
}

/// [`Transaction`] variant of [`record_usage`].
pub async fn record_usage_tx(
    tx: &Transaction,
    conversation_id: ConversationId,
    bucket: UsageBucket,
    key: &str,
    usage: &Usage,
) -> pi_js::Result<()> {
    let draft = tx.doc_conversation(&*USAGE_DOC, conversation_id).await?;
    draft.update(|state| {
        let totals = match bucket {
            UsageBucket::Models => &mut state.models,
            UsageBucket::Tools => &mut state.tools,
        };
        if let Some(total) = totals.get_mut(key) {
            add_usage(total, usage);
        } else {
            totals.insert(key.to_owned(), usage.clone());
        }
    })
}

/// Add every counter of `usage` to `total`; optional counters are added once either side reports them.
pub fn add_usage(total: &mut Usage, usage: &Usage) {
    total.input += usage.input;
    total.output += usage.output;
    total.cache_read += usage.cache_read;
    total.cache_write += usage.cache_write;
    total.total_tokens += usage.total_tokens;
    if let Some(cache_write_1h) = usage.cache_write_1h {
        total.cache_write_1h = Some(total.cache_write_1h.unwrap_or(0) + cache_write_1h);
    }
    if let Some(reasoning) = usage.reasoning {
        total.reasoning = Some(total.reasoning.unwrap_or(0) + reasoning);
    }
    total.cost.input += usage.cost.input;
    total.cost.output += usage.cost.output;
    total.cost.cache_read += usage.cost.cache_read;
    total.cost.cache_write += usage.cost.cache_write;
    total.cost.total += usage.cost.total;
}

/// Add every bucket of `state` into `sum`.
pub fn add_usage_state(sum: &mut UsageState, state: &UsageState) {
    for (key, usage) in &state.models {
        if let Some(total) = sum.models.get_mut(key) {
            add_usage(total, usage);
        } else {
            sum.models.insert(key.clone(), usage.clone());
        }
    }
    for (key, usage) in &state.tools {
        if let Some(total) = sum.tools.get_mut(key) {
            add_usage(total, usage);
        } else {
            sum.tools.insert(key.clone(), usage.clone());
        }
    }
}
