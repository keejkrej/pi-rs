//! pi_js::seam (Rust-only; API contract: PORTING.md Appendix A).
//!
//! Test seams for `vi.mock` / `vi.spyOn` (PORTING.md §12.3). A module declares a thread-local
//! override slot and consults it before the real implementation:
//!
//! ```ignore
//! thread_local! { static READ_CLIPBOARD: Slot<dyn Fn(&str) -> Result<String> + Send + Sync> = Slot::new(); }
//! pub fn read_clipboard(cmd: &str) -> Result<String> {
//!     if let Some(f) = READ_CLIPBOARD.with(|s| s.get()) { return f(cmd); }
//!     read_clipboard_real(cmd)
//! }
//! pub fn override_read_clipboard(f: impl Fn(&str) -> Result<String> + Send + Sync + 'static) -> Guard {
//!     READ_CLIPBOARD.with(|s| s.set(Arc::new(f)))
//! }
//! ```
//!
//! [`Slot::set`] returns a [`Guard`]; dropping it removes that override, restoring the value that
//! was active before it. Nested overrides stack: the most recent live one wins, and guards may be
//! dropped in any order.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

struct SlotState<T: ?Sized> {
    next_id: u64,
    stack: Vec<(u64, Arc<T>)>,
}

/// An override slot holding an optional `Arc<T>`.
pub struct Slot<T: ?Sized> {
    state: Arc<Mutex<SlotState<T>>>,
}

impl<T: ?Sized> Slot<T> {
    /// An empty slot.
    pub fn new() -> Self {
        Slot {
            state: Arc::new(Mutex::new(SlotState {
                next_id: 0,
                stack: Vec::new(),
            })),
        }
    }

    /// The active override, if any.
    pub fn get(&self) -> Option<Arc<T>> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.stack.last().map(|(_, v)| v.clone())
    }

    /// Whether an override is active.
    pub fn is_set(&self) -> bool {
        !self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stack
            .is_empty()
    }
}

impl<T: ?Sized + Send + Sync + 'static> Slot<T> {
    /// Installs `v` until the returned guard drops.
    pub fn set(&self, v: Arc<T>) -> Guard {
        let id = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let id = state.next_id;
            state.next_id += 1;
            state.stack.push((id, v));
            id
        };
        let state = self.state.clone();
        Guard::new(move || {
            // Take the value out before dropping it so its destructor runs without the lock.
            let removed = {
                let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
                state
                    .stack
                    .iter()
                    .position(|(i, _)| *i == id)
                    .map(|pos| state.stack.remove(pos))
            };
            drop(removed);
        })
    }
}

impl<T: ?Sized> Default for Slot<T> {
    fn default() -> Self {
        Slot::new()
    }
}

impl<T: ?Sized> fmt::Debug for Slot<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Slot").field("set", &self.is_set()).finish()
    }
}

/// Restores the previous state when dropped.
#[must_use = "dropping the guard immediately undoes the override"]
pub struct Guard {
    restore: Option<Box<dyn FnOnce() + Send>>,
}

impl Guard {
    /// A guard that runs `restore` on drop.
    pub fn new(restore: impl FnOnce() + Send + 'static) -> Guard {
        Guard {
            restore: Some(Box::new(restore)),
        }
    }

    /// A guard that does nothing on drop.
    pub fn noop() -> Guard {
        Guard { restore: None }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(f) = self.restore.take() {
            f();
        }
    }
}

impl fmt::Debug for Guard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Guard")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn guard_drop_restores_the_previous_slot() {
        let slot: Slot<dyn Fn() -> &'static str + Send + Sync> = Slot::new();
        assert!(slot.get().is_none());
        assert!(!slot.is_set());

        let outer = slot.set(Arc::new(|| "outer"));
        assert_eq!(slot.get().unwrap()(), "outer");
        let inner = slot.set(Arc::new(|| "inner"));
        assert_eq!(slot.get().unwrap()(), "inner");
        drop(inner);
        assert_eq!(slot.get().unwrap()(), "outer");
        drop(outer);
        assert!(slot.get().is_none());
    }

    #[test]
    fn dropping_an_older_guard_keeps_the_newer_override() {
        let slot: Slot<dyn Fn() -> i32 + Send + Sync> = Slot::new();
        let first = slot.set(Arc::new(|| 1));
        let second = slot.set(Arc::new(|| 2));
        drop(first);
        assert_eq!(slot.get().unwrap()(), 2);
        drop(second);
        assert!(slot.get().is_none());
        drop(Guard::noop());
    }
}
