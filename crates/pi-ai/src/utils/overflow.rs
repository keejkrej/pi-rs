//! Port of packages/ai/src/utils/overflow.ts

use std::sync::{Arc, OnceLock};

use crate::types::{AssistantMessage, StopReason};

const OVERFLOW_PATTERN_SOURCES: &[&str] = &[
    r"prompt (?:is )?too long",               // Anthropic and z.ai token overflow
    r"prompt exceeds max length",             // z.ai CN endpoint token overflow
    r"request_too_large",                     // Anthropic request byte-size overflow (HTTP 413)
    r"input is too long for requested model", // Amazon Bedrock
    r"exceeds the context window",            // OpenAI (Completions & Responses API)
    r"exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))", // OpenAI-compatible proxies (LiteLLM)
    r"context window exceeded", // LiteLLM pre-call check (ContextWindowExceededError)
    r"input token count.*exceeds the maximum", // Google (Gemini)
    r"maximum prompt length is \d+", // xAI (Grok)
    r"reduce the length of the messages", // Groq
    r"maximum context length is \d+ tokens", // OpenRouter (most backends)
    r"exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?", // OpenRouter/Poolside
    r"input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)", // Together AI
    r"exceeds the limit of \d+", // GitHub Copilot
    r"exceeds the available context size", // llama.cpp server
    r"greater than the context length", // LM Studio
    r"context window exceeds limit", // MiniMax
    r"exceeded model token limit", // Kimi For Coding
    r"too large for model with \d+ maximum context length", // Mistral
    r"prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?", // DS4 server
    r"model_context_window_exceeded", // z.ai non-standard finish_reason surfaced as error text
    r"prompt too long; exceeded (?:max )?context length", // Ollama explicit overflow error
    r"range of input length should be", // DashScope / Qwen Token Plan
    r"context[_ ]length[_ ]exceeded", // Generic fallback
    r"too many tokens",         // Generic fallback
    r"token limit exceeded",    // Generic fallback
    // HTTP 413 means request-size overflow regardless of provider or body text.
    // Match raw SDK errors and formatProviderError's prefix, not a status mentioned in the body.
    r"^(?:[\w -]+ API error \(413\):|413(?:\s|:|$))",
];

const CEREBRAS_BODYLESS_OVERFLOW_SOURCE: &str = r"^4(?:00|13)\s*(?:status code)?\s*\(no body\)";

const NON_OVERFLOW_PATTERN_SOURCES: &[&str] = &[
    r"^(Throttling error|Service unavailable):",
    r"rate limit",
    r"too many requests",
];

#[derive(Clone)]
pub struct OverflowPattern {
    pub source: &'static str,
    matcher: Arc<dyn Fn(&str) -> bool + Send + Sync>,
}

impl OverflowPattern {
    pub fn test(&self, text: &str) -> bool {
        (self.matcher)(text)
    }
}

fn compile_patterns(sources: &[&'static str]) -> Vec<OverflowPattern> {
    sources
        .iter()
        .filter_map(|&src| {
            let re = pi_js::regex::ecma(src, "i").ok()?;
            Some(OverflowPattern {
                source: src,
                matcher: Arc::new(move |text: &str| pi_js::regex::test(&re, text)),
            })
        })
        .collect()
}

fn overflow_patterns() -> &'static [OverflowPattern] {
    static RE: OnceLock<Vec<OverflowPattern>> = OnceLock::new();
    RE.get_or_init(|| compile_patterns(OVERFLOW_PATTERN_SOURCES))
}

fn cerebras_pattern() -> Option<&'static OverflowPattern> {
    static RE: OnceLock<Option<OverflowPattern>> = OnceLock::new();
    RE.get_or_init(|| {
        let re = pi_js::regex::ecma(CEREBRAS_BODYLESS_OVERFLOW_SOURCE, "i").ok()?;
        Some(OverflowPattern {
            source: CEREBRAS_BODYLESS_OVERFLOW_SOURCE,
            matcher: Arc::new(move |text: &str| pi_js::regex::test(&re, text)),
        })
    })
    .as_ref()
}

fn non_overflow_patterns() -> &'static [OverflowPattern] {
    static RE: OnceLock<Vec<OverflowPattern>> = OnceLock::new();
    RE.get_or_init(|| compile_patterns(NON_OVERFLOW_PATTERN_SOURCES))
}

/// Check if an assistant message represents a context overflow error.
///
/// This handles three cases:
/// 1. Error-based overflow (`stop_reason == StopReason::Error` matching known provider overflow text).
/// 2. Silent overflow (z.ai style): `stop_reason == StopReason::Stop` with `input + cache_read > context_window`.
/// 3. Length-stop overflow (Xiaomi MiMo style): `stop_reason == StopReason::Length` with `output == 0`
///    and `input + cache_read >= context_window * 0.99`.
pub fn is_context_overflow(message: &AssistantMessage, context_window: Option<i64>) -> bool {
    // Case 1: Check error message patterns
    if message.stop_reason == StopReason::Error {
        if let Some(err_msg) = message.error_message.as_deref() {
            if !err_msg.is_empty() {
                let is_non_overflow = non_overflow_patterns().iter().any(|p| p.test(err_msg));
                if !is_non_overflow {
                    if overflow_patterns().iter().any(|p| p.test(err_msg)) {
                        return true;
                    }
                    if message.provider == "cerebras" {
                        if let Some(re) = cerebras_pattern() {
                            if re.test(err_msg) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }

    // Case 2: Silent overflow (z.ai style) - successful but usage exceeds context
    if let Some(cw) = context_window.filter(|&w| w > 0) {
        if message.stop_reason == StopReason::Stop {
            let input_tokens = message.usage.input + message.usage.cache_read;
            if input_tokens > cw {
                return true;
            }
        }

        // Case 3: Length-stop overflow (Xiaomi MiMo style)
        if message.stop_reason == StopReason::Length && message.usage.output == 0 {
            let input_tokens = message.usage.input + message.usage.cache_read;
            if (input_tokens as f64) >= (cw as f64) * 0.99 {
                return true;
            }
        }
    }

    false
}

/// Check whether a length stop ended below the caller or model's intended output limit.
pub fn is_recoverable_length(message: &AssistantMessage, desired_max_output: i64) -> bool {
    message.stop_reason == StopReason::Length && desired_max_output > 0 && message.usage.output < desired_max_output
}

/// Get the overflow patterns for testing purposes.
pub fn get_overflow_patterns() -> Vec<OverflowPattern> {
    overflow_patterns().to_vec()
}
