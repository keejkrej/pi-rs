//! Port of packages/coding-agent/test/virtual-models.test.ts
//!
//! Covers `get_branch_selection`, `find_latest_response`, `get_virtual_model_state`,
//! `ModelRuntime` virtual-model routing & stream behavior, and `create_agent_session` restore.

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex};

use pi_agent_core::types::AgentMessage;
use pi_ai::compat::get_supported_thinking_levels;
use pi_ai::models::ModelsSimpleStreamOptions;
use pi_ai::models_store::InMemoryModelsStore;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxModelDefinition, FauxProviderHandle,
    FauxProviderState, FauxResponseStep, RegisterFauxProviderOptions, faux_assistant_message, faux_provider,
};
use pi_ai::types::{
    AssistantMessage, Context, Message, Model, ModelInput, ModelThinkingLevel, SimpleStreamOptions, StopReason,
    ThinkingLevel, UserMessage, UserMessageContent,
};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime, ResolveModelOptions};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::virtual_models::{
    ModelRoute, ModelRouteReason, ModelRouteRequest, VIRTUAL_MODEL_API, VirtualModelDefinition, VirtualModelRouter,
    get_branch_selection,
};

struct FnRouter<F>(F);

#[async_trait::async_trait]
impl<F> VirtualModelRouter for FnRouter<F>
where
    F: Fn(ModelRouteRequest) -> pi_js::Result<ModelRoute> + Send + Sync,
{
    async fn route(&self, request: ModelRouteRequest) -> pi_js::Result<ModelRoute> {
        (self.0)(request)
    }
}

async fn create_runtime_with_requests(
    requests: Arc<Mutex<Vec<ModelRouteRequest>>>,
) -> (ModelRuntime, FauxProviderHandle, Model) {
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        models_path: Some(None),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    let faux = faux_provider(Some(RegisterFauxProviderOptions {
        models: Some(vec![
            FauxModelDefinition {
                id: "small".to_owned(),
                name: None,
                reasoning: None,
                input: Some(vec![ModelInput::Text]),
                input_limits: None,
                cost: None,
                context_window: Some(1000),
                max_tokens: Some(100),
            },
            FauxModelDefinition {
                id: "large".to_owned(),
                name: None,
                reasoning: Some(true),
                input: Some(vec![ModelInput::Text, ModelInput::Image]),
                input_limits: None,
                cost: None,
                context_window: Some(50_000),
                max_tokens: Some(5000),
            },
        ]),
        ..Default::default()
    }));
    runtime.register_native_provider(faux.provider()).unwrap();

    let rt_for_route = runtime.clone();
    let req_clone = requests.clone();
    runtime
        .register_virtual_model(VirtualModelDefinition {
            provider: "router".to_owned(),
            id: "auto".to_owned(),
            name: "Auto".to_owned(),
            thinking_levels: Some(vec![ModelThinkingLevel::Low, ModelThinkingLevel::High]),
            context_window: None,
            max_tokens: None,
            input: None,
            router: Arc::new(FnRouter(move |req: ModelRouteRequest| {
                let target_id = if req.thinking_level == ModelThinkingLevel::High {
                    "large"
                } else {
                    "small"
                };
                req_clone.lock().unwrap().push(req);
                let model = rt_for_route.get_model("faux", target_id).unwrap();
                Ok(ModelRoute {
                    model,
                    thinking_level: ModelThinkingLevel::High,
                    state: None,
                })
            })),
        })
        .unwrap();

    let virtual_model = runtime.get_model("router", "auto").unwrap();
    runtime.refresh(None).await.unwrap();
    (runtime, faux, virtual_model)
}

fn assistant_from(model: &Model, text: &str, opts: Option<FauxAssistantMessageOptions>) -> AssistantMessage {
    let mut msg = faux_assistant_message(FauxAssistantMessageContent::Text(text.to_owned()), opts);
    msg.api = model.api.clone();
    msg.provider = model.provider.clone();
    msg.model = model.id.clone();
    msg
}

fn user_message(text: &str, timestamp: i64) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_owned()),
        timestamp,
    })
}

fn select_with_lookups(
    runtime: &ModelRuntime,
    build: impl FnOnce(&SessionManager),
) -> (Option<(String, String)>, Vec<String>) {
    let session_manager = SessionManager::in_memory(None, None, None).unwrap();
    build(&session_manager);
    let lookups = Arc::new(Mutex::new(Vec::new()));
    let lookups_ref = lookups.clone();
    let branch = session_manager.get_branch(None);
    let selection = get_branch_selection(&branch, |provider, model_id| {
        lookups_ref.lock().unwrap().push(format!("{provider}/{model_id}"));
        runtime.get_model(provider, model_id)
    });
    let taken = lookups.lock().unwrap().clone();
    (selection, taken)
}

#[tokio::test]
async fn get_branch_selection_looks_up_only_the_last_model_change() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, _faux, virtual_model) = create_runtime_with_requests(requests).await;
    let small = runtime.get_model("faux", "small").unwrap();
    let large = runtime.get_model("faux", "large").unwrap();

    let (phys_sel, phys_lookups) = select_with_lookups(&runtime, |sm| {
        sm.append_model_change(&small.provider, &small.id).unwrap();
        for _ in 0..100 {
            sm.append_message(AgentMessage::Message(Message::Assistant(assistant_from(
                &large, "ok", None,
            ))))
            .unwrap();
        }
    });
    assert_eq!(phys_sel, Some(("faux".to_owned(), "large".to_owned())));
    assert_eq!(phys_lookups, vec!["faux/small"]);

    let (routed_sel, routed_lookups) = select_with_lookups(&runtime, |sm| {
        sm.append_model_change(&small.provider, &small.id).unwrap();
        sm.append_message(AgentMessage::Message(Message::Assistant(assistant_from(
            &small, "ok", None,
        ))))
        .unwrap();
        sm.append_model_change(&virtual_model.provider, &virtual_model.id)
            .unwrap();
        for _ in 0..100 {
            sm.append_message(AgentMessage::Message(Message::Assistant(assistant_from(
                &large, "ok", None,
            ))))
            .unwrap();
        }
    });
    assert_eq!(routed_sel, Some(("router".to_owned(), "auto".to_owned())));
    assert_eq!(routed_lookups, vec!["router/auto"]);
}

#[tokio::test]
async fn get_branch_selection_uses_last_model_change_without_responses_after_it() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, _faux, virtual_model) = create_runtime_with_requests(requests).await;
    let small = runtime.get_model("faux", "small").unwrap();

    let (selection, lookups) = select_with_lookups(&runtime, |sm| {
        sm.append_model_change(&virtual_model.provider, &virtual_model.id)
            .unwrap();
        sm.append_message(AgentMessage::Message(Message::Assistant(assistant_from(
            &small, "ok", None,
        ))))
        .unwrap();
        sm.append_model_change(&small.provider, &small.id).unwrap();
    });
    assert_eq!(selection, Some(("faux".to_owned(), "small".to_owned())));
    assert!(lookups.is_empty());
}

#[tokio::test]
async fn get_branch_selection_uses_latest_physical_response_without_model_change() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, _faux, virtual_model) = create_runtime_with_requests(requests).await;
    let small = runtime.get_model("faux", "small").unwrap();
    let large = runtime.get_model("faux", "large").unwrap();

    let (selection, lookups) = select_with_lookups(&runtime, |sm| {
        sm.append_message(AgentMessage::Message(Message::Assistant(assistant_from(
            &small, "ok", None,
        ))))
        .unwrap();
        sm.append_message(AgentMessage::Message(Message::Assistant(assistant_from(
            &large, "ok", None,
        ))))
        .unwrap();
        sm.append_message(AgentMessage::Message(Message::Assistant(assistant_from(
            &virtual_model,
            "",
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                ..Default::default()
            }),
        ))))
        .unwrap();
    });
    assert_eq!(selection, Some(("faux".to_owned(), "large".to_owned())));
    assert!(lookups.is_empty());
}

#[tokio::test]
async fn lists_virtual_model_and_routes_with_clamped_thinking_level() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, _faux, virtual_model) = create_runtime_with_requests(requests.clone()).await;

    assert_eq!(virtual_model.provider, "router");
    assert_eq!(virtual_model.id, "auto");
    assert_eq!(virtual_model.api, VIRTUAL_MODEL_API);
    assert_eq!(virtual_model.context_window, 0);
    assert_eq!(virtual_model.max_tokens, 0);
    assert_eq!(virtual_model.input, vec![ModelInput::Text, ModelInput::Image]);
    assert_eq!(
        get_supported_thinking_levels(&virtual_model),
        vec![ModelThinkingLevel::Low, ModelThinkingLevel::High]
    );

    let large = runtime.get_model("faux", "large").unwrap();
    let mut prev_large = assistant_from(&large, "answer", None);
    prev_large.thinking_level = Some(ModelThinkingLevel::Medium);
    let messages = vec![
        user_message("first", 1),
        Message::Assistant(prev_large),
        user_message("second", 2),
    ];

    let low = runtime
        .resolve_model(
            &virtual_model,
            &messages,
            ResolveModelOptions {
                reason: ModelRouteReason::User,
                thinking_level: ModelThinkingLevel::Low,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(low.model.id, "small");
    assert_eq!(low.thinking_level, ModelThinkingLevel::Off);

    {
        let recorded = requests.lock().unwrap();
        let prev = recorded[0].previous.as_ref().unwrap();
        assert_eq!(prev.model.id, "large");
        assert_eq!(prev.thinking_level, Some(ModelThinkingLevel::Medium));
    }

    let high = runtime
        .resolve_model(
            &virtual_model,
            &messages,
            ResolveModelOptions {
                reason: ModelRouteReason::User,
                thinking_level: ModelThinkingLevel::High,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(high.model.id, "large");
    assert_eq!(high.thinking_level, ModelThinkingLevel::High);
}

#[tokio::test]
async fn reports_failed_request_of_retry_separately_from_latest_successful_response() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, _faux, virtual_model) = create_runtime_with_requests(requests.clone()).await;
    let small = runtime.get_model("faux", "small").unwrap();
    let large = runtime.get_model("faux", "large").unwrap();

    let mut failed = assistant_from(
        &large,
        "",
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("overloaded_error".to_owned()),
            ..Default::default()
        }),
    );
    failed.thinking_level = Some(ModelThinkingLevel::High);

    let messages = vec![
        user_message("first", 1),
        Message::Assistant(assistant_from(&small, "answer", None)),
        user_message("second", 2),
    ];

    runtime
        .resolve_model(
            &virtual_model,
            &messages,
            ResolveModelOptions {
                reason: ModelRouteReason::Retry,
                thinking_level: ModelThinkingLevel::Low,
                failed: Some(failed.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let failed_route = assistant_from(
        &virtual_model,
        "",
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            ..Default::default()
        }),
    );
    runtime
        .resolve_model(
            &virtual_model,
            &messages,
            ResolveModelOptions {
                reason: ModelRouteReason::Retry,
                thinking_level: ModelThinkingLevel::Low,
                failed: Some(failed_route),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let recorded = requests.lock().unwrap();
    assert_eq!(recorded[0].previous.as_ref().unwrap().model.id, "small");
    let first_failed = recorded[0].failed.as_ref().unwrap();
    assert_eq!(first_failed.model.id, "large");
    assert_eq!(first_failed.thinking_level, Some(ModelThinkingLevel::High));
    assert_eq!(first_failed.message.error_message.as_deref(), Some("overloaded_error"));
    assert!(recorded[1].failed.is_none());
}

#[tokio::test]
async fn lists_several_virtual_models_under_provider_with_physical_models() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, _faux, _) = create_runtime_with_requests(requests).await;
    let rt_route = runtime.clone();
    let route_small = Arc::new(FnRouter(move |_req| {
        Ok(ModelRoute {
            model: rt_route.get_model("faux", "small").unwrap(),
            thinking_level: ModelThinkingLevel::Off,
            state: None,
        })
    }));

    runtime
        .register_virtual_model(VirtualModelDefinition {
            provider: "faux".to_owned(),
            id: "auto".to_owned(),
            name: "Auto".to_owned(),
            thinking_levels: Some(vec![ModelThinkingLevel::Low, ModelThinkingLevel::High]),
            context_window: None,
            max_tokens: None,
            input: None,
            router: route_small.clone(),
        })
        .unwrap();
    runtime
        .register_virtual_model(VirtualModelDefinition {
            provider: "faux".to_owned(),
            id: "fast".to_owned(),
            name: "Fast".to_owned(),
            thinking_levels: None,
            context_window: None,
            max_tokens: None,
            input: None,
            router: route_small.clone(),
        })
        .unwrap();
    runtime
        .register_virtual_model(VirtualModelDefinition {
            provider: "router".to_owned(),
            id: "second".to_owned(),
            name: "Second".to_owned(),
            thinking_levels: None,
            context_window: None,
            max_tokens: None,
            input: None,
            router: route_small.clone(),
        })
        .unwrap();

    assert_eq!(
        runtime
            .get_models(Some("faux"))
            .into_iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        vec!["small", "large", "auto", "fast"]
    );
    assert_eq!(
        runtime
            .get_models(Some("router"))
            .into_iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        vec!["auto", "second"]
    );

    let available = runtime.get_available(None, None).await.unwrap();
    assert_eq!(
        available
            .into_iter()
            .filter(|m| m.provider == "faux")
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        vec!["small", "large", "auto", "fast"]
    );

    let fast = runtime.get_model("faux", "fast").unwrap();
    let resolved = runtime
        .resolve_model(
            &fast,
            &[],
            ResolveModelOptions {
                reason: ModelRouteReason::User,
                thinking_level: ModelThinkingLevel::Off,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(resolved.model.provider, "faux");
    assert_eq!(resolved.model.id, "small");

    let conflict_err = runtime
        .register_virtual_model(VirtualModelDefinition {
            provider: "faux".to_owned(),
            id: "large".to_owned(),
            name: "Conflict".to_owned(),
            thinking_levels: None,
            context_window: None,
            max_tokens: None,
            input: None,
            router: route_small,
        })
        .unwrap_err();
    assert!(conflict_err.to_string().contains("conflicts with a physical model"));

    runtime.unregister_virtual_model("faux", "fast");
    runtime.unregister_virtual_model("router", "auto");
    assert_eq!(
        runtime
            .get_models(Some("faux"))
            .into_iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        vec!["small", "large", "auto"]
    );
    assert_eq!(
        runtime
            .get_models(Some("router"))
            .into_iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        vec!["second"]
    );
}

#[tokio::test]
async fn rejects_routes_to_virtual_or_unknown_models() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, _faux, virtual_model) = create_runtime_with_requests(requests).await;
    let mut unknown = virtual_model.clone();
    unknown.provider = "faux".to_owned();
    unknown.id = "missing".to_owned();

    for bad_target in [virtual_model.clone(), unknown] {
        runtime
            .register_virtual_model(VirtualModelDefinition {
                provider: "router".to_owned(),
                id: "auto".to_owned(),
                name: "Auto".to_owned(),
                thinking_levels: Some(vec![ModelThinkingLevel::Low, ModelThinkingLevel::High]),
                context_window: None,
                max_tokens: None,
                input: None,
                router: Arc::new(FnRouter(move |_| {
                    Ok(ModelRoute {
                        model: bad_target.clone(),
                        thinking_level: ModelThinkingLevel::Off,
                        state: None,
                    })
                })),
            })
            .unwrap();
        let err = runtime
            .resolve_model(
                &virtual_model,
                &[],
                ResolveModelOptions {
                    reason: ModelRouteReason::User,
                    thinking_level: ModelThinkingLevel::Low,
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("which is not a physical model"));
    }
}

#[tokio::test]
async fn routes_direct_stream_simple_calls_within_routed_model_limits() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, faux, virtual_model) = create_runtime_with_requests(requests.clone()).await;
    let seen_max_tokens = Arc::new(Mutex::new(None));
    let mt_clone = seen_max_tokens.clone();

    faux.set_responses(vec![FauxResponseStep::Factory(Arc::new(
        move |_ctx, opts, _state: FauxProviderState, _model| {
            *mt_clone.lock().unwrap() = opts.as_ref().and_then(|o| o.base.max_tokens);
            Box::pin(async move {
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("hello".to_owned()),
                    None,
                ))
            })
        },
    ))]);

    let message = runtime
        .complete_simple(
            &virtual_model,
            &Context {
                system_prompt: None,
                messages: vec![user_message("hi", 1)],
                tools: None,
            },
            Some(ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    reasoning: Some(ThinkingLevel::High),
                    base: pi_ai::types::StreamOptions {
                        max_tokens: Some(20_000),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .await;

    let reasons: Vec<_> = requests.lock().unwrap().iter().map(|r| r.reason).collect();
    assert_eq!(reasons, vec![ModelRouteReason::Direct]);
    assert_eq!(message.provider, "faux");
    assert_eq!(message.model, "large");
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(*seen_max_tokens.lock().unwrap(), Some(5000));
}

#[tokio::test]
async fn does_not_forward_caller_credentials_to_routed_model_of_another_provider() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, faux, virtual_model) = create_runtime_with_requests(requests).await;
    let seen: Arc<Mutex<Vec<(Option<String>, Option<String>)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = seen.clone();

    let step = FauxResponseStep::Factory(Arc::new(move |_ctx, opts, _state: FauxProviderState, _model| {
        let api_key = opts.as_ref().and_then(|o| o.base.api_key.clone());
        let caller_hdr = opts
            .as_ref()
            .and_then(|o| o.base.headers.as_ref())
            .and_then(|h| h.get("x-caller").cloned().flatten());
        seen_clone.lock().unwrap().push((api_key, caller_hdr));
        Box::pin(async move {
            Ok(faux_assistant_message(
                FauxAssistantMessageContent::Text("hello".to_owned()),
                None,
            ))
        })
    }));
    faux.set_responses(vec![step.clone(), step]);

    let rt_route = runtime.clone();
    runtime
        .register_virtual_model(VirtualModelDefinition {
            provider: "faux".to_owned(),
            id: "auto".to_owned(),
            name: "Auto".to_owned(),
            thinking_levels: Some(vec![ModelThinkingLevel::Low, ModelThinkingLevel::High]),
            context_window: None,
            max_tokens: None,
            input: None,
            router: Arc::new(FnRouter(move |req: ModelRouteRequest| {
                let target_id = if req.thinking_level == ModelThinkingLevel::High {
                    "large"
                } else {
                    "small"
                };
                Ok(ModelRoute {
                    model: rt_route.get_model("faux", target_id).unwrap(),
                    thinking_level: ModelThinkingLevel::High,
                    state: None,
                })
            })),
        })
        .unwrap();

    let context = Context {
        system_prompt: None,
        messages: vec![user_message("hi", 1)],
        tools: None,
    };
    let mut headers = indexmap::IndexMap::new();
    headers.insert("x-caller".to_owned(), Some("1".to_owned()));
    let options = ModelsSimpleStreamOptions {
        base: SimpleStreamOptions {
            base: pi_ai::types::StreamOptions {
                api_key: Some("caller-key".to_owned()),
                headers: Some(headers),
                ..Default::default()
            },
            ..Default::default()
        },
        transform_headers: None,
    };

    runtime
        .complete_simple(&virtual_model, &context, Some(options.clone()))
        .await;
    let same_provider_virtual = runtime.get_model("faux", "auto").unwrap();
    runtime
        .complete_simple(&same_provider_virtual, &context, Some(options))
        .await;

    let recorded = seen.lock().unwrap();
    assert_ne!(recorded[0].0.as_deref(), Some("caller-key"));
    assert!(recorded[0].1.is_none());
    assert_eq!(recorded[1].0.as_deref(), Some("caller-key"));
    assert_eq!(recorded[1].1.as_deref(), Some("1"));
}

#[tokio::test]
async fn fails_unrouted_stream_calls_on_virtual_models() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, _faux, virtual_model) = create_runtime_with_requests(requests).await;

    let message = runtime
        .complete(
            &virtual_model,
            &Context {
                system_prompt: None,
                messages: vec![user_message("hi", 1)],
                tools: None,
            },
            None,
        )
        .await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(
        message
            .error_message
            .as_deref()
            .unwrap_or("")
            .contains("must be routed before streaming")
    );
}

#[tokio::test]
async fn create_agent_session_restores_persisted_virtual_model_and_thinking_level() {
    use pi_coding_agent::core::extensions::types::{ExtensionApi, ExtensionVirtualModel, InlineExtension};
    use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
    use pi_coding_agent::core::settings_manager::SettingsManager;
    use support::suite::harness::HarnessOptions;
    use support::suite::harness::create_harness;

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![InlineExtension::Factory(Arc::new(
            |pi: ExtensionApi| async move {
                pi.register_virtual_model(ExtensionVirtualModel {
                    provider: "router".to_owned(),
                    id: "auto".to_owned(),
                    name: "Auto".to_owned(),
                    thinking_levels: Some(vec![ModelThinkingLevel::Low, ModelThinkingLevel::High]),
                    route: Arc::new(|req, ctx| {
                        Box::pin(async move {
                            let m = ctx
                                .model_registry()?
                                .get_available()
                                .into_iter()
                                .find(|m| m.provider.starts_with("faux"))
                                .ok_or_else(|| pi_js::Error::msg("missing target faux model"))?;
                            Ok(ModelRoute {
                                model: m,
                                thinking_level: req.thinking_level,
                                state: None,
                            })
                        })
                    }),
                })?;
                Ok(())
            },
        ))]),
        ..Default::default()
    })
    .await;

    let sm = harness.session_manager.clone();
    let vmodel = harness.session.model_runtime().get_model("router", "auto").unwrap();
    sm.append_model_change(&vmodel.provider, &vmodel.id).unwrap();
    sm.append_thinking_level_change("high").unwrap();
    sm.append_message(AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text("persisted".to_owned()),
        timestamp: 1,
    })))
    .unwrap();

    let restored = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(harness.temp_dir.clone()),
        agent_dir: Some(harness.temp_dir.clone()),
        session_manager: Some(sm),
        settings_manager: Some(SettingsManager::in_memory(None, None)),
        model_runtime: Some(harness.session.model_runtime().clone()),
        ..Default::default()
    }))
    .await
    .unwrap();
    assert_eq!(
        restored.session.model().map(|m| (m.provider, m.id)),
        Some(("router".to_owned(), "auto".to_owned()))
    );
    assert_eq!(restored.session.thinking_level(), ModelThinkingLevel::High);
    restored.session.dispose();
    harness.cleanup();
}
