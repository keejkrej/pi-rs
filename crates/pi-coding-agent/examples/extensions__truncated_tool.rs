//! Port of packages/coding-agent/examples/extensions/truncated-tool.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::types::AgentToolResult;
use pi_ai::types::{TextContent, UserContentBlock};
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, ToolDefinition,
};
use pi_coding_agent::core::tools::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncationOptions, TruncationResult, format_size, truncate_head,
};
use pi_coding_agent::core::tools::with_file_mutation_queue;
use pi_js::vendor::typebox::t;
use pi_tui::Text;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RgDetails {
    pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    glob: Option<String>,
    match_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    truncation: Option<TruncationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    full_output_path: Option<String>,
}

pub struct TruncatedToolExtension;

#[async_trait]
impl Extension for TruncatedToolExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let params_schema = t::object([
            ("pattern", t::string().with("description", "Search pattern (regex)")),
            (
                "path",
                t::string()
                    .with("description", "Directory to search (default: current directory)")
                    .optional(),
            ),
            (
                "glob",
                t::string()
                    .with("description", "File glob pattern, e.g. '*.ts'")
                    .optional(),
            ),
        ]);

        pi.register_tool(ToolDefinition {
            name: "rg".to_string(),
            label: "ripgrep".to_string(),
            description: format!(
                "Search file contents using ripgrep. Output is truncated to {DEFAULT_MAX_LINES} lines or {} (whichever is hit first). If truncated, full output is saved to a temp file.",
                format_size(DEFAULT_MAX_BYTES)
            ),
            prompt_snippet: None,
            prompt_guidelines: None,
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
            execute: Arc::new(|_tool_call_id, params, _signal, _on_update, ctx| {
                Box::pin(async move {
                    let pattern = params.get("pattern").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let search_path = params.get("path").and_then(|v| v.as_str()).map(ToString::to_string);
                    let glob = params.get("glob").and_then(|v| v.as_str()).map(ToString::to_string);

                    let mut args = vec!["--line-number".to_string(), "--color=never".to_string()];
                    if let Some(ref g) = glob {
                        args.push("--glob".to_string());
                        args.push(g.clone());
                    }
                    args.push(pattern.clone());
                    args.push(search_path.clone().unwrap_or_else(|| ".".to_string()));

                    let cwd = ctx.cwd()?;
                    let exec_out = Command::new("rg")
                        .args(&args)
                        .current_dir(cwd)
                        .output()
                        .await
                        .map_err(|e| pi_js::Error::msg(format!("ripgrep failed: {e}")))?;

                    let exit_code = exec_out.status.code();
                    if exit_code == Some(1) {
                        let details = RgDetails {
                            pattern,
                            path: search_path,
                            glob,
                            match_count: 0,
                            truncation: None,
                            full_output_path: None,
                        };
                        return Ok(AgentToolResult {
                            content: vec![UserContentBlock::Text(TextContent::new("No matches found"))],
                            details: serde_json::to_value(details).ok(),
                            structured_content: None,
                            usage: None,
                            is_error: None,
                            terminate: None,
                            control: None,
                        });
                    }

                    if !exec_out.status.success() {
                        let stderr_msg = String::from_utf8_lossy(&exec_out.stderr);
                        return Err(pi_js::Error::msg(format!("ripgrep failed: {stderr_msg}")));
                    }

                    let output = String::from_utf8_lossy(&exec_out.stdout).into_owned();
                    if output.trim().is_empty() {
                        let details = RgDetails {
                            pattern,
                            path: search_path,
                            glob,
                            match_count: 0,
                            truncation: None,
                            full_output_path: None,
                        };
                        return Ok(AgentToolResult {
                            content: vec![UserContentBlock::Text(TextContent::new("No matches found"))],
                            details: serde_json::to_value(details).ok(),
                            structured_content: None,
                            usage: None,
                            is_error: None,
                            terminate: None,
                            control: None,
                        });
                    }

                    let truncation = truncate_head(
                        &output,
                        Some(TruncationOptions {
                            max_lines: Some(DEFAULT_MAX_LINES),
                            max_bytes: Some(DEFAULT_MAX_BYTES),
                        }),
                    );
                    let match_count = output.lines().filter(|line| !line.trim().is_empty()).count();
                    let mut details = RgDetails {
                        pattern,
                        path: search_path,
                        glob,
                        match_count,
                        truncation: None,
                        full_output_path: None,
                    };
                    let mut result_text = truncation.content.clone();

                    if truncation.truncated {
                        let prefix = pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-rg-"]);
                        let temp_dir = pi_js::fs::promises::mkdtemp(&prefix).await?;
                        let temp_file = pi_js::path::join(&[&temp_dir, "output.txt"]);
                        let write_path = temp_file.clone();
                        let output_owned = output.clone();
                        with_file_mutation_queue(&temp_file, move || async move {
                            pi_js::fs::promises::write_file(&write_path, output_owned.as_bytes(), None).await
                        })
                        .await?;

                        let truncated_lines = truncation.total_lines - truncation.output_lines;
                        let truncated_bytes = truncation.total_bytes - truncation.output_bytes;
                        let out_size = format_size(usize::try_from(truncation.output_bytes).unwrap_or(0));
                        let tot_size = format_size(usize::try_from(truncation.total_bytes).unwrap_or(0));
                        let trunc_size = format_size(usize::try_from(truncated_bytes).unwrap_or(0));

                        result_text.push_str(&format!(
                            "\n\n[Output truncated: showing {} of {} lines ({out_size} of {tot_size}). {truncated_lines} lines ({trunc_size}) omitted. Full output saved to: {temp_file}]",
                            truncation.output_lines, truncation.total_lines
                        ));
                        details.truncation = Some(truncation);
                        details.full_output_path = Some(temp_file);
                    }

                    Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new(result_text))],
                        details: serde_json::to_value(details).ok(),
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: None,
                        control: None,
                    })
                })
            }),
            render_call: Some(Arc::new(|args, theme, _context| {
                let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
                let mut text = format!(
                    "{}{}",
                    theme.fg("toolTitle", &theme.bold("rg ")),
                    theme.fg("accent", &format!("\"{pattern}\""))
                );
                if let Some(p) = args.get("path").and_then(|v| v.as_str())
                    && !p.is_empty()
                {
                    text.push_str(&theme.fg("muted", &format!(" in {p}")));
                }
                if let Some(g) = args.get("glob").and_then(|v| v.as_str())
                    && !g.is_empty()
                {
                    text.push_str(&theme.fg("dim", &format!(" --glob {g}")));
                }
                Arc::new(Text::new(Some(&text), Some(0), Some(0), None))
            })),
            render_result: Some(Arc::new(|result, opts, theme, _context| {
                if opts.is_partial {
                    return Arc::new(Text::new(Some(&theme.fg("warning", "Searching...")), Some(0), Some(0), None));
                }
                let details: Option<RgDetails> = result
                    .details
                    .clone()
                    .and_then(|v| serde_json::from_value(v).ok());
                let Some(details) = details.filter(|d| d.match_count > 0) else {
                    return Arc::new(Text::new(Some(&theme.fg("dim", "No matches found")), Some(0), Some(0), None));
                };

                let mut text = theme.fg("success", &format!("{} matches", details.match_count));
                if details.truncation.as_ref().is_some_and(|t| t.truncated) {
                    text.push_str(&theme.fg("warning", " (truncated)"));
                }

                if opts.expanded {
                    if let Some(UserContentBlock::Text(content)) = result.content.first() {
                        let all_lines: Vec<&str> = content.text.split('\n').collect();
                        for line in all_lines.iter().take(20) {
                            text.push_str(&format!("\n{}", theme.fg("dim", line)));
                        }
                        if all_lines.len() > 20 {
                            text.push_str(&format!(
                                "\n{}",
                                theme.fg("muted", "... (use read tool to see full output)")
                            ));
                        }
                    }
                    if let Some(full_path) = details.full_output_path {
                        text.push_str(&format!("\n{}", theme.fg("dim", &format!("Full output: {full_path}"))));
                    }
                }

                Arc::new(Text::new(Some(&text), Some(0), Some(0), None))
            })),
        })?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(TruncatedToolExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
