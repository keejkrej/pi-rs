//! Waku-aligned 252px left session sidebar with borderless header, grouping controls, and settings footer.

use chrono::{DateTime, Local, Utc};
use indexmap::IndexMap;
use rune::{Color, Cursor, Cx, El, Icon, ScrollSpec, TextStyle, col, el, icon, row, text, text_input};
use rune_kit::{Kit, scroll_bar};

use crate::models::truncate_preview;
use crate::state::{
    PiGuiAction, PiGuiState, PopoverKind, SettingsPage, SidebarGrouping, SidebarOrdering, ThreadGroup, ThreadSummary,
};
use crate::theme::{LayoutMetrics, WakuPalette, WakuRadius};

static ICON_PANEL_LEFT: Icon = Icon {
    name: "waku-panel-left",
    view: 16.0,
    body: r#"<rect x="2.2" y="2.8" width="11.6" height="10.4" rx="2" stroke="currentColor" stroke-width="1.25" fill="none"/><path d="M6.2 2.8v10.4" stroke="currentColor" stroke-width="1.25"/>"#,
};

static ICON_ARROW_LEFT: Icon = Icon {
    name: "waku-arrow-left",
    view: 56.0,
    body: r#"<path fill="currentColor" d="M6.238 28c0 .586.258 1.125.727 1.57l15.562 15.54c.492.445.985.656 1.524.656c1.172 0 2.062-.844 2.062-2.016c0-.562-.21-1.125-.586-1.477l-5.226-5.343l-7.922-7.196l5.695.352H47.7c1.219 0 2.063-.867 2.063-2.086s-.844-2.086-2.063-2.086H18.074l-5.672.352l7.899-7.196l5.226-5.343c.399-.376.586-.915.586-1.477c0-1.172-.89-2.016-2.062-2.016c-.54 0-1.055.188-1.57.704L6.964 26.43c-.469.445-.727.984-.727 1.57"/>"#,
};

static ICON_ARROW_RIGHT: Icon = Icon {
    name: "waku-arrow-right",
    view: 56.0,
    body: r#"<path fill="currentColor" d="M49.762 28c0-.586-.234-1.125-.703-1.57L33.543 10.938c-.516-.516-1.031-.704-1.594-.704c-1.148 0-2.039.844-2.039 2.016c0 .563.188 1.101.563 1.477l5.25 5.343l7.898 7.196l-5.672-.352H8.301c-1.195 0-2.063.867-2.063 2.086s.867 2.086 2.063 2.086h29.648l5.696-.352l-7.922 7.196l-5.25 5.343c-.375.352-.563.914-.563 1.477c0 1.172.89 2.016 2.04 2.016c.562 0 1.054-.211 1.523-.657L49.059 29.57c.469-.445.703-.984.703-1.57"/>"#,
};

static ICON_SQUARE_PEN: Icon = Icon {
    name: "waku-square-pen",
    view: 16.0,
    body: r#"<path d="M7.5 3H4a1.5 1.5 0 0 0-1.5 1.5v7.5A1.5 1.5 0 0 0 4 13.5h7.5A1.5 1.5 0 0 0 13 12V8.5" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linecap="round"/><path d="M12.6 2.9a1.1 1.1 0 0 1 1.5 1.5L8.2 10.3l-2 .5.5-2 5.9-5.9Z" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linejoin="round"/>"#,
};

static ICON_SEARCH: Icon = Icon {
    name: "waku-search",
    view: 16.0,
    body: r#"<circle cx="7.2" cy="7.2" r="4.3" stroke="currentColor" stroke-width="1.25" fill="none"/><path d="M10.5 10.5l3 3" stroke="currentColor" stroke-width="1.25" stroke-linecap="round"/>"#,
};

static ICON_LIST_FILTER: Icon = Icon {
    name: "waku-list-filter",
    view: 16.0,
    body: r#"<path d="M2.5 4.5h11M4.5 8h7M6.5 11.5h3" stroke="currentColor" stroke-width="1.25" stroke-linecap="round"/>"#,
};

static ICON_SETTINGS: Icon = Icon {
    name: "waku-settings",
    view: 24.0,
    body: r#"<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.38a2 2 0 0 0-.73-2.73l-.15-.09a2 2 0 0 1-1-1.74v-.51a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><circle cx="12" cy="12" r="3" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_FOLDER_SMALL: Icon = Icon {
    name: "waku-sb-folder",
    view: 16.0,
    body: r#"<path d="M2.2 4.2a1.2 1.2 0 0 1 1.2-1.2h3.1l1.2 1.3h4.9a1.2 1.2 0 0 1 1.2 1.2v6.3a1.2 1.2 0 0 1-1.2 1.2H3.4a1.2 1.2 0 0 1-1.2-1.2V4.2Z" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linejoin="round"/>"#,
};

static ICON_BRANCH_SMALL: Icon = Icon {
    name: "waku-sb-branch",
    view: 16.0,
    body: r#"<circle cx="5" cy="4" r="1.4" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="5" cy="12" r="1.4" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="11" cy="6" r="1.4" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M5 5.4v5.2M11 7.4c0 2-1.5 3-6 3" stroke="currentColor" stroke-width="1.2" fill="none"/>"#,
};

static ICON_LOADER_CIRCLE: Icon = Icon {
    name: "waku-sb-busy",
    view: 24.0,
    body: r#"<path d="M21 12a9 9 0 1 1-6.219-8.56" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" fill="none"/>"#,
};

const DEMO_REFERENCE_NOW_MS: i64 = 1_763_370_960_000;

const GROUP_ORDER: [ThreadGroup; 4] = [
    ThreadGroup::Today,
    ThreadGroup::Yesterday,
    ThreadGroup::ThisWeek,
    ThreadGroup::Earlier,
];

/// Formats a millisecond timestamp as a compact sidebar recency label without I/O.
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
        ThreadGroup::ThisWeek => "This Week",
        ThreadGroup::Earlier => "Earlier",
    }
}

fn project_label(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("Workspace")
        .to_string()
}

fn header_icon_btn(
    key: &'static str,
    label: &'static str,
    ic: &'static Icon,
    pal: &WakuPalette,
    action: PiGuiAction,
) -> El {
    el().key(key)
        .role("button")
        .label_static(label)
        .size(26.0, 26.0)
        .radius(WakuRadius::SM)
        .center()
        .cursor(Cursor::Pointer)
        .hover(pal.overlay)
        .on_press(move |st: &mut PiGuiState| st.dispatch(action.clone(), None))
        .child(icon(ic, 14.0).color(pal.text_secondary))
}

fn render_group_header(key: String, title: &str, include_options: bool, pal: &WakuPalette, options_open: bool) -> El {
    let mut header = row()
        .key(key)
        .role("region")
        .label(title)
        .w_full()
        .h(LayoutMetrics::SIDEBAR_GROUP_HEADER_HEIGHT)
        .px(8.0)
        .justify_between()
        .center_y()
        .child(text(title).font(TextStyle::new(12.0, 500.0)).color(pal.text_tertiary));
    if include_options {
        let btn_fill = if options_open {
            pal.overlay_strong
        } else {
            Color::TRANSPARENT
        };
        header = header.child(
            el().key("workspace.sidebar.options")
                .role("button")
                .label_static("Sidebar options")
                .size(20.0, 20.0)
                .mr(14.0)
                .radius(WakuRadius::SM)
                .center()
                .fill(btn_fill)
                .cursor(Cursor::Pointer)
                .hover(pal.overlay)
                .on_press(|st: &mut PiGuiState| {
                    st.dispatch(PiGuiAction::TogglePopover(PopoverKind::SidebarOptions), None)
                })
                .child(icon(&ICON_LIST_FILTER, 14.0).color(pal.text_secondary)),
        );
    }
    header
}

fn format_relative_age(timestamp_ms: i64, now_ms: i64) -> String {
    let diff_secs = ((now_ms - timestamp_ms) / 1000).max(0);
    if diff_secs < 60 {
        "now".to_string()
    } else if diff_secs < 3600 {
        format!("{}m", diff_secs / 60)
    } else if diff_secs < 86_400 {
        format!("{}h", diff_secs / 3600)
    } else {
        format!("{}d", diff_secs / 86_400)
    }
}

fn render_session_row(
    idx: usize,
    thread: &ThreadSummary,
    is_active: bool,
    indented: bool,
    branch_label: &str,
    now_ms: i64,
    pal: &WakuPalette,
) -> El {
    let id = thread.id.clone();
    let title = truncate_preview(&thread.title, 34);
    let age = format_relative_age(thread.updated_at_ms, now_ms);
    let fill = if is_active {
        pal.sidebar_item_background
    } else {
        Color::TRANSPARENT
    };
    let pl = if indented { 28.0 } else { 8.0 };

    let mut top_row = row().w_full().center_y().gap(6.0).child(
        text(&title)
            .font(TextStyle::new(13.5, 400.0).lh(18.0))
            .color(pal.text)
            .truncate()
            .min_w(0.0)
            .grow(1.0),
    );
    if thread.is_streaming {
        top_row = top_row.child(icon(&ICON_LOADER_CIRCLE, 12.0).color(pal.accent).shrink());
    }

    let detail_icon = if indented {
        &ICON_BRANCH_SMALL
    } else {
        &ICON_FOLDER_SMALL
    };
    let detail_label = if indented {
        branch_label.to_string()
    } else {
        project_label(&thread.cwd)
    };
    let bottom_row = row()
        .w_full()
        .gap(5.0)
        .center_y()
        .child(icon(detail_icon, 12.0).color(pal.text_tertiary).shrink())
        .child(
            text(detail_label)
                .font(TextStyle::new(if indented { 12.0 } else { 12.5 }, 400.0))
                .color(pal.text_tertiary)
                .truncate()
                .grow(1.0)
                .min_w(0.0),
        )
        .child(
            text(age)
                .font(TextStyle::new(12.0, 400.0))
                .color(if thread.is_streaming {
                    pal.text_tertiary
                } else {
                    pal.text_ghost
                })
                .shrink(),
        );

    col()
        .key(if idx == 0 {
            "workspace.sidebar.session.0".to_string()
        } else {
            format!("sidebar-item-{idx}")
        })
        .role("button")
        .label(&thread.title)
        .w_full()
        .h(LayoutMetrics::SIDEBAR_SESSION_CARD_HEIGHT)
        .pl(pl)
        .pr(8.0)
        .py(7.0)
        .gap(4.0)
        .justify_center()
        .radius(WakuRadius::SM)
        .fill(fill)
        .hover(pal.overlay)
        .cursor(Cursor::Pointer)
        .clip_rounded(WakuRadius::SM)
        .on_press(move |st: &mut PiGuiState| st.dispatch(PiGuiAction::SelectThread(id.clone()), None))
        .child(top_row)
        .child(bottom_row)
}

/// Renders Waku's 252px sidebar column (`workspace.sidebar`).
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let width = s
        .sidebar_pane
        .width()
        .clamp(LayoutMetrics::SIDEBAR_MIN_WIDTH, LayoutMetrics::SIDEBAR_MAX_WIDTH);
    let query = s.search_edit.text.trim().to_ascii_lowercase();
    let mut visible: Vec<&ThreadSummary> = s
        .threads
        .iter()
        .filter(|t| query.is_empty() || t.title.to_ascii_lowercase().contains(&query))
        .collect();
    match s.sidebar_ordering {
        SidebarOrdering::Newest => visible.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms)),
        SidebarOrdering::Oldest => visible.sort_by(|a, b| a.updated_at_ms.cmp(&b.updated_at_ms)),
    }

    let now_ms = if s.demo_scene.is_some() {
        DEMO_REFERENCE_NOW_MS
    } else {
        Utc::now().timestamp_millis()
    };

    let options_open = s.popover == Some(PopoverKind::SidebarOptions);
    let mut list_col = col().w_full().px(10.0).gap(1.0);

    if visible.is_empty() {
        list_col = list_col.child(col().mb(1.0).child(render_group_header(
            "workspace.sidebar.group_header".to_string(),
            "Today",
            true,
            pal,
            options_open,
        )));
    } else {
        let mut session_idx = 0usize;
        let mut header_idx = 0usize;
        match s.sidebar_grouping {
            SidebarGrouping::Updated => {
                for grp in GROUP_ORDER {
                    let bucket: Vec<&ThreadSummary> = visible.iter().copied().filter(|t| t.group == grp).collect();
                    if bucket.is_empty() {
                        continue;
                    }
                    let hdr_key = if header_idx == 0 {
                        "workspace.sidebar.group_header".to_string()
                    } else {
                        format!("workspace.sidebar.group_header.{header_idx}")
                    };
                    list_col = list_col.child(col().mb(1.0).child(render_group_header(
                        hdr_key,
                        group_label(grp),
                        header_idx == 0,
                        pal,
                        options_open,
                    )));
                    header_idx += 1;
                    for t in bucket {
                        let active = s.active_thread_id.as_deref() == Some(t.id.as_str());
                        list_col = list_col.child(render_session_row(
                            session_idx,
                            t,
                            active,
                            false,
                            &s.selected_branch,
                            now_ms,
                            pal,
                        ));
                        session_idx += 1;
                    }
                }
            }
            SidebarGrouping::Project => {
                let mut by_project: IndexMap<String, Vec<&ThreadSummary>> = IndexMap::new();
                for t in &visible {
                    by_project.entry(project_label(&t.cwd)).or_default().push(t);
                }
                for (proj, bucket) in by_project {
                    let hdr_key = if header_idx == 0 {
                        "workspace.sidebar.group_header".to_string()
                    } else {
                        format!("workspace.sidebar.group_header.{header_idx}")
                    };
                    list_col = list_col.child(col().mb(1.0).child(render_group_header(
                        hdr_key,
                        &proj,
                        header_idx == 0,
                        pal,
                        options_open,
                    )));
                    header_idx += 1;
                    for t in bucket {
                        let active = s.active_thread_id.as_deref() == Some(t.id.as_str());
                        list_col = list_col.child(render_session_row(
                            session_idx,
                            t,
                            active,
                            true,
                            &s.selected_branch,
                            now_ms,
                            pal,
                        ));
                        session_idx += 1;
                    }
                }
            }
        }
    }

    let scroll_viewport = list_col.grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false });
    let scroll_area = scroll_bar(
        k,
        "workspace.sidebar.scroll_area",
        "workspace.sidebar.scroll",
        scroll_viewport,
    )
    .grow(1.0)
    .min_h(0.0)
    .w_full();

    let mut root = col()
        .key("workspace.sidebar")
        .role("navigation")
        .w(width)
        .h_full()
        .shrink()
        .fill(pal.sidebar)
        .clip_rounded(0.0)
        .child(
            row()
                .key("workspace.sidebar.header")
                .role("region")
                .w_full()
                .h(LayoutMetrics::SIDEBAR_HEADER_HEIGHT)
                .pl(LayoutMetrics::traffic_light_clearance())
                .center_y()
                .shrink()
                .child(header_icon_btn(
                    "workspace.sidebar.toggle",
                    "Toggle Sidebar",
                    &ICON_PANEL_LEFT,
                    pal,
                    PiGuiAction::ToggleSidebar,
                ))
                .child(
                    header_icon_btn(
                        "workspace.sidebar.nav_back",
                        "Previous Session",
                        &ICON_ARROW_LEFT,
                        pal,
                        PiGuiAction::NavigateSessionHistory(-1),
                    )
                    .ml(6.0),
                )
                .child(
                    header_icon_btn(
                        "workspace.sidebar.nav_forward",
                        "Next Session",
                        &ICON_ARROW_RIGHT,
                        pal,
                        PiGuiAction::NavigateSessionHistory(1),
                    )
                    .ml(2.0),
                ),
        )
        .child(
            row()
                .key("workspace.sidebar.new_task")
                .role("button")
                .label_static("New Task")
                .h(LayoutMetrics::SIDEBAR_ACTION_ROW_HEIGHT)
                .mx(10.0)
                .px(4.0)
                .gap(8.0)
                .center_y()
                .radius(WakuRadius::SM)
                .cursor(Cursor::Pointer)
                .hover(pal.overlay)
                .shrink()
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::NewThread, None))
                .child(icon(&ICON_SQUARE_PEN, 14.0).color(pal.text_secondary))
                .child(
                    text("New Task")
                        .font(TextStyle::new(13.0, 500.0).lh(18.0))
                        .color(pal.text),
                ),
        )
        .child(
            row()
                .key("workspace.sidebar.search")
                .role("button")
                .label_static("Search")
                .h(LayoutMetrics::SIDEBAR_ACTION_ROW_HEIGHT)
                .mx(10.0)
                .mb(10.0)
                .px(4.0)
                .gap(8.0)
                .center_y()
                .radius(WakuRadius::SM)
                .cursor(Cursor::Pointer)
                .hover(pal.overlay)
                .shrink()
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::ToggleSidebarSearch, None))
                .child(icon(&ICON_SEARCH, 14.0).color(pal.text_secondary))
                .child(
                    text("Search")
                        .font(TextStyle::new(13.0, 500.0).lh(18.0))
                        .color(pal.text),
                ),
        );

    if s.sidebar_search_open {
        root = root.child(
            row()
                .key("workspace.sidebar.search_input_box")
                .mx(10.0)
                .mb(6.0)
                .px(8.0)
                .py(5.0)
                .radius(WakuRadius::SM)
                .fill(pal.inset)
                .border_layout()
                .border(1.0, pal.border)
                .child(
                    text_input(&s.search_edit, TextStyle::new(13.0, 400.0))
                        .key("workspace.sidebar.search_input")
                        .label("Filter tasks")
                        .w_full()
                        .color(pal.text)
                        .placeholder("Filter tasks…", pal.text_ghost)
                        .caret(pal.accent, pal.selection)
                        .on_edit(|st: &mut PiGuiState| &mut st.search_edit),
                ),
        );
    }

    root.child(scroll_area).child(
        row()
            .key("workspace.sidebar.footer")
            .role("region")
            .w_full()
            .h(LayoutMetrics::SIDEBAR_FOOTER_HEIGHT)
            .px(10.0)
            .center_y()
            .shrink()
            .child(header_icon_btn(
                "workspace.sidebar.settings",
                "Open Settings",
                &ICON_SETTINGS,
                pal,
                PiGuiAction::OpenSettings(SettingsPage::General),
            )),
    )
}
