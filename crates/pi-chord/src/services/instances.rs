//! Port of packages/chord/src/services/instances.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::Value;

use futures::FutureExt;

use crate::context::{BACKGROUND_CONTEXT, with_cancel};
use crate::services::handle::ServiceObject;
use crate::types::Context;

pub struct InstanceDirectoryOptions {
    pub ready: bool,
    pub on_error: Arc<dyn Fn(pi_js::Error) + Send + Sync>,
}

#[derive(Clone)]
pub struct InstanceDirectoryEntry {
    pub key: String,
    pub generation: i64,
    pub service: ServiceObject,
    pub deactivate: Arc<dyn Fn() + Send + Sync>,
}

impl InstanceDirectoryEntry {
    pub fn new(key: &str, generation: i64, service: ServiceObject, deactivate: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            key: key.to_string(),
            generation,
            service,
            deactivate,
        }
    }

    pub fn deactivate(&self) {
        (self.deactivate)()
    }
}

// PORT: TS structural `InstanceDirectoryEntry` extensions. Rust needs a trait for the generic directory.
pub trait InstanceEntry: Clone + Send + Sync + 'static {
    fn key(&self) -> &str;
    fn generation(&self) -> i64;
    fn service(&self) -> ServiceObject;
    fn deactivate(&self);
}

impl InstanceEntry for InstanceDirectoryEntry {
    fn key(&self) -> &str {
        &self.key
    }

    fn generation(&self) -> i64 {
        self.generation
    }

    fn service(&self) -> ServiceObject {
        self.service.clone()
    }

    fn deactivate(&self) {
        (self.deactivate)()
    }
}

// PORT: JS Map identity for an entry object. A live key has one generation.
#[derive(Clone, PartialEq, Eq, Hash)]
struct EntryId {
    key: String,
    generation: i64,
}

struct ObserverTask {
    cancel: Arc<dyn Fn(Option<Value>) + Send + Sync>,
}

struct Observer {
    handler: Arc<dyn Fn(ServiceObject, Context) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>,
    tasks: IndexMap<EntryId, ObserverTask>,
    closed: bool,
}

struct DirectoryState<TEntry> {
    entries: IndexMap<String, TEntry>,
    // PORT: JS Set identity. Observers are not Hash, so each gets a generated id.
    observers: IndexMap<u64, Observer>,
    next_observer_id: u64,
    ready: bool,
    disposed: bool,
}

struct InstanceDirectoryInner<TEntry> {
    report_error: Arc<dyn Fn(pi_js::Error) + Send + Sync>,
    state: Mutex<DirectoryState<TEntry>>,
}

/// Owns keyed instance lifetime and the cancellable tasks observing those instances.
pub struct InstanceDirectory<TEntry> {
    inner: Arc<InstanceDirectoryInner<TEntry>>,
}

impl<TEntry> Clone for InstanceDirectory<TEntry> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<TEntry: InstanceEntry> InstanceDirectory<TEntry> {
    pub fn new(options: InstanceDirectoryOptions) -> Self {
        Self {
            inner: Arc::new(InstanceDirectoryInner {
                report_error: options.on_error,
                state: Mutex::new(DirectoryState {
                    entries: IndexMap::new(),
                    observers: IndexMap::new(),
                    next_observer_id: 0,
                    ready: options.ready,
                    disposed: false,
                }),
            }),
        }
    }

    pub fn observer_count(&self) -> i64 {
        self.inner.state.lock().unwrap().observers.len() as i64
    }

    pub fn values(&self) -> Vec<TEntry> {
        self.inner.state.lock().unwrap().entries.values().cloned().collect()
    }

    pub fn get(&self, key: &str) -> Option<TEntry> {
        self.inner.state.lock().unwrap().entries.get(key).cloned()
    }

    pub fn insert(&self, entry: TEntry) -> pi_js::Result<()> {
        let should_start = {
            let mut state = self.inner.state.lock().unwrap();
            if state.disposed {
                return Err(pi_js::Error::msg("Keyed service directory is disposed"));
            }
            if state.entries.contains_key(entry.key()) {
                return Err(pi_js::Error::msg(format!(
                    "Keyed service already has a live instance with key {}",
                    entry.key()
                )));
            }
            state.entries.insert(entry.key().to_string(), entry.clone());
            state.ready
        };
        if should_start {
            self.start_all(&entry);
        }
        Ok(())
    }

    pub fn replace(&self, entry: TEntry) -> pi_js::Result<()> {
        let (previous_to_deactivate, prev_cancels, should_start) = {
            let mut state = self.inner.state.lock().unwrap();
            if state.disposed {
                return Err(pi_js::Error::msg("Keyed service directory is disposed"));
            }
            let mut prev_deactivate = None;
            let mut cancels = Vec::new();
            if let Some(previous) = state.entries.get(entry.key()).cloned() {
                if previous.generation() == entry.generation() {
                    return Err(pi_js::Error::msg("Keyed service repeated a live generation"));
                }
                state.entries.shift_remove(previous.key());
                let entry_id = EntryId {
                    key: previous.key().to_string(),
                    generation: previous.generation(),
                };
                for observer in state.observers.values_mut() {
                    if let Some(task) = observer.tasks.shift_remove(&entry_id) {
                        cancels.push(task.cancel);
                    }
                }
                prev_deactivate = Some(previous);
            }
            state.entries.insert(entry.key().to_string(), entry.clone());
            (prev_deactivate, cancels, state.ready)
        };
        if let Some(previous) = previous_to_deactivate {
            previous.deactivate();
        }
        for cancel in prev_cancels {
            cancel(None);
        }
        if should_start {
            self.start_all(&entry);
        }
        Ok(())
    }

    pub fn remove(&self, entry: &TEntry) {
        self.remove_internal(entry);
    }

    pub fn ready(&self) -> pi_js::Result<()> {
        let entries: Vec<TEntry> = {
            let mut state = self.inner.state.lock().unwrap();
            if state.disposed {
                return Err(pi_js::Error::msg("Keyed service directory is disposed"));
            }
            if state.ready {
                return Ok(());
            }
            state.ready = true;
            state.entries.values().cloned().collect()
        };
        for entry in entries {
            self.start_all(&entry);
        }
        Ok(())
    }

    pub fn reset(&self) {
        let (entries, cancels) = {
            let mut state = self.inner.state.lock().unwrap();
            if state.disposed {
                return;
            }
            state.ready = false;
            let entries: Vec<TEntry> = state.entries.drain(..).map(|(_, e)| e).collect();
            let mut cancels = Vec::new();
            for observer in state.observers.values_mut() {
                for (_, task) in observer.tasks.drain(..) {
                    cancels.push(task.cancel);
                }
            }
            (entries, cancels)
        };
        for entry in entries {
            entry.deactivate();
        }
        for cancel in cancels {
            cancel(None);
        }
    }

    pub fn observe(
        &self,
        handler: Arc<dyn Fn(ServiceObject, Context) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>,
    ) -> pi_js::Result<pi_js::Unsubscribe> {
        let (observer_id, initial_entries) = {
            let mut state = self.inner.state.lock().unwrap();
            if state.disposed {
                return Err(pi_js::Error::msg("Keyed service directory is disposed"));
            }
            state.next_observer_id += 1;
            let observer_id = state.next_observer_id;
            state.observers.insert(
                observer_id,
                Observer {
                    handler,
                    tasks: IndexMap::new(),
                    closed: false,
                },
            );
            let entries = if state.ready {
                state.entries.values().cloned().collect()
            } else {
                Vec::new()
            };
            (observer_id, entries)
        };
        for entry in initial_entries {
            self.start(observer_id, &entry);
        }
        let directory = self.clone();
        Ok(pi_js::Unsubscribe::new(move || {
            let cancels = {
                let mut state = directory.inner.state.lock().unwrap();
                let Some(mut observer) = state.observers.shift_remove(&observer_id) else {
                    return;
                };
                if observer.closed {
                    return;
                }
                observer.closed = true;
                observer
                    .tasks
                    .drain(..)
                    .map(|(_, task)| task.cancel)
                    .collect::<Vec<_>>()
            };
            for cancel in cancels {
                cancel(None);
            }
        }))
    }

    pub fn dispose(&self) {
        let (cancels, entries) = {
            let mut state = self.inner.state.lock().unwrap();
            if state.disposed {
                return;
            }
            state.disposed = true;
            let mut cancels = Vec::new();
            for (_, mut observer) in state.observers.drain(..) {
                observer.closed = true;
                for (_, task) in observer.tasks.drain(..) {
                    cancels.push(task.cancel);
                }
            }
            let entries: Vec<TEntry> = state.entries.drain(..).map(|(_, e)| e).collect();
            (cancels, entries)
        };
        for cancel in cancels {
            cancel(None);
        }
        for entry in entries {
            entry.deactivate();
        }
    }

    fn remove_internal(&self, entry: &TEntry) {
        let cancels = {
            let mut state = self.inner.state.lock().unwrap();
            let Some(current) = state.entries.get(entry.key()) else {
                return;
            };
            if current.generation() != entry.generation() {
                return;
            }
            let removed = state.entries.shift_remove(entry.key());
            let entry_id = EntryId {
                key: entry.key().to_string(),
                generation: entry.generation(),
            };
            let mut cancels = Vec::new();
            for observer in state.observers.values_mut() {
                if let Some(task) = observer.tasks.shift_remove(&entry_id) {
                    cancels.push(task.cancel);
                }
            }
            drop(state);
            if let Some(removed) = removed {
                removed.deactivate();
            }
            cancels
        };
        for cancel in cancels {
            cancel(None);
        }
    }

    fn start_all(&self, entry: &TEntry) {
        let observer_ids: Vec<u64> = {
            let state = self.inner.state.lock().unwrap();
            state.observers.keys().copied().collect()
        };
        for observer_id in observer_ids {
            self.start(observer_id, entry);
        }
    }

    fn start(&self, observer_id: u64, entry: &TEntry) {
        let entry_id = EntryId {
            key: entry.key().to_string(),
            generation: entry.generation(),
        };
        let task_spec = {
            let mut state = self.inner.state.lock().unwrap();
            // Verify entry is still the live entry for this key before borrowing `state.observers` mutably.
            let is_live = state
                .entries
                .get(entry.key())
                .is_some_and(|current| current.generation() == entry.generation());
            if !is_live {
                return;
            }
            let Some(observer) = state.observers.get_mut(&observer_id) else {
                return;
            };
            if observer.closed || observer.tasks.contains_key(&entry_id) {
                return;
            }
            let wc = with_cancel(BACKGROUND_CONTEXT.clone());
            let cancel = wc.cancel;
            observer.tasks.insert(
                entry_id,
                ObserverTask {
                    cancel: Arc::new(move |value| {
                        cancel(value.map(pi_js::abort::AbortReason::value));
                    }),
                },
            );
            Some((Arc::clone(&observer.handler), entry.service(), wc.context))
        };
        let Some((handler, service, context)) = task_spec else {
            return;
        };
        let report_error = Arc::clone(&self.inner.report_error);
        let mut future = handler(service, context.clone());
        match future.as_mut().now_or_never() {
            Some(Ok(())) => {}
            Some(Err(error)) => {
                if !context.abort_signal().is_some_and(|signal| signal.aborted()) {
                    report_error(error);
                }
            }
            None => {
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        if let Err(error) = future.await {
                            if !context.abort_signal().is_some_and(|signal| signal.aborted()) {
                                report_error(error);
                            }
                        }
                    });
                } else {
                    report_error(pi_js::Error::msg(
                        "Keyed service observer returned a pending future outside a Tokio runtime",
                    ));
                }
            }
        }
    }

    fn assert_active(&self) -> pi_js::Result<()> {
        if self.inner.state.lock().unwrap().disposed {
            return Err(pi_js::Error::msg("Keyed service directory is disposed"));
        }
        Ok(())
    }
}

fn to_error(error: Box<dyn std::error::Error + Send + Sync>) -> pi_js::Error {
    pi_js::Error::msg(error.to_string())
}
