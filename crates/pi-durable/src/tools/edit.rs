//! Port of packages/durable/src/tools/edit.ts

use std::sync::Arc;

use pi_js::vendor::typebox::t;
use serde::{Deserialize, Serialize};

use crate::env::{FileContent, FileError, FileErrorCode, FileKind};
use crate::harness::define::define_tool;
use crate::harness::types::{ToolExecutionResult, ToolRegistration, ToolResultContentBlock};
use crate::tools::edit_diff::{
    Edit, apply_edits_to_normalized_content, detect_line_ending, generate_diff_string, generate_unified_patch,
    normalize_to_lf, restore_line_endings, strip_bom,
};
use crate::tools::env::require_env;
use crate::tools::file_mutation_queue::with_file_mutation_queue;
use crate::tools::path_utils::resolve_tool_path;
use crate::types::JsonValue;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolInput {
    pub path: String,
    pub edits: Vec<Edit>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolDetails {
    pub diff: String,
    pub patch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_changed_line: Option<usize>,
}

fn edit_schema() -> pi_js::vendor::typebox::Schema {
    let replace_edit_schema = t::object([
        (
            "oldText",
            t::string().with(
                "description",
                "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call.",
            ),
        ),
        (
            "newText",
            t::string().with("description", "Replacement text for this targeted edit."),
        ),
    ]);
    t::object([
        (
            "path",
            t::string().with("description", "Path to the file to edit (relative or absolute)"),
        ),
        (
            "edits",
            t::array(replace_edit_schema).with(
                "description",
                "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
            ),
        ),
    ])
}

fn is_single_edit_input(value: &JsonValue) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.get("oldText").is_some_and(|v| v.is_string()) && obj.get("newText").is_some_and(|v| v.is_string())
}

/// Repair shapes models commonly send: `edits` as a JSON string or as a single edit object,
/// and a top-level `oldText`/`newText` pair. Works on a copy; the input remains unchanged.
pub fn prepare_edit_arguments(input: &JsonValue) -> pi_js::Result<JsonValue> {
    let Some(obj) = input.as_object() else {
        return Ok(input.clone());
    };
    let mut args = obj.clone();
    match args.get("edits").cloned() {
        Some(JsonValue::String(raw)) => {
            if let Ok(parsed) = pi_js::json::parse(&raw) {
                if parsed.is_array() {
                    args.insert("edits".to_owned(), parsed);
                } else if is_single_edit_input(&parsed) {
                    args.insert("edits".to_owned(), JsonValue::Array(vec![parsed]));
                }
            }
        }
        Some(ref single) if is_single_edit_input(single) => {
            args.insert("edits".to_owned(), JsonValue::Array(vec![single.clone()]));
        }
        _ => {}
    }

    if let (Some(JsonValue::String(old_text)), Some(JsonValue::String(new_text))) =
        (args.get("oldText").cloned(), args.get("newText").cloned())
    {
        let mut edits = args
            .get("edits")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut legacy_edit = serde_json::Map::new();
        legacy_edit.insert("oldText".to_owned(), JsonValue::String(old_text));
        legacy_edit.insert("newText".to_owned(), JsonValue::String(new_text));
        edits.push(JsonValue::Object(legacy_edit));
        args.shift_remove("oldText");
        args.shift_remove("newText");
        args.insert("edits".to_owned(), JsonValue::Array(edits));
    }

    Ok(JsonValue::Object(args))
}

fn file_error_code_str(code: FileErrorCode) -> &'static str {
    match code {
        FileErrorCode::Aborted => "aborted",
        FileErrorCode::NotFound => "not_found",
        FileErrorCode::PermissionDenied => "permission_denied",
        FileErrorCode::NotDirectory => "not_directory",
        FileErrorCode::IsDirectory => "is_directory",
        FileErrorCode::Invalid => "invalid",
        FileErrorCode::NotSupported => "not_supported",
        FileErrorCode::Unknown => "unknown",
    }
}

fn edit_access_error(path: &str, error: FileError) -> pi_js::Error {
    let code = file_error_code_str(error.code);
    pi_js::Error::Js(pi_js::JsError {
        name: "Error".to_owned(),
        message: format!("Could not edit file: {path}. Error code: {code}."),
        code: None,
        cause: Some(Box::new(error.into())),
    })
}

pub fn create_edit_tool() -> ToolRegistration {
    define_tool(ToolRegistration {
        name: "edit".to_owned(),
        description: "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.".to_owned(),
        parameters: edit_schema().to_value(),
        replay: None,
        execution_mode: None,
        prepare_arguments: Some(Arc::new(prepare_edit_arguments)),
        output_limits: None,
        execute: Arc::new(|args_val, api, context| {
            Box::pin(async move {
                let args: EditToolInput = serde_json::from_value(args_val)
                    .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
                if args.edits.is_empty() {
                    return Err(pi_js::Error::msg(
                        "Edit tool input is invalid. edits must contain at least one replacement.",
                    ));
                }
                let EditToolInput { path, edits } = args;
                let env = require_env(&api)?;
                let absolute_path = resolve_tool_path(env.as_ref(), &path, &context).await?;
                let absolute_path_task = absolute_path.clone();
                let env_for_queue = Arc::clone(&env);
                let queue_ctx = context.clone();
                with_file_mutation_queue(
                    env_for_queue.as_ref(),
                    &absolute_path,
                    move || {
                        let env = Arc::clone(&env);
                        let context = queue_ctx;
                        async move {
                            if context.abort_signal().is_some_and(|s| s.aborted()) {
                                return Err(pi_js::Error::msg("Operation aborted"));
                            }
                            let info = env
                                .file_info(&absolute_path_task, &context)
                                .await
                                .map_err(|err| edit_access_error(&path, err))?;
                            if !matches!(info.kind, FileKind::File | FileKind::Symlink) {
                                return Err(pi_js::Error::msg(format!(
                                    "Could not edit file: {path}. Path is not a file."
                                )));
                            }

                            let raw_content = env
                                .read_text_file(&absolute_path_task, &context)
                                .await
                                .map_err(|err| edit_access_error(&path, err))?;
                            if context.abort_signal().is_some_and(|s| s.aborted()) {
                                return Err(pi_js::Error::msg("Operation aborted"));
                            }

                            let (bom, content) = strip_bom(&raw_content);
                            let original_ending = detect_line_ending(content);
                            let normalized_content = normalize_to_lf(content);
                            let applied = apply_edits_to_normalized_content(&normalized_content, &edits, &path)?;
                            if context.abort_signal().is_some_and(|s| s.aborted()) {
                                return Err(pi_js::Error::msg("Operation aborted"));
                            }

                            let mut final_content = String::with_capacity(bom.len() + applied.new_content.len());
                            final_content.push_str(bom);
                            final_content.push_str(&restore_line_endings(&applied.new_content, original_ending));

                            env.write_file(&absolute_path_task, FileContent::Text(&final_content), &context)
                                .await
                                .map_err(|err| edit_access_error(&path, err))?;
                            if context.abort_signal().is_some_and(|s| s.aborted()) {
                                return Err(pi_js::Error::msg("Operation aborted"));
                            }

                            let diff_result = generate_diff_string(&applied.base_content, &applied.new_content, 4);
                            let details = EditToolDetails {
                                diff: diff_result.diff,
                                patch: generate_unified_patch(&path, &applied.base_content, &applied.new_content, 4),
                                first_changed_line: diff_result.first_changed_line,
                            };
                            let details_json = serde_json::to_value(details)
                                .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
                            Ok(ToolExecutionResult {
                                content: Some(vec![ToolResultContentBlock::Text(
                                    pi_ai::types::TextContent::new(format!(
                                        "Successfully replaced {} block(s) in {path}.",
                                        edits.len()
                                    )),
                                )]),
                                details: Some(details_json),
                                ..Default::default()
                            })
                        }
                    },
                    &context,
                )
                .await
            })
        }),
    })
}
