//! Port of `packages/coding-agent/test/auth-storage.test.ts`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use pi_ai::models::{CreateModelsOptions, create_models};
use pi_ai::{
    ApiKeyCredential, ApiStreamOptions, AssistantMessageEventStream, AuthOperationOptions, AuthResult, AuthType,
    Credential, CredentialInfo, CredentialStore, LoginOptions, Model, ModelAuth, ModifyCredentialFn, OAuthAuth,
    OAuthCredential, Provider, ProviderAuth, ProviderAuthInteraction, SimpleStreamOptions, TranscriptContext,
};
use pi_coding_agent::core::auth_storage::{
    AuthStorage, AuthStorageBackend, AuthStorageData, FileAuthStorageBackend, LockResult,
};
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_coding_agent::vendor::proper_lockfile::{LockOptions, lock};
use pi_js::abort::AbortController;
use pi_js::error::{Error, Result};
use serde_json::{Value, json};
use tempfile::TempDir;

type BoxedLockResult<'a, T> = Pin<Box<dyn Future<Output = Result<LockResult<T>>> + Send + 'a>>;

fn write_auth_json(path: &std::path::Path, value: &Value) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, pi_js::json::stringify(value)).unwrap();
}

fn read_auth_json(path: &std::path::Path) -> Value {
    let raw = std::fs::read_to_string(path).unwrap();
    pi_js::json::parse(&raw).unwrap()
}

fn api_key_cred(key: &str) -> Credential {
    Credential::ApiKey(ApiKeyCredential {
        key: Some(key.to_owned()),
        env: None,
    })
}

fn api_key_mod(key: &str) -> ModifyCredentialFn {
    let owned = key.to_owned();
    Box::new(move |_| Box::pin(async move { Ok(Some(api_key_cred(&owned))) }))
}

struct ToggleFailBackend {
    inner: FileAuthStorageBackend,
    fail_next: Arc<AtomicBool>,
}

#[async_trait]
impl AuthStorageBackend for ToggleFailBackend {
    fn with_lock_dyn(&self, f: &mut dyn FnMut(Option<&str>) -> Result<LockResult<Value>>) -> Result<Value> {
        if self.fail_next.swap(false, Ordering::AcqRel) {
            return Err(Error::msg("lock unavailable"));
        }
        self.inner.with_lock(f)
    }

    async fn with_lock_async_dyn(
        &self,
        f: Box<dyn FnOnce(Option<String>) -> BoxedLockResult<'static, Value> + Send>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Value> {
        if self.fail_next.swap(false, Ordering::AcqRel) {
            return Err(Error::msg("lock unavailable"));
        }
        self.inner.with_lock_async(f, options).await
    }
}

struct FlakyCredentialStore {
    base: AuthStorage,
    fail_next_modify: Arc<AtomicBool>,
}

#[async_trait]
impl CredentialStore for FlakyCredentialStore {
    async fn read(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<Option<Credential>> {
        self.base.read(provider_id, options).await
    }

    async fn list(&self, options: Option<AuthOperationOptions>) -> Result<Vec<CredentialInfo>> {
        self.base.list(options).await
    }

    async fn modify(
        &self,
        provider_id: &str,
        f: ModifyCredentialFn,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<Credential>> {
        if self.fail_next_modify.swap(false, Ordering::AcqRel) {
            return Err(Error::msg("credential store unavailable"));
        }
        self.base.modify(provider_id, f, options).await
    }

    async fn delete(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<()> {
        self.base.delete(provider_id, options).await
    }
}

struct MockOAuthHandler;

#[async_trait]
impl OAuthAuth for MockOAuthHandler {
    fn name(&self) -> &str {
        "OAuth"
    }

    async fn login(
        &self,
        _interaction: ProviderAuthInteraction,
        _options: Option<LoginOptions>,
    ) -> Result<OAuthCredential> {
        Err(Error::msg("not used"))
    }

    async fn refresh(
        &self,
        mut credential: OAuthCredential,
        _signal: pi_js::abort::AbortSignal,
    ) -> Result<OAuthCredential> {
        credential.access = "refreshed-access".to_owned();
        credential.expires = pi_js::time::now_ms() + 60_000;
        Ok(credential)
    }

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access),
            headers: None,
            base_url: None,
        })
    }
}

struct MockOAuthProvider {
    id: String,
    auth: ProviderAuth,
}

impl Provider for MockOAuthProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "OAuth Provider"
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        Vec::new()
    }

    fn stream(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        AssistantMessageEventStream::new()
    }

    fn stream_simple(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        AssistantMessageEventStream::new()
    }
}

#[tokio::test]
async fn reads_and_resolves_stored_api_key_credentials() {
    let tmp = TempDir::new().unwrap();
    let auth_path = tmp.path().join("auth.json");
    let _env = pi_js::env::testing::EnvGuard::set("TEST_AUTH_STORAGE_KEY", Some("environment-key"));
    write_auth_json(
        &auth_path,
        &json!({ "anthropic": { "type": "api_key", "key": "$TEST_AUTH_STORAGE_KEY" } }),
    );
    let storage = AuthStorage::create(Some(auth_path.to_str().unwrap()));
    assert_eq!(
        storage.read("anthropic", None).await.unwrap(),
        Some(api_key_cred("environment-key"))
    );
}

#[tokio::test]
async fn resolves_command_backed_api_key_credentials() {
    let tmp = TempDir::new().unwrap();
    let auth_path = tmp.path().join("auth.json");
    write_auth_json(
        &auth_path,
        &json!({ "anthropic": { "type": "api_key", "key": "!printf 'command-key'" } }),
    );
    let storage = AuthStorage::create(Some(auth_path.to_str().unwrap()));
    assert_eq!(
        storage.read("anthropic", None).await.unwrap(),
        Some(api_key_cred("command-key"))
    );
}

#[tokio::test]
async fn returns_oauth_credentials_unchanged() {
    let cred = Credential::OAuth(OAuthCredential::new(
        "refresh-token",
        "access-token",
        pi_js::time::now_ms() + 60_000,
    ));
    let mut initial = AuthStorageData::new();
    initial.insert("anthropic".to_owned(), cred.clone());
    let storage = AuthStorage::in_memory(Some(initial));
    assert_eq!(storage.read("anthropic", None).await.unwrap(), Some(cred));
}

#[tokio::test]
async fn credential_scoped_env_takes_precedence_and_remains_inspectable() {
    let tmp = TempDir::new().unwrap();
    let auth_path = tmp.path().join("auth.json");
    write_auth_json(
        &auth_path,
        &json!({
            "anthropic": {
                "type": "api_key",
                "key": "$SCOPED_KEY",
                "env": { "SCOPED_KEY": "scoped-value", "REGION": "test-region" }
            }
        }),
    );
    let storage = AuthStorage::create(Some(auth_path.to_str().unwrap()));
    let got = storage.read("anthropic", None).await.unwrap().unwrap();
    let api = got.as_api_key().unwrap();
    assert_eq!(api.key.as_deref(), Some("scoped-value"));
    assert_eq!(
        api.env.as_ref().and_then(|e| e.get("REGION")).map(String::as_str),
        Some("test-region")
    );
}

#[tokio::test]
async fn coalesces_file_reloads_and_keeps_alive_when_one_reader_aborts() {
    let tmp = TempDir::new().unwrap();
    let auth_path = tmp.path().join("auth.json");
    let path_str = auth_path.to_str().unwrap().to_owned();
    write_auth_json(&auth_path, &json!({ "anthropic": { "type": "api_key", "key": "old" } }));

    let first = AuthStorage::create(Some(&path_str));
    let second = AuthStorage::create(Some(&path_str));

    write_auth_json(
        &auth_path,
        &json!({
            "anthropic": { "type": "api_key", "key": "new" },
            "openai": { "type": "api_key", "key": "openai-key" }
        }),
    );

    let c1 = AbortController::new();
    let c2 = AbortController::new();
    let c3 = AbortController::new();
    let (anthropic, openai, credentials) = tokio::join!(
        first.read(
            "anthropic",
            Some(AuthOperationOptions {
                signal: Some(c1.signal())
            })
        ),
        second.read(
            "openai",
            Some(AuthOperationOptions {
                signal: Some(c2.signal())
            })
        ),
        first.list(Some(AuthOperationOptions {
            signal: Some(c3.signal())
        })),
    );
    assert_eq!(anthropic.unwrap(), Some(api_key_cred("new")));
    assert_eq!(openai.unwrap(), Some(api_key_cred("openai-key")));
    assert_eq!(
        credentials.unwrap(),
        vec![
            CredentialInfo {
                provider_id: "anthropic".to_owned(),
                credential_type: AuthType::ApiKey
            },
            CredentialInfo {
                provider_id: "openai".to_owned(),
                credential_type: AuthType::ApiKey
            },
        ]
    );

    // Held lock + aborted reader should allow second reader to succeed once lock releases
    write_auth_json(
        &auth_path,
        &json!({ "anthropic": { "type": "api_key", "key": "newest-after-abort" } }),
    );
    let release = lock(&path_str, LockOptions::default()).await.unwrap();
    let ctrl_abort = Arc::new(AbortController::new());
    let ctrl_ok = AbortController::new();
    let f1 = first.read(
        "anthropic",
        Some(AuthOperationOptions {
            signal: Some(ctrl_abort.signal()),
        }),
    );
    let f2 = second.read(
        "anthropic",
        Some(AuthOperationOptions {
            signal: Some(ctrl_ok.signal()),
        }),
    );
    let ctrl_trigger = Arc::clone(&ctrl_abort);
    let rel_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(15)).await;
        ctrl_trigger.abort(None);
        tokio::time::sleep(Duration::from_millis(15)).await;
        release.release().await.unwrap();
    });
    let (r1, r2) = tokio::join!(f1, f2);
    rel_handle.await.unwrap();
    assert!(r1.unwrap_err().is_abort());
    assert_eq!(r2.unwrap(), Some(api_key_cred("newest-after-abort")));
}

#[cfg(unix)]
#[tokio::test]
async fn creates_owner_only_permissions_and_preserves_existing_mode() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let new_path = tmp.path().join("new-auth.json");
    let _storage = AuthStorage::create(Some(new_path.to_str().unwrap()));
    assert_eq!(
        std::fs::metadata(&new_path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let existing_path = tmp.path().join("existing-auth.json");
    write_auth_json(
        &existing_path,
        &json!({ "anthropic": { "type": "api_key", "key": "old" } }),
    );
    std::fs::set_permissions(&existing_path, std::fs::Permissions::from_mode(0o660)).unwrap();
    let storage = AuthStorage::create(Some(existing_path.to_str().unwrap()));
    storage.modify("anthropic", api_key_mod("new"), None).await.unwrap();
    assert_eq!(
        std::fs::metadata(&existing_path).unwrap().permissions().mode() & 0o777,
        0o660
    );
}

#[tokio::test]
async fn modify_preserves_external_edits_handles_none_serializes_and_deletes() {
    let tmp = TempDir::new().unwrap();
    let auth_path = tmp.path().join("auth.json");
    let path_str = auth_path.to_str().unwrap().to_owned();
    write_auth_json(&auth_path, &json!({ "anthropic": { "type": "api_key", "key": "old" } }));

    let storage = AuthStorage::create(Some(&path_str));
    write_auth_json(
        &auth_path,
        &json!({
            "anthropic": { "type": "api_key", "key": "old" },
            "openai": { "type": "api_key", "key": "external" }
        }),
    );
    storage.modify("anthropic", api_key_mod("new"), None).await.unwrap();
    assert_eq!(
        read_auth_json(&auth_path),
        json!({
            "anthropic": { "type": "api_key", "key": "new" },
            "openai": { "type": "api_key", "key": "external" }
        })
    );

    // modify returning None leaves stored credential unchanged
    let unchanged = storage
        .modify("anthropic", Box::new(|_| Box::pin(async { Ok(None) })), None)
        .await
        .unwrap();
    assert_eq!(unchanged, Some(api_key_cred("new")));
    assert_eq!(
        storage.read("anthropic", None).await.unwrap(),
        Some(api_key_cred("new"))
    );

    // serializes concurrent modifications
    write_auth_json(&auth_path, &json!({}));
    let s1 = AuthStorage::create(Some(&path_str));
    let s2 = AuthStorage::create(Some(&path_str));
    let (m1, m2) = tokio::join!(
        s1.modify("anthropic", api_key_mod("anthropic-key"), None),
        s2.modify("openai", api_key_mod("openai-key"), None),
    );
    m1.unwrap();
    m2.unwrap();
    assert_eq!(
        read_auth_json(&auth_path),
        json!({
            "anthropic": { "type": "api_key", "key": "anthropic-key" },
            "openai": { "type": "api_key", "key": "openai-key" }
        })
    );

    // delete removes one while preserving external edits
    write_auth_json(
        &auth_path,
        &json!({
            "anthropic": { "type": "api_key", "key": "anthropic-key" },
            "openai": { "type": "api_key", "key": "openai-key" },
            "google": { "type": "api_key", "key": "external-key" }
        }),
    );
    storage.delete("anthropic", None).await.unwrap();
    assert_eq!(
        storage.list(None).await.unwrap(),
        vec![
            CredentialInfo {
                provider_id: "openai".to_owned(),
                credential_type: AuthType::ApiKey
            },
            CredentialInfo {
                provider_id: "google".to_owned(),
                credential_type: AuthType::ApiKey
            },
        ]
    );
    assert_eq!(storage.read("anthropic", None).await.unwrap(), None);
}

#[tokio::test]
async fn in_memory_storage_and_lock_failure_recovery() {
    let mut data = AuthStorageData::new();
    data.insert("anthropic".to_owned(), api_key_cred("initial"));
    let mem = AuthStorage::in_memory(Some(data));
    assert_eq!(
        mem.read("anthropic", None).await.unwrap(),
        Some(api_key_cred("initial"))
    );
    mem.modify("anthropic", api_key_mod("updated"), None).await.unwrap();
    assert_eq!(
        mem.read("anthropic", None).await.unwrap(),
        Some(api_key_cred("updated"))
    );
    mem.delete("anthropic", None).await.unwrap();
    assert!(mem.list(None).await.unwrap().is_empty());

    let tmp = TempDir::new().unwrap();
    let auth_path = tmp.path().join("auth.json");
    write_auth_json(
        &auth_path,
        &json!({ "anthropic": { "type": "api_key", "key": "stored" } }),
    );
    let fail_next = Arc::new(AtomicBool::new(false));
    let backend = Arc::new(ToggleFailBackend {
        inner: FileAuthStorageBackend::new(Some(auth_path.to_str().unwrap())),
        fail_next: Arc::clone(&fail_next),
    });
    let storage = AuthStorage::from_storage(backend);
    fail_next.store(true, Ordering::SeqCst);
    let err = storage.modify("openai", api_key_mod("new"), None).await.unwrap_err();
    assert!(err.to_string().contains("lock unavailable"));
    assert_eq!(
        read_auth_json(&auth_path),
        json!({ "anthropic": { "type": "api_key", "key": "stored" } })
    );
    storage.modify("openai", api_key_mod("new"), None).await.unwrap();
    assert_eq!(
        read_auth_json(&auth_path),
        json!({
            "anthropic": { "type": "api_key", "key": "stored" },
            "openai": { "type": "api_key", "key": "new" }
        })
    );
}

#[tokio::test]
async fn pre_aborted_and_wait_aborted_and_active_cancelled_file_locks() {
    let tmp = TempDir::new().unwrap();
    let nonexistent = tmp.path().join("never-created.json");
    let backend_new = FileAuthStorageBackend::new(Some(nonexistent.to_str().unwrap()));
    let ctrl = AbortController::new();
    ctrl.abort(None);
    let called = Arc::new(AtomicBool::new(false));
    let called_clone = Arc::clone(&called);
    let res = backend_new
        .with_lock_async(
            move |_| {
                called_clone.store(true, Ordering::SeqCst);
                async { Ok(LockResult::unchanged(())) }
            },
            Some(AuthOperationOptions {
                signal: Some(ctrl.signal()),
            }),
        )
        .await;
    assert!(res.unwrap_err().is_abort());
    assert!(!called.load(Ordering::SeqCst));
    assert!(!nonexistent.exists());

    // Abort while waiting on held lock
    let auth_path = tmp.path().join("auth.json");
    let path_str = auth_path.to_str().unwrap().to_owned();
    write_auth_json(
        &auth_path,
        &json!({ "anthropic": { "type": "api_key", "key": "stored" } }),
    );
    let release = lock(&path_str, LockOptions::default()).await.unwrap();
    let backend = Arc::new(FileAuthStorageBackend::new(Some(&path_str)));
    let wait_ctrl = Arc::new(AbortController::new());
    let wait_called = Arc::new(AtomicUsize::new(0));
    let wait_called_c = Arc::clone(&wait_called);
    let backend_c = Arc::clone(&backend);
    let wait_sig = wait_ctrl.signal();
    let pending = tokio::spawn(async move {
        backend_c
            .with_lock_async(
                move |_| {
                    wait_called_c.fetch_add(1, Ordering::SeqCst);
                    async { Ok(LockResult::write((), "{}".to_owned())) }.map_res()
                },
                Some(AuthOperationOptions { signal: Some(wait_sig) }),
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(15)).await;
    wait_ctrl.abort(None);
    assert!(pending.await.unwrap().unwrap_err().is_abort());
    release.release().await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(wait_called.load(Ordering::SeqCst), 0);

    // Hold lock until cancelled active callback settles without committing
    let started = Arc::new(tokio::sync::Notify::new());
    let started_tx = Arc::clone(&started);
    let unblock = Arc::new(tokio::sync::Notify::new());
    let unblock_rx = Arc::clone(&unblock);
    let active_ctrl = Arc::new(AbortController::new());
    let active_sig = active_ctrl.signal();
    let backend_a = Arc::clone(&backend);
    let active_handle = tokio::spawn(async move {
        backend_a
            .with_lock_async(
                move |_| async move {
                    started_tx.notify_one();
                    unblock_rx.notified().await;
                    Ok(LockResult::write(
                        (),
                        pi_js::json::stringify(&json!({ "openai": { "type": "api_key", "key": "cancelled" } })),
                    ))
                },
                Some(AuthOperationOptions {
                    signal: Some(active_sig),
                }),
            )
            .await
    });
    started.notified().await;
    active_ctrl.abort(None);

    let competing_called = Arc::new(AtomicUsize::new(0));
    let competing_c = Arc::clone(&competing_called);
    let backend_b = Arc::clone(&backend);
    let competing = tokio::spawn(async move {
        backend_b
            .with_lock_async(
                move |_| {
                    competing_c.fetch_add(1, Ordering::SeqCst);
                    async {
                        Ok(LockResult::write(
                            (),
                            pi_js::json::stringify(&json!({ "google": { "type": "api_key", "key": "committed" } })),
                        ))
                    }
                },
                None,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(competing_called.load(Ordering::SeqCst), 0);
    unblock.notify_one();
    assert!(active_handle.await.unwrap().unwrap_err().is_abort());
    competing.await.unwrap().unwrap();
    assert_eq!(competing_called.load(Ordering::SeqCst), 1);
    assert_eq!(
        read_auth_json(&auth_path),
        json!({ "google": { "type": "api_key", "key": "committed" } })
    );
}

trait MapResHelper {
    fn map_res(self) -> Pin<Box<dyn Future<Output = Result<LockResult<()>>> + Send>>;
}

impl<F> MapResHelper for F
where
    F: Future<Output = Result<LockResult<()>>> + Send + 'static,
{
    fn map_res(self) -> Pin<Box<dyn Future<Output = Result<LockResult<()>>> + Send>> {
        Box::pin(self)
    }
}

#[tokio::test]
async fn in_memory_serialization_cancellation_and_refresh_error_recovery() {
    let mem = Arc::new(AuthStorage::in_memory(None));
    let started = Arc::new(tokio::sync::Notify::new());
    let started_tx = Arc::clone(&started);
    let unblock = Arc::new(tokio::sync::Notify::new());
    let unblock_rx = Arc::clone(&unblock);
    let mem_a = Arc::clone(&mem);
    let first = tokio::spawn(async move {
        mem_a
            .modify(
                "anthropic",
                Box::new(move |_| {
                    Box::pin(async move {
                        started_tx.notify_one();
                        unblock_rx.notified().await;
                        Ok(Some(api_key_cred("anthropic-key")))
                    })
                }),
                None,
            )
            .await
    });
    started.notified().await;

    let cancel_ctrl = AbortController::new();
    let queued_called = Arc::new(AtomicUsize::new(0));
    let queued_c = Arc::clone(&queued_called);
    let queued = mem.modify(
        "openai",
        Box::new(move |_| {
            queued_c.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(Some(api_key_cred("openai-key"))) })
        }),
        Some(AuthOperationOptions {
            signal: Some(cancel_ctrl.signal()),
        }),
    );
    cancel_ctrl.abort(None);
    assert!(queued.await.unwrap_err().is_abort());
    unblock.notify_one();
    first.await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(queued_called.load(Ordering::SeqCst), 0);
    assert_eq!(mem.read("openai", None).await.unwrap(), None);

    // Models refresh failure translation + retry
    let mut init = AuthStorageData::new();
    init.insert(
        "oauth-provider".to_owned(),
        Credential::OAuth(OAuthCredential::new("refresh-token", "expired-access", 0)),
    );
    let base = AuthStorage::in_memory(Some(init));
    let store = Arc::new(FlakyCredentialStore {
        base,
        fail_next_modify: Arc::new(AtomicBool::new(true)),
    });
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(store),
        models_store: None,
        auth_context: None,
        identity: None,
    }));
    models.set_provider(Arc::new(MockOAuthProvider {
        id: "oauth-provider".to_owned(),
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(Arc::new(MockOAuthHandler)),
        },
    }));

    assert!(models.get_auth_provider_id("oauth-provider", None).await.is_err());
    let resolved: AuthResult = models
        .get_auth_provider_id("oauth-provider", None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.auth.api_key.as_deref(), Some("refreshed-access"));
}

#[tokio::test]
async fn does_not_overwrite_malformed_auth_files() {
    let tmp = TempDir::new().unwrap();
    let auth_path = tmp.path().join("auth.json");
    write_auth_json(
        &auth_path,
        &json!({ "anthropic": { "type": "api_key", "key": "stored" } }),
    );
    let storage = AuthStorage::create(Some(auth_path.to_str().unwrap()));
    std::fs::write(&auth_path, "{invalid-json").unwrap();
    assert!(storage.modify("openai", api_key_mod("new"), None).await.is_err());
    assert_eq!(std::fs::read_to_string(&auth_path).unwrap(), "{invalid-json");
}

#[cfg(unix)]
#[tokio::test]
async fn proper_lockfile_handles_symlinked_lock_targets() {
    let tmp = TempDir::new().unwrap();
    let real_file = tmp.path().join("real-auth.json");
    let link_file = tmp.path().join("link-auth.json");
    write_auth_json(
        &real_file,
        &json!({ "anthropic": { "type": "api_key", "key": "symlinked" } }),
    );
    std::os::unix::fs::symlink(&real_file, &link_file).unwrap();

    let storage = AuthStorage::create(Some(link_file.to_str().unwrap()));
    storage
        .modify("openai", api_key_mod("added-via-link"), None)
        .await
        .unwrap();
    assert_eq!(
        read_auth_json(&real_file),
        json!({
            "anthropic": { "type": "api_key", "key": "symlinked" },
            "openai": { "type": "api_key", "key": "added-via-link" }
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn model_runtime_retries_oauth_refresh_after_transient_modify_failure() {
    let mut init = AuthStorageData::new();
    init.insert(
        "oauth-provider".to_owned(),
        Credential::OAuth(OAuthCredential::new("refresh-token", "expired-access", 0)),
    );
    let base = AuthStorage::in_memory(Some(init));
    let store = Arc::new(FlakyCredentialStore {
        base,
        fail_next_modify: Arc::new(AtomicBool::new(true)),
    });
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(store),
        models_path: Some(None),
        allow_model_network: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();
    runtime
        .register_native_provider(Arc::new(MockOAuthProvider {
            id: "oauth-provider".to_owned(),
            auth: ProviderAuth {
                api_key: None,
                oauth: Some(Arc::new(MockOAuthHandler)),
            },
        }))
        .unwrap();

    assert!(runtime.get_auth_provider_id("oauth-provider", None).await.is_err());
    let resolved = runtime
        .get_auth_provider_id("oauth-provider", None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.auth.api_key.as_deref(), Some("refreshed-access"));
}

// #11655: aliases of the same auth.json must share one lock.
#[cfg(unix)]
#[tokio::test]
async fn symlinked_auth_json_path_waits_for_lock_held_on_real_file_11655() {
    use pi_js::abort::AbortController;
    use tokio::time::Duration;

    let tmp = TempDir::new().unwrap();
    let real_file = tmp.path().join("auth.json");
    let alias_file = tmp.path().join("alias.json");
    write_auth_json(
        &real_file,
        &json!({ "anthropic": { "type": "api_key", "key": "stored" } }),
    );
    std::os::unix::fs::symlink(&real_file, &alias_file).unwrap();

    let real_str = real_file.to_string_lossy().into_owned();
    let alias_str = alias_file.to_string_lossy().into_owned();
    let release = lock(
        &real_str,
        LockOptions {
            realpath: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let backend = Arc::new(FileAuthStorageBackend::new(Some(&alias_str)));
    let update_called = Arc::new(AtomicUsize::new(0));
    let sync_called = Arc::new(AtomicUsize::new(0));

    let ctrl = Arc::new(AbortController::new());
    let sig = ctrl.signal();
    let b_async = Arc::clone(&backend);
    let uc = Arc::clone(&update_called);
    let pending = tokio::spawn(async move {
        b_async
            .with_lock_async(
                move |_| {
                    uc.fetch_add(1, Ordering::SeqCst);
                    async { Ok(LockResult::write((), "{}".to_owned())) }.map_res()
                },
                Some(AuthOperationOptions { signal: Some(sig) }),
            )
            .await
    });

    tokio::time::sleep(Duration::from_millis(120)).await;
    ctrl.abort(None);
    assert!(pending.await.unwrap().unwrap_err().is_abort());

    let sc = Arc::clone(&sync_called);
    let sync_err = backend.with_lock(move |_| {
        sc.fetch_add(1, Ordering::SeqCst);
        Ok(LockResult::write((), "{}".to_owned()))
    });
    assert!(sync_err.is_err());

    release.release().await.unwrap();
    assert_eq!(update_called.load(Ordering::SeqCst), 0);
    assert_eq!(sync_called.load(Ordering::SeqCst), 0);
    assert_eq!(
        read_auth_json(&real_file),
        json!({ "anthropic": { "type": "api_key", "key": "stored" } })
    );
}
