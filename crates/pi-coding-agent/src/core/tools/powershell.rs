//! Port of packages/coding-agent/src/core/tools/powershell.ts

use std::sync::Arc;

use super::bash::{BashOperations, BashSpawnContext, BashSpawnHook, BashToolDetails, BashToolInput};

pub type PowerShellOperations = dyn BashOperations;
pub type PowerShellSpawnContext = BashSpawnContext;
pub type PowerShellSpawnHook = BashSpawnHook;
pub type PowerShellToolDetails = BashToolDetails;
pub type PowerShellToolInput = BashToolInput;

#[derive(Clone, Default)]
pub struct PowerShellToolOptions {
    pub operations: Option<Arc<dyn BashOperations>>,
    pub expose_session_environment: Option<bool>,
    pub spawn_hook: Option<BashSpawnHook>,
}

use pi_agent_core::types::AgentTool;
use pi_js::BoxFuture;
use pi_js::vendor::typebox::{Schema, t};
use serde_json::Value;

use crate::core::extensions::types::ToolDefinition;
use crate::core::tools::bash::{
    BashExecOptions, BashExecResult, ShellSystemPromptContribution, bash_tool_output_schema, execute_shell_process,
    run_shell_tool_execute,
};
use crate::core::tools::renderers;
use crate::core::tools::tool_definition_wrapper::{
    BuiltinToolDefSpec, build_builtin_tool_definition, resolve_effective_cwd, strict_prefer_sampling,
    wrap_tool_definition,
};
use crate::core::tools::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};
use crate::utils::shell::get_power_shell_config;

struct LocalPowerShellOperations;

impl BashOperations for LocalPowerShellOperations {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        options: BashExecOptions,
    ) -> BoxFuture<pi_js::Result<BashExecResult>> {
        let command = command.to_string();
        let cwd = cwd.to_string();
        Box::pin(async move {
            let shell_cfg = get_power_shell_config()?;
            let wrapped = wrap_powershell_command(&command);
            execute_shell_process(shell_cfg, &wrapped, &cwd, options).await
        })
    }
}

pub fn wrap_powershell_command(command: &str) -> String {
    format!(
        "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $OutputEncoding = [System.Text.Encoding]::UTF8\n{command}\nif (-not $?) {{ if ($LASTEXITCODE) {{ exit $LASTEXITCODE }} else {{ exit 1 }} }} elseif ($LASTEXITCODE) {{ exit $LASTEXITCODE }}"
    )
}

pub fn create_local_power_shell_operations() -> Arc<PowerShellOperations> {
    Arc::new(LocalPowerShellOperations)
}

pub fn powershell_tool_schema() -> Schema {
    t::object([
        (
            "command",
            t::string().with("description", "PowerShell command to execute"),
        ),
        (
            "timeout",
            t::number()
                .optional()
                .with("description", "Timeout in seconds (optional, no default timeout)"),
        ),
    ])
    .with("additionalProperties", false)
}

pub fn powershell_tool_system_prompt_contribution(
    options: Option<&PowerShellToolOptions>,
) -> ShellSystemPromptContribution {
    let expose_env = options.and_then(|o| o.expose_session_environment);
    let prompt_guidelines = if expose_env == Some(false) {
        None
    } else {
        Some(vec![
            "You can inspect PI_* environment variables for current model and session details.".to_string(),
        ])
    };
    ShellSystemPromptContribution {
        prompt_snippet: "Execute a PowerShell command in the current working directory".to_string(),
        prompt_guidelines,
    }
}

pub fn create_power_shell_tool_definition(cwd: &str, options: Option<PowerShellToolOptions>) -> ToolDefinition {
    let contribution = powershell_tool_system_prompt_contribution(options.as_ref());
    let default_cwd = cwd.to_string();
    let spawn_hook = options.as_ref().and_then(|o| o.spawn_hook.clone());
    let expose_session_environment = options.as_ref().and_then(|o| o.expose_session_environment);
    let ops = options
        .and_then(|o| o.operations)
        .unwrap_or_else(create_local_power_shell_operations);
    let desc = format!(
        "Execute a PowerShell command in the current working directory. Returns stdout and stderr. Output is truncated to last {} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
        DEFAULT_MAX_LINES,
        DEFAULT_MAX_BYTES / 1024
    );

    build_builtin_tool_definition(BuiltinToolDefSpec {
        name: "powershell",
        label: "powershell",
        description: desc,
        prompt_snippet: Some(contribution.prompt_snippet),
        prompt_guidelines: contribution.prompt_guidelines,
        parameters: powershell_tool_schema(),
        constrained_sampling: strict_prefer_sampling(),
        render_shell: None,
        prepare_arguments: None,
        output_schema: Some(bash_tool_output_schema()),
        execute: Arc::new(move |_tool_call_id, params, signal, on_update, ctx| {
            let ops = Arc::clone(&ops);
            let spawn_hook = spawn_hook.clone();
            let base_cwd = resolve_effective_cwd(&ctx, &default_cwd);
            Box::pin(async move {
                if signal.as_ref().is_some_and(|s| s.aborted()) {
                    return Err(pi_js::Error::msg("Command aborted"));
                }

                let raw_timeout = params.get("timeout").and_then(|v| v.as_f64()).filter(|f| f.is_finite());

                let mut clean_params = params;
                if let Value::Object(ref mut map) = clean_params {
                    map.shift_remove("timeout");
                }
                let input: PowerShellToolInput = serde_json::from_value(clean_params)
                    .map_err(|e| pi_js::Error::msg(format!("Invalid powershell parameters: {e}")))?;

                let spawn_ctx = crate::core::tools::bash::resolve_shell_spawn_context(
                    input.command,
                    base_cwd,
                    spawn_hook.as_ref(),
                    expose_session_environment,
                    &ctx,
                );

                run_shell_tool_execute(
                    spawn_ctx.command.clone(),
                    spawn_ctx.cwd.clone(),
                    raw_timeout,
                    signal,
                    on_update,
                    "pi-powershell",
                    move |mut exec_opts| async move {
                        exec_opts.env = Some(spawn_ctx.env);
                        ops.exec(&spawn_ctx.command, &spawn_ctx.cwd, exec_opts).await
                    },
                )
                .await
            })
        }),
        render_call: Some(renderers::bash::render_call()),
        render_result: Some(renderers::bash::render_result()),
    })
}

pub fn create_power_shell_tool(cwd: &str, options: Option<PowerShellToolOptions>) -> AgentTool {
    wrap_tool_definition(create_power_shell_tool_definition(cwd, options), None)
}
