//! Port of packages/coding-agent/test/suite/regressions/7027-credential-refresh-hang.test.ts
//!
//! // WIRE(@tui-w3): connect `InteractiveMode::completeProviderAuthentication` and post-login
//! // timeout warning tests once `modes/interactive/interactive_mode.rs` lands.

use std::sync::Arc;
use std::time::Duration;

use pi_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthCheck, AuthEvent, AuthInteraction, AuthPrompt, AuthResult,
    AuthType, Credential, CredentialStore, ModelAuth, ProviderAuth, ProviderAuthInteraction,
};
use pi_ai::models::{ModelsRefreshOptions, Provider, RefreshModelsContext};
use pi_ai::types::{ApiStreamOptions, Model, ModelCost, ModelInput, SimpleStreamOptions, TranscriptContext};
use pi_ai::{AssistantMessageEventStream, Result};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_resolver::default_model_per_provider;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_js::BoxFuture;
use pi_js::abort::AbortController;
use tokio::sync::watch;

fn dynamic_model() -> Model {
    Model {
        r#type: None,
        id: "dynamic".to_owned(),
        name: "Dynamic".to_owned(),
        api: "openai-completions".to_owned(),
        provider: "stalled-login".to_owned(),
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
    }
}

struct StalledLoginAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for StalledLoginAuth {
    fn name(&self) -> &str {
        "API key"
    }

    fn supports_login(&self) -> bool {
        true
    }

    async fn login(&self, _interaction: ProviderAuthInteraction) -> Result<ApiKeyCredential> {
        Ok(ApiKeyCredential {
            key: Some("secret".to_owned()),
            env: None,
        })
    }

    fn supports_check(&self) -> bool {
        true
    }

    async fn check(&self, input: ApiKeyAuthInput) -> Result<Option<AuthCheck>> {
        if input.credential.as_ref().and_then(|c| c.key.as_ref()).is_some() {
            Ok(Some(AuthCheck {
                auth_type: AuthType::ApiKey,
                source: Some("stored key".to_owned()),
            }))
        } else {
            Ok(None)
        }
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        let key = input
            .credential
            .and_then(|c| c.key)
            .unwrap_or_else(|| "ambient-key".to_owned());
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(key),
                headers: None,
                base_url: None,
            },
            env: None,
            source: Some("stored key".to_owned()),
        }))
    }
}

struct StalledLoginProvider {
    auth: ProviderAuth,
    model: Model,
    started_tx: watch::Sender<usize>,
    release_rx: watch::Receiver<bool>,
}

impl Provider for StalledLoginProvider {
    fn id(&self) -> &str {
        "stalled-login"
    }

    fn name(&self) -> &str {
        "Stalled Login"
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        vec![self.model.clone()]
    }

    fn refresh_models(&self, context: RefreshModelsContext) -> Option<BoxFuture<Result<()>>> {
        if !context.allow_network {
            return Some(Box::pin(async { Ok(()) }));
        }
        let started_tx = self.started_tx.clone();
        let mut release_rx = self.release_rx.clone();
        let signal = context.signal;
        Some(Box::pin(async move {
            started_tx.send_modify(|n| *n += 1);
            loop {
                if *release_rx.borrow() || signal.aborted() {
                    return Ok(());
                }
                tokio::select! {
                    changed = release_rx.changed() => {
                        if changed.is_err() || *release_rx.borrow() {
                            return Ok(());
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(5)) => {}
                }
            }
        }))
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

struct UnusedInteraction;

#[async_trait::async_trait]
impl AuthInteraction for UnusedInteraction {
    async fn prompt(&self, _prompt: AuthPrompt) -> Result<String> {
        Ok("unused".to_owned())
    }

    fn notify(&self, _event: AuthEvent) {}
}

#[tokio::test]
async fn does_not_hold_login_behind_an_older_stalled_network_catalog_refresh() {
    let (started_tx, mut started_rx) = watch::channel(0usize);
    let (release_tx, release_rx) = watch::channel(false);
    let provider: Arc<dyn Provider + Send + Sync> = Arc::new(StalledLoginProvider {
        auth: ProviderAuth {
            api_key: Some(Arc::new(StalledLoginAuth)),
            oauth: None,
        },
        model: dynamic_model(),
        started_tx,
        release_rx,
    });

    let credentials = Arc::new(AuthStorage::in_memory(None));
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(credentials.clone()),
        models_path: Some(None),
        allow_model_network: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .expect("create runtime");
    runtime
        .register_native_provider(provider.clone())
        .expect("register stalled-login provider");
    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec![provider.id().to_owned()]),
            force: None,
            signal: None,
        }))
        .await
        .expect("initial offline refresh");

    let rt_clone = runtime.clone();
    let provider_id = provider.id().to_owned();
    let stalled_handle = tokio::spawn(async move {
        rt_clone
            .refresh(Some(ModelsRefreshOptions {
                allow_network: Some(true),
                providers: Some(vec![provider_id]),
                force: None,
                signal: None,
            }))
            .await
    });
    while *started_rx.borrow() < 1 {
        started_rx.changed().await.unwrap();
    }

    let interaction: Arc<dyn AuthInteraction> = Arc::new(UnusedInteraction);
    let cred = runtime
        .login(provider.id(), AuthType::ApiKey, interaction, None)
        .await
        .expect("login completes without blocking behind stalled refresh");
    assert_eq!(
        cred,
        Credential::ApiKey(ApiKeyCredential {
            key: Some("secret".to_owned()),
            env: None,
        })
    );
    assert!(runtime.get_available_snapshot().iter().any(|m| m.id == "dynamic"));
    assert_eq!(
        credentials.read(provider.id(), None).await.unwrap(),
        Some(Credential::ApiKey(ApiKeyCredential {
            key: Some("secret".to_owned()),
            env: None,
        }))
    );

    let _ = release_tx.send(true);
    let stalled_result = stalled_handle.await.unwrap().expect("stalled refresh result");
    assert!(!stalled_result.aborted);

    // Bounded background refresh cancels cleanly via AbortController and keeps defaultModelPerProvider contract
    assert_eq!(
        default_model_per_provider()
            .iter()
            .find(|(p, _)| *p == "radius")
            .map(|(_, id)| *id),
        Some("balanced")
    );
    let _ = release_tx.send(false);
    let controller = AbortController::new();
    let signal = controller.signal();
    let rt_abort = runtime.clone();
    let pid = provider.id().to_owned();
    let target_start = *started_rx.borrow() + 1;
    let bounded = tokio::spawn(async move {
        rt_abort
            .refresh(Some(ModelsRefreshOptions {
                allow_network: Some(true),
                providers: Some(vec![pid]),
                force: Some(true),
                signal: Some(signal),
            }))
            .await
    });
    while *started_rx.borrow() < target_start {
        started_rx.changed().await.unwrap();
    }
    controller.abort(None);
    let aborted_res = bounded.await.unwrap().expect("aborted refresh returns Ok");
    assert!(aborted_res.aborted);
}
