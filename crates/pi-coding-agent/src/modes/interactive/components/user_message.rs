//! Port of packages/coding-agent/src/modes/interactive/components/user-message.ts

use std::sync::{Arc, Mutex};

use pi_tui::components::markdown::{DefaultTextStyle, MarkdownOptions};
use pi_tui::{Component, Container, Markdown, MarkdownTheme, TuiMouseEvent, TuiMouseEventResult};

use super::super::theme::theme::{ThemeBg, ThemeColor, get_markdown_theme, theme};
use super::markdown_transform::{MarkdownMessageType, MarkdownTransformer, create_markdown_transform};

const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";

struct UserMessageState {
    markdown_theme: MarkdownTheme,
    output_pad: i64,
    markdown_transformers: Vec<MarkdownTransformer>,
}

struct UserMessageInner {
    container: Container,
    text: String,
    state: Mutex<UserMessageState>,
}

/// Component that renders a user message.
#[derive(Clone)]
pub struct UserMessageComponent {
    inner: Arc<UserMessageInner>,
}

impl UserMessageComponent {
    pub fn new(
        text: &str,
        markdown_theme: Option<MarkdownTheme>,
        output_pad: Option<i64>,
        markdown_transformers: Option<Vec<MarkdownTransformer>>,
    ) -> Self {
        let comp = Self {
            inner: Arc::new(UserMessageInner {
                container: Container::new(),
                text: text.to_string(),
                state: Mutex::new(UserMessageState {
                    markdown_theme: markdown_theme.unwrap_or_else(get_markdown_theme),
                    output_pad: output_pad.unwrap_or(1),
                    markdown_transformers: markdown_transformers.unwrap_or_default(),
                }),
            }),
        };
        comp.rebuild();
        comp
    }

    pub fn set_output_pad(&self, padding: i64) {
        self.inner.state.lock().unwrap().output_pad = padding;
        self.rebuild();
    }

    fn rebuild(&self) {
        self.inner.container.clear();
        let (markdown_theme, output_pad, transformers) = {
            let state = self.inner.state.lock().unwrap();
            (
                state.markdown_theme.clone(),
                state.output_pad,
                state.markdown_transformers.clone(),
            )
        };
        self.inner.container.add_child(Arc::new(Markdown::new(
            &self.inner.text,
            output_pad,
            1,
            markdown_theme,
            Some(DefaultTextStyle {
                color: Some(Arc::new(|content: &str| {
                    theme().fg(ThemeColor::UserMessageText, content)
                })),
                bg_color: Some(Arc::new(|content: &str| theme().bg(ThemeBg::UserMessageBg, content))),
                ..Default::default()
            }),
            Some(MarkdownOptions {
                preserve_ordered_list_markers: Some(true),
                preserve_backslash_escapes: Some(true),
                transform: Some(create_markdown_transform(
                    MarkdownMessageType::User,
                    false,
                    transformers,
                )),
                render_latex: None,
            }),
        )));
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let mut lines = self.inner.container.render(width);
        if let Some(first) = lines.first_mut() {
            *first = format!("{OSC133_ZONE_START}{first}");
        }
        if let Some(last) = lines.last_mut() {
            *last = format!("{OSC133_ZONE_END}{OSC133_ZONE_FINAL}{last}");
        }
        lines
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
    }
}

impl Component for UserMessageComponent {
    fn render(&self, width: usize) -> Vec<String> {
        UserMessageComponent::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        UserMessageComponent::invalidate(self)
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.container.children())
    }
}
