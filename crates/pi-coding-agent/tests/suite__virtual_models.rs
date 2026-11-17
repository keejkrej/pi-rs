//! Port of packages/coding-agent/test/suite/virtual-models.test.ts
//!
//! Tests router state persistence via `VIRTUAL_MODEL_STATE_ENTRY` and `get_virtual_model_state`,
//! virtual-model multi-step route selection (`User`, `Retry`, `Continuation`, `Direct`),
//! and `ModelRuntime` stream/retry dispatching over `faux_provider`.
//! // WIRE(@core-session-w3): extend with full `AgentSession` / compaction harness once `core::agent_session` and `core::sdk` land.

use std::sync::{Arc, Mutex};

use pi_agent_core::types::AgentMessage;
use pi_ai::models::ModelsSimpleStreamOptions;
use pi_ai::models_store::InMemoryModelsStore;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxModelDefinition, FauxResponseStep,
    RegisterFauxProviderOptions, faux_assistant_message, faux_provider,
};
use pi_ai::types::{
    AssistantMessage, Context, Message, Model, ModelInput, ModelThinkingLevel, SimpleStreamOptions, StopReason,
    ThinkingLevel, UserMessage, UserMessageContent,
};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime, ResolveModelOptions};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::virtual_models::{
    ModelRoute, ModelRouteReason, ModelRouteRequest, VIRTUAL_MODEL_STATE_ENTRY, VirtualModelDefinition,
    VirtualModelRouter, VirtualModelStateData, find_latest_response, get_branch_selection, get_virtual_model_state,
};
use serde_json::json;

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

fn assistant_from(
    model: &Model,
    text: &str,
    thinking_level: ModelThinkingLevel,
    opts: Option<FauxAssistantMessageOptions>,
) -> AssistantMessage {
    let mut msg = faux_assistant_message(FauxAssistantMessageContent::Text(text.to_owned()), opts);
    msg.api = model.api.clone();
    msg.provider = model.provider.clone();
    msg.model = model.id.clone();
    msg.thinking_level = Some(thinking_level);
    msg
}

#[tokio::test]
async fn stores_router_state_on_branch_and_restores_across_steps() {
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
                max_tokens: None,
            },
            FauxModelDefinition {
                id: "large".to_owned(),
                name: None,
                reasoning: Some(true),
                input: Some(vec![ModelInput::Text]),
                input_limits: None,
                cost: None,
                context_window: Some(50_000),
                max_tokens: Some(4000),
            },
        ]),
        ..Default::default()
    }));
    runtime.register_native_provider(faux.provider()).unwrap();

    let seen_states = Arc::new(Mutex::new(Vec::new()));
    let states_clone = seen_states.clone();
    let rt_clone = runtime.clone();
    runtime
        .register_virtual_model(VirtualModelDefinition {
            provider: "router".to_owned(),
            id: "auto".to_owned(),
            name: "Auto".to_owned(),
            thinking_levels: Some(vec![ModelThinkingLevel::Low, ModelThinkingLevel::High]),
            context_window: Some(1000),
            max_tokens: None,
            input: None,
            router: Arc::new(FnRouter(move |req: ModelRouteRequest| {
                states_clone.lock().unwrap().push((req.reason, req.state.clone()));
                if req.reason == ModelRouteReason::Direct {
                    return Ok(ModelRoute {
                        model: rt_clone.get_model("faux", "large").unwrap(),
                        thinking_level: ModelThinkingLevel::Low,
                        state: req.state,
                    });
                }
                let turns = req
                    .state
                    .as_ref()
                    .and_then(|v| v.get("turns"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
                    + 1;
                let model_id = if turns >= 2 { "large" } else { "small" };
                let thinking = if model_id == "large" {
                    ModelThinkingLevel::High
                } else {
                    ModelThinkingLevel::Off
                };
                Ok(ModelRoute {
                    model: rt_clone.get_model("faux", model_id).unwrap(),
                    thinking_level: thinking,
                    state: Some(json!({ "turns": turns })),
                })
            })),
        })
        .unwrap();

    let virtual_model = runtime.get_model("router", "auto").unwrap();
    runtime.refresh(None).await.unwrap();
    let sm = SessionManager::in_memory(None, None, None).unwrap();
    sm.append_model_change("router", "auto").unwrap();

    // First user turn (no prior state)
    let state_before_t1 = get_virtual_model_state(&sm.get_branch(None), "router", "auto");
    let route_t1 = runtime
        .resolve_model(
            &virtual_model,
            &[],
            ResolveModelOptions {
                reason: ModelRouteReason::User,
                thinking_level: ModelThinkingLevel::High,
                state: state_before_t1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(route_t1.model.id, "small");
    let payload_t1 = serde_json::to_value(VirtualModelStateData {
        provider: "router".to_owned(),
        model_id: "auto".to_owned(),
        state: route_t1.state.unwrap(),
    })
    .unwrap();
    sm.append_custom_entry(VIRTUAL_MODEL_STATE_ENTRY, Some(payload_t1))
        .unwrap();

    // Second user turn reads `{ turns: 1 }` and advances to `{ turns: 2 }` on large
    let state_before_t2 = get_virtual_model_state(&sm.get_branch(None), "router", "auto");
    assert_eq!(state_before_t2, Some(json!({ "turns": 1 })));
    let route_t2 = runtime
        .resolve_model(
            &virtual_model,
            &[],
            ResolveModelOptions {
                reason: ModelRouteReason::User,
                thinking_level: ModelThinkingLevel::High,
                state: state_before_t2,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(route_t2.model.id, "large");
    assert_eq!(route_t2.thinking_level, ModelThinkingLevel::High);

    // Direct call sees whatever state was passed (e.g., undefined or current) and picks large:low
    let route_direct = runtime
        .resolve_model(
            &virtual_model,
            &[],
            ResolveModelOptions {
                reason: ModelRouteReason::Direct,
                thinking_level: ModelThinkingLevel::Low,
                state: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(route_direct.model.id, "large");
    assert_eq!(route_direct.thinking_level, ModelThinkingLevel::Low);

    let recorded = seen_states.lock().unwrap();
    assert_eq!(
        *recorded,
        vec![
            (ModelRouteReason::User, None),
            (ModelRouteReason::User, Some(json!({ "turns": 1 }))),
            (ModelRouteReason::Direct, None),
        ]
    );

    // Verify find_latest_response skips non-assistant/virtual-error messages as expected
    let small = runtime.get_model("faux", "small").unwrap();
    let large = runtime.get_model("faux", "large").unwrap();
    sm.append_message(AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text("hi".to_owned()),
        timestamp: 1,
    })))
    .unwrap();
    sm.append_message(AgentMessage::Message(Message::Assistant(assistant_from(
        &small,
        "one",
        ModelThinkingLevel::Off,
        None,
    ))))
    .unwrap();
    sm.append_message(AgentMessage::Message(Message::Assistant(assistant_from(
        &large,
        "two",
        ModelThinkingLevel::High,
        None,
    ))))
    .unwrap();
    sm.append_message(AgentMessage::Message(Message::Assistant(assistant_from(
        &virtual_model,
        "",
        ModelThinkingLevel::High,
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            ..Default::default()
        }),
    ))))
    .unwrap();

    let messages: Vec<AgentMessage> = vec![
        AgentMessage::Message(Message::Assistant(assistant_from(
            &small,
            "one",
            ModelThinkingLevel::Off,
            None,
        ))),
        AgentMessage::Message(Message::Assistant(assistant_from(
            &large,
            "two",
            ModelThinkingLevel::High,
            None,
        ))),
        AgentMessage::Message(Message::Assistant(assistant_from(
            &virtual_model,
            "",
            ModelThinkingLevel::High,
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                ..Default::default()
            }),
        ))),
    ];
    let latest_phys = find_latest_response(&messages).unwrap();
    assert_eq!(latest_phys.model, "large");

    let sel = get_branch_selection(&sm.get_branch(None), |p, id| runtime.get_model(p, id));
    assert_eq!(sel, Some(("router".to_owned(), "auto".to_owned())));
}

#[tokio::test]
async fn complete_simple_respects_routed_model_limits_and_thinking() {
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
        models: Some(vec![FauxModelDefinition {
            id: "large".to_owned(),
            name: None,
            reasoning: Some(true),
            input: Some(vec![ModelInput::Text]),
            input_limits: None,
            cost: None,
            context_window: Some(50_000),
            max_tokens: Some(4000),
        }]),
        ..Default::default()
    }));
    runtime.register_native_provider(faux.provider()).unwrap();

    let seen_calls = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = seen_calls.clone();
    faux.set_responses(vec![FauxResponseStep::Factory(Arc::new(
        move |_ctx, opts, _core, model| {
            let reasoning = opts
                .as_ref()
                .and_then(|o| o.reasoning)
                .map(|r| match r {
                    ThinkingLevel::Minimal => "minimal",
                    ThinkingLevel::Low => "low",
                    ThinkingLevel::Medium => "medium",
                    ThinkingLevel::High => "high",
                    ThinkingLevel::XHigh => "xhigh",
                    ThinkingLevel::Max => "max",
                })
                .unwrap_or("off");
            let max_tokens = opts.as_ref().and_then(|o| o.base.max_tokens);
            seen_clone
                .lock()
                .unwrap()
                .push(format!("{}:{}:{}", model.id, reasoning, max_tokens.unwrap_or(0)));
            Box::pin(async move {
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("summary".to_owned()),
                    None,
                ))
            })
        },
    ))]);

    let rt = runtime.clone();
    runtime
        .register_virtual_model(VirtualModelDefinition {
            provider: "router".to_owned(),
            id: "auto".to_owned(),
            name: "Auto".to_owned(),
            thinking_levels: Some(vec![ModelThinkingLevel::Low, ModelThinkingLevel::High]),
            context_window: Some(1000),
            max_tokens: None,
            input: None,
            router: Arc::new(FnRouter(move |_| {
                Ok(ModelRoute {
                    model: rt.get_model("faux", "large").unwrap(),
                    thinking_level: ModelThinkingLevel::Low,
                    state: None,
                })
            })),
        })
        .unwrap();

    let virtual_model = runtime.get_model("router", "auto").unwrap();
    runtime.refresh(None).await.unwrap();
    let msg = runtime
        .complete_simple(
            &virtual_model,
            &Context {
                system_prompt: None,
                messages: vec![Message::User(UserMessage {
                    content: UserMessageContent::Text("summarize".to_owned()),
                    timestamp: 1,
                })],
                tools: None,
            },
            Some(ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: pi_ai::types::StreamOptions {
                        max_tokens: Some(16_000),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .await;

    assert_eq!(msg.stop_reason, StopReason::Stop);
    assert_eq!(*seen_calls.lock().unwrap(), vec!["large:low:4000"]);
}
