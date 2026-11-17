//! Port of packages/coding-agent/examples/extensions/bash-spawn-hook.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{Extension, ExtensionApi, ExtensionFactory, InlineExtension};
use pi_coding_agent::core::tools::tool_definition_wrapper::create_tool_definition_from_agent_tool;
use pi_coding_agent::core::tools::{BashSpawnContext, BashToolOptions, create_bash_tool};

pub struct BashSpawnHookExtension;

#[async_trait]
impl Extension for BashSpawnHookExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let cwd = pi_js::env::cwd();

        let bash_tool = create_bash_tool(
            &cwd,
            Some(BashToolOptions {
                spawn_hook: Some(Arc::new(|mut ctx: BashSpawnContext| {
                    ctx.command = format!("source ~/.profile\n{}", ctx.command);
                    ctx.env.insert("PI_SPAWN_HOOK".to_string(), "1".to_string());
                    ctx
                })),
                ..Default::default()
            }),
        );

        pi.register_tool(create_tool_definition_from_agent_tool(bash_tool))?;
        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(BashSpawnHookExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
