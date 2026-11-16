//! Port of packages/durable/src/harness/util.ts

use std::future::Future;
use std::hash::Hash;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use indexmap::IndexMap;
use pi_chord::Context;
use pi_js::abort::{AbortReason, AbortSignal};
use tokio::sync::oneshot;

use crate::session::transaction::clone_pi_error;
use crate::types::{Cursor, Page};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn abort_error(signal: &AbortSignal) -> pi_js::Error {
    match signal.reason() {
        Some(reason) if reason.value.is_none() => pi_js::Error::Abort(reason),
        _ => pi_js::Error::Abort(AbortReason::error("AbortError", "The operation was aborted")),
    }
}

struct WaitersInner<K, T> {
    next_id: u64,
    sets: IndexMap<K, Vec<(u64, oneshot::Sender<pi_js::Result<T>>)>>,
}

struct WaiterCleanup<'a, K: Eq + Hash, T> {
    waiters: &'a Waiters<K, T>,
    key: Option<K>,
    id: u64,
    active: bool,
}

impl<K: Eq + Hash, T> Drop for WaiterCleanup<'_, K, T> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(key) = self.key.take() {
            let mut guard = lock(&self.waiters.inner);
            if let Some(entries) = guard.sets.get_mut(&key) {
                entries.retain(|(entry_id, _)| *entry_id != self.id);
                if entries.is_empty() {
                    guard.sets.shift_remove(&key);
                }
            }
        }
    }
}

struct OwnedWaiterCleanup<K: Eq + Hash, T> {
    waiters: Arc<Waiters<K, T>>,
    key: Option<K>,
    id: u64,
    active: bool,
}

impl<K: Eq + Hash, T> Drop for OwnedWaiterCleanup<K, T> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(key) = self.key.take() {
            let mut guard = lock(&self.waiters.inner);
            if let Some(entries) = guard.sets.get_mut(&key) {
                entries.retain(|(entry_id, _)| *entry_id != self.id);
                if entries.is_empty() {
                    guard.sets.shift_remove(&key);
                }
            }
        }
    }
}

/// Pending waits by key. Each settles once: through `resolve`, `reject_all`, or cancellation of
/// its context.
pub struct Waiters<K, T> {
    inner: Mutex<WaitersInner<K, T>>,
}

impl<K, T> Default for Waiters<K, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, T> Waiters<K, T> {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(WaitersInner {
                next_id: 1,
                sets: IndexMap::new(),
            }),
        }
    }
}

impl<K, T> Waiters<K, T>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    T: Clone + Send + 'static,
{
    pub fn add_owned(self: &Arc<Self>, key: K, context: Context) -> pi_js::BoxFuture<pi_js::Result<T>> {
        let this = Arc::clone(self);
        Box::pin(async move { this.add(key, &context).await })
    }

    /// Synchronously register a waiter on the current line and return an owned future that
    /// awaits its settlement (cancel-safe).
    pub fn register(self: &Arc<Self>, key: K, context: Context) -> pi_js::Result<pi_js::BoxFuture<pi_js::Result<T>>> {
        let signal = context.abort_signal();
        if let Some(ref sig) = signal
            && sig.aborted()
        {
            return Err(abort_error(sig));
        }

        let (tx, rx) = oneshot::channel();
        let id = {
            let mut guard = lock(&self.inner);
            let id = guard.next_id;
            guard.next_id += 1;
            guard.sets.entry(key.clone()).or_default().push((id, tx));
            id
        };

        let this = Arc::clone(self);
        Ok(Box::pin(async move {
            let mut cleanup = OwnedWaiterCleanup {
                waiters: this,
                key: Some(key),
                id,
                active: true,
            };

            let result = match signal {
                None => match rx.await {
                    Ok(res) => res,
                    Err(_) => Err(closed_error()),
                },
                Some(sig) => {
                    tokio::select! {
                        biased;
                        _ = sig.cancelled() => Err(abort_error(&sig)),
                        res = rx => match res {
                            Ok(inner) => inner,
                            Err(_) => Err(closed_error()),
                        },
                    }
                }
            };

            cleanup.active = false;
            result
        }))
    }
}

impl<K, T> Waiters<K, T>
where
    K: Eq + Hash + Clone,
    T: Clone,
{
    pub async fn add(&self, key: K, context: &Context) -> pi_js::Result<T> {
        let signal = context.abort_signal();
        if let Some(ref sig) = signal
            && sig.aborted()
        {
            return Err(abort_error(sig));
        }

        let (tx, rx) = oneshot::channel();
        let id = {
            let mut guard = lock(&self.inner);
            let id = guard.next_id;
            guard.next_id += 1;
            guard.sets.entry(key.clone()).or_default().push((id, tx));
            id
        };

        let mut cleanup = WaiterCleanup {
            waiters: self,
            key: Some(key),
            id,
            active: true,
        };

        let result = match signal {
            None => match rx.await {
                Ok(res) => res,
                Err(_) => Err(closed_error()),
            },
            Some(sig) => {
                tokio::select! {
                    biased;
                    _ = sig.cancelled() => Err(abort_error(&sig)),
                    res = rx => match res {
                        Ok(inner) => inner,
                        Err(_) => Err(closed_error()),
                    },
                }
            }
        };

        cleanup.active = false;
        result
    }

    pub fn keys(&self) -> Vec<K> {
        lock(&self.inner).sets.keys().cloned().collect()
    }

    pub fn resolve(&self, key: &K, value: T) {
        let waiters = {
            let mut guard = lock(&self.inner);
            guard.sets.shift_remove(key)
        };
        if let Some(entries) = waiters {
            for (_, tx) in entries {
                let _ = tx.send(Ok(value.clone()));
            }
        }
    }

    pub fn reject_all(&self, error: &pi_js::Error) {
        let all = {
            let mut guard = lock(&self.inner);
            std::mem::take(&mut guard.sets)
        };
        for (_, entries) in all {
            for (_, tx) in entries {
                let _ = tx.send(Err(clone_pi_error(error)));
            }
        }
    }
}

/// Every item of a paginated scan, in page order.
pub async fn scan_all<T, F, Fut>(mut scan: F) -> pi_js::Result<Vec<T>>
where
    F: FnMut(Option<Cursor>) -> Fut,
    Fut: Future<Output = pi_js::Result<Page<T, Cursor>>>,
{
    let mut items = Vec::new();
    let mut cursor: Option<Cursor> = None;
    loop {
        let page = scan(cursor).await?;
        items.extend(page.items);
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(items)
}

pub fn closed_error() -> pi_js::Error {
    pi_js::Error::msg("Harness is closed")
}
