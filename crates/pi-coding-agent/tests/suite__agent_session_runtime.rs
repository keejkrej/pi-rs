//! Port of packages/coding-agent/test/suite/agent-session-runtime.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pi_agent_core::types::{AgentMessage, AgentToolResult};
use pi_ai::compat::register_faux_provider;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxModelDefinition,
    FauxProviderRegistration, FauxResponseStep, RegisterFauxProviderOptions, faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{
    Message, ModelThinkingLevel, StopReason, TextContent, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_ai::{ApiKeyCredential, Credential, CredentialStore};
use pi_coding_agent::core::agent_session_runtime::{
    AgentSessionRuntime, CreateAgentSessionFromServicesOptions, CreateAgentSessionRuntimeFactory,
    CreateAgentSessionRuntimeFactoryOptions, CreateAgentSessionRuntimeOptions, CreateAgentSessionRuntimeResult,
    CreateAgentSessionServicesOptions, RuntimeForkOptions, RuntimeForkResult, create_agent_session_from_services,
    create_agent_session_runtime, create_agent_session_services,
};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, ForkPosition, InlineExtension, MessageEndEvent, MessageEndEventResult, ProviderChatModelConfig,
    ProviderConfig, ProviderModelConfig, SessionBeforeForkEvent, SessionBeforeForkResult, SessionBeforeSwitchEvent,
    SessionBeforeSwitchReason, SessionBeforeSwitchResult, SessionEntry, SessionShutdownEvent, SessionShutdownReason,
    SessionStartEvent, SessionStartReason, ToolDefinition,
};
use pi_coding_agent::core::resource_loader::DefaultResourceLoaderOptions;
use pi_coding_agent::core::session_manager::SessionManager;
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::json;
use support::model_runtime_test_utils::{create_model_registry, get_model_runtime};
use tokio::sync::Notify;

#[derive(Clone, Debug, PartialEq, Eq)]
enum RecordedSessionEvent {
    BeforeSwitch(SessionBeforeSwitchEvent),
    BeforeFork(SessionBeforeForkEvent),
    Shutdown(SessionShutdownEvent),
    Start(SessionStartEvent),
}

fn provider_config_for_faux(faux: &FauxProviderRegistration) -> (String, ProviderConfig) {
    let model = faux.get_model();
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
                headers: m.headers.clone(),
                reasoning: m.reasoning,
                thinking_level_map: m.thinking_level_map.clone(),
                prompt_cache: m.prompt_cache.clone(),
                context_window: m.context_window,
                max_tokens: m.max_tokens,
                sampling_params: m.sampling_params.clone(),
                compat: m.compat.clone(),
                sampling_params_by_thinking_level: None,
            })
        })
        .collect();
    (
        model.provider.clone(),
        ProviderConfig {
            base_url: Some(model.base_url),
            api_key: Some("faux-key".to_string()),
            api: Some(faux.api().to_string()),
            models: Some(models),
            ..Default::default()
        },
    )
}

async fn make_faux_auth_storage(provider: &str) -> Arc<AuthStorage> {
    let auth_storage = Arc::new(AuthStorage::in_memory(None));
    auth_storage
        .modify(
            provider,
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
    auth_storage
}

fn build_runtime_factory(
    faux: &FauxProviderRegistration,
    auth_storage: Arc<AuthStorage>,
    agent_dir: String,
    model_opt: Option<pi_ai::Model>,
    extra_ext: Option<InlineExtension>,
) -> CreateAgentSessionRuntimeFactory {
    let (prov_name, prov_cfg) = provider_config_for_faux(faux);
    let models_json_path = pi_js::path::join(&[&agent_dir, "models.json"]);
    Arc::new(move |opts: CreateAgentSessionRuntimeFactoryOptions| {
        let auth = Arc::clone(&auth_storage);
        let p_name = prov_name.clone();
        let p_cfg = prov_cfg.clone();
        let m_path = models_json_path.clone();
        let bootstrap_model = model_opt.clone();
        let extra = extra_ext.clone();
        Box::pin(async move {
            let reg_ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
                let pn = p_name.clone();
                let pc = p_cfg.clone();
                let ex = extra.clone();
                async move {
                    pi.register_provider(&pn, pc)?;
                    if let Some(InlineExtension::Factory(factory)) = ex {
                        factory.register(pi).await?;
                    }
                    Ok(())
                }
            }));
            let cred_store: Arc<dyn CredentialStore + Send + Sync> = auth;
            let model_registry = create_model_registry(cred_store, Some(&m_path)).await?;
            let model_runtime = get_model_runtime(&model_registry);
            let services = create_agent_session_services(CreateAgentSessionServicesOptions {
                cwd: opts.cwd.clone(),
                agent_dir: Some(opts.agent_dir.clone()),
                model_runtime: Some(model_runtime),
                resource_loader_options: Some(DefaultResourceLoaderOptions {
                    cwd: opts.cwd,
                    agent_dir: opts.agent_dir,
                    extension_factories: vec![pi_coding_agent::core::resource_loader::InlineExtension::Core(reg_ext)],
                    no_skills: true,
                    no_prompt_templates: true,
                    no_themes: true,
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await?;
            let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
                services: services.clone(),
                session_manager: opts.session_manager,
                session_start_event: opts.session_start_event,
                model: bootstrap_model,
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
    })
}

async fn create_runtime_for_test(
    extra_ext: Option<InlineExtension>,
    cwd: Option<String>,
    bootstrap_model: bool,
) -> (AgentSessionRuntime, FauxProviderRegistration, String) {
    let temp_dir = cwd.unwrap_or_else(|| {
        pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-runtime-suite-"])).unwrap()
    });
    pi_js::fs::mkdir(&temp_dir, true, None).unwrap();

    let faux = register_faux_provider(Some(RegisterFauxProviderOptions {
        models: Some(vec![
            FauxModelDefinition {
                id: "faux-1".to_string(),
                name: None,
                reasoning: Some(true),
                input: None,
                input_limits: None,
                cost: None,
                context_window: None,
                max_tokens: None,
            },
            FauxModelDefinition {
                id: "faux-2".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                input_limits: None,
                cost: None,
                context_window: None,
                max_tokens: None,
            },
        ]),
        ..Default::default()
    }));
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

    let provider_id = faux.get_model().provider;
    let auth_storage = make_faux_auth_storage(&provider_id).await;
    let factory = build_runtime_factory(
        &faux,
        auth_storage,
        temp_dir.clone(),
        if bootstrap_model { Some(faux.get_model()) } else { None },
        extra_ext,
    );
    let runtime = create_agent_session_runtime(
        factory,
        CreateAgentSessionRuntimeOptions {
            cwd: temp_dir.clone(),
            agent_dir: temp_dir.clone(),
            session_manager: SessionManager::create(&temp_dir, None, None).unwrap(),
            session_start_event: None,
        },
    )
    .await
    .unwrap();
    runtime.session().bind_extensions(Default::default()).await.unwrap();
    (runtime, faux, temp_dir)
}

async fn cleanup_runtime(runtime: AgentSessionRuntime, faux: FauxProviderRegistration, temp_dir: &str) {
    runtime.dispose().await;
    faux.unregister();
    if pi_js::fs::exists(temp_dir) {
        let _ = pi_js::fs::rm(temp_dir, true, true);
    }
}

#[tokio::test]
async fn persists_message_end_assistant_replacements_to_the_session_manager() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<MessageEndEvent>(|event, _ctx| {
            Box::pin(async move {
                let AgentMessage::Message(pi_ai::Message::Assistant(mut assistant)) = event.message else {
                    return Ok(None);
                };
                assistant.usage.cost.total = 0.123;
                Ok(Some(MessageEndEventResult {
                    message: Some(AgentMessage::from(pi_ai::Message::Assistant(assistant))),
                }))
            })
        });
        Ok(())
    }));

    let (runtime, faux, temp_dir) = create_runtime_for_test(Some(ext), None, true).await;

    runtime.session().prompt("hello", None).await.unwrap();

    let session_assistant = runtime
        .session()
        .messages()
        .into_iter()
        .find_map(|m| match m {
            AgentMessage::Message(pi_ai::Message::Assistant(a)) => Some(a),
            _ => None,
        })
        .expect("missing assistant message");
    assert_eq!(session_assistant.usage.cost.total, 0.123);

    let persisted_assistant = runtime
        .session()
        .session_manager()
        .get_entries()
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::Message(me) => match me.message {
                AgentMessage::Message(pi_ai::Message::Assistant(a)) => Some(a),
                _ => None,
            },
            _ => None,
        })
        .next()
        .expect("missing persisted assistant message");
    assert_eq!(persisted_assistant.usage.cost.total, 0.123);

    cleanup_runtime(runtime, faux, &temp_dir).await;
}

#[tokio::test]
async fn settles_the_active_response_before_session_replacement() {
    let tool_started = Arc::new(Notify::new());
    let ts_c = Arc::clone(&tool_started);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let ts = Arc::clone(&ts_c);
        async move {
            pi.register_tool(ToolDefinition {
                name: "block".to_string(),
                label: "Block".to_string(),
                description: "Blocks until aborted".to_string(),
                parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]),
                prompt_snippet: None,
                prompt_guidelines: None,
                exposure: None,
                default_active: None,
                namespace: None,
                output_schema: None,
                annotations: None,
                constrained_sampling: None,
                render_shell: None,
                prepare_loadout: None,
                execution_mode: None,
                execute: Arc::new(move |_id, _params, signal, _upd, _ctx| {
                    let ts = Arc::clone(&ts);
                    Box::pin(async move {
                        let aborted = Arc::new(Notify::new());
                        let ab_c = Arc::clone(&aborted);
                        let _sub = signal.as_ref().map(|s| {
                            s.on_abort(move |_| {
                                ab_c.notify_one();
                            })
                        });
                        if signal.as_ref().is_some_and(|s| s.aborted()) {
                            aborted.notify_one();
                        }
                        ts.notify_one();
                        aborted.notified().await;
                        Ok(AgentToolResult {
                            content: vec![UserContentBlock::Text(TextContent::new("tool aborted"))],
                            details: Some(json!({})),
                            ..Default::default()
                        })
                    })
                }),
                prepare_arguments: None,
                render_call: None,
                render_result: None,
            })?;
            Ok(())
        }
    }));

    let (runtime, faux, temp_dir) = create_runtime_for_test(Some(ext), None, true).await;

    runtime.session().prompt("hello", None).await.unwrap();
    let first_session_file = runtime.session().session_file().expect("first session file");
    runtime.new_session(None).await.unwrap();
    runtime.session().bind_extensions(Default::default()).await.unwrap();

    faux.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![FauxContentBlock::ToolCall(faux_tool_call(
            "block",
            Default::default(),
            None,
        ))]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    ))]);
    let outgoing_session = runtime.session();
    let outgoing_file = outgoing_session.session_file().expect("outgoing session file");
    let ts_wait = tool_started.notified();
    let prompt_fut = outgoing_session.prompt("start blocking tool", None);
    let switch_fut = async {
        ts_wait.await;
        runtime.switch_session(&first_session_file, None).await.unwrap()
    };
    let (prompt_res, switch_result) = tokio::join!(prompt_fut, switch_fut);
    prompt_res.unwrap();

    assert!(!switch_result.cancelled);
    assert_eq!(runtime.session().session_file(), Some(first_session_file));
    let outgoing_roles: Vec<String> = SessionManager::open(&outgoing_file, None, None)
        .unwrap()
        .get_entries()
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::Message(m) => m.message.role().map(str::to_string),
            _ => None,
        })
        .collect();
    assert_eq!(
        outgoing_roles,
        vec!["system", "user", "assistant", "toolResult", "assistant"]
    );

    cleanup_runtime(runtime, faux, &temp_dir).await;
}

#[tokio::test]
async fn preserves_an_existing_session_when_importing_a_file_with_the_same_name() {
    let (runtime, faux, temp_dir) = create_runtime_for_test(None, None, true).await;
    let session_dir = runtime.session().session_manager().get_session_dir();
    let import_dir = pi_js::path::join(&[&temp_dir, "import"]);
    let filename = "collision.jsonl";
    let stored_path = pi_js::path::join(&[&session_dir, filename]);
    let import_path = pi_js::path::join(&[&import_dir, filename]);
    let now_iso = pi_js::time::iso_now();
    let stored_session = format!(
        "{}\n",
        pi_js::json::stringify(&json!({
            "type": "session",
            "version": 3,
            "id": "stored",
            "timestamp": now_iso,
            "cwd": temp_dir,
        }))
    );
    let imported_session = format!(
        "{}\n",
        pi_js::json::stringify(&json!({
            "type": "session",
            "version": 3,
            "id": "imported",
            "timestamp": now_iso,
            "cwd": temp_dir,
        }))
    );
    pi_js::fs::mkdir(&session_dir, true, None).unwrap();
    pi_js::fs::mkdir(&import_dir, true, None).unwrap();
    pi_js::fs::write_file(&stored_path, stored_session.as_bytes(), None).unwrap();
    pi_js::fs::write_file(&import_path, imported_session.as_bytes(), None).unwrap();

    runtime.import_from_jsonl(&import_path, None).await.unwrap();

    assert_eq!(pi_js::fs::read_to_string(&stored_path).unwrap(), stored_session);
    let active_file = runtime.session().session_file().expect("imported session file");
    assert_ne!(active_file, stored_path);
    assert!(
        pi_js::fs::read_to_string(&active_file)
            .unwrap()
            .contains("\"id\":\"imported\"")
    );

    cleanup_runtime(runtime, faux, &temp_dir).await;
}

#[tokio::test]
async fn emits_session_before_switch_and_session_start_for_new_and_resume_flows() {
    let events = Arc::new(Mutex::new(Vec::<RecordedSessionEvent>::new()));
    let ev_c = Arc::clone(&events);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let ev1 = Arc::clone(&ev_c);
        let ev2 = Arc::clone(&ev_c);
        let ev3 = Arc::clone(&ev_c);
        async move {
            pi.on::<SessionBeforeSwitchEvent>(move |e, _ctx| {
                let ev1 = Arc::clone(&ev1);
                Box::pin(async move {
                    ev1.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(RecordedSessionEvent::BeforeSwitch(e));
                    Ok(None)
                })
            });
            pi.on::<SessionShutdownEvent>(move |e, _ctx| {
                let ev2 = Arc::clone(&ev2);
                Box::pin(async move {
                    ev2.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(RecordedSessionEvent::Shutdown(e));
                    Ok(None)
                })
            });
            pi.on::<SessionStartEvent>(move |e, _ctx| {
                let ev3 = Arc::clone(&ev3);
                Box::pin(async move {
                    ev3.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(RecordedSessionEvent::Start(e));
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let (runtime, faux, temp_dir) = create_runtime_for_test(Some(ext), None, true).await;

    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![RecordedSessionEvent::Start(SessionStartEvent {
            reason: SessionStartReason::Startup,
            previous_session_file: None,
        })]
    );
    events.lock().unwrap_or_else(PoisonError::into_inner).clear();

    runtime.session().prompt("hello", None).await.unwrap();
    let original_session_file = runtime.session().session_file();
    let original_id = runtime.session().session_id();

    let new_res = runtime.new_session(None).await.unwrap();
    assert!(!new_res.cancelled);
    runtime.session().bind_extensions(Default::default()).await.unwrap();
    assert_ne!(runtime.session().session_id(), original_id);
    assert!(runtime.session().messages().is_empty());
    let second_session_file = runtime.session().session_file();
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            RecordedSessionEvent::BeforeSwitch(SessionBeforeSwitchEvent {
                reason: SessionBeforeSwitchReason::New,
                target_session_file: None,
            }),
            RecordedSessionEvent::Shutdown(SessionShutdownEvent {
                reason: SessionShutdownReason::New,
                target_session_file: second_session_file.clone(),
            }),
            RecordedSessionEvent::Start(SessionStartEvent {
                reason: SessionStartReason::New,
                previous_session_file: original_session_file.clone(),
            }),
        ]
    );

    events.lock().unwrap_or_else(PoisonError::into_inner).clear();
    let orig_path = original_session_file.clone().unwrap();
    let switch_res = runtime.switch_session(&orig_path, None).await.unwrap();
    assert!(!switch_res.cancelled);
    runtime.session().bind_extensions(Default::default()).await.unwrap();
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            RecordedSessionEvent::BeforeSwitch(SessionBeforeSwitchEvent {
                reason: SessionBeforeSwitchReason::Resume,
                target_session_file: original_session_file,
            }),
            RecordedSessionEvent::Shutdown(SessionShutdownEvent {
                reason: SessionShutdownReason::Resume,
                target_session_file: Some(orig_path),
            }),
            RecordedSessionEvent::Start(SessionStartEvent {
                reason: SessionStartReason::Resume,
                previous_session_file: second_session_file,
            }),
        ]
    );

    cleanup_runtime(runtime, faux, &temp_dir).await;
}

#[tokio::test]
async fn honors_session_before_switch_cancellation_for_new_and_resume() {
    let events = Arc::new(Mutex::new(Vec::<RecordedSessionEvent>::new()));
    let cancel_reason = Arc::new(Mutex::new(Option::<SessionBeforeSwitchReason>::None));

    let ev_c = Arc::clone(&events);
    let cr_c = Arc::clone(&cancel_reason);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let ev1 = Arc::clone(&ev_c);
        let ev2 = Arc::clone(&ev_c);
        let cr = Arc::clone(&cr_c);
        async move {
            pi.on::<SessionBeforeSwitchEvent>(move |e, _ctx| {
                let ev1 = Arc::clone(&ev1);
                let cr = Arc::clone(&cr);
                Box::pin(async move {
                    let matches_cancel = *cr.lock().unwrap_or_else(PoisonError::into_inner) == Some(e.reason);
                    ev1.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(RecordedSessionEvent::BeforeSwitch(e));
                    if matches_cancel {
                        Ok(Some(SessionBeforeSwitchResult { cancel: Some(true) }))
                    } else {
                        Ok(None)
                    }
                })
            });
            pi.on::<SessionStartEvent>(move |e, _ctx| {
                let ev2 = Arc::clone(&ev2);
                Box::pin(async move {
                    ev2.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(RecordedSessionEvent::Start(e));
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let (runtime, faux, temp_dir) = create_runtime_for_test(Some(ext), None, true).await;

    runtime.session().prompt("hello", None).await.unwrap();
    let original_session_file = runtime.session().session_file();

    *cancel_reason.lock().unwrap_or_else(PoisonError::into_inner) = Some(SessionBeforeSwitchReason::New);
    let new_res = runtime.new_session(None).await.unwrap();
    assert!(new_res.cancelled);
    assert_eq!(runtime.session().session_file(), original_session_file);

    let other_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-runtime-other-"])).unwrap();
    let other_session = SessionManager::create(&other_dir, None, None).unwrap();
    other_session
        .append_message(AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new("other"))]),
            timestamp: pi_js::time::now_ms(),
        })))
        .unwrap();
    let other_file = other_session.get_session_file().unwrap();

    *cancel_reason.lock().unwrap_or_else(PoisonError::into_inner) = Some(SessionBeforeSwitchReason::Resume);
    let resume_res = runtime.switch_session(&other_file, None).await.unwrap();
    assert!(resume_res.cancelled);
    assert_eq!(runtime.session().session_file(), original_session_file);

    let _ = pi_js::fs::rm(&other_dir, true, true);
    cleanup_runtime(runtime, faux, &temp_dir).await;
}

#[tokio::test]
async fn emits_session_before_fork_and_session_start_and_honors_cancellation() {
    let events = Arc::new(Mutex::new(Vec::<RecordedSessionEvent>::new()));
    let cancel_next_fork = Arc::new(AtomicBool::new(false));

    let ev_c = Arc::clone(&events);
    let cnf_c = Arc::clone(&cancel_next_fork);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let ev1 = Arc::clone(&ev_c);
        let ev2 = Arc::clone(&ev_c);
        let ev3 = Arc::clone(&ev_c);
        let cnf = Arc::clone(&cnf_c);
        async move {
            pi.on::<SessionBeforeForkEvent>(move |e, _ctx| {
                let ev1 = Arc::clone(&ev1);
                let cnf = Arc::clone(&cnf);
                Box::pin(async move {
                    ev1.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(RecordedSessionEvent::BeforeFork(e));
                    if cnf.swap(false, Ordering::SeqCst) {
                        Ok(Some(SessionBeforeForkResult {
                            cancel: Some(true),
                            ..Default::default()
                        }))
                    } else {
                        Ok(None)
                    }
                })
            });
            pi.on::<SessionShutdownEvent>(move |e, _ctx| {
                let ev2 = Arc::clone(&ev2);
                Box::pin(async move {
                    ev2.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(RecordedSessionEvent::Shutdown(e));
                    Ok(None)
                })
            });
            pi.on::<SessionStartEvent>(move |e, _ctx| {
                let ev3 = Arc::clone(&ev3);
                Box::pin(async move {
                    ev3.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(RecordedSessionEvent::Start(e));
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let (runtime, faux, temp_dir) = create_runtime_for_test(Some(ext), None, true).await;

    events.lock().unwrap_or_else(PoisonError::into_inner).clear();
    runtime.session().prompt("hello", None).await.unwrap();
    let user_entry_id = runtime.session().get_user_messages_for_forking()[0].entry_id.clone();
    let previous_session_file = runtime.session().session_file();

    let success_res = runtime.fork(&user_entry_id, None).await.unwrap();
    assert!(!success_res.cancelled);
    assert_eq!(success_res.selected_text.as_deref(), Some("hello"));
    runtime.session().bind_extensions(Default::default()).await.unwrap();
    let current_file = runtime.session().session_file();
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            RecordedSessionEvent::BeforeFork(SessionBeforeForkEvent {
                entry_id: user_entry_id.clone(),
                position: ForkPosition::Before,
            }),
            RecordedSessionEvent::Shutdown(SessionShutdownEvent {
                reason: SessionShutdownReason::Fork,
                target_session_file: current_file.clone(),
            }),
            RecordedSessionEvent::Start(SessionStartEvent {
                reason: SessionStartReason::Fork,
                previous_session_file,
            }),
        ]
    );
    let stem = std::path::Path::new(current_file.as_deref().unwrap())
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap();
    assert!(stem.ends_with(&format!("_{}", runtime.session().session_id())));

    events.lock().unwrap_or_else(PoisonError::into_inner).clear();
    cancel_next_fork.store(true, Ordering::SeqCst);
    let cancel_res = runtime.fork(&user_entry_id, None).await.unwrap();
    assert_eq!(
        cancel_res,
        RuntimeForkResult {
            cancelled: true,
            selected_text: None,
        }
    );
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![RecordedSessionEvent::BeforeFork(SessionBeforeForkEvent {
            entry_id: user_entry_id,
            position: ForkPosition::Before,
        })]
    );

    events.lock().unwrap_or_else(PoisonError::into_inner).clear();
    cancel_next_fork.store(true, Ordering::SeqCst);
    let cancel_at_res = runtime
        .fork(
            "missing-entry",
            Some(RuntimeForkOptions {
                position: Some(ForkPosition::At),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        cancel_at_res,
        RuntimeForkResult {
            cancelled: true,
            selected_text: None,
        }
    );
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![RecordedSessionEvent::BeforeFork(SessionBeforeForkEvent {
            entry_id: "missing-entry".to_string(),
            position: ForkPosition::At,
        })]
    );

    cleanup_runtime(runtime, faux, &temp_dir).await;
}

#[tokio::test]
async fn reports_why_an_unflushed_session_cannot_be_forked() {
    let (runtime, faux, temp_dir) = create_runtime_for_test(None, None, true).await;
    let session_file = runtime.session().session_file().expect("session file");
    let leaf_id = runtime.session().session_manager().get_leaf_id().expect("leaf id");
    assert!(!pi_js::fs::exists(&session_file));

    let err = runtime
        .fork(
            &leaf_id,
            Some(RuntimeForkOptions {
                position: Some(ForkPosition::At),
                ..Default::default()
            }),
        )
        .await
        .unwrap_err();
    assert!(
        err.message()
            .contains("This session has not been saved yet. Send a message before cloning or forking it.")
    );

    cleanup_runtime(runtime, faux, &temp_dir).await;
}

#[tokio::test]
async fn duplicates_the_current_active_branch_when_forking_at_the_current_position() {
    let (runtime, faux, temp_dir) = create_runtime_for_test(None, None, true).await;
    runtime.session().prompt("hello", None).await.unwrap();
    runtime.session().prompt("again", None).await.unwrap();

    let summarize = |msgs: Vec<AgentMessage>| -> Vec<(&'static str, Option<String>)> {
        msgs.into_iter()
            .map(|m| match m {
                AgentMessage::Message(pi_ai::Message::User(u)) => {
                    let txt = match u.content {
                        UserMessageContent::Text(s) => s,
                        UserMessageContent::Blocks(blocks) => blocks
                            .into_iter()
                            .filter_map(|p| match p {
                                UserContentBlock::Text(t) => Some(t.text),
                                _ => None,
                            })
                            .collect::<String>(),
                    };
                    ("user", Some(txt))
                }
                AgentMessage::Message(pi_ai::Message::Assistant(_)) => ("assistant", None),
                AgentMessage::Message(pi_ai::Message::System(_)) => ("system", None),
                _ => ("other", None),
            })
            .collect()
    };

    let before_messages = summarize(runtime.session().messages());
    let previous_session_file = runtime.session().session_file();
    let leaf_id = runtime.session().session_manager().get_leaf_id().expect("leaf id");

    let result = runtime
        .fork(
            &leaf_id,
            Some(RuntimeForkOptions {
                position: Some(ForkPosition::At),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        result,
        RuntimeForkResult {
            cancelled: false,
            selected_text: None,
        }
    );
    assert_ne!(runtime.session().session_file(), previous_session_file);
    assert_eq!(summarize(runtime.session().messages()), before_messages);

    cleanup_runtime(runtime, faux, &temp_dir).await;
}

#[tokio::test]
async fn duplicates_the_current_active_branch_in_memory_when_forking_at_the_current_position() {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-runtime-inmem-"])).unwrap();
    let faux = register_faux_provider(Some(RegisterFauxProviderOptions {
        models: Some(vec![
            FauxModelDefinition {
                id: "faux-1".to_string(),
                name: None,
                reasoning: Some(true),
                input: None,
                input_limits: None,
                cost: None,
                context_window: None,
                max_tokens: None,
            },
            FauxModelDefinition {
                id: "faux-2".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                input_limits: None,
                cost: None,
                context_window: None,
                max_tokens: None,
            },
        ]),
        ..Default::default()
    }));
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

    let auth_storage = make_faux_auth_storage(&faux.get_model().provider).await;
    let factory = build_runtime_factory(&faux, auth_storage, temp_dir.clone(), Some(faux.get_model()), None);
    let runtime = create_agent_session_runtime(
        factory,
        CreateAgentSessionRuntimeOptions {
            cwd: temp_dir.clone(),
            agent_dir: temp_dir.clone(),
            session_manager: SessionManager::in_memory(Some(&temp_dir), None, None).unwrap(),
            session_start_event: None,
        },
    )
    .await
    .unwrap();
    runtime.session().bind_extensions(Default::default()).await.unwrap();

    runtime.session().prompt("hello", None).await.unwrap();
    runtime.session().prompt("again", None).await.unwrap();

    let before_count = runtime.session().messages().len();
    let leaf_id = runtime.session().session_manager().get_leaf_id().expect("leaf id");
    assert_eq!(runtime.session().session_file(), None);

    let result = runtime
        .fork(
            &leaf_id,
            Some(RuntimeForkOptions {
                position: Some(ForkPosition::At),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        result,
        RuntimeForkResult {
            cancelled: false,
            selected_text: None,
        }
    );
    assert_eq!(runtime.session().session_file(), None);
    assert_eq!(runtime.session().messages().len(), before_count);

    cleanup_runtime(runtime, faux, &temp_dir).await;
}

#[tokio::test]
async fn throws_when_forking_with_an_invalid_entry_id() {
    let (runtime, faux, temp_dir) = create_runtime_for_test(None, None, true).await;
    let err = runtime.fork("missing-entry", None).await.unwrap_err();
    assert!(err.message().contains("Invalid entry ID for forking"));
    cleanup_runtime(runtime, faux, &temp_dir).await;
}

#[tokio::test]
async fn updates_the_runtime_session_cwd_on_cross_cwd_session_replacement() {
    let first_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-runtime-cwd-a-"])).unwrap();
    let second_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-runtime-cwd-b-"])).unwrap();
    let (runtime, faux, temp_dir) = create_runtime_for_test(None, Some(first_dir.clone()), true).await;

    let other_auth = make_faux_auth_storage(&faux.get_model().provider).await;
    let other_factory = build_runtime_factory(&faux, other_auth, temp_dir.clone(), None, None);
    let other_runtime = create_agent_session_runtime(
        other_factory,
        CreateAgentSessionRuntimeOptions {
            cwd: second_dir.clone(),
            agent_dir: temp_dir.clone(),
            session_manager: SessionManager::create(&second_dir, None, None).unwrap(),
            session_start_event: None,
        },
    )
    .await
    .unwrap();
    other_runtime.session().prompt("other", None).await.unwrap();
    let other_session_file = other_runtime.session().session_file().expect("other session file");

    runtime.switch_session(&other_session_file, None).await.unwrap();

    let canonical_second = std::fs::canonicalize(&second_dir).unwrap();
    assert_eq!(
        std::fs::canonicalize(runtime.session().session_manager().get_cwd()).unwrap(),
        canonical_second
    );
    assert_eq!(std::fs::canonicalize(runtime.cwd()).unwrap(), canonical_second);

    other_runtime.dispose().await;
    cleanup_runtime(runtime, faux, &temp_dir).await;
    let _ = pi_js::fs::rm(&second_dir, true, true);
}

#[tokio::test]
async fn restores_model_and_thinking_state_from_the_destination_session() {
    let (runtime, faux, temp_dir) = create_runtime_for_test(None, None, false).await;
    let other_dir = pi_js::path::join(&[&temp_dir, "other"]);
    pi_js::fs::mkdir(&other_dir, true, None).unwrap();

    let other_auth = make_faux_auth_storage(&faux.get_model().provider).await;
    let other_factory = build_runtime_factory(&faux, other_auth, temp_dir.clone(), None, None);
    let other_runtime = create_agent_session_runtime(
        other_factory,
        CreateAgentSessionRuntimeOptions {
            cwd: other_dir.clone(),
            agent_dir: temp_dir.clone(),
            session_manager: SessionManager::create(&other_dir, None, None).unwrap(),
            session_start_event: None,
        },
    )
    .await
    .unwrap();
    let faux_2 = faux.get_model_model_id("faux-2").expect("faux-2 model");
    other_runtime.session().set_model(faux_2, None).await.unwrap();
    other_runtime
        .session()
        .set_thinking_level(ModelThinkingLevel::Off, None);
    other_runtime.session().prompt("hello", None).await.unwrap();
    let target_session_file = other_runtime.session().session_file().expect("target file");

    runtime.switch_session(&target_session_file, None).await.unwrap();

    assert_eq!(runtime.session().model().map(|m| m.id), Some("faux-2".to_string()));
    assert_eq!(runtime.session().thinking_level(), ModelThinkingLevel::Off);

    other_runtime.dispose().await;
    cleanup_runtime(runtime, faux, &temp_dir).await;
}
