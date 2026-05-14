use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMetadata {
    pub id: String,
    pub title: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionData {
    pub metadata: SessionMetadata,
    pub state: Value,
}

pub trait StorageBackend: Send + Sync {
    fn get(&self, store: &str, key: &str) -> Result<Option<Value>>;
    fn set(&self, store: &str, key: &str, value: Value) -> Result<()>;
    fn delete(&self, store: &str, key: &str) -> Result<()>;
    fn list(&self, store: &str) -> Result<Vec<(String, Value)>>;
}

#[derive(Debug, Clone, Default)]
pub struct MemoryStorageBackend {
    stores: Arc<Mutex<BTreeMap<String, BTreeMap<String, Value>>>>,
}

impl MemoryStorageBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

impl StorageBackend for MemoryStorageBackend {
    fn get(&self, store: &str, key: &str) -> Result<Option<Value>> {
        Ok(self
            .stores
            .lock()
            .expect("lock poisoned")
            .get(store)
            .and_then(|values| values.get(key).cloned()))
    }

    fn set(&self, store: &str, key: &str, value: Value) -> Result<()> {
        self.stores
            .lock()
            .expect("lock poisoned")
            .entry(store.to_string())
            .or_default()
            .insert(key.to_string(), value);
        Ok(())
    }

    fn delete(&self, store: &str, key: &str) -> Result<()> {
        if let Some(values) = self.stores.lock().expect("lock poisoned").get_mut(store) {
            values.remove(key);
        }
        Ok(())
    }

    fn list(&self, store: &str) -> Result<Vec<(String, Value)>> {
        Ok(self
            .stores
            .lock()
            .expect("lock poisoned")
            .get(store)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect())
    }
}
