//! Port of packages/coding-agent/src/extensions/codemode/renderer.ts

use std::sync::{Arc, LazyLock};

use serde_json::{Value, json};

use crate::core::extensions::types::{RenderCallFn, RenderResultFn, Theme, ToolRenderContext, ToolRenderResultOptions};
use crate::core::tools::render_utils::{
    ToolTimerState, get_text_output, replace_tabs, start_tool_timer, str_val, tool_timer_text,
};
use crate::extensions::codemode::tool::{CodemodeNestedCall, CodemodeNestedCallStatus, CodemodeToolDetails};
use crate::extensions::wire::{CodeHighlighter, VisualLinePreview, VisualTruncateKeep, key_hint};
use pi_agent_core::AgentToolResult;
use pi_ai::UserContentBlock;
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::tui::{Component, Container};

const CODE_PREVIEW_LINES: usize = 10;
const CALL_PREVIEW_COUNT: usize = 8;
const OUTPUT_PREVIEW_LINES: usize = 5;
const COLLAPSED_ARGS_CHARS: usize = 80;

#[derive(Default)]
struct CodemodeRenderState {
    timer: ToolTimerState,
    script_highlighter: CodeHighlighter,
}

static SCRIPT_HEADER_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^Script (?:completed|failed)\nWall time [0-9.]+ seconds\nOutput:\n$")
        .expect("valid script header regex")
});

fn expand_hint(theme: &Theme, hidden: usize, noun: &str) -> String {
    format!(
        "{} {}{}",
        theme.fg("muted", &format!("... ({hidden} more {noun},")),
        key_hint("app.tools.expand", "to expand", theme),
        theme.fg("muted", ")")
    )
}

fn format_duration(ms: Option<f64>) -> String {
    let Some(ms) = ms else {
        return String::new();
    };
    if ms < 1000.0 {
        format!("{}ms", ms.round() as i64)
    } else {
        format!("{:.1}s", ms / 1000.0)
    }
}

/// Cents for larger amounts, two significant digits for fractions of a cent classifier calls cost.
fn format_cost(cost: f64) -> String {
    if cost >= 0.01 {
        format!("${cost:.2}")
    } else {
        // Two significant digits matching JS Number.toPrecision(2) for positive sub-cent numbers.
        if cost <= 0.0 || !cost.is_finite() {
            return "$0.0".to_string();
        }
        let exp = cost.abs().log10().floor() as i32;
        let decimals = (1 - exp).max(0) as usize;
        let formatted = format!("{cost:.decimals$}");
        let trimmed = formatted.trim_end_matches('0');
        let trimmed = if trimmed.ends_with('.') {
            format!("{trimmed}0")
        } else {
            trimmed.to_string()
        };
        format!("${trimmed}")
    }
}

fn status_icon(status: CodemodeNestedCallStatus, theme: &Theme) -> String {
    match status {
        CodemodeNestedCallStatus::Running => theme.fg("warning", "…"),
        CodemodeNestedCallStatus::Ok => theme.fg("success", "✓"),
        CodemodeNestedCallStatus::Error => theme.fg("error", "✗"),
        CodemodeNestedCallStatus::Cancelled => theme.fg("muted", "⊘"),
    }
}

fn format_call(call: &CodemodeNestedCall, theme: &Theme, expanded: bool) -> String {
    let args = if !expanded && pi_js::str16::len(&call.args) > COLLAPSED_ARGS_CHARS {
        let prefix = pi_js::str16::slice(&call.args, 0, Some((COLLAPSED_ARGS_CHARS - 3) as i64));
        format!("{prefix}...")
    } else {
        call.args.clone()
    };
    let duration = format_duration(call.duration_ms);
    let mut line = format!(
        "{} {}",
        status_icon(call.status, theme),
        theme.fg("toolTitle", &call.name)
    );
    if !args.is_empty() {
        line.push(' ');
        line.push_str(&theme.fg("muted", &args));
    }
    if !duration.is_empty() {
        line.push(' ');
        line.push_str(&theme.fg("dim", &duration));
    }
    if let Some(cost) = call.cost
        && cost != 0.0
    {
        line.push(' ');
        line.push_str(&theme.fg("dim", &format_cost(cost)));
    }
    if expanded
        && let Some(ref error) = call.error
        && !error.is_empty()
    {
        let indented = error.split('\n').collect::<Vec<_>>().join("\n    ");
        line.push_str("\n    ");
        line.push_str(&theme.fg("error", &indented));
    }
    line
}

fn content_block_to_value(block: &UserContentBlock) -> Value {
    match block {
        UserContentBlock::Text(t) => json!({ "type": "text", "text": t.text }),
        UserContentBlock::Image(img) => json!({ "type": "image", "data": img.data, "mimeType": img.mime_type }),
    }
}

pub fn render_call(args: Value, theme: Theme, context: ToolRenderContext) -> Arc<dyn Component> {
    start_tool_timer(&context);
    let code = str_val(args.get("code"));
    let title = theme.fg("toolTitle", &theme.bold("codemode"));
    let component = Container::new();
    match code {
        None => {
            component.add_child(Arc::new(Text::new(
                Some(&format!("{title} {}", theme.fg("error", "[invalid arg]"))),
                Some(0),
                Some(0),
                None,
            )));
            Arc::new(component)
        }
        Some(code) => {
            component.add_child(Arc::new(Text::new(Some(&title), Some(0), Some(0), None)));
            if !code.is_empty() {
                let cleaned = code.replace('\r', "");
                let trimmed = cleaned.trim_end();
                let highlighter = context
                    .state
                    .lock()
                    .ok()
                    .map(|mut guard| {
                        let state = guard.get_or_insert_with(|| Box::new(ToolTimerState::default()));
                        if let Some(st) = state.downcast_mut::<ToolTimerState>() {
                            if let Some(existing) = st.custom.as_ref().and_then(|c| c.downcast_ref::<CodeHighlighter>())
                            {
                                return existing.clone();
                            }
                            let created = CodeHighlighter::new();
                            st.custom = Some(Arc::new(created.clone()));
                            return created;
                        }
                        CodeHighlighter::new()
                    })
                    .unwrap_or_else(CodeHighlighter::new);
                let highlighted = highlighter
                    .highlight(&replace_tabs(trimmed), Some("javascript"), &theme)
                    .join("\n");
                if context.expanded {
                    component.add_child(Arc::new(Text::new(Some(&highlighted), Some(0), Some(0), None)));
                } else {
                    let t = theme.clone();
                    component.add_child(Arc::new(VisualLinePreview::new(
                        highlighted,
                        CODE_PREVIEW_LINES,
                        VisualTruncateKeep::Start,
                        Arc::new(move |hidden| expand_hint(&t, hidden, "lines")),
                    )));
                }
            }
            Arc::new(component)
        }
    }
}

pub fn render_result(
    result: AgentToolResult,
    options: ToolRenderResultOptions,
    theme: Theme,
    context: ToolRenderContext,
) -> Arc<dyn Component> {
    let timer_text = tool_timer_text(&context, options.is_partial);
    let component = Container::new();
    let details: Option<CodemodeToolDetails> = result
        .details
        .as_ref()
        .and_then(|v| pi_js::json::parse_as(&pi_js::json::stringify(v)).ok());

    if let Some(ref det) = details
        && !det.calls.is_empty()
    {
        let calls = &det.calls;
        let shown: &[CodemodeNestedCall] = if options.expanded || calls.len() <= CALL_PREVIEW_COUNT {
            calls
        } else {
            &calls[calls.len() - CALL_PREVIEW_COUNT..]
        };
        let mut lines: Vec<String> = shown
            .iter()
            .map(|call| format_call(call, &theme, options.expanded))
            .collect();
        if shown.len() < calls.len() {
            let earlier = calls.len() - shown.len();
            lines.insert(
                0,
                format!(
                    "{} {}{}",
                    theme.fg("muted", &format!("... ({earlier} earlier calls,")),
                    key_hint("app.tools.expand", "to expand", &theme),
                    theme.fg("muted", ")")
                ),
            );
        }
        let priced: Vec<&CodemodeNestedCall> = calls
            .iter()
            .filter(|call| call.cost.is_some_and(|c| c != 0.0))
            .collect();
        if priced.len() > 1 {
            let total: f64 = priced.iter().filter_map(|c| c.cost).sum();
            lines.push(theme.fg("muted", &format!("Model calls: {}", format_cost(total))));
        }
        component.add_child(Arc::new(Spacer::new(Some(1))));
        component.add_child(Arc::new(Text::new(Some(&lines.join("\n")), Some(0), Some(0), None)));
    }

    let has_header = matches!(
        result.content.first(),
        Some(UserContentBlock::Text(t)) if SCRIPT_HEADER_RE.is_match(&t.text)
    );
    let output = if options.is_partial {
        String::new()
    } else {
        let slice = if has_header {
            &result.content[1..]
        } else {
            &result.content[..]
        };
        let values: Vec<Value> = slice.iter().map(content_block_to_value).collect();
        pi_js::str16::trim(&get_text_output(&values, context.show_images)).to_string()
    };

    if !output.is_empty() {
        let color = if context.is_error { "error" } else { "toolOutput" };
        let styled = replace_tabs(&output)
            .split('\n')
            .map(|line| theme.fg(color, line))
            .collect::<Vec<_>>()
            .join("\n");
        component.add_child(Arc::new(Spacer::new(Some(1))));
        if options.expanded {
            component.add_child(Arc::new(Text::new(Some(&styled), Some(0), Some(0), None)));
        } else {
            let t = theme.clone();
            component.add_child(Arc::new(VisualLinePreview::new(
                styled,
                OUTPUT_PREVIEW_LINES,
                VisualTruncateKeep::Start,
                Arc::new(move |hidden| expand_hint(&t, hidden, "lines")),
            )));
            if let Some(full_path) = details.and_then(|d| d.full_output_path) {
                component.add_child(Arc::new(Text::new(
                    Some(&theme.fg("muted", &format!("Full output: {full_path}"))),
                    Some(0),
                    Some(0),
                    None,
                )));
            }
        }
    }

    if let Some(timer) = timer_text {
        component.add_child(Arc::new(Spacer::new(Some(1))));
        component.add_child(Arc::new(Text::new(
            Some(&theme.fg("muted", &timer)),
            Some(0),
            Some(0),
            None,
        )));
    }

    Arc::new(component)
}

pub fn codemode_render_call() -> RenderCallFn {
    Arc::new(render_call)
}

pub fn codemode_render_result() -> RenderResultFn {
    Arc::new(render_result)
}
