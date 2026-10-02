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
        todo!("port: Text::new")
    }

    pub fn set_text(&self, text: &str) {
        todo!("port: Text::set_text")
    }

    pub fn set_custom_bg_fn(&self, custom_bg_fn: Option<TextColorFn>) {
        todo!("port: Text::set_custom_bg_fn")
    }

    pub fn invalidate(&self) {
        todo!("port: Text::invalidate")
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        todo!("port: Text::render")
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
