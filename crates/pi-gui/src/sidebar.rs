//! Leading navigation rail and session/file/plugin sidebar view.

use chrono::{DateTime, Local, Utc};
use rune::{Cx, El, col, el, row, text};
use rune_kit::Kit;

use crate::state::{NavRailTab, PiGuiAction, PiGuiState, ThreadGroup};
use crate::theme::WakuPalette;

/// Formats a millisecond timestamp as a short sidebar time label without I/O.
#[must_use]
pub fn format_thread_time(timestamp_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
        .map(|dt| dt.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

/// Renders the sidebar pane: nav tabs, action header, and active tab list.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let mut tabs = row().key("sidebar-nav-tabs").w_full().gap(4.0).px(8.0).py(6.0);
    for tab in NavRailTab::ALL {
        let active = s.nav_tab == tab;
        tabs = tabs.child(
            el().key(format!("nav-tab-{}", tab.label()))
                .px(8.0)
                .py(4.0)
                .radius(k.radius.sm)
                .fill(if active {
                    pal.sidebar_item_background
                } else {
                    rune::Color::TRANSPARENT
                })
                .hover(pal.overlay)
                .on_press(move |st: &mut PiGuiState| st.dispatch(PiGuiAction::SelectNavTab(tab), None))
                .child(text(tab.label()).font(k.ty.small_medium).color(if active {
                    pal.text
                } else {
                    pal.text_secondary
                })),
        );
    }

    let mut body = col().key("sidebar-body").grow(1.0).min_h(0.0).px(8.0).gap(4.0);
    match s.nav_tab {
        NavRailTab::Sessions => {
            body = body.child(
                el().key("new-thread-btn")
                    .px(10.0)
                    .py(6.0)
                    .radius(k.radius.sm)
                    .fill(pal.raised)
                    .hover(pal.overlay_strong)
                    .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::NewThread, None))
                    .child(text("New Thread").font(k.ty.ui_medium).color(pal.text)),
            );
            if s.threads.is_empty() {
                body = body.child(
                    text("No saved sessions yet")
                        .key("sidebar-empty")
                        .font(k.ty.small)
                        .color(pal.text_tertiary)
                        .px(8.0)
                        .py(8.0),
                );
            } else {
                for thread in &s.threads {
                    let selected = s.active_thread_id.as_deref() == Some(thread.id.as_str());
                    let thread_id = thread.id.clone();
                    let group_label = match thread.group {
                        ThreadGroup::Today => "Today",
                        ThreadGroup::Yesterday => "Yesterday",
                        ThreadGroup::ThisWeek => "This week",
                        ThreadGroup::Earlier => "Earlier",
                    };
                    let time_text = format_thread_time(thread.updated_at_ms);
                    body = body.child(
                        row()
                            .key(format!("thread-{}", thread.id))
                            .w_full()
                            .px(8.0)
                            .py(6.0)
                            .radius(k.radius.sm)
                            .justify_between()
                            .fill(if selected {
                                pal.sidebar_item_background
                            } else {
                                rune::Color::TRANSPARENT
                            })
                            .hover(pal.overlay)
                            .on_press(move |st: &mut PiGuiState| {
                                st.dispatch(PiGuiAction::SelectThread(thread_id.clone()), None);
                            })
                            .child(text(&thread.title).font(k.ty.ui).color(pal.text).truncate())
                            .child(
                                text(if time_text.is_empty() {
                                    group_label.to_string()
                                } else {
                                    time_text
                                })
                                .font(k.ty.small)
                                .color(pal.text_tertiary),
                            ),
                    );
                }
            }
        }
        NavRailTab::Files => {
            body = body.child(
                text(format!("{} files indexed", s.files.len()))
                    .font(k.ty.small)
                    .color(pal.text_secondary)
                    .px(8.0)
                    .py(6.0),
            );
        }
        NavRailTab::Extensions => {
            body = body.child(
                text(format!("{} plugins loaded", s.plugins.len()))
                    .font(k.ty.small)
                    .color(pal.text_secondary)
                    .px(8.0)
                    .py(6.0),
            );
        }
        NavRailTab::Settings => {
            body = body.child(
                text("Settings overlay active")
                    .font(k.ty.small)
                    .color(pal.text_secondary)
                    .px(8.0)
                    .py(6.0),
            );
        }
    }

    col()
        .key("sidebar")
        .w_full()
        .h_full()
        .min_h(0.0)
        .fill(pal.sidebar)
        .child(tabs)
        .child(body)
}
