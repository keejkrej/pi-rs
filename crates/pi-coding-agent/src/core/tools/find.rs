//! Port of packages/coding-agent/src/core/tools/find.ts

use std::sync::Arc;

use pi_js::BoxFuture;
use serde::{Deserialize, Serialize};

use super::truncate::TruncationResult;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindToolInput {
    pub pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_limit_reached: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct FindGlobOptions {
    pub ignore: Vec<String>,
    pub limit: usize,
}

/// Pluggable operations for the find tool.
pub trait FindOperations: Send + Sync {
    fn exists(&self, absolute_path: &str) -> BoxFuture<bool>;
    fn glob(&self, pattern: &str, cwd: &str, options: FindGlobOptions) -> BoxFuture<pi_js::Result<Vec<String>>>;
}

#[derive(Clone, Default)]
pub struct FindToolOptions {
    pub operations: Option<Arc<dyn FindOperations>>,
}

use std::process::Stdio;

use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_ai::types::{TextContent, UserContentBlock};
use pi_js::vendor::typebox::{Schema, t};
use serde_json::Value;
use tokio::process::Command;

use crate::core::extensions::types::ToolDefinition;
use crate::core::tools::edit::ToolSystemPromptContribution;
use crate::core::tools::mcp_wire::{ManagedTool, Minimatch, ensure_tool, get_shell_env};
use crate::core::tools::path_utils::resolve_to_cwd;
use crate::core::tools::renderers;
use crate::core::tools::tool_definition_wrapper::{
    BuiltinToolDefSpec, build_builtin_tool_definition, resolve_effective_cwd, wrap_tool_definition,
};
use crate::core::tools::truncate::{DEFAULT_MAX_BYTES, TruncationOptions, format_size, truncate_head};

const DEFAULT_FIND_LIMIT: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindPathMode {
    Native,
    Posix,
    Win32,
}

fn strip_leading_dot_slash(pattern: &str) -> &str {
    let mut pat = pattern;
    while let Some(rest) = pat.strip_prefix("./") {
        pat = rest;
    }
    while let Some(rest) = pat.strip_prefix(".\\") {
        pat = rest;
    }
    pat
}

fn escape_glob_chars(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '*' | '?' | '[' | ']' | '{' | '}' => {
                out.push('\\');
                out.push(c);
            }
            other => out.push(other),
        }
    }
    out
}

pub fn relativize_find_result_path(result_path: &str, search_path: &str, mode: Option<FindPathMode>) -> String {
    let effective_mode = match mode.unwrap_or(FindPathMode::Native) {
        FindPathMode::Native => {
            if pi_js::env::platform() == "win32" {
                FindPathMode::Win32
            } else {
                FindPathMode::Posix
            }
        }
        other => other,
    };

    if effective_mode == FindPathMode::Win32 {
        let had_trailing = result_path.ends_with('/') || result_path.ends_with('\\');
        let norm_res_raw = result_path.replace('/', "\\");
        if !pi_js::path::win32::is_absolute(&norm_res_raw) {
            let mut rel = result_path.replace('\\', "/");
            if had_trailing && !rel.ends_with('/') {
                rel.push('/');
            }
            return rel;
        }
        let norm_root = {
            let trimmed = search_path.replace('/', "\\").trim_end_matches('\\').to_string();
            if trimmed.ends_with(':') {
                format!("{trimmed}\\")
            } else {
                trimmed
            }
        };
        let norm_res_trimmed = norm_res_raw.trim_end_matches('\\').to_string();
        let lower_root = norm_root.to_lowercase();
        let lower_res = norm_res_raw.to_lowercase();
        if lower_res == lower_root || norm_res_trimmed.to_lowercase() == lower_root.trim_end_matches('\\') {
            return ".".to_string();
        }
        let prefix = if lower_root.ends_with('\\') {
            lower_root.clone()
        } else {
            format!("{lower_root}\\")
        };
        let mut out = if lower_res.starts_with(&prefix) {
            norm_res_raw[prefix.len()..].replace('\\', "/")
        } else {
            let rel = pi_js::path::win32::relative(&norm_root, &norm_res_trimmed);
            if rel.is_empty() {
                ".".to_string()
            } else {
                rel.replace('\\', "/")
            }
        };
        if had_trailing && out != "." && !out.ends_with('/') {
            out.push('/');
        }
        out
    } else {
        let had_trailing = result_path.ends_with('/');
        if !pi_js::path::posix::is_absolute(result_path) {
            return result_path.to_string();
        }
        let norm_root = if search_path == "/" {
            "/".to_string()
        } else {
            search_path.trim_end_matches('/').to_string()
        };
        if result_path == norm_root {
            return ".".to_string();
        }
        let prefix = if norm_root == "/" {
            "/".to_string()
        } else {
            format!("{norm_root}/")
        };
        let mut out = if let Some(stripped) = result_path.strip_prefix(&prefix) {
            stripped.to_string()
        } else {
            let trimmed_res = result_path.trim_end_matches('/');
            let rel = pi_js::path::posix::relative(&norm_root, trimmed_res);
            if rel.is_empty() { ".".to_string() } else { rel }
        };
        if had_trailing && out != "." && !out.ends_with('/') {
            out.push('/');
        }
        out
    }
}

struct LocalFindOperations;

impl FindOperations for LocalFindOperations {
    fn exists(&self, absolute_path: &str) -> BoxFuture<bool> {
        let path = absolute_path.to_string();
        Box::pin(async move {
            pi_js::fs::promises::access(&path, pi_js::fs::constants::F_OK)
                .await
                .is_ok()
        })
    }

    fn glob(&self, pattern: &str, cwd: &str, options: FindGlobOptions) -> BoxFuture<pi_js::Result<Vec<String>>> {
        let raw_pattern = pattern.to_string();
        let search_path = cwd.to_string();
        Box::pin(async move {
            let Some(fd_bin) = ensure_tool(ManagedTool::Fd, true).await? else {
                return Err(pi_js::Error::msg(
                    "fd binary not found. Please install fd or ensure it is available in ~/.pi/agent/bin.",
                ));
            };

            let raw_no_dot = strip_leading_dot_slash(&raw_pattern);
            let clean_pattern = if pi_js::env::platform() == "win32" {
                raw_no_dot.replace('\\', "/")
            } else {
                raw_no_dot.to_string()
            };
            let clean_pattern = if clean_pattern.is_empty() {
                "*".to_string()
            } else {
                clean_pattern
            };

            let limit_str = options.limit.max(1).to_string();
            let mut args = vec![
                "--glob".to_string(),
                "--color=never".to_string(),
                "--hidden".to_string(),
                "--no-require-git".to_string(),
                "--max-results".to_string(),
                limit_str,
            ];

            for ig in &options.ignore {
                args.push("--exclude".to_string());
                args.push(ig.clone());
            }

            let is_relative_path_pattern = clean_pattern.contains('/');
            let env_map = get_shell_env();
            let is_win32 = pi_js::env::platform() == "win32";

            let root_for_glob = if is_win32 {
                search_path.replace('\\', "/").trim_end_matches('/').to_string()
            } else if search_path == "/" {
                String::new()
            } else {
                search_path.trim_end_matches('/').to_string()
            };
            let escaped_root = escape_glob_chars(&root_for_glob);

            let (fd_pattern, use_full_path) = if is_relative_path_pattern {
                let pat = if clean_pattern.starts_with('/') {
                    clean_pattern.clone()
                } else {
                    format!("{escaped_root}/{clean_pattern}")
                };
                (pat, true)
            } else if is_win32 {
                (format!("{escaped_root}/**/{clean_pattern}"), true)
            } else {
                (clean_pattern.clone(), false)
            };

            if use_full_path {
                args.push("--full-path".to_string());
            }
            args.push("--".to_string());
            args.push(fd_pattern);
            args.push(search_path.clone());

            let mut cmd = Command::new(&fd_bin);
            cmd.args(&args)
                .env_clear()
                .envs(&env_map)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());

            let output = cmd
                .output()
                .await
                .map_err(|e| pi_js::Error::msg(format!("Failed to execute fd ({fd_bin}): {e}")))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                if !stderr.is_empty() {
                    return Err(pi_js::Error::msg(format!("fd error: {stderr}")));
                }
                if output.status.code() != Some(1) {
                    return Err(pi_js::Error::msg(format!(
                        "fd exited with code {}",
                        output.status.code().unwrap_or(1)
                    )));
                }
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut results: Vec<String> = stdout
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|line| relativize_find_result_path(line, &search_path, None))
                .map(|rel| rel.trim_end_matches('/').to_string())
                .collect();

            if is_win32 && let Ok(matcher) = Minimatch::new(&clean_pattern, true) {
                results.retain(|r| {
                    let base = r.rsplit('/').next().unwrap_or(r.as_str());
                    matcher.is_match(base) || matcher.is_match(r)
                });
            }

            Ok(results)
        })
    }
}

pub fn create_local_find_operations() -> Arc<dyn FindOperations> {
    Arc::new(LocalFindOperations)
}

pub fn find_tool_schema() -> Schema {
    t::object([
        (
            "pattern",
            t::string().with(
                "description",
                "Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'",
            ),
        ),
        (
            "path",
            t::string()
                .optional()
                .with("description", "Directory to search in (default: current directory)"),
        ),
        (
            "limit",
            t::number()
                .optional()
                .with("description", "Maximum number of results (default: 1000)"),
        ),
    ])
    .with("additionalProperties", false)
}

pub fn find_tool_system_prompt_contribution() -> ToolSystemPromptContribution {
    ToolSystemPromptContribution {
        prompt_snippet: "Find files by glob pattern (respects .gitignore)".to_string(),
        prompt_guidelines: vec!["Use find to locate files by filename/path pattern instead of bash find.".to_string()],
    }
}

fn parse_effective_find_limit(raw_limit: Option<&Value>, default_limit: usize) -> usize {
    match raw_limit {
        Some(Value::Number(n)) => {
            if let Some(f) = n.as_f64() {
                if f.is_finite() {
                    (f.floor() as isize).max(1) as usize
                } else {
                    default_limit
                }
            } else {
                default_limit
            }
        }
        _ => default_limit,
    }
}

pub fn create_find_tool_definition(cwd: &str, options: Option<FindToolOptions>) -> ToolDefinition {
    let default_cwd = cwd.to_string();
    let is_custom_ops = options.as_ref().and_then(|o| o.operations.as_ref()).is_some();
    let ops = options
        .and_then(|o| o.operations)
        .unwrap_or_else(create_local_find_operations);
    let contribution = find_tool_system_prompt_contribution();
    let desc = format!(
        "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to {} results or {}KB (whichever is hit first).",
        DEFAULT_FIND_LIMIT,
        DEFAULT_MAX_BYTES / 1024
    );

    build_builtin_tool_definition(BuiltinToolDefSpec {
        name: "find",
        label: "find",
        description: desc,
        prompt_snippet: Some(contribution.prompt_snippet),
        prompt_guidelines: Some(contribution.prompt_guidelines),
        parameters: find_tool_schema(),
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
                let mut clean_params = params;
                if let Value::Object(ref mut map) = clean_params {
                    map.remove("limit");
                }
                let input: FindToolInput = serde_json::from_value(clean_params)
                    .map_err(|e| pi_js::Error::msg(format!("Invalid find parameters: {e}")))?;

                let search_dir = resolve_to_cwd(input.path.as_deref().unwrap_or("."), &effective_cwd);
                let effective_limit = parse_effective_find_limit(raw_limit.as_ref(), DEFAULT_FIND_LIMIT);

                if !ops.exists(&search_dir).await {
                    return Err(pi_js::Error::msg(format!("Path not found: {search_dir}")));
                }

                let requested_limit = effective_limit.saturating_add(1);
                let raw_results = ops
                    .glob(
                        &input.pattern,
                        &search_dir,
                        FindGlobOptions {
                            ignore: vec![],
                            limit: requested_limit,
                        },
                    )
                    .await?;

                let mut results: Vec<String> = raw_results
                    .into_iter()
                    .map(|p| relativize_find_result_path(&p, &search_dir, None))
                    .collect();

                if results.is_empty() {
                    return Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new(
                            "No files found matching pattern",
                        ))],
                        details: None,
                        ..Default::default()
                    });
                }

                let result_limit_reached = results.len() > effective_limit;
                if result_limit_reached {
                    results.truncate(effective_limit);
                }

                let raw_output = results.join("\n");
                let truncation = truncate_head(
                    &raw_output,
                    Some(TruncationOptions {
                        max_lines: Some(usize::MAX),
                        max_bytes: Some(DEFAULT_MAX_BYTES),
                    }),
                );

                let mut output = truncation.content.clone();
                let mut notices = Vec::new();
                if result_limit_reached {
                    if is_custom_ops {
                        notices.push(format!("{effective_limit} results limit reached"));
                    } else {
                        notices.push(format!(
                            "{effective_limit} results limit reached. Use limit={} for more, or refine pattern",
                            effective_limit.saturating_mul(2)
                        ));
                    }
                }
                if truncation.truncated {
                    notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
                }
                if !notices.is_empty() {
                    output.push_str(&format!("\n\n[{}]", notices.join(". ")));
                }

                let details_val = if truncation.truncated || result_limit_reached {
                    let details = FindToolDetails {
                        truncation: if truncation.truncated { Some(truncation) } else { None },
                        result_limit_reached: if result_limit_reached {
                            Some(effective_limit as i64)
                        } else {
                            None
                        },
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
        render_call: Some(renderers::find::render_call()),
        render_result: Some(renderers::find::render_result()),
    })
}

pub fn create_find_tool(cwd: &str, options: Option<FindToolOptions>) -> AgentTool {
    wrap_tool_definition(create_find_tool_definition(cwd, options), None)
}
