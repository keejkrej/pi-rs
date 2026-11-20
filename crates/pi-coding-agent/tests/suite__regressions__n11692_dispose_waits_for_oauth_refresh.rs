//! Port of packages/coding-agent/test/suite/regressions/11692-dispose-waits-for-oauth-refresh.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::auth::types::{
    Credential, CredentialStore, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuth,
    ProviderAuthInteraction,
};
use pi_ai::models::{ModelsRefreshOptions, Provider, RefreshModelsContext};
use pi_ai::types::{ApiStreamOptions, Model, ModelCost, ModelInput, SimpleStreamOptions, TranscriptContext};
use pi_ai::{AssistantMessageEventStream, Error, Result};
use pi_coding_agent::core::agent_session_runtime::{
    AgentSessionRuntime, AgentSessionServices, CreateAgentSessionRuntimeFactory,
};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_js::BoxFuture;
use pi_js::abort::AbortSignal;
use support::suite::harness::{HarnessOptions, create_harness};
use tokio::sync::Notify;

const PROVIDER_ID: &str = "rotating-oauth";

struct RotatingOAuth {
    refresh_started: Arc<Notify>,
    response_release: Arc<Notify>,
}

#[async_trait]
impl OAuthAuth for RotatingOAuth {
    fn name(&self) -> &str {
        "OAuth"
    }

    async fn login(
        &self,
        _interaction: ProviderAuthInteraction,
        _options: Option<LoginOptions>,
    ) -> Result<OAuthCredential> {
        Err(Error::msg("unused"))
    }

    async fn refresh(&self, _credential: OAuthCredential, _signal: AbortSignal) -> Result<OAuthCredential> {
        let wait = self.response_release.notified();
        self.refresh_started.notify_one();
        wait.await;
        Ok(OAuthCredential {
            access: "new-access".to_string(),
            refresh: "new-refresh".to_string(),
            expires: pi_js::time::now_ms() + 3_600_000,
            extra: Default::default(),
        })
    }

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access),
            headers: None,
            base_url: None,
        })
    }
}

struct RotatingOAuthProvider {
    auth: ProviderAuth,
    model: Model,
}

impl Provider for RotatingOAuthProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &str {
        PROVIDER_ID
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        vec![self.model.clone()]
    }

    fn refresh_models(&self, _context: RefreshModelsContext) -> Option<BoxFuture<Result<()>>> {
        Some(Box::pin(async { Ok(()) }))
    }

    fn stream(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        panic!("unused")
    }

    fn stream_simple(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        panic!("unused")
    }
}

#[tokio::test]
async fn dispose_waits_until_the_rotated_token_from_a_background_catalog_refresh_is_saved() {
    let mut creds = IndexMap::new();
    creds.insert(
        PROVIDER_ID.to_string(),
        Credential::OAuth(OAuthCredential {
            access: "old-access".to_string(),
            refresh: "old-refresh".to_string(),
            expires: pi_js::time::now_ms() - 1000,
            extra: Default::default(),
        }),
    );
    let credentials = Arc::new(AuthStorage::in_memory(Some(creds)));
    let refresh_started = Arc::new(Notify::new());
    let response_release = Arc::new(Notify::new());

    let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::clone(&credentials) as Arc<dyn CredentialStore + Send + Sync>),
        models_path: Some(None),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .expect("model_runtime");

    let model = Model {
        r#type: None,
        id: "model".to_string(),
        name: "Model".to_string(),
        api: "openai-completions".to_string(),
        provider: PROVIDER_ID.to_string(),
        base_url: "https://example.test/v1".to_string(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    };

    model_runtime
        .register_native_provider(Arc::new(RotatingOAuthProvider {
            auth: ProviderAuth {
                api_key: None,
                oauth: Some(Arc::new(RotatingOAuth {
                    refresh_started: Arc::clone(&refresh_started),
                    response_release: Arc::clone(&response_release),
                })),
            },
            model,
        }))
        .expect("register");

    let harness = create_harness(HarnessOptions::default()).await;
    let services = AgentSessionServices {
        cwd: harness.temp_dir.clone(),
        agent_dir: harness.temp_dir.clone(),
        model_runtime: model_runtime.clone(),
        settings_manager: harness.settings_manager.clone(),
        resource_loader: harness.session.resource_loader().clone(),
        diagnostics: Vec::new(),
    };
    let unused_factory: CreateAgentSessionRuntimeFactory =
        Arc::new(|_| Box::pin(async { Err(pi_js::Error::msg("runtime replacement not used")) }));
    let runtime = AgentSessionRuntime::new(harness.session.clone(), services, unused_factory, Vec::new(), None);

    let started_wait = refresh_started.notified();
    let mr_refresh = model_runtime.clone();
    let catalog_refresh = tokio::spawn(async move {
        mr_refresh
            .refresh(Some(ModelsRefreshOptions {
                allow_network: Some(true),
                providers: Some(vec![PROVIDER_ID.to_string()]),
                ..Default::default()
            }))
            .await
    });
    started_wait.await;

    let disposed = Arc::new(AtomicBool::new(false));
    let disposed_c = Arc::clone(&disposed);
    let rt_dispose = runtime.clone();
    let disposing = tokio::spawn(async move {
        rt_dispose.dispose().await;
        disposed_c.store(true, Ordering::SeqCst);
    });

    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!disposed.load(Ordering::SeqCst));

    response_release.notify_one();
    disposing.await.expect("join dispose");
    assert!(disposed.load(Ordering::SeqCst));

    let stored = credentials.read(PROVIDER_ID, None).await.expect("read");
    match stored {
        Some(Credential::OAuth(c)) => {
            assert_eq!(c.access, "new-access");
            assert_eq!(c.refresh, "new-refresh");
        }
        other => panic!("expected refreshed OAuth credential, got {other:?}"),
    }
    let _ = catalog_refresh.await;

    harness.cleanup();
}
