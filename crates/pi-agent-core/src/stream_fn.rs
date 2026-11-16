//! Port of packages/agent/src/stream-fn.ts

use std::sync::{LazyLock, Mutex};

use crate::types::StreamFn;

static DEFAULT_STREAM_FN: LazyLock<Mutex<Option<StreamFn>>> = LazyLock::new(|| Mutex::new(None));

/// Sets the module-level fallback [`StreamFn`] used when neither [`Agent`](crate::agent::Agent)
/// nor [`AgentLoopConfig`](crate::types::AgentLoopConfig) specifies one.
///
/// Pass `None` to reset back to [`pi_ai::compat::stream_simple`].
pub fn set_default_stream_fn(stream_fn: Option<StreamFn>) {
    *DEFAULT_STREAM_FN.lock().unwrap_or_else(|e| e.into_inner()) = stream_fn;
}

/// Returns the currently registered module-level fallback [`StreamFn`], if any.
pub fn get_default_stream_fn() -> Option<StreamFn> {
    DEFAULT_STREAM_FN.lock().unwrap_or_else(|e| e.into_inner()).clone()
}
