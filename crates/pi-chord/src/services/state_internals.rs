//! Port of packages/chord/src/services/state-internals.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex, Weak};

/// Atomically captured immutable value and its matching publication sequence.
#[derive(Clone, Debug)]
pub struct ReplicatedStateInternalsSnapshot {
    pub value: serde_json::Value,
    pub sequence: i64,
}

/// Source listener: `(ops, sequence, context) => void`.
pub type ReplicatedStateOpsListener = Arc<dyn Fn(&[crate::delta::Op], i64, crate::types::Context) + Send + Sync>;

/// Fallible source listener used when listener errors should propagate to the publisher caller.
pub type ReplicatedStateFallibleOpsListener =
    Arc<dyn Fn(&[crate::delta::Op], i64, crate::types::Context) -> pi_js::Result<()> + Send + Sync>;

pub trait ReplicatedStateInternals: Send + Sync {
    /// Atomically capture the immutable value and its matching publication sequence.
    fn snapshot(&self) -> ReplicatedStateInternalsSnapshot;

    fn subscribe(&self, listener: ReplicatedStateOpsListener) -> pi_js::Unsubscribe;

    fn subscribe_fallible(&self, listener: ReplicatedStateFallibleOpsListener) -> pi_js::Unsubscribe {
        self.subscribe(Arc::new(move |ops, seq, ctx| {
            let _ = listener(ops, seq, ctx);
        }))
    }
}

struct RegisteredSource {
    object: Weak<dyn std::any::Any + Send + Sync>,
    internals: Arc<dyn ReplicatedStateInternals>,
}

// PORT: JS WeakMap keyed by object identity. Entries die with the state object.
static SOURCES: LazyLock<Mutex<Vec<RegisteredSource>>> = LazyLock::new(|| Mutex::new(Vec::new()));

pub fn register_replicated_state_internals(
    value: Arc<dyn std::any::Any + Send + Sync>,
    internals: Arc<dyn ReplicatedStateInternals>,
) {
    let mut sources = SOURCES.lock().unwrap();
    sources.retain(|entry| entry.object.strong_count() > 0);
    sources.push(RegisteredSource {
        object: Arc::downgrade(&value),
        internals,
    });
}

pub(crate) fn get_replicated_state_internals_by_ptr(ptr: *const ()) -> Option<Arc<dyn ReplicatedStateInternals>> {
    let mut sources = SOURCES.lock().unwrap();
    sources.retain(|entry| entry.object.strong_count() > 0);
    for entry in sources.iter() {
        let entry_ptr = entry.object.as_ptr() as *const ();
        if std::ptr::eq(entry_ptr, ptr) {
            return Some(Arc::clone(&entry.internals));
        }
    }
    None
}

pub fn get_replicated_state_internals(value: &dyn std::any::Any) -> Option<Arc<dyn ReplicatedStateInternals>> {
    if let Some(state) = value.downcast_ref::<crate::types::ReplicatedState<crate::types::JsonValue>>() {
        return state.internals();
    }
    if let Some(state) = value.downcast_ref::<crate::types::AttachedReplicatedState<crate::types::JsonValue>>() {
        return Some(state.internals());
    }
    if let Some(state) = value.downcast_ref::<crate::types::MutableReplicatedState<crate::types::JsonValue>>() {
        return Some(state.internals());
    }
    if let Some(state) = value.downcast_ref::<crate::services::handle::ServiceObject>() {
        return state.state_internals();
    }
    let ptr = value as *const dyn std::any::Any as *const ();
    get_replicated_state_internals_by_ptr(ptr)
}
