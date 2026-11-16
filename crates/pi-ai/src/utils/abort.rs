//! Port of packages/ai/src/utils/abort.ts

use std::future::Future;

use pi_js::abort::{AbortReason, AbortSignal};
use pi_js::{Error, Result};

fn abort_error(signal: &AbortSignal) -> Error {
    Error::Abort(signal.reason().unwrap_or_else(AbortReason::abort))
}

/// Create an operation-local signal for public APIs whose signal is optional.
pub fn operation_signal(signal: Option<&AbortSignal>) -> AbortSignal {
    signal.cloned().unwrap_or_else(AbortSignal::never)
}

/// Stop waiting for an operation when its signal aborts while dropping any uncompleted
/// operation future cleanly.
pub async fn race_with_abort_signal<F, T>(operation: F, signal: &AbortSignal) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    signal.throw_if_aborted()?;

    tokio::select! {
        biased;
        _ = signal.cancelled() => Err(abort_error(signal)),
        res = operation => res,
    }
}
