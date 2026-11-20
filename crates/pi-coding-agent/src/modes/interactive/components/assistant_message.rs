//! Port of packages/coding-agent/src/modes/interactive/components/assistant-message.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::{AssistantContentBlock, AssistantMessage, StopReason};
use pi_tui::components::markdown::{DefaultTextStyle, MarkdownOptions};
use pi_tui::{
    Component, Container, Markdown, MarkdownTheme, MouseRegion, Spacer, Text, TuiMouseButton, TuiMouseEvent,
    TuiMouseEventResult, TuiMouseEventType,
};

use super::super::theme::theme::{ThemeColor, get_markdown_theme, theme};
use super::markdown_transform::{
    MarkdownMessageType, MarkdownTransformer, create_markdown_transform_dynamic_with_meta,
};

const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";

#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkdownSlotKind {
    Text,
    Thinking,
}

#[derive(Clone)]
struct MarkdownSlot {
    kind: MarkdownSlotKind,
    markdown: Markdown,
}

struct AssistantMessageState {
    hide_thinking_block: bool,
    markdown_theme: MarkdownTheme,
    hidden_thinking_label: String,
    output_pad: i64,
    markdown_transformers: Vec<MarkdownTransformer>,
    message_id: Option<String>,
    timestamp: Option<String>,
    last_message: Option<AssistantMessage>,
    has_tool_calls: bool,
    thinking_visibility_overrides: IndexMap<usize, bool>,
    markdown_slots: Vec<MarkdownSlot>,
}

struct AssistantMessageInner {
    container: Container,
    content_container: Container,
    is_streaming: Arc<AtomicBool>,
    state: Mutex<AssistantMessageState>,
}

/// Component that renders a complete assistant message.
#[derive(Clone)]
pub struct AssistantMessageComponent {
    inner: Arc<AssistantMessageInner>,
}

impl AssistantMessageComponent {
    pub fn new(
        message: Option<AssistantMessage>,
        hide_thinking_block: Option<bool>,
        markdown_theme: Option<MarkdownTheme>,
        hidden_thinking_label: Option<&str>,
        output_pad: Option<i64>,
        markdown_transformers: Option<Vec<MarkdownTransformer>>,
    ) -> Self {
        Self::new_with_meta(
            message,
            hide_thinking_block,
            markdown_theme,
            hidden_thinking_label,
            output_pad,
            markdown_transformers,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_meta(
        message: Option<AssistantMessage>,
        hide_thinking_block: Option<bool>,
        markdown_theme: Option<MarkdownTheme>,
        hidden_thinking_label: Option<&str>,
        output_pad: Option<i64>,
        markdown_transformers: Option<Vec<MarkdownTransformer>>,
        message_id: Option<String>,
        timestamp: Option<String>,
    ) -> Self {
        let container = Container::new();
        let content_container = Container::new();
        container.add_child(Arc::new(content_container.clone()));

        let comp = Self {
            inner: Arc::new(AssistantMessageInner {
                container,
                content_container,
                is_streaming: Arc::new(AtomicBool::new(false)),
                state: Mutex::new(AssistantMessageState {
                    hide_thinking_block: hide_thinking_block.unwrap_or(false),
                    markdown_theme: markdown_theme.unwrap_or_else(get_markdown_theme),
                    hidden_thinking_label: hidden_thinking_label.unwrap_or("Thinking...").to_string(),
                    output_pad: output_pad.unwrap_or(1),
                    markdown_transformers: markdown_transformers.unwrap_or_default(),
                    message_id,
                    timestamp,
                    last_message: None,
                    has_tool_calls: false,
                    thinking_visibility_overrides: IndexMap::new(),
                    markdown_slots: Vec::new(),
                }),
            }),
        };

        if let Some(msg) = message {
            comp.update_content(msg, None);
        }

        comp
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
        let (last_message, is_streaming) = {
            let state = self.inner.state.lock().unwrap();
            (
                state.last_message.clone(),
                self.inner.is_streaming.load(Ordering::SeqCst),
            )
        };
        if let Some(msg) = last_message {
            self.update_content(msg, Some(is_streaming));
        }
    }

    pub fn set_hide_thinking_block(&self, hide: bool) {
        let (last_message, is_streaming) = {
            let mut state = self.inner.state.lock().unwrap();
            state.hide_thinking_block = hide;
            state.thinking_visibility_overrides.clear();
            (
                state.last_message.clone(),
                self.inner.is_streaming.load(Ordering::SeqCst),
            )
        };
        if let Some(msg) = last_message {
            self.update_content(msg, Some(is_streaming));
        }
    }

    pub fn set_hidden_thinking_label(&self, label: &str) {
        let (last_message, is_streaming) = {
            let mut state = self.inner.state.lock().unwrap();
            state.hidden_thinking_label = label.to_string();
            (
                state.last_message.clone(),
                self.inner.is_streaming.load(Ordering::SeqCst),
            )
        };
        if let Some(msg) = last_message {
            self.update_content(msg, Some(is_streaming));
        }
    }

    pub fn set_output_pad(&self, padding: i64) {
        let (last_message, is_streaming) = {
            let mut state = self.inner.state.lock().unwrap();
            state.output_pad = padding;
            state.markdown_slots.clear();
            (
                state.last_message.clone(),
                self.inner.is_streaming.load(Ordering::SeqCst),
            )
        };
        if let Some(msg) = last_message {
            self.update_content(msg, Some(is_streaming));
        }
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let mut lines = self.inner.container.render(width);
        let has_tool_calls = self.inner.state.lock().unwrap().has_tool_calls;
        if has_tool_calls || lines.is_empty() {
            return lines;
        }
        if let Some(first) = lines.first_mut() {
            *first = format!("{OSC133_ZONE_START}{first}");
        }
        if let Some(last) = lines.last_mut() {
            *last = format!("{OSC133_ZONE_END}{OSC133_ZONE_FINAL}{last}");
        }
        lines
    }

    pub fn update_content(&self, message: AssistantMessage, is_streaming: Option<bool>) {
        if let Some(streaming) = is_streaming {
            self.inner.is_streaming.store(streaming, Ordering::SeqCst);
        }

        let (
            hide_thinking_block,
            markdown_theme,
            hidden_thinking_label,
            output_pad,
            transformers,
            message_id,
            timestamp,
            overrides,
            prev_slots,
        ) = {
            let mut state = self.inner.state.lock().unwrap();
            state.last_message = Some(message.clone());
            let prev_slots = std::mem::take(&mut state.markdown_slots);
            (
                state.hide_thinking_block,
                state.markdown_theme.clone(),
                state.hidden_thinking_label.clone(),
                state.output_pad,
                state.markdown_transformers.clone(),
                state.message_id.clone(),
                state.timestamp.clone(),
                state.thinking_visibility_overrides.clone(),
                prev_slots,
            )
        };

        self.inner.content_container.clear();

        let mut new_slots: Vec<MarkdownSlot> = Vec::new();
        let streaming_flag = Arc::clone(&self.inner.is_streaming);

        let mut next_markdown_block =
            |kind: MarkdownSlotKind, text: &str, style: Option<DefaultTextStyle>, msg_type: MarkdownMessageType| {
                let slot_idx = new_slots.len();
                let markdown = if let Some(existing) = prev_slots.get(slot_idx)
                    && existing.kind == kind
                {
                    existing.markdown.set_text(text);
                    existing.markdown.clone()
                } else {
                    let streaming_c = Arc::clone(&streaming_flag);
                    let transformers_c = transformers.clone();
                    let msg_id_c = message_id.clone();
                    let ts_c = timestamp.clone();
                    Markdown::new(
                        text,
                        output_pad,
                        0,
                        markdown_theme.clone(),
                        style,
                        Some(MarkdownOptions {
                            preserve_ordered_list_markers: None,
                            preserve_backslash_escapes: None,
                            transform: Some(create_markdown_transform_dynamic_with_meta(
                                msg_type,
                                Arc::new(move || streaming_c.load(Ordering::SeqCst)),
                                transformers_c,
                                msg_id_c,
                                ts_c,
                            )),
                            render_latex: None,
                        }),
                    )
                };
                new_slots.push(MarkdownSlot {
                    kind,
                    markdown: markdown.clone(),
                });
                markdown
            };

        let mut needs_spacer = true;

        let t = theme();
        let mut thinking_run_index = 0usize;
        let mut i = 0usize;
        while i < message.content.len() {
            match &message.content[i] {
                AssistantContentBlock::Text(tc) if !tc.text.trim().is_empty() => {
                    let trimmed = tc.text.trim();
                    let md = next_markdown_block(MarkdownSlotKind::Text, trimmed, None, MarkdownMessageType::Assistant);
                    if needs_spacer {
                        self.inner.content_container.add_child(Arc::new(Spacer::new(Some(1))));
                        needs_spacer = false;
                    }
                    self.inner.content_container.add_child(Arc::new(md));
                }
                AssistantContentBlock::Thinking(_) => {
                    let mut thinking_blocks: Vec<String> = Vec::new();
                    while i < message.content.len() {
                        if let AssistantContentBlock::Thinking(th) = &message.content[i] {
                            let trimmed = th.thinking.trim();
                            if !trimmed.is_empty() {
                                thinking_blocks.push(trimmed.to_string());
                            }
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    // Step back 1 so the outer `i += 1` advances to the next non-thinking item cleanly.
                    i -= 1;

                    if thinking_blocks.is_empty() {
                        i += 1;
                        continue;
                    }

                    let run_index = thinking_run_index;
                    thinking_run_index += 1;
                    let hidden = overrides.get(&run_index).copied().unwrap_or(hide_thinking_block);
                    if hidden && hidden_thinking_label.trim().is_empty() {
                        i += 1;
                        continue;
                    }

                    let joined = thinking_blocks.join("\n\n");
                    let thinking_component: Arc<dyn Component> = if hidden {
                        Arc::new(Text::new(
                            Some(&t.italic(&t.fg(ThemeColor::ThinkingText, &hidden_thinking_label))),
                            Some(output_pad),
                            Some(0),
                            None,
                        ))
                    } else {
                        let md = next_markdown_block(
                            MarkdownSlotKind::Thinking,
                            &joined,
                            Some(DefaultTextStyle {
                                color: Some(Arc::new(|text: &str| theme().fg(ThemeColor::ThinkingText, text))),
                                italic: Some(true),
                                ..Default::default()
                            }),
                            MarkdownMessageType::AssistantThinking,
                        );
                        Arc::new(md)
                    };

                    let comp = self.clone();
                    if needs_spacer {
                        self.inner.content_container.add_child(Arc::new(Spacer::new(Some(1))));
                    }
                    self.inner.content_container.add_child(Arc::new(MouseRegion::new(
                        thinking_component,
                        Arc::new(move |event: &TuiMouseEvent| {
                            if event.r#type != TuiMouseEventType::Click || event.button != TuiMouseButton::Left {
                                return None;
                            }
                            let last = {
                                let mut state = comp.inner.state.lock().unwrap();
                                state.thinking_visibility_overrides.insert(run_index, !hidden);
                                state.last_message.clone()
                            };
                            let streaming = comp.inner.is_streaming.load(Ordering::SeqCst);
                            if let Some(msg) = last {
                                comp.update_content(msg, Some(streaming));
                            }
                            Some(TuiMouseEventResult {
                                handled: Some(true),
                                ..Default::default()
                            })
                        }),
                    )));
                    needs_spacer = true;
                }
                _ => {}
            }
            i += 1;
        }

        let has_tool_calls = message
            .content
            .iter()
            .any(|c| matches!(c, AssistantContentBlock::ToolCall(_)));

        {
            let mut state = self.inner.state.lock().unwrap();
            state.markdown_slots = new_slots;
            state.has_tool_calls = has_tool_calls;
        }

        if message.stop_reason == StopReason::Length {
            self.inner.content_container.add_child(Arc::new(Spacer::new(Some(1))));
            self.inner.content_container.add_child(Arc::new(Text::new(
                Some(&t.fg(ThemeColor::Error, "Response was truncated before completion.")),
                Some(output_pad),
                Some(0),
                None,
            )));
        } else if !has_tool_calls {
            if message.stop_reason == StopReason::Aborted {
                let abort_message = message
                    .error_message
                    .as_deref()
                    .filter(|s| !s.is_empty() && *s != "Request was aborted")
                    .unwrap_or("Operation aborted");
                self.inner.content_container.add_child(Arc::new(Spacer::new(Some(1))));
                self.inner.content_container.add_child(Arc::new(Text::new(
                    Some(&t.fg(ThemeColor::Error, abort_message)),
                    Some(output_pad),
                    Some(0),
                    None,
                )));
            } else if message.stop_reason == StopReason::Error {
                let error_msg = message
                    .error_message
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Unknown error");
                self.inner.content_container.add_child(Arc::new(Spacer::new(Some(1))));
                self.inner.content_container.add_child(Arc::new(Text::new(
                    Some(&t.fg(ThemeColor::Error, &format!("Error: {error_msg}"))),
                    Some(output_pad),
                    Some(0),
                    None,
                )));
            }
        }
    }
}

impl Component for AssistantMessageComponent {
    fn render(&self, width: usize) -> Vec<String> {
        AssistantMessageComponent::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        AssistantMessageComponent::invalidate(self)
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.container.children())
    }
}
