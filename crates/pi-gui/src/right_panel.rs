//! Collapsible 460px right panel (`Changes`, `Files`, `Terminal`, `Tree`) matching Waku's header and body geometry.

use rune::{Color, Cursor, Cx, El, Icon, ScrollSpec, TextStyle, col, el, icon, row, text};
use rune_kit::Kit;

use crate::diff_view;
use crate::file_view;
use crate::state::{InspectorTab, PiGuiAction, PiGuiState};
use crate::terminal_view;
use crate::theme::{LayoutMetrics, WakuPalette, WakuRadius};

static ICON_PLUS: Icon = Icon {
    name: "waku-rp-plus",
    view: 16.0,
    body: r#"<path d="M8 3.2v9.6M3.2 8h9.6" stroke="currentColor" stroke-width="1.35" stroke-linecap="round"/>"#,
};

static ICON_PANEL_RIGHT: Icon = Icon {
    name: "waku-panel-right",
    view: 16.0,
    body: r#"<rect x="2.2" y="2.8" width="11.6" height="10.4" rx="2" stroke="currentColor" stroke-width="1.25" fill="none"/><path d="M9.8 2.8v10.4" stroke="currentColor" stroke-width="1.25"/>"#,
};

static ICON_CLOSE_TAB: Icon = Icon {
    name: "waku-rp-close",
    view: 24.0,
    body: r#"<path d="m6 6 12 12M18 6 6 18" stroke="currentColor" stroke-width="2" stroke-linecap="round" fill="none"/>"#,
};

static ICON_TAB_CHANGES: Icon = Icon {
    name: "waku-tab-changes",
    view: 16.0,
    body: r#"<path d="M3.2 2.8h9.6a1 1 0 0 1 1 1v8.4a1 1 0 0 1-1 1H3.2a1 1 0 0 1-1-1V3.8a1 1 0 0 1 1-1Z" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M8 5.2v5.6M5.2 8h5.6" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};

static ICON_TAB_FILES: Icon = Icon {
    name: "waku-tab-files",
    view: 16.0,
    body: r#"<path d="M2.4 4.2a1.2 1.2 0 0 1 1.2-1.2h3l1.1 1.3h4.7a1.2 1.2 0 0 1 1.2 1.2v6.3a1.2 1.2 0 0 1-1.2 1.2H3.6a1.2 1.2 0 0 1-1.2-1.2V4.2Z" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linejoin="round"/>"#,
};

static ICON_TAB_TERMINAL: Icon = Icon {
    name: "waku-tab-terminal",
    view: 16.0,
    body: r#"<rect x="2.2" y="2.8" width="11.6" height="10.4" rx="2" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M4.8 6.2L6.8 8l-2 1.8M8 9.8h3" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_TAB_TREE: Icon = Icon {
    name: "waku-tab-tree",
    view: 16.0,
    body: r#"<circle cx="4.5" cy="4" r="1.4" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="11.5" cy="8" r="1.4" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="11.5" cy="12" r="1.4" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M4.5 5.4V12h5.6M4.5 8h5.6" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linecap="round"/>"#,
};

fn tab_icon(tab: InspectorTab) -> &'static Icon {
    match tab {
        InspectorTab::Changes => &ICON_TAB_CHANGES,
        InspectorTab::Files => &ICON_TAB_FILES,
        InspectorTab::Terminal => &ICON_TAB_TERMINAL,
        InspectorTab::Tree => &ICON_TAB_TREE,
    }
}

fn tree_node_row(
    key: String,
    depth: usize,
    badge: &str,
    badge_color: Color,
    summary: &str,
    trailing: Option<String>,
    is_leaf: bool,
    pal: &WakuPalette,
) -> El {
    let mut guide = row().h_full().center_y();
    for level in 0..depth {
        let is_branch_level = level + 1 == depth;
        let line_color = if is_leaf && is_branch_level {
            pal.accent
        } else {
            pal.border_strong
        };
        guide = guide.child(
            row()
                .w(16.0)
                .h(24.0)
                .center_y()
                .child(el().w(1.0).h_full().ml(6.0).fill(line_color))
                .child(if is_branch_level {
                    el().w(8.0).h(1.0).fill(line_color)
                } else {
                    el()
                }),
        );
    }

    let mut content = row()
        .grow(1.0)
        .min_w(0.0)
        .px(10.0)
        .py(6.0)
        .gap(8.0)
        .center_y()
        .radius(WakuRadius::SM)
        .fill(if is_leaf { pal.raised } else { pal.inset })
        .border_layout()
        .border(1.0, if is_leaf { pal.border_strong } else { pal.border })
        .child(
            text(badge)
                .font(TextStyle::new(11.0, 600.0).mono())
                .color(badge_color)
                .shrink(),
        )
        .child(
            text(crate::models::truncate_preview(summary, 36))
                .font(TextStyle::new(12.5, if depth == 0 { 500.0 } else { 400.0 }))
                .color(if depth == 0 || is_leaf {
                    pal.text
                } else {
                    pal.text_secondary
                })
                .truncate()
                .grow(1.0)
                .min_w(0.0),
        );

    if let Some(meta) = trailing {
        content = content.child(
            text(meta)
                .font(TextStyle::new(11.0, 400.0).mono())
                .color(pal.text_tertiary)
                .shrink(),
        );
    }

    row()
        .key(key)
        .w_full()
        .min_w(0.0)
        .gap(4.0)
        .center_y()
        .child(guide)
        .child(content)
}

fn render_session_tree_tab(s: &PiGuiState, pal: &WakuPalette) -> El {
    let mut list = col()
        .key("right_panel.tree.body")
        .role("list")
        .grow(1.0)
        .w_full()
        .min_h(0.0)
        .padding(12.0)
        .gap(4.0)
        .scroll(ScrollSpec { stick_bottom: false });

    if s.messages.is_empty() {
        list = list.child(
            col()
                .w_full()
                .padding(12.0)
                .radius(WakuRadius::SM)
                .fill(pal.inset)
                .border_layout()
                .border(1.0, pal.border)
                .child(
                    text("No session turns recorded yet")
                        .font(TextStyle::new(12.5, 400.0))
                        .color(pal.text_tertiary),
                ),
        );
        return list;
    }

    let root_title = s
        .active_thread_id
        .as_deref()
        .and_then(|id| s.threads.iter().find(|t| t.id == id))
        .map_or("Active session", |t| t.title.as_str());
    list = list.child(tree_node_row(
        "tree-root".to_string(),
        0,
        "branch",
        pal.accent,
        root_title,
        Some(s.selected_branch.clone()),
        false,
        pal,
    ));

    let total_msgs = s.messages.len();
    for (idx, msg) in s.messages.iter().enumerate() {
        let is_last_msg = idx + 1 == total_msgs;
        match msg.role {
            crate::state::MessageRoleView::User => {
                let preview = msg
                    .blocks
                    .iter()
                    .find_map(|b| match b {
                        crate::state::ContentBlockView::Text(t) if !t.trim().is_empty() => Some(t.as_str()),
                        _ => None,
                    })
                    .unwrap_or("(prompt)");
                list = list.child(tree_node_row(
                    format!("tree-entry-{idx}"),
                    1,
                    "user",
                    pal.text_secondary,
                    preview,
                    Some(format!("#{}", idx + 1)),
                    is_last_msg && s.active_stream.is_none(),
                    pal,
                ));
            }
            crate::state::MessageRoleView::Assistant => {
                let model_meta = msg.model_label.clone();
                let turn_summary = msg
                    .blocks
                    .iter()
                    .find_map(|b| match b {
                        crate::state::ContentBlockView::Text(t) if !t.trim().is_empty() => Some(t.as_str()),
                        crate::state::ContentBlockView::Thinking(th) if !th.trim().is_empty() => Some(th.as_str()),
                        crate::state::ContentBlockView::ToolCall(tc) => Some(tc.name.as_str()),
                        _ => None,
                    })
                    .unwrap_or("assistant turn");
                let has_nested = !msg.blocks.is_empty();
                list = list.child(tree_node_row(
                    format!("tree-entry-{idx}"),
                    1,
                    "assistant",
                    pal.accent,
                    turn_summary,
                    model_meta,
                    is_last_msg && !has_nested && s.active_stream.is_none(),
                    pal,
                ));

                let block_len = msg.blocks.len();
                for (bidx, block) in msg.blocks.iter().enumerate() {
                    let is_leaf = is_last_msg && bidx + 1 == block_len && s.active_stream.is_none();
                    match block {
                        crate::state::ContentBlockView::Thinking(th) => {
                            list = list.child(tree_node_row(
                                format!("tree-entry-{idx}-think-{bidx}"),
                                2,
                                "think",
                                pal.text_tertiary,
                                th,
                                None,
                                is_leaf,
                                pal,
                            ));
                        }
                        crate::state::ContentBlockView::ToolCall(tc) => {
                            let ms = tc.duration_ms.map(|d| format!("{d}ms"));
                            list = list.child(tree_node_row(
                                format!("tree-entry-{idx}-tool-{bidx}"),
                                2,
                                &format!("tool:{}", tc.name),
                                pal.success,
                                &tc.arguments_preview,
                                ms,
                                is_leaf,
                                pal,
                            ));
                        }
                        crate::state::ContentBlockView::Text(body) => {
                            list = list.child(tree_node_row(
                                format!("tree-entry-{idx}-text-{bidx}"),
                                2,
                                if is_leaf { "leaf" } else { "text" },
                                if is_leaf { pal.gauge } else { pal.text_secondary },
                                body,
                                if is_leaf { Some("HEAD".to_string()) } else { None },
                                is_leaf,
                                pal,
                            ));
                        }
                        crate::state::ContentBlockView::Image { label, .. } => {
                            list = list.child(tree_node_row(
                                format!("tree-entry-{idx}-img-{bidx}"),
                                2,
                                "image",
                                pal.text_tertiary,
                                label,
                                None,
                                is_leaf,
                                pal,
                            ));
                        }
                    }
                }
            }
            crate::state::MessageRoleView::System | crate::state::MessageRoleView::ToolResult => {
                let preview = msg
                    .blocks
                    .iter()
                    .find_map(|b| match b {
                        crate::state::ContentBlockView::Text(t) => Some(t.as_str()),
                        _ => None,
                    })
                    .unwrap_or("(system)");
                list = list.child(tree_node_row(
                    format!("tree-entry-{idx}"),
                    1,
                    "system",
                    pal.text_tertiary,
                    preview,
                    None,
                    is_last_msg,
                    pal,
                ));
            }
        }
    }

    list
}

/// Renders the 460px collapsible right inspector panel (`right_panel`).
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, cx: &mut Cx<PiGuiState>) -> El {
    let width = s.inspector_pane.width().clamp(
        LayoutMetrics::RIGHT_PANEL_MIN_WIDTH,
        LayoutMetrics::RIGHT_PANEL_MAX_WIDTH,
    );

    let mut tabs_row = row()
        .key("right_panel.tabs")
        .role("tablist")
        .grow(1.0)
        .min_w(0.0)
        .mr(12.0)
        .h(LayoutMetrics::RIGHT_PANEL_TAB_HEIGHT)
        .gap(4.0)
        .center_y()
        .clip_rounded(0.0);

    let tabs_to_show = if s.open_right_tabs.is_empty() {
        vec![s.inspector_tab]
    } else {
        s.open_right_tabs.clone()
    };

    for tab in tabs_to_show {
        let active = s.inspector_tab == tab;
        let tab_fill = if active { pal.overlay_strong } else { Color::TRANSPARENT };
        let label_color = if active { pal.text } else { pal.text_secondary };
        let activate_area = row()
            .grow(1.0)
            .min_w(0.0)
            .h_full()
            .gap(6.0)
            .center_y()
            .on_press(move |st: &mut PiGuiState| st.dispatch(PiGuiAction::SelectInspectorTab(tab), None))
            .child(icon(tab_icon(tab), 13.0).color(pal.text_secondary))
            .child(
                text(tab.label())
                    .font(TextStyle::new(12.5, if active { 500.0 } else { 400.0 }).lh(18.0))
                    .color(label_color)
                    .truncate()
                    .grow(1.0)
                    .min_w(0.0),
            );
        let close_btn = el()
            .key(format!("close-right-panel-tab-{}", tab.id()))
            .size(16.0, 16.0)
            .center()
            .radius(4.0)
            .hover(pal.overlay_strong)
            .cursor(Cursor::Pointer)
            .on_press(move |st: &mut PiGuiState| st.dispatch(PiGuiAction::CloseInspectorTab(tab), None))
            .child(icon(&ICON_CLOSE_TAB, 10.0).color(pal.text_tertiary));
        let chip = row()
            .key(format!("right_panel.tab.{}", tab.id()))
            .role("tab")
            .label_static(tab.label())
            .min_w(100.0)
            .max_w(176.0)
            .h(LayoutMetrics::RIGHT_PANEL_TAB_HEIGHT)
            .px(8.0)
            .gap(6.0)
            .center_y()
            .shrink()
            .radius(6.0)
            .fill(tab_fill)
            .cursor(Cursor::Pointer)
            .hover(pal.overlay)
            .child(activate_area)
            .child(close_btn);
        tabs_row = tabs_row.child(chip);
    }

    let header = row()
        .key("right_panel.header")
        .role("region")
        .w_full()
        .h(LayoutMetrics::RIGHT_PANEL_HEADER_HEIGHT)
        .pl(10.0)
        .pr(14.0)
        .gap(6.0)
        .justify_between()
        .center_y()
        .shrink()
        .child(tabs_row)
        .child(
            el().key("right_panel.add_surface")
                .role("button")
                .label_static("Add Right Panel Surface")
                .size(26.0, 26.0)
                .radius(WakuRadius::SM)
                .center()
                .cursor(Cursor::Pointer)
                .hover(pal.overlay)
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::AddNextInspectorTab, None))
                .child(icon(&ICON_PLUS, 14.0).color(pal.text_secondary)),
        )
        .child(
            el().key("right_panel.toggle")
                .role("button")
                .label_static("Toggle Right Panel")
                .size(26.0, 26.0)
                .radius(WakuRadius::SM)
                .center()
                .cursor(Cursor::Pointer)
                .hover(pal.overlay)
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::ToggleInspector, None))
                .child(icon(&ICON_PANEL_RIGHT, 14.0).color(pal.text_secondary)),
        );

    let body = match s.inspector_tab {
        InspectorTab::Changes => col()
            .key("right_panel.body")
            .role("region")
            .grow(1.0)
            .min_h(0.0)
            .w_full()
            .clip_rounded(0.0)
            .child(diff_view::view(s, k, pal, cx)),
        InspectorTab::Files => row()
            .key("right_panel.body")
            .role("region")
            .grow(1.0)
            .min_h(0.0)
            .w_full()
            .clip_rounded(0.0)
            .child(file_view::view(s, k, pal, cx)),
        InspectorTab::Terminal => col()
            .key("right_panel.body")
            .role("region")
            .grow(1.0)
            .min_h(0.0)
            .w_full()
            .clip_rounded(0.0)
            .child(terminal_view::view(s, k, pal, cx)),
        InspectorTab::Tree => col()
            .key("right_panel.body")
            .role("region")
            .grow(1.0)
            .min_h(0.0)
            .w_full()
            .clip_rounded(0.0)
            .child(render_session_tree_tab(s, pal)),
    };

    col()
        .key("right_panel")
        .role("region")
        .w(width)
        .h_full()
        .shrink()
        .fill(pal.canvas)
        .border_layout()
        .border_sides([0.0, 0.0, 0.0, 1.0], pal.border)
        .clip_rounded(0.0)
        .child(header)
        .child(body)
}
