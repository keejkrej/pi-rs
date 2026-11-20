//! Port of packages/coding-agent/test/model-runtime-modify-models-compat.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthCheck, AuthInteraction, AuthPrompt, AuthResult, AuthType,
    Credential, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuth, ProviderAuthInteraction,
};
use pi_ai::compat::extension_oauth_types::OAuthCredentials;
use pi_ai::models::{
    CreateProviderApi, CreateProviderOptions, ModelsDeferredCancelOptions, ModelsDeferredFetchOptions,
    ModelsRefreshOptions, RefreshModelsContext,
};
use pi_ai::models_store::{InMemoryModelsStore, ModelsStore};
use pi_ai::types::{
    AnyModel, AssistantMessage, DeferredCancelOptions, DeferredFetchOptions, DeferredHandle, Model, ModelCost,
    ModelInput, ProviderRequestOptions, ProviderStreams, StopReason, Usage,
};
use pi_ai::utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use pi_coding_agent::core::auth_storage::{AuthStorage, AuthStorageData};
use pi_coding_agent::core::model_registry::ModelRegistry;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_coding_agent::core::provider_composer::{
    ExtensionOAuthConfig, ProviderChatModelConfig, ProviderConfigInput, ProviderModelConfig,
};
use pi_js::abort::AbortSignal;
use pi_js::fs::write_file;
use pi_js::json::stringify;
use serde_json::json;
use tempfile::TempDir;

fn test_model(provider: &str, id: &str, base_url: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: id.to_owned(),
        api: "openai-completions".to_owned(),
        provider: provider.to_owned(),
        base_url: base_url.to_owned(),
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

fn chat_cfg(provider: &str, id: &str, base_url: &str) -> ProviderModelConfig {
    let m = test_model(provider, id, base_url);
    ProviderModelConfig::Chat(ProviderChatModelConfig {
        id: m.id,
        name: m.name,
        api: Some(m.api),
        base_url: Some(m.base_url),
        input: m.input,
        input_limits: None,
        cost: m.cost,
        headers: None,
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: m.context_window,
        max_tokens: m.max_tokens,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    })
}

struct InteractiveSecretApiKeyAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for InteractiveSecretApiKeyAuth {
    fn name(&self) -> &str {
        "Native setup"
    }

    fn supports_login(&self) -> bool {
        true
    }

    async fn login(&self, interaction: ProviderAuthInteraction) -> pi_js::Result<ApiKeyCredential> {
        let key = interaction.prompt(AuthPrompt::secret("API key", None)).await?;
        Ok(ApiKeyCredential {
            key: Some(key),
            env: None,
        })
    }

    fn supports_check(&self) -> bool {
        true
    }

    async fn check(&self, input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthCheck>> {
        Ok(input
            .credential
            .and_then(|c| c.key)
            .filter(|k| !k.is_empty())
            .map(|_| AuthCheck {
                source: Some("stored native key".to_owned()),
                auth_type: AuthType::ApiKey,
            }))
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
        Ok(input
            .credential
            .and_then(|c| c.key)
            .filter(|k| !k.is_empty())
            .map(|k| AuthResult {
                auth: ModelAuth {
                    api_key: Some(k),
                    headers: None,
                    base_url: Some("https://resolved.test/v1".to_owned()),
                },
                env: None,
                source: Some("stored native key".to_owned()),
            }))
    }
}

struct SecretPromptInteraction;

#[async_trait::async_trait]
impl AuthInteraction for SecretPromptInteraction {
    async fn prompt(&self, _prompt: AuthPrompt) -> pi_js::Result<String> {
        Ok("secret".to_owned())
    }

    fn notify(&self, _event: pi_ai::auth::types::AuthEvent) {}
}

struct StaticNativeOAuth;

#[async_trait::async_trait]
impl OAuthAuth for StaticNativeOAuth {
    fn name(&self) -> &str {
        "Native OAuth"
    }

    async fn login(
        &self,
        _interaction: ProviderAuthInteraction,
        _options: Option<pi_ai::auth::types::LoginOptions>,
    ) -> pi_js::Result<OAuthCredential> {
        unreachable!("login not used")
    }

    async fn refresh(&self, credential: OAuthCredential, _signal: AbortSignal) -> pi_js::Result<OAuthCredential> {
        Ok(credential)
    }

    async fn to_auth(&self, credential: OAuthCredential) -> pi_js::Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access),
            headers: None,
            base_url: None,
        })
    }
}

struct LiteralKeyAuth {
    key: String,
}

#[async_trait::async_trait]
impl ApiKeyAuth for LiteralKeyAuth {
    fn name(&self) -> &str {
        "Native key"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(self.key.clone()),
                headers: None,
                base_url: None,
            },
            env: None,
            source: Some("native".to_owned()),
        }))
    }
}

fn unused_streams() -> CreateProviderApi {
    CreateProviderApi::Single(ProviderStreams {
        stream: Arc::new(|_, _, _| panic!("unused")),
        stream_simple: Arc::new(|_, _, _| panic!("unused")),
        fetch_deferred: None,
        cancel_deferred: None,
    })
}

#[tokio::test]
async fn registers_native_pi_ai_providers_with_their_auth_implementation() {
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        models_path: Some(None),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    let native_model = test_model("extension-native", "native", "https://fallback.test/v1");
    let provider = pi_ai::models::create_provider(CreateProviderOptions {
        id: "extension-native".to_owned(),
        name: Some("Extension Native".to_owned()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(InteractiveSecretApiKeyAuth)),
            oauth: None,
        },
        models: vec![AnyModel::Chat(native_model)],
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(unused_streams()),
        images: None,
        classifiers: None,
    })
    .unwrap();

    runtime.register_native_provider(provider.clone()).unwrap();
    let registry = ModelRegistry::new(runtime.clone());
    assert!(Arc::ptr_eq(
        &registry.get_provider("extension-native").unwrap(),
        &provider
    ));
    assert!(Arc::ptr_eq(
        &registry.get_registered_native_provider("extension-native").unwrap(),
        &provider
    ));
    assert!(
        registry
            .get_registered_provider_ids()
            .contains(&"extension-native".to_owned())
    );
    assert!(registry.find("extension-native", "native").is_some());

    runtime
        .login(
            "extension-native",
            AuthType::ApiKey,
            Arc::new(SecretPromptInteraction),
            None,
        )
        .await
        .unwrap();
    let auth = registry.get_provider_auth("extension-native").await.unwrap().unwrap();
    assert_eq!(auth.auth.api_key.as_deref(), Some("secret"));
    assert_eq!(auth.auth.base_url.as_deref(), Some("https://resolved.test/v1"));

    registry.unregister_provider("extension-native");
    assert!(registry.get_provider("extension-native").is_none());
}

#[tokio::test]
async fn marks_native_provider_with_stored_credential_as_configured_when_it_registers() {
    let mut stored: AuthStorageData = IndexMap::new();
    stored.insert(
        "extension-native".to_owned(),
        Credential::OAuth(OAuthCredential::new(
            "refresh",
            "access",
            pi_js::time::now_ms() + 3_600_000,
        )),
    );
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(Some(stored)))),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        models_path: Some(None),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    let native_model = test_model("extension-native", "native", "https://example.test/v1");
    let provider = pi_ai::models::create_provider(CreateProviderOptions {
        id: "extension-native".to_owned(),
        name: Some("Extension Native".to_owned()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(Arc::new(StaticNativeOAuth)),
        },
        models: vec![AnyModel::Chat(native_model)],
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(unused_streams()),
        images: None,
        classifiers: None,
    })
    .unwrap();

    runtime.register_native_provider(provider).unwrap();
    assert!(runtime.has_configured_auth("extension-native"));
    assert!(runtime.is_using_oauth("extension-native"));
    assert!(
        runtime
            .get_available_snapshot()
            .iter()
            .any(|m| m.provider == "extension-native" && m.id == "native")
    );

    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(runtime.has_configured_auth("extension-native"));
}

#[tokio::test]
async fn preserves_native_deferred_methods_through_provider_overlays() {
    let dir = TempDir::new().unwrap();
    let models_path = dir.path().join("models.json").to_str().unwrap().to_owned();
    write_file(
        &models_path,
        &stringify(&json!({
            "providers": {
                "extension-native-deferred": { "baseUrl": "https://overlay.test/v1" }
            }
        })),
        None,
    )
    .unwrap();

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        models_path: Some(Some(models_path)),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    let native_model = test_model("extension-native-deferred", "native-deferred", "https://native.test/v1");
    let fetched_base_url = Arc::new(Mutex::new(None::<String>));
    let fetched_opts = Arc::new(Mutex::new(
        None::<(Option<String>, Option<u64>, Option<IndexMap<String, Option<String>>>)>,
    ));
    let cancelled_id = Arc::new(Mutex::new(None::<String>));
    let cancelled_opts = Arc::new(Mutex::new(
        None::<(Option<String>, Option<u64>, Option<IndexMap<String, Option<String>>>)>,
    ));

    let fb = fetched_base_url.clone();
    let fo = fetched_opts.clone();
    let ci = cancelled_id.clone();
    let co = cancelled_opts.clone();

    let provider = pi_ai::models::create_provider(CreateProviderOptions {
        id: "extension-native-deferred".to_owned(),
        name: Some("Extension Native Deferred".to_owned()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(LiteralKeyAuth { key: "key".to_owned() })),
            oauth: None,
        },
        models: vec![AnyModel::Chat(native_model.clone())],
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(ProviderStreams {
            stream: Arc::new(|_, _, _| panic!("unused")),
            stream_simple: Arc::new(|_, _, _| panic!("unused")),
            fetch_deferred: Some(Arc::new(
                move |req_model: &Model,
                      _handle: &DeferredHandle,
                      opts: Option<DeferredFetchOptions>|
                      -> AssistantMessageEventStream {
                    *fb.lock().unwrap() = Some(req_model.base_url.clone());
                    *fo.lock().unwrap() = opts.map(|o| (o.base.api_key, o.wait, o.base.headers));
                    let stream = create_assistant_message_event_stream();
                    let msg = AssistantMessage {
                        content: Vec::new(),
                        api: req_model.api.clone(),
                        provider: req_model.provider.clone(),
                        model: req_model.id.clone(),
                        response_id: None,
                        response_model: None,
                        provider_thinking_level: None,
                        thinking_level: None,
                        diagnostics: None,
                        usage: Usage::default(),
                        stop_reason: StopReason::Stop,
                        deferred: None,
                        error_message: None,
                        raw_stop_reason: None,
                        end_turn: None,
                        timestamp: 0,
                        duration_ms: None,
                    };
                    stream.push(pi_ai::types::AssistantMessageEvent::Start { partial: msg.clone() });
                    stream.push(pi_ai::types::AssistantMessageEvent::Done {
                        reason: StopReason::Stop,
                        message: msg,
                    });
                    stream
                },
            )),
            cancel_deferred: Some(Arc::new(
                move |_req_model: &Model, handle: &DeferredHandle, opts: Option<DeferredCancelOptions>| {
                    *ci.lock().unwrap() = Some(handle.id.clone());
                    *co.lock().unwrap() = opts.map(|o| (o.api_key, o.timeout_ms, o.headers));
                    Box::pin(async { Ok(()) })
                },
            )),
        })),
        images: None,
        classifiers: None,
    })
    .unwrap();

    runtime.register_native_provider(provider).unwrap();
    let composed = runtime
        .get_model("extension-native-deferred", &native_model.id)
        .expect("composed native model");

    let mut fetch_headers = IndexMap::new();
    fetch_headers.insert("X-Fetch".to_owned(), Some("fetch".to_owned()));
    runtime
        .fetch_deferred(
            &composed,
            &DeferredHandle {
                provider: composed.provider.clone(),
                model_id: composed.id.clone(),
                api: composed.api.clone(),
                id: "fetch-id".to_owned(),
                expires_at: None,
                poll_after_ms: None,
                data: None,
            },
            Some(ModelsDeferredFetchOptions {
                base: DeferredFetchOptions {
                    base: ProviderRequestOptions {
                        headers: Some(fetch_headers),
                        ..Default::default()
                    },
                    wait: Some(25),
                },
                transform_headers: Some(Arc::new(|mut h| {
                    Box::pin(async move {
                        h.insert("X-Transformed".to_owned(), Some("fetch".to_owned()));
                        Ok(h)
                    })
                })),
            }),
        )
        .await;

    runtime
        .cancel_deferred(
            &composed,
            &DeferredHandle {
                provider: composed.provider.clone(),
                model_id: composed.id.clone(),
                api: composed.api.clone(),
                id: "cancel-id".to_owned(),
                expires_at: None,
                poll_after_ms: None,
                data: None,
            },
            Some(ModelsDeferredCancelOptions {
                base: ProviderRequestOptions {
                    timeout_ms: Some(100),
                    ..Default::default()
                },
                transform_headers: Some(Arc::new(|mut h| {
                    Box::pin(async move {
                        h.insert("X-Transformed".to_owned(), Some("cancel".to_owned()));
                        Ok(h)
                    })
                })),
            }),
        )
        .await
        .unwrap();

    assert_eq!(
        fetched_base_url.lock().unwrap().as_deref(),
        Some("https://overlay.test/v1")
    );
    let (fk, fw, fh) = fetched_opts.lock().unwrap().clone().unwrap();
    assert_eq!(fk.as_deref(), Some("key"));
    assert_eq!(fw, Some(25));
    let fhm = fh.unwrap();
    assert_eq!(fhm.get("X-Fetch").and_then(|v| v.as_deref()), Some("fetch"));
    assert_eq!(fhm.get("X-Transformed").and_then(|v| v.as_deref()), Some("fetch"));

    assert_eq!(cancelled_id.lock().unwrap().as_deref(), Some("cancel-id"));
    let (ck, ct, ch) = cancelled_opts.lock().unwrap().clone().unwrap();
    assert_eq!(ck.as_deref(), Some("key"));
    assert_eq!(ct, Some(100));
    assert_eq!(
        ch.unwrap().get("X-Transformed").and_then(|v| v.as_deref()),
        Some("cancel")
    );
}

#[tokio::test]
async fn applies_models_json_overrides_above_native_providers() {
    let dir = TempDir::new().unwrap();
    let models_path = dir.path().join("models.json").to_str().unwrap().to_owned();
    write_file(
        &models_path,
        &stringify(&json!({
            "providers": {
                "extension-native": {
                    "modelOverrides": {
                        "native": { "contextWindow": 4242 }
                    }
                }
            }
        })),
        None,
    )
    .unwrap();

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        models_path: Some(Some(models_path)),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    let provider = pi_ai::models::create_provider(CreateProviderOptions {
        id: "extension-native".to_owned(),
        name: Some("Extension Native".to_owned()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(LiteralKeyAuth { key: "key".to_owned() })),
            oauth: None,
        },
        models: vec![AnyModel::Chat(test_model(
            "extension-native",
            "native",
            "https://native.test/v1",
        ))],
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(unused_streams()),
        images: None,
        classifiers: None,
    })
    .unwrap();
    runtime.register_native_provider(provider).unwrap();
    assert_eq!(
        runtime
            .get_model("extension-native", "native")
            .map(|m| m.context_window),
        Some(4242)
    );
}

#[tokio::test]
async fn publishes_refresh_models_results_without_forcing_models_store_persistence() {
    let store = Arc::new(InMemoryModelsStore::new());
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_store: Some(store.clone()),
        models_path: Some(None),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    runtime
        .register_provider(
            "extension-dynamic",
            ProviderConfigInput {
                base_url: Some("http://localhost:8080/v1".to_owned()),
                api_key: Some("local".to_owned()),
                api: Some("openai-completions".to_owned()),
                refresh_models: Some(Arc::new(|_ctx: RefreshModelsContext| {
                    Box::pin(async { Ok(vec![chat_cfg("extension-dynamic", "live", "http://localhost:8080/v1")]) })
                })),
                ..Default::default()
            },
        )
        .unwrap();

    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(runtime.get_model("extension-dynamic", "live").is_some());
    assert!(store.read("extension-dynamic", None).await.unwrap().is_none());
}

#[tokio::test]
async fn applies_legacy_oauth_modify_models_after_async_credential_initialization() {
    let mut stored: AuthStorageData = IndexMap::new();
    stored.insert(
        "extension-oauth".to_owned(),
        Credential::OAuth(OAuthCredential::new(
            "refresh",
            "access",
            pi_js::time::now_ms() + 60_000,
        )),
    );
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(Some(stored)))),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        models_path: Some(None),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    runtime
        .register_provider(
            "extension-oauth",
            ProviderConfigInput {
                base_url: Some("https://example.test/v1".to_owned()),
                api: Some("openai-completions".to_owned()),
                models: Some(vec![chat_cfg("extension-oauth", "base", "https://example.test/v1")]),
                oauth: Some(ExtensionOAuthConfig {
                    name: "Extension OAuth".to_owned(),
                    is_subscription: None,
                    uses_callback_server: None,
                    login: Arc::new(|_| Box::pin(async { Err(pi_js::Error::msg("not used")) })),
                    refresh_token: Arc::new(|cred, _| Box::pin(async move { Ok(cred) })),
                    get_api_key: Arc::new(|cred: &OAuthCredentials| cred.access.clone()),
                    modify_models: Some(Arc::new(|mut models, cred: &OAuthCredentials| {
                        if cred.access == "access" {
                            models.push(test_model(
                                "extension-oauth",
                                "credential-model",
                                "https://example.test/v1",
                            ));
                        }
                        models
                    })),
                }),
                ..Default::default()
            },
        )
        .unwrap();

    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(runtime.get_model("extension-oauth", "base").is_some());
    assert!(runtime.get_model("extension-oauth", "credential-model").is_some());

    runtime.logout("extension-oauth", None).await.unwrap();
    assert!(runtime.get_model("extension-oauth", "credential-model").is_none());
}
