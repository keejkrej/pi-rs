//! Centered 720px scrollable conversation transcript and Waku empty state.

use rune::{Cursor, Cx, El, Icon, ScrollSpec, TextStyle, col, el, icon, row, text};
use rune_kit::{
    Badge, BadgeVariant, Button, ButtonVariant, ControlSize, Kit, Markdown, MarkdownStyle, Spinner, scroll_bar,
};

use crate::models::truncate_preview;
use crate::sidebar::format_thread_time;
use crate::state::{
    ContentBlockView, MessageRoleView, MessageViewModel, PiGuiAction, PiGuiState, ToastTone, ToolCallStatus,
    ToolCallViewModel,
};
use crate::theme::{LayoutMetrics, WakuPalette, WakuRadius};

static ICON_SPARKLES: Icon = Icon {
    name: "waku-sparkles",
    view: 20.0,
    body: r#"<path d="M10 2.2l1.6 4.6 4.6 1.6-4.6 1.6L10 14.6 8.4 10 3.8 8.4l4.6-1.6L10 2.2Z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" fill="none"/><path d="M15.8 12.6l.7 1.9 1.9.7-1.9.7-.7 1.9-.7-1.9-1.9-.7 1.9-.7.7-1.9Z" fill="currentColor"/>"#,
};

static ICON_CHEVRON_DOWN: Icon = Icon {
    name: "waku-chevron-down",
    view: 16.0,
    body: r#"<path d="M4.5 6.2L8 9.8l3.5-3.6" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_CHEVRON_RIGHT: Icon = Icon {
    name: "waku-chevron-right",
    view: 16.0,
    body: r#"<path d="M6.2 4.5L9.8 8l-3.6 3.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_CHECK_CIRCLE: Icon = Icon {
    name: "waku-tool-ok",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="5.5" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M5.5 8.2l1.8 1.8 3.3-3.8" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_ERROR_CIRCLE: Icon = Icon {
    name: "waku-tool-err",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="5.5" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M5.8 5.8l4.4 4.4M10.2 5.8l-4.4 4.4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};

static ICON_ALERT_TRIANGLE: Icon = Icon {
    name: "waku-tool-abort",
    view: 16.0,
    body: r#"<path d="M8 2.5L14 13H2L8 2.5Z" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linejoin="round"/><path d="M8 6.5v3M8 11.2v.4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};

fn strip_up_to_3_spaces(line: &str) -> Option<&str> {
    let spaces = line.bytes().take(4).take_while(|&b| b == b' ').count();
    (spaces <= 3).then(|| &line[spaces..])
}

fn parse_fence_open(line: &str) -> Option<(u8, usize)> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let rest = strip_up_to_3_spaces(trimmed)?;
    let first = *rest.as_bytes().first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let count = rest.bytes().take_while(|&b| b == first).count();
    if count < 3 {
        return None;
    }
    let info = &rest[count..];
    if first == b'`' && info.as_bytes().contains(&b'`') {
        return None;
    }
    Some((first, count))
}

fn is_fence_close(line: &str, fence_ch: u8, fence_len: usize) -> bool {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let Some(rest) = strip_up_to_3_spaces(trimmed) else {
        return false;
    };
    let count = rest.bytes().take_while(|&b| b == fence_ch).count();
    count >= fence_len && rest[count..].bytes().all(|b| b == b' ' || b == b'\t')
}

fn scan_inline_code_segment(src: &str, seg_start: usize, seg_end: usize, out: &mut Vec<std::ops::Range<usize>>) {
    let bytes = &src.as_bytes()[seg_start..seg_end];
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let mut backslashes = 0usize;
        let mut k = i;
        while k > 0 && bytes[k - 1] == b'\\' {
            backslashes += 1;
            k -= 1;
        }
        if backslashes % 2 == 1 {
            i += 1;
            continue;
        }
        let mut open_end = i;
        while open_end < bytes.len() && bytes[open_end] == b'`' {
            open_end += 1;
        }
        let n = open_end - i;
        let mut j = open_end;
        let mut closed: Option<(usize, usize)> = None;
        while j < bytes.len() {
            if bytes[j] == b'`' {
                let close_start = j;
                while j < bytes.len() && bytes[j] == b'`' {
                    j += 1;
                }
                if j - close_start == n {
                    closed = Some((close_start, j));
                    break;
                }
            } else {
                j += 1;
            }
        }
        if let Some((close_start, close_end)) = closed {
            if open_end < close_start {
                out.push((seg_start + open_end)..(seg_start + close_start));
            }
            i = close_end;
        } else {
            i = open_end;
        }
    }
}

fn inline_code_byte_ranges(src: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    let mut seg_start: Option<usize> = None;
    let mut offset = 0usize;

    for line in src.split_inclusive('\n') {
        let line_start = offset;
        let line_end = offset + line.len();
        offset = line_end;

        if let Some((fch, flen)) = fence {
            if is_fence_close(line, fch, flen) {
                fence = None;
            }
            continue;
        }

        if let Some(opened) = parse_fence_open(line) {
            if let Some(start) = seg_start.take() {
                scan_inline_code_segment(src, start, line_start, &mut out);
            }
            fence = Some(opened);
            continue;
        }

        let is_blank = line
            .trim_end_matches(['\r', '\n'])
            .bytes()
            .all(|b| b == b' ' || b == b'\t');
        if is_blank {
            if let Some(start) = seg_start.take() {
                scan_inline_code_segment(src, start, line_start, &mut out);
            }
        } else if seg_start.is_none() {
            seg_start = Some(line_start);
        }
    }

    if let Some(start) = seg_start {
        scan_inline_code_segment(src, start, src.len(), &mut out);
    }

    out
}

fn render_markdown_block(block_key: &str, body: &str, streaming: bool, k: &Kit, pal: &WakuPalette) -> El {
    let style = MarkdownStyle::from_kit(k)
        .text(pal.text)
        .muted(pal.text_secondary)
        .inline_code_bg(pal.code_wash)
        .code_block(pal.inset, pal.border)
        .diagram_max_width(LayoutMetrics::CONTENT_MAX_WIDTH - 48.0)
        .text_width(LayoutMetrics::CONTENT_MAX_WIDTH);
    let prefix = block_key.to_string();
    let dark = pal.is_dark;
    let copy_fill = pal.raised;
    let copy_border = pal.border;
    let copy_fg = pal.text_tertiary;
    let inline_code_ranges = inline_code_byte_ranges(body);

    let mut md = Markdown::new(body)
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
                .min_w(0.0)
                .max_w(LayoutMetrics::CONTENT_MAX_WIDTH)
                .radius(kit.radius.md)
                .border_layout()
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
                        .child(
                            text(&lang_label)
                                .font(TextStyle::new(11.5, 400.0).mono())
                                .color(copy_fg),
                        )
                        .child(
                            Button::new("Copy")
                                .control_size(ControlSize::Small)
                                .variant(ButtonVariant::Ghost)
                                .on_press_tx(move |st: &mut PiGuiState, tx| {
                                    tx.copy(&source);
                                    st.push_toast("Copied code snippet to clipboard", ToastTone::Info);
                                })
                                .build(kit),
                        ),
                )
                .child(card)
        });
    if !inline_code_ranges.is_empty() {
        md = md.highlights(vec![(
            inline_code_ranges,
            rune_kit::markdown::Highlight::new()
                .bg(rune::Color::TRANSPARENT)
                .fg(pal.code_text),
        )]);
    }
    md.build(k)
}

fn tool_status_view(status: ToolCallStatus, pal: &WakuPalette, k: &Kit) -> (El, &'static str, BadgeVariant) {
    match status {
        ToolCallStatus::Pending => (
            el().size(6.0, 6.0).radius(3.0).fill(pal.text_tertiary),
            "pending",
            BadgeVariant::Subtle,
        ),
        ToolCallStatus::Running => (
            Spinner::new().size(13.0).color(pal.accent).build(k),
            "running",
            BadgeVariant::Accent,
        ),
        ToolCallStatus::Success => (
            icon(&ICON_CHECK_CIRCLE, 13.0).color(pal.success),
            "done",
            BadgeVariant::Neutral,
        ),
        ToolCallStatus::Error => (
            icon(&ICON_ERROR_CIRCLE, 13.0).color(pal.danger),
            "error",
            BadgeVariant::Accent,
        ),
        ToolCallStatus::Aborted => (
            icon(&ICON_ALERT_TRIANGLE, 13.0).color(pal.warning),
            "aborted",
            BadgeVariant::Subtle,
        ),
    }
}

fn render_tool_call_card(tc: &ToolCallViewModel, k: &Kit, pal: &WakuPalette) -> El {
    let tool_id = tc.id.clone();
    let (status_icon, status_label, badge_var) = tool_status_view(tc.status, pal, k);
    let dur = tc.duration_ms.map(|ms| format!("{ms}ms"));

    let mut header = row()
        .w_full()
        .px(10.0)
        .py(7.0)
        .gap(8.0)
        .center_y()
        .cursor(Cursor::Pointer)
        .hover(pal.overlay)
        .on_press(move |st: &mut PiGuiState| st.dispatch(PiGuiAction::ToggleToolExpanded(tool_id.clone()), None))
        .child(status_icon)
        .child(text(&tc.name).font(TextStyle::new(12.5, 600.0).mono()).color(pal.text));

    if !tc.arguments_preview.is_empty() {
        header = header.child(
            text(truncate_preview(&tc.arguments_preview, 54))
                .font(TextStyle::new(12.0, 400.0).mono())
                .color(pal.text_tertiary)
                .min_w(0.0)
                .truncate()
                .grow(1.0),
        );
    } else {
        header = header.child(el().grow(1.0));
    }

    if let Some(d) = dur {
        header = header.child(
            text(d)
                .font(TextStyle::new(11.0, 400.0).mono())
                .color(pal.text_tertiary),
        );
    }
    header = header.child(Badge::new(status_label).variant(badge_var).build(k));

    let mut card = col()
        .key(format!("tool-card-{}", tc.id))
        .w_full()
        .min_w(0.0)
        .max_w(LayoutMetrics::CONTENT_MAX_WIDTH)
        .radius(k.radius.lg)
        .border_layout()
        .border(1.0, pal.border)
        .fill(pal.surface)
        .clip_rounded(k.radius.lg)
        .child(header);

    if tc.expanded {
        let mut body = col()
            .w_full()
            .min_w(0.0)
            .px(12.0)
            .py(10.0)
            .gap(6.0)
            .fill(pal.inset)
            .clip_rounded(0.0);
        if !tc.arguments_preview.is_empty() {
            body = body
                .child(
                    text("Arguments")
                        .font(TextStyle::new(11.0, 600.0))
                        .color(pal.text_tertiary),
                )
                .child(
                    text(truncate_preview(&tc.arguments_preview, 240))
                        .font(TextStyle::new(12.0, 400.0).mono())
                        .color(pal.text_secondary)
                        .min_w(0.0)
                        .shrink()
                        .wrap_anywhere(),
                );
        }
        if let Some(out) = &tc.output_preview
            && !out.trim().is_empty()
        {
            body = body
                .child(
                    text("Output")
                        .font(TextStyle::new(11.0, 600.0))
                        .color(pal.text_tertiary),
                )
                .child(
                    text(truncate_preview(out, 600))
                        .font(TextStyle::new(12.0, 400.0).mono())
                        .color(pal.text)
                        .min_w(0.0)
                        .shrink()
                        .wrap_anywhere(),
                );
        }
        card = card.child(body);
    }

    card
}

fn render_empty_state(s: &PiGuiState, pal: &WakuPalette) -> El {
    let heading = format!("What should we build in {}?", s.selected_project);
    col()
        .key("workspace.main.empty_state")
        .role("region")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .center()
        .gap(14.0)
        .pt(22.0)
        .px(32.0)
        .pb(52.0)
        .clip_rounded(0.0)
        .child(
            el().key("workspace.main.empty_state.sparkle")
                .role("img")
                .label_static("Empty State Sparkle")
                .size(20.0, 20.0)
                .center()
                .child(icon(&ICON_SPARKLES, 18.0).color(pal.accent)),
        )
        .child(
            row()
                .key("workspace.main.empty_state.heading")
                .role("heading")
                .label(&heading)
                .justify_center()
                .center_y()
                .child(
                    text(&heading)
                        .font(TextStyle::new(20.0, 500.0).lh(26.0))
                        .color(pal.text),
                ),
        )
}

fn render_user_bubble(msg: &MessageViewModel, idx: usize, pal: &WakuPalette) -> El {
    let mut inner = col()
        .min_w(0.0)
        .shrink()
        .max_w(LayoutMetrics::USER_BUBBLE_MAX_WIDTH - 24.0)
        .gap(4.0);
    for block in &msg.blocks {
        match block {
            ContentBlockView::Text(body) => {
                inner = inner.child(
                    text(body)
                        .font(TextStyle::new(14.0, 400.0).lh(20.0))
                        .color(pal.text)
                        .max_w(LayoutMetrics::USER_BUBBLE_MAX_WIDTH - 24.0)
                        .min_w(0.0)
                        .shrink()
                        .wrap_anywhere(),
                );
            }
            ContentBlockView::Image { mime_type, label } => {
                inner = inner.child(
                    text(format!("[image {mime_type}: {label}]"))
                        .font(TextStyle::new(12.0, 400.0))
                        .color(pal.text_secondary),
                );
            }
            _ => {}
        }
    }

    row().w_full().min_w(0.0).justify_end().child(
        col()
            .key(format!("workspace.main.transcript.user_bubble.{idx}"))
            .role("region")
            .min_w(0.0)
            .shrink()
            .max_w(LayoutMetrics::USER_BUBBLE_MAX_WIDTH)
            .py(7.0)
            .px(11.0)
            .radius(WakuRadius::LG)
            .fill(pal.raised)
            .border_layout()
            .border(1.0, pal.raised)
            .clip_rounded(WakuRadius::LG)
            .child(inner),
    )
}

fn render_worked_fold(msg_id: &str, idx: usize, summary: &str, expanded: bool, pal: &WakuPalette) -> El {
    let owned_id = msg_id.to_string();
    let chev = if expanded {
        &ICON_CHEVRON_DOWN
    } else {
        &ICON_CHEVRON_RIGHT
    };
    row()
        .key(format!("workspace.main.transcript.worked_fold.{idx}"))
        .role("button")
        .label(summary)
        .expanded(expanded)
        .w_full()
        .max_w(LayoutMetrics::CONTENT_MAX_WIDTH)
        .h(24.0)
        .gap(10.0)
        .center_y()
        .cursor(Cursor::Pointer)
        .hover(pal.overlay)
        .radius(WakuRadius::SM)
        .on_press(move |st: &mut PiGuiState| st.dispatch(PiGuiAction::ToggleWorkedFold(owned_id.clone()), None))
        .child(el().grow(1.0).h(1.0).fill(pal.border))
        .child(
            row()
                .gap(4.0)
                .center_y()
                .child(text(summary).font(TextStyle::new(12.5, 400.0)).color(pal.text_tertiary))
                .child(icon(chev, 12.0).color(pal.text_tertiary)),
        )
        .child(el().grow(1.0).h(1.0).fill(pal.border))
}

/// Renders Waku's centered 720px transcript column (`workspace.main.transcript`) or `workspace.main.empty_state`.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    if s.messages.is_empty() && s.active_stream.is_none() {
        return render_empty_state(s, pal);
    }

    let mut column = col()
        .w_full()
        .max_w(LayoutMetrics::CONTENT_MAX_WIDTH)
        .min_w(0.0)
        .gap(12.0);

    let mut user_turn_idx = 0usize;
    let mut assistant_turn_idx = 0usize;

    for msg in &s.messages {
        match msg.role {
            MessageRoleView::User => {
                column = column.child(render_user_bubble(msg, user_turn_idx, pal));
                user_turn_idx += 1;
            }
            MessageRoleView::Assistant => {
                let mut thinking_texts: Vec<&str> = Vec::new();
                let mut tools: Vec<&ToolCallViewModel> = Vec::new();
                let mut prose_blocks: Vec<(usize, &str)> = Vec::new();
                for (bidx, block) in msg.blocks.iter().enumerate() {
                    match block {
                        ContentBlockView::Thinking(th) if !th.trim().is_empty() => thinking_texts.push(th.as_str()),
                        ContentBlockView::ToolCall(tc) => tools.push(tc),
                        ContentBlockView::Text(body) if !body.trim().is_empty() => {
                            prose_blocks.push((bidx, body.as_str()))
                        }
                        _ => {}
                    }
                }

                let has_work = !thinking_texts.is_empty() || !tools.is_empty();
                let fold_expanded = s.worked_folds_expanded.get(&msg.id).copied().unwrap_or(false);
                if has_work {
                    let tool_suffix = match tools.len() {
                        0 => String::new(),
                        1 => " · 1 tool".to_string(),
                        n => format!(" · {n} tools"),
                    };
                    let fold_label = format!("Worked for 6s{tool_suffix}");
                    column = column.child(render_worked_fold(
                        &msg.id,
                        assistant_turn_idx,
                        &fold_label,
                        fold_expanded,
                        pal,
                    ));
                }

                if fold_expanded {
                    for (tidx, th) in thinking_texts.iter().enumerate() {
                        column = column.child(
                            col()
                                .key(format!("worked-thinking-{}-{tidx}", msg.id))
                                .w_full()
                                .padding(10.0)
                                .radius(k.radius.md)
                                .fill(pal.inset)
                                .border_layout()
                                .border(1.0, pal.border)
                                .child(
                                    text(*th)
                                        .font(TextStyle::new(12.5, 400.0).lh(18.0))
                                        .color(pal.text_secondary)
                                        .wrap(),
                                ),
                        );
                    }
                    for tc in &tools {
                        column = column.child(render_tool_call_card(tc, k, pal));
                    }
                } else {
                    for tc in &tools {
                        if tc.expanded {
                            column = column.child(render_tool_call_card(tc, k, pal));
                        }
                    }
                }

                let mut assistant_col = col()
                    .key(format!("workspace.main.transcript.assistant.{assistant_turn_idx}"))
                    .role("region")
                    .w_full()
                    .max_w(LayoutMetrics::CONTENT_MAX_WIDTH)
                    .min_w(0.0)
                    .gap(8.0)
                    .clip_rounded(0.0);

                for (bidx, body) in &prose_blocks {
                    let bkey = format!("msg-{}-b{bidx}", msg.id);
                    assistant_col = assistant_col.child(render_markdown_block(&bkey, body, false, k, pal));
                }

                if msg.is_error
                    && let Some(err) = &msg.error_message
                {
                    assistant_col = assistant_col.child(
                        row()
                            .w_full()
                            .padding(10.0)
                            .gap(8.0)
                            .center_y()
                            .radius(k.radius.md)
                            .fill(pal.danger_soft)
                            .border_layout()
                            .border(1.0, pal.danger)
                            .child(icon(&ICON_ERROR_CIRCLE, 14.0).color(pal.danger))
                            .child(
                                text(err)
                                    .font(TextStyle::new(12.5, 400.0))
                                    .color(pal.danger)
                                    .wrap()
                                    .grow(1.0),
                            ),
                    );
                }

                if !prose_blocks.is_empty() || msg.is_error {
                    column = column.child(assistant_col);
                }
                assistant_turn_idx += 1;
            }
            MessageRoleView::System | MessageRoleView::ToolResult => {
                let ts = format_thread_time(msg.timestamp_ms);
                let mut note = col()
                    .key(format!("system-msg-{}", msg.id))
                    .w_full()
                    .py(6.0)
                    .px(10.0)
                    .gap(4.0)
                    .radius(k.radius.md)
                    .border_layout()
                    .border(1.0, pal.border);
                if !ts.is_empty() {
                    note = note.child(
                        text(format!("System · {ts}"))
                            .font(TextStyle::new(11.5, 400.0))
                            .color(pal.text_tertiary),
                    );
                }
                for block in &msg.blocks {
                    if let ContentBlockView::Text(body) = block {
                        note = note.child(
                            text(body)
                                .font(TextStyle::new(12.5, 400.0))
                                .color(pal.text_secondary)
                                .wrap(),
                        );
                    }
                }
                column = column.child(note);
            }
        }
    }

    if let Some(stream) = &s.active_stream {
        let mut live = col().key("active-stream").w_full().min_w(0.0).gap(8.0);
        if let Some(thinking) = &stream.partial_thinking
            && !thinking.trim().is_empty()
        {
            live = live.child(render_worked_fold(
                "live-stream",
                assistant_turn_idx,
                "Working…",
                false,
                pal,
            ));
        }
        if !stream.partial_text.trim().is_empty() {
            live = live.child(
                col()
                    .key(format!("workspace.main.transcript.assistant.{assistant_turn_idx}"))
                    .role("region")
                    .w_full()
                    .max_w(LayoutMetrics::CONTENT_MAX_WIDTH)
                    .min_w(0.0)
                    .gap(8.0)
                    .child(render_markdown_block(
                        "live-stream-md",
                        &stream.partial_text,
                        true,
                        k,
                        pal,
                    )),
            );
        }
        column = column.child(live);
    }

    let scroll_content = col()
        .w_full()
        .min_w(0.0)
        .items_center()
        .pt(16.0)
        .pb(16.0)
        .px(20.0)
        .scroll(ScrollSpec { stick_bottom: false })
        .grow(1.0)
        .min_h(0.0)
        .child(column);

    scroll_bar(
        k,
        "workspace.main.transcript",
        "workspace.main.transcript.scroll",
        scroll_content,
    )
    .role("region")
    .grow(1.0)
    .min_h(0.0)
    .w_full()
    .clip_rounded(0.0)
}

#[cfg(test)]
mod tests {
    use super::inline_code_byte_ranges;

    #[test]
    fn inline_code_byte_ranges_handles_single_and_double_backticks() {
        let src = "Use `AgentSessionRuntime` and ``nested `tick` span`` here.";
        let ranges = inline_code_byte_ranges(src);
        let slices: Vec<&str> = ranges.iter().map(|r| &src[r.clone()]).collect();
        assert_eq!(slices, vec!["AgentSessionRuntime", "nested `tick` span"]);
    }

    #[test]
    fn inline_code_byte_ranges_skips_fenced_code_blocks() {
        let src = concat!(
            "Before `one`.\n",
            "```rust\n",
            "let s = \"`not_inline`\";\n",
            "```\n",
            "   ~~~\n",
            "also `not_inline` and ```still_in_tilde```\n",
            "   ~~~\n",
            "After `two`.\n",
        );
        let ranges = inline_code_byte_ranges(src);
        let slices: Vec<&str> = ranges.iter().map(|r| &src[r.clone()]).collect();
        assert_eq!(slices, vec!["one", "two"]);
    }

    #[test]
    fn inline_code_byte_ranges_ignores_unclosed_and_resets_on_blank_lines() {
        let src = "Unclosed `foo\n\nSecond paragraph `bar` and `broken ``ok``.";
        let ranges = inline_code_byte_ranges(src);
        let slices: Vec<&str> = ranges.iter().map(|r| &src[r.clone()]).collect();
        assert_eq!(slices, vec!["bar", "ok"]);
    }
}
