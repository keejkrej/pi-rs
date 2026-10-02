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

pub trait ReplicatedStateInternals: Send + Sync {
    /// Atomically capture the immutable value and its matching publication sequence.
    fn snapshot(&self) -> ReplicatedStateInternalsSnapshot;

    fn subscribe(&self, listener: ReplicatedStateOpsListener) -> pi_js::Unsubscribe;
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
    todo!("port: register_replicated_state_internals")
}

pub fn get_replicated_state_internals(value: &dyn std::any::Any) -> Option<Arc<dyn ReplicatedStateInternals>> {
    todo!("port: get_replicated_state_internals")
}
