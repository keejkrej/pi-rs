//! Port of packages/coding-agent/examples/sdk/13-session-runtime.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use pi_coding_agent::config::get_agent_dir;
use pi_coding_agent::core::agent_session::{AgentSession, AgentSessionEvent};
use pi_coding_agent::core::agent_session_runtime::{
    CreateAgentSessionRuntimeFactory, CreateAgentSessionRuntimeOptions, CreateAgentSessionRuntimeResult,
    create_agent_session_runtime,
};
use pi_coding_agent::core::agent_session_services::{
    CreateAgentSessionFromServicesOptions, CreateAgentSessionServicesOptions, create_agent_session_from_services,
    create_agent_session_services,
};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_js::Unsubscribe;

async fn run() -> pi_coding_agent::Result<()> {
    let create_runtime: CreateAgentSessionRuntimeFactory = Arc::new(|opts| {
        Box::pin(async move {
            let services = create_agent_session_services(CreateAgentSessionServicesOptions {
                cwd: opts.cwd,
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
            let diagnostics = services.diagnostics.clone();
            Ok(CreateAgentSessionRuntimeResult::from_session_result(
                created,
                services,
                diagnostics,
            ))
        })
    });

    let cwd = pi_js::env::cwd();
    let runtime = create_agent_session_runtime(
        create_runtime,
        CreateAgentSessionRuntimeOptions {
            cwd: cwd.clone(),
            agent_dir: get_agent_dir(),
            session_manager: SessionManager::create(&cwd, None, None)?,
            session_start_event: None,
        },
    )
    .await?;

    let unsub: Arc<Mutex<Option<Unsubscribe>>> = Arc::new(Mutex::new(None));

    async fn bind_session(
        runtime: &pi_coding_agent::core::agent_session_runtime::AgentSessionRuntime,
        unsub: &Arc<Mutex<Option<Unsubscribe>>>,
    ) -> pi_coding_agent::Result<AgentSession> {
        if let Some(prev) = unsub.lock().unwrap_or_else(|e| e.into_inner()).take() {
            prev.call();
        }
        let session = runtime.session();
        session.bind_extensions(Default::default()).await?;
        let next_unsub = session.subscribe(|event| {
            if let AgentSessionEvent::QueueUpdate { steering, follow_up } = event {
                pi_js::console::log(&format!("Queued: {}", steering.len() + follow_up.len()));
            }
        });
        *unsub.lock().unwrap_or_else(|e| e.into_inner()) = Some(next_unsub);
        Ok(session)
    }

    let mut session = bind_session(&runtime, &unsub).await?;
    let original_session_file = session.session_file();
    pi_js::console::log(&format!(
        "Initial session: {}",
        original_session_file.as_deref().unwrap_or("(none)")
    ));

    runtime.new_session(None).await?;
    session = bind_session(&runtime, &unsub).await?;
    pi_js::console::log(&format!(
        "After newSession(): {}",
        session.session_file().as_deref().unwrap_or("(none)")
    ));

    if let Some(ref orig) = original_session_file {
        runtime.switch_session(orig, None).await?;
        session = bind_session(&runtime, &unsub).await?;
        pi_js::console::log(&format!(
            "After switchSession(): {}",
            session.session_file().as_deref().unwrap_or("(none)")
        ));
    }

    if let Some(prev) = unsub.lock().unwrap_or_else(|e| e.into_inner()).take() {
        prev.call();
    }
    runtime.dispose().await;
    Ok(())
}

fn main() -> ExitCode {
    pi_js::ensure_crypto_provider();
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            pi_js::console::error(&format!("Failed to build runtime: {err}"));
            return ExitCode::from(1);
        }
    };
    match rt.block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            pi_js::console::error(&format!("Error: {err}"));
            ExitCode::from(1)
        }
    }
}
