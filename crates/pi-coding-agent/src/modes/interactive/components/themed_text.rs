//! Port of packages/coding-agent/src/modes/interactive/components/themed-text.ts

use std::sync::{Arc, Mutex};

use pi_tui::{Component, Text, TuiMouseEvent, TuiMouseEventResult};

struct ThemedTextInner {
    text: Text,
    build: Arc<dyn Fn() -> String + Send + Sync>,
    stale: Mutex<bool>,
}

/// Text whose content applies theme colors. Rebuilds its string from `build` after every invalidation.
#[derive(Clone)]
pub struct ThemedText {
    inner: Arc<ThemedTextInner>,
}

impl ThemedText {
    pub fn new(build: Arc<dyn Fn() -> String + Send + Sync>, padding_x: Option<i64>, padding_y: Option<i64>) -> Self {
        Self {
            inner: Arc::new(ThemedTextInner {
                text: Text::new(
                    Some(""),
                    Some(padding_x.unwrap_or(1)),
                    Some(padding_y.unwrap_or(1)),
                    None,
                ),
                build,
                stale: Mutex::new(true),
            }),
        }
    }

    pub fn invalidate(&self) {
        self.inner.text.invalidate();
        *self.inner.stale.lock().unwrap() = true;
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let should_rebuild = {
            let mut guard = self.inner.stale.lock().unwrap();
            let was_stale = *guard;
            *guard = false;
            was_stale
        };
        if should_rebuild {
            let content = (self.inner.build)();
            self.inner.text.set_text(&content);
        }
        self.inner.text.render(width)
    }
}

impl Component for ThemedText {
    fn render(&self, width: usize) -> Vec<String> {
        ThemedText::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        ThemedText::invalidate(self)
    }
}
