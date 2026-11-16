//! Port of packages/coding-agent/src/modes/interactive/components/skill-invocation-message.ts

use std::sync::{Arc, Mutex};

use pi_tui::components::box_::TuiBox;
use pi_tui::components::markdown::DefaultTextStyle;
use pi_tui::{
    Component, Container, Markdown, MarkdownTheme, MouseRegion, Text, TuiMouseButton, TuiMouseEvent,
    TuiMouseEventResult, TuiMouseEventType,
};
use serde::{Deserialize, Serialize};

use super::super::theme::theme::{ThemeBg, ThemeColor, get_markdown_theme, theme};
use super::keybinding_hints::key_text;

// WIRE(@tui-w3): Replace local mirror type with `crate::core::agent_session::ParsedSkillBlock` once @ca-cli merges onto main.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedSkillBlock {
    pub name: String,
    pub location: String,
    pub content: String,
    pub user_message: Option<String>,
}

struct SkillInvocationState {
    expanded: bool,
}

struct SkillInvocationInner {
    content_box: TuiBox,
    skill_block: ParsedSkillBlock,
    markdown_theme: MarkdownTheme,
    state: Mutex<SkillInvocationState>,
}

/// Component that renders a skill invocation message with collapsed/expanded state.
/// Uses same background color as custom messages for visual consistency.
/// Only renders the skill block itself - user message is rendered separately.
#[derive(Clone)]
pub struct SkillInvocationMessageComponent {
    inner: Arc<SkillInvocationInner>,
}

impl SkillInvocationMessageComponent {
    pub fn new(skill_block: ParsedSkillBlock, markdown_theme: Option<MarkdownTheme>) -> Self {
        let content_box = TuiBox::new(
            Some(1),
            Some(1),
            Some(Arc::new(|t| theme().bg(ThemeBg::CustomMessageBg, t))),
        );
        let comp = Self {
            inner: Arc::new(SkillInvocationInner {
                content_box,
                skill_block,
                markdown_theme: markdown_theme.unwrap_or_else(get_markdown_theme),
                state: Mutex::new(SkillInvocationState { expanded: false }),
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

        let expanded = self.inner.state.lock().unwrap().expanded;
        if expanded {
            let label = t.fg(ThemeColor::CustomMessageLabel, "\x1b[1m[skill]\x1b[22m");
            content.add_child(Arc::new(Text::new(Some(&label), Some(0), Some(0), None)));
            let header = format!("**{}**\n\n", self.inner.skill_block.name);
            content.add_child(Arc::new(Markdown::new(
                &format!("{header}{}", self.inner.skill_block.content),
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
            let line = format!(
                "{}{}{}",
                t.fg(ThemeColor::CustomMessageLabel, "\x1b[1m[skill]\x1b[22m "),
                t.fg(ThemeColor::CustomMessageText, &self.inner.skill_block.name),
                t.fg(
                    ThemeColor::Dim,
                    &format!(" ({} to expand)", key_text(&"app.tools.expand".to_string())),
                ),
            );
            content.add_child(Arc::new(Text::new(Some(&line), Some(0), Some(0), None)));
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

impl Component for SkillInvocationMessageComponent {
    fn render(&self, width: usize) -> Vec<String> {
        SkillInvocationMessageComponent::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        SkillInvocationMessageComponent::handle_mouse(self, event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        SkillInvocationMessageComponent::invalidate(self)
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.content_box.children())
    }
}
