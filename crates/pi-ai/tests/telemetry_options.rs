//! Port of packages/ai/test/telemetry-options.test.ts (unit ai-providers-builtin)

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::api::simple_options::build_base_options;
use pi_ai::auth::types::{ApiKeyAuth, ApiKeyAuthInput, AuthResult, ProviderAuth};
use pi_ai::images::generate_images;
use pi_ai::images_api_registry::{clear_images_api_providers, register_images_api_provider};
use pi_ai::models::{
    CreateProviderApi, CreateProviderOptions, ModelsApiStreamOptions, ModelsDeferredCancelOptions,
    ModelsDeferredFetchOptions, ModelsImagesOptions, ModelsSimpleStreamOptions, create_models, create_provider,
};
use pi_ai::types::{
    AnyModel, AssistantImages, AssistantMessage, AssistantMessageEvent, Context, DeferredCancelOptions,
    DeferredFetchOptions, DeferredHandle, ImageModel, ImagesContext, ImagesOptions, ImagesStopReason, Model, ModelCost,
    ModelInput, ModelOutput, ProviderImages, ProviderRequestOptions, ProviderStreams, SimpleStreamOptions, StopReason,
    StreamOptions, TelemetryContextHandle, TelemetrySpanHandle, TextContent, Usage, UserContentBlock,
};
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use pi_ai::utils::transcript::normalize_context;
use pi_telemetry::NOOP_TELEMETRY_CONTEXT;
use serial_test::serial;

fn test_model() -> Model {
    Model {
        r#type: None,
        id: "model".to_string(),
        name: "Model".to_string(),
        api: "telemetry-test".to_string(),
        provider: "telemetry-provider".into(),
        base_url: "https://example.test".to_string(),
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
    }
}

fn test_image_model() -> ImageModel {
    ImageModel {
        id: "image-model".to_string(),
        name: "Image Model".to_string(),
        api: "telemetry-test-images".to_string(),
        provider: "telemetry-image-provider".into(),
        base_url: "https://example.test".to_string(),
        input: vec![ModelInput::Text],
        input_limits: None,
        output: vec![ModelOutput::Image],
        cost: ModelCost::default(),
        headers: None,
    }
}

struct TelemetryAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for TelemetryAuth {
    fn name(&self) -> &str {
        "Test"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
        Ok(Some(AuthResult::default()))
    }
}

fn completed_stream(req: &Model) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let msg = AssistantMessage {
        content: Vec::new(),
        api: req.api.clone(),
        provider: req.provider.clone(),
        model: req.id.clone(),
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
        timestamp: 0,
        duration_ms: None,
    };
    stream.push(AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message: msg,
    });
    stream
}

#[test]
fn inherited_by_simple_stream_conversion() {
    let telemetry: TelemetryContextHandle = Arc::new(NOOP_TELEMETRY_CONTEXT);
    let req_opts = ProviderRequestOptions::<Model> {
        telemetry_context: Some(Arc::clone(&telemetry)),
        ..Default::default()
    };
    let model = test_model();
    let ctx = normalize_context(&Context {
        system_prompt: None,
        messages: Vec::new(),
        tools: None,
    });
    let simple_opts = SimpleStreamOptions {
        base: StreamOptions {
            telemetry_context: req_opts.telemetry_context.clone(),
            ..Default::default()
        },
        ..Default::default()
    };
    let base_opts = build_base_options(&model, &ctx, Some(&simple_opts), None);
    assert!(Arc::ptr_eq(base_opts.telemetry_context.as_ref().unwrap(), &telemetry));
}

#[tokio::test]
async fn survives_provider_and_models_stream_and_deferred_dispatch() {
    let telemetry: TelemetryContextHandle = Arc::new(NOOP_TELEMETRY_CONTEXT);
    let model = test_model();
    let ctx = normalize_context(&Context {
        system_prompt: None,
        messages: Vec::new(),
        tools: None,
    });
    let handle = DeferredHandle {
        provider: model.provider.to_string(),
        model_id: model.id.clone(),
        api: model.api.clone(),
        id: "response".to_string(),
        expires_at: None,
        poll_after_ms: None,
        data: None,
    };

    let observed: Arc<Mutex<Vec<Option<TelemetryContextHandle>>>> = Arc::new(Mutex::new(Vec::new()));
    let obs1 = Arc::clone(&observed);
    let obs2 = Arc::clone(&observed);
    let obs3 = Arc::clone(&observed);
    let obs4 = Arc::clone(&observed);

    let provider = create_provider(CreateProviderOptions {
        id: model.provider.to_string(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(TelemetryAuth)),
            oauth: None,
        },
        models: vec![AnyModel::Chat(model.clone())],
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(ProviderStreams {
            stream: Arc::new(move |req, _ctx, opts| {
                obs1.lock().unwrap().push(opts.and_then(|o| o.base.telemetry_context));
                completed_stream(req)
            }),
            stream_simple: Arc::new(move |req, _ctx, opts| {
                obs2.lock().unwrap().push(opts.and_then(|o| o.base.telemetry_context));
                completed_stream(req)
            }),
            fetch_deferred: Some(Arc::new(move |req, _h, opts| {
                obs3.lock().unwrap().push(opts.and_then(|o| o.base.telemetry_context));
                completed_stream(req)
            })),
            cancel_deferred: Some(Arc::new(move |_req, _h, opts| {
                let obs4 = Arc::clone(&obs4);
                Box::pin(async move {
                    obs4.lock().unwrap().push(opts.and_then(|o| o.telemetry_context));
                    Ok(())
                })
            })),
        })),
        images: None,
        classifiers: None,
    })
    .unwrap();

    provider
        .stream(
            &model,
            &ctx,
            Some(pi_ai::types::ApiStreamOptions {
                base: StreamOptions {
                    telemetry_context: Some(Arc::clone(&telemetry)),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .result()
        .await;
    provider
        .stream_simple(
            &model,
            &ctx,
            Some(SimpleStreamOptions {
                base: StreamOptions {
                    telemetry_context: Some(Arc::clone(&telemetry)),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .result()
        .await;
    provider
        .fetch_deferred(
            &model,
            &handle,
            Some(DeferredFetchOptions {
                base: ProviderRequestOptions {
                    telemetry_context: Some(Arc::clone(&telemetry)),
                    ..Default::default()
                },
                wait: None,
            }),
        )
        .unwrap()
        .result()
        .await;
    provider
        .cancel_deferred(
            &model,
            &handle,
            Some(DeferredCancelOptions {
                telemetry_context: Some(Arc::clone(&telemetry)),
                ..Default::default()
            }),
        )
        .unwrap()
        .await
        .unwrap();

    let models = create_models(None);
    models.set_provider(provider);

    models
        .stream(
            &model,
            &Context {
                system_prompt: None,
                messages: Vec::new(),
                tools: None,
            },
            Some(ModelsApiStreamOptions {
                base: pi_ai::types::ApiStreamOptions {
                    base: StreamOptions {
                        telemetry_context: Some(Arc::clone(&telemetry)),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .result()
        .await;
    models
        .stream_simple(
            &model,
            &Context {
                system_prompt: None,
                messages: Vec::new(),
                tools: None,
            },
            Some(ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        telemetry_context: Some(Arc::clone(&telemetry)),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .result()
        .await;
    models
        .fetch_deferred(
            &model,
            &handle,
            Some(ModelsDeferredFetchOptions {
                base: DeferredFetchOptions {
                    base: ProviderRequestOptions {
                        telemetry_context: Some(Arc::clone(&telemetry)),
                        ..Default::default()
                    },
                    wait: None,
                },
                transform_headers: None,
            }),
        )
        .await;
    models
        .cancel_deferred(
            &model,
            &handle,
            Some(ModelsDeferredCancelOptions {
                base: DeferredCancelOptions {
                    telemetry_context: Some(Arc::clone(&telemetry)),
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .await
        .unwrap();

    let list = observed.lock().unwrap();
    assert_eq!(list.len(), 8);
    assert!(
        list.iter()
            .all(|v| v.as_ref().is_some_and(|t| Arc::ptr_eq(t, &telemetry)))
    );
}

#[tokio::test]
#[serial]
async fn survives_direct_and_models_image_dispatch() {
    clear_images_api_providers();
    let telemetry: TelemetryContextHandle = Arc::new(NOOP_TELEMETRY_CONTEXT);
    let image_model = test_image_model();
    let images_ctx = ImagesContext {
        input: vec![UserContentBlock::Text(TextContent {
            text: "circle".to_string(),
            text_signature: None,
        })],
    };

    let observed: Arc<Mutex<Vec<Option<TelemetryContextHandle>>>> = Arc::new(Mutex::new(Vec::new()));
    let obs1 = Arc::clone(&observed);
    let obs2 = Arc::clone(&observed);

    register_images_api_provider(
        image_model.api.clone(),
        Arc::new(move |req, _ctx, opts| {
            let obs1 = Arc::clone(&obs1);
            let req = req.clone();
            Box::pin(async move {
                obs1.lock().unwrap().push(opts.and_then(|o| o.base.telemetry_context));
                AssistantImages {
                    api: req.api,
                    provider: req.provider,
                    model: req.id,
                    output: Vec::new(),
                    response_id: None,
                    usage: None,
                    stop_reason: ImagesStopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                }
            })
        }),
        None,
    );

    generate_images(
        &image_model,
        &images_ctx,
        Some(
            ImagesOptions {
                base: ProviderRequestOptions {
                    telemetry_context: Some(Arc::clone(&telemetry)),
                    ..Default::default()
                },
                ..Default::default()
            }
            .into(),
        ),
    )
    .await
    .unwrap();

    let mut images_map = IndexMap::new();
    images_map.insert(
        image_model.api.clone(),
        ProviderImages {
            generate_images: Arc::new(move |req, _ctx, opts| {
                let obs2 = Arc::clone(&obs2);
                let req = req.clone();
                Box::pin(async move {
                    obs2.lock().unwrap().push(opts.and_then(|o| o.base.telemetry_context));
                    AssistantImages {
                        api: req.api,
                        provider: req.provider,
                        model: req.id,
                        output: Vec::new(),
                        response_id: None,
                        usage: None,
                        stop_reason: ImagesStopReason::Stop,
                        error_message: None,
                        timestamp: 0,
                    }
                })
            }),
        },
    );

    let models = create_models(None);
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: image_model.provider.to_string(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: Some(Arc::new(TelemetryAuth)),
                oauth: None,
            },
            models: vec![AnyModel::Image(image_model.clone())],
            fetch_models: None,
            filter_models: None,
            filter_all_models: None,
            api: None,
            images: Some(images_map),
            classifiers: None,
        })
        .unwrap(),
    );

    models
        .generate_images(
            &image_model,
            &images_ctx,
            Some(ModelsImagesOptions {
                base: ImagesOptions {
                    base: ProviderRequestOptions {
                        telemetry_context: Some(Arc::clone(&telemetry)),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .await;

    let list = observed.lock().unwrap();
    assert_eq!(list.len(), 2);
    assert!(
        list.iter()
            .all(|v| v.as_ref().is_some_and(|t| Arc::ptr_eq(t, &telemetry)))
    );
    clear_images_api_providers();
}

#[test]
fn telemetry_span_handle_survives_simple_stream_conversion() {
    let span: TelemetrySpanHandle = Arc::new(NOOP_TELEMETRY_CONTEXT);
    let simple_opts = SimpleStreamOptions {
        base: StreamOptions {
            telemetry: Some(Arc::clone(&span)),
            ..Default::default()
        },
        ..Default::default()
    };
    let model = test_model();
    let ctx = normalize_context(&Context {
        system_prompt: None,
        messages: Vec::new(),
        tools: None,
    });
    let base_opts = build_base_options(&model, &ctx, Some(&simple_opts), None);
    assert!(Arc::ptr_eq(base_opts.telemetry.as_ref().unwrap(), &span));
}
