//! Port of packages/coding-agent/examples/sdk/06-extensions.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_ai::types::AssistantMessageEvent;
use pi_coding_agent::config::get_agent_dir;
use pi_coding_agent::core::agent_session::AgentSessionEvent;
use pi_coding_agent::core::extensions::types::{AgentStartEvent, Extension, ExtensionApi, InlineExtension};
use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;

struct InlineLoggingExtension;

#[async_trait]
impl Extension for InlineLoggingExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_coding_agent::Result<()> {
        pi.on::<AgentStartEvent>(|_event, _ctx| {
            Box::pin(async move {
                pi_js::console::log("[Inline Extension] Agent starting");
                Ok(None)
            })
        });
        Ok(())
    }
}

async fn run() -> pi_coding_agent::Result<()> {
    let loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: pi_js::env::cwd(),
        agent_dir: get_agent_dir(),
        additional_extension_paths: vec![
            "./my-logging-extension.ts".to_string(),
            "./my-safety-extension.ts".to_string(),
        ],
        extension_factories: vec![InlineExtension::Factory(Arc::new(InlineLoggingExtension)).into()],
        ..Default::default()
    }));
    loader.reload(None).await?;

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        resource_loader: Some(loader),
        session_manager: Some(SessionManager::in_memory(None, None, None)?),
        ..Default::default()
    }))
    .await?;
    let session = created.session;

    let _unsub = session.subscribe(|event| {
        if let AgentSessionEvent::MessageUpdate {
            assistant_message_event: AssistantMessageEvent::TextDelta { delta, .. },
            ..
        } = event
        {
            pi_js::console::stdout_write(&delta);
        }
    });

    let res = session.prompt("List files in the current directory.", None).await;
    pi_js::console::log("");
    session.dispose();
    res
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
