//! Port of packages/tui/src/components/text.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex, MutexGuard};

use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult};

type TextColorFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

struct TextState {
    text: String,
    /// Left/right padding.
    padding_x: i64,
    /// Top/bottom padding.
    padding_y: i64,
    custom_bg_fn: Option<TextColorFn>,
    cached_text: Option<String>,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

struct TextInner {
    state: Mutex<TextState>,
}

/// Text component - displays multi-line text with word wrapping.
#[derive(Clone)]
pub struct Text {
    inner: Arc<TextInner>,
}

impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Text {}

impl Text {
    fn state(&self) -> MutexGuard<'_, TextState> {
        self.inner.state.lock().unwrap()
    }

    /// `text` `None` is `""`. `padding_x` and `padding_y` `None` are `1`.
    pub fn new(
        text: Option<&str>,
        padding_x: Option<i64>,
        padding_y: Option<i64>,
        custom_bg_fn: Option<TextColorFn>,
    ) -> Self {
        Self {
            inner: Arc::new(TextInner {
                state: Mutex::new(TextState {
                    text: text.unwrap_or("").to_string(),
                    padding_x: padding_x.unwrap_or(1),
                    padding_y: padding_y.unwrap_or(1),
                    custom_bg_fn,
                    cached_text: None,
                    cached_width: None,
                    cached_lines: None,
                }),
            }),
        }
    }

    pub fn set_text(&self, text: &str) {
        let mut state = self.state();
        if text == state.text {
            return;
        }
        state.text = text.to_string();
        state.cached_text = None;
        state.cached_width = None;
        state.cached_lines = None;
    }

    pub fn set_custom_bg_fn(&self, custom_bg_fn: Option<TextColorFn>) {
        let mut state = self.state();
        state.custom_bg_fn = custom_bg_fn;
        state.cached_text = None;
        state.cached_width = None;
        state.cached_lines = None;
    }

    pub fn set_padding_x(&self, padding_x: i64) {
        let mut state = self.state();
        state.padding_x = padding_x;
        state.cached_text = None;
        state.cached_width = None;
        state.cached_lines = None;
    }

    pub fn invalidate(&self) {
        let mut state = self.state();
        state.cached_text = None;
        state.cached_width = None;
        state.cached_lines = None;
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let (text, state_padding_x, state_padding_y, custom_bg_fn) = {
            let mut state = self.state();
            if let Some(cached) = &state.cached_lines
                && state.cached_text.as_deref() == Some(state.text.as_str())
                && state.cached_width == Some(width)
            {
                return cached.clone();
            }

            if state.text.is_empty() || pi_js::str16::trim(&state.text).is_empty() {
                let result = Vec::new();
                state.cached_text = Some(state.text.clone());
                state.cached_width = Some(width);
                state.cached_lines = Some(result.clone());
                return result;
            }

            (
                state.text.clone(),
                state.padding_x,
                state.padding_y,
                state.custom_bg_fn.clone(),
            )
        };

        let normalized_text = text.replace('\t', "   ");
        let max_pad = ((width as i64 - 1).div_euclid(2)).max(0);
        let padding_x = state_padding_x.min(max_pad);
        let content_width = (width as i64 - padding_x * 2).max(1);

        let wrapped_lines = crate::utils::wrap_text_with_ansi(&normalized_text, content_width);
        let margin = " ".repeat(padding_x.max(0) as usize);
        let mut content_lines = Vec::with_capacity(wrapped_lines.len());

        for line in wrapped_lines {
            let line_with_margins = format!("{margin}{line}{margin}");
            if let Some(ref bg_fn) = custom_bg_fn {
                content_lines.push(crate::utils::apply_background_to_line(
                    &line_with_margins,
                    width as i64,
                    bg_fn.clone(),
                ));
            } else {
                let visible_len = crate::utils::visible_width(&line_with_margins);
                let padding_needed = (width as i64 - visible_len).max(0) as usize;
                content_lines.push(format!("{line_with_margins}{}", " ".repeat(padding_needed)));
            }
        }

        let empty_line = " ".repeat(width);
        let padding_y_count = state_padding_y.max(0) as usize;
        let mut empty_lines = Vec::with_capacity(padding_y_count);
        for _ in 0..padding_y_count {
            let line = if let Some(ref bg_fn) = custom_bg_fn {
                crate::utils::apply_background_to_line(&empty_line, width as i64, bg_fn.clone())
            } else {
                empty_line.clone()
            };
            empty_lines.push(line);
        }

        let mut result = Vec::with_capacity(empty_lines.len() * 2 + content_lines.len());
        result.extend(empty_lines.iter().cloned());
        result.extend(content_lines);
        result.extend(empty_lines);
        crate::utils::flatten_lines(&result);

        {
            let mut state = self.state();
            if state.text == text {
                state.cached_text = Some(text);
                state.cached_width = Some(width);
                state.cached_lines = Some(result.clone());
            }
        }

        if result.is_empty() { vec![String::new()] } else { result }
    }
}

impl Component for Text {
    fn render(&self, width: usize) -> Vec<String> {
        Text::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        Text::invalidate(self)
    }
}
