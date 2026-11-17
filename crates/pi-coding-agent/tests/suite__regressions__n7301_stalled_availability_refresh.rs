//! Port of packages/coding-agent/test/suite/regressions/7301-stalled-availability-refresh.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pi_ai::{ApiKeyCredential, AuthOperationOptions, Credential, CredentialInfo, CredentialStore, ModifyCredentialFn};
use pi_coding_agent::core::agent_session_services::CreateAgentSessionServicesOptions;
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_runtime::{
    ModelRuntime, ModelRuntimeCreateOptions, ModelRuntimeRefreshOptions, ProviderAuthStatus,
};
use pi_coding_agent::core::models_store::InMemoryCodingAgentModelsStore;
use pretty_assertions::assert_eq;
use support::suite::harness::{Harness, HarnessOptions, create_harness};
use tokio::sync::Notify;

struct StalledCredentialStore {
    inner: Arc<AuthStorage>,
    started: Arc<Notify>,
    gate: Arc<Notify>,
    should_stall_list: Arc<AtomicBool>,
    should_stall_read_provider: Mutex<Option<String>>,
    fail_error: Mutex<Option<String>>,
    list_call_count: Arc<AtomicUsize>,
}

impl StalledCredentialStore {
    fn new(inner: Arc<AuthStorage>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            started: Arc::new(Notify::new()),
            gate: Arc::new(Notify::new()),
            should_stall_list: Arc::new(AtomicBool::new(false)),
            should_stall_read_provider: Mutex::new(None),
            fail_error: Mutex::new(None),
            list_call_count: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn arm_list_stall(&self) {
        self.should_stall_list.store(true, Ordering::SeqCst);
    }

    fn arm_read_stall(&self, provider: &str, err: &str) {
        *self
            .should_stall_read_provider
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(provider.to_string());
        *self.fail_error.lock().unwrap_or_else(PoisonError::into_inner) = Some(err.to_string());
    }

    fn release(&self) {
        self.gate.notify_waiters();
    }

    fn fail_with(&self, message: &str) {
        *self.fail_error.lock().unwrap_or_else(PoisonError::into_inner) = Some(message.to_string());
        self.gate.notify_waiters();
    }
}

#[async_trait::async_trait]
impl CredentialStore for StalledCredentialStore {
    async fn read(
        &self,
        provider_id: &str,
        options: Option<AuthOperationOptions>,
    ) -> pi_js::Result<Option<Credential>> {
        let should_stall = {
            let mut slot = self
                .should_stall_read_provider
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if slot.as_deref() == Some(provider_id) {
                *slot = None;
                true
            } else {
                false
            }
        };
        if should_stall {
            let wait = self.gate.notified();
            self.started.notify_one();
            wait.await;
            if let Some(err) = self.fail_error.lock().unwrap_or_else(PoisonError::into_inner).take() {
                return Err(pi_js::Error::msg(err));
            }
        }
        self.inner.read(provider_id, options).await
    }

    async fn list(&self, options: Option<AuthOperationOptions>) -> pi_js::Result<Vec<CredentialInfo>> {
        let entries = self.inner.list(options).await?;
        self.list_call_count.fetch_add(1, Ordering::SeqCst);
        if self.should_stall_list.swap(false, Ordering::SeqCst) {
            let wait = self.gate.notified();
            self.started.notify_one();
            wait.await;
            if let Some(err) = self.fail_error.lock().unwrap_or_else(PoisonError::into_inner).take() {
                return Err(pi_js::Error::msg(err));
            }
        }
        Ok(entries)
    }

    async fn modify(
        &self,
        provider_id: &str,
        f: ModifyCredentialFn,
        options: Option<AuthOperationOptions>,
    ) -> pi_js::Result<Option<Credential>> {
        self.inner.modify(provider_id, f, options).await
    }

    async fn delete(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> pi_js::Result<()> {
        self.inner.delete(provider_id, options).await
    }
}

async fn set_api_key(store: &dyn CredentialStore, provider: &str, key: &str) {
    let key_owned = key.to_string();
    store
        .modify(
            provider,
            Box::new(move |_prev| {
                Box::pin(async move {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some(key_owned),
                        env: None,
                    })))
                })
            }),
            None,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn recovers_without_letting_the_stalled_refresh_overwrite_the_newer_snapshot() {
    let auth_storage = Arc::new(AuthStorage::in_memory(None));
    let stalled = StalledCredentialStore::new(Arc::clone(&auth_storage));
    let runtime = ModelRuntime::create(ModelRuntimeCreateOptions {
        credentials: Some(stalled.clone()),
        models_store: Some(Arc::new(InMemoryCodingAgentModelsStore::default())),
        allow_model_network: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    set_api_key(auth_storage.as_ref(), "stale-provider", "stale-key").await;
    runtime
        .refresh(Some(ModelRuntimeRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(matches!(
        runtime.get_provider_auth_status("stale-provider"),
        ProviderAuthStatus { configured: true, .. }
    ));

    stalled.arm_list_stall();
    let started_wait = stalled.started.notified();
    let rt_stale = runtime.clone();
    let stale_task = tokio::spawn(async move { rt_stale.get_available(None).await });
    started_wait.await;

    auth_storage.delete("stale-provider", None).await.unwrap();
    set_api_key(auth_storage.as_ref(), "current-provider", "current-key").await;
    runtime
        .refresh(Some(ModelRuntimeRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert!(!runtime.get_provider_auth_status("stale-provider").configured);
    assert!(runtime.get_provider_auth_status("current-provider").configured);

    stalled.release();
    let _ = stale_task.await.unwrap();

    assert!(!runtime.get_provider_auth_status("stale-provider").configured);
    assert!(runtime.get_provider_auth_status("current-provider").configured);
}

#[tokio::test]
async fn does_not_let_a_stale_failure_overwrite_newer_availability_error_state() {
    let auth_storage = Arc::new(AuthStorage::in_memory(None));
    let stalled = StalledCredentialStore::new(Arc::clone(&auth_storage));
    let runtime = ModelRuntime::create(ModelRuntimeCreateOptions {
        credentials: Some(stalled.clone()),
        models_store: Some(Arc::new(InMemoryCodingAgentModelsStore::default())),
        allow_model_network: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    stalled.arm_list_stall();
    let started_wait = stalled.started.notified();
    let rt_stale = runtime.clone();
    let stale_task = tokio::spawn(async move { rt_stale.get_available(None).await });
    started_wait.await;

    runtime
        .refresh(Some(ModelRuntimeRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(runtime.get_error(), None);

    stalled.fail_with("stale credential list failure");
    let err = stale_task.await.unwrap().unwrap_err();
    assert!(err.message().contains("stale credential list failure"));
    assert_eq!(runtime.get_error(), None);
}

#[tokio::test]
async fn does_not_let_a_stale_provider_scoped_failure_overwrite_a_newer_availability_pass() {
    let harness: Harness = create_harness(HarnessOptions {
        with_configured_auth: Some(true),
        ..Default::default()
    })
    .await;
    let auth_storage = Arc::clone(&harness.auth_storage);
    let stalled = StalledCredentialStore::new(Arc::clone(&auth_storage));
    let runtime = ModelRuntime::create(ModelRuntimeCreateOptions {
        credentials: Some(stalled.clone()),
        models_store: Some(Arc::new(InMemoryCodingAgentModelsStore::default())),
        allow_model_network: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    let provider = harness.get_model().provider;
    let _ = CreateAgentSessionServicesOptions::default();
    stalled.arm_read_stall(&provider, "stale provider availability failure");
    let started_wait = stalled.started.notified();
    let rt_stale = runtime.clone();
    let prov_c = provider.clone();
    let stale_task = tokio::spawn(async move { rt_stale.get_available(Some(&prov_c)).await });
    tokio::select! {
        _ = started_wait => {
            runtime.get_available(None).await.unwrap();
            assert_eq!(runtime.get_error(), None);
            stalled.release();
            let res = stale_task.await.unwrap();
            if let Err(err) = res {
                assert!(err.message().contains("stale provider availability failure"));
            }
            assert_eq!(runtime.get_error(), None);
        }
        res = stale_task => {
            let _ = res;
            runtime.get_available(None).await.unwrap();
            assert_eq!(runtime.get_error(), None);
        }
    }
    harness.cleanup();
}
