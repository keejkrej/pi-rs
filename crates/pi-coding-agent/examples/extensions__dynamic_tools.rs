//! Port of packages/coding-agent/examples/extensions/dynamic-tools.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexSet;
use pi_agent_core::types::AgentToolResult;
use pi_ai::types::{TextContent, UserContentBlock};
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, NotifyType, RegisteredCommandOptions,
    SessionStartEvent, ToolDefinition,
};
use pi_js::vendor::typebox::t;

fn normalize_tool_name(input: &str) -> Option<String> {
    let trimmed = input.trim().to_lowercase();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    Some(trimmed)
}

fn register_echo_tool(
    pi: &ExtensionApi,
    registered: &Mutex<IndexSet<String>>,
    name: &str,
    label: &str,
    prefix: &str,
) -> pi_js::Result<bool> {
    {
        let mut guard = registered.lock().unwrap_or_else(|e| e.into_inner());
        if guard.contains(name) {
            return Ok(false);
        }
        guard.insert(name.to_string());
    }

    let tool_name = name.to_string();
    let prefix_owned = prefix.to_string();
    let params_schema = t::object([("message", t::string().with("description", "Message to echo"))]);

    pi.register_tool(ToolDefinition {
        name: name.to_string(),
        label: label.to_string(),
        description: format!("Echo a message with prefix: {prefix}"),
        prompt_snippet: Some(format!("Echo back user-provided text with {} prefix", prefix.trim())),
        prompt_guidelines: Some(vec![
            "Use echo_session when the user asks for exact echo output.".to_string(),
        ]),
        parameters: params_schema,
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
        execute: Arc::new(move |_tool_call_id, params, _signal, _on_update, _ctx| {
            let tool = tool_name.clone();
            let prefix = prefix_owned.clone();
            Box::pin(async move {
                let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(format!("{prefix}{message}")))],
                    details: Some(serde_json::json!({
                        "tool": tool,
                        "prefix": prefix,
                    })),
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

    Ok(true)
}

pub struct DynamicToolsExtension;

#[async_trait]
impl Extension for DynamicToolsExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let registered = Arc::new(Mutex::new(IndexSet::new()));

        {
            let pi_ss = pi.clone();
            let reg_ss = Arc::clone(&registered);
            pi.on::<SessionStartEvent>(Arc::new(move |_event, ctx| {
                let pi = pi_ss.clone();
                let reg = Arc::clone(&reg_ss);
                Box::pin(async move {
                    register_echo_tool(&pi, &reg, "echo_session", "Echo Session", "[session] ")?;
                    ctx.ui()?
                        .notify("Registered dynamic tool: echo_session", Some(NotifyType::Info));
                    Ok(None)
                })
            }));
        }

        {
            let pi_cmd = pi.clone();
            let reg_cmd = Arc::clone(&registered);
            pi.register_command(
                "add-echo-tool",
                RegisteredCommandOptions {
                    description: Some("Register a new echo tool dynamically: /add-echo-tool <tool_name>".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |args, ctx| {
                        let pi = pi_cmd.clone();
                        let reg = Arc::clone(&reg_cmd);
                        Box::pin(async move {
                            let ui = ctx.as_context().ui()?;
                            let Some(tool_name) = normalize_tool_name(&args) else {
                                ui.notify(
                                    "Usage: /add-echo-tool <tool_name> (lowercase, numbers, underscores)",
                                    Some(NotifyType::Warning),
                                );
                                return Ok(());
                            };

                            let created = register_echo_tool(
                                &pi,
                                &reg,
                                &tool_name,
                                &format!("Echo {tool_name}"),
                                &format!("[{tool_name}] "),
                            )?;
                            if !created {
                                ui.notify(
                                    &format!("Tool already registered: {tool_name}"),
                                    Some(NotifyType::Warning),
                                );
                                return Ok(());
                            }

                            ui.notify(&format!("Registered dynamic tool: {tool_name}"), Some(NotifyType::Info));
                            Ok(())
                        })
                    }),
                },
            )?;
        }

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(DynamicToolsExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
