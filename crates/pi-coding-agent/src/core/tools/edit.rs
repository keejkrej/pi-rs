//! Port of packages/coding-agent/src/core/tools/edit.ts

use std::sync::Arc;

use pi_js::BoxFuture;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditReplacement {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolInput {
    pub path: String,
    pub edits: Vec<EditReplacement>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolDetails {
    /// Display-oriented diff of the changes made
    pub diff: String,
    /// Standard unified patch of the changes made
    pub patch: String,
    /// Line number of the first change in the new file (for editor navigation)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_changed_line: Option<i64>,
}

/// Pluggable operations for the edit tool.
pub trait EditOperations: Send + Sync {
    fn read_file(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<Vec<u8>>>;
    fn write_file(&self, absolute_path: &str, content: &str) -> BoxFuture<pi_js::Result<()>>;
    fn access(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<()>>;
}

#[derive(Clone, Default)]
pub struct EditToolOptions {
    pub operations: Option<Arc<dyn EditOperations>>,
}

struct LocalEditOperations;

impl EditOperations for LocalEditOperations {
    fn read_file(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<Vec<u8>>> {
        let path = absolute_path.to_string();
        Box::pin(async move {
            tokio::fs::read(&path)
                .await
                .map_err(|e| pi_js::Error::msg(e.to_string()))
        })
    }

    fn write_file(&self, absolute_path: &str, content: &str) -> BoxFuture<pi_js::Result<()>> {
        let path = absolute_path.to_string();
        let data = content.as_bytes().to_vec();
        Box::pin(async move {
            tokio::fs::write(&path, &data)
                .await
                .map_err(|e| pi_js::Error::msg(e.to_string()))
        })
    }

    fn access(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<()>> {
        let path = absolute_path.to_string();
        Box::pin(async move {
            let mode = pi_js::fs::constants::R_OK | pi_js::fs::constants::W_OK;
            pi_js::fs::promises::access(&path, mode).await
        })
    }
}

pub fn create_local_edit_operations() -> Arc<dyn EditOperations> {
    Arc::new(LocalEditOperations)
}

use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_ai::types::{TextContent, UserContentBlock};
use pi_js::vendor::typebox::{Schema, t};
use serde_json::{Map, Value};

use crate::core::extensions::types::{RenderShell, ToolDefinition};
use crate::core::tools::edit_diff::{
    apply_edits_to_normalized_content, apply_replacements_to_original, create_edit_patch, generate_diff_string,
    normalize_to_lf,
};
use crate::core::tools::file_mutation_queue::with_file_mutation_queue;
use crate::core::tools::path_utils::resolve_to_cwd;
use crate::core::tools::renderers;
use crate::core::tools::tool_definition_wrapper::{
    BuiltinToolDefSpec, build_builtin_tool_definition, format_edit_access_error, resolve_effective_cwd,
    strict_prefer_sampling, wrap_tool_definition,
};
use crate::utils::text::split_bom;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSystemPromptContribution {
    pub prompt_snippet: String,
    pub prompt_guidelines: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EditDiffPreview {
    Success {
        diff: String,
        #[serde(rename = "firstChangedLine", skip_serializing_if = "Option::is_none")]
        first_changed_line: Option<usize>,
        patch: String,
    },
    Error {
        error: String,
    },
}

pub fn edit_tool_schema() -> Schema {
    let edit_item = t::object([
        (
            "oldText",
            t::string()
                .with("description", "Exact text for one targeted replacement (must be unique in the file and match including whitespace and newlines)."),
        ),
        (
            "newText",
            t::string()
                .with("description", "Replacement text for this matched block."),
        ),
    ])
    .with("additionalProperties", false);

    t::object([
        (
            "path",
            t::string().with("description", "Path to the file to edit (relative or absolute)"),
        ),
        (
            "edits",
            t::array(edit_item).with(
                "description",
                "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping edits.",
            ),
        ),
    ])
    .with("additionalProperties", false)
}

/// Convert legacy `{ oldText, newText }` top-level arguments and JSON-stringified `edits` into `edits: [...]`.
pub fn prepare_edit_arguments(args: Value) -> Value {
    let Value::Object(mut obj) = args else {
        return args;
    };

    if let Some(Value::String(raw_edits)) = obj.get("edits")
        && let Ok(Value::Array(parsed)) = pi_js::json::parse(raw_edits)
    {
        obj.insert("edits".to_string(), Value::Array(parsed));
    }

    let has_legacy_pair =
        obj.get("oldText").is_some_and(Value::is_string) && obj.get("newText").is_some_and(Value::is_string);

    if has_legacy_pair {
        if let (Some(old_text), Some(new_text)) = (obj.shift_remove("oldText"), obj.shift_remove("newText")) {
            let mut item = Map::new();
            item.insert("oldText".to_string(), old_text);
            item.insert("newText".to_string(), new_text);
            let legacy_entry = Value::Object(item);
            if let Some(Value::Array(existing)) = obj.get_mut("edits") {
                existing.push(legacy_entry);
            } else {
                obj.insert("edits".to_string(), Value::Array(vec![legacy_entry]));
            }
        }
    }

    Value::Object(obj)
}

pub fn validate_edit_input(input: &EditToolInput) -> Result<(), pi_js::Error> {
    if input.edits.is_empty() {
        return Err(pi_js::Error::msg("Invalid arguments: edits must be a non-empty array."));
    }
    Ok(())
}

pub fn edit_tool_system_prompt_contribution() -> ToolSystemPromptContribution {
    ToolSystemPromptContribution {
        prompt_snippet: "Replace exact text in a file (supports multiple non-overlapping edits per call)".to_string(),
        prompt_guidelines: vec![
            "Use edit for precise, targeted modifications to existing files. Prefer multiple entries in `edits` over multiple sequential edit calls on the same file.".to_string(),
            "Each `oldText` must match the original file exactly, including whitespace, indentation, and newlines.".to_string(),
            "Include enough surrounding context in each `oldText` so it matches uniquely within the file.".to_string(),
            "Edits are all matched against the original file before any replacement is applied and must not overlap.".to_string(),
        ],
    }
}

pub async fn compute_edits_diff(
    path: &str,
    edits: &[EditReplacement],
    cwd: &str,
    operations: Option<&dyn EditOperations>,
) -> EditDiffPreview {
    if edits.is_empty() {
        return EditDiffPreview::Error {
            error: "Invalid arguments: edits must be a non-empty array.".to_string(),
        };
    }

    let local = create_local_edit_operations();
    let ops = operations.unwrap_or(local.as_ref());
    let absolute_path = resolve_to_cwd(path, cwd);

    if let Err(err) = ops.access(&absolute_path).await {
        return EditDiffPreview::Error {
            error: format_edit_access_error(path, &err),
        };
    }

    let raw_bytes = match ops.read_file(&absolute_path).await {
        Ok(b) => b,
        Err(err) => {
            return EditDiffPreview::Error {
                error: format_edit_access_error(path, &err),
            };
        }
    };

    let raw_content = String::from_utf8_lossy(&raw_bytes).into_owned();
    let (_bom, content) = split_bom(&raw_content);
    let normalized = normalize_to_lf(content);
    let pairs: Vec<(&str, &str)> = edits
        .iter()
        .map(|e| (e.old_text.as_str(), e.new_text.as_str()))
        .collect();

    let applied = match apply_edits_to_normalized_content(&normalized, &pairs, path) {
        Ok(a) => a,
        Err(err) => {
            return EditDiffPreview::Error { error: err.to_string() };
        }
    };

    let diff_res = generate_diff_string(&normalized, &applied.new_content, None);
    let patch = create_edit_patch(path, &normalized, &applied.new_content);

    EditDiffPreview::Success {
        diff: diff_res.diff,
        first_changed_line: diff_res.first_changed_line,
        patch,
    }
}

pub fn create_edit_tool_definition(cwd: &str, options: Option<EditToolOptions>) -> ToolDefinition {
    let default_cwd = cwd.to_string();
    let ops = options
        .and_then(|o| o.operations)
        .unwrap_or_else(create_local_edit_operations);
    let contribution = edit_tool_system_prompt_contribution();

    build_builtin_tool_definition(BuiltinToolDefSpec {
        name: "edit",
        label: "edit",
        description: "Edit a file by replacing exact text. Supports making multiple disjoint edits in a single call via the `edits` array. Each `oldText` must be unique in the file and match exactly (including whitespace and newlines).".to_string(),
        prompt_snippet: Some(contribution.prompt_snippet),
        prompt_guidelines: Some(contribution.prompt_guidelines),
        parameters: edit_tool_schema(),
        constrained_sampling: strict_prefer_sampling(),
        render_shell: Some(RenderShell::Self_),
        prepare_arguments: Some(Arc::new(prepare_edit_arguments)),
        output_schema: None,
        execute: Arc::new(move |_tool_call_id, params, signal, _on_update, ctx| {
            let ops = Arc::clone(&ops);
            let effective_cwd = resolve_effective_cwd(&ctx, &default_cwd);
            Box::pin(async move {
                if signal.as_ref().is_some_and(|s| s.aborted()) {
                    return Err(pi_js::Error::msg("Operation aborted"));
                }

                let prepared = prepare_edit_arguments(params);
                let input: EditToolInput = serde_json::from_value(prepared)
                    .map_err(|e| pi_js::Error::msg(format!("Invalid edit parameters: {e}")))?;
                validate_edit_input(&input)?;

                let absolute_path = resolve_to_cwd(&input.path, &effective_cwd);
                let queue_key = absolute_path.clone();
                let display_path = input.path.clone();
                let edits = input.edits.clone();

                with_file_mutation_queue(&queue_key, || async move {
                    if signal.as_ref().is_some_and(|s| s.aborted()) {
                        return Err(pi_js::Error::msg("Operation aborted"));
                    }

                    ops.access(&absolute_path)
                        .await
                        .map_err(|err| pi_js::Error::msg(format_edit_access_error(&display_path, &err)))?;

                    if signal.as_ref().is_some_and(|s| s.aborted()) {
                        return Err(pi_js::Error::msg("Operation aborted"));
                    }

                    let raw_bytes = ops
                        .read_file(&absolute_path)
                        .await
                        .map_err(|err| pi_js::Error::msg(format_edit_access_error(&display_path, &err)))?;

                    let raw_content = String::from_utf8_lossy(&raw_bytes).into_owned();
                    let (bom, content) = split_bom(&raw_content);
                    let normalized = normalize_to_lf(content);
                    let pairs: Vec<(&str, &str)> = edits
                        .iter()
                        .map(|e| (e.old_text.as_str(), e.new_text.as_str()))
                        .collect();

                    let applied = apply_edits_to_normalized_content(&normalized, &pairs, &display_path)?;
                    let updated_body = apply_replacements_to_original(content, &applied.replacements);
                    let final_content = format!("{bom}{updated_body}");

                    if signal.as_ref().is_some_and(|s| s.aborted()) {
                        return Err(pi_js::Error::msg("Operation aborted"));
                    }

                    ops.write_file(&absolute_path, &final_content).await?;

                    if signal.as_ref().is_some_and(|s| s.aborted()) {
                        return Err(pi_js::Error::msg("Operation aborted"));
                    }

                    let diff_res = generate_diff_string(&normalized, &applied.new_content, None);
                    let patch = create_edit_patch(&display_path, &normalized, &applied.new_content);
                    let details_val = serde_json::to_value(&serde_json::json!({
                        "diff": diff_res.diff,
                        "firstChangedLine": diff_res.first_changed_line,
                        "patch": patch,
                    }))
                    .ok();

                    let message = format!(
                        "Successfully replaced {} block(s) in {display_path}.",
                        edits.len()
                    );

                    Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new(message))],
                        details: details_val,
                        ..Default::default()
                    })
                })
                .await
            })
        }),
        render_call: Some(renderers::edit::render_call()),
        render_result: Some(renderers::edit::render_result()),
    })
}

pub fn create_edit_tool(cwd: &str, options: Option<EditToolOptions>) -> AgentTool {
    wrap_tool_definition(create_edit_tool_definition(cwd, options), None)
}
