//! Port of packages/coding-agent/test/model-registry.test.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::auth::types::{ApiKeyCredential, AuthOperationOptions, Credential, OAuthCredential};
use pi_ai::compat::{get_api_provider, normalize_context};
use pi_ai::models_store::InMemoryModelsStore;
use pi_ai::providers::all::get_builtin_models;
use pi_ai::types::{
    CacheControlFormat, ChatTemplateKwargValue, ChatTemplateVarKind, Context, MaxTokensField, Model, ModelCompat,
    ModelCost, ModelInput, ModelThinkingLevel, OpenRouterRouting, ThinkingFormat,
};
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_registry::ModelRegistry;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_coding_agent::core::provider_composer::{
    AuthStatusSource, ExtensionOAuthConfig, ProviderChatModelConfig, ProviderConfigInput, ProviderModelConfig,
};
use pi_coding_agent::core::resolve_config_value::clear_config_value_cache;
use pi_js::console::testing::capture as capture_console;
use pi_js::env::testing::EnvGuard;
use pi_js::fs::{read_to_string, write_file as js_write_file};
use pi_js::json::stringify;
use pi_js::time::now_ms;
use serde_json::{Value, json};
use serial_test::serial;
use tempfile::TempDir;

async fn create_registry(auth: Arc<AuthStorage>, models_json_path: &str) -> ModelRegistry {
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(auth),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        models_path: Some(Some(models_json_path.to_owned())),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();
    ModelRegistry::new(runtime)
}

fn chat_model_cfg(id: &str, name: Option<&str>, api: Option<&str>) -> ProviderModelConfig {
    ProviderModelConfig::Chat(ProviderChatModelConfig {
        id: id.to_owned(),
        name: name.unwrap_or(id).to_owned(),
        api: api.map(str::to_owned),
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
        context_window: 100_000,
        max_tokens: 8000,
        sampling_params: None,
        compat: None,
    })
}

fn empty_provider_config() -> ProviderConfigInput {
    ProviderConfigInput {
        name: None,
        base_url: None,
        api_key: None,
        api: None,
        stream_simple: None,
        images: None,
        classifiers: None,
        headers: None,
        auth_header: None,
        oauth: None,
        models: None,
        refresh_models: None,
    }
}

fn provider_config(base_url: &str, model_ids: &[&str], api: &str) -> ProviderConfigInput {
    ProviderConfigInput {
        base_url: Some(base_url.to_owned()),
        api_key: Some("test-key".to_owned()),
        api: Some(api.to_owned()),
        models: Some(model_ids.iter().map(|id| chat_model_cfg(id, None, None)).collect()),
        ..empty_provider_config()
    }
}

fn get_models_for_provider(registry: &ModelRegistry, provider: &str) -> Vec<Model> {
    registry
        .get_all()
        .into_iter()
        .filter(|m| m.provider == provider)
        .collect()
}

fn write_raw_models_json(path: &str, providers: Value) {
    let doc = json!({ "providers": providers });
    js_write_file(path, stringify(&doc), None).unwrap();
}

fn to_sh_path(path: &str) -> String {
    path.replace('\\', "/").replace('"', "\\\"")
}

fn provider_with_api_key(api_key: &str) -> Value {
    json!({
        "baseUrl": "https://example.com/v1",
        "apiKey": api_key,
        "api": "anthropic-messages",
        "models": [
            {
                "id": "test-model",
                "name": "Test Model",
                "reasoning": false,
                "input": ["text"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": 100000,
                "maxTokens": 8000
            }
        ]
    })
}

#[tokio::test]
async fn base_url_override_keeps_all_built_in_models_and_updates_urls() {
    let dir = TempDir::new().unwrap();
    let models_path = dir.path().join("models.json").to_str().unwrap().to_owned();
    let auth = Arc::new(AuthStorage::in_memory(None));

    write_raw_models_json(
        &models_path,
        json!({ "anthropic": { "baseUrl": "https://my-proxy.example.com/v1" } }),
    );
    let registry = create_registry(auth, &models_path).await;
    let anthropic_models = get_models_for_provider(&registry, "anthropic");
    assert!(anthropic_models.len() > 1);
    assert!(anthropic_models.iter().any(|m| m.id.contains("claude")));
    for m in &anthropic_models {
        assert_eq!(m.base_url, "https://my-proxy.example.com/v1");
    }
}

#[tokio::test]
async fn overriding_headers_and_headers_only_resolve_at_request_time() {
    let dir = TempDir::new().unwrap();
    let models_path = dir.path().join("models.json").to_str().unwrap().to_owned();
    let auth = Arc::new(AuthStorage::in_memory(None));

    write_raw_models_json(
        &models_path,
        json!({
            "anthropic": {
                "baseUrl": "https://my-proxy.example.com/v1",
                "headers": { "X-Custom-Header": "custom-value" }
            }
        }),
    );
    let reg1 = create_registry(auth.clone(), &models_path).await;
    for model in get_models_for_provider(&reg1, "anthropic") {
        let req_auth = reg1.get_api_key_and_headers(&model).await;
        match req_auth {
            pi_coding_agent::core::model_registry::ResolvedRequestAuth::Ok { headers, .. } => {
                assert_eq!(
                    headers.unwrap().get("X-Custom-Header").cloned().flatten().as_deref(),
                    Some("custom-value")
                );
            }
            pi_coding_agent::core::model_registry::ResolvedRequestAuth::Err { error } => {
                panic!("unexpected auth error: {error}");
            }
        }
    }

    write_raw_models_json(
        &models_path,
        json!({ "anthropic": { "headers": { "X-Custom-Header": "custom-value" } } }),
    );
    let reg2 = create_registry(auth, &models_path).await;
    assert!(reg2.get_error().is_none());
    for model in get_models_for_provider(&reg2, "anthropic") {
        let req_auth = reg2.get_api_key_and_headers(&model).await;
        match req_auth {
            pi_coding_agent::core::model_registry::ResolvedRequestAuth::Ok { headers, .. } => {
                assert_eq!(
                    headers.unwrap().get("X-Custom-Header").cloned().flatten().as_deref(),
                    Some("custom-value")
                );
            }
            pi_coding_agent::core::model_registry::ResolvedRequestAuth::Err { error } => {
                panic!("unexpected auth error: {error}");
            }
        }
    }
}

#[tokio::test]
async fn unconfigured_compatibility_auth_includes_static_model_headers() {
    let dir = TempDir::new().unwrap();
    let models_path = dir.path().join("models.json").to_str().unwrap().to_owned();
    let registry = create_registry(Arc::new(AuthStorage::in_memory(None)), &models_path).await;
    let mut model = registry.get_all()[0].clone();
    model.provider = "missing-provider".to_owned();
    let mut hdrs = IndexMap::new();
    hdrs.insert("X-Static-Model".to_owned(), "static-value".to_owned());
    model.headers = Some(hdrs);

    let req_auth = registry.get_api_key_and_headers(&model).await;
    match req_auth {
        pi_coding_agent::core::model_registry::ResolvedRequestAuth::Ok { headers, .. } => {
            assert_eq!(
                headers.unwrap().get("X-Static-Model").cloned().flatten().as_deref(),
                Some("static-value")
            );
        }
        other => panic!("expected Ok auth, got {other:?}"),
    }
}

#[tokio::test]
async fn base_url_override_isolation_mix_and_refresh() {
    let dir = TempDir::new().unwrap();
    let models_path = dir.path().join("models.json").to_str().unwrap().to_owned();
    let auth = Arc::new(AuthStorage::in_memory(None));

    write_raw_models_json(
        &models_path,
        json!({
            "anthropic": { "baseUrl": "https://anthropic-proxy.example.com/v1" },
            "google": {
                "baseUrl": "https://google-proxy.example.com/v1",
                "apiKey": "test-key",
                "api": "google-generative-ai",
                "models": [{
                    "id": "gemini-custom",
                    "name": "gemini-custom",
                    "reasoning": false,
                    "input": ["text"],
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                    "contextWindow": 100000,
                    "maxTokens": 8000
                }]
            }
        }),
    );

    let registry = create_registry(auth, &models_path).await;
    let anthropic = get_models_for_provider(&registry, "anthropic");
    assert!(anthropic.len() > 1);
    assert_eq!(anthropic[0].base_url, "https://anthropic-proxy.example.com/v1");

    let google = get_models_for_provider(&registry, "google");
    assert!(google.len() > 1);
    assert!(google.iter().any(|m| m.id == "gemini-custom"));

    write_raw_models_json(
        &models_path,
        json!({ "anthropic": { "baseUrl": "https://second-proxy.example.com/v1" } }),
    );
    registry.refresh(None).await.unwrap();
    assert_eq!(
        get_models_for_provider(&registry, "anthropic")[0].base_url,
        "https://second-proxy.example.com/v1"
    );
}

#[tokio::test]
async fn custom_models_merge_inheritance_validation_and_replacement() {
    let dir = TempDir::new().unwrap();
    let models_path = dir.path().join("models.json").to_str().unwrap().to_owned();
    let auth = Arc::new(AuthStorage::in_memory(None));

    // Inherit api + baseUrl on built-in provider
    write_raw_models_json(
        &models_path,
        json!({
            "openrouter": {
                "models": [{
                    "id": "fake-provider/fake-model",
                    "name": "Fake model",
                    "reasoning": true,
                    "input": ["text"]
                }]
            }
        }),
    );
    let r1 = create_registry(auth.clone(), &models_path).await;
    assert!(r1.get_error().is_none());
    let inherited = r1.find("openrouter", "fake-provider/fake-model").unwrap();
    assert_eq!(inherited.api, "openai-completions");
    assert_eq!(inherited.base_url, "https://openrouter.ai/api/v1");

    // Non-built-in provider requires baseUrl
    write_raw_models_json(
        &models_path,
        json!({
            "my-custom-provider": {
                "apiKey": "test-key",
                "models": [{
                    "id": "my-model",
                    "api": "openai-completions",
                    "reasoning": false,
                    "input": ["text"]
                }]
            }
        }),
    );
    let r2 = create_registry(auth.clone(), &models_path).await;
    assert!(r2.get_error().unwrap_or_default().contains("baseUrl"));

    // Reports every composition error
    write_raw_models_json(
        &models_path,
        json!({
            "broken-one": { "api": "openai-completions", "models": [{ "id": "one" }] },
            "broken-two": { "api": "openai-completions", "models": [{ "id": "two" }] }
        }),
    );
    let r3 = create_registry(auth.clone(), &models_path).await;
    let err = r3.get_error().unwrap_or_default();
    assert!(err.contains("Provider \"broken-one\""));
    assert!(err.contains("Provider \"broken-two\""));

    // Replacing built-in model by id
    write_raw_models_json(
        &models_path,
        json!({
            "openrouter": {
                "baseUrl": "https://my-proxy.example.com/v1",
                "apiKey": "test-key",
                "api": "openai-completions",
                "models": [{
                    "id": "anthropic/claude-sonnet-4",
                    "name": "Replaced",
                    "reasoning": false,
                    "input": ["text"],
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                    "contextWindow": 100000,
                    "maxTokens": 8000
                }]
            }
        }),
    );
    let r4 = create_registry(auth, &models_path).await;
    let sonnet_models: Vec<_> = get_models_for_provider(&r4, "openrouter")
        .into_iter()
        .filter(|m| m.id == "anthropic/claude-sonnet-4")
        .collect();
    assert_eq!(sonnet_models.len(), 1);
    assert_eq!(sonnet_models[0].base_url, "https://my-proxy.example.com/v1");
}

#[tokio::test]
async fn compat_merging_schemas_and_model_overrides() {
    let dir = TempDir::new().unwrap();
    let models_path = dir.path().join("models.json").to_str().unwrap().to_owned();
    let auth = Arc::new(AuthStorage::in_memory(None));

    // Provider compat + model compat + thinkingLevelMap + chatTemplateKwargs/Args + cacheControlFormat
    write_raw_models_json(
        &models_path,
        json!({
            "demo": {
                "baseUrl": "https://example.com/v1",
                "apiKey": "DEMO_KEY",
                "api": "openai-completions",
                "compat": {
                    "supportsUsageInStreaming": false,
                    "maxTokensField": "max_tokens",
                    "supportsFinishReason": true
                },
                "models": [
                    {
                        "id": "kwargs-model",
                        "reasoning": true,
                        "input": ["text"],
                        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                        "contextWindow": 1000,
                        "maxTokens": 100,
                        "thinkingLevelMap": { "minimal": null, "high": "max" },
                        "compat": {
                            "supportsUsageInStreaming": true,
                            "maxTokensField": "max_completion_tokens",
                            "supportsStrictMode": false,
                            "cacheControlFormat": "anthropic",
                            "thinkingFormat": "chat-template",
                            "chatTemplateKwargs": {
                                "preserve_thinking": true,
                                "thinking": { "$var": "thinking.enabled" }
                            }
                        }
                    },
                    {
                        "id": "args-model",
                        "reasoning": true,
                        "input": ["text"],
                        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                        "contextWindow": 1000,
                        "maxTokens": 100,
                        "compat": {
                            "supportsFinishReason": false,
                            "thinkingFormat": "baseten",
                            "chatTemplateArgs": {
                                "enable_thinking": { "$var": "thinking.enabled" }
                            }
                        }
                    }
                ]
            },
            "anthropic-demo": {
                "baseUrl": "https://example.com",
                "apiKey": "DEMO_KEY",
                "api": "anthropic-messages",
                "compat": {
                    "supportsEagerToolInputStreaming": false,
                    "supportsLongCacheRetention": false
                },
                "models": [{
                    "id": "demo-model",
                    "reasoning": true,
                    "input": ["text"],
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                    "contextWindow": 1000,
                    "maxTokens": 100
                }]
            }
        }),
    );

    let registry = create_registry(auth.clone(), &models_path).await;
    assert!(registry.get_error().is_none());

    let kwargs_model = registry.find("demo", "kwargs-model").unwrap();
    let tmap = kwargs_model.thinking_level_map.as_ref().unwrap();
    assert_eq!(tmap.get(&ModelThinkingLevel::Minimal), Some(&None));
    assert_eq!(tmap.get(&ModelThinkingLevel::High), Some(&Some("max".to_owned())));
    match kwargs_model.compat.as_ref().unwrap() {
        ModelCompat::OpenAICompletions(c) => {
            assert_eq!(c.supports_usage_in_streaming, Some(true));
            assert_eq!(c.max_tokens_field, Some(MaxTokensField::MaxCompletionTokens));
            assert_eq!(c.supports_finish_reason, Some(true));
            assert_eq!(c.supports_strict_mode, Some(false));
            assert_eq!(c.cache_control_format, Some(CacheControlFormat::Anthropic));
            assert_eq!(c.thinking_format, Some(ThinkingFormat::ChatTemplate));
            let kw = c.chat_template_kwargs.as_ref().unwrap();
            assert_eq!(kw.get("preserve_thinking"), Some(&ChatTemplateKwargValue::Bool(true)));
            assert!(matches!(
                kw.get("thinking"),
                Some(ChatTemplateKwargValue::Var {
                    var: ChatTemplateVarKind::ThinkingEnabled,
                    ..
                })
            ));
        }
        other => panic!("expected OpenAICompletions compat, got {other:?}"),
    }

    let args_model = registry.find("demo", "args-model").unwrap();
    match args_model.compat.as_ref().unwrap() {
        ModelCompat::OpenAICompletions(c) => {
            assert_eq!(c.supports_usage_in_streaming, Some(false));
            assert_eq!(c.max_tokens_field, Some(MaxTokensField::MaxTokens));
            assert_eq!(c.supports_finish_reason, Some(false));
            assert_eq!(c.thinking_format, Some(ThinkingFormat::Baseten));
            assert!(c.chat_template_args.as_ref().unwrap().contains_key("enable_thinking"));
        }
        other => panic!("expected OpenAICompletions compat, got {other:?}"),
    }

    let anth = registry.find("anthropic-demo", "demo-model").unwrap();
    match anth.compat.as_ref().unwrap() {
        ModelCompat::AnthropicMessages(c) => {
            assert_eq!(c.supports_eager_tool_input_streaming, Some(false));
            assert_eq!(c.supports_long_cache_retention, Some(false));
        }
        other => panic!("expected AnthropicMessages compat, got {other:?}"),
    }

    // Anthropic allowedFallbackModels override + deep merge of inputLimits + promptCache + samplingParams + cost + openRouterRouting
    write_raw_models_json(
        &models_path,
        json!({
            "anthropic": {
                "modelOverrides": {
                    "claude-fable-5": {
                        "compat": {
                            "allowedFallbackModels": [{
                                "provider": "anthropic",
                                "model": "claude-opus-5",
                                "cost": { "input": 5, "output": 25, "cacheRead": 0.5, "cacheWrite": 6.25 }
                            }]
                        }
                    },
                    "claude-sonnet-4-6": {
                        "promptCache": { "long": 1800 }
                    }
                }
            },
            "openrouter": {
                "baseUrl": "https://my-proxy.example.com/v1",
                "api": "openai-completions",
                "models": [
                    {
                        "id": "custom/sampling-model",
                        "samplingParams": { "temperature": 1, "top_p": 0.95, "top_k": 0 }
                    },
                    {
                        "id": "custom/cached-model",
                        "promptCache": { "short": 120 }
                    }
                ],
                "modelOverrides": {
                    "anthropic/claude-sonnet-4": {
                        "name": "Proxied Sonnet",
                        "cost": { "input": 99 },
                        "samplingParams": { "top_p": 0.9 },
                        "promptCache": { "short": 300 },
                        "headers": { "X-Custom-Model-Header": "value" }
                    },
                    "amazon/nova-2-lite-v1": {
                        "compat": { "openRouterRouting": { "only": ["amazon-bedrock"] } }
                    },
                    "nonexistent/model-id": {
                        "name": "Ignored"
                    }
                }
            },
            "vision": {
                "baseUrl": "https://example.com",
                "apiKey": "test-key",
                "api": "openai-completions",
                "models": [{
                    "id": "vision-model",
                    "input": ["text", "image"],
                    "inputLimits": {
                        "maxRequestBytes": 33554432,
                        "images": {
                            "maxPerRequest": 100,
                            "resize": { "maxWidth": 2000, "maxHeight": 2000, "maxBytes": 4718592, "jpegQuality": 80 }
                        }
                    }
                }],
                "modelOverrides": {
                    "vision-model": {
                        "inputLimits": {
                            "images": { "resize": { "maxWidth": 1568, "maxBytes": 524288, "jpegQuality": 75 } }
                        }
                    }
                }
            }
        }),
    );

    let reg = create_registry(auth, &models_path).await;
    assert!(reg.get_error().is_none());
    assert!(reg.find("openrouter", "nonexistent/model-id").is_none());

    let sonnet = reg.find("openrouter", "anthropic/claude-sonnet-4").unwrap();
    assert_eq!(sonnet.base_url, "https://my-proxy.example.com/v1");
    assert_eq!(sonnet.name, "Proxied Sonnet");
    assert_eq!(sonnet.cost.input, 99.0);
    assert!(sonnet.cost.output > 0.0);
    assert_eq!(sonnet.sampling_params.as_ref().unwrap().get("top_p"), Some(&json!(0.9)));
    assert_eq!(sonnet.prompt_cache.as_ref().unwrap().short, Some(300));
    let nova = reg.find("openrouter", "amazon/nova-2-lite-v1").unwrap();
    match nova.compat.as_ref().unwrap() {
        ModelCompat::OpenAICompletions(c) => {
            assert_eq!(
                c.open_router_routing,
                Some(OpenRouterRouting {
                    only: Some(vec!["amazon-bedrock".to_owned()]),
                    ..Default::default()
                })
            );
        }
        other => panic!("expected OpenAI compat, got {other:?}"),
    }

    let vision = reg.find("vision", "vision-model").unwrap();
    let limits = vision.input_limits.as_ref().unwrap();
    assert_eq!(limits.max_request_bytes, Some(33554432));
    let resize = limits.images.as_ref().unwrap().resize.as_ref().unwrap();
    assert_eq!(resize.max_width, Some(1568));
    assert_eq!(resize.max_height, Some(2000));
    assert_eq!(resize.max_bytes, Some(524288));
    assert_eq!(resize.jpeg_quality, Some(75.0));

    let fable = reg.find("anthropic", "claude-fable-5").unwrap();
    match fable.compat.as_ref().unwrap() {
        ModelCompat::AnthropicMessages(c) => {
            assert_eq!(c.allowed_fallback_models.as_ref().unwrap().len(), 1);
            assert_eq!(c.allowed_fallback_models.as_ref().unwrap()[0].model, "claude-opus-5");
        }
        other => panic!("expected Anthropic compat, got {other:?}"),
    }
}

#[tokio::test]
async fn dynamic_provider_lifecycle_overrides_and_validation() {
    let dir = TempDir::new().unwrap();
    let models_path = dir.path().join("models.json").to_str().unwrap().to_owned();
    let auth = Arc::new(AuthStorage::in_memory(None));
    let registry = create_registry(auth.clone(), &models_path).await;

    assert_eq!(registry.get_provider_display_name("openai"), "OpenAI");
    assert_eq!(registry.get_provider_display_name("github-copilot"), "GitHub Copilot");
    assert_eq!(registry.get_provider_display_name("zai"), "Z.AI");
    assert_eq!(
        registry.get_provider_display_name("unknown-provider"),
        "unknown-provider"
    );

    registry
        .register_provider(
            "named-provider",
            ProviderConfigInput {
                name: Some("Named Provider".to_owned()),
                ..provider_config("https://provider.test/v1", &["demo-model"], "openai-completions")
            },
        )
        .unwrap();
    assert_eq!(registry.get_provider_display_name("named-provider"), "Named Provider");

    registry
        .register_provider(
            "oauth-provider",
            ProviderConfigInput {
                base_url: Some("https://provider.test/v1".to_owned()),
                api: Some("openai-completions".to_owned()),
                oauth: Some(ExtensionOAuthConfig {
                    name: "OAuth Provider".to_owned(),
                    is_subscription: None,
                    uses_callback_server: None,
                    login: Arc::new(|_| {
                        Box::pin(async move {
                            Ok(pi_ai::compat::extension_oauth_types::OAuthCredentials {
                                access: "access".to_owned(),
                                refresh: "refresh".to_owned(),
                                expires: now_ms() + 60_000,
                                extra: IndexMap::new(),
                            })
                        })
                    }),
                    refresh_token: Arc::new(|creds, _| Box::pin(async move { Ok(creds) })),
                    get_api_key: Arc::new(|creds| creds.access.clone()),
                    modify_models: None,
                }),
                models: Some(vec![chat_model_cfg("demo-model", None, None)]),
                ..empty_provider_config()
            },
        )
        .unwrap();
    assert_eq!(registry.get_provider_display_name("oauth-provider"), "OAuth Provider");

    // Failed register_provider does not persist broken streamSimple config
    let broken_err = registry
        .register_provider(
            "broken-provider",
            ProviderConfigInput {
                stream_simple: Some(Arc::new(|_, _, _| AssistantMessageEventStream::new())),
                ..empty_provider_config()
            },
        )
        .unwrap_err();
    assert!(
        broken_err
            .to_string()
            .contains("Provider broken-provider: \"api\" is required when registering streamSimple.")
    );
    let refresh_res = registry.refresh(None).await.unwrap();
    assert!(!refresh_res.aborted);

    // streamSimple overlay does not mutate global compat API provider
    let empty_ctx = normalize_context(&Context {
        system_prompt: None,
        messages: Vec::new(),
        tools: None,
    });
    let mut dummy_model = registry.get_all()[0].clone();
    dummy_model.api = "openai-completions".to_owned();
    registry
        .register_provider(
            "stream-override-provider",
            ProviderConfigInput {
                api: Some("openai-completions".to_owned()),
                stream_simple: Some(Arc::new(|_, _, _| {
                    panic!("custom streamSimple override should not mutate global api");
                })),
                ..empty_provider_config()
            },
        )
        .unwrap();
    assert!(get_api_provider("openai-completions").is_some());
    let _stream = (get_api_provider("openai-completions").unwrap().stream_simple)(&dummy_model, &empty_ctx, None);
    registry.unregister_provider("stream-override-provider");

    // Persistence of dynamic overrides across refresh
    registry
        .register_provider(
            "anthropic",
            ProviderConfigInput {
                base_url: Some("https://proxy.test/anthropic".to_owned()),
                ..empty_provider_config()
            },
        )
        .unwrap();
    registry.refresh(None).await.unwrap();
    let anth = get_models_for_provider(&registry, "anthropic");
    assert!(anth.len() > 1);
    assert!(anth.iter().all(|m| m.base_url == "https://proxy.test/anthropic"));

    registry
        .register_provider(
            "custom-provider",
            provider_config(
                "https://custom.test/v1",
                &["custom-a", "custom-b"],
                "openai-completions",
            ),
        )
        .unwrap();
    registry
        .register_provider(
            "custom-provider",
            ProviderConfigInput {
                base_url: Some("https://proxy.test/custom".to_owned()),
                ..empty_provider_config()
            },
        )
        .unwrap();
    registry.refresh(None).await.unwrap();
    let custom_models = get_models_for_provider(&registry, "custom-provider");
    assert_eq!(
        custom_models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["custom-a", "custom-b"]
    );
    assert!(custom_models.iter().all(|m| m.base_url == "https://proxy.test/custom"));
}

#[tokio::test]
#[serial]
async fn api_key_resolution_env_escaping_and_commands() {
    let dir = TempDir::new().unwrap();
    let models_path = dir.path().join("models.json").to_str().unwrap().to_owned();
    let auth = Arc::new(AuthStorage::in_memory(None));
    clear_config_value_cache();

    let _env_guard = EnvGuard::set_many(&[
        ("TEST_API_KEY_12345", Some("env-api-key-value")),
        ("TEST_BRACED_API_KEY_12345", Some("braced-env-api-key-value")),
        ("TEST_INTERPOLATED_PART_A_12345", Some("left")),
        ("TEST_INTERPOLATED_PART_B_12345", Some("right")),
        ("TEST_API_KEY_MISSING_TEST_98765", None),
        ("literal_api_key_value", None),
    ]);

    write_raw_models_json(
        &models_path,
        json!({
            "cmd-echo": provider_with_api_key("!echo test-api-key-from-command"),
            "cmd-trim": provider_with_api_key("!echo '  spaced-key  '"),
            "cmd-multiline": provider_with_api_key("!printf 'line1\\nline2'"),
            "cmd-fail": provider_with_api_key("!exit 1"),
            "cmd-empty": provider_with_api_key("!printf ''"),
            "cmd-pipe": provider_with_api_key("!echo 'hello world' | tr ' ' '-'"),
            "env-dollar": provider_with_api_key("$TEST_API_KEY_12345"),
            "env-braced": provider_with_api_key("${TEST_BRACED_API_KEY_12345}"),
            "env-interp": provider_with_api_key("${TEST_INTERPOLATED_PART_A_12345}_${TEST_INTERPOLATED_PART_B_12345}"),
            "env-escaped-dollar": provider_with_api_key("$$TEST_API_KEY_12345"),
            "env-escaped-bang": provider_with_api_key("$!literal-$TEST_API_KEY_12345"),
            "plain-matching-env": provider_with_api_key("TEST_API_KEY_12345"),
            "plain-literal": provider_with_api_key("literal_api_key_value"),
            "env-missing": provider_with_api_key("$TEST_API_KEY_MISSING_TEST_98765")
        }),
    );

    let registry = create_registry(auth, &models_path).await;
    assert_eq!(
        registry.get_api_key_for_provider("cmd-echo").await.as_deref(),
        Some("test-api-key-from-command")
    );
    assert_eq!(
        registry.get_api_key_for_provider("cmd-trim").await.as_deref(),
        Some("spaced-key")
    );
    assert_eq!(
        registry.get_api_key_for_provider("cmd-multiline").await.as_deref(),
        Some("line1\nline2")
    );
    assert!(registry.get_api_key_for_provider("cmd-fail").await.is_none());
    assert!(registry.get_api_key_for_provider("cmd-empty").await.is_none());
    assert_eq!(
        registry.get_api_key_for_provider("cmd-pipe").await.as_deref(),
        Some("hello-world")
    );
    assert_eq!(
        registry.get_api_key_for_provider("env-dollar").await.as_deref(),
        Some("env-api-key-value")
    );
    assert_eq!(
        registry.get_api_key_for_provider("env-braced").await.as_deref(),
        Some("braced-env-api-key-value")
    );
    assert_eq!(
        registry.get_api_key_for_provider("env-interp").await.as_deref(),
        Some("left_right")
    );
    assert_eq!(
        registry.get_api_key_for_provider("env-escaped-dollar").await.as_deref(),
        Some("$TEST_API_KEY_12345")
    );
    assert_eq!(
        registry.get_api_key_for_provider("env-escaped-bang").await.as_deref(),
        Some("!literal-env-api-key-value")
    );
    assert_eq!(
        registry.get_api_key_for_provider("plain-matching-env").await.as_deref(),
        Some("TEST_API_KEY_12345")
    );
    assert_eq!(
        registry.get_api_key_for_provider("plain-literal").await.as_deref(),
        Some("literal_api_key_value")
    );

    let env_status = registry.get_provider_auth_status("env-dollar");
    assert!(env_status.configured);
    assert_eq!(env_status.source, Some(AuthStatusSource::Environment));
    assert_eq!(env_status.label.as_deref(), Some("TEST_API_KEY_12345"));

    let interp_status = registry.get_provider_auth_status("env-interp");
    assert!(interp_status.configured);
    assert_eq!(interp_status.source, Some(AuthStatusSource::Environment));
    assert_eq!(
        interp_status.label.as_deref(),
        Some("TEST_INTERPOLATED_PART_A_12345, TEST_INTERPOLATED_PART_B_12345")
    );

    let key_status = registry.get_provider_auth_status("plain-literal");
    assert!(key_status.configured);
    assert_eq!(key_status.source, Some(AuthStatusSource::ModelsJsonKey));

    let missing_status = registry.get_provider_auth_status("env-missing");
    assert!(!missing_status.configured);
    assert!(!registry.get_available().iter().any(|m| m.provider == "env-missing"));
}

#[tokio::test]
#[serial]
async fn register_provider_treats_uppercase_api_key_and_headers_as_literals() {
    let dir = TempDir::new().unwrap();
    let models_path = dir.path().join("models.json").to_str().unwrap().to_owned();
    let auth = Arc::new(AuthStorage::in_memory(None));
    let _env_guard = EnvGuard::set_many(&[
        ("CUSTOM_NAME", Some("env-CUSTOM_NAME")),
        ("BEARER", Some("env-BEARER")),
        ("MODEL_TOKEN", Some("env-MODEL_TOKEN")),
    ]);
    let cap = capture_console();
    let registry = create_registry(auth, &models_path).await;

    let mut hdrs = IndexMap::new();
    hdrs.insert("Authorization".to_owned(), "BEARER".to_owned());
    let mut m_hdrs = IndexMap::new();
    m_hdrs.insert("x-model-token".to_owned(), "MODEL_TOKEN".to_owned());

    let mut cfg = provider_config("https://provider.test/v1", &["demo-model"], "openai-completions");
    cfg.api_key = Some("CUSTOM_NAME".to_owned());
    cfg.headers = Some(hdrs);
    cfg.models = Some(vec![ProviderModelConfig::Chat(ProviderChatModelConfig {
        id: "demo-model".to_owned(),
        name: "demo-model".to_owned(),
        api: None,
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
        headers: Some(m_hdrs),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 100_000,
        max_tokens: 8000,
        sampling_params: None,
        compat: None,
    })]);

    registry.register_provider("literal-provider", cfg).unwrap();
    assert_eq!(
        registry.get_api_key_for_provider("literal-provider").await.as_deref(),
        Some("CUSTOM_NAME")
    );
    let model = registry.find("literal-provider", "demo-model").unwrap();
    match registry.get_api_key_and_headers(&model).await {
        pi_coding_agent::core::model_registry::ResolvedRequestAuth::Ok { api_key, headers, .. } => {
            assert_eq!(api_key.as_deref(), Some("CUSTOM_NAME"));
            let h = headers.unwrap();
            assert_eq!(h.get("Authorization").cloned().flatten().as_deref(), Some("BEARER"));
            assert_eq!(
                h.get("x-model-token").cloned().flatten().as_deref(),
                Some("MODEL_TOKEN")
            );
        }
        other => panic!("unexpected auth result: {other:?}"),
    }
    assert!(cap.stderr().is_empty());
}

#[tokio::test]
#[serial]
async fn stored_api_key_env_and_request_time_command_execution() {
    let dir = TempDir::new().unwrap();
    let models_path = dir.path().join("models.json").to_str().unwrap().to_owned();
    let auth = Arc::new(AuthStorage::in_memory(None));

    // Stored API key env propagates to Cloudflare headers + env
    let mut stored_env = IndexMap::new();
    stored_env.insert("CLOUDFLARE_API_KEY".to_owned(), "stored-cf-token".to_owned());
    stored_env.insert("CLOUDFLARE_ACCOUNT_ID".to_owned(), "stored-account".to_owned());
    stored_env.insert("CLOUDFLARE_GATEWAY_ID".to_owned(), "stored-gateway".to_owned());
    auth.modify_with(
        "cloudflare-ai-gateway",
        move |_| async move {
            Ok(Some(Credential::ApiKey(ApiKeyCredential {
                key: Some("$CLOUDFLARE_API_KEY".to_owned()),
                env: Some(stored_env),
            })))
        },
        None,
    )
    .await
    .unwrap();
    write_raw_models_json(
        &models_path,
        json!({
            "cloudflare-ai-gateway": {
                "headers": { "x-account": "$CLOUDFLARE_ACCOUNT_ID" }
            }
        }),
    );
    let cf_reg = create_registry(auth.clone(), &models_path).await;
    let cf_model = cf_reg
        .get_all()
        .into_iter()
        .find(|m| m.provider == "cloudflare-ai-gateway")
        .unwrap();
    match cf_reg.get_api_key_and_headers(&cf_model).await {
        pi_coding_agent::core::model_registry::ResolvedRequestAuth::Ok {
            api_key, headers, env, ..
        } => {
            assert!(api_key.is_none());
            let h = headers.unwrap();
            assert_eq!(
                h.get("cf-aig-authorization").cloned().flatten().as_deref(),
                Some("Bearer stored-cf-token")
            );
            assert_eq!(h.get("x-account").cloned().flatten().as_deref(), Some("stored-account"));
            let e = env.unwrap();
            assert_eq!(
                e.get("CLOUDFLARE_ACCOUNT_ID").map(String::as_str),
                Some("stored-account")
            );
        }
        other => panic!("unexpected cf auth: {other:?}"),
    }

    // Command executed per lookup + not executed during getAvailable or getProviderAuthStatus
    let counter_file = dir.path().join("counter");
    let counter_path = counter_file.to_str().unwrap().to_owned();
    js_write_file(&counter_path, "0", None).unwrap();
    let cmd = format!(
        "!sh -c 'count=$(cat \"{}\"); echo $((count + 1)) > \"{}\"; echo \"key-value\"'",
        to_sh_path(&counter_path),
        to_sh_path(&counter_path)
    );
    write_raw_models_json(
        &models_path,
        json!({
            "custom-cmd": {
                "baseUrl": "https://example.com/v1",
                "apiKey": cmd,
                "api": "anthropic-messages",
                "authHeader": true,
                "models": [{
                    "id": "test-model",
                    "name": "Test Model",
                    "reasoning": false,
                    "input": ["text"],
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                    "contextWindow": 100000,
                    "maxTokens": 8000
                }]
            },
            "missing-auth": {
                "baseUrl": "https://example.test/v1",
                "api": "openai-completions",
                "authHeader": true,
                "models": [{ "id": "test-model" }]
            }
        }),
    );

    let reg = create_registry(auth.clone(), &models_path).await;
    assert_eq!(
        reg.get_provider_auth_status("custom-cmd").source,
        Some(AuthStatusSource::ModelsJsonCommand)
    );
    assert!(reg.get_available().iter().any(|m| m.provider == "custom-cmd"));
    assert_eq!(read_to_string(&counter_path).unwrap().trim(), "0");

    let key1 = reg.get_api_key_for_provider("custom-cmd").await;
    let key2 = reg.get_api_key_for_provider("custom-cmd").await;
    assert_eq!(key1.as_deref(), Some("key-value"));
    assert_eq!(key2.as_deref(), Some("key-value"));
    assert_eq!(read_to_string(&counter_path).unwrap().trim(), "2");

    let missing_model = reg.find("missing-auth", "test-model").unwrap();
    match reg.get_api_key_and_headers(&missing_model).await {
        pi_coding_agent::core::model_registry::ResolvedRequestAuth::Err { error } => {
            assert_eq!(error, "No API key found for \"missing-auth\"");
        }
        other => panic!("expected missing key err, got {other:?}"),
    }

    // Stored credentials bypass fallback command
    auth.modify_with(
        "custom-cmd",
        |_| async move {
            Ok(Some(Credential::ApiKey(ApiKeyCredential {
                key: Some("stored-key".to_owned()),
                env: None,
            })))
        },
        None,
    )
    .await
    .unwrap();
    let custom_model = reg.find("custom-cmd", "test-model").unwrap();
    let bypassed = reg.get_api_key_and_headers(&custom_model).await;
    assert_eq!(bypassed.api_key(), Some("stored-key"));
    assert_eq!(read_to_string(&counter_path).unwrap().trim(), "2");

    // GitHub Copilot availableModelIds filter
    let copilot_model = get_builtin_models("github-copilot")[0].clone();
    let copilot_id = copilot_model.id.clone();
    let mut copilot_extra = IndexMap::new();
    copilot_extra.insert("availableModelIds".to_owned(), json!([copilot_id]));
    auth.modify_with(
        "github-copilot",
        move |_| async move {
            Ok(Some(Credential::OAuth(OAuthCredential {
                refresh: "github-access-token".to_owned(),
                access: "tid=test;exp=9999999999;proxy-ep=proxy.individual.githubcopilot.com;".to_owned(),
                expires: now_ms() + 60_000,
                extra: copilot_extra,
            })))
        },
        None,
    )
    .await
    .unwrap();
    let copilot_reg = create_registry(auth, &models_path).await;
    let copilot_avail: Vec<_> = copilot_reg
        .get_available()
        .into_iter()
        .filter(|m| m.provider == "github-copilot")
        .map(|m| m.id)
        .collect();
    assert_eq!(copilot_avail, vec![copilot_model.id]);
    let _unused: Option<AuthOperationOptions> = None;
}
