//! Port of packages/coding-agent/test/suite/regressions/7153-scoped-models-refresh.test.ts
//!
//! // WIRE(@tui-w3): connect `ScopedModelsSelectorComponent` and `InteractiveMode::showModelsSelector`
//! // once `modes/interactive/components/scoped_models_selector.rs` and `interactive_mode.rs` land.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pi_ai::auth::credential_store::InMemoryCredentialStore;
use pi_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthCheck, AuthResult, AuthType, ModelAuth, ProviderAuth,
    ProviderAuthInteraction,
};
use pi_ai::models::{ModelsRefreshOptions, Provider, RefreshModelsContext};
use pi_ai::types::{ApiStreamOptions, Model, ModelCost, ModelInput, SimpleStreamOptions, TranscriptContext};
use pi_ai::{AssistantMessageEventStream, Result};
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_js::BoxFuture;
use pi_js::abort::AbortController;
use tokio::sync::watch;

fn make_model(id: &str, name: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: name.to_owned(),
        api: "openai-completions".to_owned(),
        provider: "scoped-refresh".to_owned(),
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
        context_window: 8192,
        max_tokens: 1024,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

struct StubApiKeyAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for StubApiKeyAuth {
    fn name(&self) -> &str {
        "API key"
    }

    async fn check(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthCheck>> {
        Ok(Some(AuthCheck {
            auth_type: AuthType::ApiKey,
            source: Some("env".to_owned()),
        }))
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some("test-key".to_owned()),
                headers: None,
                base_url: None,
            },
            env: None,
            source: Some("env".to_owned()),
        }))
    }

    async fn login(&self, _interaction: ProviderAuthInteraction) -> Result<ApiKeyCredential> {
        Ok(ApiKeyCredential {
            key: Some("test-key".to_owned()),
            env: None,
        })
    }
}

struct ScopedRefreshProvider {
    auth: ProviderAuth,
    models: Mutex<Vec<Model>>,
    started_tx: watch::Sender<usize>,
    release_rx: watch::Receiver<bool>,
}

impl Provider for ScopedRefreshProvider {
    fn id(&self) -> &str {
        "scoped-refresh"
    }

    fn name(&self) -> &str {
        "Scoped Refresh"
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        self.models.lock().unwrap_or_else(PoisonError::into_inner).clone()
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

#[tokio::test]
async fn serves_cached_snapshot_immediately_updates_after_refresh_and_cancels_on_abort() {
    let (started_tx, mut started_rx) = watch::channel(0usize);
    let (release_tx, release_rx) = watch::channel(false);
    let provider = Arc::new(ScopedRefreshProvider {
        auth: ProviderAuth {
            api_key: Some(Arc::new(StubApiKeyAuth)),
            oauth: None,
        },
        models: Mutex::new(vec![make_model("cached", "Cached")]),
        started_tx,
        release_rx,
    });

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(InMemoryCredentialStore::new())),
        models_path: Some(None),
        allow_model_network: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .expect("create runtime");
    runtime
        .register_native_provider(provider.clone())
        .expect("register native provider");
    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            providers: Some(vec!["scoped-refresh".to_owned()]),
            force: None,
            signal: None,
        }))
        .await
        .expect("initial offline load");

    // 1. Cached snapshot immediately shows `cached` while background network refresh is pending.
    let initial_ids: Vec<String> = runtime
        .get_available_snapshot()
        .into_iter()
        .filter(|m| m.provider == "scoped-refresh")
        .map(|m| m.id)
        .collect();
    assert_eq!(initial_ids, vec!["cached"]);

    let rt_bg = runtime.clone();
    let controller = AbortController::new();
    let bg_signal = controller.signal();
    let bg_task = tokio::spawn(async move {
        rt_bg
            .refresh(Some(ModelsRefreshOptions {
                allow_network: Some(true),
                providers: Some(vec!["scoped-refresh".to_owned()]),
                force: Some(true),
                signal: Some(bg_signal),
            }))
            .await
    });
    while *started_rx.borrow() < 1 {
        started_rx.changed().await.unwrap();
    }

    // While in-flight, cached snapshot stays intact without blocking.
    assert!(runtime.get_available_snapshot().iter().any(|m| m.id == "cached"));
    assert!(!runtime.get_available_snapshot().iter().any(|m| m.id == "refreshed"));

    // Completing refresh updates available snapshot.
    *provider.models.lock().unwrap() = vec![make_model("cached", "Cached"), make_model("refreshed", "Refreshed")];
    let _ = release_tx.send(true);
    let res = bg_task.await.unwrap().expect("refresh complete");
    assert!(!res.aborted);

    let updated_ids: Vec<String> = runtime
        .get_available_snapshot()
        .into_iter()
        .filter(|m| m.provider == "scoped-refresh")
        .map(|m| m.id)
        .collect();
    assert!(updated_ids.contains(&"cached".to_owned()));
    assert!(updated_ids.contains(&"refreshed".to_owned()));

    // 2. Cancelling background refresh when selector closes aborts signal cleanly.
    let _ = release_tx.send(false);
    let close_controller = AbortController::new();
    let close_signal = close_controller.signal();
    let rt_cancel = runtime.clone();
    let target_start = *started_rx.borrow() + 1;
    let cancel_task = tokio::spawn(async move {
        rt_cancel
            .refresh(Some(ModelsRefreshOptions {
                allow_network: Some(true),
                providers: Some(vec!["scoped-refresh".to_owned()]),
                force: Some(true),
                signal: Some(close_signal),
            }))
            .await
    });
    while *started_rx.borrow() < target_start {
        started_rx.changed().await.unwrap();
    }
    close_controller.abort(None);
    let cancel_res = cancel_task.await.unwrap().expect("aborted refresh returns Ok");
    assert!(cancel_res.aborted);
}
