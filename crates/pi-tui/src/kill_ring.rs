//! Port of packages/tui/src/kill-ring.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

/// Push options. Field order is `prepend`, then `accumulate`.
///
/// `accumulate` `None` is the omitted flag (treated as false inside [`KillRing::push`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillRingPushOptions {
    /// If accumulating, prepend (backward deletion) or append (forward deletion).
    pub prepend: bool,
    /// Merge with the most recent entry instead of creating a new one.
    pub accumulate: Option<bool>,
}

struct KillRingInner {
    ring: Mutex<Vec<String>>,
}

/// Ring buffer for Emacs-style kill/yank operations.
///
/// Tracks killed (deleted) text entries. Consecutive kills can accumulate
/// into a single entry. Supports yank (paste most recent) and yank-pop
/// (cycle through older entries).
///
/// PORT: TS class with identity. Handle so `Input` / `Editor` can store it behind their own mutex.
#[derive(Clone)]
pub struct KillRing {
    inner: Arc<KillRingInner>,
}

impl KillRing {
    pub fn new() -> Self {
        todo!("port: KillRing::new")
    }

    /// Add text to the kill ring.
    ///
    /// @param text - The killed text to add
    /// @param opts - Push options
    pub fn push(&self, text: &str, opts: KillRingPushOptions) {
        todo!("port: KillRing::push")
    }

    /// Get most recent entry without modifying the ring.
    pub fn peek(&self) -> Option<String> {
        todo!("port: KillRing::peek")
    }

    /// Move last entry to front (for yank-pop cycling).
    pub fn rotate(&self) {
        todo!("port: KillRing::rotate")
    }

    /// `get length()`.
    pub fn length(&self) -> i64 {
        self.inner.ring.lock().unwrap().len() as i64
    }
}
