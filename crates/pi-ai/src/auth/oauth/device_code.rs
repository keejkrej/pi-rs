//! Port of packages/ai/src/auth/oauth/device-code.ts

use std::future::Future;

use pi_js::abort::AbortSignal;
use pi_js::{Error, Result};

const CANCEL_MESSAGE: &str = "Login cancelled";
const TIMEOUT_MESSAGE: &str = "Device flow timed out";
const SLOW_DOWN_TIMEOUT_MESSAGE: &str = "Device flow timed out after one or more slow_down responses. This is often caused by clock drift in WSL or VM environments. Please sync or restart the VM clock and try again.";
const MINIMUM_INTERVAL_MS: f64 = 1000.0;
// RFC 8628 section 3.2: if the authorization server omits `interval`, the client must use 5 seconds.
const DEFAULT_POLL_INTERVAL_SECONDS: f64 = 5.0;
// RFC 8628 section 3.5: `slow_down` means the polling interval must increase by 5 seconds.
const SLOW_DOWN_INTERVAL_INCREMENT_MS: f64 = 5000.0;

#[derive(Clone, Debug, PartialEq)]
pub enum OAuthDeviceCodePollResult<T> {
    Pending,
    SlowDown { interval_seconds: Option<f64> },
    Failed { message: String },
    Complete { value: T },
}

pub struct OAuthDeviceCodePollOptions<'a, T, F> {
    pub interval_seconds: Option<f64>,
    pub expires_in_seconds: Option<f64>,
    pub wait_before_first_poll: bool,
    pub poll: F,
    pub signal: &'a AbortSignal,
    pub _marker: std::marker::PhantomData<T>,
}

impl<'a, T, F> OAuthDeviceCodePollOptions<'a, T, F> {
    pub fn new(signal: &'a AbortSignal, poll: F) -> Self {
        Self {
            interval_seconds: None,
            expires_in_seconds: None,
            wait_before_first_poll: false,
            poll,
            signal,
            _marker: std::marker::PhantomData,
        }
    }
}

pub async fn abortable_sleep(ms: u64, signal: &AbortSignal, cancel_message: &str) -> Result<()> {
    if signal.aborted() {
        return Err(Error::msg(cancel_message));
    }
    crate::utils::sleep::sleep(ms, signal)
        .await
        .map_err(|_| Error::msg(cancel_message))
}

pub async fn poll_oauth_device_code_flow<T, F, Fut>(
    interval_seconds: Option<f64>,
    expires_in_seconds: Option<f64>,
    wait_before_first_poll: bool,
    signal: &AbortSignal,
    mut poll: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<OAuthDeviceCodePollResult<T>>>,
{
    let deadline_ms = match expires_in_seconds {
        Some(secs) => (pi_js::time::now_ms() as f64) + secs * 1000.0,
        None => f64::INFINITY,
    };

    let mut interval_ms = (interval_seconds.unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS) * 1000.0)
        .floor()
        .max(MINIMUM_INTERVAL_MS);
    if !interval_ms.is_finite() {
        interval_ms = MINIMUM_INTERVAL_MS.max(DEFAULT_POLL_INTERVAL_SECONDS * 1000.0);
    }

    let mut slow_down_responses: usize = 0;

    if wait_before_first_poll {
        let remaining_ms = deadline_ms - (pi_js::time::now_ms() as f64);
        if remaining_ms > 0.0 {
            let wait_ms = interval_ms.min(remaining_ms).max(0.0) as u64;
            abortable_sleep(wait_ms, signal, CANCEL_MESSAGE).await?;
        }
    }

    while (pi_js::time::now_ms() as f64) < deadline_ms {
        if signal.aborted() {
            return Err(Error::msg(CANCEL_MESSAGE));
        }

        match poll().await? {
            OAuthDeviceCodePollResult::Complete { value } => return Ok(value),
            OAuthDeviceCodePollResult::Failed { message } => return Err(Error::msg(message)),
            OAuthDeviceCodePollResult::Pending => {}
            OAuthDeviceCodePollResult::SlowDown { interval_seconds } => {
                slow_down_responses += 1;
                interval_ms = match interval_seconds {
                    Some(secs) if secs.is_finite() && secs > 0.0 => (secs * 1000.0).floor().max(MINIMUM_INTERVAL_MS),
                    _ => (interval_ms + SLOW_DOWN_INTERVAL_INCREMENT_MS).max(MINIMUM_INTERVAL_MS),
                };
            }
        }

        let remaining_ms = deadline_ms - (pi_js::time::now_ms() as f64);
        if remaining_ms <= 0.0 {
            break;
        }

        let wait_ms = interval_ms.min(remaining_ms).max(0.0) as u64;
        abortable_sleep(wait_ms, signal, CANCEL_MESSAGE).await?;
    }

    Err(Error::msg(if slow_down_responses > 0 {
        SLOW_DOWN_TIMEOUT_MESSAGE
    } else {
        TIMEOUT_MESSAGE
    }))
}
