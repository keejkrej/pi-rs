//! Port of packages/ai/src/auth/credential-store.ts

use std::sync::{Arc, Mutex as StdMutex};

use indexmap::IndexMap;
use tokio::sync::Mutex as AsyncMutex;

use pi_js::abort::AbortSignal;
use pi_js::{Error, Result};

use crate::auth::types::{AuthOperationOptions, Credential, CredentialInfo, CredentialStore, ModifyCredentialFn};
use crate::utils::abort::{operation_signal, race_with_abort_signal};

/// Default in-memory credential store. Apps inject persistent stores.
/// Writes (`modify` and `delete`) are serialized per `provider_id`.
#[derive(Debug, Default)]
pub struct InMemoryCredentialStore {
    credentials: StdMutex<IndexMap<String, Credential>>,
    locks: StdMutex<IndexMap<String, Arc<AsyncMutex<()>>>>,
}

impl InMemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ergonomic typed helper for Rust callers updating a provider credential under the store lock.
    pub async fn modify_with<F, Fut>(
        &self,
        provider_id: &str,
        f: F,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<Credential>>
    where
        F: FnOnce(Option<Credential>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Option<Credential>>> + Send + 'static,
    {
        let boxed: ModifyCredentialFn = Box::new(move |current| Box::pin(f(current)));
        self.modify(provider_id, boxed, options).await
    }

    fn provider_lock(&self, provider_id: &str) -> Result<Arc<AsyncMutex<()>>> {
        let mut guard = self
            .locks
            .lock()
            .map_err(|_| Error::msg("InMemoryCredentialStore lock poisoned"))?;
        Ok(guard
            .entry(provider_id.to_owned())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone())
    }

    fn lock_credentials(&self) -> Result<std::sync::MutexGuard<'_, IndexMap<String, Credential>>> {
        self.credentials
            .lock()
            .map_err(|_| Error::msg("InMemoryCredentialStore credentials lock poisoned"))
    }
}

fn check_aborted(options: Option<&AuthOperationOptions>) -> Result<()> {
    if let Some(signal) = options.and_then(|o| o.signal.as_ref()) {
        signal.throw_if_aborted()?;
    }
    Ok(())
}

async fn enqueue_for_provider<T, F, Fut>(
    store: &InMemoryCredentialStore,
    provider_id: &str,
    options: Option<AuthOperationOptions>,
    task: F,
) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let signal: AbortSignal = operation_signal(options.as_ref().and_then(|o| o.signal.as_ref()));
    signal.throw_if_aborted()?;

    let mutex = store.provider_lock(provider_id)?;
    let acquire = race_with_abort_signal(async move { Ok(mutex.lock_owned().await) }, &signal).await?;
    let _provider_guard = acquire;
    signal.throw_if_aborted()?;
    race_with_abort_signal(task(), &signal).await
}

#[async_trait::async_trait]
impl CredentialStore for InMemoryCredentialStore {
    async fn read(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<Option<Credential>> {
        check_aborted(options.as_ref())?;
        let guard = self.lock_credentials()?;
        Ok(guard.get(provider_id).cloned())
    }

    async fn list(&self, options: Option<AuthOperationOptions>) -> Result<Vec<CredentialInfo>> {
        check_aborted(options.as_ref())?;
        let guard = self.lock_credentials()?;
        Ok(guard
            .iter()
            .map(|(provider_id, credential)| CredentialInfo {
                provider_id: provider_id.clone(),
                credential_type: credential.auth_type(),
            })
            .collect())
    }

    async fn modify(
        &self,
        provider_id: &str,
        f: ModifyCredentialFn,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<Credential>> {
        let signal_for_commit = options.as_ref().and_then(|o| o.signal.clone());
        enqueue_for_provider(self, provider_id, options, || async move {
            let current = {
                let guard = self.lock_credentials()?;
                guard.get(provider_id).cloned()
            };
            let next = f(current.clone()).await?;
            if let Some(sig) = signal_for_commit.as_ref() {
                sig.throw_if_aborted()?;
            }
            if let Some(ref updated) = next {
                let mut guard = self.lock_credentials()?;
                guard.insert(provider_id.to_owned(), updated.clone());
            }
            Ok(next.or(current))
        })
        .await
    }

    async fn delete(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<()> {
        enqueue_for_provider(self, provider_id, options, || async move {
            let mut guard = self.lock_credentials()?;
            guard.shift_remove(provider_id);
            Ok(())
        })
        .await
    }
}
