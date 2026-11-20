//! Port of `packages/coding-agent/test/suite/regressions/2860-replaced-session-context.test.ts`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent_core::AgentMessage;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, Usage, UserMessage, UserMessageContent, faux_text,
};
use pi_coding_agent::core::agent_session_runtime::{
    CreateAgentSessionFromServicesOptions, CreateAgentSessionRuntimeFactory, CreateAgentSessionRuntimeOptions,
    CreateAgentSessionServicesOptions, SwitchSessionOptions, create_agent_session_from_services,
    create_agent_session_runtime, create_agent_session_services,
};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::event_bus::create_event_bus;
use pi_coding_agent::core::extensions::{
    Extension, ExtensionActions, ExtensionApi, ExtensionCommandContextActions, ExtensionContextActions,
    ExtensionFactory, ExtensionRunner, ModelRegistry, create_extension_runtime, load_extension_from_factory,
};
use pi_coding_agent::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
use pi_coding_agent::core::resource_loader::DefaultResourceLoaderOptions;
use pi_coding_agent::core::session_manager::{NewSessionOptions, SessionManager};
use tempfile::TempDir;

fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_owned()),
        timestamp: 1,
    }))
}

fn assistant_msg(text: &str) -> AgentMessage {
    AgentMessage::Message(Message::Assistant(AssistantMessage {
        content: vec![AssistantContentBlock::Text(faux_text(text))],
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
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
        timestamp: 2,
        duration_ms: None,
    }))
}

#[test]
fn replacement_session_fork_and_switch_isolate_context_and_session_files() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("cwd");
    let sess_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&sess_dir).unwrap();
    let cwd_str = cwd.to_string_lossy().into_owned();
    let dir_str = sess_dir.to_string_lossy().into_owned();

    let sm = SessionManager::create(&cwd_str, Some(&dir_str), None).unwrap();
    sm.append_message(user_msg("seed")).unwrap();
    sm.append_message(assistant_msg("seed reply")).unwrap();
    let original_file = sm.get_session_file().unwrap();

    // New session with parent_session yields fresh file path and empty context
    let replaced_file = sm
        .new_session(Some(NewSessionOptions {
            id: None,
            parent_session: Some(original_file.clone()),
        }))
        .unwrap();
    assert!(replaced_file.is_some());
    assert_ne!(replaced_file.as_deref(), Some(original_file.as_str()));
    assert!(sm.build_session_context().messages.is_empty());

    sm.append_message(user_msg("Hello from the new session!")).unwrap();
    sm.append_message(assistant_msg("hello reply")).unwrap();
    assert_eq!(sm.build_session_context().messages.len(), 2);

    // Fork from original file creates a third distinct session retaining only prior branch
    let forked = SessionManager::fork_from(&original_file, &cwd_str, Some(&dir_str), None).unwrap();
    forked.append_message(user_msg("fork callback message")).unwrap();
    forked.append_message(assistant_msg("fork reply")).unwrap();
    assert_eq!(forked.build_session_context().messages.len(), 4);
    assert_ne!(forked.get_session_file(), replaced_file);

    // Switching back via open restores target session cleanly
    let switched = SessionManager::open(&original_file, Some(&dir_str), None).unwrap();
    assert_eq!(switched.build_session_context().messages.len(), 2);
}

struct ClosureFactory<F>(F);

#[async_trait]
impl<F> Extension for ClosureFactory<F>
where
    F: Fn(ExtensionApi) -> pi_js::Result<()> + Send + Sync + 'static,
{
    async fn register(&self, api: ExtensionApi) -> pi_js::Result<()> {
        (self.0)(api)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn invalidates_stale_extension_api_and_command_context_when_runner_is_replaced() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_string_lossy().into_owned();
    let rt = create_extension_runtime();
    let bus = create_event_bus();
    let captured_api: Arc<Mutex<Option<ExtensionApi>>> = Arc::new(Mutex::new(None));
    let api_slot = Arc::clone(&captured_api);

    let factory: ExtensionFactory = Arc::new(ClosureFactory(move |pi: ExtensionApi| {
        *api_slot.lock().unwrap() = Some(pi);
        Ok(())
    }));
    let ext = load_extension_from_factory(factory, &cwd, bus, rt.clone(), Some("<n2860>"))
        .await
        .expect("load extension");

    let sm = SessionManager::in_memory(Some(&cwd), None, None).unwrap();
    let runner = ExtensionRunner::new(vec![ext], rt, &cwd, sm, ModelRegistry::in_memory());
    runner.bind_core(
        ExtensionActions {
            send_message: Arc::new(|_, _| {}),
            send_user_message: Arc::new(|_, _| {}),
            append_entry: Arc::new(|_, _| {}),
            set_session_name: Arc::new(|_| {}),
            get_session_name: Arc::new(|| None),
            set_label: Arc::new(|_, _| {}),
            get_active_tools: Arc::new(Vec::new),
            get_all_tools: Arc::new(Vec::new),
            get_settings: Arc::new(Default::default),
            set_active_tools: Arc::new(|_| {}),
            refresh_tools: Arc::new(|| {}),
            get_commands: Arc::new(Vec::new),
            set_model: Arc::new(|_| Box::pin(async { Ok(false) })),
            get_thinking_level: Arc::new(|| pi_agent_core::types::ThinkingLevel::Off),
            set_thinking_level: Arc::new(|_| {}),
        },
        ExtensionContextActions {
            get_model: Arc::new(|| None),
            get_scoped_models: Arc::new(Vec::new),
            is_idle: Arc::new(|| true),
            is_project_trusted: Arc::new(|| true),
            get_signal: Arc::new(|| None),
            abort: Arc::new(|| {}),
            has_pending_messages: Arc::new(|| false),
            shutdown: Arc::new(|| {}),
            get_context_usage: Arc::new(|| None),
            compact: Arc::new(|_| {}),
            get_system_prompt: Arc::new(String::new),
            get_system_prompt_options: None,
            execute_tool: None,
            get_callable_tools: None,
        },
        None,
    );
    let _cmd_actions = ExtensionCommandContextActions {
        wait_for_idle: Arc::new(|| Box::pin(async { Ok(()) })),
        new_session: Arc::new(|_| {
            Box::pin(async { Ok(pi_coding_agent::core::extensions::CancelledResult { cancelled: false }) })
        }),
        fork: Arc::new(|_, _| {
            Box::pin(async { Ok(pi_coding_agent::core::extensions::CancelledResult { cancelled: false }) })
        }),
        navigate_tree: Arc::new(|_, _| {
            Box::pin(async { Ok(pi_coding_agent::core::extensions::CancelledResult { cancelled: false }) })
        }),
        switch_session: Arc::new(|_, _| {
            Box::pin(async { Ok(pi_coding_agent::core::extensions::CancelledResult { cancelled: false }) })
        }),
        reload: Arc::new(|| Box::pin(async { Ok(()) })),
    };

    let old_ctx = runner.create_command_context();
    let old_pi = captured_api.lock().unwrap().clone().expect("captured pi");
    assert!(old_ctx.base.cwd().is_ok());
    assert!(old_pi.get_commands().is_ok());

    runner.invalidate(None);

    let ctx_err = old_ctx.base.cwd().unwrap_err();
    let pi_err = old_pi.get_commands().unwrap_err();
    assert!(ctx_err.to_string().contains("stale"), "{ctx_err}");
    assert!(pi_err.to_string().contains("stale"), "{pi_err}");
}

#[tokio::test]
async fn create_agent_session_runtime_with_session_rebinds_without_leaking_previous_session() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("cwd");
    let agent_dir = tmp.path().join("agent");
    let sess_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::create_dir_all(&sess_dir).unwrap();
    let cwd_str = cwd.to_string_lossy().into_owned();
    let agent_str = agent_dir.to_string_lossy().into_owned();
    let sess_str = sess_dir.to_string_lossy().into_owned();

    let model_runtime = ModelRuntime::create(ModelRuntimeCreateOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    let initial_sm = SessionManager::create(&cwd_str, Some(&sess_str), None).unwrap();
    initial_sm.append_message(user_msg("seed")).unwrap();
    initial_sm.append_message(assistant_msg("reply")).unwrap();
    let other_sm = SessionManager::create(&cwd_str, Some(&sess_str), None).unwrap();
    other_sm.append_message(user_msg("other")).unwrap();
    other_sm.append_message(assistant_msg("other reply")).unwrap();
    let other_file = other_sm.get_session_file().unwrap();

    let factory: CreateAgentSessionRuntimeFactory = Arc::new(move |opts| {
        let mr = model_runtime.clone();
        Box::pin(async move {
            let services = create_agent_session_services(CreateAgentSessionServicesOptions {
                cwd: opts.cwd.clone(),
                agent_dir: Some(opts.agent_dir.clone()),
                model_runtime: Some(mr),
                resource_loader_options: Some(DefaultResourceLoaderOptions {
                    cwd: opts.cwd,
                    agent_dir: opts.agent_dir,
                    no_extensions: true,
                    no_skills: true,
                    no_prompt_templates: true,
                    no_themes: true,
                    no_context_files: true,
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await?;
            let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
                services: services.clone(),
                session_manager: opts.session_manager,
                session_start_event: opts.session_start_event,
                model: None,
                thinking_level: None,
                scoped_models: None,
                tools: None,
                exclude_tools: None,
                no_tools: None,
                custom_tools: None,
            })
            .await?;
            Ok(
                pi_coding_agent::core::agent_session_runtime::CreateAgentSessionRuntimeResult {
                    session: created.session,
                    services,
                    extensions_result: created.extensions_result,
                    diagnostics: Vec::new(),
                    model_fallback_message: created.model_fallback_message,
                },
            )
        })
    });

    let runtime = create_agent_session_runtime(
        factory,
        CreateAgentSessionRuntimeOptions {
            cwd: cwd_str,
            agent_dir: agent_str,
            session_manager: initial_sm,
            session_start_event: None,
        },
    )
    .await
    .unwrap();

    let seen_ids = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_c = Arc::clone(&seen_ids);
    let sw = runtime
        .switch_session(
            &other_file,
            Some(SwitchSessionOptions {
                with_session: Some(Box::new(move |ctx| {
                    let seen = Arc::clone(&seen_c);
                    Box::pin(async move {
                        let sid = ctx.base.base.session_manager()?.get_session_id();
                        seen.lock().unwrap().push(sid);
                        Ok(())
                    })
                })),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert!(!sw.cancelled);
    assert_eq!(*seen_ids.lock().unwrap(), vec![runtime.session().session_id()]);
    assert_eq!(runtime.session().session_file().as_deref(), Some(other_file.as_str()));
    runtime.dispose().await;
}
