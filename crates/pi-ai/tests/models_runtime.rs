//! Port of packages/ai/test/models-runtime.test.ts

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use indexmap::IndexMap;
use pi_ai::auth::credential_store::InMemoryCredentialStore;
use pi_ai::auth::resolve::AuthResolutionOverrides;
use pi_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthCheck, AuthEvent, AuthInteraction, AuthOperationOptions,
    AuthPrompt, AuthResult, AuthType, Credential, CredentialInfo, CredentialStore, LoginOptions, ModelAuth,
    ModifyCredentialFn, OAuthAuth, OAuthCredential, ProviderAuth, ProviderAuthInteraction,
};
use pi_ai::models::{
    CreateModelsOptions, CreateProviderApi, CreateProviderOptions, ModelsError, ModelsErrorCode, ModelsPublication,
    ModelsRefreshOptions, ModelsSimpleStreamOptions, Provider, RefreshModelsContext, calculate_cost, create_models,
    create_provider, has_api,
};
use pi_ai::models_store::{InMemoryModelsStore, ModelsStore, ModelsStoreEntry, ModelsStoreOperationOptions};
use pi_ai::types::{
    AnyModel, ApiStreamOptions, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Message,
    Model, ModelCost, ModelCostTier, ModelInput, ProviderEnv, ProviderHeaders, ProviderStreams, SimpleStreamOptions,
    StopReason, StreamOptions, TextContent, TranscriptContext, Usage, UsageCost, UserMessage, UserMessageContent,
};
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::{BoxFuture, Error, Result, time};
use tokio::sync::Notify;

fn test_model(provider: &str, id: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: id.to_owned(),
        api: "test-api".to_owned(),
        provider: provider.to_owned(),
        base_url: "https://example.test/v1".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 10000,
        max_tokens: 1000,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn done_message(model: &Model, text: &str) -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new(text))],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: time::now_ms(),
    }
}

#[derive(Clone)]
struct ProviderCall {
    model: Model,
    options: Option<StreamOptions>,
}

struct AmbientAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for AmbientAuth {
    fn name(&self) -> &str {
        "Ambient"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        Ok(Some(AuthResult::default()))
    }
}

struct EnvKeyAuth {
    key: Option<String>,
    supports_login_flag: bool,
    login_key: Option<String>,
}

impl EnvKeyAuth {
    fn new(key: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            key: key.map(str::to_owned),
            supports_login_flag: false,
            login_key: None,
        })
    }

    fn with_login(key: Option<&str>, login_key: &str) -> Arc<Self> {
        Arc::new(Self {
            key: key.map(str::to_owned),
            supports_login_flag: true,
            login_key: Some(login_key.to_owned()),
        })
    }
}

#[async_trait::async_trait]
impl ApiKeyAuth for EnvKeyAuth {
    fn name(&self) -> &str {
        "Test API key"
    }

    fn supports_login(&self) -> bool {
        self.supports_login_flag
    }

    async fn login(&self, _interaction: ProviderAuthInteraction) -> Result<ApiKeyCredential> {
        Ok(ApiKeyCredential {
            key: self.login_key.clone(),
            env: None,
        })
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        let source = if input.credential.is_some() { "stored" } else { "env" };
        let resolved = input.credential.and_then(|c| c.key).or_else(|| self.key.clone());
        let Some(k) = resolved else {
            return Ok(None);
        };
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(k),
                headers: None,
                base_url: None,
            },
            env: None,
            source: Some(source.to_owned()),
        }))
    }
}

type RefreshFn = Arc<dyn Fn(OAuthCredential, AbortSignal) -> BoxFuture<Result<OAuthCredential>> + Send + Sync>;

struct TestOAuth {
    refresh_fn: Option<RefreshFn>,
}

impl TestOAuth {
    fn new() -> Arc<Self> {
        Arc::new(Self { refresh_fn: None })
    }

    fn with_refresh(refresh_fn: RefreshFn) -> Arc<Self> {
        Arc::new(Self {
            refresh_fn: Some(refresh_fn),
        })
    }
}

#[async_trait::async_trait]
impl OAuthAuth for TestOAuth {
    fn name(&self) -> &str {
        "Test OAuth"
    }

    async fn login(
        &self,
        _interaction: ProviderAuthInteraction,
        _options: Option<LoginOptions>,
    ) -> Result<OAuthCredential> {
        Err(Error::msg("not used"))
    }

    async fn refresh(&self, credential: OAuthCredential, signal: AbortSignal) -> Result<OAuthCredential> {
        if let Some(f) = &self.refresh_fn {
            f(credential, signal).await
        } else {
            Ok(credential)
        }
    }

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access),
            headers: None,
            base_url: None,
        })
    }
}

type GetModelsFn = Arc<dyn Fn() -> Vec<Model> + Send + Sync>;
type RefreshModelsFn = Arc<dyn Fn(RefreshModelsContext) -> BoxFuture<Result<()>> + Send + Sync>;

struct TestProvider {
    id: String,
    models: Vec<Model>,
    auth: ProviderAuth,
    get_models_fn: Option<GetModelsFn>,
    refresh_fn: Option<RefreshModelsFn>,
    calls: Option<Arc<Mutex<Vec<ProviderCall>>>>,
    get_all_models_fn: Option<Arc<dyn Fn() -> Option<Vec<AnyModel>> + Send + Sync>>,
}

impl TestProvider {
    fn respond(&self, model: &Model, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        if let Some(c) = &self.calls {
            c.lock().unwrap().push(ProviderCall {
                model: model.clone(),
                options,
            });
        }
        let s = AssistantMessageEventStream::new();
        let message = done_message(model, "ok");
        s.push(AssistantMessageEvent::Start {
            partial: message.clone(),
        });
        s.push(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: message.clone(),
        });
        s.end(Some(message));
        s
    }
}

impl Provider for TestProvider {
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
        if let Some(f) = &self.get_models_fn {
            f()
        } else {
            self.models.clone()
        }
    }

    fn get_all_models(&self) -> Option<Vec<AnyModel>> {
        if let Some(f) = &self.get_all_models_fn {
            f()
        } else {
            None
        }
    }

    fn refresh_models(&self, context: RefreshModelsContext) -> Option<BoxFuture<Result<()>>> {
        self.refresh_fn.as_ref().map(|f| f(context))
    }

    fn stream(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.respond(model, options.map(|o| o.base))
    }

    fn stream_simple(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.respond(model, options.map(|o| o.base))
    }
}

fn default_provider(id: &str) -> Arc<TestProvider> {
    Arc::new(TestProvider {
        id: id.to_owned(),
        models: vec![test_model(id, "model-a")],
        auth: ProviderAuth {
            api_key: Some(Arc::new(AmbientAuth)),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    })
}

fn sample_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
    }
}

#[tokio::test]
async fn enumerates_credential_metadata_without_exposing_secrets() {
    let credentials = InMemoryCredentialStore::new();
    credentials
        .modify(
            "api-provider",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("secret".to_owned()),
                        env: None,
                    })))
                })
            }),
            None,
        )
        .await
        .unwrap();
    credentials
        .modify(
            "oauth-provider",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "refresh",
                        "access",
                        time::now_ms() + 60_000,
                    ))))
                })
            }),
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        credentials.list(None).await.unwrap(),
        vec![
            CredentialInfo {
                provider_id: "api-provider".to_owned(),
                credential_type: AuthType::ApiKey,
            },
            CredentialInfo {
                provider_id: "oauth-provider".to_owned(),
                credential_type: AuthType::OAuth,
            },
        ]
    );
}

#[test]
fn applies_request_wide_pricing_tiers_above_the_configured_input_threshold() {
    let mut model = test_model("openai", "gpt-5.6-sol");
    model.cost = ModelCost {
        input: 5.0,
        output: 30.0,
        cache_read: 0.5,
        cache_write: 6.25,
        tiers: Some(vec![ModelCostTier {
            input_tokens_above: 272_000,
            input: 10.0,
            output: 45.0,
            cache_read: 1.0,
            cache_write: 12.5,
        }]),
    };
    let make_usage = |cache_write: i64| Usage {
        input: 200_000,
        output: 100_000,
        cache_read: 72_000,
        cache_write,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 372_000 + cache_write,
        cost: UsageCost::default(),
    };

    let any_model = AnyModel::Chat(model);
    let mut short_usage = make_usage(0);
    let short = calculate_cost(&any_model, &mut short_usage);
    assert_eq!(short.input, 1.0);
    assert_eq!(short.output, 3.0);
    assert_eq!(short.cache_read, 0.036);
    assert_eq!(short.cache_write, 0.0);

    let mut long_usage = make_usage(1);
    let long = calculate_cost(&any_model, &mut long_usage);
    assert_eq!(long.input, 2.0);
    assert_eq!(long.output, 4.5);
    assert_eq!(long.cache_read, 0.072);
    assert_eq!(long.cache_write, 0.0000125);
}

#[test]
fn registers_replaces_and_deletes_providers() {
    let models = create_models(None);
    models.set_provider(default_provider("p1"));
    models.set_provider(default_provider("p2"));
    assert_eq!(
        models
            .get_providers()
            .into_iter()
            .map(|p| p.id().to_owned())
            .collect::<Vec<_>>(),
        vec!["p1", "p2"]
    );

    let replacement: Arc<dyn Provider + Send + Sync> = default_provider("p1");
    models.set_provider(replacement.clone());
    assert!(Arc::ptr_eq(&models.get_provider("p1").unwrap(), &replacement));
    assert_eq!(models.get_providers().len(), 2);

    models.delete_provider("p1");
    assert!(models.get_provider("p1").is_none());

    models.clear_providers();
    assert!(models.get_providers().is_empty());
}

#[test]
fn lists_and_finds_models_per_provider() {
    let models = create_models(None);
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "m1"), test_model("p1", "m2")],
        auth: ProviderAuth {
            api_key: Some(Arc::new(AmbientAuth)),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "p2".to_owned(),
        models: vec![test_model("p2", "m3")],
        auth: ProviderAuth {
            api_key: Some(Arc::new(AmbientAuth)),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));

    assert_eq!(
        models.get_models(None).into_iter().map(|m| m.id).collect::<Vec<_>>(),
        vec!["m1", "m2", "m3"]
    );
    assert_eq!(
        models
            .get_models(Some("p1"))
            .into_iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        vec!["m1", "m2"]
    );
    assert!(models.get_models(Some("nope")).is_empty());
    assert_eq!(models.get_model("p2", "m3").map(|m| m.id), Some("m3".to_owned()));
    assert!(models.get_model("p2", "missing").is_none());

    let found = models.get_model("p2", "m3").unwrap();
    let any_found = AnyModel::Chat(found);
    assert!(!has_api(&any_found, "openai-completions"));
    assert!(has_api(&any_found, "test-api"));
}

#[tokio::test]
async fn keeps_chat_reads_independent_from_the_all_model_catalog() {
    // Rust's `get_all_models` returns `Option<Vec<AnyModel>>` (non-panicking), so simulate an empty/unavailable catalog
    // while `get_models` still returns chat models cleanly; `get_available` uses `get_models` independently.
    let provider = Arc::new(TestProvider {
        id: "chat-only".to_owned(),
        models: vec![test_model("chat-only", "model-a")],
        auth: ProviderAuth {
            api_key: Some(Arc::new(AmbientAuth)),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: Some(Arc::new(|| Some(Vec::new()))),
    });
    let models = create_models(None);
    models.set_provider(provider);

    assert_eq!(
        models
            .get_models(Some("chat-only"))
            .into_iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        vec!["model-a"]
    );
    assert_eq!(
        models.get_model("chat-only", "model-a").map(|m| m.id),
        Some("model-a".to_owned())
    );
    assert_eq!(
        models
            .get_available(Some("chat-only"), None)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        vec!["model-a"]
    );
    assert!(models.get_all_models(Some("chat-only")).is_empty());
}

#[tokio::test]
async fn refresh_updates_every_configured_dynamic_provider_and_reports_failures() {
    let list = Arc::new(Mutex::new(vec![test_model("dyn", "before")]));
    let refreshes = Arc::new(AtomicUsize::new(0));
    let list_get = list.clone();
    let list_upd = list.clone();
    let refreshes_clone = refreshes.clone();

    let models = create_models(None);
    models.set_provider(Arc::new(TestProvider {
        id: "dyn".to_owned(),
        models: Vec::new(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(AmbientAuth)),
            oauth: None,
        },
        get_models_fn: Some(Arc::new(move || list_get.lock().unwrap().clone())),
        refresh_fn: Some(Arc::new(move |refresh| {
            let list_inner = list_upd.clone();
            let ref_inner = refreshes_clone.clone();
            Box::pin(async move {
                if !refresh.allow_network {
                    return Ok(());
                }
                ref_inner.fetch_add(1, Ordering::SeqCst);
                (refresh.publish)(ModelsPublication {
                    persist: None,
                    update: Some(Arc::new(move || {
                        *list_inner.lock().unwrap() = vec![test_model("dyn", "after")];
                    })),
                })
                .await?;
                Ok(())
            })
        })),
        calls: None,
        get_all_models_fn: None,
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "static".to_owned(),
        models: vec![test_model("static", "s1")],
        auth: ProviderAuth {
            api_key: Some(Arc::new(AmbientAuth)),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));

    assert!(models.get_model("dyn", "before").is_some());
    let first = models.refresh(None).await.unwrap();
    assert!(first.errors.is_empty());
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert!(models.get_model("dyn", "after").is_some());
    assert!(models.get_model("dyn", "before").is_none());

    models.set_provider(Arc::new(TestProvider {
        id: "flaky".to_owned(),
        models: Vec::new(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(AmbientAuth)),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: Some(Arc::new(|refresh| {
            Box::pin(async move {
                if refresh.allow_network {
                    return Err(Error::msg("fetch failed"));
                }
                Ok(())
            })
        })),
        calls: None,
        get_all_models_fn: None,
    }));
    let second = models.refresh(None).await.unwrap();
    assert_eq!(refreshes.load(Ordering::SeqCst), 2);
    assert_eq!(
        second.errors.get("flaky").map(ToString::to_string).as_deref(),
        Some("fetch failed")
    );
}

#[tokio::test]
async fn restricts_refresh_work_to_selected_providers() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let models = create_models(None);
    for id in ["one", "two"] {
        let id_owned = id.to_owned();
        let calls_clone = calls.clone();
        models.set_provider(Arc::new(TestProvider {
            id: id_owned.clone(),
            models: vec![test_model(id, "model-a")],
            auth: ProviderAuth {
                api_key: Some(Arc::new(AmbientAuth)),
                oauth: None,
            },
            get_models_fn: None,
            refresh_fn: Some(Arc::new(move |refresh| {
                let id_c = id_owned.clone();
                let calls_c = calls_clone.clone();
                Box::pin(async move {
                    calls_c.lock().unwrap().push(format!(
                        "{}:{}",
                        id_c,
                        if refresh.allow_network { "network" } else { "cache" }
                    ));
                    Ok(())
                })
            })),
            calls: None,
            get_all_models_fn: None,
        }));
    }

    let result = models
        .refresh(Some(ModelsRefreshOptions {
            providers: Some(vec!["two".to_owned(), "unknown".to_owned()]),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert!(result.errors.is_empty());
    assert_eq!(*calls.lock().unwrap(), vec!["two:cache", "two:network"]);
}

#[tokio::test]
async fn restores_cached_models_before_waiting_for_network_auth() {
    struct BlockedAuth {
        started: Arc<Notify>,
        finish: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl ApiKeyAuth for BlockedAuth {
        fn name(&self) -> &str {
            "Blocked auth"
        }

        async fn resolve(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
            self.started.notify_waiters();
            self.finish.notified().await;
            Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some("key".to_owned()),
                    headers: None,
                    base_url: None,
                },
                env: None,
                source: None,
            }))
        }
    }

    let store = Arc::new(InMemoryModelsStore::new());
    store
        .write(
            "dynamic",
            ModelsStoreEntry {
                models: vec![AnyModel::Chat(test_model("dynamic", "cached"))],
                last_modified: None,
                checked_at: None,
                etag: None,
            },
            None,
        )
        .await
        .unwrap();

    let started = Arc::new(Notify::new());
    let finish = Arc::new(Notify::new());
    let provider = create_provider(CreateProviderOptions {
        id: "dynamic".to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(BlockedAuth {
                started: started.clone(),
                finish: finish.clone(),
            })),
            oauth: None,
        },
        models: Vec::new(),
        fetch_models: Some(Arc::new(|_| Box::pin(async { Err(Error::msg("must not fetch")) }))),
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(ProviderStreams {
            stream: Arc::new(|_, _, _| AssistantMessageEventStream::new()),
            stream_simple: Arc::new(|_, _, _| AssistantMessageEventStream::new()),
            fetch_deferred: None,
            cancel_deferred: None,
        })),
        images: None,
        classifiers: None,
    })
    .unwrap();

    let models = create_models(Some(CreateModelsOptions {
        models_store: Some(store),
        ..Default::default()
    }));
    models.set_provider(provider);
    let controller = AbortController::new();
    let models_c = models.clone();
    let sig = controller.signal();
    let pending = tokio::spawn(async move {
        models_c
            .refresh(Some(ModelsRefreshOptions {
                providers: Some(vec!["dynamic".to_owned()]),
                signal: Some(sig),
                ..Default::default()
            }))
            .await
    });
    started.notified().await;

    assert!(models.get_model("dynamic", "cached").is_some());
    controller.abort(None);
    assert!(pending.await.unwrap().unwrap().aborted);
    finish.notify_waiters();
}

#[tokio::test]
async fn lets_providers_choose_persistent_deletion_and_ephemeral_publication_atomically() {
    struct MockStore {
        entry: Arc<Mutex<Option<ModelsStoreEntry>>>,
    }

    #[async_trait::async_trait]
    impl ModelsStore for MockStore {
        async fn read(
            &self,
            _provider_id: &str,
            _options: Option<ModelsStoreOperationOptions>,
        ) -> Result<Option<ModelsStoreEntry>> {
            Ok(self.entry.lock().unwrap().clone())
        }

        async fn write(
            &self,
            _provider_id: &str,
            next: ModelsStoreEntry,
            _options: Option<ModelsStoreOperationOptions>,
        ) -> Result<()> {
            *self.entry.lock().unwrap() = Some(next);
            Ok(())
        }

        async fn delete(&self, _provider_id: &str, _options: Option<ModelsStoreOperationOptions>) -> Result<()> {
            *self.entry.lock().unwrap() = None;
            Ok(())
        }
    }

    let entry = Arc::new(Mutex::new(Some(ModelsStoreEntry {
        models: vec![AnyModel::Chat(test_model("dynamic", "stored"))],
        last_modified: None,
        checked_at: None,
        etag: None,
    })));
    let store = Arc::new(MockStore { entry: entry.clone() });
    let state = Arc::new(Mutex::new("initial".to_owned()));
    let state_upd1 = state.clone();
    let state_upd2 = state.clone();
    let entry_check = entry.clone();

    let models = create_models(Some(CreateModelsOptions {
        models_store: Some(store),
        ..Default::default()
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "dynamic".to_owned(),
        models: Vec::new(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(AmbientAuth)),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: Some(Arc::new(move |context| {
            let entry_c = entry_check.clone();
            let s1 = state_upd1.clone();
            let s2 = state_upd2.clone();
            Box::pin(async move {
                assert_eq!(context.stored.as_ref().unwrap().models[0].id(), "stored");
                (context.publish)(ModelsPublication {
                    persist: Some(None),
                    update: Some(Arc::new(move || {
                        assert!(entry_c.lock().unwrap().is_none());
                        *s1.lock().unwrap() = "deleted".to_owned();
                    })),
                })
                .await?;
                (context.publish)(ModelsPublication {
                    persist: None,
                    update: Some(Arc::new(move || {
                        *s2.lock().unwrap() = "ephemeral".to_owned();
                    })),
                })
                .await?;
                Ok(())
            })
        })),
        calls: None,
        get_all_models_fn: None,
    }));

    let result = models
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert!(result.errors.is_empty());
    assert!(entry.lock().unwrap().is_none());
    assert_eq!(*state.lock().unwrap(), "ephemeral");
}

#[tokio::test]
async fn persists_dynamic_catalogs_and_restores_them_without_network_access() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    let models_store = Arc::new(InMemoryModelsStore::new());
    credentials
        .modify(
            "dynamic",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("key".to_owned()),
                        env: None,
                    })))
                })
            }),
            None,
        )
        .await
        .unwrap();

    let make_dyn = |fetch: pi_ai::models::FetchModels| {
        create_provider(CreateProviderOptions {
            id: "dynamic".to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: Some(EnvKeyAuth::new(None)),
                oauth: None,
            },
            models: Vec::new(),
            fetch_models: Some(fetch),
            filter_models: None,
            filter_all_models: None,
            api: Some(CreateProviderApi::Single(ProviderStreams {
                stream: Arc::new(|_, _, _| AssistantMessageEventStream::new()),
                stream_simple: Arc::new(|_, _, _| AssistantMessageEventStream::new()),
                fetch_deferred: None,
                cancel_deferred: None,
            })),
            images: None,
            classifiers: None,
        })
        .unwrap()
    };

    let online = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials.clone()),
        models_store: Some(models_store.clone()),
        ..Default::default()
    }));
    online.set_provider(make_dyn(Arc::new(|_| {
        Box::pin(async { Ok(vec![AnyModel::Chat(test_model("dynamic", "fetched"))]) })
    })));
    assert!(online.refresh(None).await.unwrap().errors.is_empty());
    assert!(online.get_model("dynamic", "fetched").is_some());

    let offline = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials),
        models_store: Some(models_store),
        ..Default::default()
    }));
    offline.set_provider(make_dyn(Arc::new(|_| {
        Box::pin(async { Err(Error::msg("must not fetch")) })
    })));
    assert!(
        offline
            .refresh(Some(ModelsRefreshOptions {
                allow_network: Some(false),
                ..Default::default()
            }))
            .await
            .unwrap()
            .errors
            .is_empty()
    );
    assert!(offline.get_model("dynamic", "fetched").is_some());
}

#[tokio::test]
async fn passes_effective_api_key_credentials_and_refresh_options_while_skipping_unconfigured_providers() {
    let effective_credential = Arc::new(Mutex::new(None));
    let force_refresh = Arc::new(Mutex::new(None));
    let unconfigured_refreshes = Arc::new(AtomicUsize::new(0));

    let eff_clone = effective_credential.clone();
    let force_clone = force_refresh.clone();
    let unconf_clone = unconfigured_refreshes.clone();

    let models = create_models(None);
    models.set_provider(Arc::new(TestProvider {
        id: "configured".to_owned(),
        models: vec![test_model("configured", "model-a")],
        auth: ProviderAuth {
            api_key: Some(EnvKeyAuth::new(Some("ambient-key"))),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: Some(Arc::new(move |context| {
            let ec = eff_clone.clone();
            let fc = force_clone.clone();
            Box::pin(async move {
                if !context.allow_network {
                    return Ok(());
                }
                *ec.lock().unwrap() = context.credential;
                *fc.lock().unwrap() = context.force;
                Ok(())
            })
        })),
        calls: None,
        get_all_models_fn: None,
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "unconfigured".to_owned(),
        models: vec![test_model("unconfigured", "model-a")],
        auth: ProviderAuth {
            api_key: Some(EnvKeyAuth::new(None)),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: Some(Arc::new(move |context| {
            let uc = unconf_clone.clone();
            Box::pin(async move {
                if context.allow_network {
                    uc.fetch_add(1, Ordering::SeqCst);
                }
                Ok(())
            })
        })),
        calls: None,
        get_all_models_fn: None,
    }));

    models
        .refresh(Some(ModelsRefreshOptions {
            force: Some(true),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert_eq!(
        *effective_credential.lock().unwrap(),
        Some(Credential::ApiKey(ApiKeyCredential {
            key: Some("ambient-key".to_owned()),
            env: None,
        }))
    );
    assert_eq!(*force_refresh.lock().unwrap(), Some(true));
    assert_eq!(unconfigured_refreshes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn refreshes_expired_oauth_before_refreshing_models() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    let model_refresh_credential = Arc::new(Mutex::new(None));
    credentials
        .modify(
            "oauth-dynamic",
            Box::new(|_| {
                Box::pin(async { Ok(Some(Credential::OAuth(OAuthCredential::new("refresh", "expired", 0)))) })
            }),
            None,
        )
        .await
        .unwrap();

    let mrc = model_refresh_credential.clone();
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials.clone()),
        ..Default::default()
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "oauth-dynamic".to_owned(),
        models: vec![test_model("oauth-dynamic", "model-a")],
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(TestOAuth::with_refresh(Arc::new(|_, _| {
                Box::pin(async { Ok(OAuthCredential::new("rotated", "fresh", time::now_ms() + 60_000)) })
            }))),
        },
        get_models_fn: None,
        refresh_fn: Some(Arc::new(move |context| {
            let mrc_inner = mrc.clone();
            Box::pin(async move {
                if context.allow_network {
                    *mrc_inner.lock().unwrap() = context.credential;
                }
                Ok(())
            })
        })),
        calls: None,
        get_all_models_fn: None,
    }));

    assert!(models.refresh(None).await.unwrap().errors.is_empty());
    let captured = model_refresh_credential.lock().unwrap().clone().unwrap();
    assert_eq!(captured.as_oauth().unwrap().access, "fresh");
    assert_eq!(captured.as_oauth().unwrap().refresh, "rotated");
    let stored = credentials.read("oauth-dynamic", None).await.unwrap().unwrap();
    assert_eq!(stored.as_oauth().unwrap().access, "fresh");
    assert_eq!(stored.as_oauth().unwrap().refresh, "rotated");
}

#[tokio::test]
async fn always_gives_providers_a_concrete_signal() {
    let received_signal = Arc::new(Mutex::new(None));
    let rs = received_signal.clone();
    let models = create_models(None);
    models.set_provider(Arc::new(TestProvider {
        id: "dynamic".to_owned(),
        models: vec![test_model("dynamic", "model-a")],
        auth: ProviderAuth {
            api_key: Some(Arc::new(AmbientAuth)),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: Some(Arc::new(move |context| {
            let r = rs.clone();
            Box::pin(async move {
                *r.lock().unwrap() = Some(context.signal);
                Ok(())
            })
        })),
        calls: None,
        get_all_models_fn: None,
    }));

    let result = models.refresh(None).await.unwrap();
    assert!(!result.aborted);
    let sig = received_signal.lock().unwrap().take().unwrap();
    assert!(!sig.aborted());
}

#[tokio::test]
async fn binds_model_store_waits_to_the_provider_refresh_signal() {
    struct SignalStore {
        signals: Arc<Mutex<Vec<Option<AbortSignal>>>>,
    }

    #[async_trait::async_trait]
    impl ModelsStore for SignalStore {
        async fn read(
            &self,
            _provider_id: &str,
            options: Option<ModelsStoreOperationOptions>,
        ) -> Result<Option<ModelsStoreEntry>> {
            self.signals.lock().unwrap().push(options.and_then(|o| o.signal));
            Ok(None)
        }

        async fn write(
            &self,
            _provider_id: &str,
            _entry: ModelsStoreEntry,
            options: Option<ModelsStoreOperationOptions>,
        ) -> Result<()> {
            self.signals.lock().unwrap().push(options.and_then(|o| o.signal));
            Ok(())
        }

        async fn delete(&self, _provider_id: &str, options: Option<ModelsStoreOperationOptions>) -> Result<()> {
            self.signals.lock().unwrap().push(options.and_then(|o| o.signal));
            Ok(())
        }
    }

    let storage_signals = Arc::new(Mutex::new(Vec::new()));
    let provider_signal = Arc::new(Mutex::new(None));
    let ps = provider_signal.clone();
    let store = Arc::new(SignalStore {
        signals: storage_signals.clone(),
    });
    let models = create_models(Some(CreateModelsOptions {
        models_store: Some(store),
        ..Default::default()
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "dynamic".to_owned(),
        models: vec![test_model("dynamic", "model-a")],
        auth: ProviderAuth {
            api_key: Some(EnvKeyAuth::new(Some("key"))),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: Some(Arc::new(move |context| {
            let ps_c = ps.clone();
            Box::pin(async move {
                *ps_c.lock().unwrap() = Some(context.signal.clone());
                if !context.allow_network {
                    return Ok(());
                }
                (context.publish)(ModelsPublication {
                    persist: Some(Some(ModelsStoreEntry {
                        models: vec![AnyModel::Chat(test_model("dynamic", "fresh"))],
                        last_modified: None,
                        checked_at: None,
                        etag: None,
                    })),
                    update: None,
                })
                .await?;
                Ok(())
            })
        })),
        calls: None,
        get_all_models_fn: None,
    }));

    let result = models
        .refresh(Some(ModelsRefreshOptions {
            providers: Some(vec!["dynamic".to_owned()]),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert!(result.errors.is_empty());
    let sigs = storage_signals.lock().unwrap();
    assert_eq!(sigs.len(), 3);
    assert!(sigs.iter().all(|s| s.is_some()));
}

#[tokio::test]
async fn returns_aborted_state_without_reporting_cancellation_as_a_provider_error() {
    let controller = Arc::new(AbortController::new());
    let ctrl = controller.clone();
    let models = create_models(None);
    models.set_provider(Arc::new(TestProvider {
        id: "dynamic".to_owned(),
        models: vec![test_model("dynamic", "model-a")],
        auth: ProviderAuth {
            api_key: Some(Arc::new(AmbientAuth)),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: Some(Arc::new(move |context| {
            let c = ctrl.clone();
            Box::pin(async move {
                c.abort(None);
                if context.signal.aborted() {
                    return Ok(());
                }
                Ok(())
            })
        })),
        calls: None,
        get_all_models_fn: None,
    }));

    let result = models
        .refresh(Some(ModelsRefreshOptions {
            signal: Some(controller.signal()),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(result.aborted);
    assert!(result.errors.is_empty());
}

#[tokio::test]
async fn stops_waiting_on_abort_when_a_provider_ignores_its_signal() {
    let controller = AbortController::new();
    let started = Arc::new(Notify::new());
    let fail_notify = Arc::new(Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let started_c = started.clone();
    let fail_c = fail_notify.clone();
    let calls_c = calls.clone();

    let models = create_models(None);
    models.set_provider(Arc::new(TestProvider {
        id: "dynamic".to_owned(),
        models: vec![test_model("dynamic", "model-a")],
        auth: ProviderAuth {
            api_key: Some(Arc::new(AmbientAuth)),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: Some(Arc::new(move |_| {
            let s = started_c.clone();
            let f = fail_c.clone();
            let c = calls_c.clone();
            Box::pin(async move {
                if c.fetch_add(1, Ordering::SeqCst) + 1 != 1 {
                    return Ok(());
                }
                s.notify_waiters();
                f.notified().await;
                Err(Error::msg("late provider failure"))
            })
        })),
        calls: None,
        get_all_models_fn: None,
    }));

    let sig = controller.signal();
    let pending = tokio::spawn(async move {
        models
            .refresh(Some(ModelsRefreshOptions {
                signal: Some(sig),
                ..Default::default()
            }))
            .await
    });
    started.notified().await;
    controller.abort(None);

    let result = pending.await.unwrap().unwrap();
    assert!(result.aborted);
    assert!(result.errors.is_empty());

    fail_notify.notify_waiters();
    tokio::task::yield_now().await;
    assert!(result.errors.is_empty());
}

#[tokio::test]
async fn rejects_late_publication_from_a_superseded_non_cooperative_provider() {
    let store = Arc::new(InMemoryModelsStore::new());
    let state = Arc::new(Mutex::new("initial".to_owned()));
    let calls = Arc::new(AtomicUsize::new(0));
    let first_started = Arc::new(Notify::new());
    let finish_first = Arc::new(Notify::new());

    let state_c = state.clone();
    let calls_c = calls.clone();
    let fs_c = first_started.clone();
    let ff_c = finish_first.clone();

    let models = create_models(Some(CreateModelsOptions {
        models_store: Some(store.clone()),
        ..Default::default()
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "dynamic".to_owned(),
        models: vec![test_model("dynamic", "model-a")],
        auth: ProviderAuth {
            api_key: Some(Arc::new(AmbientAuth)),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: Some(Arc::new(move |context| {
            let st = state_c.clone();
            let ca = calls_c.clone();
            let fs = fs_c.clone();
            let ff = ff_c.clone();
            Box::pin(async move {
                if !context.allow_network {
                    return Ok(());
                }
                let current = ca.fetch_add(1, Ordering::SeqCst) + 1;
                if current == 1 {
                    fs.notify_waiters();
                    ff.notified().await;
                }
                let value = format!("generation-{current}");
                let val_for_update = value.clone();
                let st_upd = st.clone();
                (context.publish)(ModelsPublication {
                    persist: Some(Some(ModelsStoreEntry {
                        models: vec![AnyModel::Chat(test_model("dynamic", &value))],
                        last_modified: None,
                        checked_at: None,
                        etag: None,
                    })),
                    update: Some(Arc::new(move || {
                        *st_upd.lock().unwrap() = val_for_update.clone();
                    })),
                })
                .await?;
                Ok(())
            })
        })),
        calls: None,
        get_all_models_fn: None,
    }));

    let m1 = models.clone();
    let first = tokio::spawn(async move {
        m1.refresh(Some(ModelsRefreshOptions {
            providers: Some(vec!["dynamic".to_owned()]),
            ..Default::default()
        }))
        .await
    });
    first_started.notified().await;
    let second = models
        .refresh(Some(ModelsRefreshOptions {
            providers: Some(vec!["dynamic".to_owned()]),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(second.errors.is_empty());
    let _ = first.await.unwrap().unwrap();
    finish_first.notify_waiters();
    tokio::task::yield_now().await;

    assert_eq!(*state.lock().unwrap(), "generation-2");
    assert_eq!(
        store.read("dynamic", None).await.unwrap().unwrap().models[0].id(),
        "generation-2"
    );
}

#[tokio::test]
async fn passes_caller_signals_to_provider_auth_callbacks() {
    struct SignalCapturingAuth {
        received: Arc<Mutex<Vec<AbortSignal>>>,
    }

    #[async_trait::async_trait]
    impl ApiKeyAuth for SignalCapturingAuth {
        fn name(&self) -> &str {
            "Signal auth"
        }

        fn supports_login(&self) -> bool {
            true
        }

        async fn login(&self, interaction: ProviderAuthInteraction) -> Result<ApiKeyCredential> {
            self.received.lock().unwrap().push(interaction.signal);
            Ok(ApiKeyCredential {
                key: Some("saved".to_owned()),
                env: None,
            })
        }

        fn supports_check(&self) -> bool {
            true
        }

        async fn check(&self, input: ApiKeyAuthInput) -> Result<Option<AuthCheck>> {
            self.received.lock().unwrap().push(input.signal);
            Ok(Some(AuthCheck {
                source: None,
                auth_type: AuthType::ApiKey,
            }))
        }

        async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
            self.received.lock().unwrap().push(input.signal);
            Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some("resolved".to_owned()),
                    headers: None,
                    base_url: None,
                },
                env: None,
                source: None,
            }))
        }
    }

    struct UnusedInteraction {
        signal: AbortSignal,
    }

    #[async_trait::async_trait]
    impl AuthInteraction for UnusedInteraction {
        fn signal(&self) -> Option<&AbortSignal> {
            Some(&self.signal)
        }

        async fn prompt(&self, _prompt: AuthPrompt) -> Result<String> {
            Ok("unused".to_owned())
        }

        fn notify(&self, _event: AuthEvent) {}
    }

    let controller = AbortController::new();
    let received = Arc::new(Mutex::new(Vec::new()));
    let models = create_models(None);
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: Some(Arc::new(SignalCapturingAuth {
                received: received.clone(),
            })),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));

    models
        .check_auth(
            "p1",
            Some(AuthOperationOptions {
                signal: Some(controller.signal()),
            }),
        )
        .await
        .unwrap();
    models
        .get_auth_provider_id(
            "p1",
            Some(AuthResolutionOverrides {
                signal: Some(controller.signal()),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    models
        .login(
            "p1",
            AuthType::ApiKey,
            Arc::new(UnusedInteraction {
                signal: controller.signal(),
            }),
            None,
        )
        .await
        .unwrap();

    let sigs = received.lock().unwrap();
    assert_eq!(sigs.len(), 3);
    controller.abort(None);
    assert!(sigs.iter().all(AbortSignal::aborted));
}

#[tokio::test]
async fn stops_waiting_for_non_cooperative_auth_callbacks() {
    struct BlockedAuthCallbacks {
        start_check: Arc<Notify>,
        finish_check: Arc<Notify>,
        start_resolve: Arc<Notify>,
        finish_resolve: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl ApiKeyAuth for BlockedAuthCallbacks {
        fn name(&self) -> &str {
            "Blocked auth"
        }

        fn supports_check(&self) -> bool {
            true
        }

        async fn check(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthCheck>> {
            self.start_check.notify_waiters();
            self.finish_check.notified().await;
            Ok(Some(AuthCheck {
                source: None,
                auth_type: AuthType::ApiKey,
            }))
        }

        async fn resolve(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
            self.start_resolve.notify_waiters();
            self.finish_resolve.notified().await;
            Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some("key".to_owned()),
                    headers: None,
                    base_url: None,
                },
                env: None,
                source: None,
            }))
        }
    }

    let start_check = Arc::new(Notify::new());
    let finish_check = Arc::new(Notify::new());
    let start_resolve = Arc::new(Notify::new());
    let finish_resolve = Arc::new(Notify::new());

    let models = create_models(None);
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: Some(Arc::new(BlockedAuthCallbacks {
                start_check: start_check.clone(),
                finish_check: finish_check.clone(),
                start_resolve: start_resolve.clone(),
                finish_resolve: finish_resolve.clone(),
            })),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));

    let available_controller = AbortController::new();
    let m1 = models.clone();
    let sig1 = available_controller.signal();
    let available = tokio::spawn(async move {
        m1.get_available(None, Some(AuthOperationOptions { signal: Some(sig1) }))
            .await
    });
    start_check.notified().await;
    available_controller.abort(None);
    let err1 = available.await.unwrap().unwrap_err();
    assert_eq!(err1.name(), "AbortError");

    let auth_controller = AbortController::new();
    let m2 = models.clone();
    let sig2 = auth_controller.signal();
    let auth = tokio::spawn(async move {
        m2.get_auth_provider_id(
            "p1",
            Some(AuthResolutionOverrides {
                signal: Some(sig2),
                ..Default::default()
            }),
        )
        .await
    });
    start_resolve.notified().await;
    auth_controller.abort(None);
    let err2 = auth.await.unwrap().unwrap_err();
    assert_eq!(err2.name(), "AbortError");

    finish_check.notify_waiters();
    finish_resolve.notify_waiters();
}

#[tokio::test]
async fn cancels_queued_credential_mutations_without_running_them_later() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    let first_started = Arc::new(Notify::new());
    let finish_first = Arc::new(Notify::new());
    let second_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let c1 = credentials.clone();
    let fs1 = first_started.clone();
    let ff1 = finish_first.clone();
    let first = tokio::spawn(async move {
        c1.modify(
            "p1",
            Box::new(move |_| {
                Box::pin(async move {
                    fs1.notify_waiters();
                    ff1.notified().await;
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("first".to_owned()),
                        env: None,
                    })))
                })
            }),
            None,
        )
        .await
    });

    first_started.notified().await;

    let controller = AbortController::new();
    let sr2 = second_ran.clone();
    let c2 = credentials.clone();
    let sig2 = controller.signal();
    let second = tokio::spawn(async move {
        c2.modify(
            "p1",
            Box::new(move |_| {
                sr2.store(true, Ordering::SeqCst);
                Box::pin(async {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("second".to_owned()),
                        env: None,
                    })))
                })
            }),
            Some(AuthOperationOptions { signal: Some(sig2) }),
        )
        .await
    });

    tokio::task::yield_now().await;
    controller.abort(None);
    let err = second.await.unwrap().unwrap_err();
    assert_eq!(err.name(), "AbortError");

    finish_first.notify_waiters();
    first.await.unwrap().unwrap();
    tokio::task::yield_now().await;

    assert!(!second_ran.load(Ordering::SeqCst));
    assert_eq!(
        credentials.read("p1", None).await.unwrap(),
        Some(Credential::ApiKey(ApiKeyCredential {
            key: Some("first".to_owned()),
            env: None,
        }))
    );
}

#[tokio::test]
async fn passes_cancellation_to_oauth_refresh_and_preserves_the_previous_credential() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    let previous = OAuthCredential::new("old-refresh", "old", 0);
    let prev_clone = previous.clone();
    credentials
        .modify(
            "p1",
            Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(prev_clone))) })),
            None,
        )
        .await
        .unwrap();

    let refresh_started = Arc::new(Notify::new());
    let finish_refresh = Arc::new(Notify::new());
    let received_signal = Arc::new(Mutex::new(None));

    let rs_c = refresh_started.clone();
    let ff_c = finish_refresh.clone();
    let sig_c = received_signal.clone();

    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials.clone()),
        ..Default::default()
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(TestOAuth::with_refresh(Arc::new(move |cred, signal| {
                let rs = rs_c.clone();
                let ff = ff_c.clone();
                let sg = sig_c.clone();
                Box::pin(async move {
                    *sg.lock().unwrap() = Some(signal);
                    rs.notify_waiters();
                    ff.notified().await;
                    Ok(OAuthCredential {
                        access: "new".to_owned(),
                        expires: time::now_ms() + 60_000,
                        ..cred
                    })
                })
            }))),
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));

    let controller = AbortController::new();
    let sig = controller.signal();
    let m_c = models.clone();
    let auth = tokio::spawn(async move {
        m_c.get_auth_provider_id(
            "p1",
            Some(AuthResolutionOverrides {
                signal: Some(sig),
                ..Default::default()
            }),
        )
        .await
    });
    refresh_started.notified().await;
    controller.abort(None);

    let err = auth.await.unwrap().unwrap_err();
    assert_eq!(err.name(), "AbortError");
    assert!(received_signal.lock().unwrap().as_ref().unwrap().aborted());
    finish_refresh.notify_waiters();
    tokio::task::yield_now().await;
    assert_eq!(
        credentials.read("p1", None).await.unwrap(),
        Some(Credential::OAuth(previous))
    );
}

#[tokio::test]
async fn resolves_auth_stored_credential_owns_the_provider_ambient_only_when_nothing_stored() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials.clone()),
        ..Default::default()
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: Some(EnvKeyAuth::new(Some("env-key"))),
            oauth: Some(TestOAuth::new()),
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));
    let model = test_model("p1", "model-a");
    let any_model = AnyModel::Chat(model.clone());

    assert_eq!(
        models
            .get_auth_model(&any_model, None)
            .await
            .unwrap()
            .and_then(|r| r.auth.api_key),
        Some("env-key".to_owned())
    );
    assert_eq!(
        models
            .get_auth_provider_id(&model.provider, None)
            .await
            .unwrap()
            .and_then(|r| r.auth.api_key),
        Some("env-key".to_owned())
    );
    assert_eq!(
        models
            .get_auth_model(
                &any_model,
                Some(AuthResolutionOverrides {
                    api_key: Some("explicit-key".to_owned()),
                    ..Default::default()
                }),
            )
            .await
            .unwrap()
            .and_then(|r| r.auth.api_key),
        Some("explicit-key".to_owned())
    );

    credentials
        .modify(
            "p1",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "r",
                        "oauth-token",
                        time::now_ms() + 10 * 60_000,
                    ))))
                })
            }),
            None,
        )
        .await
        .unwrap();
    let resolution = models.get_auth_provider_id("p1", None).await.unwrap().unwrap();
    assert_eq!(resolution.auth.api_key.as_deref(), Some("oauth-token"));
    assert_eq!(resolution.source.as_deref(), Some("OAuth"));

    credentials
        .modify(
            "p1",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("stored-key".to_owned()),
                        env: None,
                    })))
                })
            }),
            None,
        )
        .await
        .unwrap();
    let api_res = models.get_auth_provider_id("p1", None).await.unwrap().unwrap();
    assert_eq!(api_res.auth.api_key.as_deref(), Some("stored-key"));
    assert_eq!(api_res.source.as_deref(), Some("stored"));
}

#[tokio::test]
async fn checks_provider_auth_without_refreshing_oauth_and_filters_available_models() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    let refreshes = Arc::new(AtomicUsize::new(0));
    let refreshes_c = refreshes.clone();
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials.clone()),
        ..Default::default()
    }));

    models.set_provider(Arc::new(TestProvider {
        id: "ambient".to_owned(),
        models: vec![test_model("ambient", "model-a")],
        auth: ProviderAuth {
            api_key: Some(EnvKeyAuth::new(Some("env-key"))),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "missing".to_owned(),
        models: vec![test_model("missing", "model-a")],
        auth: ProviderAuth {
            api_key: Some(EnvKeyAuth::new(None)),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "oauth".to_owned(),
        models: vec![test_model("oauth", "model-a")],
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(TestOAuth::with_refresh(Arc::new(move |cred, _| {
                let r = refreshes_c.clone();
                Box::pin(async move {
                    r.fetch_add(1, Ordering::SeqCst);
                    Ok(cred)
                })
            }))),
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));
    credentials
        .modify(
            "oauth",
            Box::new(|_| {
                Box::pin(async { Ok(Some(Credential::OAuth(OAuthCredential::new("refresh", "expired", 0)))) })
            }),
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        models.check_auth("ambient", None).await.unwrap(),
        Some(AuthCheck {
            source: Some("env".to_owned()),
            auth_type: AuthType::ApiKey,
        })
    );
    assert_eq!(models.check_auth("missing", None).await.unwrap(), None);
    assert_eq!(
        models.check_auth("oauth", None).await.unwrap(),
        Some(AuthCheck {
            source: Some("OAuth".to_owned()),
            auth_type: AuthType::OAuth,
        })
    );
    assert_eq!(refreshes.load(Ordering::SeqCst), 0);
    assert_eq!(
        models
            .get_available(None, None)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.provider)
            .collect::<Vec<_>>(),
        vec!["ambient", "oauth"]
    );
    assert_eq!(
        models
            .get_available(Some("ambient"), None)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.provider)
            .collect::<Vec<_>>(),
        vec!["ambient"]
    );
}

#[tokio::test]
async fn runs_provider_login_and_logout_through_the_credential_store() {
    struct UnusedInteraction;
    #[async_trait::async_trait]
    impl AuthInteraction for UnusedInteraction {
        async fn prompt(&self, _prompt: AuthPrompt) -> Result<String> {
            Ok("unused".to_owned())
        }
        fn notify(&self, _event: AuthEvent) {}
    }

    let credentials = Arc::new(InMemoryCredentialStore::new());
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials.clone()),
        ..Default::default()
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: Some(EnvKeyAuth::with_login(None, "logged-in")),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));

    let credential = models
        .login("p1", AuthType::ApiKey, Arc::new(UnusedInteraction), None)
        .await
        .unwrap();
    assert_eq!(
        credential,
        Credential::ApiKey(ApiKeyCredential {
            key: Some("logged-in".to_owned()),
            env: None,
        })
    );
    assert_eq!(credentials.read("p1", None).await.unwrap(), Some(credential));

    models.logout("p1", None).await.unwrap();
    assert!(credentials.read("p1", None).await.unwrap().is_none());
}

#[tokio::test]
async fn a_stored_credential_without_a_matching_handler_blocks_ambient_fallback() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials.clone()),
        ..Default::default()
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: Some(EnvKeyAuth::new(Some("env-key"))),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));
    credentials
        .modify(
            "p1",
            Box::new(|_| Box::pin(async { Ok(Some(Credential::OAuth(OAuthCredential::new("r", "a", 0)))) })),
            None,
        )
        .await
        .unwrap();

    assert!(models.get_auth_provider_id("p1", None).await.unwrap().is_none());
}

#[tokio::test]
async fn refreshes_expired_oauth_credentials_and_persists_the_rotated_credential() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials.clone()),
        ..Default::default()
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(TestOAuth::with_refresh(Arc::new(|cred, _| {
                Box::pin(async move {
                    Ok(OAuthCredential {
                        access: "new-token".to_owned(),
                        expires: time::now_ms() + 60 * 60_000,
                        ..cred
                    })
                })
            }))),
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));
    credentials
        .modify(
            "p1",
            Box::new(|_| Box::pin(async { Ok(Some(Credential::OAuth(OAuthCredential::new("r", "old-token", 0)))) })),
            None,
        )
        .await
        .unwrap();

    let resolution = models.get_auth_provider_id("p1", None).await.unwrap().unwrap();
    assert_eq!(resolution.auth.api_key.as_deref(), Some("new-token"));
    assert_eq!(
        credentials
            .read("p1", None)
            .await
            .unwrap()
            .unwrap()
            .as_oauth()
            .unwrap()
            .access,
        "new-token"
    );
}

#[tokio::test]
async fn refreshes_oauth_credentials_with_less_than_five_minutes_remaining() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_c = calls.clone();
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials.clone()),
        ..Default::default()
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(TestOAuth::with_refresh(Arc::new(move |cred, _| {
                let c = calls_c.clone();
                Box::pin(async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(OAuthCredential {
                        access: "new-token".to_owned(),
                        expires: time::now_ms() + 60 * 60_000,
                        ..cred
                    })
                })
            }))),
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));
    credentials
        .modify(
            "p1",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "r",
                        "old-token",
                        time::now_ms() + 60_000,
                    ))))
                })
            }),
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        models
            .get_auth_provider_id("p1", None)
            .await
            .unwrap()
            .and_then(|r| r.auth.api_key),
        Some("new-token".to_owned())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn honors_a_callers_longer_oauth_minimum_validity() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_c = calls.clone();
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials.clone()),
        ..Default::default()
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(TestOAuth::with_refresh(Arc::new(move |cred, _| {
                let c = calls_c.clone();
                Box::pin(async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(OAuthCredential {
                        access: "new-token".to_owned(),
                        expires: time::now_ms() + 60 * 60_000,
                        ..cred
                    })
                })
            }))),
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));
    credentials
        .modify(
            "p1",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "r",
                        "old-token",
                        time::now_ms() + 10 * 60_000,
                    ))))
                })
            }),
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        models
            .get_auth_provider_id(
                "p1",
                Some(AuthResolutionOverrides {
                    min_oauth_validity_ms: Some(30 * 60_000),
                    ..Default::default()
                }),
            )
            .await
            .unwrap()
            .and_then(|r| r.auth.api_key),
        Some("new-token".to_owned())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn rejects_with_code_oauth_when_refresh_fails_preserving_the_stored_credential() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials.clone()),
        ..Default::default()
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(TestOAuth::with_refresh(Arc::new(|_, _| {
                Box::pin(async { Err(Error::msg("invalid_grant")) })
            }))),
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));
    credentials
        .modify(
            "p1",
            Box::new(|_| Box::pin(async { Ok(Some(Credential::OAuth(OAuthCredential::new("r", "old", 0)))) })),
            None,
        )
        .await
        .unwrap();

    let err = models.get_auth_provider_id("p1", None).await.unwrap_err();
    let me = err.downcast_ref::<ModelsError>().unwrap();
    assert_eq!(me.code, ModelsErrorCode::OAuth);
    assert_eq!(
        credentials
            .read("p1", None)
            .await
            .unwrap()
            .unwrap()
            .as_oauth()
            .unwrap()
            .access,
        "old"
    );
}

#[tokio::test]
async fn serializes_concurrent_oauth_refreshes_through_store_modify_no_double_refresh() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    credentials
        .modify(
            "p1",
            Box::new(|_| Box::pin(async { Ok(Some(Credential::OAuth(OAuthCredential::new("r1", "old", 0)))) })),
            None,
        )
        .await
        .unwrap();

    let refreshes = Arc::new(AtomicUsize::new(0));
    let refreshes_c = refreshes.clone();
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials),
        ..Default::default()
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(TestOAuth::with_refresh(Arc::new(move |_, _| {
                let r = refreshes_c.clone();
                Box::pin(async move {
                    let n = r.fetch_add(1, Ordering::SeqCst) + 1;
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    Ok(OAuthCredential::new(
                        "r2",
                        format!("new-{n}"),
                        time::now_ms() + 60 * 60_000,
                    ))
                })
            }))),
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));

    let (a, b) = tokio::join!(
        models.get_auth_provider_id("p1", None),
        models.get_auth_provider_id("p1", None),
    );
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(a.unwrap().unwrap().auth.api_key.as_deref(), Some("new-1"));
    assert_eq!(b.unwrap().unwrap().auth.api_key.as_deref(), Some("new-1"));
}

#[tokio::test]
async fn valid_oauth_tokens_resolve_without_touching_modify() {
    struct CountingStore {
        base: InMemoryCredentialStore,
        modifies: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl CredentialStore for CountingStore {
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
            self.modifies.fetch_add(1, Ordering::SeqCst);
            self.base.modify(provider_id, f, options).await
        }

        async fn delete(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<()> {
            self.base.delete(provider_id, options).await
        }
    }

    let base = InMemoryCredentialStore::new();
    base.modify(
        "p1",
        Box::new(|_| {
            Box::pin(async {
                Ok(Some(Credential::OAuth(OAuthCredential::new(
                    "r",
                    "valid",
                    time::now_ms() + 10 * 60_000,
                ))))
            })
        }),
        None,
    )
    .await
    .unwrap();
    let modifies = Arc::new(AtomicUsize::new(0));
    let credentials = Arc::new(CountingStore {
        base,
        modifies: modifies.clone(),
    });
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials),
        ..Default::default()
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(TestOAuth::new()),
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));

    assert_eq!(
        models
            .get_auth_provider_id("p1", None)
            .await
            .unwrap()
            .and_then(|r| r.auth.api_key),
        Some("valid".to_owned())
    );
    assert_eq!(modifies.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn wraps_credential_store_failures_in_models_error() {
    struct ReadFailingStore;
    #[async_trait::async_trait]
    impl CredentialStore for ReadFailingStore {
        async fn read(&self, _: &str, _: Option<AuthOperationOptions>) -> Result<Option<Credential>> {
            Err(Error::msg("disk on fire"))
        }
        async fn list(&self, _: Option<AuthOperationOptions>) -> Result<Vec<CredentialInfo>> {
            Ok(Vec::new())
        }
        async fn modify(
            &self,
            _: &str,
            _: ModifyCredentialFn,
            _: Option<AuthOperationOptions>,
        ) -> Result<Option<Credential>> {
            Ok(None)
        }
        async fn delete(&self, _: &str, _: Option<AuthOperationOptions>) -> Result<()> {
            Ok(())
        }
    }

    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(Arc::new(ReadFailingStore)),
        ..Default::default()
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: Some(EnvKeyAuth::new(Some("env-key"))),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));
    let err = models.get_auth_provider_id("p1", None).await.unwrap_err();
    assert_eq!(err.downcast_ref::<ModelsError>().unwrap().code, ModelsErrorCode::Auth);

    struct ModifyFailingStore;
    #[async_trait::async_trait]
    impl CredentialStore for ModifyFailingStore {
        async fn read(&self, _: &str, _: Option<AuthOperationOptions>) -> Result<Option<Credential>> {
            Ok(Some(Credential::OAuth(OAuthCredential::new("r", "old", 0))))
        }
        async fn list(&self, _: Option<AuthOperationOptions>) -> Result<Vec<CredentialInfo>> {
            Ok(vec![CredentialInfo {
                provider_id: "p1".to_owned(),
                credential_type: AuthType::OAuth,
            }])
        }
        async fn modify(
            &self,
            _: &str,
            _: ModifyCredentialFn,
            _: Option<AuthOperationOptions>,
        ) -> Result<Option<Credential>> {
            Err(Error::msg("disk on fire"))
        }
        async fn delete(&self, _: &str, _: Option<AuthOperationOptions>) -> Result<()> {
            Ok(())
        }
    }

    let oauth_models = create_models(Some(CreateModelsOptions {
        credentials: Some(Arc::new(ModifyFailingStore)),
        ..Default::default()
    }));
    oauth_models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(TestOAuth::new()),
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));
    let err2 = oauth_models.get_auth_provider_id("p1", None).await.unwrap_err();
    assert_eq!(err2.downcast_ref::<ModelsError>().unwrap().code, ModelsErrorCode::Auth);
}

#[tokio::test]
async fn keeps_the_underlying_reason_in_wrapped_oauth_refresh_errors() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    credentials
        .modify(
            "p1",
            Box::new(|_| Box::pin(async { Ok(Some(Credential::OAuth(OAuthCredential::new("r", "old", 0)))) })),
            None,
        )
        .await
        .unwrap();

    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials),
        ..Default::default()
    }));
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(TestOAuth::with_refresh(Arc::new(|_, _| {
                Box::pin(async { Err(Error::msg("token refresh failed (400): invalid_grant")) })
            }))),
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));

    let err = models.get_auth_provider_id("p1", None).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("OAuth refresh failed for p1: token refresh failed (400): invalid_grant"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn wraps_api_key_auth_failures_in_models_error() {
    struct FailingAuth;
    #[async_trait::async_trait]
    impl ApiKeyAuth for FailingAuth {
        fn name(&self) -> &str {
            "Failing"
        }
        async fn resolve(&self, _: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
            Err(Error::msg("nope"))
        }
    }

    let models = create_models(None);
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: Some(Arc::new(FailingAuth)),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: None,
        get_all_models_fn: None,
    }));
    let err = models.get_auth_provider_id("p1", None).await.unwrap_err();
    assert_eq!(err.downcast_ref::<ModelsError>().unwrap().code, ModelsErrorCode::Auth);
}

#[tokio::test]
async fn uses_explicit_request_api_key_and_env_during_provider_auth_resolution() {
    struct ScopedAuth;
    #[async_trait::async_trait]
    impl ApiKeyAuth for ScopedAuth {
        fn name(&self) -> &str {
            "Scoped"
        }
        async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
            let account = input
                .credential
                .as_ref()
                .and_then(|c| c.env.as_ref())
                .and_then(|e| e.get("ACCOUNT_ID").cloned())
                .or(input.ctx.env("ACCOUNT_ID").await);
            let key = input.credential.and_then(|c| c.key);
            let (Some(k), Some(acc)) = (key, account) else {
                return Ok(None);
            };
            let mut env: ProviderEnv = IndexMap::new();
            env.insert("ACCOUNT_ID".to_owned(), acc.clone());
            Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some(k),
                    headers: None,
                    base_url: Some(format!("https://example.test/{acc}")),
                },
                env: Some(env),
                source: None,
            }))
        }
    }

    let calls = Arc::new(Mutex::new(Vec::new()));
    let models = create_models(None);
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: Some(Arc::new(ScopedAuth)),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: Some(calls.clone()),
        get_all_models_fn: None,
    }));
    let model = test_model("p1", "model-a");

    let mut req_env = IndexMap::new();
    req_env.insert("ACCOUNT_ID".to_owned(), "acct".to_owned());
    models
        .complete_simple(
            &model,
            &sample_context(),
            Some(ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        api_key: Some("explicit-key".to_owned()),
                        env: Some(req_env.clone()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .await;

    let guard = calls.lock().unwrap();
    assert_eq!(guard[0].model.base_url, "https://example.test/acct");
    assert_eq!(
        guard[0].options.as_ref().and_then(|o| o.api_key.as_deref()),
        Some("explicit-key")
    );
    assert_eq!(guard[0].options.as_ref().and_then(|o| o.env.as_ref()), Some(&req_env));
}

#[tokio::test]
async fn merges_resolved_auth_into_stream_options_explicit_options_win_per_field() {
    struct TestAuth;
    #[async_trait::async_trait]
    impl ApiKeyAuth for TestAuth {
        fn name(&self) -> &str {
            "Test"
        }
        async fn resolve(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
            let mut headers = IndexMap::new();
            headers.insert("Authorization".to_owned(), Some("Bearer resolved-key".to_owned()));
            headers.insert("x-a".to_owned(), Some("auth".to_owned()));
            headers.insert("x-b".to_owned(), Some("auth".to_owned()));
            Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some("resolved-key".to_owned()),
                    headers: Some(headers),
                    base_url: Some("https://auth.test/v1".to_owned()),
                },
                env: None,
                source: None,
            }))
        }
    }

    let calls = Arc::new(Mutex::new(Vec::new()));
    let models = create_models(None);
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: Some(Arc::new(TestAuth)),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: Some(calls.clone()),
        get_all_models_fn: None,
    }));
    let model = test_model("p1", "model-a");

    let mut exp_headers = IndexMap::new();
    exp_headers.insert("authorization".to_owned(), Some("Explicit token".to_owned()));
    exp_headers.insert("x-b".to_owned(), Some("explicit".to_owned()));
    let result = models
        .complete_simple(
            &model,
            &sample_context(),
            Some(ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        api_key: Some("explicit-key".to_owned()),
                        headers: Some(exp_headers),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .await;
    assert_eq!(result.stop_reason, StopReason::Stop);

    let result2 = models.complete_simple(&model, &sample_context(), None).await;
    assert_eq!(result2.stop_reason, StopReason::Stop);

    let guard = calls.lock().unwrap();
    assert_eq!(guard.len(), 2);
    assert_eq!(
        guard[0].options.as_ref().and_then(|o| o.api_key.as_deref()),
        Some("explicit-key")
    );
    let mut expected_headers = IndexMap::new();
    expected_headers.insert("x-a".to_owned(), Some("auth".to_owned()));
    expected_headers.insert("authorization".to_owned(), Some("Explicit token".to_owned()));
    expected_headers.insert("x-b".to_owned(), Some("explicit".to_owned()));
    assert_eq!(
        guard[0].options.as_ref().and_then(|o| o.headers.as_ref()),
        Some(&expected_headers)
    );
    assert_eq!(guard[0].model.base_url, "https://auth.test/v1");
    assert_eq!(
        guard[1].options.as_ref().and_then(|o| o.api_key.as_deref()),
        Some("resolved-key")
    );
}

#[tokio::test]
async fn adds_model_headers_only_for_model_auth_and_transforms_assembled_headers_once() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let models = create_models(None);
    models.set_provider(Arc::new(TestProvider {
        id: "p1".to_owned(),
        models: vec![test_model("p1", "model-a")],
        auth: ProviderAuth {
            api_key: Some(EnvKeyAuth::new(Some("key"))),
            oauth: None,
        },
        get_models_fn: None,
        refresh_fn: None,
        calls: Some(calls.clone()),
        get_all_models_fn: None,
    }));
    let mut model = test_model("p1", "model-a");
    let mut mh = IndexMap::new();
    mh.insert("x-model".to_owned(), "model".to_owned());
    mh.insert("x-shared".to_owned(), "model".to_owned());
    model.headers = Some(mh);

    assert!(
        models
            .get_auth_provider_id("p1", None)
            .await
            .unwrap()
            .unwrap()
            .auth
            .headers
            .is_none()
    );
    let mut expected_model_headers: ProviderHeaders = IndexMap::new();
    expected_model_headers.insert("x-model".to_owned(), Some("model".to_owned()));
    expected_model_headers.insert("x-shared".to_owned(), Some("model".to_owned()));
    assert_eq!(
        models
            .get_auth_model(&AnyModel::Chat(model.clone()), None)
            .await
            .unwrap()
            .unwrap()
            .auth
            .headers,
        Some(expected_model_headers)
    );

    let transforms = Arc::new(AtomicUsize::new(0));
    let t_c = transforms.clone();
    let mut explicit_headers = IndexMap::new();
    explicit_headers.insert("x-explicit".to_owned(), Some("explicit".to_owned()));
    explicit_headers.insert("X-Shared".to_owned(), Some("explicit".to_owned()));
    models
        .complete_simple(
            &model,
            &sample_context(),
            Some(ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        headers: Some(explicit_headers),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: Some(Arc::new(move |mut headers| {
                    let tc = t_c.clone();
                    Box::pin(async move {
                        tc.fetch_add(1, Ordering::SeqCst);
                        let mut expected_before = IndexMap::new();
                        expected_before.insert("x-model".to_owned(), Some("model".to_owned()));
                        expected_before.insert("x-explicit".to_owned(), Some("explicit".to_owned()));
                        expected_before.insert("X-Shared".to_owned(), Some("explicit".to_owned()));
                        assert_eq!(headers, expected_before);
                        headers.insert("x-transformed".to_owned(), Some("yes".to_owned()));
                        Ok(headers)
                    })
                })),
            }),
        )
        .await;

    assert_eq!(transforms.load(Ordering::SeqCst), 1);
    let mut expected_after = IndexMap::new();
    expected_after.insert("x-model".to_owned(), Some("model".to_owned()));
    expected_after.insert("x-explicit".to_owned(), Some("explicit".to_owned()));
    expected_after.insert("X-Shared".to_owned(), Some("explicit".to_owned()));
    expected_after.insert("x-transformed".to_owned(), Some("yes".to_owned()));
    assert_eq!(
        calls.lock().unwrap()[0]
            .options
            .as_ref()
            .and_then(|o| o.headers.as_ref()),
        Some(&expected_after)
    );
}

#[tokio::test]
async fn produces_an_error_stream_for_unknown_providers_instead_of_throwing() {
    let models = create_models(None);
    let result = models
        .complete_simple(&test_model("ghost", "model-a"), &sample_context(), None)
        .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("Unknown provider: ghost")
    );
}

#[tokio::test]
async fn streams_through_the_provider() {
    let models = create_models(None);
    models.set_provider(default_provider("p1"));
    let model = test_model("p1", "model-a");

    let mut events = Vec::new();
    let mut s = models.stream_simple(&model, &sample_context(), None);
    while let Some(event) = s.next().await {
        events.push(match event {
            pi_ai::types::AssistantMessageEvent::Start { .. } => "start",
            pi_ai::types::AssistantMessageEvent::Done { .. } => "done",
            _ => "other",
        });
    }
    assert_eq!(events, vec!["start", "done"]);
    let message = s.result().await;
    assert_eq!(message.stop_reason, StopReason::Stop);
}
