//! Trailing Waku inspector shell hosting Diff, Files, Editor, Terminal, Tools, and Plugins tabs.

use rune::{Align, Cx, El, ScrollSpec, col, el, icon, row, text};
use rune_kit::{
    Badge, BadgeVariant, Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, Switch, glyphs,
};

use crate::diff_view;
use crate::file_view;
use crate::models::{PluginKind, PluginOrigin, PluginViewModel};
use crate::state::{InspectorTab, PiGuiAction, PiGuiState};
use crate::terminal_view;
use crate::theme::WakuPalette;

/// Renders the 380-460pt Waku right inspector header strip and delegates to the active tab view.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, cx: &mut Cx<PiGuiState>) -> El {
    let header = render_header(s, k, pal);
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
        .border_sides([0.0, 0.0, 0.0, 1.0], pal.border)
        .child(header)
        .child(body)
}

fn tab_badge(s: &PiGuiState, tab: InspectorTab) -> Option<String> {
    match tab {
        InspectorTab::Diff if !s.diff.files.is_empty() => Some(s.diff.files.len().to_string()),
        InspectorTab::Files if !s.files.is_empty() => Some(s.files.len().to_string()),
        InspectorTab::File if s.open_file.is_some() => Some("1".to_string()),
        InspectorTab::Terminal if s.terminal.running => Some("live".to_string()),
        InspectorTab::Tools => {
            let count = s.plugins.iter().map(|p| p.tool_count).sum::<usize>();
            (count > 0).then(|| count.to_string())
        }
        InspectorTab::Plugins if !s.plugins.is_empty() => Some(s.plugins.len().to_string()),
        _ => None,
    }
}

fn render_header(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let mut tabs = row()
        .key("inspector-tabs")
        .grow(1.0)
        .min_w(0.0)
        .gap(4.0)
        .center_y()
        .flex_wrap();

    for tab in InspectorTab::ALL {
        let active = s.inspector_tab == tab;
        let badge = tab_badge(s, tab);
        let mut pill = row()
            .key(format!("inspector-tab-{}", tab.label()))
            .role("tab")
            .checked(active)
            .focusable(true)
            .px(8.0)
            .py(4.0)
            .gap(5.0)
            .center_y()
            .radius(k.radius.sm)
            .fill(if active { pal.raised } else { rune::Color::TRANSPARENT })
            .border(
                1.0,
                if active {
                    pal.border_strong
                } else {
                    rune::Color::TRANSPARENT
                },
            )
            .hover(pal.overlay)
            .on_press(move |st: &mut PiGuiState| st.dispatch(PiGuiAction::SelectInspectorTab(tab), None))
            .child(
                text(tab.label())
                    .font(if active { k.ty.small_medium } else { k.ty.small })
                    .color(if active { pal.text } else { pal.text_secondary }),
            );
        if let Some(count) = badge {
            pill = pill.child(
                Badge::new(count)
                    .variant(if active {
                        BadgeVariant::Accent
                    } else {
                        BadgeVariant::Subtle
                    })
                    .build(k),
            );
        }
        tabs = tabs.child(pill);
    }

    let collapse_btn = IconButton::new(&glyphs::CLOSE)
        .variant(IconButtonVariant::Ghost)
        .control_size(ControlSize::Small)
        .label("Collapse Inspector")
        .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::ToggleInspector, None))
        .build(k)
        .key("inspector-collapse-btn");

    row()
        .key("inspector-header")
        .w_full()
        .px(10.0)
        .py(6.0)
        .gap(8.0)
        .items_start()
        .border_sides([0.0, 0.0, 1.0, 0.0], pal.border)
        .child(tabs)
        .child(collapse_btn)
}

fn plugin_kind_label(kind: PluginKind) -> &'static str {
    match kind {
        PluginKind::Builtin => "Builtin",
        PluginKind::QuickJs => "QuickJS",
        PluginKind::Mcp => "MCP",
        PluginKind::Skill => "Skill",
    }
}

fn plugin_origin_label(origin: PluginOrigin) -> &'static str {
    match origin {
        PluginOrigin::Builtin => "builtin",
        PluginOrigin::Global => "global",
        PluginOrigin::Project => "project",
        PluginOrigin::Inline => "inline",
    }
}

fn render_tools_panel(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let tool_count = s.plugins.iter().map(|p| p.tool_count).sum::<usize>();
    let cmd_count = s.plugins.iter().map(|p| p.command_count).sum::<usize>();

    let mut body = col()
        .key("inspector-tools")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .padding(12.0)
        .gap(10.0)
        .scroll(ScrollSpec { stick_bottom: false })
        .child(
            row()
                .w_full()
                .justify_between()
                .center_y()
                .child(
                    col()
                        .gap(2.0)
                        .child(text("Workspace Tools").font(k.ty.ui_medium).color(pal.text))
                        .child(
                            text(format!("{tool_count} tools · {cmd_count} slash commands active"))
                                .font(k.ty.small)
                                .color(pal.text_secondary),
                        ),
                )
                .child(
                    Button::new("Reload")
                        .variant(ButtonVariant::Outline)
                        .control_size(ControlSize::Small)
                        .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::ReloadPlugins, None))
                        .build(k)
                        .key("tools-reload-btn"),
                ),
        );

    let active_sources: Vec<&PluginViewModel> = s
        .plugins
        .iter()
        .filter(|p| p.tool_count > 0 || p.command_count > 0)
        .collect();
    if active_sources.is_empty() {
        body = body.child(
            col()
                .w_full()
                .padding(12.0)
                .gap(4.0)
                .radius(k.radius.sm)
                .fill(pal.inset)
                .border(1.0, pal.border)
                .child(
                    text("Built-in coding tools ready")
                        .font(k.ty.ui)
                        .color(pal.text_secondary),
                )
                .child(
                    text("Load QuickJS (.ts/.js/.mjs) or MCP extensions to inspect registered tools here.")
                        .font(k.ty.small)
                        .color(pal.text_tertiary)
                        .wrap(),
                ),
        );
    } else {
        for plugin in active_sources {
            body = body.child(
                row()
                    .key(format!("tool-src-{}", plugin.id))
                    .w_full()
                    .px(10.0)
                    .py(8.0)
                    .gap(8.0)
                    .justify_between()
                    .center_y()
                    .radius(k.radius.sm)
                    .fill(pal.raised)
                    .border(1.0, pal.border)
                    .child(
                        col()
                            .grow(1.0)
                            .min_w(0.0)
                            .gap(2.0)
                            .child(text(&plugin.name).font(k.ty.ui_medium).color(pal.text).truncate())
                            .child(
                                text(format!(
                                    "{} · {}",
                                    plugin_kind_label(plugin.kind),
                                    plugin_origin_label(plugin.origin)
                                ))
                                .font(k.ty.small)
                                .color(pal.text_tertiary),
                            ),
                    )
                    .child(
                        row()
                            .gap(6.0)
                            .center_y()
                            .child(
                                Badge::new(format!("{} tools", plugin.tool_count))
                                    .variant(BadgeVariant::Neutral)
                                    .build(k),
                            )
                            .child(
                                Badge::new(format!("{} cmds", plugin.command_count))
                                    .variant(BadgeVariant::Subtle)
                                    .build(k),
                            ),
                    ),
            );
        }
    }
    body
}

fn render_plugins_panel(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let qjs_count = s.plugins.iter().filter(|p| p.kind == PluginKind::QuickJs).count();
    let mcp_count = s.plugins.iter().filter(|p| p.kind == PluginKind::Mcp).count();

    let mut panel = col()
        .key("inspector-plugins")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .padding(12.0)
        .gap(10.0)
        .scroll(ScrollSpec { stick_bottom: false })
        .child(
            row()
                .w_full()
                .justify_between()
                .center_y()
                .child(
                    col()
                        .gap(2.0)
                        .child(text("Extensions & Plugins").font(k.ty.ui_medium).color(pal.text))
                        .child(
                            text(format!(
                                "{} total · {qjs_count} QuickJS · {mcp_count} MCP",
                                s.plugins.len()
                            ))
                            .font(k.ty.small)
                            .color(pal.text_secondary),
                        ),
                )
                .child(
                    Button::new("Reload")
                        .variant(ButtonVariant::Outline)
                        .control_size(ControlSize::Small)
                        .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::ReloadPlugins, None))
                        .build(k)
                        .key("plugins-reload-btn"),
                ),
        );

    if s.plugins.is_empty() {
        return panel.child(
            col()
                .w_full()
                .padding(14.0)
                .gap(4.0)
                .align(Align::Start)
                .radius(k.radius.sm)
                .fill(pal.inset)
                .border(1.0, pal.border)
                .child(text("No extensions or MCP plugins loaded").font(k.ty.ui).color(pal.text_secondary))
                .child(
                    text("Add QuickJS TypeScript/ESM extensions under .pi/extensions or configure MCP servers in Settings.")
                        .font(k.ty.small)
                        .color(pal.text_tertiary)
                        .wrap(),
                ),
        );
    }

    for plugin in &s.plugins {
        let plugin_id = plugin.id.clone();
        let next_enabled = !plugin.enabled;
        let has_err = plugin.error.is_some();

        let mut card = col()
            .key(format!("plugin-{}", plugin.id))
            .w_full()
            .px(10.0)
            .py(8.0)
            .gap(6.0)
            .radius(k.radius.sm)
            .fill(pal.raised)
            .border(1.0, if has_err { pal.danger } else { pal.border })
            .child(
                row()
                    .w_full()
                    .justify_between()
                    .center_y()
                    .gap(8.0)
                    .child(
                        row()
                            .grow(1.0)
                            .min_w(0.0)
                            .gap(6.0)
                            .center_y()
                            .child(icon(&glyphs::SPARKLE, 14.0).color(if has_err {
                                pal.danger
                            } else if plugin.enabled {
                                pal.accent
                            } else {
                                pal.text_tertiary
                            }))
                            .child(text(&plugin.name).font(k.ty.ui_medium).color(pal.text).truncate())
                            .child(
                                Badge::new(plugin_kind_label(plugin.kind))
                                    .variant(if plugin.kind == PluginKind::QuickJs {
                                        BadgeVariant::Accent
                                    } else {
                                        BadgeVariant::Subtle
                                    })
                                    .build(k),
                            )
                            .child(
                                Badge::new(plugin_origin_label(plugin.origin))
                                    .variant(BadgeVariant::Neutral)
                                    .build(k),
                            ),
                    )
                    .child(
                        Switch::new(plugin.enabled)
                            .web()
                            .label(&plugin.name)
                            .hide_label()
                            .on_toggle(move |st: &mut PiGuiState| {
                                st.dispatch(
                                    PiGuiAction::TogglePlugin {
                                        id: plugin_id.clone(),
                                        enabled: next_enabled,
                                    },
                                    None,
                                );
                            })
                            .build(k)
                            .key(format!("plugin-toggle-{}", plugin.id)),
                    ),
            );

        if !plugin.description.is_empty() {
            card = card.child(
                text(&plugin.description)
                    .font(k.ty.small)
                    .color(pal.text_secondary)
                    .wrap(),
            );
        }

        card = card.child(
            row()
                .w_full()
                .justify_between()
                .center_y()
                .child(
                    text(format!(
                        "{} tools · {} commands",
                        plugin.tool_count, plugin.command_count
                    ))
                    .font(k.ty.small)
                    .color(pal.text_tertiary),
                )
                .child(
                    text(if has_err {
                        "Load error"
                    } else if plugin.enabled {
                        "Enabled"
                    } else {
                        "Disabled"
                    })
                    .font(k.ty.small_medium)
                    .color(if has_err {
                        pal.danger
                    } else if plugin.enabled {
                        pal.success
                    } else {
                        pal.text_tertiary
                    }),
                ),
        );

        if let Some(err) = &plugin.error {
            card = card.child(
                el().w_full()
                    .px(8.0)
                    .py(6.0)
                    .radius(k.radius.xs)
                    .fill(pal.danger_soft)
                    .child(text(err).font(k.ty.small).color(pal.danger).wrap()),
            );
        }

        panel = panel.child(card);
    }

    panel
}
