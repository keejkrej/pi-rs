//! Port of packages/coding-agent/examples/extensions/tool-override.ts

use std::process::ExitCode;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use chrono::Utc;
use pi_agent_core::types::AgentToolResult;
use pi_ai::types::{TextContent, UserContentBlock};
use pi_coding_agent::config::get_agent_dir;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, NotifyType, RegisteredCommandOptions, ToolDefinition,
};
use pi_coding_agent::core::tools::with_file_mutation_queue;
use pi_js::vendor::typebox::t;

static BLOCKED_PATTERNS: LazyLock<Vec<regex::Regex>> = LazyLock::new(|| {
    [
        r"\.env$",
        r"\.env\..+$",
        r"(?i)secrets?\.(json|yaml|yml|toml)$",
        r"(?i)credentials?\.(json|yaml|yml|toml)$",
        r"/\.ssh/",
        r"/\.aws/",
        r"/\.gnupg/",
    ]
    .into_iter()
    .filter_map(|p| regex::Regex::new(p).ok())
    .collect()
});

fn log_file_path() -> String {
    pi_js::path::join(&[&get_agent_dir(), "read-access.log"])
}

fn is_blocked_path(path: &str) -> bool {
    BLOCKED_PATTERNS.iter().any(|p| p.is_match(path))
}

async fn log_access(path: &str, allowed: bool, reason: Option<&str>) {
    let timestamp = Utc::now().to_rfc3339();
    let status = if allowed { "ALLOWED" } else { "BLOCKED" };
    let msg = reason.map(|r| format!(" ({r})")).unwrap_or_default();
    let line = format!("[{timestamp}] {status}: {path}{msg}\n");
    let log_file = log_file_path();
    let queue_file = log_file.clone();
    let _ = with_file_mutation_queue(&queue_file, move || async move {
        pi_js::fs::promises::append_file(&log_file, line.as_bytes()).await
    })
    .await;
}

pub struct ToolOverrideExtension;

#[async_trait]
impl Extension for ToolOverrideExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let parameters = t::object([
            (
                "path",
                t::string().with("description", "Path to the file to read (relative or absolute)"),
            ),
            (
                "offset",
                t::number()
                    .with("description", "Line number to start reading from (1-indexed)")
                    .optional(),
            ),
            (
                "limit",
                t::number()
                    .with("description", "Maximum number of lines to read")
                    .optional(),
            ),
        ]);

        pi.register_tool(ToolDefinition {
            name: "read".to_string(),
            label: "read (audited)".to_string(),
            description: "Read the contents of a file with access logging. Some sensitive paths (.env, secrets, credentials) are blocked.".to_string(),
            prompt_snippet: None,
            prompt_guidelines: None,
            parameters,
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
            execute: Arc::new(|_tool_call_id, params, _signal, _on_update, ctx| {
                Box::pin(async move {
                    let path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
                    let offset = params.get("offset").and_then(|v| v.as_f64()).map(|n| n as usize);
                    let limit = params.get("limit").and_then(|v| v.as_f64()).map(|n| n as usize);
                    let cwd = ctx.cwd()?;
                    let absolute_path = pi_js::path::resolve(&[&cwd, path]);

                    if is_blocked_path(&absolute_path) {
                        log_access(&absolute_path, false, Some("matches blocked pattern")).await;
                        return Ok(AgentToolResult {
                            content: vec![UserContentBlock::Text(TextContent::new(format!(
                                "Access denied: \"{path}\" matches a blocked pattern (sensitive file). This tool blocks access to .env files, secrets, credentials, and SSH/AWS/GPG directories."
                            )))],
                            details: Some(serde_json::json!({ "blocked": true })),
                            structured_content: None,
                            usage: None,
                            is_error: None,
                            terminate: None,
                            control: None,
                        });
                    }

                    log_access(&absolute_path, true, None).await;

                    if let Err(err) = pi_js::fs::promises::access(&absolute_path, pi_js::fs::constants::R_OK).await {
                        return Ok(AgentToolResult {
                            content: vec![UserContentBlock::Text(TextContent::new(format!(
                                "Error reading file: {err}"
                            )))],
                            details: Some(serde_json::json!({ "error": true })),
                            structured_content: None,
                            usage: None,
                            is_error: None,
                            terminate: None,
                            control: None,
                        });
                    }

                    match pi_js::fs::promises::read_to_string(&absolute_path).await {
                        Ok(content) => {
                            let lines: Vec<&str> = content.split('\n').collect();
                            let start_line = offset.map(|o| o.saturating_sub(1)).unwrap_or(0).min(lines.len());
                            let end_line = limit
                                .map(|l| start_line.saturating_add(l).min(lines.len()))
                                .unwrap_or(lines.len());
                            let mut text = lines[start_line..end_line].join("\n");
                            let max_bytes = 50 * 1024;
                            if text.len() > max_bytes {
                                let mut end = max_bytes;
                                while end > 0 && !text.is_char_boundary(end) {
                                    end -= 1;
                                }
                                text = format!("{}\n\n[Output truncated at 50KB]", &text[..end]);
                            }
                            Ok(AgentToolResult {
                                content: vec![UserContentBlock::Text(TextContent::new(text))],
                                details: Some(serde_json::json!({ "lines": lines.len() })),
                                structured_content: None,
                                usage: None,
                                is_error: None,
                                terminate: None,
                                control: None,
                            })
                        }
                        Err(err) => Ok(AgentToolResult {
                            content: vec![UserContentBlock::Text(TextContent::new(format!(
                                "Error reading file: {err}"
                            )))],
                            details: Some(serde_json::json!({ "error": true })),
                            structured_content: None,
                            usage: None,
                            is_error: None,
                            terminate: None,
                            control: None,
                        }),
                    }
                })
            }),
            render_call: None,
            render_result: None,
        })?;

        pi.register_command(
            "read-log",
            RegisteredCommandOptions {
                description: Some("View the file access log".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let ui = ctx.as_context().ui()?;
                        match pi_js::fs::read_to_string(&log_file_path()) {
                            Ok(log) => {
                                let all_lines: Vec<&str> = log.trim().lines().collect();
                                let start = all_lines.len().saturating_sub(20);
                                ui.notify(
                                    &format!("Recent file access:\n{}", all_lines[start..].join("\n")),
                                    Some(NotifyType::Info),
                                );
                            }
                            Err(_) => {
                                ui.notify("No access log found", Some(NotifyType::Info));
                            }
                        }
                        Ok(())
                    })
                }),
            },
        )?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(ToolOverrideExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
