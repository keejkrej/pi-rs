//! Port of packages/coding-agent/src/core/tools/render-utils.ts

use pi_tui::terminal_image::{get_capabilities, get_image_dimensions, hyperlink, image_fallback};
use serde_json::Value;

use crate::core::extensions::types::Theme;
use crate::utils::ansi::strip_ansi;
use crate::utils::paths::{PathInputOptions, resolve_path};
use crate::utils::shell::sanitize_binary_output;

const COLLAPSED_ARGS_CHARS: usize = 100;

pub fn shorten_path(path: Option<&str>) -> String {
    let Some(p) = path else {
        return String::new();
    };
    let home = pi_js::env::home_dir();
    if let Some(rest) = p.strip_prefix(&home) {
        format!("~{rest}")
    } else {
        p.to_string()
    }
}

pub fn link_path(styled_text: &str, raw_path: &str, cwd: &str) -> String {
    if !get_capabilities().hyperlinks {
        return styled_text.to_string();
    }
    let absolute_path = resolve_path(
        raw_path,
        Some(cwd),
        PathInputOptions {
            strip_at_prefix: Some(true),
            ..Default::default()
        },
    );
    let url = url::Url::from_file_path(&absolute_path)
        .map(|u| u.to_string())
        .unwrap_or_else(|()| format!("file://{absolute_path}"));
    hyperlink(styled_text, &url)
}

pub fn str_val(value: Option<&Value>) -> Option<String> {
    match value {
        None | Some(Value::Null) => Some(String::new()),
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => None,
    }
}

pub fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

pub fn normalize_display_text(text: &str) -> String {
    text.replace('\r', "")
}

pub fn get_text_output(content: &[Value], show_images: bool) -> String {
    let mut text_parts: Vec<String> = Vec::new();
    let mut image_parts: Vec<(&Value, &str)> = Vec::new();

    for c in content {
        let kind = c.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if kind == "text" {
            let txt = c.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let clean = sanitize_binary_output(&strip_ansi(txt)).replace('\r', "");
            text_parts.push(clean);
        } else if kind == "image" {
            let mime = c.get("mimeType").and_then(|v| v.as_str()).unwrap_or("image/unknown");
            image_parts.push((c, mime));
        }
    }

    let mut output = text_parts.join("\n");
    let caps = get_capabilities();
    if !image_parts.is_empty() && (caps.images.is_none() || !show_images) {
        let indicators: Vec<String> = image_parts
            .into_iter()
            .map(|(img, mime)| {
                let dims = img
                    .get("data")
                    .and_then(|d| d.as_str())
                    .and_then(|data| img.get("mimeType").and_then(|m| m.as_str()).map(|m| (data, m)))
                    .and_then(|(data, m)| get_image_dimensions(data, m));
                image_fallback(mime, dims, None)
            })
            .collect();
        let joined = indicators.join("\n");
        if output.is_empty() {
            output = joined;
        } else {
            output = format!("{output}\n{joined}");
        }
    }
    output
}

pub fn format_tool_call_with_args(title: &str, args: Option<&Value>, theme: &Theme, expanded: bool) -> String {
    let header = theme.fg("toolTitle", &theme.bold(title));
    let Some(args_val) = args else {
        return header;
    };
    if args_val.is_null() {
        return header;
    }
    let entries: Vec<(String, Value)> = match args_val {
        Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        other => vec![("args".to_string(), other.clone())],
    };
    if entries.is_empty() {
        return header;
    }
    if expanded {
        let lines: Vec<String> = entries
            .into_iter()
            .map(|(key, value)| {
                let text = match value {
                    Value::String(s) => s,
                    other => pi_js::json::stringify_pretty(&other, "  "),
                };
                let indented = replace_tabs(&text).replace('\r', "").replace('\n', "\n    ");
                format!("  {key}: {indented}")
            })
            .collect();
        return format!("{header}\n{}", theme.fg("muted", &lines.join("\n")));
    }
    let pairs: String = entries
        .into_iter()
        .map(|(key, value)| format!("{key}={}", pi_js::json::stringify(&value)))
        .collect::<Vec<_>>()
        .join(" ");
    let len16 = pi_js::str16::len(&pairs);
    let preview = if len16 > COLLAPSED_ARGS_CHARS {
        format!(
            "{}...",
            pi_js::str16::slice(&pairs, 0, Some((COLLAPSED_ARGS_CHARS - 3) as i64))
        )
    } else {
        pairs
    };
    format!("{header} {}", theme.fg("muted", &preview))
}

pub fn invalid_arg_text(theme: &Theme) -> String {
    theme.fg("error", "[invalid arg]")
}

pub fn render_tool_path(raw_path: Option<&str>, theme: &Theme, cwd: &str, empty_fallback: Option<&str>) -> String {
    let Some(raw) = raw_path else {
        return invalid_arg_text(theme);
    };
    let value = if raw.is_empty() {
        empty_fallback.unwrap_or("")
    } else {
        raw
    };
    if value.is_empty() {
        return theme.fg("toolOutput", "...");
    }
    link_path(&theme.fg("accent", &shorten_path(Some(value))), value, cwd)
}

pub fn format_read_line_range(offset: Option<&Value>, limit: Option<&Value>) -> String {
    let offset_num = offset.and_then(Value::as_i64);
    let limit_num = limit.and_then(Value::as_i64);
    if offset_num.is_none() && limit_num.is_none() {
        return String::new();
    }
    let start = offset_num.unwrap_or(1);
    match limit_num {
        Some(lim) => format!(":{start}-{}", start + lim - 1),
        None => format!(":{start}"),
    }
}
