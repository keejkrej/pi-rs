//! Port of packages/tui/src/components/truncated-text.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex, MutexGuard};

use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult};

struct TruncatedTextState {
    text: String,
    padding_x: i64,
    padding_y: i64,
}

struct TruncatedTextInner {
    state: Mutex<TruncatedTextState>,
}

/// Text component that truncates to fit viewport width.
#[derive(Clone)]
pub struct TruncatedText {
    inner: Arc<TruncatedTextInner>,
}

impl PartialEq for TruncatedText {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for TruncatedText {}

impl TruncatedText {
    fn state(&self) -> MutexGuard<'_, TruncatedTextState> {
        self.inner.state.lock().unwrap()
    }

    /// `padding_x` and `padding_y` `None` are `0`.
    pub fn new(text: &str, padding_x: Option<i64>, padding_y: Option<i64>) -> Self {
        Self {
            inner: Arc::new(TruncatedTextInner {
                state: Mutex::new(TruncatedTextState {
                    text: text.to_string(),
                    padding_x: padding_x.unwrap_or(0),
                    padding_y: padding_y.unwrap_or(0),
                }),
            }),
        }
    }

    /// No cached state to invalidate currently.
    pub fn invalidate(&self) {}

    pub fn render(&self, width: usize) -> Vec<String> {
        let (text, padding_x, padding_y) = {
            let state = self.state();
            (state.text.clone(), state.padding_x, state.padding_y)
        };

        let padding_x_count = padding_x.max(0) as usize;
        let padding_y_count = padding_y.max(0) as usize;
        let mut result = Vec::with_capacity(padding_y_count * 2 + 1);
        let empty_line = " ".repeat(width);

        for _ in 0..padding_y_count {
            result.push(empty_line.clone());
        }

        let available_width = (width as i64 - padding_x * 2).max(1);
        let single_line_text = match text.find('\n') {
            Some(idx) => &text[..idx],
            None => &text,
        };
        let display_text = crate::utils::truncate_to_width(single_line_text, available_width, None, None);

        let h_padding = " ".repeat(padding_x_count);
        let line_with_padding = format!("{h_padding}{display_text}{h_padding}");
        let line_visible_width = crate::utils::visible_width(&line_with_padding);
        let padding_needed = (width as i64 - line_visible_width).max(0) as usize;
        let final_line = format!("{line_with_padding}{}", " ".repeat(padding_needed));
        result.push(final_line);

        for _ in 0..padding_y_count {
            result.push(empty_line.clone());
        }

        result
    }
}

impl Component for TruncatedText {
    fn render(&self, width: usize) -> Vec<String> {
        TruncatedText::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        TruncatedText::invalidate(self)
    }
}
