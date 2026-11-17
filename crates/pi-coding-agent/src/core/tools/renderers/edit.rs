//! Port of packages/coding-agent/src/core/tools/renderers/edit.ts

use std::sync::Arc;

use serde_json::Value;

use crate::core::extensions::types::{RenderCallFn, RenderResultFn, ToolRenderContext};
use crate::core::tools::edit::EditToolDetails;
use crate::core::tools::render_utils::{shorten_path, str_val};
use crate::core::tools::renderers::w3b_wire::render_diff;
use crate::core::tools::renderers::{extract_text_output, reuse_or_init_text};
use crate::utils::paths::format_path_relative_to_cwd_or_absolute;

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
            theme.fg("toolTitle", &theme.bold("edit")),
            theme.fg("accent", &display_path)
        );
        reuse_or_init_text(&context, &text, 0, 0)
    })
}

pub fn render_result() -> RenderResultFn {
    Arc::new(|result, options, theme, context: ToolRenderContext| {
        if options.is_partial {
            return reuse_or_init_text(&context, "", 0, 0);
        }

        if context.is_error {
            let msg = extract_text_output(&result.content, context.show_images);
            let formatted = format!("\n{}", theme.fg("error", &msg));
            return reuse_or_init_text(&context, &formatted, 0, 0);
        }

        if let Some(details_val) = &result.details
            && let Ok(details) = serde_json::from_value::<EditToolDetails>(details_val.clone())
            && !details.diff.is_empty()
        {
            let diff_view = format!("\n{}", render_diff(&details.diff, Some(&theme)));
            return reuse_or_init_text(&context, &diff_view, 0, 0);
        }

        let text = extract_text_output(&result.content, context.show_images);
        let formatted = if text.is_empty() {
            String::new()
        } else {
            format!("\n{}", theme.fg("toolOutput", &text))
        };
        reuse_or_init_text(&context, &formatted, 0, 0)
    })
}
