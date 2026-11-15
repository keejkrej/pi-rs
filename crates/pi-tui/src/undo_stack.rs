//! Port of packages/tui/src/undo-stack.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

struct UndoStackInner<S> {
    stack: Mutex<Vec<S>>,
}

/// Generic undo stack with clone-on-push semantics.
///
/// Stores deep clones of state snapshots. Popped snapshots are returned
/// directly (no re-cloning) since they are already detached.
///
/// PORT: TS class with identity. `S: Clone` stands in for `structuredClone` of plain data.
#[derive(Clone)]
pub struct UndoStack<S> {
    inner: Arc<UndoStackInner<S>>,
}

impl<S: Clone + Send> UndoStack<S> {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(UndoStackInner {
                stack: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Push a deep clone of the given state onto the stack.
    pub fn push(&self, state: &S) {
        self.inner.stack.lock().unwrap().push(state.clone());
    }

    /// Pop and return the most recent snapshot, or undefined if empty.
    pub fn pop(&self) -> Option<S> {
        self.inner.stack.lock().unwrap().pop()
    }

    /// Remove all snapshots.
    pub fn clear(&self) {
        self.inner.stack.lock().unwrap().clear();
    }

    /// `get length()`.
    pub fn length(&self) -> i64 {
        self.inner.stack.lock().unwrap().len() as i64
    }
}

impl<S: Clone + Send> Default for UndoStack<S> {
    fn default() -> Self {
        Self::new()
    }
}
