//! pi_js::abort (Rust-only; API contract: PORTING.md Appendix A).
//!
//! `AbortController` / `AbortSignal` with Node 24 semantics:
//!
//! - `abort(reason)` stores the reason (default `AbortError: This operation was aborted`), then
//!   runs the `abort` listeners synchronously, in registration order, before returning. Aborting
//!   an aborted signal does nothing; the first reason wins.
//! - A listener added while the listeners are being dispatched (e.g. by an earlier listener) still
//!   runs; one removed during dispatch does not. A listener added after dispatch never runs.
//! - [`AbortSignal::any`] returns a signal that aborts with the reason of the first input to
//!   abort. Inputs that are themselves `any` signals are flattened to their sources. When a
//!   source aborts, every dependent signal is marked aborted first, then the source's listeners
//!   run, then each dependent's listeners (in dependent creation order).
//! - [`AbortSignal::timeout`] aborts with `TimeoutError: The operation was aborted due to timeout`.
//!
//! Async code awaits [`AbortSignal::cancelled`] or uses [`AbortSignal::token`] (a
//! `CancellationToken` that is cancelled when the signal aborts).

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use tokio_util::sync::CancellationToken;

use crate::error::{Error, Result};

/// The `signal.reason` of an aborted signal.
///
/// `name` / `message` are those of the reason when it is an error. A non-error reason
/// (`controller.abort("x")`) keeps the raw value in `value`, with an empty `name` and
/// `String(value)` as `message`.
#[derive(Clone, Debug, PartialEq)]
pub struct AbortReason {
    pub name: String,
    pub message: String,
    pub value: Option<serde_json::Value>,
}

impl AbortReason {
    /// The default reason: `DOMException` `AbortError: This operation was aborted`.
    pub fn abort() -> Self {
        AbortReason::error("AbortError", "This operation was aborted")
    }

    /// The reason of [`AbortSignal::timeout`]: `TimeoutError: The operation was aborted due to timeout`.
    pub fn timeout() -> Self {
        AbortReason::error("TimeoutError", "The operation was aborted due to timeout")
    }

    /// An error reason, e.g. `controller.abort(new Error("Request aborted"))`.
    pub fn error(name: &str, message: &str) -> Self {
        AbortReason {
            name: name.to_string(),
            message: message.to_string(),
            value: None,
        }
    }

    /// The reason for `controller.abort(err)` where `err` is a [`Error`].
    pub fn from_error(e: &Error) -> Self {
        AbortReason::error(e.name(), &e.message())
    }

    /// A non-error reason (`controller.abort(value)`); `message` is `String(value)`.
    // PORT: JS keeps any value as the reason; Rust keeps it as JSON with an empty `name`, so
    // `Error::to_js_string` yields `String(value)` like JS.
    pub fn value(v: serde_json::Value) -> Self {
        AbortReason {
            name: String::new(),
            message: js_string(&v),
            value: Some(v),
        }
    }
}

/// `String(v)` for a JSON value.
fn js_string(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if !(n.is_i64() || n.is_u64()) => crate::json::number_to_string(f),
            _ => n.to_string(),
        },
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => js_string(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

type Listener = Box<dyn FnOnce(&AbortReason) + Send>;

struct State {
    reason: Option<AbortReason>,
    /// Set once the `abort` listeners have run (or for signals created aborted); later listeners
    /// are never invoked.
    dispatched: bool,
    next_id: u64,
    listeners: VecDeque<(u64, Listener)>,
    /// Signals created by `AbortSignal.any` that depend on this one.
    dependents: Vec<Weak<Inner>>,
    /// For `AbortSignal.any` signals: the (non-composite) source signals.
    sources: Option<Vec<Weak<Inner>>>,
}

struct Inner {
    state: Mutex<State>,
    token: CancellationToken,
}

impl Inner {
    fn new(sources: Option<Vec<Weak<Inner>>>) -> Arc<Inner> {
        Arc::new(Inner {
            state: Mutex::new(State {
                reason: None,
                dispatched: false,
                next_id: 0,
                listeners: VecDeque::new(),
                dependents: Vec::new(),
                sources,
            }),
            token: CancellationToken::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Marks the signal aborted. Returns false if it already was.
    fn set_reason(&self, reason: &AbortReason) -> bool {
        {
            let mut state = self.lock();
            if state.reason.is_some() {
                return false;
            }
            state.reason = Some(reason.clone());
        }
        self.token.cancel();
        true
    }

    /// Runs the `abort` listeners in order, including ones added during the dispatch.
    fn dispatch(&self, reason: &AbortReason) {
        loop {
            let next = {
                let mut state = self.lock();
                match state.listeners.pop_front() {
                    Some((_, f)) => f,
                    None => {
                        state.dispatched = true;
                        return;
                    }
                }
            };
            next(reason);
        }
    }

    /// The "signal abort" algorithm (DOM spec), as Node implements it.
    fn signal_abort(&self, reason: AbortReason) {
        if !self.set_reason(&reason) {
            return;
        }
        let dependents: Vec<Arc<Inner>> = {
            let mut state = self.lock();
            let alive: Vec<Arc<Inner>> = state.dependents.iter().filter_map(Weak::upgrade).collect();
            state.dependents.clear();
            alive
        };
        let to_abort: Vec<Arc<Inner>> = dependents.into_iter().filter(|d| d.set_reason(&reason)).collect();
        self.dispatch(&reason);
        for dependent in to_abort {
            dependent.dispatch(&reason);
        }
    }
}

/// `new AbortController()`.
pub struct AbortController {
    signal: AbortSignal,
}

impl AbortController {
    pub fn new() -> Self {
        AbortController {
            signal: AbortSignal::new_inner(None),
        }
    }

    /// `controller.signal`.
    pub fn signal(&self) -> AbortSignal {
        self.signal.clone()
    }

    /// `controller.abort(reason)`; `None` uses the default `AbortError` reason.
    pub fn abort(&self, reason: Option<AbortReason>) {
        self.signal
            .inner
            .signal_abort(reason.unwrap_or_else(AbortReason::abort));
    }
}

impl Default for AbortController {
    fn default() -> Self {
        AbortController::new()
    }
}

impl fmt::Debug for AbortController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AbortController").field("signal", &self.signal).finish()
    }
}

/// `AbortSignal`: a cheap, cloneable handle; clones observe the same state.
#[derive(Clone)]
pub struct AbortSignal {
    inner: Arc<Inner>,
}

impl AbortSignal {
    fn new_inner(sources: Option<Vec<Weak<Inner>>>) -> AbortSignal {
        AbortSignal {
            inner: Inner::new(sources),
        }
    }

    /// A signal that is already aborted (no listeners will ever run).
    fn new_aborted(reason: AbortReason) -> AbortSignal {
        let signal = AbortSignal::new_inner(None);
        signal.inner.set_reason(&reason);
        signal.inner.lock().dispatched = true;
        signal
    }

    /// `AbortSignal.abort(reason)`: an already-aborted signal.
    pub fn abort(reason: Option<AbortReason>) -> AbortSignal {
        AbortSignal::new_aborted(reason.unwrap_or_else(AbortReason::abort))
    }

    /// A signal that never aborts.
    pub fn never() -> AbortSignal {
        AbortSignal::new_inner(None)
    }

    /// `AbortSignal.timeout(ms)`: aborts with `TimeoutError` after `ms` (on the tokio clock, so
    /// paused-time tests can advance it).
    pub fn timeout(ms: u64) -> AbortSignal {
        let controller = AbortController::new();
        let signal = controller.signal();
        crate::time::set_timeout(ms, move || controller.abort(Some(AbortReason::timeout())));
        signal
    }

    /// `AbortSignal.any(signals)`.
    pub fn any(signals: &[AbortSignal]) -> AbortSignal {
        for s in signals {
            if let Some(reason) = s.reason() {
                return AbortSignal::new_aborted(reason);
            }
        }
        let mut sources: Vec<Arc<Inner>> = Vec::new();
        for s in signals {
            let flattened: Vec<Arc<Inner>> = match &s.inner.lock().sources {
                Some(srcs) => srcs.iter().filter_map(Weak::upgrade).collect(),
                None => vec![s.inner.clone()],
            };
            for src in flattened {
                if !sources.iter().any(|known| Arc::ptr_eq(known, &src)) {
                    sources.push(src);
                }
            }
        }
        let signal = AbortSignal::new_inner(Some(sources.iter().map(Arc::downgrade).collect()));
        for src in &sources {
            let mut state = src.lock();
            state.dependents.retain(|d| d.strong_count() > 0);
            state.dependents.push(Arc::downgrade(&signal.inner));
        }
        // A source may have aborted while the dependents were being registered.
        if let Some(reason) = sources.iter().find_map(|src| src.lock().reason.clone())
            && signal.inner.set_reason(&reason)
        {
            signal.inner.dispatch(&reason);
        }
        signal
    }

    /// `signal.aborted`.
    pub fn aborted(&self) -> bool {
        self.inner.lock().reason.is_some()
    }

    /// `signal.reason` (`None` while not aborted).
    pub fn reason(&self) -> Option<AbortReason> {
        self.inner.lock().reason.clone()
    }

    /// `signal.throwIfAborted()`.
    pub fn throw_if_aborted(&self) -> Result<()> {
        match self.reason() {
            Some(reason) => Err(Error::Abort(reason)),
            None => Ok(()),
        }
    }

    /// Resolves once the signal is aborted (immediately if it already is).
    pub async fn cancelled(&self) {
        self.inner.token.cancelled().await
    }

    /// A `CancellationToken` cancelled when this signal aborts. Cancelling the returned token
    /// does not abort the signal.
    pub fn token(&self) -> CancellationToken {
        self.inner.token.child_token()
    }

    /// `signal.addEventListener("abort", f, { once: true })`. Dropping the returned guard is
    /// `removeEventListener`; use [`ListenerGuard::forget`] to keep the listener for the life of
    /// the signal. Registering on a signal whose `abort` event already fired does nothing.
    pub fn on_abort(&self, f: impl FnOnce(&AbortReason) + Send + 'static) -> ListenerGuard {
        let mut state = self.inner.lock();
        if state.dispatched {
            return ListenerGuard { target: None };
        }
        let id = state.next_id;
        state.next_id += 1;
        state.listeners.push_back((id, Box::new(f)));
        ListenerGuard {
            target: Some((Arc::downgrade(&self.inner), id)),
        }
    }

    /// Whether both handles refer to the same signal.
    pub fn ptr_eq(&self, other: &AbortSignal) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl fmt::Debug for AbortSignal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AbortSignal").field("reason", &self.reason()).finish()
    }
}

/// An `abort` listener registration; dropping it removes the listener.
#[must_use = "dropping the guard removes the listener; call .forget() to keep it"]
pub struct ListenerGuard {
    target: Option<(Weak<Inner>, u64)>,
}

impl ListenerGuard {
    /// Keeps the listener registered for the life of the signal.
    pub fn forget(mut self) {
        self.target = None;
    }

    /// `removeEventListener` (same as dropping the guard).
    pub fn remove(self) {}
}

impl Drop for ListenerGuard {
    fn drop(&mut self) {
        let Some((inner, id)) = self.target.take() else {
            return;
        };
        let Some(inner) = inner.upgrade() else {
            return;
        };
        // Take the listener out first so its captures drop without the lock held.
        let removed = {
            let mut state = inner.lock();
            state
                .listeners
                .iter()
                .position(|(i, _)| *i == id)
                .and_then(|pos| state.listeners.remove(pos))
        };
        drop(removed);
    }
}

impl fmt::Debug for ListenerGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ListenerGuard")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use crate::error::Error;

    #[test]
    fn reason_and_throw_if_aborted() {
        let ctrl = AbortController::new();
        let signal = ctrl.signal();
        assert!(!signal.aborted());
        assert!(signal.reason().is_none());
        signal.throw_if_aborted().unwrap();

        ctrl.abort(None);
        assert!(signal.aborted());
        let reason = signal.reason().unwrap();
        assert_eq!(reason, AbortReason::abort());
        assert_eq!(reason.name, "AbortError");
        assert_eq!(reason.message, "This operation was aborted");
        assert!(reason.value.is_none());
        let err = signal.throw_if_aborted().unwrap_err();
        assert!(matches!(err, Error::Abort(_)));
        assert_eq!(err.to_string(), "This operation was aborted");
        assert_eq!(err.name(), "AbortError");
        assert_eq!(err.message(), "This operation was aborted");
        assert!(err.is_abort());
        assert_eq!(err.to_js_string(), "AbortError: This operation was aborted");

        let custom = AbortController::new();
        custom.abort(Some(AbortReason::error("Error", "Request aborted")));
        let err = custom.signal().throw_if_aborted().unwrap_err();
        assert_eq!(err.name(), "Error");
        assert_eq!(err.message(), "Request aborted");
        assert_eq!(err.to_string(), "Request aborted");
        assert!(!err.is_abort());
        assert_eq!(err.to_js_string(), "Error: Request aborted");
        // The first reason wins.
        custom.abort(Some(AbortReason::abort()));
        assert_eq!(custom.signal().reason().unwrap().message, "Request aborted");
    }

    #[test]
    fn any_uses_the_first_already_aborted_reason() {
        let live = AbortController::new();
        let dead = AbortController::new();
        dead.abort(Some(AbortReason::error("Error", "Login cancelled")));
        let signal = AbortSignal::any(&[live.signal(), dead.signal()]);
        assert!(signal.aborted());
        let err = signal.throw_if_aborted().unwrap_err();
        assert_eq!(err.to_string(), "Login cancelled");
        assert_eq!(err.name(), "Error");
        assert_eq!(err.to_js_string(), "Error: Login cancelled");

        let earlier = AbortController::new();
        earlier.abort(Some(AbortReason::error("Error", "first")));
        let signal = AbortSignal::any(&[earlier.signal(), dead.signal()]);
        assert_eq!(signal.reason().unwrap().message, "first");
    }

    #[test]
    fn any_aborts_with_the_source_reason_and_keeps_it() {
        let a = AbortController::new();
        let b = AbortController::new();
        let signal = AbortSignal::any(&[a.signal(), b.signal(), AbortSignal::never()]);
        signal.throw_if_aborted().unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let source_order = order.clone();
        let source_guard = b
            .signal()
            .on_abort(move |_| source_order.lock().unwrap().push("source"));
        let derived_order = order.clone();
        let derived_guard = signal.on_abort(move |reason| {
            assert_eq!(reason.message, "Request aborted");
            derived_order.lock().unwrap().push("derived");
        });
        b.abort(Some(AbortReason::error("Error", "Request aborted")));
        assert_eq!(order.lock().unwrap().as_slice(), ["source", "derived"]);
        let err = signal.throw_if_aborted().unwrap_err();
        assert_eq!(err.name(), "Error");
        assert_eq!(err.message(), "Request aborted");
        assert_eq!(err.to_string(), "Request aborted");
        assert_eq!(err.to_js_string(), "Error: Request aborted");
        a.abort(Some(AbortReason::abort()));
        assert_eq!(signal.reason().unwrap().message, "Request aborted");
        drop((source_guard, derived_guard));
    }

    #[test]
    fn any_flattens_composite_signals() {
        let ctrl = AbortController::new();
        let mid = AbortSignal::any(&[ctrl.signal()]);
        let outer = AbortSignal::any(&[mid.clone(), AbortSignal::never()]);
        ctrl.abort(Some(AbortReason::error("Error", "src")));
        assert_eq!(mid.reason().unwrap().message, "src");
        assert_eq!(outer.reason().unwrap().message, "src");
        assert_eq!(outer.throw_if_aborted().unwrap_err().to_js_string(), "Error: src");
    }

    #[tokio::test]
    async fn any_cancelled_resolves_when_a_source_aborts() {
        let ctrl = AbortController::new();
        let signal = AbortSignal::any(&[AbortSignal::never(), ctrl.signal()]);
        let waiting = signal.clone();
        let task = tokio::spawn(async move { waiting.cancelled().await });
        ctrl.abort(None);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("cancelled hung")
            .unwrap();
        let err = signal.throw_if_aborted().unwrap_err();
        assert_eq!(err.name(), "AbortError");
        assert_eq!(err.to_string(), "This operation was aborted");
        assert!(err.is_abort());
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_cancellation_shape() {
        let signal = AbortSignal::timeout(40);
        assert!(!signal.aborted());
        signal.throw_if_aborted().unwrap();
        let waiting = signal.clone();
        let task = tokio::spawn(async move { waiting.cancelled().await });
        tokio::time::advance(Duration::from_millis(39)).await;
        assert!(!signal.aborted(), "timeout fired early");
        tokio::time::advance(Duration::from_millis(1)).await;
        task.await.unwrap();
        assert!(signal.aborted());
        let reason = signal.reason().unwrap();
        assert_eq!(reason.name, "TimeoutError");
        assert_eq!(reason.message, "The operation was aborted due to timeout");
        assert!(reason.value.is_none());
        let err = signal.throw_if_aborted().unwrap_err();
        assert_eq!(err.to_string(), "The operation was aborted due to timeout");
        assert_eq!(err.name(), "TimeoutError");
        assert_eq!(err.message(), "The operation was aborted due to timeout");
        assert!(!err.is_abort());
        assert_eq!(
            err.to_js_string(),
            "TimeoutError: The operation was aborted due to timeout"
        );
    }
}
