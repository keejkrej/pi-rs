//! Port of packages/ai/src/models-store.ts

use std::sync::Mutex;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use pi_js::abort::AbortSignal;
use pi_js::{Error, Result};

use crate::types::AnyModel;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsStoreEntry {
    /// Persisted models of every type.
    pub models: Vec<AnyModel>,
    /// Unix timestamp (ms) from the remote catalog's Last-Modified header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<i64>,
    /// Unix timestamp (ms) of the last completed remote check.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<i64>,
    /// Opaque validator from the remote catalog's ETag header, stored verbatim
    /// and echoed back as `If-None-Match`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ModelsStoreOperationOptions {
    pub signal: Option<AbortSignal>,
}

/// Persistent model catalogs keyed by provider ID.
#[async_trait::async_trait]
pub trait ModelsStore: Send + Sync {
    async fn read(
        &self,
        provider_id: &str,
        options: Option<ModelsStoreOperationOptions>,
    ) -> Result<Option<ModelsStoreEntry>>;

    async fn write(
        &self,
        provider_id: &str,
        entry: ModelsStoreEntry,
        options: Option<ModelsStoreOperationOptions>,
    ) -> Result<()>;

    async fn delete(&self, provider_id: &str, options: Option<ModelsStoreOperationOptions>) -> Result<()>;
}

/// Default in-memory implementation of [`ModelsStore`].
#[derive(Debug, Default)]
pub struct InMemoryModelsStore {
    entries: Mutex<IndexMap<String, ModelsStoreEntry>>,
}

impl InMemoryModelsStore {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(IndexMap::new()),
        }
    }

    fn lock_entries(&self) -> Result<std::sync::MutexGuard<'_, IndexMap<String, ModelsStoreEntry>>> {
        self.entries
            .lock()
            .map_err(|_| Error::msg("InMemoryModelsStore lock poisoned"))
    }
}

fn check_aborted(options: Option<&ModelsStoreOperationOptions>) -> Result<()> {
    if let Some(signal) = options.and_then(|o| o.signal.as_ref()) {
        signal.throw_if_aborted()?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl ModelsStore for InMemoryModelsStore {
    async fn read(
        &self,
        provider_id: &str,
        options: Option<ModelsStoreOperationOptions>,
    ) -> Result<Option<ModelsStoreEntry>> {
        check_aborted(options.as_ref())?;
        let guard = self.lock_entries()?;
        Ok(guard.get(provider_id).cloned())
    }

    async fn write(
        &self,
        provider_id: &str,
        entry: ModelsStoreEntry,
        options: Option<ModelsStoreOperationOptions>,
    ) -> Result<()> {
        check_aborted(options.as_ref())?;
        let mut guard = self.lock_entries()?;
        guard.insert(provider_id.to_owned(), entry);
        Ok(())
    }

    async fn delete(&self, provider_id: &str, options: Option<ModelsStoreOperationOptions>) -> Result<()> {
        check_aborted(options.as_ref())?;
        let mut guard = self.lock_entries()?;
        guard.shift_remove(provider_id);
        Ok(())
    }
}
