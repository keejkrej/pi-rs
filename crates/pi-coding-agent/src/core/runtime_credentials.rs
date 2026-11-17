//! Port of packages/coding-agent/src/core/runtime-credentials.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::{
    ApiKeyCredential, AuthOperationOptions, AuthType, Credential, CredentialInfo, CredentialStore, ModifyCredentialFn,
};
use pi_js::Result;

/// Async credential store overlay for non-persistent runtime API keys.
#[derive(Clone)]
pub struct RuntimeCredentials {
    store: Arc<dyn CredentialStore>,
    overrides: Arc<Mutex<IndexMap<String, String>>>,
}

impl RuntimeCredentials {
    pub fn new(store: Arc<dyn CredentialStore>) -> Self {
        Self {
            store,
            overrides: Arc::new(Mutex::new(IndexMap::new())),
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
