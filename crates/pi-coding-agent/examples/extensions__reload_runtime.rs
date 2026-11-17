//! Port of packages/coding-agent/examples/extensions/reload-runtime.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::types::AgentToolResult;
use pi_ai::types::{TextContent, UserContentBlock};
use pi_coding_agent::core::extensions::types::{
    CustomMessageContent, Extension, ExtensionApi, ExtensionFactory, InlineExtension, InputDeliveryBehavior,
    RegisteredCommandOptions, SendUserMessageOptions, ToolDefinition,
};
use pi_js::vendor::typebox::t;

pub struct ReloadRuntimeExtension;

#[async_trait]
impl Extension for ReloadRuntimeExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.register_command(
            "reload-runtime",
            RegisteredCommandOptions {
                description: Some("Reload extensions, skills, prompts, themes, and context files".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| Box::pin(async move { ctx.reload().await })),
            },
        )?;

        let pi_tool = pi.clone();
        pi.register_tool(ToolDefinition {
            name: "reload_runtime".to_string(),
            label: "Reload Runtime".to_string(),
            description: "Reload extensions, skills, prompts, themes, and context files".to_string(),
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
            execute: Arc::new(move |_tool_call_id, _params, _signal, _on_update, _ctx| {
                let pi = pi_tool.clone();
                Box::pin(async move {
                    pi.send_user_message(
                        CustomMessageContent::Text("/reload-runtime".to_string()),
                        Some(SendUserMessageOptions {
                            deliver_as: Some(InputDeliveryBehavior::FollowUp),
                            expand_prompt_templates: None,
                        }),
                    )?;
                    Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new(
                            "Queued /reload-runtime as a follow-up command.",
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

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(ReloadRuntimeExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
