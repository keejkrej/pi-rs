//! Port of packages/coding-agent/src/modes/interactive/components/custom-message.ts

use std::sync::{Arc, Mutex};

pub use crate::core::messages::{CustomMessage, CustomMessageContent};
use pi_ai::UserContentBlock;
use pi_tui::components::box_::TuiBox;
use pi_tui::components::markdown::DefaultTextStyle;
use pi_tui::{Component, Container, Markdown, MarkdownTheme, Spacer, Text, TuiMouseEvent, TuiMouseEventResult};
use serde::{Deserialize, Serialize};

use super::super::theme::theme::{Theme, ThemeBg, ThemeColor, get_markdown_theme, theme};

// WIRE(@tui-w3): Replace local `MessageRenderOptions`/`MessageRenderer` with `crate::core::extensions::types::{MessageRenderOptions, MessageRenderer}` once @codemode merges.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRenderOptions {
    pub expanded: bool,
    pub output_pad: i64,
}

pub type MessageRenderer =
    Arc<dyn Fn(CustomMessage, MessageRenderOptions, Theme) -> Option<Arc<dyn Component>> + Send + Sync>;

struct CustomMessageState {
    custom_component: Option<Arc<dyn Component>>,
    expanded: bool,
    output_pad: i64,
}

struct CustomMessageInner {
    container: Container,
    message: CustomMessage,
    custom_renderer: Option<MessageRenderer>,
    content_box: TuiBox,
    markdown_theme: MarkdownTheme,
    state: Mutex<CustomMessageState>,
}

/// Component that renders a custom message entry from extensions.
/// Uses distinct styling to differentiate from user messages.
#[derive(Clone)]
pub struct CustomMessageComponent {
    inner: Arc<CustomMessageInner>,
}

impl CustomMessageComponent {
    pub fn new(
        message: CustomMessage,
        custom_renderer: Option<MessageRenderer>,
        markdown_theme: Option<MarkdownTheme>,
        output_pad: Option<i64>,
    ) -> Self {
        let pad = output_pad.unwrap_or(1);
        let container = Container::new();
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let content_box = TuiBox::new(
            Some(pad),
            Some(1),
            Some(Arc::new(|t| theme().bg(ThemeBg::CustomMessageBg, t))),
        );

        let comp = Self {
            inner: Arc::new(CustomMessageInner {
                container,
                message,
                custom_renderer,
                content_box,
                markdown_theme: markdown_theme.unwrap_or_else(get_markdown_theme),
                state: Mutex::new(CustomMessageState {
                    custom_component: None,
                    expanded: false,
                    output_pad: pad,
                }),
            }),
        };
        comp.rebuild();
        comp
    }

    pub fn set_expanded(&self, expanded: bool) {
        let changed = {
            let mut state = self.inner.state.lock().unwrap();
            if state.expanded != expanded {
                state.expanded = expanded;
                true
            } else {
                false
            }
        };
        if changed {
            self.rebuild();
        }
    }

    pub fn set_output_pad(&self, output_pad: i64) {
        let changed = {
            let mut state = self.inner.state.lock().unwrap();
            if state.output_pad != output_pad {
                state.output_pad = output_pad;
                true
            } else {
                false
            }
        };
        if changed {
            self.rebuild();
        }
    }

    fn rebuild(&self) {
        let (prev_custom, expanded, output_pad) = {
            let mut state = self.inner.state.lock().unwrap();
            (state.custom_component.take(), state.expanded, state.output_pad)
        };
        if let Some(prev) = prev_custom {
            self.inner.container.remove_child(&prev);
        }
        let box_arc: Arc<dyn Component> = Arc::new(self.inner.content_box.clone());
        self.inner.container.remove_child(&box_arc);

        if let Some(ref renderer) = self.inner.custom_renderer
            && let Some(component) = renderer(
                self.inner.message.clone(),
                MessageRenderOptions { expanded, output_pad },
                theme(),
            )
        {
            self.inner.state.lock().unwrap().custom_component = Some(Arc::clone(&component));
            self.inner.container.add_child(component);
            return;
        }

        self.inner.container.add_child(box_arc);
        self.inner.content_box.clear();
        self.inner.content_box.set_padding_x(output_pad);

        let t = theme();
        let label = t.fg(
            ThemeColor::CustomMessageLabel,
            &format!("\x1b[1m[{}]\x1b[22m", self.inner.message.custom_type),
        );
        self.inner
            .content_box
            .add_child(Arc::new(Text::new(Some(&label), Some(0), Some(0), None)));
        self.inner.content_box.add_child(Arc::new(Spacer::new(Some(1))));

        let text = match &self.inner.message.content {
            CustomMessageContent::Text(s) => s.clone(),
            CustomMessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    UserContentBlock::Text(tc) => Some(tc.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        };

        self.inner.content_box.add_child(Arc::new(Markdown::new(
            &text,
            0,
            0,
            self.inner.markdown_theme.clone(),
            Some(DefaultTextStyle {
                color: Some(Arc::new(|content: &str| {
                    theme().fg(ThemeColor::CustomMessageText, content)
                })),
                ..Default::default()
            }),
            None,
        )));
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
        self.rebuild();
    }
}

impl Component for CustomMessageComponent {
    fn render(&self, width: usize) -> Vec<String> {
        CustomMessageComponent::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        CustomMessageComponent::invalidate(self)
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.container.children())
    }
}
