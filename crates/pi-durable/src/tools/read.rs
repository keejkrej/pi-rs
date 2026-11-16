//! Port of packages/durable/src/tools/read.ts

use std::sync::Arc;

use pi_js::vendor::typebox::t;
use serde::{Deserialize, Serialize};

use crate::env::get_or_throw;
use crate::harness::define::define_tool;
use crate::harness::output::character_end;
use crate::harness::types::{
    DiagnosticSeverity, ToolDiagnostic, ToolExecutionResult, ToolRegistration, ToolResultContentBlock,
};
use crate::tools::env::require_env;
use crate::tools::image::detect_supported_image_mime_type;
use crate::tools::path_utils::resolve_read_tool_path;
use crate::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, format_size, truncate_head};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolInput {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// Omit<TruncationResult, "content">.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTruncationDetails {
    pub truncated: bool,
    pub truncated_by: Option<TruncatedBy>,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<ReadTruncationDetails>,
}

fn read_schema() -> pi_js::vendor::typebox::Schema {
    t::object([
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
    ])
}

/// Reads text files. Remarks about truncation and continuation are diagnostics; the content is only file text.
pub fn create_read_tool() -> ToolRegistration {
    define_tool(ToolRegistration {
        name: "read".to_owned(),
        description: format!(
            "Read the contents of a text file. Output is truncated to {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
            DEFAULT_MAX_BYTES / 1024
        ),
        parameters: read_schema().to_value(),
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        output_limits: None,
        execute: Arc::new(|args_val, api, context| {
            Box::pin(async move {
                let args: ReadToolInput =
                    serde_json::from_value(args_val).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
                let ReadToolInput { path, offset, limit } = args;
                let env = require_env(&api)?;
                let absolute_path = resolve_read_tool_path(env.as_ref(), &path, &context).await?;
                let bytes = get_or_throw(env.read_binary_file(&absolute_path, &context).await)?;
                if let Some(mime_type) = detect_supported_image_mime_type(&bytes) {
                    return Ok(ToolExecutionResult {
                        content: Some(Vec::new()),
                        is_error: Some(true),
                        diagnostics: Some(vec![ToolDiagnostic {
                            severity: DiagnosticSeverity::Error,
                            code: Some("unsupported_image".to_owned()),
                            message: format!("{path} is an image ({mime_type}); reading images is not supported"),
                        }]),
                        ..Default::default()
                    });
                }

                let text_content = pi_js::text::decode_utf8(&bytes);
                let all_lines: Vec<&str> = text_content.split('\n').collect();
                let total_file_lines = all_lines.len();
                let start_line = offset.map(|n| n.saturating_sub(1)).unwrap_or(0);
                let start_line_display = start_line + 1;
                if start_line >= all_lines.len() {
                    let offset_display = offset.map(|n| n.to_string()).unwrap_or_else(|| "undefined".to_owned());
                    return Err(pi_js::Error::msg(format!(
                        "Offset {offset_display} is beyond end of file ({total_file_lines} lines total)"
                    )));
                }

                let (selected_content, user_limited_lines) = if let Some(limit) = limit {
                    let end_line = (start_line + limit).min(all_lines.len());
                    (all_lines[start_line..end_line].join("\n"), Some(end_line - start_line))
                } else {
                    (all_lines[start_line..].join("\n"), None)
                };

                let truncation = truncate_head(&selected_content, Default::default());
                let mut diagnostics = Vec::new();
                let mut output_text = truncation.content.clone();
                let mut details: Option<ReadToolDetails> = None;

                if truncation.first_line_exceeds_limit {
                    let line_bytes = all_lines[start_line].as_bytes();
                    let end = character_end(line_bytes, DEFAULT_MAX_BYTES);
                    output_text = String::from_utf8_lossy(&line_bytes[..end]).into_owned();
                    diagnostics.push(ToolDiagnostic {
                        severity: DiagnosticSeverity::Warn,
                        code: Some("truncated".to_owned()),
                        message: format!(
                            "Line {start_line_display} is {}, exceeds the {} limit; showing its first {}. Use bash: sed -n '{start_line_display}p' {path} | tail -c +{}",
                            format_size(line_bytes.len()),
                            format_size(DEFAULT_MAX_BYTES),
                            format_size(end),
                            end + 1
                        ),
                    });
                    details = Some(ReadToolDetails {
                        truncation: Some(ReadTruncationDetails {
                            truncated: truncation.truncated,
                            truncated_by: truncation.truncated_by,
                            total_lines: truncation.total_lines,
                            total_bytes: truncation.total_bytes,
                            output_lines: 1,
                            output_bytes: end,
                            last_line_partial: truncation.last_line_partial,
                            first_line_exceeds_limit: truncation.first_line_exceeds_limit,
                            max_lines: truncation.max_lines,
                            max_bytes: truncation.max_bytes,
                        }),
                    });
                } else if truncation.truncated {
                    let end_line_display = start_line_display + truncation.output_lines - 1;
                    let next_offset = end_line_display + 1;
                    let limit_text = if truncation.truncated_by == Some(TruncatedBy::Lines) {
                        String::new()
                    } else {
                        format!(" ({} limit)", format_size(DEFAULT_MAX_BYTES))
                    };
                    diagnostics.push(ToolDiagnostic {
                        severity: DiagnosticSeverity::Info,
                        code: Some("truncated".to_owned()),
                        message: format!(
                            "Showing lines {start_line_display}-{end_line_display} of {total_file_lines}{limit_text}. Use offset={next_offset} to continue."
                        ),
                    });
                    details = Some(ReadToolDetails {
                        truncation: Some(ReadTruncationDetails {
                            truncated: truncation.truncated,
                            truncated_by: truncation.truncated_by,
                            total_lines: truncation.total_lines,
                            total_bytes: truncation.total_bytes,
                            output_lines: truncation.output_lines,
                            output_bytes: truncation.output_bytes,
                            last_line_partial: truncation.last_line_partial,
                            first_line_exceeds_limit: truncation.first_line_exceeds_limit,
                            max_lines: truncation.max_lines,
                            max_bytes: truncation.max_bytes,
                        }),
                    });
                } else if let Some(limited) = user_limited_lines
                    && start_line + limited < all_lines.len()
                {
                    let remaining = all_lines.len() - (start_line + limited);
                    let next_offset = start_line + limited + 1;
                    diagnostics.push(ToolDiagnostic {
                        severity: DiagnosticSeverity::Info,
                        code: None,
                        message: format!("{remaining} more lines in file. Use offset={next_offset} to continue."),
                    });
                }

                let details_json = details
                    .map(|d| serde_json::to_value(d))
                    .transpose()
                    .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;

                Ok(ToolExecutionResult {
                    content: Some(if output_text.is_empty() {
                        Vec::new()
                    } else {
                        vec![ToolResultContentBlock::Text(pi_ai::types::TextContent::new(
                            output_text,
                        ))]
                    }),
                    details: details_json,
                    diagnostics: Some(diagnostics),
                    ..Default::default()
                })
            })
        }),
    })
}
