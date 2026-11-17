//! Port of packages/coding-agent/src/core/tools/renderers/find.ts

use std::sync::Arc;

use serde_json::Value;

use crate::core::extensions::types::{RenderCallFn, RenderResultFn, ToolRenderContext};
use crate::core::tools::find::FindToolDetails;
use crate::core::tools::render_utils::{replace_tabs, shorten_path, str_val};
use crate::core::tools::renderers::w3b_wire::key_hint;
use crate::core::tools::renderers::{extract_text_output, format_preview_lines, reuse_or_init_text};
use crate::utils::paths::format_path_relative_to_cwd_or_absolute;

const FIND_PREVIEW_LINES: usize = 20;

pub fn render_call() -> RenderCallFn {
    Arc::new(|args: Value, theme, context: ToolRenderContext| {
        let pattern = str_val(args.get("pattern")).unwrap_or_default();
        let raw_path = str_val(args.get("path"));
        let limit = args.get("limit").and_then(Value::as_f64);

        let mut text = format!(
            "{} {}",
            theme.fg("toolTitle", &theme.bold("find")),
            theme.fg("accent", if pattern.is_empty() { "..." } else { &pattern })
        );
        if let Some(p) = raw_path {
            let rel = format_path_relative_to_cwd_or_absolute(&p, &context.cwd);
            let short = shorten_path(Some(&rel));
            text.push_str(&theme.fg("muted", &format!(" in {short}")));
        }
        if let Some(lim) = limit {
            text.push_str(&theme.fg("muted", &format!(" (limit {lim})")));
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

        let lines: Vec<String> = raw_output.split('\n').map(replace_tabs).collect();
        let hint = key_hint("expandTools", "to expand");
        let preview = format_preview_lines(&lines, FIND_PREVIEW_LINES, options.expanded, &hint);

        let mut out = format!("\n{}", theme.fg("toolOutput", &preview));
        if let Some(details_val) = &result.details
            && let Ok(details) = serde_json::from_value::<FindToolDetails>(details_val.clone())
        {
            if let Some(lim) = details.result_limit_reached {
                out.push_str(&format!(
                    "\n{}",
                    theme.fg("warning", &format!("[{lim} results limit reached]"))
                ));
            }
            if details.truncation.is_some_and(|t| t.truncated) {
                out.push_str(&format!("\n{}", theme.fg("warning", "[Output truncated to 50.0KB]")));
            }
        }

        reuse_or_init_text(&context, &out, 0, 0)
    })
}
