//! Port of packages/ai/test/images-models.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::auth::types::{ApiKeyAuth, ApiKeyAuthInput, AuthContext, AuthResult, ModelAuth, ProviderAuth};
use pi_ai::compat::{builtin_models, get_image_model, get_image_models, get_models};
use pi_ai::models::{
    CreateModelsOptions, CreateProviderApi, CreateProviderOptions, ModelsImagesOptions, ModelsRefreshOptions,
    create_models, create_provider, get_model_type, has_api, is_model_type,
};
use pi_ai::models_generated::provider_classifier_models;
use pi_ai::models_store::{InMemoryModelsStore, ModelsStore};
use pi_ai::types::{
    AnyModel, AssistantImages, ChatModelType, ClassifierResult, ClassifierStopReason, ImageContent, ImageModel,
    ImagesContext, ImagesOptions, ImagesStopReason, Model, ModelCost, ModelInput, ModelOutput, ModelType,
    ProviderClassifier, ProviderEnv, ProviderHeaders, ProviderImages, ProviderStreams, TextContent, UserContentBlock,
};
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use pi_ai::utils::model_operations::{assert_chat_model, assert_image_model};
use pi_js::abort::AbortController;
use pi_js::time;
use serial_test::serial;

struct EnvVarGuard {
    key: String,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &str, value: &str) -> Self {
        let previous = pi_js::env::var(key);
        pi_js::env::set_var(key, value);
        Self {
            key: key.to_owned(),
            previous,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(prev) => pi_js::env::set_var(&self.key, prev),
            None => pi_js::env::remove_var(&self.key),
        }
    }
}

struct FakeAuthContext {
    env: IndexMap<String, String>,
}

impl FakeAuthContext {
    fn new(pairs: &[(&str, &str)]) -> Arc<dyn AuthContext> {
        let mut map = IndexMap::new();
        for (k, v) in pairs {
            map.insert((*k).to_owned(), (*v).to_owned());
        }
        Arc::new(Self { env: map })
    }
}

#[async_trait::async_trait]
impl AuthContext for FakeAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    async fn file_exists(&self, _path: &str) -> bool {
        false
    }
}

struct EnvOrEmptyAuth {
    env_var: Option<String>,
}

#[async_trait::async_trait]
impl ApiKeyAuth for EnvOrEmptyAuth {
    fn name(&self) -> &str {
        "Test key"
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
        let Some(var_name) = &self.env_var else {
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: None,
                source: None,
            }));
        };
        let key = input.credential.and_then(|c| c.key).or(input.ctx.env(var_name).await);
        Ok(key.map(|k| AuthResult {
            auth: ModelAuth {
                api_key: Some(k),
                headers: None,
                base_url: None,
            },
            env: None,
            source: Some(var_name.clone()),
        }))
    }
}

fn image_model(provider: &str, id: &str) -> ImageModel {
    ImageModel {
        id: id.to_owned(),
        name: id.to_owned(),
        api: "test-images".to_owned(),
        provider: provider.to_owned(),
        base_url: "https://example.test/v1".to_owned(),
        input: vec![ModelInput::Text],
        input_limits: None,
        output: vec![ModelOutput::Image],
        cost: ModelCost::default(),
        headers: None,
    }
}

fn chat_model(provider: &str, id: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: id.to_owned(),
        api: "test-chat".to_owned(),
        provider: provider.to_owned(),
        base_url: "https://example.test/v1".to_owned(),
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
    }
}

fn ok_result(model: &ImageModel) -> AssistantImages {
    AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: vec![UserContentBlock::Image(ImageContent::new("aGk=", "image/png"))],
        response_id: None,
        usage: None,
        stop_reason: ImagesStopReason::Stop,
        error_message: None,
        timestamp: time::now_ms(),
    }
}

#[derive(Clone)]
struct GenerateCall {
    #[allow(dead_code)]
    model: ImageModel,
    options: Option<ImagesOptions>,
}

fn test_provider(
    id: &str,
    models: Option<Vec<AnyModel>>,
    env_var: Option<&str>,
    calls: Option<Arc<Mutex<Vec<GenerateCall>>>>,
    image_apis: Option<&[&str]>,
) -> Arc<dyn pi_ai::models::Provider + Send + Sync> {
    let all_models = models.unwrap_or_else(|| vec![AnyModel::Image(image_model(id, "model-a"))]);
    let apis = image_apis.unwrap_or(&["test-images"]);
    let mut images_map = IndexMap::new();
    for &api_name in apis {
        let calls_ref = calls.clone();
        images_map.insert(
            api_name.to_owned(),
            ProviderImages {
                generate_images: Arc::new(move |model, _ctx, options| {
                    let m = model.clone();
                    let calls_inner = calls_ref.clone();
                    Box::pin(async move {
                        if let Some(c) = calls_inner {
                            c.lock().unwrap().push(GenerateCall {
                                model: m.clone(),
                                options,
                            });
                        }
                        ok_result(&m)
                    })
                }),
            },
        );
    }

    let mut api_map = IndexMap::new();
    api_map.insert(
        "test-chat".to_owned(),
        ProviderStreams {
            stream: Arc::new(|_, _, _| AssistantMessageEventStream::new()),
            stream_simple: Arc::new(|_, _, _| AssistantMessageEventStream::new()),
            fetch_deferred: None,
            cancel_deferred: None,
        },
    );

    create_provider(CreateProviderOptions {
        id: id.to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(EnvOrEmptyAuth {
                env_var: env_var.map(str::to_owned),
            })),
            oauth: None,
        },
        models: all_models,
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::ByApi(api_map)),
        images: Some(images_map),
        classifiers: None,
    })
    .unwrap()
}

fn sample_context() -> ImagesContext {
    ImagesContext {
        input: vec![UserContentBlock::Text(TextContent::new("a red circle"))],
    }
}

#[test]
fn treats_models_without_a_type_as_chat_models() {
    let chat = chat_model("p", "c");
    let image = image_model("p", "i");

    assert!(chat.r#type.is_none());
    let any_chat = AnyModel::Chat(chat.clone());
    let any_chat_explicit = AnyModel::Chat(Model {
        r#type: Some(ChatModelType::Chat),
        ..chat.clone()
    });
    let any_image = AnyModel::Image(image.clone());

    assert_eq!(get_model_type(&any_chat), ModelType::Chat);
    assert_eq!(get_model_type(&any_chat_explicit), ModelType::Chat);
    assert_eq!(get_model_type(&any_image), ModelType::Image);
    assert!(is_model_type(&any_chat, ModelType::Chat));
    assert!(!is_model_type(&any_chat, ModelType::Image));
    assert!(is_model_type(&any_image, ModelType::Image));

    // has_api never matches an image model, even on an equal api string
    let image_same_api = AnyModel::Image(ImageModel {
        api: "test-chat".to_owned(),
        ..image
    });
    assert!(!has_api(&image_same_api, "test-chat"));
    assert!(has_api(&any_chat, "test-chat"));
}

#[tokio::test]
async fn lists_models_without_a_type_as_chat_models_at_create_provider_boundaries() {
    struct StaticAllowAuth;
    #[async_trait::async_trait]
    impl ApiKeyAuth for StaticAllowAuth {
        fn name(&self) -> &str {
            "Test"
        }
        async fn resolve(&self, _input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
            Ok(Some(AuthResult::default()))
        }
    }

    let provider = create_provider(CreateProviderOptions {
        id: "legacy".to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(StaticAllowAuth)),
            oauth: None,
        },
        models: vec![
            AnyModel::Chat(chat_model("legacy", "static")),
            AnyModel::Image(image_model("legacy", "static")),
        ],
        fetch_models: Some(Arc::new(|_| {
            Box::pin(async { Ok(vec![AnyModel::Chat(chat_model("legacy", "dynamic"))]) })
        })),
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

    let models = create_models(None);
    models.set_provider(provider.clone());

    assert_eq!(
        provider.get_models().into_iter().map(|m| m.id).collect::<Vec<_>>(),
        vec!["static"]
    );
    models
        .refresh(Some(ModelsRefreshOptions {
            providers: Some(vec![provider.id().to_owned()]),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(
        provider.get_models().into_iter().map(|m| m.id).collect::<Vec<_>>(),
        vec!["static", "dynamic"]
    );
    assert!(provider.get_models().iter().all(|m| m.r#type.is_none()));
    assert_eq!(
        models
            .get_models_of_type(ModelType::Image, Some("legacy"))
            .into_iter()
            .map(|m| m.id().to_owned())
            .collect::<Vec<_>>(),
        vec!["static"]
    );
}

#[test]
fn lists_chat_image_and_all_models_through_typed_accessors() {
    let models = create_models(None);
    models.set_provider(test_provider(
        "p1",
        Some(vec![
            AnyModel::Chat(chat_model("p1", "c1")),
            AnyModel::Image(image_model("p1", "i1")),
            AnyModel::Image(image_model("p1", "i2")),
        ]),
        None,
        None,
        None,
    ));
    models.set_provider(test_provider(
        "p2",
        Some(vec![AnyModel::Image(image_model("p2", "i3"))]),
        None,
        None,
        None,
    ));

    assert_eq!(
        models.get_models(None).into_iter().map(|m| m.id).collect::<Vec<_>>(),
        vec!["c1"]
    );
    assert_eq!(
        models
            .get_models_of_type(ModelType::Chat, None)
            .into_iter()
            .map(|m| m.id().to_owned())
            .collect::<Vec<_>>(),
        vec!["c1"]
    );
    assert_eq!(
        models
            .get_models_of_type(ModelType::Image, None)
            .into_iter()
            .map(|m| m.id().to_owned())
            .collect::<Vec<_>>(),
        vec!["i1", "i2", "i3"]
    );
    assert_eq!(
        models
            .get_models_of_type(ModelType::Image, Some("p1"))
            .into_iter()
            .map(|m| m.id().to_owned())
            .collect::<Vec<_>>(),
        vec!["i1", "i2"]
    );
    assert!(models.get_models_of_type(ModelType::Classifier, None).is_empty());
    assert_eq!(
        models
            .get_all_models(None)
            .into_iter()
            .map(|m| m.id().to_owned())
            .collect::<Vec<_>>(),
        vec!["c1", "i1", "i2", "i3"]
    );

    assert_eq!(models.get_model("p1", "c1").map(|m| m.id), Some("c1".to_owned()));
    assert!(models.get_model("p1", "i1").is_none());
    assert_eq!(
        models
            .get_model_of_type(ModelType::Chat, "p1", "c1")
            .map(|m| m.id().to_owned()),
        Some("c1".to_owned())
    );
    assert_eq!(
        models
            .get_model_of_type(ModelType::Image, "p1", "i1")
            .map(|m| m.id().to_owned()),
        Some("i1".to_owned())
    );
    assert!(models.get_model_of_type(ModelType::Image, "p1", "c1").is_none());
}

#[tokio::test]
async fn splits_available_models_by_type() {
    let models = create_models(Some(CreateModelsOptions {
        auth_context: Some(FakeAuthContext::new(&[("KEY", "k")])),
        ..Default::default()
    }));
    models.set_provider(test_provider(
        "p1",
        Some(vec![
            AnyModel::Chat(chat_model("p1", "c1")),
            AnyModel::Image(image_model("p1", "i1")),
        ]),
        Some("KEY"),
        None,
        None,
    ));
    models.set_provider(test_provider(
        "p2",
        Some(vec![AnyModel::Image(image_model("p2", "i2"))]),
        Some("MISSING"),
        None,
        None,
    ));

    assert_eq!(
        models
            .get_available(None, None)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        vec!["c1"]
    );
    assert_eq!(
        models
            .get_available_of_type(ModelType::Chat, None, None)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.id().to_owned())
            .collect::<Vec<_>>(),
        vec!["c1"]
    );
    assert_eq!(
        models
            .get_available_of_type(ModelType::Image, None, None)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.id().to_owned())
            .collect::<Vec<_>>(),
        vec!["i1"]
    );
    assert_eq!(
        models
            .get_all_available(None, None)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.id().to_owned())
            .collect::<Vec<_>>(),
        vec!["c1", "i1"]
    );
}

#[tokio::test]
async fn resolves_auth_through_the_provider_and_merges_it_into_image_requests_explicit_options_win() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let models = create_models(Some(CreateModelsOptions {
        auth_context: Some(FakeAuthContext::new(&[("TEST_KEY", "env-key")])),
        ..Default::default()
    }));
    models.set_provider(test_provider("p1", None, Some("TEST_KEY"), Some(calls.clone()), None));
    let any_model = models.get_model_of_type(ModelType::Image, "p1", "model-a").unwrap();
    let model = any_model.as_image().unwrap().clone();

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
                Some(pi_ai::auth::resolve::AuthResolutionOverrides {
                    api_key: Some("explicit-key".to_owned()),
                    ..Default::default()
                }),
            )
            .await
            .unwrap()
            .and_then(|r| r.auth.api_key),
        Some("explicit-key".to_owned())
    );

    let result = models.generate_images(&model, &sample_context(), None).await;
    assert_eq!(result.stop_reason, ImagesStopReason::Stop);
    assert_eq!(
        calls.lock().unwrap()[0]
            .options
            .as_ref()
            .and_then(|o| o.base.api_key.as_deref()),
        Some("env-key")
    );

    models
        .generate_images(
            &model,
            &sample_context(),
            Some(ModelsImagesOptions {
                base: ImagesOptions {
                    base: pi_ai::types::ProviderRequestOptions {
                        api_key: Some("explicit".to_owned()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .await;
    assert_eq!(
        calls.lock().unwrap()[1]
            .options
            .as_ref()
            .and_then(|o| o.base.api_key.as_deref()),
        Some("explicit")
    );
}

#[tokio::test]
async fn merges_provider_resolved_env_and_applies_header_transforms() {
    struct HeaderEnvAuth;
    #[async_trait::async_trait]
    impl ApiKeyAuth for HeaderEnvAuth {
        fn name(&self) -> &str {
            "Test key"
        }
        async fn resolve(&self, _input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
            let mut headers: ProviderHeaders = IndexMap::new();
            headers.insert("x-base".to_owned(), Some("1".to_owned()));
            let mut env: ProviderEnv = IndexMap::new();
            env.insert("PROVIDER_ONLY".to_owned(), "provider".to_owned());
            env.insert("SHARED".to_owned(), "provider".to_owned());
            Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some("provider-key".to_owned()),
                    headers: Some(headers),
                    base_url: None,
                },
                env: Some(env),
                source: None,
            }))
        }
    }

    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_inner = calls.clone();
    let mut images = IndexMap::new();
    images.insert(
        "test-images".to_owned(),
        ProviderImages {
            generate_images: Arc::new(move |model, _ctx, options| {
                let m = model.clone();
                let c = calls_inner.clone();
                Box::pin(async move {
                    c.lock().unwrap().push(GenerateCall {
                        model: m.clone(),
                        options,
                    });
                    ok_result(&m)
                })
            }),
        },
    );

    let models = create_models(None);
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "p1".to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: Some(Arc::new(HeaderEnvAuth)),
                oauth: None,
            },
            models: vec![AnyModel::Image(image_model("p1", "model-a"))],
            fetch_models: None,
            filter_models: None,
            filter_all_models: None,
            api: None,
            images: Some(images),
            classifiers: None,
        })
        .unwrap(),
    );

    let model = models
        .get_model_of_type(ModelType::Image, "p1", "model-a")
        .unwrap()
        .as_image()
        .unwrap()
        .clone();

    let mut req_env: ProviderEnv = IndexMap::new();
    req_env.insert("REQUEST_ONLY".to_owned(), "request".to_owned());
    req_env.insert("SHARED".to_owned(), "request".to_owned());

    models
        .generate_images(
            &model,
            &sample_context(),
            Some(ModelsImagesOptions {
                base: ImagesOptions {
                    base: pi_ai::types::ProviderRequestOptions {
                        api_key: Some("request-key".to_owned()),
                        env: Some(req_env),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: Some(Arc::new(|mut headers| {
                    Box::pin(async move {
                        headers.insert("x-extra".to_owned(), Some("2".to_owned()));
                        Ok(headers)
                    })
                })),
            }),
        )
        .await;

    let guard = calls.lock().unwrap();
    let opts = guard[0].options.as_ref().unwrap();
    assert_eq!(opts.base.api_key.as_deref(), Some("request-key"));
    let mut expected_env = IndexMap::new();
    expected_env.insert("PROVIDER_ONLY".to_owned(), "provider".to_owned());
    expected_env.insert("SHARED".to_owned(), "request".to_owned());
    expected_env.insert("REQUEST_ONLY".to_owned(), "request".to_owned());
    assert_eq!(opts.base.env.as_ref(), Some(&expected_env));
    let mut expected_headers = IndexMap::new();
    expected_headers.insert("x-base".to_owned(), Some("1".to_owned()));
    expected_headers.insert("x-extra".to_owned(), Some("2".to_owned()));
    assert_eq!(opts.base.headers.as_ref(), Some(&expected_headers));
}

#[tokio::test]
async fn returns_error_results_instead_of_rejecting() {
    let models = create_models(Some(CreateModelsOptions {
        auth_context: Some(FakeAuthContext::new(&[])),
        ..Default::default()
    }));

    let ghost = models
        .generate_images(&image_model("ghost", "m"), &sample_context(), None)
        .await;
    assert_eq!(ghost.stop_reason, ImagesStopReason::Error);
    assert!(
        ghost
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("Unknown provider: ghost")
    );

    let calls = Arc::new(Mutex::new(Vec::new()));
    models.set_provider(test_provider("p1", None, Some("MISSING"), Some(calls.clone()), None));
    let any_model = models.get_model_of_type(ModelType::Image, "p1", "model-a").unwrap();
    let model = any_model.as_image().unwrap().clone();
    assert!(models.get_auth_model(&any_model, None).await.unwrap().is_none());
    let unconfigured = models.generate_images(&model, &sample_context(), None).await;
    assert_eq!(unconfigured.stop_reason, ImagesStopReason::Error);
    assert!(
        unconfigured
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("not configured")
    );
    assert!(calls.lock().unwrap().is_empty());

    let controller = AbortController::new();
    controller.abort(None);
    let cancelled = models
        .generate_images(
            &model,
            &sample_context(),
            Some(ModelsImagesOptions {
                base: ImagesOptions {
                    base: pi_ai::types::ProviderRequestOptions {
                        signal: Some(controller.signal()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .await;
    assert_eq!(cancelled.stop_reason, ImagesStopReason::Aborted);
    assert!(calls.lock().unwrap().is_empty());

    // A provider without any images implementation rejects image models it lists.
    struct OkAuth;
    #[async_trait::async_trait]
    impl ApiKeyAuth for OkAuth {
        fn name(&self) -> &str {
            "Test"
        }
        async fn resolve(&self, _input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
            Ok(Some(AuthResult::default()))
        }
    }
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "chat-only".to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: Some(Arc::new(OkAuth)),
                oauth: None,
            },
            models: vec![AnyModel::Image(image_model("chat-only", "i"))],
            fetch_models: None,
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
        .unwrap(),
    );
    let chat_only_img = models
        .get_model_of_type(ModelType::Image, "chat-only", "i")
        .unwrap()
        .into_image()
        .unwrap();
    let unsupported = models.generate_images(&chat_only_img, &sample_context(), None).await;
    assert_eq!(unsupported.stop_reason, ImagesStopReason::Error);
    assert!(
        unsupported
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("does not support image generation")
    );

    // An images map without the model's api yields a provider error result.
    models.set_provider(test_provider(
        "wrong-api",
        Some(vec![AnyModel::Image(image_model("wrong-api", "i"))]),
        None,
        None,
        Some(&["other-images"]),
    ));
    let wrong_api_img = models
        .get_model_of_type(ModelType::Image, "wrong-api", "i")
        .unwrap()
        .into_image()
        .unwrap();
    let missing_api = models.generate_images(&wrong_api_img, &sample_context(), None).await;
    assert_eq!(missing_api.stop_reason, ImagesStopReason::Error);
    assert!(
        missing_api
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains(r#"no image generation implementation for "test-images""#)
    );
}

#[test]
fn rejects_wrong_model_types_at_runtime_entry_points() {
    let chat = AnyModel::Chat(chat_model("p1", "chat"));
    let err = assert_image_model(&chat).unwrap_err();
    assert!(err.to_string().contains("is not an image model"));

    let img = AnyModel::Image(image_model("p1", "img"));
    let err2 = assert_chat_model(&img).unwrap_err();
    assert!(err2.to_string().contains("is not a chat model"));
}

#[test]
fn requires_at_least_one_concrete_operation_implementation() {
    struct StaticAuth;
    #[async_trait::async_trait]
    impl ApiKeyAuth for StaticAuth {
        fn name(&self) -> &str {
            "Test"
        }
        async fn resolve(&self, _input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
            Ok(Some(AuthResult::default()))
        }
    }

    let make = |api: Option<CreateProviderApi>,
                images: Option<IndexMap<String, ProviderImages>>,
                classifiers: Option<IndexMap<String, ProviderClassifier>>| {
        create_provider(CreateProviderOptions {
            id: "empty".to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: Some(Arc::new(StaticAuth)),
                oauth: None,
            },
            models: Vec::new(),
            fetch_models: None,
            filter_models: None,
            filter_all_models: None,
            api,
            images,
            classifiers,
        })
    };

    let expected = r#"at least one of "api", "images", or "classifiers""#;
    assert!(make(None, None, None).err().unwrap().to_string().contains(expected));
    assert!(
        make(Some(CreateProviderApi::ByApi(IndexMap::new())), None, None)
            .err()
            .unwrap()
            .to_string()
            .contains(expected)
    );
    assert!(
        make(None, Some(IndexMap::new()), None)
            .err()
            .unwrap()
            .to_string()
            .contains(expected)
    );
    assert!(
        make(None, None, Some(IndexMap::new()))
            .err()
            .unwrap()
            .to_string()
            .contains(expected)
    );

    let _unused_clf: fn(ClassifierResult) -> ClassifierStopReason = |r| r.stop_reason;
}

#[tokio::test]
async fn supports_dynamic_providers_listing_image_models_via_refresh() {
    struct OkAuth;
    #[async_trait::async_trait]
    impl ApiKeyAuth for OkAuth {
        fn name(&self) -> &str {
            "Test"
        }
        async fn resolve(&self, _input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
            Ok(Some(AuthResult::default()))
        }
    }

    let fetches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fetches_inner = fetches.clone();
    let models_store = Arc::new(InMemoryModelsStore::new());
    let models = create_models(Some(CreateModelsOptions {
        models_store: Some(models_store.clone()),
        ..Default::default()
    }));

    let mut images = IndexMap::new();
    images.insert(
        "test-images".to_owned(),
        ProviderImages {
            generate_images: Arc::new(|m, _, _| {
                let m = m.clone();
                Box::pin(async move { ok_result(&m) })
            }),
        },
    );

    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "dyn".to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: Some(Arc::new(OkAuth)),
                oauth: None,
            },
            models: Vec::new(),
            fetch_models: Some(Arc::new(move |_| {
                let f = fetches_inner.clone();
                Box::pin(async move {
                    f.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(vec![
                        AnyModel::Image(image_model("dyn", "listed")),
                        AnyModel::Chat(chat_model("dyn", "chat")),
                    ])
                })
            })),
            filter_models: None,
            filter_all_models: None,
            api: None,
            images: Some(images),
            classifiers: None,
        })
        .unwrap(),
    );

    assert!(models.get_all_models(Some("dyn")).is_empty());
    let result = models
        .refresh(Some(ModelsRefreshOptions {
            providers: Some(vec!["dyn".to_owned()]),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(result.errors.is_empty());
    assert_eq!(fetches.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(models.get_model_of_type(ModelType::Image, "dyn", "listed").is_some());
    assert!(models.get_model("dyn", "chat").is_some());
    let stored = models_store.read("dyn", None).await.unwrap().unwrap();
    assert_eq!(
        stored.models.iter().map(|m| m.id().to_owned()).collect::<Vec<_>>(),
        vec!["listed", "chat"]
    );
}

#[test]
fn keeps_existing_built_in_and_compat_model_reads_chat_only() {
    // WIRE(@mcp-w2): also cross-check providers::all::{get_builtin_models, get_builtin_image_models, get_all_builtin_models}
    let compat = get_models(Some("openrouter"));
    let images = get_image_models("openrouter");
    let all = builtin_models().get_all_models(Some("openrouter"));
    let chat_typed = builtin_models().get_models_of_type(ModelType::Chat, Some("openrouter"));
    let clfs = provider_classifier_models("openrouter").map(|m| m.len()).unwrap_or(0);

    assert_eq!(
        compat,
        chat_typed.into_iter().filter_map(|m| m.into_chat()).collect::<Vec<_>>()
    );
    assert!(!images.is_empty());
    assert!(compat.iter().all(|m| m.context_window > 0));
    assert!(all.iter().any(|m| is_model_type(m, ModelType::Image)));
    assert_eq!(compat.len() + images.len() + clfs, all.len());
    assert!(get_image_model("openrouter", "black-forest-labs/flux.2-pro").is_some());
}

#[tokio::test]
#[serial]
async fn builtin_models_exposes_openrouter_image_models_under_the_openrouter_provider() {
    // WIRE(@mcp-w2): providers::all::builtin_models() will supply injected AuthContext support;
    // compat::builtin_models() uses process env here.
    let _guard = EnvVarGuard::set("OPENROUTER_API_KEY", "or-key");
    let models = builtin_models();
    let provider = models.get_provider("openrouter").unwrap();
    let images = models.get_models_of_type(ModelType::Image, Some("openrouter"));
    assert!(!images.is_empty());
    assert!(!provider.get_models().is_empty());
    assert!(
        provider
            .get_all_models()
            .unwrap()
            .iter()
            .any(|m| is_model_type(m, ModelType::Image))
    );
    assert!(
        images
            .iter()
            .all(|m| { m.as_image().is_some_and(|im| im.api == "openrouter-images") })
    );
    assert!(
        models
            .get_models_of_type(ModelType::Image, None)
            .iter()
            .all(|m| m.provider() == "openrouter")
    );

    // One upstream id can expose separate chat and image operations.
    let chat = models.get_model("openrouter", "google/gemini-3-pro-image").unwrap();
    let image = models
        .get_model_of_type(ModelType::Image, "openrouter", "google/gemini-3-pro-image")
        .unwrap();
    assert_eq!(chat.api, "openai-completions");
    assert_eq!(image.as_image().unwrap().api, "openrouter-images");

    // One credential covers both.
    assert_eq!(
        models
            .get_auth_model(&image, None)
            .await
            .unwrap()
            .and_then(|r| r.auth.api_key),
        Some("or-key".to_owned())
    );
    assert_eq!(
        models
            .get_auth_model(&AnyModel::Chat(chat), None)
            .await
            .unwrap()
            .and_then(|r| r.auth.api_key),
        Some("or-key".to_owned())
    );
    let img_ref = image.as_image().unwrap();
    assert!(provider.generate_images(img_ref, &sample_context(), None).is_some());
}
