//! Port of packages/coding-agent/src/modes/interactive/components/bash-execution.ts

use std::sync::{Arc, Mutex};

pub use crate::core::tools::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncationOptions, TruncationResult, truncate_tail,
};
use pi_tui::{Component, Container, Loader, Spacer, TUI, Text, TuiMouseEvent, TuiMouseEventResult};

use super::super::theme::theme::{ThemeColor, theme};
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::{key_hint, key_text};
use super::visual_truncate::truncate_to_visual_lines;

const PREVIEW_LINES: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BashStatus {
    Running,
    Complete,
    Cancelled,
    Error,
}

struct VisualPreviewCache {
    width: usize,
    lines: Vec<String>,
}

struct VisualPreviewComponent {
    styled_input: String,
    output_pad: i64,
    cache: Mutex<Option<VisualPreviewCache>>,
}

impl Component for VisualPreviewComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let mut cache = self.cache.lock().unwrap();
        if let Some(ref c) = *cache
            && c.width == width
        {
            return c.lines.clone();
        }
        let result = truncate_to_visual_lines(&self.styled_input, PREVIEW_LINES, width, Some(self.output_pad), None);
        let lines = result.visual_lines;
        *cache = Some(VisualPreviewCache {
            width,
            lines: lines.clone(),
        });
        lines
    }

    fn handle_input(&self, _data: &str) {}
    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }
    fn wants_key_release(&self) -> bool {
        false
    }
    fn invalidate(&self) {
        *self.cache.lock().unwrap() = None;
    }
}

struct BashExecutionState {
    output_lines: Vec<String>,
    status: BashStatus,
    exit_code: Option<i64>,
    truncation_result: Option<TruncationResult>,
    full_output_path: Option<String>,
    expanded: bool,
    output_pad: i64,
}

struct BashExecutionInner {
    command: String,
    color_key: ThemeColor,
    container: Container,
    content_container: Container,
    loader: Loader,
    state: Mutex<BashExecutionState>,
}

impl Drop for BashExecutionInner {
    fn drop(&mut self) {
        self.loader.stop();
    }
}

/// Component for displaying bash command execution with streaming output.
#[derive(Clone)]
pub struct BashExecutionComponent {
    inner: Arc<BashExecutionInner>,
}

impl BashExecutionComponent {
    pub fn new(
        command: &str,
        ui: Arc<dyn TUI + Send + Sync>,
        exclude_from_context: Option<bool>,
        output_pad: Option<i64>,
    ) -> Self {
        let color_key = if exclude_from_context.unwrap_or(false) {
            ThemeColor::Dim
        } else {
            ThemeColor::BashMode
        };
        let border_color: Arc<dyn Fn(&str) -> String + Send + Sync> = Arc::new(move |s: &str| theme().fg(color_key, s));

        let container = Container::new();
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(DynamicBorder::new(Some(Arc::clone(&border_color)))));

        let content_container = Container::new();
        container.add_child(Arc::new(content_container.clone()));

        let loader = Loader::new(
            ui,
            Arc::new(move |spinner: &str| theme().fg(color_key, spinner)),
            Arc::new(|text: &str| theme().fg(ThemeColor::Muted, text)),
            Some(&format!(
                "Running... ({} to cancel)",
                key_text(&"tui.select.cancel".to_string())
            )),
            None,
        );

        container.add_child(Arc::new(DynamicBorder::new(Some(border_color))));

        let comp = Self {
            inner: Arc::new(BashExecutionInner {
                command: command.to_string(),
                color_key,
                container,
                content_container,
                loader,
                state: Mutex::new(BashExecutionState {
                    output_lines: Vec::new(),
                    status: BashStatus::Running,
                    exit_code: None,
                    truncation_result: None,
                    full_output_path: None,
                    expanded: false,
                    output_pad: output_pad.unwrap_or(1),
                }),
            }),
        };
        comp.update_display();
        comp
    }

    pub fn set_expanded(&self, expanded: bool) {
        self.inner.state.lock().unwrap().expanded = expanded;
        self.update_display();
    }

    pub fn set_output_pad(&self, output_pad: i64) {
        self.inner.state.lock().unwrap().output_pad = output_pad;
        self.update_display();
    }

    pub fn append_output(&self, chunk: &str) {
        let clean = crate::utils::ansi::strip_ansi(chunk)
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        let new_lines: Vec<&str> = clean.split('\n').collect();
        {
            let mut state = self.inner.state.lock().unwrap();
            if let Some(last) = state.output_lines.last_mut()
                && let Some(first_new) = new_lines.first()
            {
                last.push_str(first_new);
                for &extra in &new_lines[1..] {
                    state.output_lines.push(extra.to_string());
                }
            } else {
                for line in new_lines {
                    state.output_lines.push(line.to_string());
                }
            }
        }
        self.update_display();
    }

    pub fn set_complete(
        &self,
        exit_code: Option<i64>,
        cancelled: bool,
        truncation_result: Option<TruncationResult>,
        full_output_path: Option<&str>,
    ) {
        {
            let mut state = self.inner.state.lock().unwrap();
            state.exit_code = exit_code;
            state.status = if cancelled {
                BashStatus::Cancelled
            } else if matches!(exit_code, Some(code) if code != 0) {
                BashStatus::Error
            } else {
                BashStatus::Complete
            };
            state.truncation_result = truncation_result;
            state.full_output_path = full_output_path.map(ToString::to_string);
        }
        self.inner.loader.stop();
        self.update_display();
    }

    pub fn get_output(&self) -> String {
        self.inner.state.lock().unwrap().output_lines.join("\n")
    }

    pub fn get_command(&self) -> &str {
        &self.inner.command
    }

    fn update_display(&self) {
        let (full_output, status, exit_code, truncation_result, full_output_path, expanded, output_pad) = {
            let state = self.inner.state.lock().unwrap();
            (
                state.output_lines.join("\n"),
                state.status,
                state.exit_code,
                state.truncation_result.clone(),
                state.full_output_path.clone(),
                state.expanded,
                state.output_pad,
            )
        };

        let context_truncation = truncate_tail(
            &full_output,
            Some(TruncationOptions {
                max_lines: Some(DEFAULT_MAX_LINES),
                max_bytes: Some(DEFAULT_MAX_BYTES),
            }),
        );
        let available_lines: Vec<&str> = if context_truncation.content.is_empty() {
            Vec::new()
        } else {
            context_truncation.content.split('\n').collect()
        };

        let start = available_lines.len().saturating_sub(PREVIEW_LINES);
        let preview_logical_lines = &available_lines[start..];
        let hidden_line_count = start;

        let t = theme();
        self.inner.content_container.clear();

        let header = Text::new(
            Some(&t.fg(self.inner.color_key, &t.bold(&format!("$ {}", self.inner.command)))),
            Some(output_pad),
            Some(0),
            None,
        );
        self.inner.content_container.add_child(Arc::new(header));

        if !available_lines.is_empty() {
            if expanded {
                let display_text = available_lines
                    .iter()
                    .map(|line| t.fg(ThemeColor::Muted, line))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.inner.content_container.add_child(Arc::new(Text::new(
                    Some(&format!("\n{display_text}")),
                    Some(output_pad),
                    Some(0),
                    None,
                )));
            } else {
                let styled_output = preview_logical_lines
                    .iter()
                    .map(|line| t.fg(ThemeColor::Muted, line))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.inner.content_container.add_child(Arc::new(VisualPreviewComponent {
                    styled_input: format!("\n{styled_output}"),
                    output_pad,
                    cache: Mutex::new(None),
                }));
            }
        }

        if status == BashStatus::Running {
            self.inner
                .content_container
                .add_child(Arc::new(self.inner.loader.clone()));
        } else {
            let mut status_parts: Vec<String> = Vec::new();
            if hidden_line_count > 0 {
                if expanded {
                    status_parts.push(format!(
                        "{}{}{}",
                        t.fg(ThemeColor::Muted, "("),
                        key_hint(&"app.tools.expand".to_string(), "to collapse"),
                        t.fg(ThemeColor::Muted, ")"),
                    ));
                } else {
                    status_parts.push(format!(
                        "{}{}{}",
                        t.fg(ThemeColor::Muted, &format!("... {hidden_line_count} more lines (")),
                        key_hint(&"app.tools.expand".to_string(), "to expand"),
                        t.fg(ThemeColor::Muted, ")"),
                    ));
                }
            }

            if status == BashStatus::Cancelled {
                status_parts.push(t.fg(ThemeColor::Warning, "(cancelled)"));
            } else if status == BashStatus::Error
                && let Some(code) = exit_code
            {
                status_parts.push(t.fg(ThemeColor::Error, &format!("(exit {code})")));
            }

            let was_truncated = truncation_result.as_ref().is_some_and(|r| r.truncated) || context_truncation.truncated;
            if was_truncated && let Some(ref path) = full_output_path {
                status_parts.push(t.fg(ThemeColor::Warning, &format!("Output truncated. Full output: {path}")));
            }

            if !status_parts.is_empty() {
                self.inner.content_container.add_child(Arc::new(Text::new(
                    Some(&format!("\n{}", status_parts.join("\n"))),
                    Some(output_pad),
                    Some(0),
                    None,
                )));
            }
        }
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
        self.update_display();
    }
}

impl Component for BashExecutionComponent {
    fn render(&self, width: usize) -> Vec<String> {
        BashExecutionComponent::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        BashExecutionComponent::invalidate(self)
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.container.children())
    }
}
