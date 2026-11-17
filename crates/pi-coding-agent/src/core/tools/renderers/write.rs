//! Port of packages/coding-agent/src/core/tools/renderers/write.ts

use std::sync::Arc;

use serde_json::Value;

use crate::core::extensions::types::{RenderCallFn, RenderResultFn, ToolRenderContext};
use crate::core::tools::render_utils::{normalize_display_text, replace_tabs, shorten_path, str_val};
use crate::core::tools::renderers::w3b_wire::{get_language_from_path, highlight_code, key_hint};
use crate::core::tools::renderers::{extract_text_output, reuse_or_init_call_text, reuse_or_init_result_text};
use crate::utils::paths::format_path_relative_to_cwd_or_absolute;

const WRITE_PREVIEW_LINES: usize = 10;

pub fn render_call() -> RenderCallFn {
    Arc::new(|args: Value, theme, context: ToolRenderContext| {
        let raw_path = str_val(args.get("file_path"))
            .filter(|s| !s.is_empty())
            .or_else(|| str_val(args.get("path")))
            .unwrap_or_default();
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

        let mut text = format!(
            "{} {}",
            theme.fg("toolTitle", &theme.bold("write")),
            theme.fg("accent", &display_path)
        );
        if let Some(content) = str_val(args.get("content")) {
            if !content.is_empty() {
                let normalized = replace_tabs(&normalize_display_text(&content));
                let lang = get_language_from_path(&raw_path);
                let mut lines: Vec<String> = if let Some(lang) = lang {
                    highlight_code(&normalized, Some(lang))
                } else {
                    normalized
                        .split('\n')
                        .map(|line| theme.fg("toolOutput", line))
                        .collect()
                };
                while lines.last().is_some_and(String::is_empty) {
                    lines.pop();
                }
                if !lines.is_empty() {
                    let total_lines = lines.len();
                    let max_lines = if context.expanded {
                        lines.len()
                    } else {
                        WRITE_PREVIEW_LINES
                    };
                    let display_count = lines.len().min(max_lines);
                    let remaining = lines.len().saturating_sub(display_count);
                    text.push_str("\n\n");
                    text.push_str(&lines[..display_count].join("\n"));
                    if remaining > 0 {
                        text.push_str(&format!(
                            "{} {}{}",
                            theme.fg("muted", &format!("\n... ({remaining} more lines, {total_lines} total,")),
                            key_hint("expandTools", "to expand"),
                            theme.fg("muted", ")"),
                        ));
                    }
                }
            }
        } else if args.get("content").is_some_and(|v| !v.is_null() && !v.is_string()) {
            text.push_str(&format!(
                "\n\n{}",
                theme.fg("error", "[invalid content arg - expected string]")
            ));
        }

        reuse_or_init_call_text(&context, &text, 0, 0)
    })
}

pub fn render_result() -> RenderResultFn {
    Arc::new(|result, _options, theme, context: ToolRenderContext| {
        if !context.is_error {
            return Arc::new(pi_tui::Container::new());
        }
        let msg = extract_text_output(&result.content, context.show_images);
        if msg.is_empty() {
            return Arc::new(pi_tui::Container::new());
        }
        let formatted = format!("\n{}", theme.fg("error", &msg));
        reuse_or_init_result_text(&context, &formatted, 0, 0)
    })
}
