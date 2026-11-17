//! Port of packages/coding-agent/test/runtime-credentials.test.ts

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::{
    ApiKeyCredential, AuthOperationOptions, AuthType, Credential, CredentialInfo, CredentialStore, ModifyCredentialFn,
    OAuthCredential,
};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::runtime_credentials::RuntimeCredentials;
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::{Error, Result};

fn api_cred(key: &str) -> Credential {
    Credential::ApiKey(ApiKeyCredential {
        key: Some(key.to_owned()),
        env: None,
    })
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_overrides_mask_stored_credentials_without_persisting() {
    let storage = AuthStorage::in_memory(Some(IndexMap::from([("anthropic".to_owned(), api_cred("stored-key"))])));
    let credentials = RuntimeCredentials::new(Arc::new(storage.clone()));

    credentials.set_runtime_api_key("anthropic", "runtime-key");
    assert_eq!(
        credentials.read("anthropic", None).await.expect("read"),
        Some(api_cred("runtime-key"))
    );
    assert_eq!(
        storage.read("anthropic", None).await.expect("storage read"),
        Some(api_cred("stored-key"))
    );

    credentials.remove_runtime_api_key("anthropic");
    assert_eq!(
        credentials.read("anthropic", None).await.expect("read after remove"),
        Some(api_cred("stored-key"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn enumeration_merges_overrides_without_exposing_keys() {
    let storage = AuthStorage::in_memory(Some(IndexMap::from([(
        "anthropic".to_owned(),
        Credential::OAuth(OAuthCredential::new(
            "refresh",
            "access",
            pi_js::time::now_ms() + 60_000,
        )),
    )])));
    let credentials = RuntimeCredentials::new(Arc::new(storage));
    credentials.set_runtime_api_key("anthropic", "runtime-key");
    credentials.set_runtime_api_key("openai", "other-runtime-key");

    pretty_assertions::assert_eq!(
        credentials.list(None).await.expect("list"),
        vec![
            CredentialInfo {
                provider_id: "anthropic".to_owned(),
                credential_type: AuthType::ApiKey,
            },
            CredentialInfo {
                provider_id: "openai".to_owned(),
                credential_type: AuthType::ApiKey,
            },
        ]
    );
}

struct SignalRecordingStore {
    signals: Arc<Mutex<Vec<AbortSignal>>>,
}

#[async_trait]
impl CredentialStore for SignalRecordingStore {
    async fn read(&self, _provider_id: &str, options: Option<AuthOperationOptions>) -> Result<Option<Credential>> {
        if let Some(sig) = options.and_then(|o| o.signal) {
            self.signals.lock().unwrap_or_else(PoisonError::into_inner).push(sig);
        }
        Ok(None)
    }

    async fn list(&self, options: Option<AuthOperationOptions>) -> Result<Vec<CredentialInfo>> {
        if let Some(sig) = options.and_then(|o| o.signal) {
            self.signals.lock().unwrap_or_else(PoisonError::into_inner).push(sig);
        }
        Ok(Vec::new())
    }

    async fn modify(
        &self,
        _provider_id: &str,
        _f: ModifyCredentialFn,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<Credential>> {
        if let Some(sig) = options.and_then(|o| o.signal) {
            self.signals.lock().unwrap_or_else(PoisonError::into_inner).push(sig);
        }
        Ok(None)
    }

    async fn delete(&self, _provider_id: &str, options: Option<AuthOperationOptions>) -> Result<()> {
        if let Some(sig) = options.and_then(|o| o.signal) {
            self.signals.lock().unwrap_or_else(PoisonError::into_inner).push(sig);
        }
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn forwards_operation_signals_to_the_persistent_store() {
    let signals = Arc::new(Mutex::new(Vec::new()));
    let credentials = RuntimeCredentials::new(Arc::new(SignalRecordingStore {
        signals: signals.clone(),
    }));
    let controller = AbortController::new();
    let opts = Some(AuthOperationOptions {
        signal: Some(controller.signal()),
    });

    let _ = credentials.read("anthropic", opts.clone()).await.expect("read");
    let _ = credentials.list(opts.clone()).await.expect("list");
    let _ = credentials
        .modify("anthropic", Box::new(|_| Box::pin(async { Ok(None) })), opts.clone())
        .await
        .expect("modify");
    let _ = credentials.delete("anthropic", opts).await.expect("delete");

    let captured = signals.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(captured.len(), 4);
    assert!(captured.iter().all(|s| !s.aborted()));
    controller.abort(None);
    assert!(captured.iter().all(AbortSignal::aborted));
}

struct FailingDeleteStore {
    inner: AuthStorage,
}

#[async_trait]
impl CredentialStore for FailingDeleteStore {
    async fn read(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<Option<Credential>> {
        self.inner.read(provider_id, options).await
    }

    async fn list(&self, options: Option<AuthOperationOptions>) -> Result<Vec<CredentialInfo>> {
        self.inner.list(options).await
    }

    async fn modify(
        &self,
        provider_id: &str,
        f: ModifyCredentialFn,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<Credential>> {
        self.inner.modify(provider_id, f, options).await
    }

    async fn delete(&self, _provider_id: &str, _options: Option<AuthOperationOptions>) -> Result<()> {
        Err(Error::js("AbortError", "cancelled"))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn keeps_a_runtime_override_when_persistent_deletion_is_cancelled() {
    let storage = AuthStorage::in_memory(Some(IndexMap::from([("anthropic".to_owned(), api_cred("stored-key"))])));
    let credentials = RuntimeCredentials::new(Arc::new(FailingDeleteStore { inner: storage }));
    credentials.set_runtime_api_key("anthropic", "runtime-key");

    let controller = AbortController::new();
    let err = credentials
        .delete(
            "anthropic",
            Some(AuthOperationOptions {
                signal: Some(controller.signal()),
            }),
        )
        .await
        .expect_err("expected delete abort error");
    assert_eq!(err.name(), "AbortError");
    assert_eq!(
        credentials.read("anthropic", None).await.expect("read"),
        Some(api_cred("runtime-key"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn delete_clears_both_the_override_and_persisted_credential() {
    let storage = AuthStorage::in_memory(Some(IndexMap::from([("anthropic".to_owned(), api_cred("stored-key"))])));
    let credentials = RuntimeCredentials::new(Arc::new(storage));
    credentials.set_runtime_api_key("anthropic", "runtime-key");

    credentials.delete("anthropic", None).await.expect("delete");
    assert_eq!(credentials.read("anthropic", None).await.expect("read"), None);
    assert_eq!(
        credentials.list(None).await.expect("list"),
        Vec::<CredentialInfo>::new()
    );
}

// PORT: TS had two `credentials.settled()` tests (#11692). In Rust, `RuntimeCredentials`
// awaits `self.store.modify(...)` directly inside its `async fn modify(...)` future and does
// not expose a separate `.settled()` tracker method.
