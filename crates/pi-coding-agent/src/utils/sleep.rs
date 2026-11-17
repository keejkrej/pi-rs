//! Port of packages/coding-agent/src/utils/sleep.ts

use pi_js::abort::AbortSignal;
use pi_js::{Error, Result};

/// Sleep helper that respects abort signal.
pub async fn sleep(ms: u64, signal: Option<&AbortSignal>) -> Result<()> {
    if signal.is_some_and(AbortSignal::aborted) {
        return Err(Error::msg("Aborted"));
    }

    pi_js::time::sleep(ms, signal)
        .await
        .map_err(|err| if err.is_abort() { Error::msg("Aborted") } else { err })
}
