//! Trailing inspector panel hosting Diff, Files, Editor, Terminal, Tools, and Plugins tabs.

use rune::{Cx, El, col, el, row, text};
use rune_kit::Kit;

use crate::diff_view;
use crate::file_view;
use crate::state::{InspectorTab, PiGuiAction, PiGuiState};
use crate::terminal_view;
use crate::theme::WakuPalette;

/// Renders the inspector tab strip and delegates content to the active inspector view module.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, cx: &mut Cx<PiGuiState>) -> El {
    let mut tab_strip = row()
        .key("inspector-tabs")
        .w_full()
        .gap(4.0)
        .px(8.0)
        .py(6.0)
        .border_sides([0.0, 0.0, 1.0, 0.0], pal.border);

    for tab in InspectorTab::ALL {
        let active = s.inspector_tab == tab;
        tab_strip = tab_strip.child(
            el().key(format!("inspector-tab-{}", tab.label()))
                .px(8.0)
                .py(4.0)
                .radius(k.radius.sm)
                .fill(if active { pal.raised } else { rune::Color::TRANSPARENT })
                .hover(pal.overlay)
                .on_press(move |st: &mut PiGuiState| st.dispatch(PiGuiAction::SelectInspectorTab(tab), None))
                .child(text(tab.label()).font(k.ty.small_medium).color(if active {
                    pal.text
                } else {
                    pal.text_secondary
                })),
        );
    }

    let body = match s.inspector_tab {
        InspectorTab::Diff => diff_view::view(s, k, pal, cx),
        InspectorTab::Files | InspectorTab::File => file_view::view(s, k, pal, cx),
        InspectorTab::Terminal => terminal_view::view(s, k, pal, cx),
        InspectorTab::Tools => render_tools_panel(s, k, pal),
        InspectorTab::Plugins => render_plugins_panel(s, k, pal),
    };

    col()
        .key("right-panel")
        .w_full()
        .h_full()
        .min_h(0.0)
        .fill(pal.sidebar)
        .child(tab_strip)
        .child(body)
}

fn render_tools_panel(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let tool_count = s.plugins.iter().map(|p| p.tool_count).sum::<usize>();
    col()
        .key("inspector-tools")
        .grow(1.0)
        .min_h(0.0)
        .padding(12.0)
        .gap(8.0)
        .child(text("Workspace Tools").font(k.ty.ui_medium).color(pal.text))
        .child(
            text(format!("{tool_count} extension tools active"))
                .font(k.ty.small)
                .color(pal.text_secondary),
        )
}

fn render_plugins_panel(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let mut panel = col()
        .key("inspector-plugins")
        .grow(1.0)
        .min_h(0.0)
        .padding(12.0)
        .gap(8.0)
        .child(text("Extensions & Plugins").font(k.ty.ui_medium).color(pal.text));

    if s.plugins.is_empty() {
        panel = panel.child(
            text("No extensions or MCP plugins loaded")
                .font(k.ty.small)
                .color(pal.text_tertiary),
        );
    } else {
        for plugin in &s.plugins {
            panel = panel.child(
                row()
                    .key(format!("plugin-{}", plugin.id))
                    .w_full()
                    .justify_between()
                    .px(8.0)
                    .py(6.0)
                    .radius(k.radius.sm)
                    .fill(pal.raised)
                    .child(text(&plugin.name).font(k.ty.ui).color(pal.text))
                    .child(
                        text(if plugin.enabled { "Enabled" } else { "Disabled" })
                            .font(k.ty.small)
                            .color(if plugin.enabled { pal.success } else { pal.text_tertiary }),
                    ),
            );
        }
    }
    panel
}
