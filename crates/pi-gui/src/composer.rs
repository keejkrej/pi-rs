//! Bottom 720px composer card, 692px docked queue tray, `@`/`/` autocomplete, and 28px workspace footer.

use rune::{Color, Cursor, Cx, El, Icon, Key, TextStyle, col, el, icon, row, text, text_input};
use rune_kit::Kit;

use crate::state::{InspectorTab, OverlayKind, PiGuiAction, PiGuiState, PopoverKind, SettingsPage, WorkspaceMode};
use crate::theme::{LayoutMetrics, ThemePreference, WakuPalette, WakuRadius};

static ICON_SEND: Icon = Icon {
    name: "waku-send",
    view: 16.0,
    body: r#"<path d="M8 12.5V3.5M4 7.5L8 3.5l4 4" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_STOP: Icon = Icon {
    name: "waku-stop",
    view: 24.0,
    body: r#"<rect x="6" y="6" width="12" height="12" rx="2.5" fill="currentColor"/>"#,
};

static ICON_QUEUE: Icon = Icon {
    name: "waku-queue-row",
    view: 24.0,
    body: r#"<path d="M4 4h16" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><path d="M4 4v7a4 4 0 0 0 4 4h12" stroke="currentColor" stroke-width="2" stroke-linecap="round" fill="none"/><polyline points="15 10 20 15 15 20" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_CORNER_DOWN_RIGHT: Icon = Icon {
    name: "waku-steer",
    view: 24.0,
    body: r#"<polyline points="15 10 20 15 15 20" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="M4 4v7a4 4 0 0 0 4 4h12" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_TRASH: Icon = Icon {
    name: "waku-trash",
    view: 24.0,
    body: r#"<path d="M10 11v6M14 11v6M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_ELLIPSIS: Icon = Icon {
    name: "waku-ellipsis",
    view: 24.0,
    body: r#"<circle cx="12" cy="12" r="1.5" fill="currentColor"/><circle cx="19" cy="12" r="1.5" fill="currentColor"/><circle cx="5" cy="12" r="1.5" fill="currentColor"/>"#,
};

static ICON_CHEVRON_DOWN: Icon = Icon {
    name: "waku-chip-chev",
    view: 16.0,
    body: r#"<path d="M4.8 6.4L8 9.6l3.2-3.2" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_PLUS: Icon = Icon {
    name: "waku-attach-plus",
    view: 16.0,
    body: r#"<path d="M8 3.2v9.6M3.2 8h9.6" stroke="currentColor" stroke-width="1.45" stroke-linecap="round"/>"#,
};

static ICON_FOLDER: Icon = Icon {
    name: "waku-folder",
    view: 16.0,
    body: r#"<path d="M2.2 4.2a1.2 1.2 0 0 1 1.2-1.2h3.1l1.2 1.3h4.9a1.2 1.2 0 0 1 1.2 1.2v6.3a1.2 1.2 0 0 1-1.2 1.2H3.4a1.2 1.2 0 0 1-1.2-1.2V4.2Z" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linejoin="round"/>"#,
};

static ICON_LAPTOP: Icon = Icon {
    name: "waku-laptop",
    view: 24.0,
    body: r#"<rect width="18" height="12" x="3" y="4" rx="2" ry="2" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><line x1="2" x2="22" y1="20" y2="20" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

static ICON_FORK: Icon = Icon {
    name: "waku-fork",
    view: 24.0,
    body: r#"<path d="M2 12h9L21 2" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="M13 2h8v8" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="m13 14 8 8" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="M13 22h8v-8" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_BRANCH: Icon = Icon {
    name: "waku-branch",
    view: 16.0,
    body: r#"<circle cx="5" cy="4" r="1.4" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="5" cy="12" r="1.4" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="11" cy="6" r="1.4" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M5 5.4v5.2M11 7.4c0 2-1.5 3-6 3" stroke="currentColor" stroke-width="1.2" fill="none"/>"#,
};

static ICON_GAUGE_EMPTY: Icon = Icon {
    name: "waku-gauge-empty",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="5.5" stroke="currentColor" stroke-opacity="0.32" stroke-width="1.6" fill="none"/>"#,
};

static ICON_GAUGE_LOW: Icon = Icon {
    name: "waku-gauge-low",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="5.5" stroke="currentColor" stroke-opacity="0.28" stroke-width="1.6" fill="none"/><path d="M8 2.5A5.5 5.5 0 0 1 13.5 8" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" fill="none"/>"#,
};

static ICON_GAUGE_HIGH: Icon = Icon {
    name: "waku-gauge-high",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="5.5" stroke="currentColor" stroke-opacity="0.28" stroke-width="1.6" fill="none"/><path d="M8 2.5A5.5 5.5 0 1 1 2.8 9.8" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" fill="none"/>"#,
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
    ("/skills", "Inspect QuickJS extensions and MCP skills"),
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
        match replacement.trim() {
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
                st.dispatch(PiGuiAction::OpenPopover(PopoverKind::ModelPicker), None);
                return;
            }
            "/settings" => {
                st.composer_edit.clear();
                st.dispatch(PiGuiAction::OpenSettings(SettingsPage::General), None);
                return;
            }
            "/skills" => {
                st.composer_edit.clear();
                st.dispatch(PiGuiAction::OpenSettings(SettingsPage::Skills), None);
                return;
            }
            "/diff" => {
                st.composer_edit.clear();
                st.dispatch(PiGuiAction::SelectInspectorTab(InspectorTab::Changes), None);
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
        .radius(WakuRadius::LG)
        .fill(pal.raised)
        .border_layout()
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
                    .radius(WakuRadius::SM)
                    .fill(if idx == 0 { pal.overlay } else { Color::TRANSPARENT })
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

fn title_case_thinking(raw: &str) -> &'static str {
    match raw {
        "off" => "Off",
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" | "max" => "Max",
        _ => "Medium",
    }
}

fn render_chip_button(
    key: &'static str,
    label: &'static str,
    display_text: &str,
    leading: Option<El>,
    height: f32,
    active: bool,
    pal: &WakuPalette,
    popover: PopoverKind,
) -> El {
    let fill = if active { pal.overlay_strong } else { Color::TRANSPARENT };
    let mut chip = row()
        .key(key)
        .role("button")
        .label_static(label)
        .h(height)
        .px(6.0)
        .gap(6.0)
        .center_y()
        .radius(WakuRadius::SM)
        .fill(fill)
        .hover(pal.overlay)
        .cursor(Cursor::Pointer)
        .on_press(move |st: &mut PiGuiState| st.dispatch(PiGuiAction::TogglePopover(popover), None));
    if let Some(lead) = leading {
        chip = chip.child(lead.shrink());
    }
    chip.child(
        text(display_text)
            .font(TextStyle::new(12.5, 400.0).lh(14.0))
            .color(pal.text_secondary)
            .truncate(),
    )
    .child(icon(&ICON_CHEVRON_DOWN, 11.0).color(pal.text_tertiary))
}

fn render_queue_tray(s: &PiGuiState, pal: &WakuPalette) -> El {
    let mut tray = col()
        .key("workspace.main.queue_tray")
        .role("region")
        .w_full()
        .max_w(LayoutMetrics::QUEUE_TRAY_MAX_WIDTH)
        .py(4.0)
        .corners([WakuRadius::LG, WakuRadius::LG, 0.0, 0.0])
        .fill(pal.composer)
        .border_sides([1.0, 1.0, 0.0, 1.0], pal.border)
        .clip_rounded(0.0);

    for (idx, item) in s.steering_queue.iter().enumerate() {
        let steer_id = item.id.clone();
        let remove_id = item.id.clone();
        let edit_id = item.id.clone();
        let edit_row_id = item.id.clone();
        tray = tray.child(
            row()
                .key(format!("queue-item-{idx}"))
                .w_full()
                .h(30.0)
                .pl(12.0)
                .pr(6.0)
                .gap(9.0)
                .center_y()
                .hover(pal.overlay)
                .cursor(Cursor::Pointer)
                .on_press(move |st: &mut PiGuiState| {
                    st.dispatch(PiGuiAction::EditQueuedItem(edit_row_id.clone()), None)
                })
                .child(icon(&ICON_QUEUE, 12.0).color(pal.text_tertiary).shrink())
                .child(
                    text(&item.text)
                        .font(TextStyle::new(12.5, 400.0))
                        .color(pal.text)
                        .truncate()
                        .grow(1.0)
                        .min_w(0.0),
                )
                .child(
                    row()
                        .gap(2.0)
                        .center_y()
                        .shrink()
                        .child(
                            row()
                                .key(format!("queue-item-{idx}-steer"))
                                .role("button")
                                .label_static("Steer")
                                .h(24.0)
                                .px(7.0)
                                .gap(5.0)
                                .center_y()
                                .radius(6.0)
                                .hover(pal.overlay_strong)
                                .cursor(Cursor::Pointer)
                                .on_press(move |st: &mut PiGuiState| {
                                    st.dispatch(PiGuiAction::SteerQueuedItem(steer_id.clone()), None);
                                })
                                .child(icon(&ICON_CORNER_DOWN_RIGHT, 11.0).color(pal.text_secondary))
                                .child(
                                    text("Steer")
                                        .font(TextStyle::new(12.5, 400.0))
                                        .color(pal.text_secondary),
                                ),
                        )
                        .child(
                            el().key(format!("queue-item-{idx}-remove"))
                                .role("button")
                                .label_static("Remove queued message")
                                .size(24.0, 24.0)
                                .radius(6.0)
                                .center()
                                .hover(pal.overlay_strong)
                                .cursor(Cursor::Pointer)
                                .on_press(move |st: &mut PiGuiState| {
                                    st.dispatch(PiGuiAction::RemoveQueuedItem(remove_id.clone()), None);
                                })
                                .child(icon(&ICON_TRASH, 12.0).color(pal.text_secondary)),
                        )
                        .child(
                            el().key(format!("queue-item-{idx}-more"))
                                .role("button")
                                .label_static("More queued actions")
                                .size(24.0, 24.0)
                                .radius(6.0)
                                .center()
                                .hover(pal.overlay_strong)
                                .cursor(Cursor::Pointer)
                                .on_press(move |st: &mut PiGuiState| {
                                    st.dispatch(PiGuiAction::EditQueuedItem(edit_id.clone()), None);
                                })
                                .child(icon(&ICON_ELLIPSIS, 12.5).color(pal.text_secondary)),
                        ),
                ),
        );
    }
    tray
}

fn render_workspace_footer(s: &PiGuiState, pal: &WakuPalette) -> El {
    let pct = s.context_usage.percent.clamp(0.0, 100.0);
    let (gauge_icon, gauge_color) = if pct <= 0.0 {
        (&ICON_GAUGE_EMPTY, pal.text_tertiary)
    } else if pct >= 85.0 {
        (&ICON_GAUGE_HIGH, pal.danger)
    } else if pct >= 65.0 {
        (&ICON_GAUGE_HIGH, pal.warning)
    } else if pct >= 50.0 {
        (&ICON_GAUGE_HIGH, pal.gauge)
    } else {
        (&ICON_GAUGE_LOW, pal.gauge)
    };
    let workspace_label = match s.workspace_mode {
        WorkspaceMode::Local => "Local",
        WorkspaceMode::NewWorktree => "Worktree",
    };

    let mut footer = row()
        .key("workspace.main.footer")
        .role("region")
        .w_full()
        .max_w(LayoutMetrics::CONTENT_MAX_WIDTH)
        .h(28.0)
        .mt(4.0)
        .mb(8.0)
        .px(10.0)
        .gap(2.0)
        .center_y()
        .clip_rounded(0.0)
        .child(render_chip_button(
            "workspace.main.footer.project_chip",
            "Select Project",
            &s.selected_project,
            Some(icon(&ICON_FOLDER, 13.0).color(pal.text_tertiary)),
            26.0,
            s.popover == Some(PopoverKind::ProjectPicker),
            pal,
            PopoverKind::ProjectPicker,
        ))
        .child(render_chip_button(
            "workspace.main.footer.workspace_chip",
            "Select Workspace",
            workspace_label,
            Some(
                icon(
                    match s.workspace_mode {
                        WorkspaceMode::Local => &ICON_LAPTOP,
                        WorkspaceMode::NewWorktree => &ICON_FORK,
                    },
                    12.0,
                )
                .color(pal.text_tertiary),
            ),
            26.0,
            s.popover == Some(PopoverKind::WorkspacePicker),
            pal,
            PopoverKind::WorkspacePicker,
        ))
        .child(render_chip_button(
            "workspace.main.footer.branch_chip",
            "Switch Git Branch",
            &s.selected_branch,
            Some(icon(&ICON_BRANCH, 13.0).color(pal.text_tertiary)),
            26.0,
            s.popover == Some(PopoverKind::BranchPicker),
            pal,
            PopoverKind::BranchPicker,
        ));

    if let Some(ext_status) = &s.extension_status {
        footer = footer.child(
            row()
                .key("workspace.main.footer.extension_status")
                .role("status")
                .h(22.0)
                .px(8.0)
                .center_y()
                .radius(WakuRadius::SM)
                .fill(pal.overlay)
                .child(
                    text(ext_status)
                        .font(TextStyle::new(11.5, 400.0))
                        .color(pal.text_tertiary),
                ),
        );
    }

    let gauge_btn = el()
        .key("workspace.main.footer.context_gauge")
        .role("button")
        .label_static("Context Window Usage")
        .size(24.0, 24.0)
        .radius(WakuRadius::SM)
        .center()
        .cursor(Cursor::Pointer)
        .hover(pal.overlay)
        .help(format!("Context usage: {pct:.1}%"))
        .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::TogglePopover(PopoverKind::ContextUsage), None))
        .child(icon(gauge_icon, 15.0).color(gauge_color));

    footer.child(el().grow(1.0)).child(gauge_btn)
}

/// Renders Waku's bottom 720px composer card (`workspace.main.composer`), optional queue tray, and 28px footer.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let model_label = s.active_model.as_ref().map_or("Select model", |m| m.name.as_str());
    let is_streaming = s.active_stream.is_some();
    let has_text = !s.composer_edit.text.trim().is_empty();
    let autocomplete = active_autocomplete(s);

    let field = text_input(&s.composer_edit, TextStyle::new(14.0, 400.0).lh(20.0))
        .w_full()
        .color(pal.text)
        .placeholder("Do anything…", pal.text_ghost)
        .caret(pal.accent, pal.selection)
        .label("Prompt editor")
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
                        st.dispatch(PiGuiAction::QueueSteering { text, follow_up: true }, Some(tx));
                        st.composer_edit.clear();
                    } else {
                        st.dispatch(PiGuiAction::SubmitPrompt, Some(tx));
                    }
                }
                return true;
            }
            false
        });

    let editor = el()
        .key("workspace.main.composer.editor")
        .role("textbox")
        .label_static("Prompt editor")
        .mx(11.0)
        .min_h(28.0)
        .clip_rounded(0.0)
        .child(rune::auto_grow(field, 1, 7).w_full());

    let model_mark = s
        .active_model
        .as_ref()
        .map(|m| crate::overlays::provider_mark_view(&m.provider, 13.0, pal, true));

    let mut toolbar = row()
        .key("workspace.main.composer.toolbar")
        .role("toolbar")
        .mx(9.0)
        .mt(8.0)
        .h(26.0)
        .gap(4.0)
        .center_y()
        .clip_rounded(0.0)
        .child(render_chip_button(
            "workspace.main.composer.model_chip",
            "Select Model Chip",
            model_label,
            model_mark,
            24.0,
            s.popover == Some(PopoverKind::ModelPicker),
            pal,
            PopoverKind::ModelPicker,
        ))
        .child(render_chip_button(
            "workspace.main.composer.thinking_chip",
            "Select Thinking Chip",
            title_case_thinking(&s.context_usage.thinking_level),
            None,
            24.0,
            s.popover == Some(PopoverKind::ThinkingPicker),
            pal,
            PopoverKind::ThinkingPicker,
        ))
        .child(
            el().key("workspace.main.composer.attach")
                .role("button")
                .label_static("Attach clipboard image")
                .size(24.0, 24.0)
                .radius(WakuRadius::SM)
                .center()
                .cursor(Cursor::Pointer)
                .hover(pal.overlay)
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::AttachClipboardImage, None))
                .child(icon(&ICON_PLUS, 13.5).color(pal.text_tertiary)),
        );

    if let Some(untracked) = s.bash_mode {
        let mode_str = if untracked { "!!" } else { "!" };
        toolbar = toolbar.child(
            el().key("workspace.main.composer.bash_badge")
                .role("status")
                .h(22.0)
                .px(6.0)
                .center()
                .radius(WakuRadius::SM)
                .fill(pal.overlay_strong)
                .cursor(Cursor::Pointer)
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::ToggleBashMode, None))
                .child(
                    text(mode_str)
                        .font(TextStyle::new(11.5, 600.0).mono())
                        .color(pal.accent),
                ),
        );
    }

    toolbar = toolbar.child(el().grow(1.0));

    if is_streaming {
        toolbar = toolbar
            .child(
                el().key("workspace.main.composer.send")
                    .role("button")
                    .label_static("Stop")
                    .size(26.0, 26.0)
                    .radius(WakuRadius::PILL)
                    .center()
                    .fill(pal.overlay_strong)
                    .cursor(Cursor::Pointer)
                    .hover(pal.danger_soft)
                    .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::AbortRun, None))
                    .child(icon(&ICON_STOP, 18.0).color(pal.text)),
            )
            .child(
                el().key("workspace.main.composer.queue_followup")
                    .role("button")
                    .label_static("Queue Follow-up")
                    .ml(6.0)
                    .size(26.0, 26.0)
                    .radius(WakuRadius::PILL)
                    .center()
                    .fill(pal.inverse)
                    .cursor(Cursor::Pointer)
                    .on_press(|st: &mut PiGuiState| {
                        let text = st.composer_edit.text.trim().to_string();
                        if !text.is_empty() {
                            st.dispatch(PiGuiAction::QueueSteering { text, follow_up: true }, None);
                            st.composer_edit.clear();
                        } else {
                            st.dispatch(PiGuiAction::OpenOverlay(OverlayKind::CommandPalette), None);
                        }
                    })
                    .child(icon(&ICON_SEND, 13.0).color(pal.on_inverse)),
            );
    } else {
        let send_fill = if has_text { pal.inverse } else { pal.overlay_strong };
        let send_fg = if has_text { pal.on_inverse } else { pal.text_tertiary };
        toolbar = toolbar.child(
            el().key("workspace.main.composer.send")
                .role("button")
                .label_static("Send")
                .size(26.0, 26.0)
                .radius(WakuRadius::PILL)
                .center()
                .fill(send_fill)
                .cursor(Cursor::Pointer)
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::SubmitPrompt, None))
                .child(icon(&ICON_SEND, 13.0).color(send_fg)),
        );
    }

    let card = col()
        .key("workspace.main.composer")
        .role("region")
        .w_full()
        .max_w(LayoutMetrics::CONTENT_MAX_WIDTH)
        .py(10.0)
        .radius(WakuRadius::COMPOSER)
        .fill(pal.composer)
        .border_layout()
        .border(1.0, pal.border)
        .clip_rounded(WakuRadius::COMPOSER)
        .child(editor)
        .child(toolbar);

    let mut dock = col().key("composer-dock").w_full().items_center().px(20.0).shrink();

    if let Some(menu) = &autocomplete {
        dock = dock.child(render_autocomplete_popover(menu, k, pal));
    }
    if is_streaming && !s.steering_queue.is_empty() {
        dock = dock.child(render_queue_tray(s, pal));
    }
    dock.child(card).child(render_workspace_footer(s, pal))
}
