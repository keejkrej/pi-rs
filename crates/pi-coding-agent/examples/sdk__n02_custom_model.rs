//! Port of packages/coding-agent/examples/sdk/02-custom-model.ts

use std::process::ExitCode;

use pi_ai::types::{AssistantMessageEvent, ModelThinkingLevel};
use pi_coding_agent::core::agent_session::AgentSessionEvent;
use pi_coding_agent::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};

async fn run() -> pi_coding_agent::Result<()> {
    let model_runtime = ModelRuntime::create(ModelRuntimeCreateOptions::default()).await?;

    if let Some(opus) = model_runtime.get_model("anthropic", "claude-opus-4-5") {
        pi_js::console::log(&format!("Found model: {}/{}", opus.provider, opus.id));
    }

    if let Some(custom_model) = model_runtime.get_model("my-provider", "my-model") {
        pi_js::console::log(&format!(
            "Found custom model: {}/{}",
            custom_model.provider, custom_model.id
        ));
    }

    let available = model_runtime.get_available(None, None).await?;
    let labels: Vec<String> = available.iter().map(|m| format!("{}/{}", m.provider, m.id)).collect();
    pi_js::console::log(&format!("Available models: {}", labels.join(", ")));

    if let Some(first) = available.into_iter().next() {
        let created = create_agent_session(Some(CreateAgentSessionOptions {
            model: Some(first),
            thinking_level: Some(ModelThinkingLevel::Medium),
            model_runtime: Some(model_runtime),
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

        let res = session.prompt("Say hello in one sentence.", None).await;
        pi_js::console::log("");
        session.dispose();
        res?;
    }

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
