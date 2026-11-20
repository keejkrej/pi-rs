//! Port of packages/ai/src/utils/provider-retry.ts

use std::fmt;
use std::future::Future;

use pi_js::abort::{AbortReason, AbortSignal};
use pi_js::fetch::Headers;

use crate::utils::sleep::sleep;

const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;

#[derive(Clone, Debug, Default)]
pub struct ProviderRetryOptions {
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
    pub signal: Option<AbortSignal>,
}

/// Structured HTTP error produced by provider transport calls so retry logic can inspect
/// HTTP status and response headers (`retry-after`, `retry-after-ms`, `x-should-retry`).
#[derive(Clone, Debug)]
pub struct ProviderHttpError {
    pub status: Option<u16>,
    pub headers: Option<Headers>,
    pub message: String,
}

impl ProviderHttpError {
    pub fn new(status: Option<u16>, headers: Option<Headers>, message: impl Into<String>) -> Self {
        Self {
            status,
            headers,
            message: message.into(),
        }
    }

    pub fn into_js_error(self) -> pi_js::Error {
        pi_js::Error::typed("ProviderHttpError", self)
    }
}

impl fmt::Display for ProviderHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderHttpError {}

impl From<ProviderHttpError> for pi_js::Error {
    fn from(value: ProviderHttpError) -> Self {
        value.into_js_error()
    }
}

fn is_retryable_provider_error(error: &ProviderHttpError) -> bool {
    if let Some(headers) = &error.headers {
        if let Some(should_retry) = headers.get("x-should-retry") {
            if should_retry == "true" {
                return true;
            }
            if should_retry == "false" {
                return false;
            }
        }
    }

    let Some(status) = error.status else {
        return true;
    };

    status == 408 || status == 409 || status == 429 || status >= 500
}

fn validate_server_retry_delay_ms(
    delay_ms: f64,
    max_retry_delay_ms: Option<u64>,
    provider_error_message: &str,
) -> pi_js::Result<u64> {
    let max_delay_ms = max_retry_delay_ms.unwrap_or(DEFAULT_MAX_RETRY_DELAY_MS);
    if max_delay_ms > 0 && delay_ms > max_delay_ms as f64 {
        let requested_secs = (delay_ms / 1000.0).ceil() as u64;
        let max_secs = (max_delay_ms as f64 / 1000.0).ceil() as u64;
        return Err(pi_js::Error::msg(format!(
            "Server requested {requested_secs}s retry delay (max: {max_secs}s). {provider_error_message}"
        )));
    }
    Ok(if delay_ms <= 0.0 { 0 } else { delay_ms.round() as u64 })
}

fn get_retry_delay_ms(
    error: &ProviderHttpError,
    retry_index: u32,
    max_retry_delay_ms: Option<u64>,
) -> pi_js::Result<u64> {
    if let Some(headers) = &error.headers {
        if let Some(retry_after_ms) = headers.get("retry-after-ms") {
            let value = pi_js::num::parse_float(&retry_after_ms);
            if value.is_finite() {
                return validate_server_retry_delay_ms(value, max_retry_delay_ms, &error.message);
            }
        }

        if let Some(retry_after) = headers.get("retry-after") {
            let seconds = pi_js::num::parse_float(&retry_after);
            let delay_ms = if seconds.is_nan() {
                pi_js::time::parse_date(&retry_after)
                    .map(|target| (target - pi_js::time::now_ms()) as f64)
                    .unwrap_or(f64::NAN)
            } else {
                seconds * 1000.0
            };
            if delay_ms.is_finite() {
                return validate_server_retry_delay_ms(delay_ms, max_retry_delay_ms, &error.message);
            }
        }
    }

    let exponential_delay = (0.5_f64 * 2_f64.powi(retry_index as i32)).min(8.0) * 1000.0;
    let jittered = exponential_delay * (1.0 - pi_js::crypto::math_random() * 0.25);
    Ok(if jittered <= 0.0 { 0 } else { jittered as u64 })
}

fn create_abort_error() -> pi_js::Error {
    pi_js::Error::Abort(AbortReason::error("AbortError", "Request aborted"))
}

async fn abortable_sleep(ms: u64, signal: Option<&AbortSignal>) -> pi_js::Result<()> {
    if signal.is_some_and(AbortSignal::aborted) {
        return Err(create_abort_error());
    }
    let result = if let Some(sig) = signal {
        sleep(ms, sig).await
    } else {
        pi_js::time::sleep(ms, None).await
    };
    result.map_err(|err| if err.is_abort() { create_abort_error() } else { err })
}

/// Reproduce the retry behavior used by the OpenAI and Anthropic SDKs while making
/// their backoff sleep interruptible.
pub async fn retry_provider_request<T, F, Fut>(request: F, options: ProviderRetryOptions) -> pi_js::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = pi_js::Result<T>>,
{
    retry_provider_request_with_no_retry_statuses(request, options, None).await
}

/// Variant of [`retry_provider_request`] that accepts explicit `no_retry_statuses` (`v1.5.2` `noRetryStatuses`).
// PORT: Separate helper during Stage 1 avoids adding a field to `ProviderRetryOptions` that would break `src/api/*` literals before Stage 2.
pub async fn retry_provider_request_with_no_retry_statuses<T, F, Fut>(
    mut request: F,
    options: ProviderRetryOptions,
    no_retry_statuses: Option<&[u16]>,
) -> pi_js::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = pi_js::Result<T>>,
{
    let max_retries = options.max_retries.unwrap_or(0);
    let mut retries_remaining = max_retries;

    loop {
        match request().await {
            Ok(val) => return Ok(val),
            Err(error) => {
                if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
                    return Err(create_abort_error());
                }
                let Some(http_error) = error.downcast_ref::<ProviderHttpError>() else {
                    return Err(error);
                };
                if retries_remaining == 0 || !is_retryable_provider_error(http_error) {
                    return Err(error);
                }
                if let Some(status) = http_error.status
                    && no_retry_statuses.is_some_and(|list| list.contains(&status))
                {
                    return Err(error);
                }

                let retry_index = max_retries - retries_remaining;
                retries_remaining -= 1;
                let delay_ms = get_retry_delay_ms(http_error, retry_index, options.max_retry_delay_ms)?;
                abortable_sleep(delay_ms, options.signal.as_ref()).await?;
            }
        }
    }
}
