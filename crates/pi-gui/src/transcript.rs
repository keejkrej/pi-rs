//! Centered scrollable conversation transcript with Markdown, Mermaid, syntax code frames, and tool cards.

use rune::{Cursor, Cx, El, Icon, ScrollSpec, col, el, icon, row, text};
use rune_kit::{
    Badge, BadgeVariant, Button, ButtonVariant, ControlSize, Kit, Markdown, MarkdownStyle, ScrollBar, ScrollBarStyle,
    Spinner, glyphs,
};

use crate::sidebar::format_thread_time;
use crate::state::{
    ContentBlockView, MessageRoleView, MessageViewModel, PiGuiAction, PiGuiState, ToastTone, ToolCallStatus,
    ToolCallViewModel,
};
use crate::theme::{LayoutMetrics, WakuPalette};

static ICON_CHECK_CIRCLE: Icon = Icon {
    name: "pi-tool-ok",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="5.5" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M5.5 8.2l1.8 1.8 3.3-3.8" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_ERROR_CIRCLE: Icon = Icon {
    name: "pi-tool-err",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="5.5" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M5.8 5.8l4.4 4.4M10.2 5.8l-4.4 4.4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};

static ICON_ALERT_TRIANGLE: Icon = Icon {
    name: "pi-tool-abort",
    view: 16.0,
    body: r#"<path d="M8 2.5L14 13H2L8 2.5Z" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linejoin="round"/><path d="M8 6.5v3M8 11.2v.4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};

static STARTER_PROMPTS: &[(&str, &str)] = &[
    (
        "Inspect workspace",
        "Inspect the workspace structure and summarize active changes.",
    ),
    (
        "Review git diff",
        "Review the current git diff and highlight any subtle bugs.",
    ),
    (
        "Run workspace tests",
        "Run the crate test suite and investigate any failures.",
    ),
];

fn render_markdown_block(block_key: &str, body: &str, streaming: bool, k: &Kit, pal: &WakuPalette) -> El {
    let style = MarkdownStyle::from_kit(k).diagram_max_width(LayoutMetrics::CONTENT_MAX_WIDTH - 48.0);
    let prefix = block_key.to_string();
    let dark = pal.is_dark;
    let copy_fill = pal.raised;
    let copy_border = pal.border;
    let copy_fg = pal.text_tertiary;

    Markdown::new(body)
        .style(style)
        .streaming(streaming)
        .selectable(true)
        .scroll_wide(true)
        .code_frame(move |card, frame| {
            let kit = crate::theme::waku_kit(dark);
            let lang_label = frame.language.unwrap_or("code").to_string();
            let source = frame.source.to_string();
            let frame_key = format!("{prefix}-code-{}", frame.index);
            col()
                .key(frame_key)
                .w_full()
                .radius(kit.radius.md)
                .border(1.0, copy_border)
                .clip_rounded(kit.radius.md)
                .child(
                    row()
                        .w_full()
                        .px(10.0)
                        .py(4.0)
                        .justify_between()
                        .center_y()
                        .fill(copy_fill)
                        .border_sides([0.0, 0.0, 1.0, 0.0], copy_border)
                        .child(text(lang_label).font(kit.ty.small_medium).color(copy_fg))
                        .child(
                            Button::new("Copy")
                                .icon(&glyphs::COPY)
                                .variant(ButtonVariant::GhostMuted)
                                .control_size(ControlSize::Mini)
                                .on_press_tx(move |st: &mut PiGuiState, tx| {
                                    tx.copy(source.clone());
                                    st.push_toast("Copied code block", ToastTone::Success);
                                })
                                .build(kit),
                        ),
                )
                .child(card)
        })
        .diagram_frame(move |card, frame| {
            let kit = crate::theme::waku_kit(dark);
            let source = frame.source.to_string();
            col()
                .key(format!("mermaid-frame-{}", frame.index))
                .w_full()
                .gap(4.0)
                .child(card)
                .child(
                    row().w_full().justify_end().child(
                        Button::new("Copy Mermaid")
                            .icon(&glyphs::COPY)
                            .variant(ButtonVariant::GhostMuted)
                            .control_size(ControlSize::Mini)
                            .on_press_tx(move |st: &mut PiGuiState, tx| {
                                tx.copy(source.clone());
                                st.push_toast("Copied Mermaid source", ToastTone::Success);
                            })
                            .build(kit),
                    ),
                )
        })
        .build(k)
        .key(block_key.to_string())
}

fn render_thinking_block(msg_id: &str, idx: usize, thought: &str, s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let flag_key = format!("ui.think.{msg_id}.{idx}");
    let expanded = s.settings_flags.get(&flag_key).copied().unwrap_or(false);
    let toggle_key = flag_key.clone();
    let lines_count = thought.lines().count().max(1);
    let chevron = if expanded {
        &glyphs::TRIANGLE_DOWN
    } else {
        &glyphs::TRIANGLE_RIGHT
    };

    let header = row()
        .key(format!("think-hdr-{msg_id}-{idx}"))
        .w_full()
        .gap(6.0)
        .center_y()
        .px(10.0)
        .py(6.0)
        .cursor(Cursor::Pointer)
        .hover(pal.overlay)
        .on_press(move |st: &mut PiGuiState| {
            let cur = st.settings_flags.get(&toggle_key).copied().unwrap_or(false);
            st.settings_flags.insert(toggle_key.clone(), !cur);
        })
        .child(icon(chevron, 12.0).color(pal.text_tertiary))
        .child(text("Thinking").font(k.ty.small_medium).color(pal.text_secondary))
        .child(
            text(format!("{lines_count} lines"))
                .font(k.ty.small)
                .color(pal.text_tertiary),
        );

    let mut card = col()
        .key(format!("msg-{msg_id}-think-{idx}"))
        .w_full()
        .radius(k.radius.sm)
        .fill(pal.inset)
        .border_sides([0.0, 0.0, 0.0, 2.0], pal.border_strong)
        .child(header);

    if expanded {
        card = card.child(
            col().w_full().px(12.0).pb(8.0).child(
                text(thought)
                    .font(k.ty.text_small)
                    .color(pal.text_secondary)
                    .wrap()
                    .selectable(true),
            ),
        );
    }
    card
}

fn render_tool_card(tc: &ToolCallViewModel, k: &Kit, pal: &WakuPalette) -> El {
    let tool_id = tc.id.clone();
    let status_el = match tc.status {
        ToolCallStatus::Pending | ToolCallStatus::Running => Spinner::new().size(13.0).color(pal.accent).build(k),
        ToolCallStatus::Success => icon(&ICON_CHECK_CIRCLE, 13.0).color(pal.success),
        ToolCallStatus::Error => icon(&ICON_ERROR_CIRCLE, 13.0).color(pal.danger),
        ToolCallStatus::Aborted => icon(&ICON_ALERT_TRIANGLE, 13.0).color(pal.warning),
    };
    let chevron = if tc.expanded {
        &glyphs::CHEVRON_UP
    } else {
        &glyphs::CHEVRON_DOWN
    };

    let mut hdr_right = row().gap(6.0).center_y();
    if let Some(ms) = tc.duration_ms {
        hdr_right = hdr_right.child(text(format!("{ms}ms")).font(k.ty.small).color(pal.text_tertiary));
    }
    hdr_right = hdr_right.child(icon(chevron, 12.0).color(pal.text_tertiary));

    let header = row()
        .key(format!("tool-hdr-{}", tc.id))
        .w_full()
        .justify_between()
        .center_y()
        .gap(8.0)
        .px(10.0)
        .py(7.0)
        .cursor(Cursor::Pointer)
        .hover(pal.overlay)
        .on_press(move |st: &mut PiGuiState| {
            st.dispatch(PiGuiAction::ToggleToolExpanded(tool_id.clone()), None);
        })
        .child(
            row()
                .gap(7.0)
                .center_y()
                .min_w(0.0)
                .grow(1.0)
                .child(status_el)
                .child(text(&tc.name).font(k.ty.ui_medium).color(pal.text))
                .child(
                    text(&tc.arguments_preview)
                        .font(k.ty.small)
                        .color(pal.text_tertiary)
                        .truncate()
                        .shrink(),
                ),
        )
        .child(hdr_right);

    let mut card = col()
        .key(format!("tool-{}", tc.id))
        .w_full()
        .radius(k.radius.md)
        .fill(pal.raised)
        .border(1.0, pal.border)
        .child(header);

    if tc.expanded {
        let mut body = col()
            .w_full()
            .px(10.0)
            .pb(8.0)
            .gap(6.0)
            .border_sides([1.0, 0.0, 0.0, 0.0], pal.border);
        if !tc.arguments_preview.is_empty() {
            body = body.child(
                col().w_full().padding(6.0).radius(k.radius.xs).fill(pal.inset).child(
                    text(&tc.arguments_preview)
                        .font(k.ty.mono)
                        .color(pal.text_secondary)
                        .wrap()
                        .selectable(true),
                ),
            );
        }
        if let Some(output) = tc.output_preview.as_deref().filter(|o| !o.is_empty()) {
            let output_copy = output.to_string();
            body = body.child(
                col()
                    .w_full()
                    .gap(4.0)
                    .child(
                        row()
                            .w_full()
                            .justify_between()
                            .center_y()
                            .child(text("Output").font(k.ty.small_medium).color(pal.text_tertiary))
                            .child(
                                Button::new("Copy")
                                    .icon(&glyphs::COPY)
                                    .variant(ButtonVariant::GhostMuted)
                                    .control_size(ControlSize::Mini)
                                    .on_press_tx(move |st: &mut PiGuiState, tx| {
                                        tx.copy(output_copy.clone());
                                        st.push_toast("Copied tool output", ToastTone::Success);
                                    })
                                    .build(k),
                            ),
                    )
                    .child(
                        col()
                            .w_full()
                            .max_h(220.0)
                            .scroll(ScrollSpec { stick_bottom: false })
                            .padding(8.0)
                            .radius(k.radius.xs)
                            .fill(pal.inset)
                            .child(text(output).font(k.ty.mono).color(pal.text).wrap().selectable(true)),
                    ),
            );
        }
        card = card.child(body);
    }
    card
}

fn render_message(msg: &MessageViewModel, s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let is_user = msg.role == MessageRoleView::User;
    let role_label = match msg.role {
        MessageRoleView::User => "You",
        MessageRoleView::Assistant => msg.model_label.as_deref().unwrap_or("Assistant"),
        MessageRoleView::System => "System",
        MessageRoleView::ToolResult => "Tool",
    };
    let ts = format_thread_time(msg.timestamp_ms);

    let mut header = row().w_full().justify_between().center_y().child(
        row()
            .gap(6.0)
            .center_y()
            .child(text(role_label).font(k.ty.small_medium).color(if is_user {
                pal.text_secondary
            } else {
                pal.text_tertiary
            }))
            .children(
                msg.is_error
                    .then(|| Badge::new("Error").variant(BadgeVariant::Subtle).build(k)),
            ),
    );
    if !ts.is_empty() {
        header = header.child(text(ts).font(k.ty.small).color(pal.text_ghost));
    }

    let mut card = col()
        .key(format!("msg-{}", msg.id))
        .w_full()
        .gap(8.0)
        .px(if is_user { 14.0 } else { 2.0 })
        .py(if is_user { 10.0 } else { 4.0 })
        .radius(k.radius.lg)
        .fill(if is_user {
            pal.composer
        } else {
            rune::Color::TRANSPARENT
        });
    if is_user {
        card = card.border(1.0, pal.border);
    }
    card = card.child(header);

    for (idx, block) in msg.blocks.iter().enumerate() {
        match block {
            ContentBlockView::Text(body) => {
                let md = render_markdown_block(&format!("msg-{}-text-{idx}", msg.id), body, false, k, pal);
                card = card.child(md);
            }
            ContentBlockView::Thinking(thought) => {
                card = card.child(render_thinking_block(&msg.id, idx, thought, s, k, pal));
            }
            ContentBlockView::ToolCall(tc) => {
                card = card.child(render_tool_card(tc, k, pal));
            }
            ContentBlockView::Image { mime_type, label } => {
                card = card.child(
                    row()
                        .key(format!("msg-{}-img-{idx}", msg.id))
                        .px(10.0)
                        .py(6.0)
                        .gap(8.0)
                        .center_y()
                        .radius(k.radius.sm)
                        .fill(pal.raised)
                        .border(1.0, pal.border)
                        .child(icon(&glyphs::FILE_IMAGE, 14.0).color(pal.text_secondary))
                        .child(text(label).font(k.ty.small_medium).color(pal.text))
                        .child(text(mime_type).font(k.ty.small).color(pal.text_tertiary)),
                );
            }
        }
    }

    if let Some(err) = msg.error_message.as_deref().filter(|e| !e.is_empty()) {
        card = card.child(
            el().w_full()
                .px(10.0)
                .py(8.0)
                .radius(k.radius.sm)
                .fill(pal.danger_soft)
                .border(1.0, pal.danger)
                .child(text(err).font(k.ty.small).color(pal.danger).wrap().selectable(true)),
        );
    }

    card
}

fn render_empty_state(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let mut chips = row().key("empty-starter-chips").gap(8.0).flex_wrap();
    for (title, prompt) in STARTER_PROMPTS {
        let prompt_str = (*prompt).to_string();
        chips = chips.child(
            Button::new(*title)
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .on_press_tx(move |st: &mut PiGuiState, tx| {
                    st.composer_edit.replace_text(&prompt_str);
                    st.composer_edit.focused = true;
                    tx.settle();
                })
                .build(k),
        );
    }

    col()
        .key("transcript-empty")
        .w_full()
        .py(48.0)
        .px(16.0)
        .gap(12.0)
        .radius(k.radius.lg)
        .fill(pal.raised)
        .border(1.0, pal.border)
        .child(text("Start a conversation").font(k.ty.body_medium).color(pal.text))
        .child(
            text(format!(
                "Workspace: {} • Ask pi to inspect, edit, or run checks.",
                s.config.cwd
            ))
            .font(k.ty.ui)
            .color(pal.text_secondary)
            .wrap(),
        )
        .child(chips)
}

/// Renders the centered scrollable session transcript column.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let mut stack = col()
        .key("transcript-content")
        .w_full()
        .max_w(LayoutMetrics::CONTENT_MAX_WIDTH)
        .gap(16.0)
        .py(20.0)
        .px(20.0);

    if s.messages.is_empty() && s.active_stream.is_none() {
        stack = stack.child(render_empty_state(s, k, pal));
    }

    for msg in &s.messages {
        stack = stack.child(render_message(msg, s, k, pal));
    }

    if let Some(stream) = &s.active_stream {
        let mut live = col()
            .key("transcript-streaming")
            .w_full()
            .gap(8.0)
            .px(2.0)
            .py(4.0)
            .child(
                row()
                    .gap(6.0)
                    .center_y()
                    .child(Spinner::new().size(13.0).color(pal.accent).build(k))
                    .child(text("Streaming").font(k.ty.small_medium).color(pal.accent)),
            );
        if let Some(thinking) = &stream.partial_thinking {
            live = live.child(
                el().key("stream-partial-thinking")
                    .px(10.0)
                    .py(6.0)
                    .radius(k.radius.sm)
                    .fill(pal.inset)
                    .border_sides([0.0, 0.0, 0.0, 2.0], pal.border_strong)
                    .child(text(thinking).font(k.ty.text_small).color(pal.text_secondary).wrap()),
            );
        }
        if !stream.partial_text.is_empty() {
            live = live.child(render_markdown_block(
                "stream-partial-md",
                &stream.partial_text,
                true,
                k,
                pal,
            ));
        }
        stack = stack.child(live);
    }

    if !s.steering_queue.is_empty() {
        let mut queue_col = col().key("steering-queue").w_full().gap(6.0);
        for item in &s.steering_queue {
            queue_col = queue_col.child(
                row()
                    .key(format!("steer-{}", item.id))
                    .w_full()
                    .px(10.0)
                    .py(6.0)
                    .gap(8.0)
                    .center_y()
                    .radius(k.radius.sm)
                    .fill(pal.raised)
                    .border(1.0, pal.border)
                    .child(
                        Badge::new(if item.is_follow_up { "Follow-up" } else { "Steering" })
                            .variant(BadgeVariant::Subtle)
                            .build(k),
                    )
                    .child(text(&item.text).font(k.ty.small).color(pal.text_secondary).truncate()),
            );
        }
        stack = stack.child(queue_col);
    }

    let viewport = col()
        .key("transcript-scroll-host")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .scroll(ScrollSpec { stick_bottom: true })
        .items_center()
        .child(stack);

    viewport
        .scroll_bar_styled(ScrollBarStyle::scroll_view(k))
        .key("transcript-scroll-area")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
}
