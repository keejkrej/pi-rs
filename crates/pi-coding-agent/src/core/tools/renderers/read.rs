//! Port of packages/coding-agent/src/core/tools/renderers/read.ts

use std::sync::Arc;

use serde_json::Value;

use crate::core::extensions::types::{RenderCallFn, RenderResultFn, ToolRenderContext};
use crate::core::tools::mcp_wire::format_path_relative_to_cwd_or_absolute;
use crate::core::tools::read::ReadToolDetails;
use crate::core::tools::render_utils::{shorten_path, str_val};
use crate::core::tools::renderers::{extract_text_output, format_preview_lines, reuse_or_init_text};
use crate::core::tools::truncate::format_size;
use crate::core::tools::w3b_wire::{get_language_from_path, highlight_code, key_hint};

const READ_PREVIEW_LINES: usize = 10;

pub fn render_call() -> RenderCallFn {
    Arc::new(|args: Value, theme, context: ToolRenderContext| {
        let raw_path = str_val(args.get("path")).unwrap_or_default();
        let display_path = if raw_path.is_empty() {
            if context.args_complete {
                "(invalid path)".to_string()
            } else {
                "...".to_string()
            }
        } else {
            let rel = format_path_relative_to_cwd_or_absolute(&raw_path, &context.cwd);
            shorten_path(Some(&rel))
        };

        let offset = args.get("offset").and_then(Value::as_u64);
        let limit = args.get("limit").and_then(Value::as_u64);
        let range_suffix = match (offset, limit) {
            (Some(off), Some(lim)) => format!(":{}-{}", off, off.saturating_add(lim).saturating_sub(1)),
            (Some(off), None) => format!(":{off}"),
            _ => String::new(),
        };

        let mut text = format!(
            "{} {}",
            theme.fg("toolTitle", &theme.bold("read")),
            theme.fg("accent", &display_path)
        );
        if !range_suffix.is_empty() {
            text.push_str(&theme.fg("warning", &range_suffix));
        }

        reuse_or_init_text(&context, &text, 0, 0)
    })
}

pub fn render_result() -> RenderResultFn {
    Arc::new(|result, options, theme, context: ToolRenderContext| {
        if options.is_partial {
            return reuse_or_init_text(&context, "", 0, 0);
        }

        let raw_output = extract_text_output(&result.content, context.show_images);
        if raw_output.is_empty() {
            return reuse_or_init_text(&context, "", 0, 0);
        }

        let path = str_val(context.args.get("path")).unwrap_or_default();
        let lang = get_language_from_path(&path);
        let highlighted = highlight_code(&raw_output, lang);
        let hint = key_hint("expandTools", "to expand");
        let preview = format_preview_lines(&highlighted, READ_PREVIEW_LINES, options.expanded, &hint);

        let mut out = format!("\n{}", theme.fg("toolOutput", &preview));
        if let Some(details_val) = &result.details
            && let Ok(details) = serde_json::from_value::<ReadToolDetails>(details_val.clone())
            && let Some(trunc) = details.truncation
            && trunc.truncated
        {
            let warn = format!(
                "[Truncated: showing {} of {} lines ({} of {})]",
                trunc.output_lines,
                trunc.total_lines,
                format_size(trunc.output_bytes.max(0) as usize),
                format_size(trunc.total_bytes.max(0) as usize),
            );
            out.push_str(&format!("\n{}", theme.fg("warning", &warn)));
        }

        reuse_or_init_text(&context, &out, 0, 0)
    })
}
