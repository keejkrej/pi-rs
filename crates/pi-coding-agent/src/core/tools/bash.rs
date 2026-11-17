//! Port of packages/coding-agent/src/core/tools/bash.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_js::BoxFuture;
use pi_js::abort::AbortSignal;
use serde::{Deserialize, Serialize};

use super::truncate::TruncationResult;

pub const DEFAULT_SHELL_MAX_OUTPUT_BYTES: usize = 50 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolInput {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BashToolOutput {
    pub output: String,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    pub exit_code: i64,
    pub wall_time_seconds: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

pub type BashChunkCallback = Arc<dyn Fn(&[u8]) + Send + Sync>;

#[derive(Clone, Default)]
pub struct BashExecOptions {
    pub on_data: Option<BashChunkCallback>,
    pub signal: Option<AbortSignal>,
    pub timeout: Option<f64>,
    pub env: Option<IndexMap<String, String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BashExecResult {
    pub exit_code: Option<i64>,
}

/// Pluggable operations for the bash tool.
/// Override these to delegate command execution to remote systems (for example SSH).
pub trait BashOperations: Send + Sync {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        options: BashExecOptions,
    ) -> BoxFuture<pi_js::Result<BashExecResult>>;
}

#[derive(Clone, Debug, Default)]
pub struct BashSpawnContext {
    pub command: String,
    pub cwd: String,
    pub env: IndexMap<String, String>,
}

pub type BashSpawnHook = Arc<dyn Fn(BashSpawnContext) -> BashSpawnContext + Send + Sync>;

#[derive(Clone, Default)]
pub struct BashToolOptions {
    pub operations: Option<Arc<dyn BashOperations>>,
    pub command_prefix: Option<String>,
    pub spawn_hook: Option<BashSpawnHook>,
    pub expose_session_environment: Option<bool>,
}

use std::process::Stdio;
use std::sync::{LazyLock, Mutex as StdMutex};
use std::time::Duration;

use pi_agent_core::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback};
use pi_ai::types::{TextContent, UserContentBlock};
use pi_js::seam::{Guard, Slot};
use pi_js::text::Utf8StreamDecoder;
use pi_js::vendor::typebox::{Schema, t};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::core::extensions::types::ToolDefinition;
use crate::core::tools::output_accumulator::{OutputAccumulator, StructuredOutputBuffer};
use crate::core::tools::renderers;
use crate::core::tools::tool_definition_wrapper::{
    BuiltinToolDefSpec, build_builtin_tool_definition, resolve_effective_cwd, strict_prefer_sampling,
    wrap_tool_definition,
};
use crate::core::tools::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, format_size};
use crate::utils::child_process::{ChildProcessExit, wait_for_child_process_status};
use crate::utils::shell::{
    CommandTransport, ShellConfig, get_shell_config as real_get_shell_config, get_shell_env, kill_process_tree,
    sanitize_binary_output, track_detached_child_pid, untrack_detached_child_pid,
};

pub type ShellConfigOverrideFn = Arc<dyn Fn(Option<&str>) -> Result<ShellConfig, pi_js::Error> + Send + Sync>;
static SHELL_CONFIG_SLOT: LazyLock<Slot<ShellConfigOverrideFn>> = LazyLock::new(Slot::new);

#[doc(hidden)]
pub mod testing {
    use super::*;

    pub fn override_get_shell_config(func: ShellConfigOverrideFn) -> Guard {
        SHELL_CONFIG_SLOT.set(Arc::new(func))
    }
}

pub(crate) fn resolve_shell_config(custom_shell_path: Option<&str>) -> Result<ShellConfig, pi_js::Error> {
    if let Some(func) = SHELL_CONFIG_SLOT.get() {
        return func(custom_shell_path);
    }
    real_get_shell_config(custom_shell_path)
}

struct LocalBashOperations {
    custom_shell_path: Option<String>,
}

impl BashOperations for LocalBashOperations {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        options: BashExecOptions,
    ) -> BoxFuture<pi_js::Result<BashExecResult>> {
        let custom_shell_path = self.custom_shell_path.clone();
        let command = command.to_string();
        let cwd = cwd.to_string();
        Box::pin(async move {
            let shell_cfg = resolve_shell_config(custom_shell_path.as_deref())?;
            execute_shell_process(shell_cfg, &command, &cwd, options).await
        })
    }
}

pub(crate) async fn execute_shell_process(
    shell_cfg: ShellConfig,
    command: &str,
    cwd: &str,
    options: BashExecOptions,
) -> Result<BashExecResult, pi_js::Error> {
    if !tokio::fs::try_exists(cwd).await.unwrap_or(false) {
        return Err(pi_js::Error::msg(format!(
            "Working directory does not exist: {cwd}\nCannot execute shell commands."
        )));
    }

    if options.signal.as_ref().is_some_and(|s| s.aborted()) {
        return Err(pi_js::Error::msg("aborted"));
    }

    let mut args = shell_cfg.args.clone();
    let use_stdin = shell_cfg.command_transport == Some(CommandTransport::Stdin);
    if !use_stdin {
        args.push(command.to_string());
    }

    let env_map = options.env.unwrap_or_else(get_shell_env);

    let mut cmd = Command::new(&shell_cfg.shell);
    cmd.args(&args)
        .current_dir(cwd)
        .env_clear()
        .envs(&env_map)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if use_stdin {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }

    #[cfg(unix)]
    cmd.process_group(0);

    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    cmd.kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            pi_js::Error::msg(format!("Failed to spawn shell {}: ENOENT ({e})", shell_cfg.shell))
        } else {
            pi_js::Error::msg(format!("Failed to spawn shell {}: {e}", shell_cfg.shell))
        }
    })?;

    let child_pid = child.id();
    if let Some(pid) = child_pid {
        track_detached_child_pid(pid);
    }

    if use_stdin && let Some(mut stdin) = child.stdin.take() {
        let bytes = command.as_bytes().to_vec();
        tokio::spawn(async move {
            let _ = stdin.write_all(&bytes).await;
            let _ = stdin.shutdown().await;
        });
    }

    let on_stdout = options.on_data.clone().map(|cb| {
        let b: Box<dyn FnMut(&[u8]) + Send> = Box::new(move |chunk: &[u8]| cb(chunk));
        b
    });
    let on_stderr = options.on_data.clone().map(|cb| {
        let b: Box<dyn FnMut(&[u8]) + Send> = Box::new(move |chunk: &[u8]| cb(chunk));
        b
    });

    let timeout_secs = options.timeout;
    let signal = options.signal.clone();

    let abort_fut = async move {
        if let Some(sig) = signal {
            sig.cancelled().await;
        } else {
            std::future::pending::<()>().await;
        }
    };

    let timeout_fut = async move {
        if let Some(secs) = timeout_secs
            && secs > 0.0
            && secs.is_finite()
        {
            tokio::time::sleep(Duration::from_secs_f64(secs)).await;
        } else {
            std::future::pending::<()>().await;
        }
    };

    enum RaceOutcome {
        Exited(Result<ChildProcessExit, pi_js::Error>),
        Aborted,
        TimedOut,
    }

    let race = {
        let wait_fut = wait_for_child_process_status(child, on_stdout, on_stderr);
        tokio::pin!(wait_fut);
        tokio::pin!(abort_fut);
        tokio::pin!(timeout_fut);

        tokio::select! {
            res = &mut wait_fut => RaceOutcome::Exited(res),
            _ = &mut abort_fut => RaceOutcome::Aborted,
            _ = &mut timeout_fut => RaceOutcome::TimedOut,
        }
    };

    let outcome = match race {
        RaceOutcome::Exited(res) => res.map(|exit| BashExecResult {
            exit_code: exit.into_shell_code().map(i64::from),
        }),
        RaceOutcome::Aborted => {
            if let Some(pid) = child_pid {
                kill_process_tree(pid);
            }
            Err(pi_js::Error::msg("aborted"))
        }
        RaceOutcome::TimedOut => {
            if let Some(pid) = child_pid {
                kill_process_tree(pid);
            }
            let secs = timeout_secs.unwrap_or(0.0);
            let secs_str = if secs.fract() == 0.0 {
                format!("{}", secs as u64)
            } else {
                format!("{secs}")
            };
            Err(pi_js::Error::msg(format!("timeout:{secs_str}")))
        }
    };

    if let Some(pid) = child_pid {
        untrack_detached_child_pid(pid);
    }

    outcome
}

pub fn create_local_bash_operations() -> Arc<dyn BashOperations> {
    Arc::new(LocalBashOperations {
        custom_shell_path: None,
    })
}

pub fn create_local_bash_operations_with_shell(shell_path: Option<String>) -> Arc<dyn BashOperations> {
    Arc::new(LocalBashOperations {
        custom_shell_path: shell_path,
    })
}

pub fn bash_tool_schema() -> Schema {
    t::object([
        ("command", t::string().with("description", "Bash command to execute")),
        (
            "timeout",
            t::number()
                .optional()
                .with("description", "Timeout in seconds (optional, no default timeout)"),
        ),
    ])
    .with("additionalProperties", false)
}

pub fn bash_tool_output_schema() -> Schema {
    t::object([
        ("output", t::string()),
        ("truncated", t::boolean()),
        ("exit_code", t::integer()),
        ("wall_time_seconds", t::number()),
        ("full_output_path", t::string().optional()),
    ])
    .with("additionalProperties", false)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellSystemPromptContribution {
    pub prompt_snippet: String,
    pub prompt_guidelines: Option<Vec<String>>,
}

pub fn bash_tool_system_prompt_contribution(options: Option<&BashToolOptions>) -> ShellSystemPromptContribution {
    let expose_env = options.and_then(|o| o.expose_session_environment);
    let prompt_guidelines = if expose_env == Some(false) {
        None
    } else {
        Some(vec![
            "Prefer dedicated read, grep, find, and ls tools instead of running bash cat, grep, find, or ls."
                .to_string(),
        ])
    };
    ShellSystemPromptContribution {
        prompt_snippet: "Execute a bash command in the current working directory".to_string(),
        prompt_guidelines,
    }
}

pub(crate) fn format_shell_output_text(
    accumulator: &mut OutputAccumulator,
    prefix: &str,
) -> (
    String,
    Option<crate::core::tools::truncate::TruncationResult>,
    Option<String>,
) {
    let truncation = accumulator.snapshot(Some(DEFAULT_MAX_LINES), Some(DEFAULT_MAX_BYTES));
    if !truncation.truncated {
        return (truncation.content, None, None);
    }

    let full_output_path = accumulator.ensure_temp_file();
    let path_str = full_output_path.as_deref().unwrap_or("");
    let notice = if truncation.last_line_partial {
        let total_lines = truncation.total_lines.max(0) as usize;
        let line_num = if accumulator.retained_text().ends_with('\n') && total_lines > 1 {
            total_lines - 1
        } else {
            total_lines
        };
        format!(
            "\n\n[Showing last {} of line {line_num} ({}). Full output: {path_str}]",
            format_size(truncation.output_bytes.max(0) as usize),
            format_size(truncation.total_bytes.max(0) as usize),
        )
    } else {
        let total_lines = truncation.total_lines.max(0) as usize;
        let output_lines = truncation.output_lines.max(0) as usize;
        let start_line = total_lines.saturating_sub(output_lines) + 1;
        let end_line = start_line + output_lines.saturating_sub(1);
        let limit_note = if truncation.truncated_by == Some(crate::core::tools::truncate::TruncatedBy::Bytes) {
            format!(" ({} limit)", format_size(DEFAULT_MAX_BYTES))
        } else {
            String::new()
        };
        format!("\n\n[Showing lines {start_line}-{end_line} of {total_lines}{limit_note}. Full output: {path_str}]")
    };
    let _ = prefix;
    let text = format!("{}{}", truncation.content, notice);
    (text, Some(truncation), full_output_path)
}

pub(crate) async fn run_shell_tool_execute<F, Fut>(
    _command: String,
    cwd: String,
    timeout: Option<f64>,
    signal: Option<AbortSignal>,
    on_update: Option<AgentToolUpdateCallback>,
    temp_prefix: &'static str,
    exec_fn: F,
) -> Result<AgentToolResult, pi_js::Error>
where
    F: FnOnce(BashExecOptions) -> Fut,
    Fut: std::future::Future<Output = Result<BashExecResult, pi_js::Error>>,
{
    let started_at_ms = pi_js::time::performance_now();

    struct SharedState {
        decoder: Utf8StreamDecoder,
        accumulator: OutputAccumulator,
        structured: StructuredOutputBuffer,
        finished: bool,
    }

    let state = Arc::new(StdMutex::new(SharedState {
        decoder: Utf8StreamDecoder::new(),
        accumulator: OutputAccumulator::new(Some(DEFAULT_MAX_BYTES), Some(temp_prefix)),
        structured: StructuredOutputBuffer::new(DEFAULT_SHELL_MAX_OUTPUT_BYTES),
        finished: false,
    }));

    let state_for_cb = Arc::clone(&state);
    let on_update_cb = on_update.clone();
    let on_data: BashChunkCallback = Arc::new(move |chunk: &[u8]| {
        let state_ref = Arc::clone(&state_for_cb);
        let update_cb = on_update_cb.clone();
        let mut guard = state_ref.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.finished {
            return;
        }
        let decoded = guard.decoder.decode(chunk, true);
        let sanitized = sanitize_binary_output(&decoded);
        if !sanitized.is_empty() {
            guard.accumulator.push(&sanitized);
            guard.structured.push(&sanitized);
        }
        if let Some(cb) = update_cb {
            let partial_snap = guard
                .accumulator
                .snapshot(Some(DEFAULT_MAX_LINES), Some(DEFAULT_MAX_BYTES));
            let text = partial_snap.content.clone();
            let details = serde_json::to_value(&BashToolDetails {
                truncation: if partial_snap.truncated {
                    Some(partial_snap)
                } else {
                    None
                },
                full_output_path: guard.accumulator.temp_file_path().map(ToString::to_string),
            })
            .ok();
            cb(AgentToolResult {
                content: vec![UserContentBlock::Text(TextContent::new(text))],
                details,
                ..Default::default()
            });
        }
    });

    let exec_opts = BashExecOptions {
        on_data: Some(on_data),
        signal,
        timeout,
        env: Some(get_shell_env()),
    };

    let exec_res = exec_fn(exec_opts).await;
    let wall_time_seconds = ((pi_js::time::performance_now() - started_at_ms) / 1000.0).max(0.0);

    let mut guard = state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.finished = true;
    let tail_decoded = guard.decoder.decode(&[], false);
    let tail_sanitized = sanitize_binary_output(&tail_decoded);
    if !tail_sanitized.is_empty() {
        guard.accumulator.push(&tail_sanitized);
        guard.structured.push(&tail_sanitized);
    }

    let (model_text, truncation, full_output_path) = format_shell_output_text(&mut guard.accumulator, &cwd);
    let (structured_out, structured_truncated) = guard.structured.finish();
    drop(guard);

    match exec_res {
        Err(err) => {
            let msg = err.to_string();
            let prefix = if model_text.is_empty() {
                String::new()
            } else {
                format!("{model_text}\n\n")
            };
            if let Some(secs) = msg.strip_prefix("timeout:") {
                return Err(pi_js::Error::msg(format!(
                    "{prefix}Command timed out after {secs} seconds"
                )));
            }
            if msg == "aborted" {
                return Err(pi_js::Error::msg(format!("{prefix}Command aborted")));
            }
            Err(err)
        }
        Ok(res) => {
            let Some(exit_code) = res.exit_code else {
                let prefix = if model_text.is_empty() {
                    String::new()
                } else {
                    format!("{model_text}\n\n")
                };
                return Err(pi_js::Error::msg(format!(
                    "{prefix}Command terminated without an exit code"
                )));
            };

            let details_val = if truncation.is_some() {
                serde_json::to_value(&BashToolDetails {
                    truncation,
                    full_output_path: full_output_path.clone(),
                })
                .ok()
            } else {
                None
            };

            let mut sc_map = serde_json::Map::new();
            sc_map.insert("output".to_string(), Value::String(structured_out));
            sc_map.insert("truncated".to_string(), Value::Bool(structured_truncated));
            sc_map.insert("exit_code".to_string(), json!(exit_code));
            sc_map.insert("wall_time_seconds".to_string(), json!(wall_time_seconds));
            if let Some(path) = full_output_path {
                sc_map.insert("full_output_path".to_string(), Value::String(path));
            }
            let structured_content = Some(Value::Object(sc_map));

            if exit_code != 0 {
                let display_text = if model_text.is_empty() {
                    format!("Command exited with code {exit_code}")
                } else {
                    format!("{model_text}\n\nCommand exited with code {exit_code}")
                };
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(display_text))],
                    details: details_val,
                    structured_content,
                    is_error: Some(true),
                    ..Default::default()
                })
            } else {
                let display_text = if model_text.is_empty() {
                    "(no output)".to_string()
                } else {
                    model_text
                };
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(display_text))],
                    details: details_val,
                    structured_content,
                    ..Default::default()
                })
            }
        }
    }
}

pub fn create_bash_tool_definition(cwd: &str, options: Option<BashToolOptions>) -> ToolDefinition {
    create_bash_tool_definition_with_shell(cwd, None, options)
}

pub fn create_bash_tool_definition_with_shell(
    cwd: &str,
    shell_path: Option<String>,
    options: Option<BashToolOptions>,
) -> ToolDefinition {
    let contribution = bash_tool_system_prompt_contribution(options.as_ref());
    let default_cwd = cwd.to_string();
    let command_prefix = options.as_ref().and_then(|o| o.command_prefix.clone());
    let spawn_hook = options.as_ref().and_then(|o| o.spawn_hook.clone());
    let ops = options
        .and_then(|o| o.operations)
        .unwrap_or_else(move || create_local_bash_operations_with_shell(shell_path));
    let desc = format!(
        "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last {} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
        DEFAULT_MAX_LINES,
        DEFAULT_MAX_BYTES / 1024
    );

    build_builtin_tool_definition(BuiltinToolDefSpec {
        name: "bash",
        label: "bash",
        description: desc,
        prompt_snippet: Some(contribution.prompt_snippet),
        prompt_guidelines: contribution.prompt_guidelines,
        parameters: bash_tool_schema(),
        constrained_sampling: strict_prefer_sampling(),
        render_shell: None,
        prepare_arguments: None,
        output_schema: Some(bash_tool_output_schema()),
        execute: Arc::new(move |_tool_call_id, params, signal, on_update, ctx| {
            let ops = Arc::clone(&ops);
            let command_prefix = command_prefix.clone();
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
                let input: BashToolInput = serde_json::from_value(clean_params)
                    .map_err(|e| pi_js::Error::msg(format!("Invalid bash parameters: {e}")))?;

                let prefixed_cmd = if let Some(prefix) = &command_prefix {
                    format!("{prefix}\n{}", input.command)
                } else {
                    input.command
                };

                let initial_env = get_shell_env();
                let spawn_ctx = if let Some(hook) = &spawn_hook {
                    hook(BashSpawnContext {
                        command: prefixed_cmd,
                        cwd: base_cwd,
                        env: initial_env,
                    })
                } else {
                    BashSpawnContext {
                        command: prefixed_cmd,
                        cwd: base_cwd,
                        env: initial_env,
                    }
                };

                run_shell_tool_execute(
                    spawn_ctx.command.clone(),
                    spawn_ctx.cwd.clone(),
                    raw_timeout,
                    signal,
                    on_update,
                    "pi-bash",
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

pub fn create_bash_tool(cwd: &str, options: Option<BashToolOptions>) -> AgentTool {
    wrap_tool_definition(create_bash_tool_definition(cwd, options), None)
}
