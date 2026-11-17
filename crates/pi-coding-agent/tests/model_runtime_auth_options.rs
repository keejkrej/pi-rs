//! Port of packages/coding-agent/test/model-runtime-auth-options.test.ts

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::{IndexMap, IndexSet};
use pi_ai::auth::credential_store::InMemoryCredentialStore;
use pi_ai::auth::types::{
    ApiKeyAuth, ApiKeyCredential, AuthOperationOptions, AuthType, Credential, CredentialInfo, CredentialStore,
    ModifyCredentialFn, OAuthAuth, ProviderAuth,
};
use pi_ai::compat::extension_oauth_types::OAuthCredentials;
use pi_ai::models::{ModelsSimpleStreamOptions, Provider};
use pi_ai::types::{Context, ModelCost, ModelInput, ProviderHeaders, SimpleStreamOptions};
use pi_ai::{OAuthCredential, Result};
use pi_coding_agent::core::auth_storage::{AuthStorage, AuthStorageData};
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelRuntimeAuthOverrides};
use pi_coding_agent::core::provider_composer::{
    ExtensionOAuthConfig, ProviderChatModelConfig, ProviderConfigInput, ProviderModelConfig,
};
use pi_js::Error;
use pi_js::abort::{AbortController, AbortReason, AbortSignal};

enum AuthOptionMethod {
    OAuth(Arc<dyn OAuthAuth>),
    ApiKey(Arc<dyn ApiKeyAuth>),
}

struct ProjectedAuthOption {
    auth_type: AuthType,
    provider: Arc<dyn Provider + Send + Sync>,
    method: AuthOptionMethod,
}

fn auth_options(runtime: &ModelRuntime, filter_type: Option<AuthType>) -> Vec<ProjectedAuthOption> {
    let mut out = Vec::new();
    for provider in runtime.get_providers() {
        let auth: &ProviderAuth = provider.auth();
        if filter_type.is_none_or(|t| t == AuthType::OAuth)
            && let Some(oauth) = auth.oauth.as_ref()
        {
            out.push(ProjectedAuthOption {
                auth_type: AuthType::OAuth,
                provider: provider.clone(),
                method: AuthOptionMethod::OAuth(oauth.clone()),
            });
        }
        if filter_type.is_none_or(|t| t == AuthType::ApiKey)
            && let Some(api_key) = auth.api_key.as_ref()
        {
            out.push(ProjectedAuthOption {
                auth_type: AuthType::ApiKey,
                provider: provider.clone(),
                method: AuthOptionMethod::ApiKey(api_key.clone()),
            });
        }
    }
    out
}

fn test_chat_model(id: &str) -> ProviderModelConfig {
    ProviderModelConfig::Chat(ProviderChatModelConfig {
        id: id.to_owned(),
        name: id.to_owned(),
        api: Some("openai-completions".to_owned()),
        base_url: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        headers: None,
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 10_000,
        max_tokens: 1000,
        sampling_params: None,
        compat: None,
    })
}

struct SpyCredentialStore {
    base: InMemoryCredentialStore,
    reads: Mutex<Vec<String>>,
    fail_reads: Mutex<bool>,
}

#[async_trait::async_trait]
impl CredentialStore for SpyCredentialStore {
    async fn read(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<Option<Credential>> {
        self.reads.lock().unwrap().push(provider_id.to_owned());
        if *self.fail_reads.lock().unwrap() {
            return Err(Error::msg(format!("read failed for {provider_id}")));
        }
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
        self.base.delete(provider_id, options).await
    }
}

#[tokio::test]
async fn accepts_pi_ai_credential_store() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    credentials
        .modify_with(
            "anthropic",
            |_| async {
                Ok(Some(Credential::ApiKey(ApiKeyCredential {
                    key: Some("stored-key".to_owned()),
                    env: None,
                })))
            },
            None,
        )
        .await
        .unwrap();

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(credentials),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();

    let auth = runtime.get_auth_provider_id("anthropic", None).await.unwrap().unwrap();
    assert_eq!(auth.auth.api_key.as_deref(), Some("stored-key"));
}

#[tokio::test]
async fn scopes_provider_availability_reads_and_records_refresh_failures() {
    let credentials = Arc::new(SpyCredentialStore {
        base: InMemoryCredentialStore::new(),
        reads: Mutex::new(Vec::new()),
        fail_reads: Mutex::new(false),
    });
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(credentials.clone()),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();

    credentials.reads.lock().unwrap().clear();
    runtime.get_available(Some("anthropic"), None).await.unwrap();
    let seen: IndexSet<String> = credentials.reads.lock().unwrap().drain(..).collect();
    assert_eq!(seen.into_iter().collect::<Vec<_>>(), vec!["anthropic"]);

    *credentials.fail_reads.lock().unwrap() = true;
    let err = runtime.get_available(Some("anthropic"), None).await.unwrap_err();
    assert!(err.to_string().contains("Credential store read failed for anthropic"));
    assert!(
        runtime
            .get_error()
            .as_deref()
            .is_some_and(|e| e.contains("Availability refresh: Credential store read failed for anthropic"))
    );

    *credentials.fail_reads.lock().unwrap() = false;
    runtime.get_available(None, None).await.unwrap();
    assert!(runtime.get_error().is_none());
}

#[tokio::test]
async fn projects_provider_owned_methods_names_and_status() {
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();

    let all = auth_options(&runtime, None);
    let has_entry = |pid: &str, pname: &str, t: AuthType, mname: Option<&str>| {
        all.iter().any(|opt| {
            opt.provider.id() == pid
                && opt.provider.name() == pname
                && opt.auth_type == t
                && match (&opt.method, mname) {
                    (AuthOptionMethod::ApiKey(a), Some(n)) => a.name() == n,
                    (_, None) => true,
                    _ => false,
                }
        })
    };

    assert!(has_entry(
        "amazon-bedrock",
        "Amazon Bedrock",
        AuthType::ApiKey,
        Some("AWS credentials or bearer token")
    ));
    assert!(has_entry(
        "google-vertex",
        "Google Vertex AI",
        AuthType::ApiKey,
        Some("Google Cloud credentials")
    ));
    assert!(has_entry("anthropic", "Anthropic", AuthType::OAuth, None));
    assert!(has_entry(
        "cloudflare-ai-gateway",
        "Cloudflare AI Gateway",
        AuthType::ApiKey,
        None
    ));
    assert!(has_entry(
        "cloudflare-workers-ai",
        "Cloudflare Workers AI",
        AuthType::ApiKey,
        None
    ));

    assert!(
        auth_options(&runtime, Some(AuthType::ApiKey))
            .iter()
            .all(|o| o.auth_type == AuthType::ApiKey)
    );
    assert!(
        auth_options(&runtime, Some(AuthType::OAuth))
            .iter()
            .all(|o| o.auth_type == AuthType::OAuth)
    );
    assert!(
        !all.iter()
            .any(|o| o.provider.id() == "openai-codex" && o.auth_type == AuthType::ApiKey)
    );
}

#[tokio::test]
async fn attaches_providers_active_auth_status_to_every_method_option() {
    let mut stored: AuthStorageData = IndexMap::new();
    stored.insert(
        "anthropic".to_owned(),
        Credential::OAuth(OAuthCredential::new(
            "refresh",
            "access",
            pi_js::time::now_ms() + 60_000,
        )),
    );
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(Some(stored)))),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();

    let options: Vec<_> = auth_options(&runtime, None)
        .into_iter()
        .filter(|o| o.provider.id() == "anthropic")
        .collect();
    assert_eq!(options.len(), 2);
    let check = runtime.check_auth("anthropic", None).await.unwrap().unwrap();
    assert_eq!(check.auth_type, AuthType::OAuth);
}

#[tokio::test]
async fn distinguishes_subscription_oauth_from_generic_oauth_sign_in() {
    let mut stored: AuthStorageData = IndexMap::new();
    stored.insert(
        "anthropic".to_owned(),
        Credential::OAuth(OAuthCredential::new(
            "anthropic-refresh",
            "anthropic-access",
            pi_js::time::now_ms() + 60 * 60_000,
        )),
    );
    stored.insert(
        "openrouter".to_owned(),
        Credential::OAuth(OAuthCredential::new("", "openrouter-key", 9_007_199_254_740_991)),
    );
    stored.insert(
        "radius".to_owned(),
        Credential::OAuth(OAuthCredential::new(
            "radius-refresh",
            "radius-access",
            pi_js::time::now_ms() + 60 * 60_000,
        )),
    );

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(Some(stored)))),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();

    assert!(
        runtime.get_error().is_none(),
        "unexpected error: {:?}",
        runtime.get_error()
    );
    assert!(runtime.is_using_oauth("anthropic"));
    assert!(runtime.is_using_subscription("anthropic"));
    assert!(runtime.is_using_oauth("openrouter"));
    assert!(!runtime.is_using_subscription("openrouter"));
    assert!(runtime.is_using_oauth("radius"));
    assert!(!runtime.is_using_subscription("radius"));
}

#[tokio::test]
async fn constructs_an_api_key_method_for_an_extension_api_key_provider() {
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();

    runtime
        .register_provider(
            "extension-api-key",
            ProviderConfigInput {
                name: Some("Extension API Key".to_owned()),
                base_url: Some("https://example.test/v1".to_owned()),
                api_key: Some("$EXTENSION_TEST_API_KEY".to_owned()),
                api: Some("openai-completions".to_owned()),
                models: Some(vec![test_chat_model("extension-model")]),
                ..Default::default()
            },
        )
        .unwrap();

    let options: Vec<_> = auth_options(&runtime, None)
        .into_iter()
        .filter(|o| o.provider.id() == "extension-api-key")
        .collect();
    assert_eq!(options.len(), 1);
    assert_eq!(options[0].auth_type, AuthType::ApiKey);
    assert_eq!(options[0].provider.id(), "extension-api-key");
    assert_eq!(options[0].provider.name(), "Extension API Key");
    match &options[0].method {
        AuthOptionMethod::ApiKey(a) => {
            assert_eq!(a.name(), "API key");
            assert!(a.supports_login());
        }
        AuthOptionMethod::OAuth(_) => panic!("expected api_key auth option"),
    }
}

#[tokio::test]
async fn resolves_configured_auth_from_request_scoped_environment_overrides() {
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();

    let mut headers = IndexMap::new();
    headers.insert("x-request-value".to_owned(), "$REQUEST_SCOPED_HEADER".to_owned());
    runtime
        .register_provider(
            "request-env-provider",
            ProviderConfigInput {
                base_url: Some("https://example.test/v1".to_owned()),
                api_key: Some("$REQUEST_SCOPED_API_KEY".to_owned()),
                headers: Some(headers),
                api: Some("openai-completions".to_owned()),
                models: Some(vec![test_chat_model("request-env-model")]),
                ..Default::default()
            },
        )
        .unwrap();

    let mut env = IndexMap::new();
    env.insert("REQUEST_SCOPED_API_KEY".to_owned(), "request-key".to_owned());
    env.insert("REQUEST_SCOPED_HEADER".to_owned(), "request-header".to_owned());
    let auth = runtime
        .get_auth_provider_id(
            "request-env-provider",
            Some(ModelRuntimeAuthOverrides {
                env: Some(env),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(auth.auth.api_key.as_deref(), Some("request-key"));
    let mut expected_headers = IndexMap::new();
    expected_headers.insert("x-request-value".to_owned(), Some("request-header".to_owned()));
    assert_eq!(auth.auth.headers, Some(expected_headers));
}

#[tokio::test]
async fn lets_explicit_authorization_header_override_auth_header_case_insensitively() {
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();

    let captured_headers: Arc<Mutex<Option<ProviderHeaders>>> = Arc::new(Mutex::new(None));
    let cap_clone = captured_headers.clone();

    runtime
        .register_provider(
            "auth-header-provider",
            ProviderConfigInput {
                base_url: Some("https://example.test/v1".to_owned()),
                api_key: Some("generated-key".to_owned()),
                auth_header: Some(true),
                api: Some("openai-completions".to_owned()),
                stream_simple: Some(Arc::new(move |model, _, opts| {
                    *cap_clone.lock().unwrap() = opts.and_then(|o| o.base.headers);
                    pi_ai::api::lazy::lazy_stream(model, async { Err(Error::msg("captured")) })
                })),
                models: Some(vec![test_chat_model("auth-header-model")]),
                ..Default::default()
            },
        )
        .unwrap();

    let model = runtime.get_model("auth-header-provider", "auth-header-model").unwrap();
    let mut explicit_headers = IndexMap::new();
    explicit_headers.insert("authorization".to_owned(), Some("Explicit token".to_owned()));
    let _ = runtime
        .complete_simple(
            &model,
            &Context {
                system_prompt: None,
                messages: Vec::new(),
                tools: None,
            },
            Some(ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: pi_ai::types::StreamOptions {
                        headers: Some(explicit_headers.clone()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .await;

    assert_eq!(*captured_headers.lock().unwrap(), Some(explicit_headers));
}

#[tokio::test]
async fn transforms_fully_assembled_headers_once_without_forwarding_transform() {
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();

    let captured_headers: Arc<Mutex<Option<ProviderHeaders>>> = Arc::new(Mutex::new(None));
    let cap_clone = captured_headers.clone();
    let transforms = Arc::new(AtomicUsize::new(0));
    let transforms_clone = transforms.clone();

    let mut provider_headers = IndexMap::new();
    provider_headers.insert("x-provider".to_owned(), "provider".to_owned());
    let mut model_headers = IndexMap::new();
    model_headers.insert("x-model".to_owned(), "model".to_owned());

    runtime
        .register_provider(
            "header-provider",
            ProviderConfigInput {
                base_url: Some("https://example.test/v1".to_owned()),
                api_key: Some("generated-key".to_owned()),
                auth_header: Some(true),
                headers: Some(provider_headers),
                api: Some("openai-completions".to_owned()),
                stream_simple: Some(Arc::new(move |model, _, opts| {
                    *cap_clone.lock().unwrap() = opts.and_then(|o| o.base.headers);
                    pi_ai::api::lazy::lazy_stream(model, async { Err(Error::msg("captured")) })
                })),
                models: Some(vec![ProviderModelConfig::Chat(ProviderChatModelConfig {
                    id: "header-model".to_owned(),
                    name: "header-model".to_owned(),
                    api: Some("openai-completions".to_owned()),
                    base_url: None,
                    input: vec![ModelInput::Text],
                    input_limits: None,
                    cost: ModelCost {
                        input: 0.0,
                        output: 0.0,
                        cache_read: 0.0,
                        cache_write: 0.0,
                        tiers: None,
                    },
                    headers: Some(model_headers),
                    reasoning: false,
                    thinking_level_map: None,
                    prompt_cache: None,
                    context_window: 10_000,
                    max_tokens: 1000,
                    sampling_params: None,
                    compat: None,
                })]),
                ..Default::default()
            },
        )
        .unwrap();

    let model = runtime.get_model("header-provider", "header-model").unwrap();
    let mut explicit_headers = IndexMap::new();
    explicit_headers.insert("x-explicit".to_owned(), Some("explicit".to_owned()));

    let _ = runtime
        .complete_simple(
            &model,
            &Context {
                system_prompt: None,
                messages: Vec::new(),
                tools: None,
            },
            Some(ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: pi_ai::types::StreamOptions {
                        headers: Some(explicit_headers),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: Some(Arc::new(move |mut headers| {
                    let t = transforms_clone.clone();
                    Box::pin(async move {
                        t.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(
                            headers.get("Authorization").and_then(|v| v.as_deref()),
                            Some("Bearer generated-key")
                        );
                        assert_eq!(headers.get("x-provider").and_then(|v| v.as_deref()), Some("provider"));
                        assert_eq!(headers.get("x-model").and_then(|v| v.as_deref()), Some("model"));
                        assert_eq!(headers.get("x-explicit").and_then(|v| v.as_deref()), Some("explicit"));
                        headers.insert("x-transformed".to_owned(), Some("yes".to_owned()));
                        Ok(headers)
                    })
                })),
            }),
        )
        .await;

    assert_eq!(transforms.load(Ordering::SeqCst), 1);
    let captured = captured_headers.lock().unwrap().clone().unwrap();
    assert_eq!(
        captured.get("Authorization").and_then(|v| v.as_deref()),
        Some("Bearer generated-key")
    );
    assert_eq!(captured.get("x-provider").and_then(|v| v.as_deref()), Some("provider"));
    assert_eq!(captured.get("x-model").and_then(|v| v.as_deref()), Some("model"));
    assert_eq!(captured.get("x-explicit").and_then(|v| v.as_deref()), Some("explicit"));
    assert_eq!(captured.get("x-transformed").and_then(|v| v.as_deref()), Some("yes"));
}

#[tokio::test]
async fn forwards_cancellation_to_extension_oauth_refresh() {
    let mut stored: AuthStorageData = IndexMap::new();
    stored.insert(
        "extension-oauth".to_owned(),
        Credential::OAuth(OAuthCredential::new("refresh", "expired", 0)),
    );
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(Some(stored)))),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();

    let refresh_signal: Arc<Mutex<Option<AbortSignal>>> = Arc::new(Mutex::new(None));
    let sig_clone = refresh_signal.clone();

    runtime
        .register_provider(
            "extension-oauth",
            ProviderConfigInput {
                name: Some("Extension OAuth".to_owned()),
                base_url: Some("https://example.test/v1".to_owned()),
                api: Some("openai-completions".to_owned()),
                oauth: Some(ExtensionOAuthConfig {
                    name: "Extension subscription".to_owned(),
                    is_subscription: None,
                    uses_callback_server: None,
                    login: Arc::new(|_| {
                        Box::pin(async {
                            Ok(OAuthCredentials {
                                refresh: "refresh".to_owned(),
                                access: "access".to_owned(),
                                expires: pi_js::time::now_ms() + 60_000,
                                extra: IndexMap::new(),
                            })
                        })
                    }),
                    refresh_token: Arc::new(move |mut cred, sig| {
                        let slot = sig_clone.clone();
                        Box::pin(async move {
                            *slot.lock().unwrap() = Some(sig);
                            cred.expires = pi_js::time::now_ms() + 60_000;
                            Ok(cred)
                        })
                    }),
                    get_api_key: Arc::new(|cred| cred.access.clone()),
                    modify_models: None,
                }),
                models: Some(vec![test_chat_model("extension-model")]),
                ..Default::default()
            },
        )
        .unwrap();

    let ctrl = AbortController::new();
    runtime
        .get_auth_provider_id(
            "extension-oauth",
            Some(ModelRuntimeAuthOverrides {
                signal: Some(ctrl.signal()),
                ..Default::default()
            }),
        )
        .await
        .unwrap();

    let captured = refresh_signal.lock().unwrap().clone().expect("refresh signal");
    ctrl.abort(Some(AbortReason::error("Error", "cancelled")));
    assert!(captured.aborted());
    assert_eq!(captured.reason(), Some(AbortReason::error("Error", "cancelled")));
}

#[tokio::test]
async fn does_not_fabricate_api_key_method_for_extension_oauth_only_provider() {
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_path: Some(None),
        ..Default::default()
    })
    .await
    .unwrap();

    runtime
        .register_provider(
            "extension-oauth",
            ProviderConfigInput {
                name: Some("Extension OAuth".to_owned()),
                base_url: Some("https://example.test/v1".to_owned()),
                api: Some("openai-completions".to_owned()),
                oauth: Some(ExtensionOAuthConfig {
                    name: "Extension subscription".to_owned(),
                    is_subscription: Some(true),
                    uses_callback_server: None,
                    login: Arc::new(|_| {
                        Box::pin(async {
                            Ok(OAuthCredentials {
                                refresh: "refresh".to_owned(),
                                access: "access".to_owned(),
                                expires: pi_js::time::now_ms() + 60_000,
                                extra: IndexMap::new(),
                            })
                        })
                    }),
                    refresh_token: Arc::new(|cred, _| Box::pin(async move { Ok(cred) })),
                    get_api_key: Arc::new(|cred| cred.access.clone()),
                    modify_models: None,
                }),
                models: Some(vec![test_chat_model("extension-model")]),
                ..Default::default()
            },
        )
        .unwrap();

    let options: Vec<_> = auth_options(&runtime, None)
        .into_iter()
        .filter(|o| o.provider.id() == "extension-oauth")
        .collect();
    assert_eq!(options.len(), 1);
    assert_eq!(options[0].auth_type, AuthType::OAuth);
    assert_eq!(options[0].provider.id(), "extension-oauth");
    assert_eq!(options[0].provider.name(), "Extension OAuth");
    match &options[0].method {
        AuthOptionMethod::OAuth(o) => {
            assert_eq!(o.name(), "Extension subscription");
            assert!(o.is_subscription());
        }
        AuthOptionMethod::ApiKey(_) => panic!("expected oauth auth option"),
    }
}
