//! Port of packages/chord/src/services/instances.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::Value;

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
        todo!("port: InstanceDirectory::observer_count")
    }

    pub fn values(&self) -> Vec<TEntry> {
        todo!("port: InstanceDirectory::values")
    }

    pub fn get(&self, key: &str) -> Option<TEntry> {
        todo!("port: InstanceDirectory::get")
    }

    pub fn insert(&self, entry: TEntry) -> pi_js::Result<()> {
        todo!("port: InstanceDirectory::insert")
    }

    pub fn replace(&self, entry: TEntry) -> pi_js::Result<()> {
        todo!("port: InstanceDirectory::replace")
    }

    pub fn remove(&self, entry: &TEntry) {
        todo!("port: InstanceDirectory::remove")
    }

    pub fn ready(&self) -> pi_js::Result<()> {
        todo!("port: InstanceDirectory::ready")
    }

    pub fn reset(&self) {
        todo!("port: InstanceDirectory::reset")
    }

    pub fn observe(
        &self,
        handler: Arc<dyn Fn(ServiceObject, Context) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>,
    ) -> pi_js::Result<pi_js::Unsubscribe> {
        todo!("port: InstanceDirectory::observe")
    }

    pub fn dispose(&self) {
        todo!("port: InstanceDirectory::dispose")
    }

    fn remove_internal(&self, entry: &TEntry) {
        todo!("port: InstanceDirectory::remove_internal")
    }

    fn start_all(&self, entry: &TEntry) {
        todo!("port: InstanceDirectory::start_all")
    }

    fn start(&self, observer_id: u64, entry: &TEntry) {
        todo!("port: InstanceDirectory::start")
    }

    fn assert_active(&self) -> pi_js::Result<()> {
        todo!("port: InstanceDirectory::assert_active")
    }
}

fn to_error(error: Box<dyn std::error::Error + Send + Sync>) -> pi_js::Error {
    todo!("port: to_error")
}
