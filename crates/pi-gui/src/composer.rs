//! Bottom prompt composer card, `@file`/`/command` autocomplete, and 26pt Waku status footer.

use rune::{Cursor, Cx, El, Icon, Key, col, el, row, text, text_input};
use rune_kit::{Badge, BadgeVariant, Button, ButtonVariant, ControlSize, Kit};

use crate::state::{InspectorTab, OverlayKind, PiGuiAction, PiGuiState};
use crate::theme::{LayoutMetrics, ThemePreference, WakuPalette};

static ICON_SEND: Icon = Icon {
    name: "pi-send",
    view: 16.0,
    body: r#"<path d="M8 13V3M3.5 7.5L8 3l4.5 4.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_STOP: Icon = Icon {
    name: "pi-stop",
    view: 16.0,
    body: r#"<rect x="4.5" y="4.5" width="7" height="7" rx="1.2" fill="currentColor"/>"#,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum AutocompleteKind {
    File,
    SlashCommand,
}

struct AutocompleteItem {
    label: String,
    detail: String,
    replacement: String,
}

struct ActiveAutocomplete {
    kind: AutocompleteKind,
    token_start: usize,
    caret: usize,
    items: Vec<AutocompleteItem>,
}

const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/new", "Start a fresh session thread"),
    ("/compact", "Summarize and compact older context"),
    ("/model", "Switch active provider and model"),
    ("/theme", "Toggle dark / light appearance"),
    ("/settings", "Open workspace and plugin settings"),
    ("/plugins", "Inspect loaded QuickJS and MCP plugins"),
    ("/diff", "Open workspace git diff panel"),
    ("/clear", "Clear composer draft"),
];

fn active_autocomplete(s: &PiGuiState) -> Option<ActiveAutocomplete> {
    let text = &s.composer_edit.text;
    let raw_end = s.composer_edit.range().1.min(text.len());
    let caret = if text.is_char_boundary(raw_end) {
        raw_end
    } else {
        text.len()
    };
    let before = &text[..caret];
    let token_start = before
        .rfind(char::is_whitespace)
        .map_or(0, |idx| idx + before[idx..].chars().next().map_or(1, char::len_utf8));
    let token = &before[token_start..];

    if let Some(query) = token.strip_prefix('@') {
        let q = query.to_ascii_lowercase();
        let items: Vec<AutocompleteItem> = s
            .files
            .iter()
            .filter(|f| !f.is_dir && (q.is_empty() || f.path.to_ascii_lowercase().contains(&q)))
            .take(8)
            .map(|f| AutocompleteItem {
                label: f.path.clone(),
                detail: if f.modified {
                    "modified".to_string()
                } else {
                    "file".to_string()
                },
                replacement: format!("@{} ", f.path),
            })
            .collect();
        return Some(ActiveAutocomplete {
            kind: AutocompleteKind::File,
            token_start,
            caret,
            items,
        });
    }

    if token_start == 0
        && let Some(query) = token.strip_prefix('/')
    {
        let q = query.to_ascii_lowercase();
        let items: Vec<AutocompleteItem> = SLASH_COMMANDS
            .iter()
            .filter(|(cmd, _)| q.is_empty() || cmd[1..].contains(&q))
            .map(|(cmd, desc)| AutocompleteItem {
                label: (*cmd).to_string(),
                detail: (*desc).to_string(),
                replacement: format!("{cmd} "),
            })
            .collect();
        if !items.is_empty() {
            return Some(ActiveAutocomplete {
                kind: AutocompleteKind::SlashCommand,
                token_start,
                caret,
                items,
            });
        }
    }
    None
}

fn apply_autocomplete_choice(
    st: &mut PiGuiState,
    kind: AutocompleteKind,
    token_start: usize,
    caret: usize,
    replacement: &str,
) {
    if kind == AutocompleteKind::SlashCommand {
        let cmd = replacement.trim();
        match cmd {
            "/new" => {
                st.composer_edit.clear();
                st.dispatch(PiGuiAction::NewThread, None);
                return;
            }
            "/compact" => {
                st.composer_edit.clear();
                st.dispatch(PiGuiAction::CompactSession, None);
                return;
            }
            "/model" => {
                st.composer_edit.clear();
                st.dispatch(PiGuiAction::OpenOverlay(OverlayKind::ModelPicker), None);
                return;
            }
            "/settings" => {
                st.composer_edit.clear();
                st.dispatch(PiGuiAction::OpenOverlay(OverlayKind::Settings), None);
                return;
            }
            "/plugins" => {
                st.composer_edit.clear();
                st.dispatch(PiGuiAction::SelectInspectorTab(InspectorTab::Plugins), None);
                return;
            }
            "/diff" => {
                st.composer_edit.clear();
                st.dispatch(PiGuiAction::SelectInspectorTab(InspectorTab::Diff), None);
                return;
            }
            "/theme" => {
                st.composer_edit.clear();
                let next = if matches!(st.theme_preference, ThemePreference::Light) {
                    ThemePreference::Dark
                } else {
                    ThemePreference::Light
                };
                st.dispatch(PiGuiAction::SetThemePreference(next), None);
                return;
            }
            "/clear" => {
                st.composer_edit.clear();
                return;
            }
            _ => {}
        }
    }

    let current = &st.composer_edit.text;
    let start = token_start.min(current.len());
    let end = caret.min(current.len());
    if current.is_char_boundary(start) && current.is_char_boundary(end) && start <= end {
        let mut next = String::with_capacity(current.len() + replacement.len());
        next.push_str(&current[..start]);
        next.push_str(replacement);
        next.push_str(&current[end..]);
        let next_caret = start + replacement.len();
        st.composer_edit.replace_text(&next);
        st.composer_edit.set_caret(next_caret);
        st.composer_edit.focused = true;
    }
}

fn render_autocomplete_popover(menu: &ActiveAutocomplete, k: &Kit, pal: &WakuPalette) -> El {
    let heading = match menu.kind {
        AutocompleteKind::File => "Files (@)",
        AutocompleteKind::SlashCommand => "Commands (/)",
    };

    let mut list = col()
        .key("composer-autocomplete")
        .w_full()
        .max_w(LayoutMetrics::CONTENT_MAX_WIDTH)
        .padding(6.0)
        .gap(2.0)
        .radius(k.radius.md)
        .fill(pal.raised)
        .border(1.0, pal.border_strong)
        .child(
            row()
                .w_full()
                .px(8.0)
                .py(4.0)
                .justify_between()
                .child(text(heading).font(k.ty.small_medium).color(pal.text_tertiary))
                .child(text("Tab to complete").font(k.ty.small).color(pal.text_ghost)),
        );

    if menu.items.is_empty() {
        list = list.child(
            text("No matching workspace files")
                .font(k.ty.small)
                .color(pal.text_tertiary)
                .px(8.0)
                .py(6.0),
        );
    } else {
        for (idx, item) in menu.items.iter().enumerate() {
            let kind = menu.kind;
            let token_start = menu.token_start;
            let caret = menu.caret;
            let replacement = item.replacement.clone();
            list = list.child(
                row()
                    .key(format!("ac-item-{idx}"))
                    .w_full()
                    .px(8.0)
                    .py(5.0)
                    .gap(8.0)
                    .justify_between()
                    .center_y()
                    .radius(k.radius.sm)
                    .fill(if idx == 0 {
                        pal.overlay
                    } else {
                        rune::Color::TRANSPARENT
                    })
                    .cursor(Cursor::Pointer)
                    .hover(pal.overlay_strong)
                    .preserve_focus_on_press(true)
                    .on_press(move |st: &mut PiGuiState| {
                        apply_autocomplete_choice(st, kind, token_start, caret, &replacement);
                    })
                    .child(text(&item.label).font(k.ty.ui).color(pal.text).truncate().shrink())
                    .child(text(&item.detail).font(k.ty.small).color(pal.text_tertiary)),
            );
        }
    }
    list
}

fn format_compact_tokens(tokens: i64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn compact_cwd(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    let mut parts: Vec<&str> = trimmed.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    if parts.len() <= 2 {
        return cwd.to_string();
    }
    let tail = parts.split_off(parts.len() - 2);
    format!("…/{}", tail.join("/"))
}

fn render_status_footer(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let pct = s.context_usage.percent.clamp(0.0, 100.0);
    let fill_color = if pct >= 85.0 {
        pal.danger
    } else if pct >= 65.0 {
        pal.warning
    } else {
        pal.gauge
    };
    let bar_width = 52.0;
    let fill_width = (bar_width * (pct as f32 / 100.0)).max(if pct > 0.0 { 2.0 } else { 0.0 });

    let mut left = row()
        .key("footer-left")
        .grow(1.0)
        .min_w(0.0)
        .gap(10.0)
        .center_y()
        .child(
            text(compact_cwd(&s.config.cwd))
                .font(k.ty.small)
                .color(pal.text_tertiary)
                .help(s.config.cwd.clone())
                .truncate()
                .shrink(),
        );

    if let Some(branch) = s.diff.branch.as_deref().filter(|b| !b.is_empty()) {
        left = left.child(
            row()
                .gap(4.0)
                .center_y()
                .cursor(Cursor::Pointer)
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::SelectInspectorTab(InspectorTab::Diff), None))
                .child(text(branch).font(k.ty.small_medium).color(pal.text_secondary))
                .child(
                    text(format!("+{}", s.diff.total_additions))
                        .font(k.ty.small)
                        .color(pal.success),
                )
                .child(
                    text(format!("-{}", s.diff.total_deletions))
                        .font(k.ty.small)
                        .color(pal.danger),
                ),
        );
    }

    let token_str = if s.context_usage.window_tokens > 0 {
        format!(
            "{}/{} ({pct:.0}%)",
            format_compact_tokens(s.context_usage.used_tokens),
            format_compact_tokens(s.context_usage.window_tokens),
        )
    } else {
        format!("{pct:.0}%")
    };

    let right = row()
        .key("footer-right")
        .gap(8.0)
        .center_y()
        .child(
            row()
                .key("context-meter")
                .gap(6.0)
                .center_y()
                .child(
                    el().w(bar_width)
                        .h(5.0)
                        .radius(2.5)
                        .fill(pal.inset)
                        .border(0.5, pal.border)
                        .child(el().w(fill_width).h_full().radius(2.5).fill(fill_color)),
                )
                .child(text(token_str).font(k.ty.small).color(pal.text_tertiary)),
        )
        .child(
            text(format!("${:.4}", s.context_usage.session_usage.total_cost))
                .font(k.ty.small)
                .color(pal.text_tertiary),
        );

    row()
        .key("composer-status-footer")
        .w_full()
        .max_w(LayoutMetrics::CONTENT_MAX_WIDTH)
        .h(26.0)
        .px(4.0)
        .justify_between()
        .center_y()
        .gap(12.0)
        .child(left)
        .child(right)
}

/// Renders the composer card and 26pt Waku status footer docked beneath the transcript viewport.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let model_label = s.active_model.as_ref().map_or("Select model", |m| m.name.as_str());
    let is_streaming = s.active_stream.is_some();
    let has_text = !s.composer_edit.text.trim().is_empty();
    let autocomplete = active_autocomplete(s);

    let field = text_input(&s.composer_edit, k.ty.body)
        .key("composer-input")
        .w_full()
        .color(pal.text)
        .placeholder("Ask pi anything... (@ for files, / for commands)", pal.text_ghost)
        .caret(pal.accent, pal.selection)
        .label("Prompt composer")
        .on_edit(|st: &mut PiGuiState| &mut st.composer_edit)
        .on_input_key(|st: &mut PiGuiState, ev, tx| {
            if ev.key == Key::Tab
                && let Some(menu) = active_autocomplete(st)
                && let Some(first) = menu.items.first()
            {
                apply_autocomplete_choice(st, menu.kind, menu.token_start, menu.caret, &first.replacement);
                return true;
            }
            if ev.key == Key::Enter && (ev.mods.cmd || (!ev.mods.shift && !ev.mods.alt)) {
                let text = st.composer_edit.text.trim().to_string();
                if !text.is_empty() {
                    if st.active_stream.is_some() {
                        st.dispatch(PiGuiAction::QueueSteering { text, follow_up: false }, Some(tx));
                        st.composer_edit.clear();
                    } else {
                        st.dispatch(PiGuiAction::SubmitPrompt, Some(tx));
                    }
                }
                return true;
            }
            false
        });
    let input = rune::auto_grow(field, 2, 7).key("composer-input-scroll");

    let active_plugins = s.plugins.iter().filter(|p| p.enabled).count();
    let total_tools: usize = s.plugins.iter().filter(|p| p.enabled).map(|p| p.tool_count).sum();

    let left_pills = row()
        .key("composer-left-pills")
        .gap(6.0)
        .flex_wrap()
        .center_y()
        .shrink()
        .min_w(0.0)
        .child(
            el().key("composer-model-btn")
                .px(8.0)
                .py(4.0)
                .radius(k.radius.sm)
                .fill(pal.inset)
                .border(1.0, pal.border)
                .cursor(Cursor::Pointer)
                .hover(pal.overlay)
                .on_press(|st: &mut PiGuiState| {
                    st.dispatch(PiGuiAction::OpenOverlay(OverlayKind::ModelPicker), None);
                })
                .child(text(model_label).font(k.ty.small_medium).color(pal.text_secondary)),
        )
        .child(
            el().key("composer-thinking-btn")
                .px(8.0)
                .py(4.0)
                .radius(k.radius.sm)
                .fill(pal.inset)
                .border(1.0, pal.border)
                .cursor(Cursor::Pointer)
                .hover(pal.overlay)
                .on_press(|st: &mut PiGuiState| {
                    st.dispatch(PiGuiAction::CycleThinkingLevel, None);
                })
                .child(
                    text(format!("Thinking: {}", s.context_usage.thinking_level))
                        .font(k.ty.small)
                        .color(pal.text_tertiary),
                ),
        )
        .child(
            el().key("composer-plugins-chip")
                .cursor(Cursor::Pointer)
                .on_press(|st: &mut PiGuiState| {
                    st.dispatch(PiGuiAction::SelectInspectorTab(InspectorTab::Plugins), None);
                })
                .child(
                    Badge::new(format!("{active_plugins} plugins • {total_tools} tools"))
                        .variant(BadgeVariant::Subtle)
                        .build(k),
                ),
        );

    let mut right_btns = row().key("composer-right-btns").gap(6.0).center_y();
    if is_streaming {
        if has_text {
            right_btns = right_btns.child(
                Button::new("Steer")
                    .variant(ButtonVariant::Outline)
                    .control_size(ControlSize::Regular)
                    .on_press(|st: &mut PiGuiState| {
                        let text = st.composer_edit.text.trim().to_string();
                        if !text.is_empty() {
                            st.dispatch(PiGuiAction::QueueSteering { text, follow_up: false }, None);
                            st.composer_edit.clear();
                        }
                    })
                    .build(k)
                    .key("composer-steer-btn"),
            );
        }
        right_btns = right_btns.child(
            Button::new("Stop")
                .icon(&ICON_STOP)
                .variant(ButtonVariant::Danger)
                .control_size(ControlSize::Regular)
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::AbortRun, None))
                .build(k)
                .key("composer-stop-btn"),
        );
    } else {
        right_btns = right_btns.child(
            Button::new("Send")
                .icon(&ICON_SEND)
                .variant(ButtonVariant::Submit)
                .control_size(ControlSize::Regular)
                .disabled(!has_text)
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::SubmitPrompt, None))
                .build(k)
                .key("composer-send-btn"),
        );
    }

    let controls = row()
        .key("composer-controls")
        .w_full()
        .justify_between()
        .center_y()
        .child(left_pills)
        .child(right_btns);

    let card_border = if s.composer_edit.focused {
        pal.accent
    } else {
        pal.border_strong
    };

    let card = col()
        .key("composer-card")
        .w_full()
        .max_w(LayoutMetrics::CONTENT_MAX_WIDTH)
        .gap(8.0)
        .padding(12.0)
        .radius(k.radius.lg)
        .fill(pal.composer)
        .border(1.0, card_border)
        .child(input)
        .child(controls);

    let mut dock = col()
        .key("composer-dock")
        .w_full()
        .items_center()
        .gap(6.0)
        .px(20.0)
        .pt(8.0)
        .pb(10.0);

    if let Some(menu) = &autocomplete {
        dock = dock.child(render_autocomplete_popover(menu, k, pal));
    }

    dock.child(card).child(render_status_footer(s, k, pal))
}
