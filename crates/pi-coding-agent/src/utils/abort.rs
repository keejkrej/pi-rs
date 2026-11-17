//! Port of packages/coding-agent/src/utils/abort.ts

use std::future::Future;

use pi_js::abort::{AbortController, AbortReason, AbortSignal};
use pi_js::{Error, Result};

fn abort_error(signal: &AbortSignal) -> Error {
    match signal.reason() {
        Some(reason) => Error::Abort(reason),
        None => Error::Abort(AbortReason::abort()),
    }
}

/// Normalize an optional public signal without imposing a deadline.
pub fn operation_signal(signal: Option<AbortSignal>) -> AbortSignal {
    signal.unwrap_or_else(|| AbortController::new().signal())
}

/// Stop waiting on abort while observing the abandoned operation through settlement.
pub async fn race_with_abort_signal<T, F>(operation: F, signal: Option<&AbortSignal>) -> Result<T>
where
    F: Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    let Some(signal) = signal else {
        return operation.await;
    };
    if signal.aborted() {
        // Keep observing the abandoned operation in the background so its side effects continue as in TS.
        tokio::spawn(async move {
            let _ = operation.await;
        });
        return Err(abort_error(signal));
    }

    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = tx.send(operation.await);
    });

    tokio::select! {
        biased;
        _ = signal.cancelled() => {
            // Keep the spawned task running in the background while stopping our wait immediately.
            Err(abort_error(signal))
        }
        res = rx => {
            match res {
                Ok(inner) => inner,
                Err(_) => Err(Error::msg("aborted task dropped before settling")),
            }
        }
    }
}
