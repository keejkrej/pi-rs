//! Leading 44pt navigation rail and collapsible session/file/plugin sidebar view.

use std::borrow::Cow;

use chrono::{DateTime, Local, Utc};
use rune::{Color, Cursor, Cx, El, Icon, col, el, icon, row, text};
use rune_kit::shell::{RowStatus, Sidebar, SidebarHeader, SidebarRow, SidebarSection};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, TextInput, glyphs};

use crate::state::{InspectorTab, NavRailTab, OverlayKind, PiGuiAction, PiGuiState, ThreadGroup, ThreadSummary};
use crate::theme::{LayoutMetrics, ThemePreference, WakuPalette};

static ICON_THREADS: Icon = Icon {
    name: "pi-nav-threads",
    view: 16.0,
    body: r#"<path d="M2.5 3.5h11v7.5H6.5l-3 2.5V11H2.5V3.5Z" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linejoin="round"/>"#,
};

static ICON_FILES: Icon = Icon {
    name: "pi-nav-files",
    view: 16.0,
    body: r#"<path d="M2 3.5h4.2l1.3 1.4H14v8H2V3.5Z" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linejoin="round"/>"#,
};

static ICON_PLUGINS: Icon = Icon {
    name: "pi-nav-plugins",
    view: 16.0,
    body: r#"<path d="M5.5 2.5v3M10.5 2.5v3M3.5 5.5h9v2.2a4.5 4.5 0 0 1-9 0V5.5ZM8 12.2v1.8" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

static ICON_SETTINGS: Icon = Icon {
    name: "pi-nav-settings",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="2.2" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M8 2.2v1.3M8 12.5v1.3M2.2 8h1.3M12.5 8h1.3M3.9 3.9l.9.9M11.2 11.2l.9.9M12.1 3.9l-.9.9M4.8 11.2l-.9.9" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};

static ICON_PLUS: Icon = Icon {
    name: "pi-plus",
    view: 16.0,
    body: r#"<path d="M8 3v10M3 8h10" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"#,
};

static ICON_THEME: Icon = Icon {
    name: "pi-theme-toggle",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="5.5" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M8 2.5a5.5 5.5 0 0 0 0 11V2.5Z" fill="currentColor"/>"#,
};

const GROUP_ORDER: [ThreadGroup; 4] = [
    ThreadGroup::Today,
    ThreadGroup::Yesterday,
    ThreadGroup::ThisWeek,
    ThreadGroup::Earlier,
];

enum FlatThreadItem<'a> {
    Header(ThreadGroup, usize),
    Thread(&'a ThreadSummary),
}

/// Formats a millisecond timestamp as a short sidebar time label without I/O.
#[must_use]
pub fn format_thread_time(timestamp_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
        .map(|dt| dt.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

fn group_label(group: ThreadGroup) -> &'static str {
    match group {
        ThreadGroup::Today => "Today",
        ThreadGroup::Yesterday => "Yesterday",
        ThreadGroup::ThisWeek => "This week",
        ThreadGroup::Earlier => "Earlier",
    }
}

fn group_flag_key(group: ThreadGroup) -> &'static str {
    match group {
        ThreadGroup::Today => "ui.sidebar_group.today",
        ThreadGroup::Yesterday => "ui.sidebar_group.yesterday",
        ThreadGroup::ThisWeek => "ui.sidebar_group.week",
        ThreadGroup::Earlier => "ui.sidebar_group.earlier",
    }
}

fn nav_icon(tab: NavRailTab) -> &'static Icon {
    match tab {
        NavRailTab::Sessions => &ICON_THREADS,
        NavRailTab::Files => &ICON_FILES,
        NavRailTab::Extensions => &ICON_PLUGINS,
        NavRailTab::Settings => &ICON_SETTINGS,
    }
}

fn build_nav_rail(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let mut rail = col()
        .key("sidebar-nav-rail")
        .w(LayoutMetrics::NAV_RAIL_WIDTH)
        .h_full()
        .min_h(0.0)
        .py(8.0)
        .gap(6.0)
        .items_center()
        .border_sides([0.0, 1.0, 0.0, 0.0], pal.sidebar_border);

    for tab in [NavRailTab::Sessions, NavRailTab::Files, NavRailTab::Extensions] {
        let active = s.nav_tab == tab;
        rail = rail.child(
            row()
                .key(format!("nav-rail-{}", tab.label()))
                .w(LayoutMetrics::NAV_RAIL_WIDTH)
                .h(32.0)
                .center_y()
                .child(
                    el().w(3.0)
                        .h(18.0)
                        .radius(1.5)
                        .fill(if active { pal.accent } else { Color::TRANSPARENT }),
                )
                .child(
                    row().grow(1.0).justify_center().child(
                        IconButton::new(nav_icon(tab))
                            .variant(IconButtonVariant::GhostMuted)
                            .expanded(active)
                            .label(tab.label())
                            .help(tab.label())
                            .on_press(move |st: &mut PiGuiState| st.dispatch(PiGuiAction::SelectNavTab(tab), None))
                            .build(k),
                    ),
                )
                .child(el().w(3.0)),
        );
    }

    let next_theme = if pal.is_dark {
        ThemePreference::Light
    } else {
        ThemePreference::Dark
    };

    rail.child(el().key("nav-rail-spacer").grow(1.0).min_h(0.0))
        .child(
            IconButton::new(&ICON_THEME)
                .variant(IconButtonVariant::GhostMuted)
                .label(if pal.is_dark {
                    "Switch to Light Theme"
                } else {
                    "Switch to Dark Theme"
                })
                .on_press(move |st: &mut PiGuiState| {
                    st.dispatch(PiGuiAction::SetThemePreference(next_theme), None);
                })
                .build(k)
                .key("nav-rail-theme"),
        )
        .child(
            IconButton::new(&ICON_SETTINGS)
                .variant(IconButtonVariant::GhostMuted)
                .expanded(s.nav_tab == NavRailTab::Settings)
                .label("Settings")
                .shortcut(if cfg!(target_os = "macos") { "⌘," } else { "Ctrl+," })
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::SelectNavTab(NavRailTab::Settings), None))
                .build(k)
                .key("nav-rail-settings"),
        )
}

fn filtered_threads<'a>(s: &'a PiGuiState) -> Vec<&'a ThreadSummary> {
    let query = s.search_edit.text.trim().to_ascii_lowercase();
    s.threads
        .iter()
        .filter(|t| {
            query.is_empty()
                || t.title.to_ascii_lowercase().contains(&query)
                || t.cwd.to_ascii_lowercase().contains(&query)
        })
        .collect()
}

fn flatten_threads<'a>(s: &'a PiGuiState) -> Vec<FlatThreadItem<'a>> {
    let matches = filtered_threads(s);
    let mut out = Vec::with_capacity(matches.len() + 4);
    for group in GROUP_ORDER {
        let count = matches.iter().filter(|t| t.group == group).count();
        if count == 0 {
            continue;
        }
        out.push(FlatThreadItem::Header(group, count));
        let collapsed = s.settings_flags.get(group_flag_key(group)).copied().unwrap_or(false);
        if !collapsed {
            for t in matches.iter().filter(|t| t.group == group) {
                out.push(FlatThreadItem::Thread(t));
            }
        }
    }
    out
}

fn build_thread_row(thread: &ThreadSummary, selected: bool, k: &Kit, pal: &WakuPalette) -> El {
    let thread_id = thread.id.clone();
    let time_text = format_thread_time(thread.updated_at_ms);
    let status = if thread.is_streaming {
        RowStatus::Busy
    } else if thread.has_unread {
        RowStatus::Unread
    } else {
        let dot_color = if selected { pal.success } else { pal.text_ghost };
        RowStatus::Other(el().size(6.0, 6.0).radius(3.0).fill(dot_color))
    };

    let mut row_builder = SidebarRow::new(&thread.title)
        .selected(selected)
        .status(status)
        .height(40.0)
        .subtitle(thread.model.as_deref().unwrap_or("default model"))
        .on_select(move |st: &mut PiGuiState, _| {
            st.dispatch(PiGuiAction::SelectThread(thread_id.clone()), None);
        });

    if !time_text.is_empty() {
        row_builder = row_builder.detail(time_text);
    } else if thread.message_count > 0 {
        row_builder = row_builder.badge(thread.message_count.to_string());
    }
    row_builder.build(k).key(format!("thread-{}", thread.id))
}

fn build_sessions_sidebar(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let ws_name = s
        .config
        .cwd
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|p| !p.is_empty())
        .unwrap_or("workspace");

    let header_bar = col()
        .key("sidebar-ws-header")
        .w_full()
        .gap(6.0)
        .child(
            row()
                .w_full()
                .justify_between()
                .center_y()
                .gap(6.0)
                .child(
                    row()
                        .gap(6.0)
                        .center_y()
                        .min_w(0.0)
                        .grow(1.0)
                        .child(icon(&glyphs::FOLDER, 13.0).color(pal.text_tertiary))
                        .child(
                            text(ws_name)
                                .font(k.ty.small_medium)
                                .color(pal.text_secondary)
                                .truncate()
                                .shrink(),
                        ),
                )
                .child(
                    text(format!("{}", s.threads.len()))
                        .font(k.ty.small)
                        .color(pal.text_ghost),
                ),
        )
        .child(
            Button::new("New Thread")
                .icon(&ICON_PLUS)
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .shortcut(if cfg!(target_os = "macos") { "⌘N" } else { "Ctrl+N" })
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::NewThread, None))
                .build(k)
                .w_full()
                .key("new-thread-btn"),
        );

    let search_box = TextInput::new(&s.search_edit)
        .placeholder("Filter threads...")
        .leading_icon(&glyphs::SEARCH)
        .clear_button()
        .width(None)
        .on_edit(|st: &mut PiGuiState| &mut st.search_edit)
        .on_action(|st: &mut PiGuiState, _| st.search_edit.clear())
        .build(k)
        .key("sidebar-thread-search");

    let items = flatten_threads(s);
    let mut sb = Sidebar::new()
        .label("Threads")
        .header(header_bar)
        .header(search_box)
        .header_gap(6.0)
        .header_padding(4.0, 8.0);

    if items.is_empty() {
        sb = sb.child(
            col()
                .key("sidebar-empty")
                .w_full()
                .px(8.0)
                .py(18.0)
                .gap(4.0)
                .child(
                    text(if s.threads.is_empty() {
                        "No saved sessions yet"
                    } else {
                        "No matching threads"
                    })
                    .font(k.ty.small_medium)
                    .color(pal.text_secondary)
                    .wrap(),
                )
                .child(
                    text("Start a new thread or adjust your filter.")
                        .font(k.ty.small)
                        .color(pal.text_tertiary)
                        .wrap(),
                ),
        );
    } else if items.len() > 36 {
        let count = items.len();
        let dark = pal.is_dark;
        sb = sb.row_height(40.0).virtual_rows::<PiGuiState>(
            count,
            |st, idx| {
                let items = flatten_threads(st);
                match items.get(idx) {
                    Some(FlatThreadItem::Header(group, _)) => Cow::Owned(format!("hdr-{}", group_label(*group))),
                    Some(FlatThreadItem::Thread(t)) => Cow::Owned(format!("thread-{}", t.id)),
                    None => Cow::Borrowed("empty"),
                }
            },
            move |st, idx| {
                let kit = crate::theme::waku_kit(dark);
                let pal = crate::theme::palette(dark);
                let items = flatten_threads(st);
                match items.get(idx) {
                    Some(FlatThreadItem::Header(group, count)) => {
                        let g = *group;
                        let collapsed = st.settings_flags.get(group_flag_key(g)).copied().unwrap_or(false);
                        SidebarHeader::new(group_label(g))
                            .count(*count)
                            .collapsed(collapsed)
                            .on_toggle(move |state: &mut PiGuiState, _| {
                                let key = group_flag_key(g).to_string();
                                let cur = state.settings_flags.get(&key).copied().unwrap_or(false);
                                state.settings_flags.insert(key, !cur);
                            })
                            .build(kit)
                    }
                    Some(FlatThreadItem::Thread(t)) => {
                        let selected = st.active_thread_id.as_deref() == Some(t.id.as_str());
                        build_thread_row(t, selected, kit, &pal)
                    }
                    None => el(),
                }
            },
        );
    } else {
        let matches = filtered_threads(s);
        for group in GROUP_ORDER {
            let group_rows: Vec<&ThreadSummary> = matches.iter().copied().filter(|t| t.group == group).collect();
            if group_rows.is_empty() {
                continue;
            }
            let collapsed = s.settings_flags.get(group_flag_key(group)).copied().unwrap_or(false);
            let mut section = SidebarSection::new(group_label(group))
                .key(format!("section-{}", group_label(group)))
                .count(group_rows.len())
                .collapsed(collapsed)
                .on_toggle(move |st: &mut PiGuiState, _| {
                    let key = group_flag_key(group).to_string();
                    let cur = st.settings_flags.get(&key).copied().unwrap_or(false);
                    st.settings_flags.insert(key, !cur);
                });
            for t in group_rows {
                let selected = s.active_thread_id.as_deref() == Some(t.id.as_str());
                section = section.row(build_thread_row(t, selected, k, pal));
            }
            sb = sb.section(section);
        }
    }

    sb.build(k)
}

fn build_files_sidebar(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let query = s.search_edit.text.trim().to_ascii_lowercase();
    let search_box = TextInput::new(&s.search_edit)
        .placeholder("Filter files...")
        .leading_icon(&glyphs::SEARCH)
        .clear_button()
        .width(None)
        .on_edit(|st: &mut PiGuiState| &mut st.search_edit)
        .on_action(|st: &mut PiGuiState, _| st.search_edit.clear())
        .build(k);

    let mut sb = Sidebar::new()
        .label("Files")
        .header(search_box)
        .header_padding(4.0, 8.0);
    let mut shown = 0usize;
    for entry in s.files.iter().filter(|f| !f.is_dir) {
        if !query.is_empty() && !entry.path.to_ascii_lowercase().contains(&query) {
            continue;
        }
        let path = entry.path.clone();
        let selected = s.open_file.as_ref().is_some_and(|f| f.path == entry.path);
        sb = sb.child(
            SidebarRow::new(&entry.name)
                .icon(&glyphs::FILE_CODE)
                .selected(selected)
                .detail(if entry.modified { "M" } else { "" })
                .on_select(move |st: &mut PiGuiState, _| {
                    st.dispatch(PiGuiAction::OpenFile(path.clone()), None);
                })
                .build(k)
                .key(format!("sidebar-file-{}", entry.path)),
        );
        shown += 1;
        if shown >= 150 {
            break;
        }
    }
    if shown == 0 {
        sb = sb.child(
            text(format!("{} files indexed", s.files.len()))
                .font(k.ty.small)
                .color(pal.text_tertiary)
                .px(8.0)
                .py(8.0),
        );
    }
    sb.build(k)
}

fn build_extensions_sidebar(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let header_btn = Button::new("Open Plugins Inspector")
        .variant(ButtonVariant::Outline)
        .control_size(ControlSize::Small)
        .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::SelectInspectorTab(InspectorTab::Plugins), None))
        .build(k);
    let mut sb = Sidebar::new()
        .label("Plugins")
        .header(header_btn)
        .header_padding(4.0, 8.0);
    if s.plugins.is_empty() {
        sb = sb.child(
            text("No extensions loaded")
                .font(k.ty.small)
                .color(pal.text_tertiary)
                .px(8.0)
                .py(8.0),
        );
    } else {
        for plugin in &s.plugins {
            sb = sb.child(
                SidebarRow::new(&plugin.name)
                    .subtitle(plugin.description.clone())
                    .status(if plugin.error.is_some() {
                        RowStatus::Attention
                    } else if plugin.enabled {
                        RowStatus::Other(el().size(6.0, 6.0).radius(3.0).fill(pal.success))
                    } else {
                        RowStatus::Other(el().size(6.0, 6.0).radius(3.0).fill(pal.text_ghost))
                    })
                    .on_select(|st: &mut PiGuiState, _| {
                        st.dispatch(PiGuiAction::OpenOverlay(OverlayKind::Settings), None);
                    })
                    .build(k)
                    .key(format!("sidebar-plugin-{}", plugin.id)),
            );
        }
    }
    sb.build(k)
}

/// Renders the sidebar pane: 44pt nav rail + active session/file/plugin list.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let body = match s.nav_tab {
        NavRailTab::Sessions => build_sessions_sidebar(s, k, pal),
        NavRailTab::Files => build_files_sidebar(s, k, pal),
        NavRailTab::Extensions => build_extensions_sidebar(s, k, pal),
        NavRailTab::Settings => col()
            .key("sidebar-settings-shortcut")
            .w_full()
            .h_full()
            .min_h(0.0)
            .padding(12.0)
            .gap(8.0)
            .child(text("Settings").font(k.ty.ui_medium).color(pal.text))
            .child(
                row()
                    .px(10.0)
                    .py(8.0)
                    .radius(k.radius.sm)
                    .fill(pal.raised)
                    .cursor(Cursor::Pointer)
                    .hover(pal.overlay)
                    .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::OpenOverlay(OverlayKind::Settings), None))
                    .child(text("Open Settings Modal").font(k.ty.small_medium).color(pal.text)),
            ),
    };

    row()
        .key("sidebar")
        .w_full()
        .h_full()
        .min_h(0.0)
        .fill(pal.sidebar)
        .child(build_nav_rail(s, k, pal))
        .child(
            col()
                .key("sidebar-pane-body")
                .grow(1.0)
                .min_w(0.0)
                .h_full()
                .min_h(0.0)
                .child(body),
        )
}
