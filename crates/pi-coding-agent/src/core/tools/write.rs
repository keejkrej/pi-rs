//! Port of packages/coding-agent/src/core/tools/write.ts

use std::sync::Arc;

use pi_js::BoxFuture;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteToolInput {
    pub path: String,
    pub content: String,
}

/// Pluggable operations for the write tool.
pub trait WriteOperations: Send + Sync {
    fn write_file(&self, absolute_path: &str, content: &str) -> BoxFuture<pi_js::Result<()>>;
    fn mkdir(&self, dir: &str) -> BoxFuture<pi_js::Result<()>>;
}

#[derive(Clone, Default)]
pub struct WriteToolOptions {
    pub operations: Option<Arc<dyn WriteOperations>>,
}

use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_ai::types::{TextContent, UserContentBlock};
use pi_js::vendor::typebox::{Schema, t};

use crate::core::extensions::types::ToolDefinition;
use crate::core::tools::edit::ToolSystemPromptContribution;
use crate::core::tools::file_mutation_queue::with_file_mutation_queue;
use crate::core::tools::path_utils::resolve_to_cwd;
use crate::core::tools::renderers;
use crate::core::tools::tool_definition_wrapper::{
    BuiltinToolDefSpec, build_builtin_tool_definition, resolve_effective_cwd, strict_prefer_sampling,
    wrap_tool_definition,
};

struct LocalWriteOperations;

impl WriteOperations for LocalWriteOperations {
    fn write_file(&self, absolute_path: &str, content: &str) -> BoxFuture<pi_js::Result<()>> {
        let path = absolute_path.to_string();
        let bytes = content.as_bytes().to_vec();
        Box::pin(async move {
            tokio::fs::write(&path, &bytes)
                .await
                .map_err(|e| pi_js::Error::msg(e.to_string()))
        })
    }

    fn mkdir(&self, dir: &str) -> BoxFuture<pi_js::Result<()>> {
        let path = dir.to_string();
        Box::pin(async move {
            tokio::fs::create_dir_all(&path)
                .await
                .map_err(|e| pi_js::Error::msg(e.to_string()))
        })
    }
}

pub fn create_local_write_operations() -> Arc<dyn WriteOperations> {
    Arc::new(LocalWriteOperations)
}

pub fn write_tool_schema() -> Schema {
    t::object([
        (
            "path",
            t::string().with("description", "Path to the file to write (relative or absolute)"),
        ),
        (
            "content",
            t::string().with("description", "Content to write to the file"),
        ),
    ])
    .with("additionalProperties", false)
}

pub fn write_tool_system_prompt_contribution() -> ToolSystemPromptContribution {
    ToolSystemPromptContribution {
        prompt_snippet: "Write content to a file (creates parent directories automatically)"
            .to_string(),
        prompt_guidelines: vec![
            "Use write only for new files or complete rewrites; prefer edit for partial modifications to existing files.".to_string(),
        ],
    }
}

pub fn create_write_tool_definition(cwd: &str, options: Option<WriteToolOptions>) -> ToolDefinition {
    let default_cwd = cwd.to_string();
    let ops = options
        .and_then(|o| o.operations)
        .unwrap_or_else(create_local_write_operations);
    let contribution = write_tool_system_prompt_contribution();

    build_builtin_tool_definition(BuiltinToolDefSpec {
        name: "write",
        label: "write",
        description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.".to_string(),
        prompt_snippet: Some(contribution.prompt_snippet),
        prompt_guidelines: Some(contribution.prompt_guidelines),
        parameters: write_tool_schema(),
        constrained_sampling: strict_prefer_sampling(),
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

                let input: WriteToolInput = serde_json::from_value(params)
                    .map_err(|e| pi_js::Error::msg(format!("Invalid write parameters: {e}")))?;

                let absolute_path = resolve_to_cwd(&input.path, &effective_cwd);
                let queue_key = absolute_path.clone();
                let display_path = input.path.clone();
                let content = input.content.clone();

                with_file_mutation_queue(&queue_key, || async move {
                    if signal.as_ref().is_some_and(|s| s.aborted()) {
                        return Err(pi_js::Error::msg("Operation aborted"));
                    }

                    let parent_dir = pi_js::path::dirname(&absolute_path);
                    if !parent_dir.is_empty() {
                        ops.mkdir(&parent_dir).await?;
                    }

                    if signal.as_ref().is_some_and(|s| s.aborted()) {
                        return Err(pi_js::Error::msg("Operation aborted"));
                    }

                    ops.write_file(&absolute_path, &content).await?;

                    if signal.as_ref().is_some_and(|s| s.aborted()) {
                        return Err(pi_js::Error::msg("Operation aborted"));
                    }

                    Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new(format!(
                            "Successfully wrote to {display_path}"
                        )))],
                        details: None,
                        ..Default::default()
                    })
                })
                .await
            })
        }),
        render_call: Some(renderers::write::render_call()),
        render_result: Some(renderers::write::render_result()),
    })
}

pub fn create_write_tool(cwd: &str, options: Option<WriteToolOptions>) -> AgentTool {
    wrap_tool_definition(create_write_tool_definition(cwd, options), None)
}
