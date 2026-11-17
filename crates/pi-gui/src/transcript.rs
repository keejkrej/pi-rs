//! Scrollable conversation transcript view with markdown, tool cards, and streaming tail.

use rune::{Cx, El, ScrollSpec, col, el, row, text};
use rune_kit::Kit;

use crate::state::{ContentBlockView, MessageRoleView, PiGuiAction, PiGuiState};
use crate::theme::{LayoutMetrics, WakuPalette};

/// Renders the scrollable session transcript column.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let mut stack = col()
        .key("transcript-content")
        .w_full()
        .max_w(LayoutMetrics::CONTENT_MAX_WIDTH)
        .gap(12.0)
        .py(16.0)
        .px(20.0);

    if s.messages.is_empty() && s.active_stream.is_none() {
        stack = stack.child(
            col()
                .key("transcript-empty")
                .py(32.0)
                .gap(6.0)
                .child(text("Start a conversation").font(k.ty.body_medium).color(pal.text))
                .child(
                    text("Ask pi to inspect, edit, or run checks in the current workspace.")
                        .font(k.ty.ui)
                        .color(pal.text_secondary),
                ),
        );
    }

    for msg in &s.messages {
        let is_user = msg.role == MessageRoleView::User;
        let role_label = match msg.role {
            MessageRoleView::User => "You",
            MessageRoleView::Assistant => msg.model_label.as_deref().unwrap_or("Assistant"),
            MessageRoleView::System => "System",
            MessageRoleView::ToolResult => "Tool",
        };

        let mut card = col()
            .key(format!("msg-{}", msg.id))
            .w_full()
            .gap(6.0)
            .px(if is_user { 14.0 } else { 0.0 })
            .py(if is_user { 10.0 } else { 4.0 })
            .radius(k.radius.md)
            .fill(if is_user {
                pal.composer
            } else {
                rune::Color::TRANSPARENT
            })
            .child(text(role_label).font(k.ty.small_medium).color(pal.text_tertiary));

        for (idx, block) in msg.blocks.iter().enumerate() {
            match block {
                ContentBlockView::Text(body) => {
                    let has_mermaid = body
                        .split("```mermaid")
                        .nth(1)
                        .and_then(|rest| rest.split("```").next())
                        .is_some_and(rune_mermaid::supported);
                    card = card.child(
                        text(body)
                            .key(format!("msg-{}-text-{idx}", msg.id))
                            .font(k.ty.body)
                            .color(pal.text),
                    );
                    if has_mermaid {
                        card = card.child(text("[Mermaid diagram]").font(k.ty.small).color(pal.accent));
                    }
                }
                ContentBlockView::Thinking(thought) => {
                    card = card.child(
                        el().key(format!("msg-{}-think-{idx}", msg.id))
                            .px(10.0)
                            .py(6.0)
                            .radius(k.radius.sm)
                            .fill(pal.inset)
                            .child(text(thought).font(k.ty.text_small).color(pal.text_secondary)),
                    );
                }
                ContentBlockView::ToolCall(tc) => {
                    let tool_id = tc.id.clone();
                    card = card.child(
                        col()
                            .key(format!("tool-{}", tc.id))
                            .px(10.0)
                            .py(6.0)
                            .radius(k.radius.sm)
                            .fill(pal.raised)
                            .hover(pal.overlay)
                            .on_press(move |st: &mut PiGuiState| {
                                st.dispatch(PiGuiAction::ToggleToolExpanded(tool_id.clone()), None);
                            })
                            .child(
                                row()
                                    .justify_between()
                                    .child(text(&tc.name).font(k.ty.ui_medium).color(pal.text))
                                    .child(text(&tc.arguments_preview).font(k.ty.small).color(pal.text_tertiary)),
                            ),
                    );
                }
                ContentBlockView::Image { label, .. } => {
                    card = card.child(
                        text(format!("[Image: {label}]"))
                            .font(k.ty.small)
                            .color(pal.text_secondary),
                    );
                }
            }
        }
        stack = stack.child(card);
    }

    if let Some(stream) = &s.active_stream {
        let mut live = col()
            .key("transcript-streaming")
            .w_full()
            .gap(6.0)
            .child(text("Streaming").font(k.ty.small_medium).color(pal.accent));
        if let Some(thinking) = &stream.partial_thinking {
            live = live.child(text(thinking).font(k.ty.text_small).color(pal.text_secondary));
        }
        live = live.child(text(&stream.partial_text).font(k.ty.body).color(pal.text));
        stack = stack.child(live);
    }

    col()
        .key("transcript-scroll-host")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .scroll(ScrollSpec { stick_bottom: true })
        .align(rune::Align::Center)
        .child(stack)
}
