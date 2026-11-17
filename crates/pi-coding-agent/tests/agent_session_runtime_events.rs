//! Port of packages/coding-agent/test/agent-session-runtime-events.test.ts

use std::sync::{Arc, Mutex, PoisonError};

use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxResponseStep, faux_assistant_message, register_faux_provider,
};
use pi_ai::{ApiKeyCredential, Credential, CredentialStore};
use pi_coding_agent::core::agent_session::ExtensionBindings;
use pi_coding_agent::core::agent_session_runtime::{
    CreateAgentSessionFromServicesOptions, CreateAgentSessionRuntimeFactory, CreateAgentSessionRuntimeOptions,
    CreateAgentSessionRuntimeResult, RuntimeForkOptions, create_agent_session_from_services,
    create_agent_session_runtime, create_agent_session_services,
};
use pi_coding_agent::core::agent_session_services::CreateAgentSessionServicesOptions;
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, ForkPosition, InlineExtension, ProviderChatModelConfig, ProviderConfig, ProviderModelConfig,
    SessionBeforeForkEvent, SessionBeforeForkResult, SessionBeforeSwitchEvent, SessionBeforeSwitchReason,
    SessionBeforeSwitchResult, SessionShutdownEvent, SessionShutdownReason, SessionStartEvent, SessionStartReason,
};
use pi_coding_agent::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
use pi_coding_agent::core::resource_loader::DefaultResourceLoaderOptions;
use pi_coding_agent::core::session_manager::SessionManager;
use pretty_assertions::assert_eq;

#[derive(Clone, Debug, PartialEq, Eq)]
enum RecordedSessionEvent {
    BeforeSwitch {
        reason: SessionBeforeSwitchReason,
        target_session_file: Option<String>,
    },
    BeforeFork {
        entry_id: String,
        position: ForkPosition,
    },
    Shutdown {
        reason: SessionShutdownReason,
        target_session_file: Option<String>,
    },
    Start {
        reason: SessionStartReason,
        previous_session_file: Option<String>,
    },
}

async fn create_runtime_host(
    extension_factory: InlineExtension,
) -> (
    pi_coding_agent::core::agent_session_runtime::AgentSessionRuntime,
    pi_ai::providers::faux::FauxProviderRegistration,
    String,
) {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-runtime-events-"])).unwrap();

    let faux = register_faux_provider(None);
    faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("one".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("two".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("three".to_string()),
            None,
        )),
    ]);

    let auth_storage = Arc::new(AuthStorage::in_memory(None));
    let model = faux.get_model();
    auth_storage
        .modify(
            &model.provider,
            Box::new(|_prev| {
                Box::pin(async move {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("faux-key".to_string()),
                        env: None,
                    })))
                })
            }),
            None,
        )
        .await
        .unwrap();

    let model_runtime = ModelRuntime::create(ModelRuntimeCreateOptions {
        credentials: Some(auth_storage),
        models_path: Some(pi_js::path::join(&[&temp_dir, "models.json"])),
        ..Default::default()
    })
    .await
    .unwrap();

    let models = faux
        .models()
        .iter()
        .map(|m| {
            ProviderModelConfig::Chat(ProviderChatModelConfig {
                id: m.id.clone(),
                name: m.name.clone(),
                api: Some(m.api.clone()),
                base_url: Some(m.base_url.clone()),
                input: m.input.clone(),
                input_limits: m.input_limits.clone(),
                cost: m.cost.clone(),
                headers: None,
                reasoning: m.reasoning,
                thinking_level_map: m.thinking_level_map.clone(),
                prompt_cache: m.prompt_cache.clone(),
                context_window: m.context_window,
                max_tokens: m.max_tokens,
                sampling_params: m.sampling_params.clone(),
                compat: m.compat.clone(),
            })
        })
        .collect();
    model_runtime.register_provider(
        &model.provider,
        ProviderConfig {
            base_url: Some(model.base_url.clone()),
            api: Some(model.api.clone()),
            models: Some(models),
            ..Default::default()
        },
        None,
    );

    let mr_for_factory = model_runtime.clone();
    let model_for_factory = model.clone();
    let ext_for_factory = extension_factory;
    let create_runtime: CreateAgentSessionRuntimeFactory = Arc::new(move |opts| {
        let mr = mr_for_factory.clone();
        let mdl = model_for_factory.clone();
        let ext = ext_for_factory.clone();
        Box::pin(async move {
            let services = create_agent_session_services(CreateAgentSessionServicesOptions {
                cwd: opts.cwd,
                agent_dir: Some(opts.agent_dir),
                model_runtime: Some(mr),
                resource_loader_options: Some(DefaultResourceLoaderOptions {
                    extension_factories: Some(vec![ext]),
                    no_skills: Some(true),
                    no_prompt_templates: Some(true),
                    no_themes: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await?;

            let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
                services: services.clone(),
                session_manager: opts.session_manager,
                session_start_event: opts.session_start_event,
                model: Some(mdl),
                thinking_level: None,
                scoped_models: None,
                tools: None,
                exclude_tools: None,
                no_tools: None,
                custom_tools: None,
            })
            .await?;
            let diagnostics = services.diagnostics.clone();
            Ok(CreateAgentSessionRuntimeResult::from_session_result(
                created,
                services,
                diagnostics,
            ))
        })
    });

    let runtime_host = create_agent_session_runtime(
        create_runtime,
        CreateAgentSessionRuntimeOptions {
            cwd: temp_dir.clone(),
            agent_dir: temp_dir.clone(),
            session_manager: SessionManager::create(&temp_dir, None, None).unwrap(),
            session_start_event: None,
        },
    )
    .await
    .unwrap();
    runtime_host
        .session()
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();

    (runtime_host, faux, temp_dir)
}

#[tokio::test]
async fn emits_session_before_switch_and_session_start_for_new_and_resume_flows() {
    let events: Arc<Mutex<Vec<RecordedSessionEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let ev_sw = Arc::clone(&events);
    let ev_sh = Arc::clone(&events);
    let ev_st = Arc::clone(&events);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let e1 = Arc::clone(&ev_sw);
        let e2 = Arc::clone(&ev_sh);
        let e3 = Arc::clone(&ev_st);
        async move {
            pi.on::<SessionBeforeSwitchEvent>(move |event, _ctx| {
                let list = Arc::clone(&e1);
                Box::pin(async move {
                    list.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(RecordedSessionEvent::BeforeSwitch {
                            reason: event.reason,
                            target_session_file: event.target_session_file,
                        });
                    Ok(None)
                })
            });
            pi.on::<SessionShutdownEvent>(move |event, _ctx| {
                let list = Arc::clone(&e2);
                Box::pin(async move {
                    list.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(RecordedSessionEvent::Shutdown {
                            reason: event.reason,
                            target_session_file: event.target_session_file,
                        });
                    Ok(None)
                })
            });
            pi.on::<SessionStartEvent>(move |event, _ctx| {
                let list = Arc::clone(&e3);
                Box::pin(async move {
                    list.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(RecordedSessionEvent::Start {
                            reason: event.reason,
                            previous_session_file: event.previous_session_file,
                        });
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let (runtime_host, faux, temp_dir) = create_runtime_host(ext).await;
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![RecordedSessionEvent::Start {
            reason: SessionStartReason::Startup,
            previous_session_file: None,
        }]
    );
    events.lock().unwrap_or_else(PoisonError::into_inner).clear();

    runtime_host.session().prompt("hello", None).await.unwrap();
    let original_session_file = runtime_host.session().session_file().unwrap();

    let new_res = runtime_host.new_session(None).await.unwrap();
    assert!(!new_res.cancelled);
    runtime_host
        .session()
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();
    let second_session_file = runtime_host.session().session_file().unwrap();
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            RecordedSessionEvent::BeforeSwitch {
                reason: SessionBeforeSwitchReason::New,
                target_session_file: None,
            },
            RecordedSessionEvent::Shutdown {
                reason: SessionShutdownReason::New,
                target_session_file: Some(second_session_file.clone()),
            },
            RecordedSessionEvent::Start {
                reason: SessionStartReason::New,
                previous_session_file: Some(original_session_file.clone()),
            },
        ]
    );
    events.lock().unwrap_or_else(PoisonError::into_inner).clear();

    let switch_res = runtime_host.switch_session(&original_session_file, None).await.unwrap();
    assert!(!switch_res.cancelled);
    runtime_host
        .session()
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            RecordedSessionEvent::BeforeSwitch {
                reason: SessionBeforeSwitchReason::Resume,
                target_session_file: Some(original_session_file.clone()),
            },
            RecordedSessionEvent::Shutdown {
                reason: SessionShutdownReason::Resume,
                target_session_file: Some(original_session_file),
            },
            RecordedSessionEvent::Start {
                reason: SessionStartReason::Resume,
                previous_session_file: Some(second_session_file),
            },
        ]
    );

    runtime_host.dispose().await;
    faux.unregister();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn honors_session_before_switch_cancellation() {
    let events: Arc<Mutex<Vec<RecordedSessionEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let ev_sw = Arc::clone(&events);
    let ev_st = Arc::clone(&events);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let e1 = Arc::clone(&ev_sw);
        let e2 = Arc::clone(&ev_st);
        async move {
            pi.on::<SessionBeforeSwitchEvent>(move |event, _ctx| {
                let list = Arc::clone(&e1);
                Box::pin(async move {
                    list.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(RecordedSessionEvent::BeforeSwitch {
                            reason: event.reason,
                            target_session_file: event.target_session_file,
                        });
                    Ok(Some(SessionBeforeSwitchResult { cancel: Some(true) }))
                })
            });
            pi.on::<SessionStartEvent>(move |event, _ctx| {
                let list = Arc::clone(&e2);
                Box::pin(async move {
                    list.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(RecordedSessionEvent::Start {
                            reason: event.reason,
                            previous_session_file: event.previous_session_file,
                        });
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let (runtime_host, faux, temp_dir) = create_runtime_host(ext).await;
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![RecordedSessionEvent::Start {
            reason: SessionStartReason::Startup,
            previous_session_file: None,
        }]
    );
    events.lock().unwrap_or_else(PoisonError::into_inner).clear();

    runtime_host.session().prompt("hello", None).await.unwrap();
    let original_session_file = runtime_host.session().session_file();

    let result = runtime_host.new_session(None).await.unwrap();
    assert!(result.cancelled);
    assert_eq!(runtime_host.session().session_file(), original_session_file);
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![RecordedSessionEvent::BeforeSwitch {
            reason: SessionBeforeSwitchReason::New,
            target_session_file: None,
        }]
    );

    runtime_host.dispose().await;
    faux.unregister();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn runs_before_session_invalidate_after_session_shutdown_and_before_rebind_session() {
    let phases: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let phases_sh = Arc::clone(&phases);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let p = Arc::clone(&phases_sh);
        async move {
            pi.on::<SessionShutdownEvent>(move |_ev, _ctx| {
                let p_inner = Arc::clone(&p);
                Box::pin(async move {
                    p_inner
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push("session_shutdown".to_string());
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let (runtime_host, faux, temp_dir) = create_runtime_host(ext).await;
    let old_session = runtime_host.session();

    let phases_inv = Arc::clone(&phases);
    let old_sess_inv = old_session.clone();
    runtime_host.set_before_session_invalidate(Some(Arc::new(move || {
        phases_inv
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push("beforeSessionInvalidate".to_string());
        let runner = old_sess_inv.extension_runner().unwrap();
        let ctx = runner.create_context().unwrap();
        assert_eq!(ctx.cwd(), old_sess_inv.session_manager().get_cwd());
    })));

    let phases_reb = Arc::clone(&phases);
    runtime_host.set_rebind_session(Some(Arc::new(move |_new_sess| {
        let p_inner = Arc::clone(&phases_reb);
        Box::pin(async move {
            p_inner
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push("rebindSession".to_string());
            Ok(())
        })
    })));

    runtime_host.new_session(None).await.unwrap();

    assert_eq!(
        *phases.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            "session_shutdown".to_string(),
            "beforeSessionInvalidate".to_string(),
            "rebindSession".to_string(),
        ]
    );
    let err = old_session.extension_runner().unwrap().create_context().unwrap_err();
    assert_eq!(
        err.to_string(),
        "This extension ctx is stale after session replacement or reload. Do not use a captured pi or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload()."
    );

    runtime_host.set_before_session_invalidate(None);
    runtime_host.set_rebind_session(None);
    runtime_host.dispose().await;
    faux.unregister();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn emits_session_before_fork_and_session_start_and_honors_cancellation() {
    let events: Arc<Mutex<Vec<RecordedSessionEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let cancel_next = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ev_fk = Arc::clone(&events);
    let ev_sh = Arc::clone(&events);
    let ev_st = Arc::clone(&events);
    let cn_ref = Arc::clone(&cancel_next);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let e1 = Arc::clone(&ev_fk);
        let e2 = Arc::clone(&ev_sh);
        let e3 = Arc::clone(&ev_st);
        let cn = Arc::clone(&cn_ref);
        async move {
            pi.on::<SessionBeforeForkEvent>(move |event, _ctx| {
                let list = Arc::clone(&e1);
                let cn_inner = Arc::clone(&cn);
                Box::pin(async move {
                    list.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(RecordedSessionEvent::BeforeFork {
                            entry_id: event.entry_id,
                            position: event.position,
                        });
                    if cn_inner.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        return Ok(Some(SessionBeforeForkResult {
                            cancel: Some(true),
                            skip_conversation_restore: None,
                        }));
                    }
                    Ok(None)
                })
            });
            pi.on::<SessionShutdownEvent>(move |event, _ctx| {
                let list = Arc::clone(&e2);
                Box::pin(async move {
                    list.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(RecordedSessionEvent::Shutdown {
                            reason: event.reason,
                            target_session_file: event.target_session_file,
                        });
                    Ok(None)
                })
            });
            pi.on::<SessionStartEvent>(move |event, _ctx| {
                let list = Arc::clone(&e3);
                Box::pin(async move {
                    list.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(RecordedSessionEvent::Start {
                            reason: event.reason,
                            previous_session_file: event.previous_session_file,
                        });
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let (runtime_host, faux, temp_dir) = create_runtime_host(ext).await;
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![RecordedSessionEvent::Start {
            reason: SessionStartReason::Startup,
            previous_session_file: None,
        }]
    );
    events.lock().unwrap_or_else(PoisonError::into_inner).clear();

    runtime_host.session().prompt("hello", None).await.unwrap();
    let user_msg = runtime_host.session().get_user_messages_for_forking()[0].clone();
    let previous_session_file = runtime_host.session().session_file();

    let success_result = runtime_host.fork(&user_msg.entry_id, None).await.unwrap();
    assert!(!success_result.cancelled);
    assert_eq!(success_result.selected_text.as_deref(), Some("hello"));
    runtime_host
        .session()
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            RecordedSessionEvent::BeforeFork {
                entry_id: user_msg.entry_id.clone(),
                position: ForkPosition::Before,
            },
            RecordedSessionEvent::Shutdown {
                reason: SessionShutdownReason::Fork,
                target_session_file: runtime_host.session().session_file(),
            },
            RecordedSessionEvent::Start {
                reason: SessionStartReason::Fork,
                previous_session_file,
            },
        ]
    );

    events.lock().unwrap_or_else(PoisonError::into_inner).clear();
    cancel_next.store(true, std::sync::atomic::Ordering::SeqCst);
    let cancel_result = runtime_host.fork(&user_msg.entry_id, None).await.unwrap();
    assert!(cancel_result.cancelled);
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![RecordedSessionEvent::BeforeFork {
            entry_id: user_msg.entry_id,
            position: ForkPosition::Before,
        }]
    );

    events.lock().unwrap_or_else(PoisonError::into_inner).clear();
    cancel_next.store(true, std::sync::atomic::Ordering::SeqCst);
    let cancel_at_result = runtime_host
        .fork(
            "missing-entry",
            Some(RuntimeForkOptions {
                position: Some(ForkPosition::At),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert!(cancel_at_result.cancelled);
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![RecordedSessionEvent::BeforeFork {
            entry_id: "missing-entry".to_string(),
            position: ForkPosition::At,
        }]
    );

    runtime_host.dispose().await;
    faux.unregister();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}
