//! Port of packages/coding-agent/src/core/tools/renderers/write.ts

use std::sync::Arc;

use serde_json::Value;

use crate::core::extensions::types::{RenderCallFn, RenderResultFn, ToolRenderContext};
use crate::core::tools::render_utils::{shorten_path, str_val};
use crate::core::tools::renderers::w3b_wire::{get_language_from_path, highlight_code, key_hint};
use crate::core::tools::renderers::{extract_text_output, format_preview_lines, reuse_or_init_text};
use crate::utils::paths::format_path_relative_to_cwd_or_absolute;

const WRITE_PREVIEW_LINES: usize = 10;

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

        let text = format!(
            "{} {}",
            theme.fg("toolTitle", &theme.bold("write")),
            theme.fg("accent", &display_path)
        );
        reuse_or_init_text(&context, &text, 0, 0)
    })
}

pub fn render_result() -> RenderResultFn {
    Arc::new(|result, options, theme, context: ToolRenderContext| {
        if context.is_error {
            let msg = extract_text_output(&result.content, context.show_images);
            let formatted = format!("\n{}", theme.fg("error", &msg));
            return reuse_or_init_text(&context, &formatted, 0, 0);
        }

        if let Some(content) = str_val(context.args.get("content"))
            && !content.is_empty()
        {
            let path = str_val(context.args.get("path")).unwrap_or_default();
            let lang = get_language_from_path(&path);
            let highlighted = highlight_code(&content, lang);
            let hint = key_hint("expandTools", "to expand");
            let preview = format_preview_lines(&highlighted, WRITE_PREVIEW_LINES, options.expanded, &hint);
            let formatted = format!("\n{}", theme.fg("toolOutput", &preview));
            return reuse_or_init_text(&context, &formatted, 0, 0);
        }

        let msg = extract_text_output(&result.content, context.show_images);
        let formatted = if msg.is_empty() {
            String::new()
        } else {
            format!("\n{}", theme.fg("toolOutput", &msg))
        };
        reuse_or_init_text(&context, &formatted, 0, 0)
    })
}
