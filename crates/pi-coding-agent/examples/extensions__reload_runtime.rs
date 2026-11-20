//! Port of packages/coding-agent/examples/extensions/reload-runtime.ts

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use pi_agent_core::types::AgentToolResult;
use pi_ai::types::{TextContent, UserContentBlock};
use pi_coding_agent::core::extensions::types::{
    AgentSettledEvent, CustomMessageContent, Extension, ExtensionApi, ExtensionFactory, InlineExtension,
    RegisteredCommandOptions, SendUserMessageOptions, ToolDefinition,
};
use pi_schema::t;

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

        // LLM-callable tool. Tools do not receive ExtensionCommandContext, so they cannot call
        // ctx.reload(), and Pi cannot reload while the agent is running. The tool records the
        // request and ends the run; the agent_settled handler then runs the command above.
        let reload_requested = Arc::new(AtomicBool::new(false));
        let reload_requested_tool = Arc::clone(&reload_requested);
        pi.register_tool(ToolDefinition {
            name: "reload_runtime".to_string(),
            label: "Reload Runtime".to_string(),
            description: "Reload extensions, skills, prompts, themes, and context files".to_string(),
            prompt_snippet: None,
            prompt_guidelines: None,
            parameters: t::object([] as [(&str, pi_schema::Schema); 0]).into(),
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
                let req = Arc::clone(&reload_requested_tool);
                Box::pin(async move {
                    req.store(true, Ordering::SeqCst);
                    Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new(
                            "Reload requested. Pi reloads after this response.",
                        ))],
                        details: Some(serde_json::json!({})),
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: Some(true),
                        control: None,
                    })
                })
            }),
            render_call: None,
            render_result: None,
        })?;

        let pi_settled = pi.clone();
        pi.on::<AgentSettledEvent>(move |_ev, _ctx| {
            let req = Arc::clone(&reload_requested);
            let pi_settled = pi_settled.clone();
            Box::pin(async move {
                if !req.swap(false, Ordering::SeqCst) {
                    return Ok(None);
                }
                let _ = pi_settled.send_user_message(
                    CustomMessageContent::Text("/reload-runtime".to_string()),
                    Some(SendUserMessageOptions {
                        deliver_as: None,
                        expand_prompt_templates: Some(true),
                    }),
                );
                Ok(None)
            })
        });

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(ReloadRuntimeExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
