//! Port of packages/coding-agent/examples/extensions/minimal-mode.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_ai::types::UserContentBlock;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, RenderShell, Theme, ToolRenderResultOptions,
};
use pi_coding_agent::core::tools::{
    create_bash_tool_definition, create_edit_tool_definition, create_find_tool_definition, create_grep_tool_definition,
    create_ls_tool_definition, create_read_tool_definition, create_write_tool_definition,
};
use pi_tui::{Component, Text};
use serde_json::Value;

fn shorten_path(path: &str) -> String {
    let home = pi_js::env::home_dir();
    if !home.is_empty()
        && let Some(rest) = path.strip_prefix(&home)
    {
        return format!("~{rest}");
    }
    path.to_string()
}

fn make_text(content: &str) -> Arc<dyn Component> {
    Arc::new(Text::new(Some(content), Some(0), Some(0), None))
}

fn format_json_number(value: &Value) -> Option<String> {
    if let Some(n) = value.as_i64() {
        return Some(n.to_string());
    }
    if let Some(n) = value.as_u64() {
        return Some(n.to_string());
    }
    value.as_f64().map(|n| n.to_string())
}

fn first_text(content: &[UserContentBlock]) -> Option<&str> {
    content.iter().find_map(|block| match block {
        UserContentBlock::Text(text) => Some(text.text.as_str()),
        UserContentBlock::Image(_) => None,
    })
}

fn render_expanded_lines(text: &str, theme: &Theme) -> String {
    text.split('\n')
        .map(|line| theme.fg("toolOutput", line))
        .collect::<Vec<_>>()
        .join("\n")
}

pub struct MinimalModeExtension;

#[async_trait]
impl Extension for MinimalModeExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let cwd = pi_js::env::cwd();

        // Read tool
        let mut read_def = create_read_tool_definition(&cwd, None);
        read_def.render_call = Some(Arc::new(|args, theme, _ctx| {
            let path = shorten_path(args.get("path").and_then(|v| v.as_str()).unwrap_or(""));
            let mut path_display = if path.is_empty() {
                theme.fg("toolOutput", "...")
            } else {
                theme.fg("accent", &path)
            };

            let offset = args.get("offset").and_then(|v| v.as_f64());
            let limit = args.get("limit").and_then(|v| v.as_f64());
            if offset.is_some() || limit.is_some() {
                let start_line = offset.unwrap_or(1.0);
                let start_str = args
                    .get("offset")
                    .and_then(format_json_number)
                    .unwrap_or_else(|| "1".to_string());
                let end_str = match limit {
                    Some(lim) => {
                        let end = start_line + lim - 1.0;
                        if end.fract() == 0.0 {
                            format!("-{}", end as i64)
                        } else {
                            format!("-{end}")
                        }
                    }
                    None => String::new(),
                };
                path_display.push_str(&theme.fg("warning", &format!(":{start_str}{end_str}")));
            }

            make_text(&format!(
                "{} {}",
                theme.fg("toolTitle", &theme.bold("read")),
                path_display
            ))
        }));
        read_def.render_result = Some(Arc::new(|result, options: ToolRenderResultOptions, theme, _ctx| {
            if !options.expanded {
                return make_text("");
            }
            let Some(text) = first_text(&result.content) else {
                return make_text("");
            };
            make_text(&format!("\n{}", render_expanded_lines(text, &theme)))
        }));
        pi.register_tool(read_def)?;

        // Bash tool
        let mut bash_def = create_bash_tool_definition(&cwd, None);
        bash_def.render_call = Some(Arc::new(|args, theme, _ctx| {
            let raw_cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let command = if raw_cmd.is_empty() { "..." } else { raw_cmd };
            let timeout_suffix = match args.get("timeout").and_then(format_json_number) {
                Some(t) => theme.fg("muted", &format!(" (timeout {t}s)")),
                None => String::new(),
            };
            let title = theme.fg("toolTitle", &theme.bold(&format!("$ {command}")));
            make_text(&format!("{title}{timeout_suffix}"))
        }));
        bash_def.render_result = Some(Arc::new(|result, options: ToolRenderResultOptions, theme, _ctx| {
            if !options.expanded {
                return make_text("");
            }
            let Some(text) = first_text(&result.content) else {
                return make_text("");
            };
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return make_text("");
            }
            make_text(&format!("\n{}", render_expanded_lines(trimmed, &theme)))
        }));
        pi.register_tool(bash_def)?;

        // Write tool
        let mut write_def = create_write_tool_definition(&cwd, None);
        write_def.render_call = Some(Arc::new(|args, theme, _ctx| {
            let path = shorten_path(args.get("path").and_then(|v| v.as_str()).unwrap_or(""));
            let path_display = if path.is_empty() {
                theme.fg("toolOutput", "...")
            } else {
                theme.fg("accent", &path)
            };
            let line_count = args
                .get("content")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|c| c.split('\n').count())
                .unwrap_or(0);
            let line_info = if line_count > 0 {
                theme.fg("muted", &format!(" ({line_count} lines)"))
            } else {
                String::new()
            };
            make_text(&format!(
                "{} {}{}",
                theme.fg("toolTitle", &theme.bold("write")),
                path_display,
                line_info
            ))
        }));
        write_def.render_result = Some(Arc::new(|result, options: ToolRenderResultOptions, theme, _ctx| {
            if !options.expanded {
                return make_text("");
            }
            if let Some(text) = first_text(&result.content)
                && !text.is_empty()
            {
                return make_text(&format!("\n{}", theme.fg("error", text)));
            }
            make_text("")
        }));
        pi.register_tool(write_def)?;

        // Edit tool
        let mut edit_def = create_edit_tool_definition(&cwd, None);
        edit_def.render_shell = Some(RenderShell::Default);
        edit_def.render_call = Some(Arc::new(|args, theme, _ctx| {
            let path = shorten_path(args.get("path").and_then(|v| v.as_str()).unwrap_or(""));
            let path_display = if path.is_empty() {
                theme.fg("toolOutput", "...")
            } else {
                theme.fg("accent", &path)
            };
            make_text(&format!(
                "{} {}",
                theme.fg("toolTitle", &theme.bold("edit")),
                path_display
            ))
        }));
        edit_def.render_result = Some(Arc::new(|result, options: ToolRenderResultOptions, theme, _ctx| {
            if !options.expanded {
                return make_text("");
            }
            let Some(text) = first_text(&result.content) else {
                return make_text("");
            };
            if text.contains("Error") || text.contains("error") {
                return make_text(&format!("\n{}", theme.fg("error", text)));
            }
            make_text(&format!("\n{}", theme.fg("toolOutput", text)))
        }));
        pi.register_tool(edit_def)?;

        // Find tool
        let mut find_def = create_find_tool_definition(&cwd, None);
        find_def.render_call = Some(Arc::new(|args, theme, _ctx| {
            let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let raw_path = args
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(".");
            let path = shorten_path(raw_path);
            let mut text = format!(
                "{} {}",
                theme.fg("toolTitle", &theme.bold("find")),
                theme.fg("accent", pattern)
            );
            text.push_str(&theme.fg("toolOutput", &format!(" in {path}")));
            if let Some(limit) = args.get("limit").and_then(format_json_number) {
                text.push_str(&theme.fg("toolOutput", &format!(" (limit {limit})")));
            }
            make_text(&text)
        }));
        find_def.render_result = Some(Arc::new(|result, options: ToolRenderResultOptions, theme, _ctx| {
            let Some(text) = first_text(&result.content) else {
                return make_text("");
            };
            if !options.expanded {
                let count = text.trim().split('\n').filter(|l| !l.is_empty()).count();
                if count > 0 {
                    return make_text(&theme.fg("muted", &format!(" → {count} files")));
                }
                return make_text("");
            }
            make_text(&format!("\n{}", render_expanded_lines(text.trim(), &theme)))
        }));
        pi.register_tool(find_def)?;

        // Grep tool
        let mut grep_def = create_grep_tool_definition(&cwd, None);
        grep_def.render_call = Some(Arc::new(|args, theme, _ctx| {
            let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let raw_path = args
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(".");
            let path = shorten_path(raw_path);
            let mut text = format!(
                "{} {}",
                theme.fg("toolTitle", &theme.bold("grep")),
                theme.fg("accent", &format!("/{pattern}/"))
            );
            text.push_str(&theme.fg("toolOutput", &format!(" in {path}")));
            if let Some(glob) = args.get("glob").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                text.push_str(&theme.fg("toolOutput", &format!(" ({glob})")));
            }
            if let Some(limit) = args.get("limit").and_then(format_json_number) {
                text.push_str(&theme.fg("toolOutput", &format!(" limit {limit}")));
            }
            make_text(&text)
        }));
        grep_def.render_result = Some(Arc::new(|result, options: ToolRenderResultOptions, theme, _ctx| {
            let Some(text) = first_text(&result.content) else {
                return make_text("");
            };
            if !options.expanded {
                let count = text.trim().split('\n').filter(|l| !l.is_empty()).count();
                if count > 0 {
                    return make_text(&theme.fg("muted", &format!(" → {count} matches")));
                }
                return make_text("");
            }
            make_text(&format!("\n{}", render_expanded_lines(text.trim(), &theme)))
        }));
        pi.register_tool(grep_def)?;

        // Ls tool
        let mut ls_def = create_ls_tool_definition(&cwd, None);
        ls_def.render_call = Some(Arc::new(|args, theme, _ctx| {
            let raw_path = args
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(".");
            let path = shorten_path(raw_path);
            let mut text = format!(
                "{} {}",
                theme.fg("toolTitle", &theme.bold("ls")),
                theme.fg("accent", &path)
            );
            if let Some(limit) = args.get("limit").and_then(format_json_number) {
                text.push_str(&theme.fg("toolOutput", &format!(" (limit {limit})")));
            }
            make_text(&text)
        }));
        ls_def.render_result = Some(Arc::new(|result, options: ToolRenderResultOptions, theme, _ctx| {
            let Some(text) = first_text(&result.content) else {
                return make_text("");
            };
            if !options.expanded {
                let count = text.trim().split('\n').filter(|l| !l.is_empty()).count();
                if count > 0 {
                    return make_text(&theme.fg("muted", &format!(" → {count} entries")));
                }
                return make_text("");
            }
            make_text(&format!("\n{}", render_expanded_lines(text.trim(), &theme)))
        }));
        pi.register_tool(ls_def)?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(MinimalModeExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
