//! Port of packages/chord/src/context/index.ts

#![allow(dead_code, unused_variables)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use pi_js::abort::{AbortController, AbortReason, AbortSignal};

use crate::types::{Context, ContextKey, ContextToken};

static NEXT_CONTEXT_KEY_ID: AtomicU64 = AtomicU64::new(1);

/// Empty root with no values and no cancellation.
pub static BACKGROUND_CONTEXT: LazyLock<Context> = LazyLock::new(|| Context::empty("[Context BACKGROUND_CONTEXT]"));

/// Empty root for call sites that do not yet thread a caller context.
pub static TODO_CONTEXT: LazyLock<Context> = LazyLock::new(|| Context::empty("[Context TODO_CONTEXT]"));

// PORT: id 0 is reserved for the abort-signal key. `create_context_key` must allocate from 1.
pub(crate) fn abort_signal_context_key() -> &'static ContextKey<Option<AbortSignal>> {
    static KEY: LazyLock<ContextKey<Option<AbortSignal>>> =
        LazyLock::new(|| ContextKey::from_token(ContextToken::new(0, "chord.abortSignal")));
    &KEY
}

pub fn create_context_key<T>(description: &str) -> ContextKey<T> {
    let id = NEXT_CONTEXT_KEY_ID.fetch_add(1, Ordering::Relaxed);
    ContextKey::from_token(ContextToken::new(id, description))
}

/// Derive a context containing one additional or replaced value.
pub fn with_context_value<T>(key: ContextKey<T>, value: T, parent: Context) -> Context
where
    T: Clone + Send + Sync + 'static,
{
    Context::with_value(key, value, parent)
}

/// Derive a context cancelled by either the parent signal or the supplied signal.
/// The parent context remains unchanged.
pub fn with_abort_signal(signal: AbortSignal, context: Context) -> Context {
    let combined = match context.abort_signal() {
        None => signal,
        Some(parent_signal) => AbortSignal::any(&[parent_signal, signal]),
    };
    with_context_value(abort_signal_context_key().clone(), Some(combined), context)
}

/// Derive a context retaining all values except caller cancellation. Intended for mandatory cleanup only.
pub fn without_abort_signal(context: Context) -> Context {
    with_context_value(abort_signal_context_key().clone(), None, context)
}

/// Independently cancellable child context.
pub struct WithCancel {
    pub context: Context,
    pub cancel: Arc<dyn Fn(Option<AbortReason>) + Send + Sync>,
}

/// Derive an independently cancellable child context.
pub fn with_cancel(context: Context) -> WithCancel {
    let controller = Arc::new(AbortController::new());
    let signal = controller.signal();
    let cancel_controller = Arc::clone(&controller);
    WithCancel {
        context: with_abort_signal(signal, context),
        cancel: Arc::new(move |reason| cancel_controller.abort(reason)),
    }
}

/// Observe a promise until it settles or the invocation is cancelled.
/// Cancellation rejects only this waiter; it does not cancel the underlying promise.
pub async fn await_with_context<T: Send>(
    promise: impl std::future::Future<Output = pi_js::Result<T>> + Send,
    context: Context,
) -> pi_js::Result<T> {
    let Some(signal) = context.abort_signal() else {
        return promise.await;
    };
    if signal.aborted() {
        return Err(abort_error(&signal));
    }
    tokio::select! {
        biased;
        _ = signal.cancelled() => Err(abort_error(&signal)),
        res = promise => res,
    }
}

fn abort_error(signal: &AbortSignal) -> pi_js::Error {
    match signal.reason() {
        Some(reason) if reason.value.is_none() => pi_js::Error::Abort(reason),
        _ => pi_js::Error::Abort(AbortReason::error("AbortError", "The operation was aborted")),
    }
}
