//! Port of packages/ai/src/utils/retry.ts

use std::future::Future;
use std::sync::OnceLock;

use async_trait::async_trait;
use pi_js::abort::AbortSignal;
use serde::{Deserialize, Serialize};

use crate::types::{AssistantMessage, StopReason};
use crate::utils::sleep::sleep;

const NON_RETRYABLE_PROVIDER_LIMIT_PATTERNS: &[&str] = &[
    "GoUsageLimitError",
    "FreeUsageLimitError",
    "Monthly usage limit reached",
    "available balance",
    "insufficient_quota",
    "out of budget",
    "quota exceeded",
    "billing",
    "subscription_sharing_usage_limit_exceeded",
];

const RETRYABLE_PROVIDER_ERROR_PATTERNS: &[&str] = &[
    "overloaded",
    "currently experiencing high demand",
    "rate.?limit",
    "too many requests",
    "429",
    "500",
    "502",
    "503",
    "504",
    "520",
    "524",
    "service.?unavailable",
    "server.?error",
    "internal.?error",
    "provider.?returned.?error",
    "exceeded request buffer limit while retrying upstream",
    "network.?error",
    "connection.?error",
    "connection.?refused",
    "connection.?lost",
    "other side closed",
    "fetch failed",
    "getaddrinfo",
    "ENOTFOUND",
    "EAI_AGAIN",
    "upstream.?connect",
    "reset before headers",
    "socket hang up",
    "socket connection was closed",
    "timed? out",
    "timeout",
    "terminated",
    "websocket.?closed",
    "websocket.?error",
    "ended without",
    "stream ended before message_stop",
    "stream ended before a terminal response event",
    "http2 request did not get a response",
    "retry delay",
    "you can retry your request",
    "try your request again",
    "please retry your request",
    "ResourceExhausted",
    "subscription_sharing_usage_unavailable",
    "subscription_sharing_user_unavailable",
];

type Matcher = Box<dyn Fn(&str) -> bool + Send + Sync>;

fn compile_matcher(pattern: &str) -> Option<Matcher> {
    let re = pi_js::regex::ecma(pattern, "i").ok()?;
    Some(Box::new(move |text: &str| pi_js::regex::test(&re, text)))
}

fn non_retryable_pattern() -> Option<&'static Matcher> {
    static RE: OnceLock<Option<Matcher>> = OnceLock::new();
    RE.get_or_init(|| compile_matcher(&NON_RETRYABLE_PROVIDER_LIMIT_PATTERNS.join("|")))
        .as_ref()
}

fn retryable_pattern() -> Option<&'static Matcher> {
    static RE: OnceLock<Option<Matcher>> = OnceLock::new();
    RE.get_or_init(|| compile_matcher(&RETRYABLE_PROVIDER_ERROR_PATTERNS.join("|")))
        .as_ref()
}

pub const DEFAULT_MAX_AGENT_RETRY_DELAY_MS: u64 = 60_000;

/// Retry policy: bounded attempts with exponential backoff (`base_delay_ms * 2^(attempt-1)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryPolicy {
    pub enabled: bool,
    /// Max retry attempts (0 = no retries). The initial call never counts as a retry.
    pub max_retries: u32,
    /// Base delay in ms. Per-attempt delay is `base_delay_ms * 2^(attempt-1)` before jitter.
    pub base_delay_ms: u64,
    /// Optional cap for agent-level retry delays in ms. Defaults to 60 seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_agent_delay_ms: Option<u64>,
}

pub fn retry_delay_ms(policy: &RetryPolicy, attempt: u32) -> u64 {
    let shift = attempt.saturating_sub(1);
    let factor = 2u128.saturating_pow(shift);
    let delay = (policy.base_delay_ms as u128).saturating_mul(factor);
    let cap = policy.max_agent_delay_ms.unwrap_or(DEFAULT_MAX_AGENT_RETRY_DELAY_MS) as u128;
    delay.min(cap) as u64
}

/// Optional callbacks emitted by [`retry_assistant_call`] around each retry.
#[async_trait]
pub trait RetryCallbacks: Send + Sync {
    /// Emitted before the backoff sleep of each retry attempt (1-indexed).
    async fn on_retry_scheduled(&self, _attempt: u32, _max_attempts: u32, _delay_ms: u64, _error_message: &str) {}

    /// Emitted after the backoff sleep, immediately before the retried call starts.
    async fn on_retry_attempt_start(&self) {}

    /// Emitted once when the loop ends: `success` if a later call completed normally.
    async fn on_retry_finished(&self, _success: bool, _attempt: u32, _final_error: Option<&str>) {}
}

/// Run a single assistant-producing call with bounded retry on transient errors.
pub async fn retry_assistant_call<F, Fut>(
    mut produce: F,
    policy: Option<&RetryPolicy>,
    signal: Option<&AbortSignal>,
    callbacks: Option<&dyn RetryCallbacks>,
) -> pi_js::Result<AssistantMessage>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = pi_js::Result<AssistantMessage>>,
{
    let max_attempts = match policy {
        Some(p) if p.enabled => p.max_retries,
        _ => 0,
    };

    let mut attempt: u32 = 0;
    let mut last_retry: Option<(u32, String)> = None;

    loop {
        let response = produce().await?;

        // Abort: terminal but not successful. Never retry an aborted message.
        if response.stop_reason == StopReason::Aborted {
            if let Some((last_attempt, _)) = &last_retry {
                if let Some(cb) = callbacks {
                    cb.on_retry_finished(false, *last_attempt, None).await;
                }
            }
            return Ok(response);
        }

        // Success: non-error, non-abort responses return as-is.
        if response.stop_reason != StopReason::Error {
            if let Some((last_attempt, _)) = &last_retry {
                if let Some(cb) = callbacks {
                    cb.on_retry_finished(true, *last_attempt, None).await;
                }
            }
            return Ok(response);
        }

        // Non-retryable, or budget exhausted: return the final error message.
        if attempt >= max_attempts || !is_retryable_assistant_error(&response) {
            if let Some((last_attempt, _)) = &last_retry {
                if let Some(cb) = callbacks {
                    cb.on_retry_finished(false, *last_attempt, response.error_message.as_deref())
                        .await;
                }
            }
            return Ok(response);
        }

        attempt += 1;
        let err_msg = response
            .error_message
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("Unknown error")
            .to_string();
        last_retry = Some((attempt, err_msg.clone()));

        let delay_ms = match policy {
            Some(p) => retry_delay_ms(p, attempt),
            None => 0,
        };

        if let Some(cb) = callbacks {
            cb.on_retry_scheduled(attempt, max_attempts, delay_ms, &err_msg).await;
        }

        let sleep_res = if let Some(sig) = signal {
            sleep(delay_ms, sig).await
        } else {
            pi_js::time::sleep(delay_ms, None).await
        };

        if let Err(err) = sleep_res {
            if let Some(cb) = callbacks {
                cb.on_retry_finished(false, attempt, Some(&err_msg)).await;
            }
            if err.is_abort() {
                let mut aborted = response;
                aborted.error_message = None;
                aborted.stop_reason = StopReason::Aborted;
                return Ok(aborted);
            }
            return Err(err);
        }

        if let Some(cb) = callbacks {
            cb.on_retry_attempt_start().await;
        }
    }
}

/// Classifies whether a failed assistant message looks like a transient provider
/// or transport error, so callers can decide if the last assistant turn should be
/// restarted.
pub fn is_retryable_assistant_error(message: &AssistantMessage) -> bool {
    if message.stop_reason != StopReason::Error {
        return false;
    }
    let Some(error_message) = message.error_message.as_deref() else {
        return false;
    };
    if error_message.is_empty() {
        return false;
    }
    if non_retryable_pattern().is_some_and(|m| m(error_message)) {
        return false;
    }
    retryable_pattern().is_some_and(|m| m(error_message))
}
