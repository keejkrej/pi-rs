//! Port of packages/coding-agent/test/auth-logout.test.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::auth::types::{ModifyCredentialFn, OAuthAuth, OAuthCredential, ProviderAuth, ProviderAuthInteraction};
use pi_ai::models::{Provider, RefreshModelsContext};
use pi_ai::models_store::InMemoryModelsStore;
use pi_ai::types::{ApiStreamOptions, Model, SimpleStreamOptions, TranscriptContext};
use pi_ai::{
    ApiKeyCredential, AssistantMessageEventStream, AuthOperationOptions, Credential, CredentialInfo, CredentialStore,
    Error, LoginOptions, ModelAuth, Result,
};
use pi_coding_agent::cli::auth_logout::{AuthLogoutOptions, run_auth_logout};
use pi_coding_agent::cli::auth_terminal::AuthOutput;
use pi_coding_agent::core::auth_storage::{AuthStorage, AuthStorageData};
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_js::BoxFuture;
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::time::now_ms;

struct CaptureOutput {
    lines: Mutex<Vec<String>>,
    errors: Mutex<Vec<String>>,
}

impl CaptureOutput {
    fn new() -> Self {
        Self {
            lines: Mutex::new(Vec::new()),
            errors: Mutex::new(Vec::new()),
        }
    }

    fn take_errors(&self) -> Vec<String> {
        std::mem::take(&mut *self.errors.lock().unwrap())
    }

    fn take_lines(&self) -> Vec<String> {
        std::mem::take(&mut *self.lines.lock().unwrap())
    }
}

impl AuthOutput for CaptureOutput {
    fn write(&self, line: &str) {
        self.lines.lock().unwrap().push(line.to_owned());
    }

    fn error(&self, line: &str) {
        self.errors.lock().unwrap().push(line.to_owned());
    }
}

struct FailingDeleteStore {
    base: AuthStorage,
    fail_delete: AtomicBool,
}

#[async_trait]
impl CredentialStore for FailingDeleteStore {
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
        self.base.modify(provider_id, f, options).await
    }

    async fn delete(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<()> {
        if self.fail_delete.load(Ordering::SeqCst) {
            return Err(Error::msg("storage failed"));
        }
        self.base.delete(provider_id, options).await
    }
}

struct BrokenSyncOAuth;

#[async_trait]
impl OAuthAuth for BrokenSyncOAuth {
    fn name(&self) -> &str {
        "Broken Sync"
    }

    async fn login(&self, _i: ProviderAuthInteraction, _o: Option<LoginOptions>) -> Result<OAuthCredential> {
        Err(Error::msg("unused"))
    }

    async fn refresh(&self, cred: OAuthCredential, _sig: AbortSignal) -> Result<OAuthCredential> {
        Ok(cred)
    }

    async fn to_auth(&self, cred: OAuthCredential) -> Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(cred.access),
            headers: None,
            base_url: None,
        })
    }
}

struct BrokenSyncProvider {
    auth: ProviderAuth,
    fail_sync: Arc<AtomicBool>,
}

impl Provider for BrokenSyncProvider {
    fn id(&self) -> &str {
        "broken-sync"
    }

    fn name(&self) -> &str {
        "Broken Sync"
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        Vec::new()
    }

    fn refresh_models(&self, context: RefreshModelsContext) -> Option<BoxFuture<Result<()>>> {
        let fail = Arc::clone(&self.fail_sync);
        Some(Box::pin(async move {
            if !context.allow_network && fail.load(Ordering::SeqCst) {
                return Err(Error::msg("snapshot failed"));
            }
            Ok(())
        }))
    }

    fn stream(&self, _m: &Model, _c: &TranscriptContext, _o: Option<ApiStreamOptions>) -> AssistantMessageEventStream {
        AssistantMessageEventStream::new()
    }

    fn stream_simple(
        &self,
        _m: &Model,
        _c: &TranscriptContext,
        _o: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        AssistantMessageEventStream::new()
    }
}

fn oauth_cred() -> Credential {
    Credential::OAuth(OAuthCredential::new("refresh", "access", now_ms() + 3_600_000))
}

async fn setup_runtime(data: AuthStorageData) -> (Arc<AuthStorage>, ModelRuntime) {
    let credentials = Arc::new(AuthStorage::in_memory(Some(data)));
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::clone(&credentials) as Arc<dyn CredentialStore + Send + Sync>),
        models_path: Some(None),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();
    (credentials, runtime)
}

#[tokio::test]
async fn requires_exactly_one_provider() {
    let (_creds, runtime) = setup_runtime(IndexMap::new()).await;
    let output = CaptureOutput::new();
    for args in [Vec::<&str>::new(), vec!["--api-key"], vec!["radius", "extra"]] {
        let code = run_auth_logout(
            &args,
            AuthLogoutOptions {
                model_runtime: &runtime,
                terminal: &output,
                signal: AbortController::new().signal(),
            },
        )
        .await;
        assert_eq!(code, 1);
        assert_eq!(output.take_errors(), vec!["Usage: pi auth logout <provider>"]);
    }
}

#[tokio::test]
async fn removes_stored_account_login() {
    let mut data = IndexMap::new();
    data.insert("radius".to_owned(), oauth_cred());
    let (creds, runtime) = setup_runtime(data).await;
    let output = CaptureOutput::new();

    let code = run_auth_logout(
        &["radius"],
        AuthLogoutOptions {
            model_runtime: &runtime,
            terminal: &output,
            signal: AbortController::new().signal(),
        },
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(output.take_lines(), vec!["Logged out of Radius."]);
    assert_eq!(creds.read("radius", None).await.unwrap(), None);
}

#[tokio::test]
async fn removes_stored_api_key_by_provider_name_and_keeps_other_credentials() {
    let mut data = IndexMap::new();
    data.insert(
        "openai".to_owned(),
        Credential::ApiKey(ApiKeyCredential {
            key: Some("sk-test".to_owned()),
            env: None,
        }),
    );
    data.insert("radius".to_owned(), oauth_cred());
    let (creds, runtime) = setup_runtime(data).await;
    let output = CaptureOutput::new();

    let code = run_auth_logout(
        &["OpenAI"],
        AuthLogoutOptions {
            model_runtime: &runtime,
            terminal: &output,
            signal: AbortController::new().signal(),
        },
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(
        output.take_lines(),
        vec!["Removed stored API key for OpenAI. Environment variables and models.json config are unchanged."]
    );
    assert_eq!(creds.read("openai", None).await.unwrap(), None);
    assert!(matches!(
        creds.read("radius", None).await.unwrap(),
        Some(Credential::OAuth(_))
    ));
}

#[tokio::test]
async fn succeeds_when_nothing_is_stored() {
    let (_creds, runtime) = setup_runtime(IndexMap::new()).await;
    let output = CaptureOutput::new();

    let code = run_auth_logout(
        &["radius"],
        AuthLogoutOptions {
            model_runtime: &runtime,
            terminal: &output,
            signal: AbortController::new().signal(),
        },
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(
        output.take_lines(),
        vec!["No stored credentials for Radius. Environment variables and models.json config are unchanged."]
    );
}

#[tokio::test]
async fn removes_credentials_of_unloaded_providers_by_id() {
    let mut data = IndexMap::new();
    data.insert("extension-provider".to_owned(), oauth_cred());
    let (creds, runtime) = setup_runtime(data).await;
    let output = CaptureOutput::new();

    let code = run_auth_logout(
        &["extension-provider"],
        AuthLogoutOptions {
            model_runtime: &runtime,
            terminal: &output,
            signal: AbortController::new().signal(),
        },
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(output.take_lines(), vec!["Logged out of extension-provider."]);
    assert_eq!(creds.read("extension-provider", None).await.unwrap(), None);
}

#[tokio::test]
async fn reports_credential_removal_failures() {
    let mut data = IndexMap::new();
    data.insert("radius".to_owned(), oauth_cred());
    let failing = Arc::new(FailingDeleteStore {
        base: AuthStorage::in_memory(Some(data)),
        fail_delete: AtomicBool::new(true),
    });
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(failing),
        models_path: Some(None),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();
    let output = CaptureOutput::new();

    let code = run_auth_logout(
        &["radius"],
        AuthLogoutOptions {
            model_runtime: &runtime,
            terminal: &output,
            signal: AbortController::new().signal(),
        },
    )
    .await;
    assert_eq!(code, 1);
    let errors = output.take_errors();
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].contains("Logout failed:") && errors[0].contains("storage failed"),
        "unexpected error: {errors:?}"
    );
}

#[tokio::test]
async fn reports_committed_credential_removal_separately_from_sync_failure() {
    let mut data = IndexMap::new();
    data.insert("broken-sync".to_owned(), oauth_cred());
    let credentials = Arc::new(AuthStorage::in_memory(Some(data)));
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::clone(&credentials) as Arc<dyn CredentialStore + Send + Sync>),
        models_path: Some(None),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    let fail_sync = Arc::new(AtomicBool::new(false));
    runtime
        .register_native_provider(Arc::new(BrokenSyncProvider {
            auth: ProviderAuth {
                api_key: None,
                oauth: Some(Arc::new(BrokenSyncOAuth)),
            },
            fail_sync: Arc::clone(&fail_sync),
        }))
        .unwrap();
    fail_sync.store(true, Ordering::SeqCst);

    let output = CaptureOutput::new();
    let code = run_auth_logout(
        &["broken-sync"],
        AuthLogoutOptions {
            model_runtime: &runtime,
            terminal: &output,
            signal: AbortController::new().signal(),
        },
    )
    .await;
    assert_eq!(code, 1);
    let errors = output.take_errors();
    assert!(
        errors.first().is_some_and(
            |e| e.contains("Credentials removed for Broken Sync, but local model state could not be synchronized:")
        ),
        "unexpected errors: {errors:?}"
    );
    assert_eq!(credentials.read("broken-sync", None).await.unwrap(), None);
}

#[tokio::test]
async fn rejects_unknown_provider_and_lists_stored_credentials() {
    let mut data = IndexMap::new();
    data.insert("radius".to_owned(), oauth_cred());
    let (_creds, runtime) = setup_runtime(data).await;
    let output = CaptureOutput::new();

    let code = run_auth_logout(
        &["nope"],
        AuthLogoutOptions {
            model_runtime: &runtime,
            terminal: &output,
            signal: AbortController::new().signal(),
        },
    )
    .await;
    assert_eq!(code, 1);
    assert_eq!(
        output.take_errors(),
        vec!["Unknown provider \"nope\".", "Stored credentials: radius"]
    );
}
