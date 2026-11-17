//! Waku Settings modal (`Cmd+,`) with Appearance, Providers & Auth, QuickJS/Native Plugins, and MCP sections.

use rune::{Cx, El, ScrollSpec, col, el, row, text};
use rune_kit::{Badge, BadgeVariant, Button, ButtonVariant, ControlSize, Divider, Kit, SegmentedControl, Switch};

use crate::models::{PluginKind, PluginOrigin, PluginViewModel};
use crate::state::{PiGuiAction, PiGuiState};
use crate::theme::{ThemePreference, WakuPalette, palette};

/// Renders the Waku-styled Settings modal (`Appearance`, `Providers & Auth`, `Plugins`, `MCP`).
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let header = row()
        .key("settings-header")
        .w_full()
        .px(20.0)
        .py(14.0)
        .gap(12.0)
        .justify_between()
        .center_y()
        .border_sides([0.0, 0.0, 1.0, 0.0], pal.border)
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .gap(2.0)
                .child(text("Settings").font(k.ty.body_medium).color(pal.text))
                .child(
                    text(format!("pi {} · {}", *pi_coding_agent::config::VERSION, s.config.cwd))
                        .font(k.ty.small)
                        .color(pal.text_tertiary)
                        .truncate(),
                ),
        )
        .child(
            Button::new("Close")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::CloseOverlay, None))
                .build(k)
                .key("settings-close-btn"),
        );

    let scroll = col()
        .key("settings-scroll")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .padding(20.0)
        .gap(16.0)
        .scroll(ScrollSpec { stick_bottom: false })
        .child(render_appearance_section(s, k, pal))
        .child(render_providers_section(s, k, pal))
        .child(render_plugins_section(s, k, pal))
        .child(render_mcp_section(s, k, pal));

    col()
        .key("overlay-settings")
        .w(660.0)
        .h(540.0)
        .min_h(0.0)
        .radius(k.radius.lg)
        .fill(pal.raised)
        .border(1.0, pal.border_strong)
        .child(header)
        .child(scroll)
}

fn section_card(k: &Kit, pal: &WakuPalette, key: &str, title: &str, subtitle: &str, content: El) -> El {
    col()
        .key(key)
        .w_full()
        .gap(8.0)
        .child(
            row()
                .w_full()
                .justify_between()
                .center_y()
                .child(text(title).font(k.ty.ui_medium).color(pal.text))
                .child(text(subtitle).font(k.ty.small).color(pal.text_tertiary)),
        )
        .child(
            col()
                .w_full()
                .padding(12.0)
                .gap(10.0)
                .radius(k.radius.md)
                .fill(pal.surface)
                .border(1.0, pal.border)
                .child(content),
        )
}

fn render_appearance_section(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let selected_idx = ThemePreference::ALL
        .iter()
        .position(|p| *p == s.theme_preference)
        .unwrap_or(0);
    let segmented = SegmentedControl::new(&["System", "Light", "Dark"], Some(selected_idx))
        .label("Theme")
        .width(Some(210.0))
        .on_pick(|st: &mut PiGuiState, idx, tx| {
            if let Some(&pref) = ThemePreference::ALL.get(idx) {
                st.dispatch(PiGuiAction::SetThemePreference(pref), Some(tx));
            }
        })
        .build(k)
        .key("settings-theme-segmented");

    let dark_swatch = palette(true);
    let light_swatch = palette(false);
    let swatches = row()
        .key("settings-palette-swatches")
        .gap(8.0)
        .center_y()
        .child(render_preview_chip(
            k,
            pal,
            "Waku Dark",
            dark_swatch.canvas,
            dark_swatch.accent,
            s.theme_preference == ThemePreference::Dark,
            ThemePreference::Dark,
        ))
        .child(render_preview_chip(
            k,
            pal,
            "Waku Light",
            light_swatch.canvas,
            light_swatch.accent,
            s.theme_preference == ThemePreference::Light,
            ThemePreference::Light,
        ));

    let motion_row = row()
        .w_full()
        .justify_between()
        .center_y()
        .child(
            col()
                .gap(2.0)
                .child(text("Reduce motion").font(k.ty.ui).color(pal.text))
                .child(
                    text("Disable spring travel animations for split panes and overlays.")
                        .font(k.ty.small)
                        .color(pal.text_tertiary),
                ),
        )
        .child(
            Switch::new(s.reduced_motion)
                .web()
                .label("Reduce motion")
                .hide_label()
                .on_toggle(|st: &mut PiGuiState| {
                    st.reduced_motion = !st.reduced_motion;
                    st.settings_flags
                        .insert("reduced_motion".to_string(), st.reduced_motion);
                })
                .build(k)
                .key("settings-reduced-motion-toggle"),
        );

    let body = col()
        .w_full()
        .gap(10.0)
        .child(
            row()
                .key("settings-theme-row")
                .w_full()
                .justify_between()
                .center_y()
                .child(text("Theme preference").font(k.ty.ui).color(pal.text))
                .child(segmented),
        )
        .child(
            row()
                .w_full()
                .justify_between()
                .center_y()
                .child(text("Palette preview").font(k.ty.ui).color(pal.text_secondary))
                .child(swatches),
        )
        .child(Divider::horizontal().build(k))
        .child(motion_row);

    section_card(k, pal, "settings-appearance", "Appearance", "Waku Graphite", body)
}

fn render_preview_chip(
    k: &Kit,
    pal: &WakuPalette,
    label: &str,
    bg: rune::Color,
    accent: rune::Color,
    active: bool,
    pref: ThemePreference,
) -> El {
    row()
        .key(format!("theme-pref-{}", pref.label()))
        .px(8.0)
        .py(5.0)
        .gap(6.0)
        .center_y()
        .radius(k.radius.sm)
        .fill(if active { pal.raised } else { pal.inset })
        .border(1.0, if active { pal.accent } else { pal.border })
        .hover(pal.overlay)
        .on_press(move |st: &mut PiGuiState| {
            st.dispatch(PiGuiAction::SetThemePreference(pref), None);
        })
        .child(
            el().size(12.0, 12.0)
                .radius(6.0)
                .fill(bg)
                .border(1.0, pal.border_strong),
        )
        .child(el().size(8.0, 8.0).radius(4.0).fill(accent))
        .child(text(label).font(k.ty.small_medium).color(pal.text))
}

fn render_providers_section(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let authed = s.auth_providers.iter().filter(|p| p.authenticated).count();
    let mut body = col().w_full().gap(8.0);
    if s.auth_providers.is_empty() {
        body = body.child(
            text("No providers registered yet")
                .font(k.ty.small)
                .color(pal.text_tertiary),
        );
    } else {
        for prov in &s.auth_providers {
            let pid_login = prov.id.clone();
            let pid_logout = prov.id.clone();
            let source = prov.source_label.as_deref().unwrap_or("unconfigured");
            body = body.child(
                row()
                    .key(format!("settings-provider-{}", prov.id))
                    .w_full()
                    .px(10.0)
                    .py(7.0)
                    .gap(8.0)
                    .justify_between()
                    .center_y()
                    .radius(k.radius.sm)
                    .fill(pal.inset)
                    .border(1.0, pal.border)
                    .child(
                        row()
                            .grow(1.0)
                            .min_w(0.0)
                            .gap(8.0)
                            .center_y()
                            .child(el().size(8.0, 8.0).radius(4.0).fill(if prov.authenticated {
                                pal.success
                            } else {
                                pal.text_ghost
                            }))
                            .child(text(&prov.name).font(k.ty.ui_medium).color(pal.text))
                            .child(Badge::new(source).variant(BadgeVariant::Subtle).build(k))
                            .child(if prov.supports_oauth {
                                Badge::new("OAuth").variant(BadgeVariant::Neutral).build(k)
                            } else {
                                Badge::new("API Key").variant(BadgeVariant::Neutral).build(k)
                            }),
                    )
                    .child(if prov.authenticated {
                        Button::new("Sign Out")
                            .variant(ButtonVariant::Ghost)
                            .control_size(ControlSize::Small)
                            .on_press(move |st: &mut PiGuiState| {
                                st.dispatch(PiGuiAction::LogoutProvider(pid_logout.clone()), None);
                            })
                            .build(k)
                    } else {
                        Button::new("Sign In")
                            .variant(ButtonVariant::Outline)
                            .control_size(ControlSize::Small)
                            .on_press(move |st: &mut PiGuiState| {
                                st.dispatch(PiGuiAction::LoginProvider(pid_login.clone()), None);
                            })
                            .build(k)
                    }),
            );
        }
    }

    section_card(
        k,
        pal,
        "settings-providers",
        "Providers & Auth",
        &format!("{authed}/{} authenticated", s.auth_providers.len()),
        body,
    )
}

fn plugin_kind_badge(kind: PluginKind) -> (&'static str, BadgeVariant) {
    match kind {
        PluginKind::QuickJs => ("QuickJS", BadgeVariant::Accent),
        PluginKind::Builtin => ("Builtin", BadgeVariant::Neutral),
        PluginKind::Skill => ("Skill", BadgeVariant::Subtle),
        PluginKind::Mcp => ("MCP", BadgeVariant::Neutral),
    }
}

fn plugin_origin_text(origin: PluginOrigin) -> &'static str {
    match origin {
        PluginOrigin::Builtin => "builtin",
        PluginOrigin::Global => "global",
        PluginOrigin::Project => "project",
        PluginOrigin::Inline => "inline",
    }
}

fn render_plugin_row(k: &Kit, pal: &WakuPalette, plugin: &PluginViewModel) -> El {
    let pid = plugin.id.clone();
    let next_enabled = !plugin.enabled;
    let (kind_label, kind_variant) = plugin_kind_badge(plugin.kind);

    let mut card = col()
        .key(format!("settings-plugin-{}", plugin.id))
        .w_full()
        .px(10.0)
        .py(8.0)
        .gap(5.0)
        .radius(k.radius.sm)
        .fill(pal.inset)
        .border(1.0, if plugin.error.is_some() { pal.danger } else { pal.border })
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
                        .child(text(&plugin.name).font(k.ty.ui_medium).color(pal.text).truncate())
                        .child(Badge::new(kind_label).variant(kind_variant).build(k))
                        .child(
                            Badge::new(plugin_origin_text(plugin.origin))
                                .variant(BadgeVariant::Subtle)
                                .build(k),
                        ),
                )
                .child(
                    row()
                        .gap(8.0)
                        .center_y()
                        .child(
                            text(format!("{} tools · {} cmds", plugin.tool_count, plugin.command_count))
                                .font(k.ty.small)
                                .color(pal.text_tertiary),
                        )
                        .child(
                            Switch::new(plugin.enabled)
                                .web()
                                .label(&plugin.name)
                                .hide_label()
                                .on_toggle(move |st: &mut PiGuiState| {
                                    st.dispatch(
                                        PiGuiAction::TogglePlugin {
                                            id: pid.clone(),
                                            enabled: next_enabled,
                                        },
                                        None,
                                    );
                                })
                                .build(k)
                                .key(format!("settings-plugin-toggle-{}", plugin.id)),
                        ),
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
    if let Some(err) = &plugin.error {
        card = card.child(
            el().w_full()
                .px(8.0)
                .py(5.0)
                .radius(k.radius.xs)
                .fill(pal.danger_soft)
                .child(text(err).font(k.ty.small).color(pal.danger).wrap()),
        );
    }
    card
}

fn render_plugins_section(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let extensions: Vec<&PluginViewModel> = s.plugins.iter().filter(|p| p.kind != PluginKind::Mcp).collect();
    let mut body = col().w_full().gap(8.0).child(
        row()
            .w_full()
            .justify_between()
            .center_y()
            .child(
                text(format!(
                    "{} extensions loaded (QuickJS .ts/.js/.mjs & native)",
                    extensions.len()
                ))
                .font(k.ty.small)
                .color(pal.text_secondary),
            )
            .child(
                Button::new("Reload Plugins")
                    .variant(ButtonVariant::Outline)
                    .control_size(ControlSize::Small)
                    .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::ReloadPlugins, None))
                    .build(k)
                    .key("settings-reload-plugins-btn"),
            ),
    );
    if extensions.is_empty() {
        body = body.child(
            text("No QuickJS or native plugins loaded in this workspace.")
                .font(k.ty.small)
                .color(pal.text_tertiary),
        );
    } else {
        for ext in &extensions {
            body = body.child(render_plugin_row(k, pal, ext));
        }
    }

    section_card(
        k,
        pal,
        "settings-plugins",
        "QuickJS & Native Plugins",
        &format!("{} active", extensions.iter().filter(|p| p.enabled).count()),
        body,
    )
}

fn render_mcp_section(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let servers: Vec<&PluginViewModel> = s.plugins.iter().filter(|p| p.kind == PluginKind::Mcp).collect();
    let mut body = col().w_full().gap(8.0);
    if servers.is_empty() {
        body = body.child(
            text("No MCP servers connected. Configure stdio or StreamableHTTP servers in .pi/settings.json.")
                .font(k.ty.small)
                .color(pal.text_tertiary)
                .wrap(),
        );
    } else {
        for srv in &servers {
            body = body.child(render_plugin_row(k, pal, srv));
        }
    }

    section_card(
        k,
        pal,
        "settings-mcp",
        "MCP Servers",
        &format!("{} configured", servers.len()),
        body,
    )
}
