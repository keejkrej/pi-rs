//! Port of packages/coding-agent/src/modes/interactive/components/branch-summary-message.ts

use std::sync::{Arc, Mutex};

use pi_tui::components::box_::TuiBox;
use pi_tui::components::markdown::DefaultTextStyle;
use pi_tui::{
    Component, Container, Markdown, MarkdownTheme, MouseRegion, Spacer, Text, TuiMouseButton, TuiMouseEvent,
    TuiMouseEventResult, TuiMouseEventType,
};
use serde::{Deserialize, Serialize};

use super::super::theme::theme::{ThemeBg, ThemeColor, get_markdown_theme, theme};
use super::keybinding_hints::key_text;

// WIRE(@tui-w3): Replace local mirror type with `crate::core::messages::BranchSummaryMessage` once @mcp merges onto main.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryMessage {
    pub role: String,
    pub summary: String,
    pub from_id: Option<String>,
    pub timestamp: i64,
}

struct BranchSummaryState {
    expanded: bool,
}

struct BranchSummaryInner {
    content_box: TuiBox,
    message: BranchSummaryMessage,
    markdown_theme: MarkdownTheme,
    state: Mutex<BranchSummaryState>,
}

/// Component that renders a branch summary message with collapsed/expanded state.
/// Uses same background color as custom messages for visual consistency.
#[derive(Clone)]
pub struct BranchSummaryMessageComponent {
    inner: Arc<BranchSummaryInner>,
}

impl BranchSummaryMessageComponent {
    pub fn new(message: BranchSummaryMessage, markdown_theme: Option<MarkdownTheme>) -> Self {
        let content_box = TuiBox::new(
            Some(1),
            Some(1),
            Some(Arc::new(|t| theme().bg(ThemeBg::CustomMessageBg, t))),
        );
        let comp = Self {
            inner: Arc::new(BranchSummaryInner {
                content_box,
                message,
                markdown_theme: markdown_theme.unwrap_or_else(get_markdown_theme),
                state: Mutex::new(BranchSummaryState { expanded: false }),
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

        let label = t.fg(ThemeColor::CustomMessageLabel, "\x1b[1m[branch]\x1b[22m");
        content.add_child(Arc::new(Text::new(Some(&label), Some(0), Some(0), None)));
        content.add_child(Arc::new(Spacer::new(Some(1))));

        let expanded = self.inner.state.lock().unwrap().expanded;
        if expanded {
            let header = "**Branch Summary**\n\n";
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
                t.fg(ThemeColor::CustomMessageText, "Branch summary ("),
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

impl Component for BranchSummaryMessageComponent {
    fn render(&self, width: usize) -> Vec<String> {
        BranchSummaryMessageComponent::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        BranchSummaryMessageComponent::handle_mouse(self, event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        BranchSummaryMessageComponent::invalidate(self)
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.content_box.children())
    }
}
