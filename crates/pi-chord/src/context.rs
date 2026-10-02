//! Port of packages/chord/src/context/index.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, LazyLock};

use crate::types::{Context, ContextKey, ContextToken};

/// Empty root with no values and no cancellation.
pub static BACKGROUND_CONTEXT: LazyLock<Context> = LazyLock::new(|| Context::empty("[Context BACKGROUND_CONTEXT]"));

/// Empty root for call sites that do not yet thread a caller context.
pub static TODO_CONTEXT: LazyLock<Context> = LazyLock::new(|| Context::empty("[Context TODO_CONTEXT]"));

// PORT: id 0 is reserved for the abort-signal key. `create_context_key` must allocate from 1.
fn abort_signal_context_key() -> &'static ContextKey<Option<pi_js::abort::AbortSignal>> {
    static KEY: LazyLock<ContextKey<Option<pi_js::abort::AbortSignal>>> =
        LazyLock::new(|| ContextKey::from_token(ContextToken::new(0, "chord.abortSignal")));
    &KEY
}

pub fn create_context_key<T>(description: &str) -> ContextKey<T> {
    todo!("port: create_context_key")
}

/// Derive a context containing one additional or replaced value.
pub fn with_context_value<T>(key: ContextKey<T>, value: T, parent: Context) -> Context
where
    T: Clone + Send + Sync + 'static,
{
    todo!("port: with_context_value")
}

/// Derive a context cancelled by either the parent signal or the supplied signal.
/// The parent context remains unchanged.
pub fn with_abort_signal(signal: pi_js::abort::AbortSignal, context: Context) -> Context {
    todo!("port: with_abort_signal")
}

/// Derive a context retaining all values except caller cancellation. Intended for mandatory cleanup only.
pub fn without_abort_signal(context: Context) -> Context {
    todo!("port: without_abort_signal")
}

/// Independently cancellable child context.
pub struct WithCancel {
    pub context: Context,
    pub cancel: Arc<dyn Fn(Option<pi_js::abort::AbortReason>) + Send + Sync>,
}

/// Derive an independently cancellable child context.
pub fn with_cancel(context: Context) -> WithCancel {
    todo!("port: with_cancel")
}

/// Observe a promise until it settles or the invocation is cancelled.
/// Cancellation rejects only this waiter; it does not cancel the underlying promise.
pub async fn await_with_context<T: Send>(
    promise: impl std::future::Future<Output = pi_js::Result<T>> + Send,
    context: Context,
) -> pi_js::Result<T> {
    todo!("port: await_with_context")
}

fn abort_error(signal: &pi_js::abort::AbortSignal) -> pi_js::Error {
    todo!("port: abort_error")
}
