//! Port of packages/coding-agent/test/model-runtime-credential-sync.test.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexSet;
use pi_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthCheck, AuthInteraction, AuthPrompt, AuthResult, AuthType,
    Credential, CredentialInfo, CredentialStore, ModelAuth, ModifyCredentialFn, ProviderAuth, ProviderAuthInteraction,
};
use pi_ai::models::{ModelsRefreshOptions, Provider, RefreshModelsContext};
use pi_ai::types::{ApiStreamOptions, Model, ModelCost, ModelInput, SimpleStreamOptions, TranscriptContext};
use pi_ai::{AssistantMessageEventStream, AuthOperationOptions, Error, Result};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_runtime::{
    CreateModelRuntimeOptions, CredentialSynchronizationError, CredentialSynchronizationOperation, ModelRuntime,
};
use pi_js::BoxFuture;
use pi_js::abort::AbortController;
use tokio::sync::Notify;

fn test_model(provider: &str) -> Model {
    Model {
        r#type: None,
        id: "dynamic".to_owned(),
        name: "Dynamic".to_owned(),
        api: "openai-completions".to_owned(),
        provider: provider.to_owned(),
        base_url: "https://example.test/v1".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

type LoginHook = Arc<dyn Fn() -> BoxFuture<Result<ApiKeyCredential>> + Send + Sync>;
type CheckHook = Arc<dyn Fn(ApiKeyAuthInput) -> BoxFuture<Result<Option<AuthCheck>>> + Send + Sync>;
type RefreshModelsHook = Arc<dyn Fn(RefreshModelsContext) -> BoxFuture<Result<()>> + Send + Sync>;

struct CustomProviderAuth {
    id: String,
    login_hook: Option<LoginHook>,
    check_hook: Option<CheckHook>,
}

#[async_trait::async_trait]
impl ApiKeyAuth for CustomProviderAuth {
    fn name(&self) -> &str {
        "API key"
    }

    fn supports_login(&self) -> bool {
        true
    }

    async fn login(&self, _interaction: ProviderAuthInteraction) -> Result<ApiKeyCredential> {
        if let Some(ref hook) = self.login_hook {
            hook().await
        } else {
            Ok(ApiKeyCredential {
                key: Some(format!("{}-key", self.id)),
                env: None,
            })
        }
    }

    fn supports_check(&self) -> bool {
        true
    }

    async fn check(&self, input: ApiKeyAuthInput) -> Result<Option<AuthCheck>> {
        if let Some(ref hook) = self.check_hook {
            return hook(input).await;
        }
        Ok(input.credential.map(|_| AuthCheck {
            source: Some("stored".to_owned()),
            auth_type: AuthType::ApiKey,
        }))
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        Ok(input.credential.map(|c| AuthResult {
            auth: ModelAuth {
                api_key: c.key,
                headers: None,
                base_url: None,
            },
            env: None,
            source: Some("stored".to_owned()),
        }))
    }
}

struct CustomTestProvider {
    id: String,
    model: Model,
    auth: ProviderAuth,
    refresh_models: Option<RefreshModelsHook>,
}

impl Provider for CustomTestProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.id
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        vec![self.model.clone()]
    }

    fn refresh_models(&self, context: RefreshModelsContext) -> Option<BoxFuture<Result<()>>> {
        self.refresh_models.as_ref().map(|rm| rm(context))
    }

    fn stream(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        panic!("unused");
    }

    fn stream_simple(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        panic!("unused");
    }
}

fn custom_provider(
    id: &str,
    login_hook: Option<LoginHook>,
    check_hook: Option<CheckHook>,
    refresh_models: Option<RefreshModelsHook>,
) -> Arc<dyn Provider + Send + Sync> {
    Arc::new(CustomTestProvider {
        id: id.to_owned(),
        model: test_model(id),
        auth: ProviderAuth {
            api_key: Some(Arc::new(CustomProviderAuth {
                id: id.to_owned(),
                login_hook,
                check_hook,
            })),
            oauth: None,
        },
        refresh_models,
    })
}

struct UnusedInteraction;

#[async_trait::async_trait]
impl AuthInteraction for UnusedInteraction {
    async fn prompt(&self, _prompt: AuthPrompt) -> Result<String> {
        Ok("unused".to_owned())
    }

    fn notify(&self, _event: pi_ai::auth::types::AuthEvent) {}
}

struct SignalledInteraction {
    signal: pi_js::abort::AbortSignal,
}

#[async_trait::async_trait]
impl AuthInteraction for SignalledInteraction {
    fn signal(&self) -> Option<&pi_js::abort::AbortSignal> {
        Some(&self.signal)
    }

    async fn prompt(&self, _prompt: AuthPrompt) -> Result<String> {
        Ok("unused".to_owned())
    }

    fn notify(&self, _event: pi_ai::auth::types::AuthEvent) {}
}

async fn runtime_with_provider(
    registered: Arc<dyn Provider + Send + Sync>,
    credentials: Arc<AuthStorage>,
) -> ModelRuntime {
    let pid = registered.id().to_owned();
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(credentials),
        models_path: Some(None),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();
    runtime.register_native_provider(registered).unwrap();
    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec![pid]),
            ..Default::default()
        }))
        .await
        .unwrap();
    runtime
}

fn find_credential_sync_err(err: &Error) -> Option<&CredentialSynchronizationError> {
    err.downcast_ref::<CredentialSynchronizationError>()
}

#[tokio::test]
async fn publishes_locally_consistent_availability_before_login_and_logout_resolve() {
    let credentials = Arc::new(AuthStorage::in_memory(None));
    let runtime = runtime_with_provider(custom_provider("dynamic", None, None, None), credentials.clone()).await;

    runtime
        .login("dynamic", AuthType::ApiKey, Arc::new(UnusedInteraction), None)
        .await
        .unwrap();
    assert!(runtime.has_configured_auth("dynamic"));
    assert!(runtime.get_available_snapshot().iter().any(|m| m.id == "dynamic"));
    assert_eq!(
        credentials.read("dynamic", None).await.unwrap(),
        Some(Credential::ApiKey(ApiKeyCredential {
            key: Some("dynamic-key".to_owned()),
            env: None,
        }))
    );

    runtime.logout("dynamic", None).await.unwrap();
    assert!(!runtime.has_configured_auth("dynamic"));
    assert!(!runtime.get_available_snapshot().iter().any(|m| m.provider == "dynamic"));
    assert!(credentials.read("dynamic", None).await.unwrap().is_none());
}

#[tokio::test]
async fn orders_same_provider_credential_operations_through_local_synchronization() {
    let login_started = Arc::new(Notify::new());
    let release_login = Arc::new(Notify::new());
    let started_flag = Arc::new(AtomicBool::new(false));

    let ls = login_started.clone();
    let rl = release_login.clone();
    let sf = started_flag.clone();
    let credentials = Arc::new(AuthStorage::in_memory(None));
    let runtime = runtime_with_provider(
        custom_provider(
            "ordered",
            Some(Arc::new(move || {
                let ls = ls.clone();
                let rl = rl.clone();
                let sf = sf.clone();
                Box::pin(async move {
                    sf.store(true, Ordering::SeqCst);
                    ls.notify_waiters();
                    rl.notified().await;
                    Ok(ApiKeyCredential {
                        key: Some("ordered-key".to_owned()),
                        env: None,
                    })
                })
            })),
            None,
            None,
        ),
        credentials.clone(),
    )
    .await;

    let rt1 = runtime.clone();
    let login = tokio::spawn(async move {
        rt1.login("ordered", AuthType::ApiKey, Arc::new(UnusedInteraction), None)
            .await
    });
    while !started_flag.load(Ordering::SeqCst) {
        login_started.notified().await;
    }

    let rt2 = runtime.clone();
    let logout = tokio::spawn(async move { rt2.logout("ordered", None).await });
    tokio::task::yield_now().await;
    assert!(credentials.read("ordered", None).await.unwrap().is_none());

    release_login.notify_waiters();
    login.await.unwrap().unwrap();
    logout.await.unwrap().unwrap();
    assert!(credentials.read("ordered", None).await.unwrap().is_none());
    assert!(!runtime.has_configured_auth("ordered"));
}

#[tokio::test]
async fn allows_different_providers_to_run_credential_flows_concurrently() {
    let first_started = Arc::new(AtomicBool::new(false));
    let second_started = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(Notify::new());
    let notify_start = Arc::new(Notify::new());

    let fs = first_started.clone();
    let g1 = gate.clone();
    let ns1 = notify_start.clone();

    let ss = second_started.clone();
    let g2 = gate.clone();
    let ns2 = notify_start.clone();

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();

    runtime
        .register_native_provider(custom_provider(
            "one",
            Some(Arc::new(move || {
                let fs = fs.clone();
                let g1 = g1.clone();
                let ns1 = ns1.clone();
                Box::pin(async move {
                    fs.store(true, Ordering::SeqCst);
                    ns1.notify_waiters();
                    g1.notified().await;
                    Ok(ApiKeyCredential {
                        key: Some("one".to_owned()),
                        env: None,
                    })
                })
            })),
            None,
            None,
        ))
        .unwrap();
    runtime
        .register_native_provider(custom_provider(
            "two",
            Some(Arc::new(move || {
                let ss = ss.clone();
                let g2 = g2.clone();
                let ns2 = ns2.clone();
                Box::pin(async move {
                    ss.store(true, Ordering::SeqCst);
                    ns2.notify_waiters();
                    g2.notified().await;
                    Ok(ApiKeyCredential {
                        key: Some("two".to_owned()),
                        env: None,
                    })
                })
            })),
            None,
            None,
        ))
        .unwrap();
    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["one".to_owned(), "two".to_owned()]),
            ..Default::default()
        }))
        .await
        .unwrap();

    let rt1 = runtime.clone();
    let rt2 = runtime.clone();
    let h1 = tokio::spawn(async move {
        rt1.login("one", AuthType::ApiKey, Arc::new(UnusedInteraction), None)
            .await
    });
    let h2 = tokio::spawn(async move {
        rt2.login("two", AuthType::ApiKey, Arc::new(UnusedInteraction), None)
            .await
    });

    while !first_started.load(Ordering::SeqCst) || !second_started.load(Ordering::SeqCst) {
        notify_start.notified().await;
    }
    gate.notify_waiters();
    h1.await.unwrap().unwrap();
    h2.await.unwrap().unwrap();
}

#[tokio::test]
async fn does_not_wait_for_unrelated_provider_availability_during_local_synchronization() {
    let stall_unrelated = Arc::new(AtomicBool::new(false));
    let su = stall_unrelated.clone();

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();
    runtime
        .register_native_provider(custom_provider("target", None, None, None))
        .unwrap();
    runtime
        .register_native_provider(custom_provider(
            "unrelated",
            None,
            Some(Arc::new(move |_| {
                let su = su.clone();
                Box::pin(async move {
                    if su.load(Ordering::SeqCst) {
                        futures::future::pending::<()>().await;
                    }
                    Ok(None)
                })
            })),
            None,
        ))
        .unwrap();
    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["target".to_owned(), "unrelated".to_owned()]),
            ..Default::default()
        }))
        .await
        .unwrap();

    stall_unrelated.store(true, Ordering::SeqCst);
    runtime
        .login("target", AuthType::ApiKey, Arc::new(UnusedInteraction), None)
        .await
        .unwrap();
    assert!(runtime.has_configured_auth("target"));
    let ref_res = runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["target".to_owned()]),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(!ref_res.aborted);
}

#[tokio::test]
async fn reports_cancellation_occurring_during_provider_scoped_availability() {
    let block_availability = Arc::new(AtomicBool::new(false));
    let started_flag = Arc::new(AtomicBool::new(false));
    let started_notify = Arc::new(Notify::new());

    let ba = block_availability.clone();
    let sf = started_flag.clone();
    let sn = started_notify.clone();
    let registered = custom_provider(
        "cancelled-availability",
        None,
        Some(Arc::new(move |input| {
            let ba = ba.clone();
            let sf = sf.clone();
            let sn = sn.clone();
            Box::pin(async move {
                if ba.load(Ordering::SeqCst) {
                    sf.store(true, Ordering::SeqCst);
                    sn.notify_waiters();
                    futures::future::pending::<()>().await;
                }
                Ok(input.credential.map(|_| AuthCheck {
                    source: Some("stored".to_owned()),
                    auth_type: AuthType::ApiKey,
                }))
            })
        })),
        None,
    );

    let runtime = runtime_with_provider(registered, Arc::new(AuthStorage::in_memory(None))).await;
    runtime
        .set_runtime_api_key("cancelled-availability", "key", None)
        .await
        .unwrap();
    block_availability.store(true, Ordering::SeqCst);

    let ctrl = AbortController::new();
    let sig = ctrl.signal();
    let rt = runtime.clone();
    let handle = tokio::spawn(async move {
        rt.refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["cancelled-availability".to_owned()]),
            signal: Some(sig),
            ..Default::default()
        }))
        .await
    });

    while !started_flag.load(Ordering::SeqCst) {
        started_notify.notified().await;
    }
    ctrl.abort(None);

    let res = handle.await.unwrap().unwrap();
    assert!(res.aborted);
}

#[tokio::test]
async fn does_not_run_network_refresh_inside_the_credential_operation_chain() {
    let called = Arc::new(AtomicBool::new(false));
    let called_clone = called.clone();
    let runtime = runtime_with_provider(
        custom_provider(
            "local-only",
            None,
            None,
            Some(Arc::new(move |ctx| {
                let called = called_clone.clone();
                Box::pin(async move {
                    if ctx.allow_network {
                        called.store(true, Ordering::SeqCst);
                        futures::future::pending::<()>().await;
                    }
                    Ok(())
                })
            })),
        ),
        Arc::new(AuthStorage::in_memory(None)),
    )
    .await;

    runtime
        .login("local-only", AuthType::ApiKey, Arc::new(UnusedInteraction), None)
        .await
        .unwrap();
    assert!(!called.load(Ordering::SeqCst));
    assert!(runtime.has_configured_auth("local-only"));
}

#[tokio::test]
async fn keeps_provider_scoped_refreshes_from_superseding_unrelated_providers() {
    let started_flag = Arc::new(AtomicBool::new(false));
    let started_notify = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let first_signal = Arc::new(Mutex::new(None::<pi_js::abort::AbortSignal>));

    let sf = started_flag.clone();
    let sn = started_notify.clone();
    let rel = release.clone();
    let fs = first_signal.clone();

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();
    runtime
        .register_native_provider(custom_provider(
            "one",
            None,
            None,
            Some(Arc::new(move |ctx| {
                let sf = sf.clone();
                let sn = sn.clone();
                let rel = rel.clone();
                let fs = fs.clone();
                Box::pin(async move {
                    if !ctx.allow_network {
                        return Ok(());
                    }
                    *fs.lock().unwrap() = Some(ctx.signal.clone());
                    sf.store(true, Ordering::SeqCst);
                    sn.notify_waiters();
                    rel.notified().await;
                    Ok(())
                })
            })),
        ))
        .unwrap();
    runtime
        .register_native_provider(custom_provider("two", None, None, None))
        .unwrap();
    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["one".to_owned(), "two".to_owned()]),
            ..Default::default()
        }))
        .await
        .unwrap();
    runtime.set_runtime_api_key("one", "one-key", None).await.unwrap();
    runtime.set_runtime_api_key("two", "two-key", None).await.unwrap();

    let rt = runtime.clone();
    let first = tokio::spawn(async move {
        rt.refresh(Some(ModelsRefreshOptions {
            allow_network: Some(true),
            providers: Some(vec!["one".to_owned()]),
            ..Default::default()
        }))
        .await
    });
    while !started_flag.load(Ordering::SeqCst) {
        started_notify.notified().await;
    }

    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(true),
            providers: Some(vec!["two".to_owned()]),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(!first_signal.lock().unwrap().as_ref().unwrap().aborted());

    release.notify_waiters();
    first.await.unwrap().unwrap();
}

struct DelayedCommitStore {
    stored: Arc<Mutex<Option<Credential>>>,
    committed: Arc<AtomicBool>,
    committed_notify: Arc<Notify>,
    release_mutation: Arc<Notify>,
}

#[async_trait::async_trait]
impl CredentialStore for DelayedCommitStore {
    async fn read(&self, _provider_id: &str, _options: Option<AuthOperationOptions>) -> Result<Option<Credential>> {
        Ok(self.stored.lock().unwrap().clone())
    }

    async fn list(&self, _options: Option<AuthOperationOptions>) -> Result<Vec<CredentialInfo>> {
        Ok(self
            .stored
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| {
                vec![CredentialInfo {
                    provider_id: "delayed-commit".to_owned(),
                    credential_type: c.auth_type(),
                }]
            })
            .unwrap_or_default())
    }

    async fn modify(
        &self,
        _provider_id: &str,
        update: ModifyCredentialFn,
        _options: Option<AuthOperationOptions>,
    ) -> Result<Option<Credential>> {
        let cur = self.stored.lock().unwrap().clone();
        let next = update(cur.clone()).await?;
        if let Some(ref c) = next {
            *self.stored.lock().unwrap() = Some(c.clone());
        }
        self.committed.store(true, Ordering::SeqCst);
        self.committed_notify.notify_waiters();
        self.release_mutation.notified().await;
        Ok(self.stored.lock().unwrap().clone())
    }

    async fn delete(&self, _provider_id: &str, _options: Option<AuthOperationOptions>) -> Result<()> {
        *self.stored.lock().unwrap() = None;
        Ok(())
    }
}

#[tokio::test]
async fn waits_for_committed_credential_mutation_before_reporting_cancellation() {
    let stored = Arc::new(Mutex::new(None));
    let committed = Arc::new(AtomicBool::new(false));
    let committed_notify = Arc::new(Notify::new());
    let release_mutation = Arc::new(Notify::new());

    let store = Arc::new(DelayedCommitStore {
        stored: stored.clone(),
        committed: committed.clone(),
        committed_notify: committed_notify.clone(),
        release_mutation: release_mutation.clone(),
    });
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(store),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();
    runtime
        .register_native_provider(custom_provider("delayed-commit", None, None, None))
        .unwrap();
    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["delayed-commit".to_owned()]),
            ..Default::default()
        }))
        .await
        .unwrap();

    let ctrl = AbortController::new();
    let settled = Arc::new(AtomicBool::new(false));
    let settled_clone = settled.clone();
    let sig = ctrl.signal();
    let rt = runtime.clone();
    let login = tokio::spawn(async move {
        let res = rt
            .login(
                "delayed-commit",
                AuthType::ApiKey,
                Arc::new(SignalledInteraction { signal: sig }),
                None,
            )
            .await;
        settled_clone.store(true, Ordering::SeqCst);
        res
    });

    while !committed.load(Ordering::SeqCst) {
        committed_notify.notified().await;
    }
    ctrl.abort(None);
    tokio::task::yield_now().await;
    assert!(!settled.load(Ordering::SeqCst));

    release_mutation.notify_waiters();
    let err = login.await.unwrap().unwrap_err();
    let sync_err = find_credential_sync_err(&err).expect("CredentialSynchronizationError");
    assert_eq!(sync_err.provider_id, "delayed-commit");
    assert_eq!(sync_err.operation, CredentialSynchronizationOperation::Login);
    assert_eq!(
        sync_err.credential,
        Some(Credential::ApiKey(ApiKeyCredential {
            key: Some("delayed-commit-key".to_owned()),
            env: None,
        }))
    );
    assert_eq!(
        *stored.lock().unwrap(),
        Some(Credential::ApiKey(ApiKeyCredential {
            key: Some("delayed-commit-key".to_owned()),
            env: None,
        }))
    );
}

#[tokio::test]
async fn reports_typed_error_when_cancellation_interrupts_post_commit_synchronization() {
    let block_cache_refresh = Arc::new(AtomicBool::new(false));
    let cache_started = Arc::new(AtomicBool::new(false));
    let cache_notify = Arc::new(Notify::new());

    let bcr = block_cache_refresh.clone();
    let cs = cache_started.clone();
    let cn = cache_notify.clone();
    let credentials = Arc::new(AuthStorage::in_memory(None));
    let runtime = runtime_with_provider(
        custom_provider(
            "cancelled-sync",
            None,
            None,
            Some(Arc::new(move |ctx| {
                let bcr = bcr.clone();
                let cs = cs.clone();
                let cn = cn.clone();
                Box::pin(async move {
                    if !ctx.allow_network && bcr.load(Ordering::SeqCst) {
                        cs.store(true, Ordering::SeqCst);
                        cn.notify_waiters();
                        futures::future::pending::<()>().await;
                    }
                    Ok(())
                })
            })),
        ),
        credentials.clone(),
    )
    .await;

    block_cache_refresh.store(true, Ordering::SeqCst);
    let ctrl = AbortController::new();
    let sig = ctrl.signal();
    let rt = runtime.clone();
    let login = tokio::spawn(async move {
        rt.login(
            "cancelled-sync",
            AuthType::ApiKey,
            Arc::new(SignalledInteraction { signal: sig }),
            None,
        )
        .await
    });

    while !cache_started.load(Ordering::SeqCst) {
        cache_notify.notified().await;
    }
    ctrl.abort(None);

    let err = login.await.unwrap().unwrap_err();
    let sync_err = find_credential_sync_err(&err).expect("CredentialSynchronizationError");
    assert_eq!(sync_err.provider_id, "cancelled-sync");
    assert_eq!(sync_err.operation, CredentialSynchronizationOperation::Login);
    assert_eq!(
        sync_err.credential,
        Some(Credential::ApiKey(ApiKeyCredential {
            key: Some("cancelled-sync-key".to_owned()),
            env: None,
        }))
    );
    assert_eq!(
        credentials.read("cancelled-sync", None).await.unwrap(),
        Some(Credential::ApiKey(ApiKeyCredential {
            key: Some("cancelled-sync-key".to_owned()),
            env: None,
        }))
    );
    // Keep clippy/unused warnings zero on IndexSet import cleanly if needed
    let _used: IndexSet<String> = IndexSet::new();
}

#[tokio::test]
async fn reports_committed_credentials_when_local_synchronization_fails() {
    let fail_cache_refresh = Arc::new(AtomicBool::new(false));
    let fcr = fail_cache_refresh.clone();
    let credentials = Arc::new(AuthStorage::in_memory(None));
    let runtime = runtime_with_provider(
        custom_provider(
            "broken-sync",
            None,
            None,
            Some(Arc::new(move |ctx| {
                let fcr = fcr.clone();
                Box::pin(async move {
                    if !ctx.allow_network && fcr.load(Ordering::SeqCst) {
                        return Err(Error::msg("cache restore failed"));
                    }
                    Ok(())
                })
            })),
        ),
        credentials.clone(),
    )
    .await;

    fail_cache_refresh.store(true, Ordering::SeqCst);
    let err = runtime
        .login("broken-sync", AuthType::ApiKey, Arc::new(UnusedInteraction), None)
        .await
        .unwrap_err();
    let sync_err = find_credential_sync_err(&err).expect("CredentialSynchronizationError");
    assert_eq!(sync_err.provider_id, "broken-sync");
    assert_eq!(sync_err.operation, CredentialSynchronizationOperation::Login);
    assert_eq!(
        sync_err.credential,
        Some(Credential::ApiKey(ApiKeyCredential {
            key: Some("broken-sync-key".to_owned()),
            env: None,
        }))
    );
    assert_eq!(
        credentials.read("broken-sync", None).await.unwrap(),
        Some(Credential::ApiKey(ApiKeyCredential {
            key: Some("broken-sync-key".to_owned()),
            env: None,
        }))
    );
}
