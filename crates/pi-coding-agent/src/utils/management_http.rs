//! Port of packages/coding-agent/src/utils/management-http.ts

use std::sync::LazyLock;

use indexmap::IndexSet;
use pi_js::abort::AbortSignal;
use pi_js::fetch::{Request, Response, fetch};
use pi_js::{Error, Result};

static RETRYABLE_STATUS_CODES: LazyLock<IndexSet<u16>> =
    LazyLock::new(|| [408, 425, 429, 500, 502, 503, 504].into_iter().collect());

#[derive(Clone, Debug, Default)]
pub struct FetchRetryOptions {
    /// Number of additional attempts after the initial request. Defaults to two.
    pub max_retries: Option<f64>,
    /// Retry transient HTTP responses as well as transport failures. Defaults to true.
    pub retry_on_status: Option<bool>,
    /// Overall time budget shared by all attempts.
    pub timeout_ms: Option<u64>,
    /// Per-attempt timeout. A new timeout is created for every attempt.
    pub attempt_timeout_ms: Option<u64>,
}

/// Fetch a management HTTP resource with a bounded immediate retry.
///
/// This is intentionally a transport-level helper for idempotent management
/// requests (version checks, catalogs, and downloads). It must not be used for
/// agent/model operations: those can fail after the HTTP request starts and are
/// retried by their semantic caller instead.
///
/// Caller cancellation and timeoutMs are terminal. attemptTimeoutMs aborts
/// only the current attempt so a hung connection can be retried.
pub async fn fetch_with_retry(req: Request, options: FetchRetryOptions) -> Result<Response> {
    let max_retries = match options.max_retries {
        None => 2usize,
        Some(n) if !n.is_finite() => 2usize,
        Some(n) => n.floor().max(0.0) as usize,
    };
    let retry_on_status = options.retry_on_status.unwrap_or(true);
    let parent_signal = req.signal.clone();
    let timeout_signal = options.timeout_ms.filter(|&ms| ms > 0).map(AbortSignal::timeout);
    let attempt_timeout_ms = options.attempt_timeout_ms.filter(|&ms| ms > 0);

    let mut attempt = 0usize;
    loop {
        if let Some(ref s) = parent_signal {
            s.throw_if_aborted()?;
        }
        if let Some(ref s) = timeout_signal {
            s.throw_if_aborted()?;
        }
        let attempt_timeout_signal = attempt_timeout_ms.map(AbortSignal::timeout);
        let mut signals = Vec::new();
        if let Some(ref s) = parent_signal {
            signals.push(s.clone());
        }
        if let Some(ref s) = timeout_signal {
            signals.push(s.clone());
        }
        if let Some(ref s) = attempt_timeout_signal {
            signals.push(s.clone());
        }
        let signal = match signals.len() {
            0 => None,
            1 => signals.pop(),
            _ => Some(AbortSignal::any(&signals)),
        };

        let mut attempt_req = req.clone();
        attempt_req.signal = signal;

        match fetch(attempt_req).await {
            Ok(response) => {
                let should_retry =
                    retry_on_status && RETRYABLE_STATUS_CODES.contains(&response.status()) && attempt < max_retries;
                if !should_retry {
                    return Ok(response);
                }
                drop(response);
            }
            Err(error) => {
                let attempt_timed_out = attempt_timeout_signal.as_ref().is_some_and(AbortSignal::aborted)
                    && !parent_signal.as_ref().is_some_and(AbortSignal::aborted)
                    && !timeout_signal.as_ref().is_some_and(AbortSignal::aborted);
                if parent_signal.as_ref().is_some_and(AbortSignal::aborted)
                    || timeout_signal.as_ref().is_some_and(AbortSignal::aborted)
                    || (error.name() == "AbortError" && !attempt_timed_out && timeout_signal.is_none())
                    || attempt >= max_retries
                {
                    return Err(error);
                }
            }
        }
        attempt += 1;
    }
}

pub fn extract_error_cause_chain(err: &Error) -> Vec<String> {
    let mut messages = Vec::new();
    messages.push(err.message());
    let mut current: Option<&Error> = match err {
        Error::Js(js) => js.cause.as_deref(),
        _ => None,
    };
    let mut depth = 0usize;
    while let Some(c) = current {
        if depth >= 5 {
            break;
        }
        let msg = c.message();
        if !messages.contains(&msg) {
            messages.push(msg);
        }
        current = match c {
            Error::Js(js) => js.cause.as_deref(),
            _ => None,
        };
        depth += 1;
    }
    messages
}
