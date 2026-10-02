//! pi_js::callback (Rust-only; API contract: PORTING.md Appendix A).
//!
//! [`Unsubscribe`] is the returned `() => void` of TS `subscribe`/`on` APIs. Dropping it does
//! not unsubscribe (as discarding the JS function does not); call [`Unsubscribe::call`].

use std::fmt;

/// A returned unsubscribe function.
pub struct Unsubscribe {
    f: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl Unsubscribe {
    /// Wraps the unsubscribe action.
    pub fn new(f: impl FnOnce() + Send + Sync + 'static) -> Self {
        Unsubscribe { f: Some(Box::new(f)) }
    }

    /// `() => {}`.
    pub fn noop() -> Self {
        Unsubscribe { f: None }
    }

    /// Invokes the unsubscribe action.
    pub fn call(self) {
        if let Some(f) = self.f {
            f();
        }
    }
}

impl Default for Unsubscribe {
    fn default() -> Self {
        Unsubscribe::noop()
    }
}

impl fmt::Debug for Unsubscribe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.f.is_some() {
            "Unsubscribe"
        } else {
            "Unsubscribe(noop)"
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn call_runs_the_callback_once_and_drop_does_not() {
        let hits = Arc::new(AtomicUsize::new(0));

        let pending = Unsubscribe::new({
            let hits = hits.clone();
            move || {
                hits.fetch_add(1, Ordering::SeqCst);
            }
        });
        drop(pending);
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        let unsub = Unsubscribe::new({
            let hits = hits.clone();
            move || {
                hits.fetch_add(1, Ordering::SeqCst);
            }
        });
        // `call` consumes `self`, so the callback cannot run again.
        unsub.call();
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        Unsubscribe::noop().call();
        Unsubscribe::default().call();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
