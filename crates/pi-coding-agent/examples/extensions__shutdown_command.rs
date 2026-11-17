//! Port of packages/coding-agent/examples/extensions/shutdown-command.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::types::AgentToolResult;
use pi_ai::types::{TextContent, UserContentBlock};
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, RegisteredCommandOptions, ToolDefinition,
};
use pi_js::vendor::typebox::t;

pub struct ShutdownCommandExtension;

#[async_trait]
impl Extension for ShutdownCommandExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.register_command(
            "quit",
            RegisteredCommandOptions {
                description: Some("Exit pi cleanly".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        ctx.as_context().shutdown()?;
                        Ok(())
                    })
                }),
            },
        )?;

        pi.register_tool(ToolDefinition {
            name: "finish_and_exit".to_string(),
            label: "Finish and Exit".to_string(),
            description: "Complete a task and exit pi".to_string(),
            prompt_snippet: None,
            prompt_guidelines: None,
            parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]),
            constrained_sampling: None,
            render_shell: None,
            prepare_arguments: None,
            output_schema: None,
            exposure: None,
            namespace: None,
            annotations: None,
            default_active: None,
            prepare_loadout: None,
            execution_mode: None,
            execute: Arc::new(|_tool_call_id, _params, _signal, _on_update, ctx| {
                Box::pin(async move {
                    ctx.base.shutdown()?;
                    Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new(
                            "Shutdown requested. Exiting after this response.",
                        ))],
                        details: Some(serde_json::json!({})),
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: None,
                        control: None,
                    })
                })
            }),
            render_call: None,
            render_result: None,
        })?;

        pi.register_tool(ToolDefinition {
            name: "deploy_and_exit".to_string(),
            label: "Deploy and Exit".to_string(),
            description: "Deploy the application and exit pi".to_string(),
            prompt_snippet: None,
            prompt_guidelines: None,
            parameters: t::object([(
                "environment",
                t::string().with("description", "Target environment (e.g., production, staging)"),
            )]),
            constrained_sampling: None,
            render_shell: None,
            prepare_arguments: None,
            output_schema: None,
            exposure: None,
            namespace: None,
            annotations: None,
            default_active: None,
            prepare_loadout: None,
            execution_mode: None,
            execute: Arc::new(|_tool_call_id, params, _signal, on_update, ctx| {
                Box::pin(async move {
                    let env = params.get("environment").and_then(|v| v.as_str()).unwrap_or("");
                    if let Some(ref upd) = on_update {
                        upd(AgentToolResult {
                            content: vec![UserContentBlock::Text(TextContent::new(format!(
                                "Deploying to {env}..."
                            )))],
                            details: Some(serde_json::json!({})),
                            structured_content: None,
                            usage: None,
                            is_error: None,
                            terminate: None,
                            control: None,
                        });
                        upd(AgentToolResult {
                            content: vec![UserContentBlock::Text(TextContent::new(
                                "Deployment complete, exiting...",
                            ))],
                            details: Some(serde_json::json!({})),
                            structured_content: None,
                            usage: None,
                            is_error: None,
                            terminate: None,
                            control: None,
                        });
                    }
                    ctx.base.shutdown()?;
                    Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new("Done! Shutdown requested."))],
                        details: Some(serde_json::json!({ "environment": env })),
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: None,
                        control: None,
                    })
                })
            }),
            render_call: None,
            render_result: None,
        })?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(ShutdownCommandExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
