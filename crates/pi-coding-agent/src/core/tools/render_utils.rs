//! Port of packages/coding-agent/src/core/tools/render-utils.ts

use std::sync::Arc;
use std::time::Duration;

use pi_tui::terminal_image::{get_capabilities, get_image_dimensions, hyperlink, image_fallback};
use serde_json::Value;

use crate::core::extensions::types::{Theme, ToolRenderContext};
use crate::utils::ansi::strip_ansi;
use crate::utils::paths::{PathInputOptions, resolve_path, shorten_home_path};
use crate::utils::shell::sanitize_binary_output;

const COLLAPSED_ARGS_CHARS: usize = 100;

pub fn shorten_path(path: Option<&str>) -> String {
    let Some(p) = path else {
        return String::new();
    };
    shorten_home_path(p, None)
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

fn parse_line_number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()),
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                trimmed.parse::<f64>().ok().filter(|f| f.is_finite())
            }
        }
        _ => None,
    }
}

fn format_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() <= (i64::MAX as f64) {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

pub fn format_read_line_range(offset: Option<&Value>, limit: Option<&Value>) -> String {
    let off_is_null = matches!(offset, None | Some(Value::Null));
    let lim_is_null = matches!(limit, None | Some(Value::Null));
    if off_is_null && lim_is_null {
        return String::new();
    }
    let start = if off_is_null {
        1.0
    } else {
        let Some(s) = parse_line_number(offset) else {
            return String::new();
        };
        s
    };
    if lim_is_null {
        return format!(":{}", format_num(start));
    }
    let Some(count) = parse_line_number(limit) else {
        return String::new();
    };
    let end = start + count - 1.0;
    if end != 0.0 {
        format!(":{}-{}", format_num(start), format_num(end))
    } else {
        format!(":{}", format_num(start))
    }
}

/// Duration as `4.2s` below a minute, then `1m 30s` and `2h 3m 4s`.
pub fn format_tool_duration(ms: f64) -> String {
    let seconds = ms.max(0.0) / 1000.0;
    if seconds < 60.0 {
        return format!("{seconds:.1}s");
    }
    let total_seconds = seconds.floor() as u64;
    let minutes = total_seconds / 60;
    let remainder = total_seconds % 60;
    if minutes < 60 {
        return format!("{minutes}m {remainder}s");
    }
    format!("{}h {}m {remainder}s", minutes / 60, minutes % 60)
}

/// Renderer state used by [`start_tool_timer`] and [`tool_timer_text`] inside `ToolRenderContext.state`.
#[derive(Default)]
pub struct ToolTimerState {
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub timer_spawned: bool,
    pub call_text: Option<pi_tui::Text>,
    pub result_text: Option<pi_tui::Text>,
    pub custom: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

/// Start the row's clock once execution starts. Call from `render_call`.
pub fn start_tool_timer(context: &ToolRenderContext) {
    if !context.execution_started {
        return;
    }
    if let Ok(mut guard) = context.state.lock() {
        let state = guard.get_or_insert_with(|| Box::new(ToolTimerState::default()));
        if let Some(st) = state.downcast_mut::<ToolTimerState>()
            && st.started_at_ms.is_none()
        {
            st.started_at_ms = Some(pi_js::time::now_ms());
            st.ended_at_ms = None;
        }
    }
}

/// Format the row's timing line from `render_result`: `Elapsed 4.2s` while partial, `Took 4.2s` once final.
pub fn tool_timer_text(context: &ToolRenderContext, is_partial: bool) -> Option<String> {
    let mut recorded_start = None;
    let mut recorded_end = None;
    if let Ok(mut guard) = context.state.lock() {
        let state = guard.get_or_insert_with(|| Box::new(ToolTimerState::default()));
        if let Some(st) = state.downcast_mut::<ToolTimerState>() {
            if st.started_at_ms.is_some() && is_partial && !st.timer_spawned {
                st.timer_spawned = true;
                let invalidate = Arc::clone(&context.invalidate);
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        invalidate();
                    });
                }
            }
            if (!is_partial || context.is_error) && st.ended_at_ms.is_none() {
                st.ended_at_ms = Some(pi_js::time::now_ms());
            }
            recorded_start = st.started_at_ms;
            recorded_end = st.ended_at_ms;
        }
    }
    if !is_partial && let Some(ms) = context.duration_ms {
        return Some(format!("Took {}", format_tool_duration(ms)));
    }
    let start = recorded_start?;
    let end = recorded_end.unwrap_or_else(pi_js::time::now_ms);
    let label = if is_partial { "Elapsed" } else { "Took" };
    Some(format!("{label} {}", format_tool_duration((end - start).max(0) as f64)))
}
