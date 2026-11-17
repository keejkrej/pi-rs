//! Port of packages/coding-agent/test/model-runtime-images.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::auth::types::{ApiKeyAuth, ApiKeyAuthInput, AuthResult, ModelAuth, ProviderAuth};
use pi_ai::models::{CreateProviderApi, CreateProviderOptions, ModelsImagesOptions};
use pi_ai::models_store::InMemoryModelsStore;
use pi_ai::types::{
    AnyModel, AssistantImages, ClassifierAnswer, ClassifierBoolCriteria, ClassifierContext, ClassifierModel,
    ClassifierQuestion, ClassifierResult, ClassifierStopReason, DeferredCancelOptions, DeferredFetchOptions,
    DeferredHandle, ImageContent, ImageModel, ImagesContext, ImagesOptions, ImagesStopReason, Model, ModelCost,
    ModelInput, ModelOutput, ModelType, ProviderClassifier, ProviderImages, ProviderStreams, TextContent,
    TranscriptContext, UserContentBlock,
};
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use pi_ai::utils::model_operations::{assert_chat_model, assert_classifier_model, assert_image_model};
use pi_ai::{Error, is_model_type};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_coding_agent::core::provider_composer::{
    ProviderChatModelConfig, ProviderClassifierModelConfig, ProviderConfigInput, ProviderImageModelConfig,
    ProviderModelConfig,
};
use pi_js::abort::AbortController;
use pi_js::fs::write_file;
use pi_js::json::stringify;
use serde_json::json;
use tempfile::TempDir;

fn zero_cost() -> ModelCost {
    ModelCost {
        input: 0.0,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
        tiers: None,
    }
}

fn image_model(provider: &str, id: &str) -> ImageModel {
    ImageModel {
        id: id.to_owned(),
        name: id.to_owned(),
        api: "test-images".to_owned(),
        provider: provider.to_owned(),
        base_url: "https://images.test/v1".to_owned(),
        input: vec![ModelInput::Text],
        input_limits: None,
        output: vec![ModelOutput::Image],
        cost: zero_cost(),
        headers: None,
    }
}

fn classifier_model(provider: &str, id: &str) -> ClassifierModel {
    ClassifierModel {
        id: id.to_owned(),
        name: id.to_owned(),
        api: "test-classifier".to_owned(),
        provider: provider.to_owned(),
        base_url: "https://classifier.test/v1".to_owned(),
        input: vec![ModelInput::Text],
        input_limits: None,
        context_window: 1000,
        cost: zero_cost(),
        headers: None,
    }
}

fn chat_model(provider: &str, id: &str, base_url: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: id.to_owned(),
        api: "test-chat".to_owned(),
        provider: provider.to_owned(),
        base_url: base_url.to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: zero_cost(),
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn ok_image_result(model: &ImageModel) -> AssistantImages {
    AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: vec![UserContentBlock::Image(ImageContent::new("aGk=", "image/png"))],
        response_id: None,
        usage: None,
        stop_reason: ImagesStopReason::Stop,
        error_message: None,
        timestamp: 0,
    }
}

fn sample_image_context() -> ImagesContext {
    ImagesContext {
        input: vec![UserContentBlock::Text(TextContent::new("a red circle"))],
    }
}

fn sample_classifier_context() -> ClassifierContext {
    let mut state = IndexMap::new();
    state.insert("text".to_owned(), json!("Looks good"));
    let mut questions = IndexMap::new();
    questions.insert(
        "approved".to_owned(),
        ClassifierQuestion::Bool {
            instructions: "Does this express approval?".to_owned(),
            criteria: ClassifierBoolCriteria {
                r#true: "Approval".to_owned(),
                r#false: "No approval".to_owned(),
            },
        },
    );
    ClassifierContext { state, questions }
}

async fn create_runtime(models_json: Option<serde_json::Value>) -> (ModelRuntime, Option<TempDir>) {
    let (models_path, dir) = match models_json {
        Some(val) => {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("models.json").to_str().unwrap().to_owned();
            write_file(&path, &stringify(&val), None).unwrap();
            (Some(path), Some(dir))
        }
        None => (None, None),
    };
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        models_path: Some(models_path),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();
    (runtime, dir)
}

struct EmptyApiKeyAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for EmptyApiKeyAuth {
    fn name(&self) -> &str {
        "Empty key"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
        Ok(Some(AuthResult {
            auth: ModelAuth::default(),
            env: None,
            source: None,
        }))
    }
}

struct StoredKeyAuth {
    label: &'static str,
}

#[async_trait::async_trait]
impl ApiKeyAuth for StoredKeyAuth {
    fn name(&self) -> &str {
        self.label
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
        let Some(key) = input.credential.and_then(|c| c.key).filter(|k| !k.is_empty()) else {
            return Ok(None);
        };
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(key),
                headers: None,
                base_url: None,
            },
            env: None,
            source: Some("stored".to_owned()),
        }))
    }
}

#[tokio::test]
async fn lists_built_in_openrouter_image_models_separately_from_chat_models() {
    let (runtime, _dir) = create_runtime(None).await;
    let images = runtime.get_models_of_type(ModelType::Image, Some("openrouter"));
    assert!(!images.is_empty());
    assert!(
        runtime
            .get_models(Some("openrouter"))
            .into_iter()
            .all(|m| is_model_type(&AnyModel::Chat(m), ModelType::Chat))
    );
    assert_eq!(
        runtime.get_model_of_type(ModelType::Image, "openrouter", images[0].id()),
        Some(images[0].clone())
    );
    assert_eq!(
        runtime
            .get_model("openrouter", "google/gemini-3-pro-image")
            .map(|m| m.api),
        Some("openai-completions".to_owned())
    );
    assert_eq!(
        runtime
            .get_model_of_type(ModelType::Image, "openrouter", "google/gemini-3-pro-image")
            .map(|m| pi_ai::get_model_type(&m)),
        Some(ModelType::Image)
    );
    let classifiers = runtime.get_models_of_type(ModelType::Classifier, Some("openrouter"));
    assert_eq!(
        runtime.get_all_models(Some("openrouter")).len(),
        runtime.get_models(Some("openrouter")).len() + images.len() + classifiers.len()
    );
}

#[tokio::test]
async fn extension_model_lists_replace_undeclared_models_of_every_operation() {
    let (runtime, _dir) = create_runtime(None).await;
    let chat = chat_model("mixed", "built-in-chat", "https://built-in.test/v1");
    let mut images = IndexMap::new();
    images.insert(
        "test-images".to_owned(),
        ProviderImages {
            generate_images: Arc::new(|m, _, _| {
                let owned = m.clone();
                Box::pin(async move { ok_image_result(&owned) })
            }),
        },
    );
    let native = pi_ai::models::create_provider(CreateProviderOptions {
        id: "mixed".to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(EmptyApiKeyAuth)),
            oauth: None,
        },
        models: vec![
            AnyModel::Chat(chat),
            AnyModel::Image(image_model("mixed", "built-in-image")),
            AnyModel::Classifier(classifier_model("mixed", "built-in-classifier")),
        ],
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: None,
        images: Some(images),
        classifiers: None,
    })
    .unwrap();
    runtime.register_native_provider(native).unwrap();

    runtime
        .register_provider(
            "mixed",
            ProviderConfigInput {
                api_key: Some("extension-secret".to_owned()),
                models: Some(vec![ProviderModelConfig::Chat(ProviderChatModelConfig {
                    id: "extension-chat".to_owned(),
                    name: "Extension chat".to_owned(),
                    api: Some("test-chat".to_owned()),
                    base_url: Some("https://chat-proxy.test/v1".to_owned()),
                    input: vec![ModelInput::Text],
                    input_limits: None,
                    cost: zero_cost(),
                    headers: None,
                    reasoning: false,
                    thinking_level_map: None,
                    prompt_cache: None,
                    context_window: 1000,
                    max_tokens: 100,
                    sampling_params: None,
                    compat: None,
                })]),
                ..Default::default()
            },
        )
        .unwrap();

    let summary: Vec<(ModelType, String)> = runtime
        .get_all_models(Some("mixed"))
        .into_iter()
        .map(|m| (pi_ai::get_model_type(&m), m.id().to_owned()))
        .collect();
    assert_eq!(summary, vec![(ModelType::Chat, "extension-chat".to_owned())]);
    assert_eq!(
        runtime.get_model("mixed", "extension-chat").map(|m| m.base_url),
        Some("https://chat-proxy.test/v1".to_owned())
    );
    assert!(runtime.get_models_of_type(ModelType::Image, Some("mixed")).is_empty());
    assert!(
        runtime
            .get_models_of_type(ModelType::Classifier, Some("mixed"))
            .is_empty()
    );
}

#[tokio::test]
async fn registers_extension_image_and_classifier_models_with_implementations() {
    let (runtime, _dir) = create_runtime(None).await;
    let observed: Arc<Mutex<Vec<(Option<String>, Option<IndexMap<String, Option<String>>>)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let obs_img = observed.clone();
    let obs_cls = observed.clone();

    let mut img_headers = IndexMap::new();
    img_headers.insert("X-Operation".to_owned(), "image".to_owned());
    let mut cls_headers = IndexMap::new();
    cls_headers.insert("X-Operation".to_owned(), "classifier".to_owned());

    let mut images_map = IndexMap::new();
    images_map.insert(
        "test-images".to_owned(),
        ProviderImages {
            generate_images: Arc::new(move |m, _, opts| {
                let owned = m.clone();
                let obs = obs_img.clone();
                let api_key = opts.as_ref().and_then(|o| o.base.api_key.clone());
                let headers = opts.as_ref().and_then(|o| o.base.headers.clone());
                Box::pin(async move {
                    obs.lock().unwrap().push((api_key, headers));
                    ok_image_result(&owned)
                })
            }),
        },
    );
    let mut classifiers_map = IndexMap::new();
    classifiers_map.insert(
        "test-classifier".to_owned(),
        ProviderClassifier {
            classify: Arc::new(move |m, _, opts| {
                let owned = m.clone();
                let obs = obs_cls.clone();
                let api_key = opts.as_ref().and_then(|o| o.base.api_key.clone());
                let headers = opts.as_ref().and_then(|o| o.base.headers.clone());
                Box::pin(async move {
                    obs.lock().unwrap().push((api_key, headers));
                    let mut answers = IndexMap::new();
                    answers.insert("approved".to_owned(), ClassifierAnswer::Bool { probability: 0.9 });
                    ClassifierResult {
                        api: owned.api,
                        provider: owned.provider,
                        model: owned.id,
                        answers,
                        usage: None,
                        stop_reason: ClassifierStopReason::Stop,
                        error_message: None,
                        timestamp: 0,
                    }
                })
            }),
        },
    );

    runtime
        .register_provider(
            "extension-operations",
            ProviderConfigInput {
                api_key: Some("extension-secret".to_owned()),
                models: Some(vec![
                    ProviderModelConfig::Image(ProviderImageModelConfig {
                        id: "shared".to_owned(),
                        name: "shared".to_owned(),
                        api: Some("test-images".to_owned()),
                        base_url: Some("https://images.test/v1".to_owned()),
                        input: vec![ModelInput::Text],
                        input_limits: None,
                        output: vec![ModelOutput::Image],
                        cost: zero_cost(),
                        headers: Some(img_headers),
                    }),
                    ProviderModelConfig::Classifier(ProviderClassifierModelConfig {
                        id: "shared".to_owned(),
                        name: "shared".to_owned(),
                        api: Some("test-classifier".to_owned()),
                        base_url: Some("https://classifier.test/v1".to_owned()),
                        input: vec![ModelInput::Text],
                        input_limits: None,
                        cost: zero_cost(),
                        headers: Some(cls_headers),
                        context_window: 1000,
                    }),
                ]),
                images: Some(images_map),
                classifiers: Some(classifiers_map),
                ..Default::default()
            },
        )
        .unwrap();

    let img_model = runtime
        .get_model_of_type(ModelType::Image, "extension-operations", "shared")
        .unwrap()
        .into_image()
        .unwrap();
    let cls_model = runtime
        .get_model_of_type(ModelType::Classifier, "extension-operations", "shared")
        .unwrap()
        .into_classifier()
        .unwrap();

    assert_eq!(
        runtime
            .generate_images(&img_model, &sample_image_context(), None)
            .await
            .stop_reason,
        ImagesStopReason::Stop
    );
    assert_eq!(
        runtime
            .classify(&cls_model, &sample_classifier_context(), None)
            .await
            .stop_reason,
        ClassifierStopReason::Stop
    );

    let obs = observed.lock().unwrap();
    assert_eq!(obs.len(), 2);
    assert_eq!(obs[0].0.as_deref(), Some("extension-secret"));
    assert_eq!(
        obs[0].1.as_ref().unwrap().get("X-Operation").and_then(|v| v.as_deref()),
        Some("image")
    );
    assert_eq!(obs[1].0.as_deref(), Some("extension-secret"));
    assert_eq!(
        obs[1].1.as_ref().unwrap().get("X-Operation").and_then(|v| v.as_deref()),
        Some("classifier")
    );
}

#[tokio::test]
async fn generates_images_through_native_provider_with_runtime_resolved_auth() {
    let (runtime, _dir) = create_runtime(None).await;
    let calls: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = calls.clone();

    let mut images_map = IndexMap::new();
    images_map.insert(
        "test-images".to_owned(),
        ProviderImages {
            generate_images: Arc::new(move |m, _, opts| {
                let owned = m.clone();
                let c = calls_clone.clone();
                let key = opts.as_ref().and_then(|o| o.base.api_key.clone());
                Box::pin(async move {
                    c.lock().unwrap().push(key);
                    ok_image_result(&owned)
                })
            }),
        },
    );

    let provider = pi_ai::models::create_provider(CreateProviderOptions {
        id: "pixels".to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(StoredKeyAuth { label: "Pixels key" })),
            oauth: None,
        },
        models: vec![AnyModel::Image(image_model("pixels", "flux"))],
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: None,
        images: Some(images_map),
        classifiers: None,
    })
    .unwrap();
    runtime.register_native_provider(provider).unwrap();

    let model = runtime
        .get_model_of_type(ModelType::Image, "pixels", "flux")
        .unwrap()
        .into_image()
        .unwrap();

    let unconfigured = runtime.generate_images(&model, &sample_image_context(), None).await;
    assert_eq!(unconfigured.stop_reason, ImagesStopReason::Error);
    assert!(
        unconfigured
            .error_message
            .as_deref()
            .is_some_and(|m| m.contains("not configured"))
    );
    assert!(calls.lock().unwrap().is_empty());

    let ctrl = AbortController::new();
    ctrl.abort(None);
    let mut cancelled_opts = ImagesOptions::default();
    cancelled_opts.base.signal = Some(ctrl.signal());
    let cancelled = runtime
        .generate_images(
            &model,
            &sample_image_context(),
            Some(ModelsImagesOptions {
                base: cancelled_opts,
                transform_headers: None,
            }),
        )
        .await;
    assert_eq!(cancelled.stop_reason, ImagesStopReason::Aborted);
    assert!(calls.lock().unwrap().is_empty());

    runtime.set_runtime_api_key("pixels", "sk-pixels", None).await.unwrap();
    let avail = runtime
        .get_available_of_type(ModelType::Image, Some("pixels"), None)
        .await
        .unwrap();
    let ids: Vec<&str> = avail.iter().map(|m| m.id()).collect();
    assert_eq!(ids, vec!["flux"]);

    let result = runtime.generate_images(&model, &sample_image_context(), None).await;
    assert_eq!(result.stop_reason, ImagesStopReason::Stop);
    assert_eq!(*calls.lock().unwrap(), vec![Some("sk-pixels".to_owned())]);
}

#[tokio::test]
async fn rejects_image_models_at_every_chat_entry_point_before_provider_dispatch() {
    let (runtime, _dir) = create_runtime(None).await;
    let dispatches = Arc::new(Mutex::new(0usize));
    let d1 = dispatches.clone();
    let d2 = dispatches.clone();
    let d3 = dispatches.clone();
    let d4 = dispatches.clone();

    let provider = pi_ai::models::create_provider(CreateProviderOptions {
        id: "hybrid".to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(StoredKeyAuth { label: "Hybrid key" })),
            oauth: None,
        },
        models: vec![AnyModel::Image(image_model("hybrid", "shared"))],
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(ProviderStreams {
            stream: Arc::new(
                move |_: &Model, _: &TranscriptContext, _| -> AssistantMessageEventStream {
                    *d1.lock().unwrap() += 1;
                    panic!("image reached chat adapter");
                },
            ),
            stream_simple: Arc::new(
                move |_: &Model, _: &TranscriptContext, _| -> AssistantMessageEventStream {
                    *d2.lock().unwrap() += 1;
                    panic!("image reached chat adapter");
                },
            ),
            fetch_deferred: Some(Arc::new(
                move |_: &Model, _: &DeferredHandle, _: Option<DeferredFetchOptions>| -> AssistantMessageEventStream {
                    *d3.lock().unwrap() += 1;
                    panic!("image reached chat adapter");
                },
            )),
            cancel_deferred: Some(Arc::new(
                move |_: &Model, _: &DeferredHandle, _: Option<DeferredCancelOptions>| {
                    *d4.lock().unwrap() += 1;
                    Box::pin(async { Err(Error::msg("image reached chat adapter")) })
                },
            )),
        })),
        images: None,
        classifiers: None,
    })
    .unwrap();
    runtime.register_native_provider(provider).unwrap();
    runtime.set_runtime_api_key("hybrid", "sk-hybrid", None).await.unwrap();

    let image = runtime
        .get_model_of_type(ModelType::Image, "hybrid", "shared")
        .unwrap()
        .into_image()
        .unwrap();
    // Compile-time Model type separation prevents image models from matching chat lookup or dispatch:
    assert!(runtime.get_model("hybrid", "shared").is_none());

    let err_img_as_chat = assert_chat_model(&AnyModel::Image(image.clone())).unwrap_err();
    assert!(err_img_as_chat.to_string().contains("is not a chat model"));

    let err_cls_as_chat = assert_chat_model(&AnyModel::Classifier(classifier_model("hybrid", "cls"))).unwrap_err();
    assert!(err_cls_as_chat.to_string().contains("is not a chat model"));

    let chat_only = AnyModel::Chat(chat_model("hybrid", "c", "https://example.test/v1"));
    assert!(
        assert_image_model(&chat_only)
            .unwrap_err()
            .to_string()
            .contains("is not an image model")
    );
    assert!(
        assert_classifier_model(&AnyModel::Image(image))
            .unwrap_err()
            .to_string()
            .contains("is not a classifier model")
    );
    assert_eq!(ImagesStopReason::Stop, ImagesStopReason::Stop);
    assert_eq!(*dispatches.lock().unwrap(), 0);
}

#[tokio::test]
async fn keeps_image_generation_on_built_in_provider_composed_with_models_json_overrides() {
    let (runtime, _dir) = create_runtime(Some(json!({
        "providers": {
            "openrouter": {
                "headers": { "X-Title": "pi" },
                "modelOverrides": {
                    "openrouter/auto": { "name": "Auto (renamed)" },
                    "google/gemini-3-pro-image": { "headers": { "X-Chat-Only": "yes" } }
                }
            }
        }
    })))
    .await;

    let provider = runtime.get_provider("openrouter").unwrap();
    let sample_img = image_model("openrouter", "dummy");
    assert!(
        provider
            .generate_images(&sample_img, &sample_image_context(), None)
            .is_some()
    );
    assert_eq!(
        runtime.get_model("openrouter", "openrouter/auto").map(|m| m.name),
        Some("Auto (renamed)".to_owned())
    );
    assert!(
        !runtime
            .get_models_of_type(ModelType::Image, Some("openrouter"))
            .is_empty()
    );

    runtime.set_runtime_api_key("openrouter", "sk-or", None).await.unwrap();
    let chat = AnyModel::Chat(runtime.get_model("openrouter", "google/gemini-3-pro-image").unwrap());
    let image = runtime
        .get_model_of_type(ModelType::Image, "openrouter", "google/gemini-3-pro-image")
        .unwrap();
    let chat_auth = runtime.get_auth_model(&chat, None).await.unwrap().unwrap();
    let image_auth = runtime.get_auth_model(&image, None).await.unwrap().unwrap();
    assert_eq!(image_auth.auth.api_key.as_deref(), Some("sk-or"));
    let ch = chat_auth.auth.headers.as_ref().unwrap();
    assert_eq!(ch.get("X-Title").and_then(|v| v.as_deref()), Some("pi"));
    assert_eq!(ch.get("X-Chat-Only").and_then(|v| v.as_deref()), Some("yes"));
    let ih = image_auth.auth.headers.as_ref().unwrap();
    assert_eq!(ih.get("X-Title").and_then(|v| v.as_deref()), Some("pi"));
    assert!(!ih.contains_key("X-Chat-Only"));
}

#[tokio::test]
async fn does_not_add_image_generation_or_classification_to_composed_chat_only_providers() {
    let (runtime, _dir) = create_runtime(Some(json!({
        "providers": {
            "anthropic": { "headers": { "X-Title": "pi" } }
        }
    })))
    .await;
    let anth = runtime.get_provider("anthropic").unwrap();
    assert!(
        anth.generate_images(&image_model("anthropic", "x"), &sample_image_context(), None)
            .is_none()
    );
    assert!(
        anth.classify(&classifier_model("anthropic", "x"), &sample_classifier_context(), None)
            .is_none()
    );
    assert!(
        runtime
            .get_provider("openrouter")
            .unwrap()
            .generate_images(&image_model("openrouter", "x"), &sample_image_context(), None)
            .is_some()
    );
    assert!(
        runtime
            .get_provider("typesafe")
            .unwrap()
            .classify(&classifier_model("typesafe", "x"), &sample_classifier_context(), None)
            .is_some()
    );
}
