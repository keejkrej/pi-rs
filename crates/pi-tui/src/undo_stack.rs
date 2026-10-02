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
        todo!("port: UndoStack::new")
    }

    /// Push a deep clone of the given state onto the stack.
    pub fn push(&self, state: &S) {
        todo!("port: UndoStack::push")
    }

    /// Pop and return the most recent snapshot, or undefined if empty.
    pub fn pop(&self) -> Option<S> {
        todo!("port: UndoStack::pop")
    }

    /// Remove all snapshots.
    pub fn clear(&self) {
        todo!("port: UndoStack::clear")
    }

    /// `get length()`.
    pub fn length(&self) -> i64 {
        self.inner.stack.lock().unwrap().len() as i64
    }
}
