//! Port of packages/coding-agent/src/modes/interactive/components/compaction-summary-message.ts

use std::sync::{Arc, Mutex};

pub use crate::core::messages::CompactionSummaryMessage;
use pi_tui::components::box_::TuiBox;
use pi_tui::components::markdown::DefaultTextStyle;
use pi_tui::{
    Component, Container, Markdown, MarkdownTheme, MouseRegion, Spacer, Text, TuiMouseButton, TuiMouseEvent,
    TuiMouseEventResult, TuiMouseEventType,
};

use super::super::theme::theme::{ThemeBg, ThemeColor, get_markdown_theme, theme};
use super::keybinding_hints::key_text;

struct CompactionSummaryState {
    expanded: bool,
}

struct CompactionSummaryInner {
    content_box: TuiBox,
    message: CompactionSummaryMessage,
    markdown_theme: MarkdownTheme,
    state: Mutex<CompactionSummaryState>,
}

/// Component that renders a compaction message with collapsed/expanded state.
/// Uses same background color as custom messages for visual consistency.
#[derive(Clone)]
pub struct CompactionSummaryMessageComponent {
    inner: Arc<CompactionSummaryInner>,
}

impl CompactionSummaryMessageComponent {
    pub fn new(message: CompactionSummaryMessage, markdown_theme: Option<MarkdownTheme>) -> Self {
        let content_box = TuiBox::new(
            Some(1),
            Some(1),
            Some(Arc::new(|t| theme().bg(ThemeBg::CustomMessageBg, t))),
        );
        let comp = Self {
            inner: Arc::new(CompactionSummaryInner {
                content_box,
                message,
                markdown_theme: markdown_theme.unwrap_or_else(get_markdown_theme),
                state: Mutex::new(CompactionSummaryState { expanded: false }),
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
        self.inner.content_box.set_padding_x(output_pad);
    }

    fn update_display(&self) {
        self.inner.content_box.clear();
        let content = Container::new();
        let t = theme();

        let token_str = pi_js::intl::number_to_locale_string(self.inner.message.tokens_before as f64);
        let label = t.fg(ThemeColor::CustomMessageLabel, "\x1b[1m[compaction]\x1b[22m");
        content.add_child(Arc::new(Text::new(Some(&label), Some(0), Some(0), None)));
        content.add_child(Arc::new(Spacer::new(Some(1))));

        let expanded = self.inner.state.lock().unwrap().expanded;
        if expanded {
            let header = format!("**Compacted from {token_str} tokens**\n\n");
            content.add_child(Arc::new(Markdown::new(
                &format!("{header}{}", self.inner.message.summary),
                0,
                0,
                self.inner.markdown_theme.clone(),
                Some(DefaultTextStyle {
                    color: Some(Arc::new(|text: &str| theme().fg(ThemeColor::CustomMessageText, text))),
                    ..Default::default()
                }),
                None,
            )));
        } else {
            let collapsed = format!(
                "{}{}{}",
                t.fg(
                    ThemeColor::CustomMessageText,
                    &format!("Compacted from {token_str} tokens (")
                ),
                t.fg(ThemeColor::Dim, &key_text(&"app.tools.expand".to_string())),
                t.fg(ThemeColor::CustomMessageText, " to expand)"),
            );
            content.add_child(Arc::new(Text::new(Some(&collapsed), Some(0), Some(0), None)));
        }

        let comp = self.clone();
        self.inner.content_box.add_child(Arc::new(MouseRegion::new(
            Arc::new(content),
            Arc::new(move |event: &TuiMouseEvent| {
                if event.r#type != TuiMouseEventType::Click || event.button != TuiMouseButton::Left {
                    return None;
                }
                let currently_expanded = comp.inner.state.lock().unwrap().expanded;
                comp.set_expanded(!currently_expanded);
                Some(TuiMouseEventResult {
                    handled: Some(true),
                    ..Default::default()
                })
            }),
        )));
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.content_box.render(width)
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.content_box.handle_mouse(event)
    }

    pub fn invalidate(&self) {
        self.inner.content_box.invalidate();
        self.update_display();
    }
}

impl Component for CompactionSummaryMessageComponent {
    fn render(&self, width: usize) -> Vec<String> {
        CompactionSummaryMessageComponent::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        CompactionSummaryMessageComponent::handle_mouse(self, event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        CompactionSummaryMessageComponent::invalidate(self)
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.content_box.children())
    }
}
