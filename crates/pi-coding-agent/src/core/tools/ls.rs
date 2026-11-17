//! Port of packages/coding-agent/src/core/tools/ls.ts

use std::sync::Arc;

use pi_js::BoxFuture;
use serde::{Deserialize, Serialize};

use super::truncate::TruncationResult;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsToolInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_limit_reached: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LsStat {
    pub is_directory: bool,
}

/// Pluggable operations for the ls tool.
pub trait LsOperations: Send + Sync {
    fn exists(&self, absolute_path: &str) -> BoxFuture<bool>;
    fn stat(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<LsStat>>;
    fn readdir(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<Vec<String>>>;
}

#[derive(Clone, Default)]
pub struct LsToolOptions {
    pub operations: Option<Arc<dyn LsOperations>>,
}

use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_ai::types::{TextContent, UserContentBlock};
use pi_js::vendor::typebox::{Schema, t};
use serde_json::Value;

use crate::core::extensions::types::ToolDefinition;
use crate::core::tools::edit::ToolSystemPromptContribution;
use crate::core::tools::path_utils::resolve_to_cwd;
use crate::core::tools::renderers;
use crate::core::tools::tool_definition_wrapper::{
    BuiltinToolDefSpec, build_builtin_tool_definition, resolve_effective_cwd, wrap_tool_definition,
};
use crate::core::tools::truncate::{DEFAULT_MAX_BYTES, TruncationOptions, format_size, truncate_head};

const DEFAULT_LS_LIMIT: usize = 500;

struct LocalLsOperations;

impl LsOperations for LocalLsOperations {
    fn exists(&self, absolute_path: &str) -> BoxFuture<bool> {
        let path = absolute_path.to_string();
        Box::pin(async move {
            pi_js::fs::promises::access(&path, pi_js::fs::constants::F_OK)
                .await
                .is_ok()
        })
    }

    fn stat(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<LsStat>> {
        let path = absolute_path.to_string();
        Box::pin(async move {
            let meta = tokio::fs::metadata(&path)
                .await
                .map_err(|e| pi_js::Error::msg(format!("Failed to stat {path}: {e}")))?;
            Ok(LsStat {
                is_directory: meta.is_dir(),
            })
        })
    }

    fn readdir(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<Vec<String>>> {
        let path = absolute_path.to_string();
        Box::pin(async move {
            let mut entries = Vec::new();
            let mut dir = tokio::fs::read_dir(&path)
                .await
                .map_err(|e| pi_js::Error::msg(format!("Failed to read directory {path}: {e}")))?;
            while let Some(entry) = dir
                .next_entry()
                .await
                .map_err(|e| pi_js::Error::msg(format!("Failed to read directory entry: {e}")))?
            {
                if let Some(name) = entry.file_name().to_str() {
                    entries.push(name.to_string());
                }
            }
            Ok(entries)
        })
    }
}

pub fn create_local_ls_operations() -> Arc<dyn LsOperations> {
    Arc::new(LocalLsOperations)
}

pub fn ls_tool_schema() -> Schema {
    t::object([
        (
            "path",
            t::string()
                .optional()
                .with("description", "Directory to list (default: current directory)"),
        ),
        (
            "limit",
            t::number()
                .optional()
                .with("description", "Maximum number of entries to return (default: 500)"),
        ),
    ])
    .with("additionalProperties", false)
}

pub fn ls_tool_system_prompt_contribution() -> ToolSystemPromptContribution {
    ToolSystemPromptContribution {
        prompt_snippet: "List directory contents (entries sorted alphabetically, directories with trailing /)"
            .to_string(),
        prompt_guidelines: vec!["Use ls to inspect directory contents instead of running bash ls.".to_string()],
    }
}

fn parse_effective_limit(raw_limit: Option<&Value>, default_limit: usize) -> usize {
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

pub fn create_ls_tool_definition(cwd: &str, options: Option<LsToolOptions>) -> ToolDefinition {
    let default_cwd = cwd.to_string();
    let ops = options
        .and_then(|o| o.operations)
        .unwrap_or_else(create_local_ls_operations);
    let contribution = ls_tool_system_prompt_contribution();
    let desc = format!(
        "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to {} entries or {}KB (whichever is hit first).",
        DEFAULT_LS_LIMIT,
        DEFAULT_MAX_BYTES / 1024
    );

    build_builtin_tool_definition(BuiltinToolDefSpec {
        name: "ls",
        label: "ls",
        description: desc,
        prompt_snippet: Some(contribution.prompt_snippet),
        prompt_guidelines: Some(contribution.prompt_guidelines),
        parameters: ls_tool_schema(),
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
                    map.shift_remove("limit");
                }
                let input: LsToolInput = serde_json::from_value(clean_params)
                    .map_err(|e| pi_js::Error::msg(format!("Invalid ls parameters: {e}")))?;

                let dir_path = resolve_to_cwd(input.path.as_deref().unwrap_or("."), &effective_cwd);
                let effective_limit = parse_effective_limit(raw_limit.as_ref(), DEFAULT_LS_LIMIT);

                if !ops.exists(&dir_path).await {
                    return Err(pi_js::Error::msg(format!("Path not found: {dir_path}")));
                }

                let stat = ops.stat(&dir_path).await?;
                if !stat.is_directory {
                    return Err(pi_js::Error::msg(format!("Not a directory: {dir_path}")));
                }

                if signal.as_ref().is_some_and(|s| s.aborted()) {
                    return Err(pi_js::Error::msg("Operation aborted"));
                }

                let mut entries = ops.readdir(&dir_path).await?;
                entries.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));

                let mut results = Vec::new();
                let mut entry_limit_reached = false;
                for entry in entries {
                    if results.len() >= effective_limit {
                        entry_limit_reached = true;
                        break;
                    }
                    let full_path = pi_js::path::join(&[&dir_path, &entry]);
                    let suffix = match ops.stat(&full_path).await {
                        Ok(s) if s.is_directory => "/",
                        _ => "",
                    };
                    results.push(format!("{entry}{suffix}"));
                }

                if results.is_empty() {
                    return Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new("(empty directory)"))],
                        details: None,
                        ..Default::default()
                    });
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
                if entry_limit_reached {
                    notices.push(format!(
                        "{effective_limit} entries limit reached. Use limit={} for more",
                        effective_limit.saturating_mul(2)
                    ));
                }
                if truncation.truncated {
                    notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
                }
                if !notices.is_empty() {
                    output.push_str(&format!("\n\n[{}]", notices.join(". ")));
                }

                let details_val = if truncation.truncated || entry_limit_reached {
                    let details = LsToolDetails {
                        truncation: if truncation.truncated { Some(truncation) } else { None },
                        entry_limit_reached: if entry_limit_reached {
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
        render_call: Some(renderers::ls::render_call()),
        render_result: Some(renderers::ls::render_result()),
    })
}

pub fn create_ls_tool(cwd: &str, options: Option<LsToolOptions>) -> AgentTool {
    wrap_tool_definition(create_ls_tool_definition(cwd, options), None)
}
