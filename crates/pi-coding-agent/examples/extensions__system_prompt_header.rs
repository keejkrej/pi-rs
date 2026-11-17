//! Port of packages/coding-agent/examples/extensions/system-prompt-header.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    AgentStartEvent, Extension, ExtensionApi, ExtensionFactory, InlineExtension, SessionShutdownEvent,
};

pub struct SystemPromptHeaderExtension;

#[async_trait]
impl Extension for SystemPromptHeaderExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.on::<AgentStartEvent>(|_event, ctx| {
            Box::pin(async move {
                let prompt = ctx.get_system_prompt()?;
                ctx.ui()?.set_status(
                    "system-prompt",
                    Some(&format!("System: {} chars", prompt.chars().count())),
                );
                Ok(None)
            })
        });

        pi.on::<SessionShutdownEvent>(|_event, ctx| {
            Box::pin(async move {
                ctx.ui()?.set_status("system-prompt", None);
                Ok(None)
            })
        });

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(SystemPromptHeaderExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
