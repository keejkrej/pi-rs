//! Port of packages/coding-agent/src/core/runtime-credentials.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use pi_ai::{
    ApiKeyCredential, AuthOperationOptions, AuthType, Credential, CredentialInfo, CredentialStore, ModifyCredentialFn,
};
use pi_js::Result;
use tokio::sync::Notify;

/// Async credential store overlay for non-persistent runtime API keys.
#[derive(Clone)]
pub struct RuntimeCredentials {
    store: Arc<dyn CredentialStore>,
    overrides: Arc<Mutex<IndexMap<String, String>>>,
    /// Modifications not yet written or failed, for example an OAuth refresh.
    pending_modifications: Arc<Mutex<IndexSet<u64>>>,
    next_mod_id: Arc<std::sync::atomic::AtomicU64>,
    mod_settled: Arc<Notify>,
}

impl RuntimeCredentials {
    pub fn new(store: Arc<dyn CredentialStore>) -> Self {
        Self {
            store,
            overrides: Arc::new(Mutex::new(IndexMap::new())),
            pending_modifications: Arc::new(Mutex::new(IndexSet::new())),
            next_mod_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            mod_settled: Arc::new(Notify::new()),
        }
    }

    /// Resolves when every modification started so far is written or failed.
    /// An OAuth refresh may already have spent the single-use refresh token, so exiting before its
    /// result is written would leave only the spent token in `auth.json`.
    pub async fn settled(&self) -> Result<()> {
        loop {
            let notified = self.mod_settled.notified();
            let empty = self.pending_modifications.lock().map(|g| g.is_empty()).unwrap_or(true);
            if empty {
                return Ok(());
            }
            notified.await;
        }
    }

    pub fn set_runtime_api_key(&self, provider_id: &str, api_key: &str) {
        if let Ok(mut guard) = self.overrides.lock() {
            guard.insert(provider_id.to_owned(), api_key.to_owned());
        }
    }

    pub fn remove_runtime_api_key(&self, provider_id: &str) {
        if let Ok(mut guard) = self.overrides.lock() {
            guard.shift_remove(provider_id);
        }
    }

    pub fn has_runtime_api_key(&self, provider_id: &str) -> bool {
        self.overrides.lock().ok().is_some_and(|g| g.contains_key(provider_id))
    }

    fn get_override(&self, provider_id: &str) -> Option<String> {
        self.overrides.lock().ok().and_then(|g| g.get(provider_id).cloned())
    }
}

#[async_trait]
impl CredentialStore for RuntimeCredentials {
    async fn read(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<Option<Credential>> {
        if let Some(ref sig) = options.as_ref().and_then(|o| o.signal.as_ref()) {
            sig.throw_if_aborted()?;
        }
        if let Some(key) = self.get_override(provider_id) {
            return Ok(Some(Credential::ApiKey(ApiKeyCredential {
                key: Some(key),
                env: None,
            })));
        }
        self.store.read(provider_id, options).await
    }

    async fn list(&self, options: Option<AuthOperationOptions>) -> Result<Vec<CredentialInfo>> {
        let base = self.store.list(options.clone()).await?;
        if let Some(ref sig) = options.as_ref().and_then(|o| o.signal.as_ref()) {
            sig.throw_if_aborted()?;
        }
        let mut entries: IndexMap<String, CredentialInfo> =
            base.into_iter().map(|e| (e.provider_id.clone(), e)).collect();
        if let Ok(guard) = self.overrides.lock() {
            for provider_id in guard.keys() {
                entries.insert(
                    provider_id.clone(),
                    CredentialInfo {
                        provider_id: provider_id.clone(),
                        credential_type: AuthType::ApiKey,
                    },
                );
            }
        }
        Ok(entries.into_values().collect())
    }

    async fn modify(
        &self,
        provider_id: &str,
        f: ModifyCredentialFn,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<Credential>> {
        struct PendingModGuard {
            pending: Arc<Mutex<IndexSet<u64>>>,
            id: u64,
            notify: Arc<Notify>,
        }
        impl Drop for PendingModGuard {
            fn drop(&mut self) {
                if let Ok(mut guard) = self.pending.lock() {
                    guard.shift_remove(&self.id);
                }
                self.notify.notify_waiters();
            }
        }

        let mod_id = self.next_mod_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut pending) = self.pending_modifications.lock() {
            pending.insert(mod_id);
        }
        let _guard = PendingModGuard {
            pending: Arc::clone(&self.pending_modifications),
            id: mod_id,
            notify: Arc::clone(&self.mod_settled),
        };
        self.store.modify(provider_id, f, options).await
    }

    async fn delete(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<()> {
        if let Some(ref sig) = options.as_ref().and_then(|o| o.signal.as_ref()) {
            sig.throw_if_aborted()?;
        }
        self.store.delete(provider_id, options).await?;
        self.remove_runtime_api_key(provider_id);
        Ok(())
    }
}
