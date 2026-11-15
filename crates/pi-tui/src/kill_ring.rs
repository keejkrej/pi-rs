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
        Self {
            inner: Arc::new(KillRingInner {
                ring: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Add text to the kill ring.
    ///
    /// @param text - The killed text to add
    /// @param opts - Push options
    pub fn push(&self, text: &str, opts: KillRingPushOptions) {
        if text.is_empty() {
            return;
        }
        let mut ring = self.inner.ring.lock().unwrap();
        if opts.accumulate.unwrap_or(false)
            && let Some(last) = ring.pop()
        {
            if opts.prepend {
                ring.push(format!("{text}{last}"));
            } else {
                ring.push(format!("{last}{text}"));
            }
        } else {
            ring.push(text.to_owned());
        }
    }

    /// Get most recent entry without modifying the ring.
    pub fn peek(&self) -> Option<String> {
        self.inner.ring.lock().unwrap().last().cloned()
    }

    /// Move last entry to front (for yank-pop cycling).
    pub fn rotate(&self) {
        let mut ring = self.inner.ring.lock().unwrap();
        if ring.len() > 1
            && let Some(last) = ring.pop()
        {
            ring.insert(0, last);
        }
    }

    /// `get length()`.
    pub fn length(&self) -> i64 {
        self.inner.ring.lock().unwrap().len() as i64
    }
}

impl Default for KillRing {
    fn default() -> Self {
        Self::new()
    }
}
