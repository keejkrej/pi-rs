//! Port of packages/coding-agent/src/core/event-bus.ts

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_js::{BoxFuture, Unsubscribe};
use serde_json::Value;

pub type EventHandler = Arc<dyn Fn(Value) -> BoxFuture<()> + Send + Sync>;

pub trait EventBus: Send + Sync {
    fn emit(&self, channel: &str, data: Value);
    fn on(&self, channel: &str, handler: EventHandler) -> Unsubscribe;
}

pub trait EventBusController: EventBus {
    fn clear(&self);
}

#[derive(Clone)]
pub struct LocalEventBus {
    inner: Arc<EventBusInner>,
}

struct EventBusInner {
    next_id: AtomicU64,
    listeners: Mutex<IndexMap<String, Vec<(u64, EventHandler)>>>,
}

impl LocalEventBus {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(EventBusInner {
                next_id: AtomicU64::new(1),
                listeners: Mutex::new(IndexMap::new()),
            }),
        }
    }

    pub fn emit(&self, channel: &str, data: Value) {
        let handlers: Vec<EventHandler> = match self.inner.listeners.lock() {
            Ok(guard) => guard
                .get(channel)
                .map(|list| list.iter().map(|(_, h)| h.clone()).collect())
                .unwrap_or_default(),
            Err(_) => return,
        };
        for handler in handlers {
            let payload = data.clone();
            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::spawn(async move {
                    handler(payload).await;
                });
            } else {
                futures::executor::block_on(handler(payload));
            }
        }
    }

    pub fn on_sync(&self, channel: &str, handler: impl Fn(Value) + Send + Sync + 'static) -> Unsubscribe {
        let handler = Arc::new(handler);
        self.on(
            channel,
            Arc::new(move |val| {
                let h = handler.clone();
                Box::pin(async move {
                    h(val);
                })
            }),
        )
    }

    pub fn on(&self, channel: &str, handler: EventHandler) -> Unsubscribe {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut guard) = self.inner.listeners.lock() {
            guard.entry(channel.to_owned()).or_default().push((id, handler));
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
}

impl EventBusController for LocalEventBus {
    fn clear(&self) {
        LocalEventBus::clear(self);
    }
}

pub fn create_event_bus() -> Arc<LocalEventBus> {
    Arc::new(LocalEventBus::new())
}
