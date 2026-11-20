//! Port of packages/coding-agent/src/core/tools/grep.ts

use std::sync::Arc;

use pi_js::BoxFuture;
use serde::{Deserialize, Serialize};

use super::truncate::TruncationResult;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolInput {
    pub pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore_case: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub literal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_limit_reached: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_truncated: Option<bool>,
}

/// Pluggable operations for the grep tool.
/// Search runs local ripgrep; override the tool's `execute` for remote search.
pub trait GrepOperations: Send + Sync {
    fn is_directory(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<bool>>;
    /// Deprecated and ignored: context lines come directly from ripgrep.
    fn read_file(&self, _absolute_path: &str) -> BoxFuture<pi_js::Result<String>> {
        Box::pin(async { Ok(String::new()) })
    }
}

#[derive(Clone, Default)]
pub struct GrepToolOptions {
    pub operations: Option<Arc<dyn GrepOperations>>,
}

use std::process::Stdio;

use indexmap::IndexMap;
use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_ai::types::{TextContent, UserContentBlock};
use pi_js::vendor::typebox::{Schema, t};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::core::extensions::types::ToolDefinition;
use crate::core::tools::edit::ToolSystemPromptContribution;
use crate::core::tools::path_utils::resolve_to_cwd;
use crate::core::tools::renderers;
use crate::core::tools::tool_definition_wrapper::{
    BuiltinToolDefSpec, build_builtin_tool_definition, resolve_effective_cwd, wrap_tool_definition,
};
use crate::core::tools::truncate::{
    DEFAULT_MAX_BYTES, GREP_MAX_LINE_LENGTH, TruncationOptions, format_size, truncate_head, truncate_line,
};
use crate::utils::shell::get_shell_env;
use crate::utils::tools_manager::{ManagedTool, ToolStatus, ensure_tool};

const DEFAULT_GREP_LIMIT: usize = 100;

struct LocalGrepOperations;

impl GrepOperations for LocalGrepOperations {
    fn is_directory(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<bool>> {
        let path = absolute_path.to_string();
        Box::pin(async move {
            let meta = tokio::fs::metadata(&path)
                .await
                .map_err(|e| pi_js::Error::msg(format!("Failed to access path {path}: {e}")))?;
            Ok(meta.is_dir())
        })
    }

    fn read_file(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<String>> {
        let path = absolute_path.to_string();
        Box::pin(async move {
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|e| pi_js::Error::msg(e.to_string()))?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        })
    }
}

pub fn create_local_grep_operations() -> Arc<dyn GrepOperations> {
    Arc::new(LocalGrepOperations)
}

pub fn grep_tool_schema() -> Schema {
    t::object([
        (
            "pattern",
            t::string().with("description", "Search pattern (regex or literal string)"),
        ),
        (
            "path",
            t::string().optional().with(
                "description",
                "Directory or file to search in (default: current directory)",
            ),
        ),
        (
            "glob",
            t::string().optional().with(
                "description",
                "Filter files by glob pattern, relative to path, e.g. '*.ts', 'src/**/*.ts' or '**/*.spec.ts'. A pattern without '/' matches file names at any depth",
            ),
        ),
        (
            "ignoreCase",
            t::boolean()
                .optional()
                .with("description", "Case-insensitive search (default: false)"),
        ),
        (
            "literal",
            t::boolean().optional().with(
                "description",
                "Treat pattern as literal string instead of regex (default: false)",
            ),
        ),
        (
            "context",
            t::number().optional().with(
                "description",
                "Number of lines to show before and after each match (default: 0)",
            ),
        ),
        (
            "limit",
            t::number()
                .optional()
                .with("description", "Maximum number of matches to return (default: 100)"),
        ),
    ])
    .with("additionalProperties", false)
}

pub fn grep_tool_system_prompt_contribution() -> ToolSystemPromptContribution {
    ToolSystemPromptContribution {
        prompt_snippet: "Search file contents for a regex or literal pattern (respects .gitignore)".to_string(),
        prompt_guidelines: vec![
            "Use grep to search file contents instead of running bash grep or rg directly.".to_string(),
        ],
    }
}

fn parse_effective_limit(raw: Option<&Value>, default_limit: usize) -> usize {
    match raw {
        Some(Value::Number(n)) => n
            .as_f64()
            .filter(|f| f.is_finite())
            .map(|f| (f.floor() as isize).max(1) as usize)
            .unwrap_or(default_limit),
        _ => default_limit,
    }
}

fn parse_effective_context(raw: Option<&Value>) -> usize {
    match raw {
        Some(Value::Number(n)) => n
            .as_f64()
            .filter(|f| f.is_finite())
            .map(|f| f.floor().clamp(0.0, 1_000_000.0) as usize)
            .unwrap_or(0),
        _ => 0,
    }
}

fn strip_dot_slash_prefix(s: &str) -> &str {
    let mut cur = s;
    while let Some(rest) = cur.strip_prefix("./") {
        cur = rest;
    }
    while let Some(rest) = cur.strip_prefix(".\\") {
        cur = rest;
    }
    cur
}

fn clean_rg_glob(raw: &str) -> String {
    if let Some(neg) = raw.strip_prefix('!') {
        format!("!{}", strip_dot_slash_prefix(neg))
    } else {
        strip_dot_slash_prefix(raw).to_string()
    }
}

fn strip_trailing_eol(s: &str) -> &str {
    s.strip_suffix("\r\n").or_else(|| s.strip_suffix('\n')).unwrap_or(s)
}

fn decode_rg_arbitrary_text(node: &Value) -> String {
    if let Some(text) = node.get("text").and_then(Value::as_str) {
        return text.to_string();
    }
    if let Some(b64) = node.get("bytes").and_then(Value::as_str) {
        let raw = pi_js::b64::decode(b64);
        return String::from_utf8_lossy(&raw).into_owned();
    }
    String::new()
}

pub fn create_grep_tool_definition(cwd: &str, options: Option<GrepToolOptions>) -> ToolDefinition {
    let default_cwd = cwd.to_string();
    let ops = options
        .and_then(|o| o.operations)
        .unwrap_or_else(create_local_grep_operations);
    let contribution = grep_tool_system_prompt_contribution();
    let desc = format!(
        "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to {} matches or {}KB (whichever is hit first).",
        DEFAULT_GREP_LIMIT,
        DEFAULT_MAX_BYTES / 1024
    );

    build_builtin_tool_definition(BuiltinToolDefSpec {
        name: "grep",
        label: "grep",
        description: desc,
        prompt_snippet: Some(contribution.prompt_snippet),
        prompt_guidelines: Some(contribution.prompt_guidelines),
        parameters: grep_tool_schema(),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: None,
        execute: Arc::new(move |_tool_call_id, params, signal, _on_update, ctx| {
            let ops = Arc::clone(&ops);
            let effective_cwd = resolve_effective_cwd(&ctx, &default_cwd);
            Box::pin(async move {
                if signal.as_ref().is_some_and(|s| s.aborted()) {
                    return Err(pi_js::Error::msg("Operation aborted"));
                }

                let raw_limit = params.get("limit").cloned();
                let raw_context = params.get("context").cloned();
                let mut clean_params = params;
                if let Value::Object(ref mut map) = clean_params {
                    map.shift_remove("limit");
                    map.shift_remove("context");
                }
                let input: GrepToolInput = serde_json::from_value(clean_params)
                    .map_err(|e| pi_js::Error::msg(format!("Invalid grep parameters: {e}")))?;

                let effective_limit = parse_effective_limit(raw_limit.as_ref(), DEFAULT_GREP_LIMIT);
                let effective_context = parse_effective_context(raw_context.as_ref());

                let Some(rg_bin) = ensure_tool(ManagedTool::Rg, None::<fn(ToolStatus)>).await else {
                    return Err(pi_js::Error::msg(
                        "ripgrep (rg) binary not found. Please install rg or ensure it is available in ~/.pi/agent/bin.",
                    ));
                };

                let search_path = resolve_to_cwd(input.path.as_deref().unwrap_or("."), &effective_cwd);
                let is_dir = ops.is_directory(&search_path).await?;

                let mut args = vec![
                    "--no-config".to_string(),
                    "--json".to_string(),
                    "--line-number".to_string(),
                    "--color=never".to_string(),
                    "--hidden".to_string(),
                    "--no-require-git".to_string(),
                ];

                if input.ignore_case.unwrap_or(false) {
                    args.push("--ignore-case".to_string());
                }
                if input.literal.unwrap_or(false) {
                    args.push("--fixed-strings".to_string());
                }
                if effective_context > 0 {
                    args.push("--context".to_string());
                    args.push(effective_context.to_string());
                }
                if let Some(glob_pat) = &input.glob {
                    let cleaned = clean_rg_glob(glob_pat);
                    if !cleaned.is_empty() {
                        args.push("--glob".to_string());
                        args.push(cleaned);
                    }
                }
                args.push("--".to_string());
                args.push(input.pattern.clone());

                let env_map = get_shell_env();
                let mut cmd = Command::new(&rg_bin);
                if is_dir {
                    args.push(".".to_string());
                    cmd.current_dir(&search_path);
                } else {
                    let parent = pi_js::path::dirname(&search_path);
                    let base = pi_js::path::basename(&search_path, None);
                    args.push(base);
                    if !parent.is_empty() {
                        cmd.current_dir(&parent);
                    }
                }

                cmd.args(&args)
                    .env_clear()
                    .envs(&env_map)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());

                let mut child = cmd
                    .spawn()
                    .map_err(|e| pi_js::Error::msg(format!("Failed to spawn ripgrep ({rg_bin}): {e}")))?;

                let stdout = child
                    .stdout
                    .take()
                    .ok_or_else(|| pi_js::Error::msg("Failed to capture ripgrep stdout"))?;
                let stderr = child.stderr.take();

                let stderr_task = tokio::spawn(async move {
                    if let Some(mut s) = stderr {
                        let mut buf = Vec::new();
                        let _ = tokio::io::AsyncReadExt::read_to_end(&mut s, &mut buf).await;
                        String::from_utf8_lossy(&buf).into_owned()
                    } else {
                        String::new()
                    }
                });

                let mut lines_reader = BufReader::new(stdout).lines();
                let mut recorded: IndexMap<(String, usize), (bool, String)> = IndexMap::new();
                let mut pending_before: Vec<(String, usize, String)> = Vec::new();
                let mut match_count: usize = 0;
                let mut match_limit_reached: Option<usize> = None;
                let mut last_allowed_match: Option<(String, usize)> = None;
                let mut killed_early = false;

                let is_within_trailing_context =
                    |last: &Option<(String, usize)>, file: &str, line_num: usize| -> bool {
                        if effective_context == 0 {
                            return false;
                        }
                        match last {
                            Some((last_file, last_line)) => {
                                last_file == file && line_num <= last_line.saturating_add(effective_context)
                            }
                            None => false,
                        }
                    };

                loop {
                    if signal.as_ref().is_some_and(|s| s.aborted()) {
                        let _ = child.kill().await;
                        return Err(pi_js::Error::msg("Operation aborted"));
                    }

                    let next_line = lines_reader
                        .next_line()
                        .await
                        .map_err(|e| pi_js::Error::msg(format!("Failed reading ripgrep output: {e}")))?;

                    let Some(raw_json_line) = next_line else {
                        break;
                    };
                    if raw_json_line.trim().is_empty() {
                        continue;
                    }
                    let Ok(event) = pi_js::json::parse(&raw_json_line) else {
                        continue;
                    };
                    let Some(kind) = event.get("type").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(data) = event.get("data") else {
                        continue;
                    };

                    if kind == "begin" || kind == "end" {
                        pending_before.clear();
                        continue;
                    }

                    if kind != "match" && kind != "context" {
                        continue;
                    }

                    let raw_rel_path = decode_rg_arbitrary_text(&data["path"]);
                    let file_path = if is_dir {
                        strip_dot_slash_prefix(&raw_rel_path).replace('\\', "/")
                    } else {
                        pi_js::path::basename(&search_path, None)
                    };
                    let line_number = data.get("line_number").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let raw_text = decode_rg_arbitrary_text(&data["lines"]);
                    let line_text = strip_trailing_eol(&raw_text).to_string();

                    if kind == "context" {
                        if is_within_trailing_context(&last_allowed_match, &file_path, line_number) {
                            pending_before.clear();
                            recorded.entry((file_path, line_number)).or_insert((false, line_text));
                        } else if match_count < effective_limit {
                            pending_before.push((file_path, line_number, line_text));
                        }
                        continue;
                    }

                    // kind == "match"
                    match_count += 1;
                    if match_count <= effective_limit {
                        for (ctx_file, ctx_line, ctx_text) in pending_before.drain(..) {
                            recorded.entry((ctx_file, ctx_line)).or_insert((false, ctx_text));
                        }
                        recorded
                            .entry((file_path.clone(), line_number))
                            .and_modify(|entry| {
                                entry.0 = true;
                                entry.1 = line_text.clone();
                            })
                            .or_insert((true, line_text));
                        last_allowed_match = Some((file_path, line_number));
                    } else {
                        pending_before.clear();
                        match_limit_reached = Some(effective_limit);
                        if is_within_trailing_context(&last_allowed_match, &file_path, line_number) {
                            // A match falling inside the final allowed match's trailing context is rendered as context
                            recorded.entry((file_path, line_number)).or_insert((false, line_text));
                        } else {
                            let _ = child.kill().await;
                            killed_early = true;
                            break;
                        }
                    }
                }

                let status = child
                    .wait()
                    .await
                    .map_err(|e| pi_js::Error::msg(format!("Failed waiting for ripgrep: {e}")))?;
                let stderr_text = stderr_task.await.unwrap_or_default();

                // ripgrep exits 0 (matches), 1 (no matches), or 2 (error)
                if !killed_early && status.code() == Some(2) && recorded.is_empty() {
                    let msg = stderr_text.trim();
                    return Err(pi_js::Error::msg(if msg.is_empty() {
                        "ripgrep error".to_string()
                    } else {
                        msg.to_string()
                    }));
                }

                if match_count == 0 || recorded.is_empty() {
                    return Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new("No matches found"))],
                        details: None,
                        ..Default::default()
                    });
                }

                let mut any_line_truncated = false;
                let mut formatted_lines = Vec::with_capacity(recorded.len());
                for ((file_path, line_num), (is_match, line_text)) in &recorded {
                    let truncated = truncate_line(line_text, Some(GREP_MAX_LINE_LENGTH));
                    if truncated.was_truncated {
                        any_line_truncated = true;
                    }
                    let formatted = if *is_match {
                        format!("{file_path}:{line_num}: {}", truncated.text)
                    } else {
                        format!("{file_path}-{line_num}- {}", truncated.text)
                    };
                    formatted_lines.push(formatted);
                }

                let raw_output = formatted_lines.join("\n");
                let truncation = truncate_head(
                    &raw_output,
                    Some(TruncationOptions {
                        max_lines: Some(usize::MAX),
                        max_bytes: Some(DEFAULT_MAX_BYTES),
                    }),
                );

                let mut output = truncation.content.clone();
                let mut notices = Vec::new();
                if let Some(limit) = match_limit_reached {
                    notices.push(format!(
                        "{limit} matches limit reached. Use limit={} for more, or refine pattern",
                        limit.saturating_mul(2)
                    ));
                }
                if truncation.truncated {
                    notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
                }
                if any_line_truncated {
                    notices.push(format!(
                        "Some lines truncated to {GREP_MAX_LINE_LENGTH} chars. Use read tool to inspect full lines"
                    ));
                }
                if !notices.is_empty() {
                    output.push_str(&format!("\n\n[{}]", notices.join(". ")));
                }

                let has_details = truncation.truncated || match_limit_reached.is_some() || any_line_truncated;
                let details_val = if has_details {
                    let details = GrepToolDetails {
                        truncation: if truncation.truncated { Some(truncation) } else { None },
                        match_limit_reached: match_limit_reached.map(|v| v as i64),
                        lines_truncated: if any_line_truncated { Some(true) } else { None },
                    };
                    serde_json::to_value(&details).ok()
                } else {
                    None
                };

                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(output))],
                    details: details_val,
                    ..Default::default()
                })
            })
        }),
        render_call: Some(renderers::grep::render_call()),
        render_result: Some(renderers::grep::render_result()),
    })
}

pub fn create_grep_tool(cwd: &str, options: Option<GrepToolOptions>) -> AgentTool {
    wrap_tool_definition(create_grep_tool_definition(cwd, options), None)
}
