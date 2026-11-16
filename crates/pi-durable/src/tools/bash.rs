//! Port of packages/durable/src/tools/bash.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_chord::Context;
use pi_js::vendor::typebox::t;
use serde::{Deserialize, Serialize};

use crate::env::{ExecutionErrorCode, ShellExecOptions, ShellSpillOptions};
use crate::harness::define::define_tool;
use crate::harness::types::{
    DiagnosticSeverity, OutputRetainMode, ToolDiagnostic, ToolExecutionApi, ToolExecutionResult, ToolOutputLimits,
    ToolRegistration,
};
use crate::tools::env::require_env;
use crate::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};

const MAX_TIMEOUT_SECONDS: f64 = 2_147_483_647.0 / 1000.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolInput {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BashExecution {
    pub command: String,
    pub cwd: String,
    pub env: IndexMap<String, String>,
    pub inherit_env: bool,
}

pub type BashPrepare =
    Arc<dyn Fn(&mut BashExecution, ToolExecutionApi, Context) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>;

#[derive(Clone, Default)]
pub struct BashToolOptions {
    pub command_prefix: Option<String>,
    pub prepare: Option<BashPrepare>,
}

fn bash_schema() -> pi_js::vendor::typebox::Schema {
    t::object([
        ("command", t::string().with("description", "Bash command to execute")),
        (
            "timeout",
            t::number()
                .with("description", "Timeout in seconds (optional, no default timeout)")
                .optional(),
        ),
    ])
}

fn validate_timeout(timeout: Option<f64>) -> pi_js::Result<()> {
    let Some(timeout) = timeout else {
        return Ok(());
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(pi_js::Error::msg("Invalid timeout: must be a finite number of seconds"));
    }
    if timeout > MAX_TIMEOUT_SECONDS {
        return Err(pi_js::Error::msg(format!(
            "Invalid timeout: maximum is {} seconds",
            pi_js::num::to_js_string(MAX_TIMEOUT_SECONDS)
        )));
    }
    Ok(())
}

/// Runs a command through the environment's shell. Output streams to `api.output()`, where the Harness keeps
/// the tail within default limits. Output beyond limits is spilled to a file reported as a diagnostic.
pub fn create_bash_tool(options: Option<BashToolOptions>) -> ToolRegistration {
    let options = options.unwrap_or_default();
    let command_prefix = options.command_prefix;
    let prepare = options.prepare;

    define_tool(ToolRegistration {
        name: "bash".to_owned(),
        description: format!(
            "Execute a bash command in the current working directory. Returns combined stdout and stderr. Output is truncated to last {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
            DEFAULT_MAX_BYTES / 1024
        ),
        parameters: bash_schema().to_value(),
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        output_limits: Some(ToolOutputLimits {
            max_bytes: None,
            max_lines: None,
            retain: Some(OutputRetainMode::Tail),
        }),
        execute: Arc::new(move |args_val, api, context| {
            let command_prefix = command_prefix.clone();
            let prepare = prepare.clone();
            Box::pin(async move {
                let args: BashToolInput =
                    serde_json::from_value(args_val).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
                let BashToolInput { command, timeout } = args;
                validate_timeout(timeout)?;
                let env = require_env(&api)?;
                let mut execution = BashExecution {
                    command: match &command_prefix {
                        Some(prefix) => format!("{prefix}\n{command}"),
                        None => command,
                    },
                    cwd: env.cwd().to_owned(),
                    env: IndexMap::new(),
                    inherit_env: true,
                };
                if let Some(prepare_fn) = &prepare {
                    prepare_fn(&mut execution, api.clone(), context.clone()).await?;
                }
                let api_out = api.clone();
                let exec_result = env
                    .exec(
                        &execution.command,
                        Some(ShellExecOptions {
                            cwd: Some(execution.cwd),
                            env: Some(execution.env),
                            inherit_env: Some(execution.inherit_env),
                            timeout,
                            on_output: Some(Arc::new(move |text, _ctx| api_out.output(text))),
                            spill: Some(ShellSpillOptions {
                                after_bytes: DEFAULT_MAX_BYTES,
                                after_lines: DEFAULT_MAX_LINES,
                            }),
                        }),
                        &context,
                    )
                    .await;

                let spill_path = match &exec_result {
                    Ok(ok) => ok.spill_path.as_deref(),
                    Err(err) => err.spill_path.as_deref(),
                };
                if let Some(spill_path) = spill_path {
                    api.diagnostic(ToolDiagnostic {
                        severity: DiagnosticSeverity::Info,
                        code: Some("full_output".to_owned()),
                        message: format!("Full output: {spill_path}"),
                    })?;
                }

                match exec_result {
                    Err(err) => {
                        if err.code == ExecutionErrorCode::Aborted
                            && context.abort_signal().is_some_and(|s| s.aborted())
                        {
                            return Err(err.into());
                        }
                        if err.code == ExecutionErrorCode::Timeout {
                            let timeout_str = timeout
                                .map(pi_js::num::to_js_string)
                                .unwrap_or_else(|| "undefined".to_owned());
                            return Err(pi_js::Error::msg(format!(
                                "Command timed out after {timeout_str} seconds"
                            )));
                        }
                        if err.code == ExecutionErrorCode::Aborted {
                            return Err(pi_js::Error::msg("Command aborted"));
                        }
                        Err(err.into())
                    }
                    Ok(ok) => {
                        if ok.exit_code != 0 {
                            return Err(pi_js::Error::msg(format!("Command exited with code {}", ok.exit_code)));
                        }
                        Ok(ToolExecutionResult::default())
                    }
                }
            })
        }),
    })
}
