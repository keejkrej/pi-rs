//! Port of packages/ai/src/api/simple-options.ts

use crate::types::{Model, SimpleStreamOptions, StreamOptions, ThinkingBudgets, ThinkingLevel, TranscriptContext};
use crate::utils::estimate::estimate_context_tokens;

const CONTEXT_SAFETY_TOKENS: i64 = 4096;
const MIN_MAX_TOKENS: i64 = 1;

/// Tokens always left for the answer when a thinking budget shares the response ceiling.
pub const MIN_ANSWER_TOKENS: i64 = 1024;

pub const DEFAULT_THINKING_BUDGETS: ThinkingBudgets = ThinkingBudgets {
    minimal: Some(1024),
    low: Some(2048),
    medium: Some(8192),
    high: Some(16384),
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdjustedMaxTokens {
    pub max_tokens: i64,
    pub thinking_budget: i64,
}

pub fn clamp_max_tokens_to_context(model: &Model, context: &TranscriptContext, max_tokens: i64) -> i64 {
    if model.context_window <= 0 {
        return MIN_MAX_TOKENS.max(max_tokens);
    }
    let available = model.context_window - estimate_context_tokens(context).tokens - CONTEXT_SAFETY_TOKENS;
    max_tokens.min(MIN_MAX_TOKENS.max(available))
}

pub fn build_base_options(
    model: &Model,
    context: &TranscriptContext,
    options: Option<&SimpleStreamOptions>,
    api_key: Option<&str>,
) -> StreamOptions {
    let base = options.map(|o| &o.base);
    let requested_max = base.and_then(|b| b.max_tokens).unwrap_or(model.max_tokens);
    let resolved_api_key = api_key
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| base.and_then(|b| b.api_key.as_ref()).filter(|s| !s.is_empty()).cloned());

    StreamOptions {
        temperature: base.and_then(|b| b.temperature),
        sampling_params: base.and_then(|b| b.sampling_params.clone()),
        max_tokens: Some(clamp_max_tokens_to_context(model, context, requested_max)),
        signal: base.and_then(|b| b.signal.clone()),
        telemetry_context: base.and_then(|b| b.telemetry_context.clone()),
        api_key: resolved_api_key,
        fetch: base.and_then(|b| b.fetch.clone()),
        transport: base.and_then(|b| b.transport),
        cache_retention: base.and_then(|b| b.cache_retention),
        session_id: base.and_then(|b| b.session_id.clone()),
        headers: base.and_then(|b| b.headers.clone()),
        on_payload: base.and_then(|b| b.on_payload.clone()),
        on_response: base.and_then(|b| b.on_response.clone()),
        on_provider_stream_event: base.and_then(|b| b.on_provider_stream_event.clone()),
        timeout_ms: base.and_then(|b| b.timeout_ms),
        websocket_connect_timeout_ms: base.and_then(|b| b.websocket_connect_timeout_ms),
        max_retries: base.and_then(|b| b.max_retries),
        max_retry_delay_ms: base.and_then(|b| b.max_retry_delay_ms),
        metadata: base.and_then(|b| b.metadata.clone()),
        env: base.and_then(|b| b.env.clone()),
    }
}

pub fn clamp_reasoning(effort: Option<ThinkingLevel>) -> Option<ThinkingLevel> {
    match effort {
        Some(ThinkingLevel::XHigh | ThinkingLevel::Max) => Some(ThinkingLevel::High),
        other => other,
    }
}

pub fn thinking_budget_for_level(reasoning_level: ThinkingLevel, custom_budgets: Option<&ThinkingBudgets>) -> i64 {
    let clamped = clamp_reasoning(Some(reasoning_level)).unwrap_or(ThinkingLevel::High);
    match clamped {
        ThinkingLevel::Minimal => custom_budgets
            .and_then(|b| b.minimal)
            .or(DEFAULT_THINKING_BUDGETS.minimal)
            .unwrap_or(1024),
        ThinkingLevel::Low => custom_budgets
            .and_then(|b| b.low)
            .or(DEFAULT_THINKING_BUDGETS.low)
            .unwrap_or(2048),
        ThinkingLevel::Medium => custom_budgets
            .and_then(|b| b.medium)
            .or(DEFAULT_THINKING_BUDGETS.medium)
            .unwrap_or(8192),
        ThinkingLevel::High | ThinkingLevel::XHigh | ThinkingLevel::Max => custom_budgets
            .and_then(|b| b.high)
            .or(DEFAULT_THINKING_BUDGETS.high)
            .unwrap_or(16384),
    }
}

/// Cap a thinking budget so at least MIN_ANSWER_TOKENS remain under a shared response ceiling.
pub fn clamp_thinking_budget_to_answer_room(thinking_budget: i64, ceiling: i64) -> i64 {
    thinking_budget.min((ceiling - MIN_ANSWER_TOKENS).max(0))
}

pub fn adjust_max_tokens_for_thinking(
    // None means no explicit caller cap. Use the model cap and fit thinking inside it.
    base_max_tokens: Option<i64>,
    model_max_tokens: i64,
    reasoning_level: ThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> AdjustedMaxTokens {
    let mut thinking_budget = thinking_budget_for_level(reasoning_level, custom_budgets);
    let max_tokens = match base_max_tokens {
        None => model_max_tokens,
        Some(base) => (base + thinking_budget).min(model_max_tokens),
    };

    if max_tokens <= thinking_budget {
        thinking_budget = clamp_thinking_budget_to_answer_room(thinking_budget, max_tokens);
    }

    AdjustedMaxTokens {
        max_tokens,
        thinking_budget,
    }
}
