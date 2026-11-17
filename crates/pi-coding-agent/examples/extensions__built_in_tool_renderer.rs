//! Port of packages/coding-agent/examples/extensions/built-in-tool-renderer.ts

use std::process::ExitCode;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use pi_ai::types::UserContentBlock;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, ToolRenderResultOptions,
};
use pi_coding_agent::core::tools::{
    BashToolDetails, EditToolDetails, ReadToolDetails, create_bash_tool_definition, create_edit_tool_definition,
    create_read_tool_definition, create_write_tool_definition,
};
use pi_tui::{Component, Text};
use regex::Regex;
use serde_json::Value;

static EXIT_CODE_RE: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"exit code: (\d+)").ok());

fn format_json_number(value: &Value) -> Option<String> {
    if let Some(n) = value.as_i64() {
        return Some(n.to_string());
    }
    if let Some(n) = value.as_u64() {
        return Some(n.to_string());
    }
    value.as_f64().map(|n| n.to_string())
}

fn make_text(content: String) -> Arc<dyn Component> {
    Arc::new(Text::new(Some(&content), Some(0), Some(0), None))
}

pub struct BuiltInToolRendererExtension;

#[async_trait]
impl Extension for BuiltInToolRendererExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let cwd = pi_js::env::cwd();

        // --- Read tool: show path and line count ---
        let mut read_def = create_read_tool_definition(&cwd, None);
        read_def.render_call = Some(Arc::new(|args, theme, _context| {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let mut text = theme.fg("toolTitle", &theme.bold("read "));
            text.push_str(&theme.fg("accent", path));

            let mut parts: Vec<String> = Vec::new();
            if let Some(offset) = args.get("offset").and_then(format_json_number) {
                parts.push(format!("offset={offset}"));
            }
            if let Some(limit) = args.get("limit").and_then(format_json_number) {
                parts.push(format!("limit={limit}"));
            }
            if !parts.is_empty() {
                text.push_str(&theme.fg("dim", &format!(" ({})", parts.join(", "))));
            }
            make_text(text)
        }));
        read_def.render_result = Some(Arc::new(|result, options: ToolRenderResultOptions, theme, _context| {
            if options.is_partial {
                return make_text(theme.fg("warning", "Reading..."));
            }

            let details = result
                .details
                .as_ref()
                .and_then(|d| serde_json::from_value::<ReadToolDetails>(d.clone()).ok());
            let content = result.content.first();

            if let Some(UserContentBlock::Image(_)) = content {
                return make_text(theme.fg("success", "Image loaded"));
            }

            let Some(UserContentBlock::Text(text_block)) = content else {
                return make_text(theme.fg("error", "No content"));
            };

            let all_lines: Vec<&str> = text_block.text.split('\n').collect();
            let line_count = all_lines.len();
            let mut text = theme.fg("success", &format!("{line_count} lines"));

            if let Some(trunc) = details.and_then(|d| d.truncation)
                && trunc.truncated
            {
                text.push_str(&theme.fg("warning", &format!(" (truncated from {})", trunc.total_lines)));
            }

            if options.expanded {
                for line in all_lines.iter().take(15) {
                    text.push('\n');
                    text.push_str(&theme.fg("dim", line));
                }
                if line_count > 15 {
                    text.push('\n');
                    text.push_str(&theme.fg("muted", &format!("... {} more lines", line_count - 15)));
                }
            }

            make_text(text)
        }));
        pi.register_tool(read_def)?;

        // --- Bash tool: show command and exit code ---
        let mut bash_def = create_bash_tool_definition(&cwd, None);
        bash_def.render_call = Some(Arc::new(|args, theme, _context| {
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let mut text = theme.fg("toolTitle", &theme.bold("$ "));
            let char_count = command.chars().count();
            let cmd = if char_count > 80 {
                let prefix: String = command.chars().take(77).collect();
                format!("{prefix}...")
            } else {
                command.to_string()
            };
            text.push_str(&theme.fg("accent", &cmd));
            if let Some(timeout) = args.get("timeout").and_then(format_json_number) {
                text.push_str(&theme.fg("dim", &format!(" (timeout: {timeout}s)")));
            }
            make_text(text)
        }));
        bash_def.render_result = Some(Arc::new(|result, options: ToolRenderResultOptions, theme, _context| {
            if options.is_partial {
                return make_text(theme.fg("warning", "Running..."));
            }

            let details = result
                .details
                .as_ref()
                .and_then(|d| serde_json::from_value::<BashToolDetails>(d.clone()).ok());
            let output = match result.content.first() {
                Some(UserContentBlock::Text(text_block)) => text_block.text.as_str(),
                _ => "",
            };

            let exit_code = EXIT_CODE_RE
                .as_ref()
                .and_then(|re| re.captures(output))
                .and_then(|caps| caps.get(1))
                .and_then(|m| m.as_str().parse::<i64>().ok());
            let all_lines: Vec<&str> = output.split('\n').collect();
            let line_count = all_lines.iter().filter(|l| !l.trim().is_empty()).count();

            let mut text = match exit_code {
                None | Some(0) => theme.fg("success", "done"),
                Some(code) => theme.fg("error", &format!("exit {code}")),
            };
            text.push_str(&theme.fg("dim", &format!(" ({line_count} lines)")));

            if let Some(trunc) = details.and_then(|d| d.truncation)
                && trunc.truncated
            {
                text.push_str(&theme.fg("warning", " [truncated]"));
            }

            if options.expanded {
                for line in all_lines.iter().take(20) {
                    text.push('\n');
                    text.push_str(&theme.fg("dim", line));
                }
                if all_lines.len() > 20 {
                    text.push('\n');
                    text.push_str(&theme.fg("muted", "... more output"));
                }
            }

            make_text(text)
        }));
        pi.register_tool(bash_def)?;

        // --- Edit tool: show path and diff stats ---
        let mut edit_def = create_edit_tool_definition(&cwd, None);
        edit_def.render_call = Some(Arc::new(|args, theme, _context| {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let mut text = theme.fg("toolTitle", &theme.bold("edit "));
            text.push_str(&theme.fg("accent", path));
            make_text(text)
        }));
        edit_def.render_result = Some(Arc::new(|result, options: ToolRenderResultOptions, theme, _context| {
            if options.is_partial {
                return make_text(theme.fg("warning", "Editing..."));
            }

            let details = result
                .details
                .as_ref()
                .and_then(|d| serde_json::from_value::<EditToolDetails>(d.clone()).ok());
            if let Some(UserContentBlock::Text(text_block)) = result.content.first()
                && text_block.text.starts_with("Error")
            {
                let first_line = text_block.text.split('\n').next().unwrap_or("");
                return make_text(theme.fg("error", first_line));
            }

            let Some(diff) = details.map(|d| d.diff).filter(|d| !d.is_empty()) else {
                return make_text(theme.fg("success", "Applied"));
            };

            let diff_lines: Vec<&str> = diff.split('\n').collect();
            let mut additions = 0usize;
            let mut removals = 0usize;
            for line in &diff_lines {
                if line.starts_with('+') && !line.starts_with("+++") {
                    additions += 1;
                }
                if line.starts_with('-') && !line.starts_with("---") {
                    removals += 1;
                }
            }

            let mut text = theme.fg("success", &format!("+{additions}"));
            text.push_str(&theme.fg("dim", " / "));
            text.push_str(&theme.fg("error", &format!("-{removals}")));

            if options.expanded {
                for line in diff_lines.iter().take(30) {
                    text.push('\n');
                    if line.starts_with('+') && !line.starts_with("+++") {
                        text.push_str(&theme.fg("success", line));
                    } else if line.starts_with('-') && !line.starts_with("---") {
                        text.push_str(&theme.fg("error", line));
                    } else {
                        text.push_str(&theme.fg("dim", line));
                    }
                }
                if diff_lines.len() > 30 {
                    text.push('\n');
                    text.push_str(&theme.fg("muted", &format!("... {} more diff lines", diff_lines.len() - 30)));
                }
            }

            make_text(text)
        }));
        pi.register_tool(edit_def)?;

        // --- Write tool: show path and size ---
        let mut write_def = create_write_tool_definition(&cwd, None);
        write_def.render_call = Some(Arc::new(|args, theme, _context| {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let line_count = content.split('\n').count();
            let mut text = theme.fg("toolTitle", &theme.bold("write "));
            text.push_str(&theme.fg("accent", path));
            text.push_str(&theme.fg("dim", &format!(" ({line_count} lines)")));
            make_text(text)
        }));
        write_def.render_result = Some(Arc::new(|result, options: ToolRenderResultOptions, theme, _context| {
            if options.is_partial {
                return make_text(theme.fg("warning", "Writing..."));
            }

            if let Some(UserContentBlock::Text(text_block)) = result.content.first()
                && text_block.text.starts_with("Error")
            {
                let first_line = text_block.text.split('\n').next().unwrap_or("");
                return make_text(theme.fg("error", first_line));
            }

            make_text(theme.fg("success", "Written"))
        }));
        pi.register_tool(write_def)?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(BuiltInToolRendererExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
