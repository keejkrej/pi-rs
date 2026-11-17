//! Port of packages/coding-agent/src/core/tools/renderers/bash.ts

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::core::extensions::types::{RenderCallFn, RenderResultFn, ToolRenderContext};
use crate::core::tools::bash::BashToolDetails;
use crate::core::tools::render_utils::{replace_tabs, str_val};
use crate::core::tools::renderers::extract_text_output;
use crate::core::tools::renderers::w3b_wire::key_hint;
use crate::core::tools::truncate::format_size;

fn now_epoch_ms() -> u64 {
    pi_js::time::now_ms().max(0) as u64
}

const BASH_PREVIEW_LINES: usize = 5;

#[derive(Default)]
struct BashTimerState {
    started_at_ms: Option<u64>,
    ended_at_ms: Option<u64>,
    timer_spawned: bool,
    call_text: Option<pi_tui::Text>,
    result_text: Option<pi_tui::Text>,
}

fn reuse_bash_text(context: &ToolRenderContext, is_result: bool, content: &str) -> Arc<dyn pi_tui::Component> {
    if let Ok(mut guard) = context.state.lock() {
        let state = guard.get_or_insert_with(|| Box::new(BashTimerState::default()));
        if let Some(st) = state.downcast_mut::<BashTimerState>() {
            let slot = if is_result {
                &mut st.result_text
            } else {
                &mut st.call_text
            };
            if let Some(existing) = slot.as_ref() {
                existing.set_text(content);
                return Arc::new(existing.clone());
            }
            let widget = pi_tui::Text::new(Some(content), Some(0), Some(0), None);
            *slot = Some(widget.clone());
            return Arc::new(widget);
        }
    }
    Arc::new(pi_tui::Text::new(Some(content), Some(0), Some(0), None))
}

fn format_duration_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", (ms as f64) / 1000.0)
    } else {
        let total_secs = ms / 1000;
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        format!("{mins}m {secs}s")
    }
}

pub fn render_call() -> RenderCallFn {
    Arc::new(|args: Value, theme, context: ToolRenderContext| {
        let cmd = str_val(args.get("command")).unwrap_or_default();
        let timeout = args.get("timeout").and_then(Value::as_f64);

        let mut text = if cmd.is_empty() {
            let placeholder = if context.args_complete { "(no command)" } else { "..." };
            format!(
                "{} {}",
                theme.fg("toolTitle", &theme.bold("$")),
                theme.fg("muted", placeholder)
            )
        } else {
            theme.fg("toolTitle", &theme.bold(&format!("$ {}", replace_tabs(&cmd))))
        };

        if let Some(secs) = timeout {
            let suffix = format!(" (timeout {secs}s)");
            text.push_str(&theme.fg("dim", &suffix));
        }

        reuse_bash_text(&context, false, &text)
    })
}

pub fn render_result() -> RenderResultFn {
    Arc::new(|result, options, theme, context: ToolRenderContext| {
        let raw_output = extract_text_output(&result.content, context.show_images);
        let lines: Vec<String> = if raw_output.is_empty() {
            Vec::new()
        } else {
            raw_output.split('\n').map(replace_tabs).collect()
        };

        let mut duration_ms = None;
        if let Ok(mut guard) = context.state.lock() {
            let state = guard.get_or_insert_with(|| Box::new(BashTimerState::default()));
            if let Some(st) = state.downcast_mut::<BashTimerState>() {
                if st.started_at_ms.is_none() {
                    st.started_at_ms = Some(now_epoch_ms());
                }
                if options.is_partial {
                    if !st.timer_spawned && context.execution_started {
                        st.timer_spawned = true;
                        let invalidate = Arc::clone(&context.invalidate);
                        if let Ok(handle) = tokio::runtime::Handle::try_current() {
                            handle.spawn(async move {
                                tokio::time::sleep(Duration::from_secs(1)).await;
                                invalidate();
                            });
                        }
                    }
                } else if st.ended_at_ms.is_none() {
                    st.ended_at_ms = Some(now_epoch_ms());
                }
                if let (Some(start), Some(end)) = (st.started_at_ms, st.ended_at_ms) {
                    duration_ms = Some(end.saturating_sub(start));
                } else if options.is_partial
                    && let Some(start) = st.started_at_ms
                {
                    duration_ms = Some(now_epoch_ms().saturating_sub(start));
                }
            }
        }

        let mut rendered_parts: Vec<String> = Vec::new();
        if !lines.is_empty() {
            if !options.expanded && lines.len() > BASH_PREVIEW_LINES {
                let hidden = lines.len() - BASH_PREVIEW_LINES;
                let tail = &lines[lines.len() - BASH_PREVIEW_LINES..];
                let hint = key_hint("expandTools", "to expand");
                rendered_parts.push(theme.fg("muted", &format!("... ({hidden} earlier lines, {hint})")));
                for l in tail {
                    rendered_parts.push(theme.fg("toolOutput", l));
                }
            } else {
                for l in &lines {
                    rendered_parts.push(theme.fg("toolOutput", l));
                }
            }
        }

        if let Some(details_val) = &result.details
            && let Ok(details) = serde_json::from_value::<BashToolDetails>(details_val.clone())
            && let Some(trunc) = details.truncation
            && trunc.truncated
        {
            let note = match details.full_output_path {
                Some(path) => format!(
                    "[Truncated ({} of {}). Full output: {path}]",
                    format_size(trunc.output_bytes.max(0) as usize),
                    format_size(trunc.total_bytes.max(0) as usize)
                ),
                None => format!(
                    "[Truncated ({} of {})]",
                    format_size(trunc.output_bytes.max(0) as usize),
                    format_size(trunc.total_bytes.max(0) as usize)
                ),
            };
            rendered_parts.push(theme.fg("warning", &note));
        }

        if let Some(ms) = duration_ms {
            rendered_parts.push(theme.fg("dim", &format!("({})", format_duration_ms(ms))));
        }

        let content = rendered_parts.join("\n");
        reuse_bash_text(&context, true, &content)
    })
}
