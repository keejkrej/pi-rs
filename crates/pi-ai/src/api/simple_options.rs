//! Port of packages/ai/src/api/simple-options.ts

use crate::models::clamp_thinking_level;
use crate::types::{
    Model, ModelThinkingLevel, SamplingParams, SimpleStreamOptions, StreamOptions, ThinkingBudgets, ThinkingLevel,
    TranscriptContext,
};
use crate::utils::estimate::estimate_context_tokens;

const CONTEXT_SAFETY_TOKENS: i64 = 4096;
// OpenRouter counts the input with its own tokenizer and rejects input + max_tokens above the
// window before routing. It counted prose up to 12% above the provider's count (#10497).
const CONTEXT_SAFETY_RATIO: f64 = 0.15;
// The 15% margin is kept only while it leaves at least this much output. Otherwise (a nearly full
// large window, or any small window such as a 32k local model) only the fixed margin is kept, and
// output is capped at this size so an OpenRouter overcount still fits for a while before compaction.
const MIN_OUTPUT_WITH_RATIO_MARGIN: i64 = 32768;
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

/// Fit `max_tokens` into the context window, leaving room for the input being counted higher than estimated.
pub fn clamp_max_tokens_to_context(model: &Model, context: &TranscriptContext, max_tokens: i64) -> i64 {
    if model.context_window <= 0 {
        return MIN_MAX_TOKENS.max(max_tokens);
    }
    let input = estimate_context_tokens(context).tokens;
    let ratio_margin = ((input as f64) * CONTEXT_SAFETY_RATIO).ceil() as i64;
    let margin = CONTEXT_SAFETY_TOKENS.max(ratio_margin);
    let mut room = model.context_window - input - margin;
    if room < MIN_OUTPUT_WITH_RATIO_MARGIN {
        room = MIN_OUTPUT_WITH_RATIO_MARGIN.min(model.context_window - input - CONTEXT_SAFETY_TOKENS);
    }
    max_tokens.min(MIN_MAX_TOKENS.max(room))
}

fn thinking_level_to_model(level: Option<ThinkingLevel>) -> ModelThinkingLevel {
    match level {
        None => ModelThinkingLevel::Off,
        Some(ThinkingLevel::Minimal) => ModelThinkingLevel::Minimal,
        Some(ThinkingLevel::Low) => ModelThinkingLevel::Low,
        Some(ThinkingLevel::Medium) => ModelThinkingLevel::Medium,
        Some(ThinkingLevel::High) => ModelThinkingLevel::High,
        Some(ThinkingLevel::XHigh) => ModelThinkingLevel::XHigh,
        Some(ThinkingLevel::Max) => ModelThinkingLevel::Max,
    }
}

pub fn resolve_sampling_params(
    model: &Model,
    thinking_level: ModelThinkingLevel,
    request_params: Option<&SamplingParams>,
) -> Option<SamplingParams> {
    let effective_thinking_level = clamp_thinking_level(model, thinking_level);
    let thinking_level_params = model
        .sampling_params_by_thinking_level
        .as_ref()
        .and_then(|map| map.get(&effective_thinking_level));
    if model.sampling_params.is_none() && thinking_level_params.is_none() && request_params.is_none() {
        return None;
    }
    let mut merged = SamplingParams::new();
    for source in [model.sampling_params.as_ref(), thinking_level_params, request_params]
        .into_iter()
        .flatten()
    {
        for (k, v) in source {
            merged.insert(k.clone(), v.clone());
        }
    }
    Some(merged)
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
    let sampling_params = resolve_sampling_params(
        model,
        thinking_level_to_model(options.and_then(|o| o.reasoning)),
        base.and_then(|b| b.sampling_params.as_ref()),
    );

    StreamOptions {
        temperature: base.and_then(|b| b.temperature),
        sampling_params,
        max_tokens: Some(clamp_max_tokens_to_context(model, context, requested_max)),
        signal: base.and_then(|b| b.signal.clone()),
        telemetry_context: base.and_then(|b| b.telemetry_context.clone()),
        telemetry: base.and_then(|b| b.telemetry.clone()),
        api_key: resolved_api_key,
        fetch: base.and_then(|b| b.fetch.clone()),
        transport: base.and_then(|b| b.transport),
        cache_retention: base.and_then(|b| b.cache_retention),
        session_id: base.and_then(|b| b.session_id.clone()),
        disable_continuation: base.and_then(|b| b.disable_continuation),
        headers: base.and_then(|b| b.headers.clone()),
        identity: base.and_then(|b| b.identity.clone()),
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
