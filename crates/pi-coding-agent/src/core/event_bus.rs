//! Port of packages/coding-agent/src/core/event-bus.ts

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_js::{BoxFuture, Unsubscribe};
use serde_json::Value;

pub type EventHandler = Arc<dyn Fn(Value) -> BoxFuture<()> + Send + Sync>;
pub type SyncEventHandler = Arc<dyn Fn(Value) + Send + Sync>;

pub trait EventBus: Send + Sync {
    fn emit(&self, channel: &str, data: Value);
    fn on(&self, channel: &str, handler: EventHandler) -> Unsubscribe;
    fn on_sync(&self, channel: &str, handler: SyncEventHandler) -> Unsubscribe {
        self.on(
            channel,
            Arc::new(move |val| {
                let h = Arc::clone(&handler);
                Box::pin(async move { h(val) })
            }),
        )
    }
}

pub trait EventBusController: EventBus {
    fn clear(&self);
}

#[derive(Clone)]
pub struct LocalEventBus {
    inner: Arc<EventBusInner>,
}

#[derive(Clone)]
enum Listener {
    Sync(Arc<dyn Fn(Value) + Send + Sync>),
    Async(EventHandler),
}

struct EventBusInner {
    next_id: AtomicU64,
    listeners: Mutex<IndexMap<String, Vec<(u64, Listener)>>>,
    runtime_handle: Mutex<Option<tokio::runtime::Handle>>,
}

impl LocalEventBus {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(EventBusInner {
                next_id: AtomicU64::new(1),
                listeners: Mutex::new(IndexMap::new()),
                runtime_handle: Mutex::new(tokio::runtime::Handle::try_current().ok()),
            }),
        }
    }

    fn resolve_runtime_handle(&self) -> Option<tokio::runtime::Handle> {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            if let Ok(mut guard) = self.inner.runtime_handle.lock()
                && guard.is_none()
            {
                *guard = Some(handle.clone());
            }
            return Some(handle);
        }
        self.inner.runtime_handle.lock().ok().and_then(|guard| guard.clone())
    }

    pub fn emit(&self, channel: &str, data: Value) {
        let listeners: Vec<Listener> = match self.inner.listeners.lock() {
            Ok(guard) => guard
                .get(channel)
                .map(|list| list.iter().map(|(_, h)| h.clone()).collect())
                .unwrap_or_default(),
            Err(_) => return,
        };
        for listener in listeners {
            let payload = data.clone();
            match listener {
                Listener::Sync(handler) => handler(payload),
                Listener::Async(handler) => {
                    if let Some(handle) = self.resolve_runtime_handle() {
                        handle.spawn(async move {
                            handler(payload).await;
                        });
                    } else {
                        // Drive synchronous prefix without creating a runtime or calling `block_on`.
                        let mut fut = handler(payload);
                        let waker = std::task::Waker::noop();
                        let mut cx = std::task::Context::from_waker(waker);
                        let _ = fut.as_mut().poll(&mut cx);
                    }
                }
            }
        }
    }

    pub fn on_sync(&self, channel: &str, handler: impl Fn(Value) + Send + Sync + 'static) -> Unsubscribe {
        self.insert_listener(channel, Listener::Sync(Arc::new(handler)))
    }

    pub fn on(&self, channel: &str, handler: EventHandler) -> Unsubscribe {
        self.insert_listener(channel, Listener::Async(handler))
    }

    fn insert_listener(&self, channel: &str, listener: Listener) -> Unsubscribe {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut guard) = self.inner.listeners.lock() {
            guard.entry(channel.to_owned()).or_default().push((id, listener));
        }
        let inner = self.inner.clone();
        let ch = channel.to_owned();
        Unsubscribe::new(move || {
            if let Ok(mut guard) = inner.listeners.lock()
                && let Some(list) = guard.get_mut(&ch)
            {
                list.retain(|(entry_id, _)| *entry_id != id);
                if list.is_empty() {
                    guard.shift_remove(&ch);
                }
            }
        })
    }

    pub fn clear(&self) {
        if let Ok(mut guard) = self.inner.listeners.lock() {
            guard.clear();
        }
    }
}

impl Default for LocalEventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus for LocalEventBus {
    fn emit(&self, channel: &str, data: Value) {
        LocalEventBus::emit(self, channel, data);
    }

    fn on(&self, channel: &str, handler: EventHandler) -> Unsubscribe {
        LocalEventBus::on(self, channel, handler)
    }

    fn on_sync(&self, channel: &str, handler: SyncEventHandler) -> Unsubscribe {
        self.insert_listener(channel, Listener::Sync(handler))
    }
}

impl EventBusController for LocalEventBus {
    fn clear(&self) {
        LocalEventBus::clear(self);
    }
}

pub fn create_event_bus() -> Arc<LocalEventBus> {
    Arc::new(LocalEventBus::new())
}
