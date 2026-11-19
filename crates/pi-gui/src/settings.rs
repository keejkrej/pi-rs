//! Full-window 2-pane Waku Settings view (`General`, `Appearance`, `Providers`, `Skills`, `Usage`).

use rune::{Color, Cursor, Cx, El, Icon, ScrollSpec, TextStyle, col, el, icon, row, text, text_input};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

use crate::models::{PluginOrigin, PluginViewModel};
use crate::state::{PiGuiAction, PiGuiState, PluginKind, SettingsPage, ToastTone};
use crate::theme::{LayoutMetrics, ThemePreference, WakuPalette, WakuRadius};

static ICON_ARROW_LEFT: Icon = Icon {
    name: "waku-settings-back",
    view: 16.0,
    body: r#"<path d="M12.5 8H4M7.5 4.5L4 8l3.5 3.5" stroke="currentColor" stroke-width="1.35" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_SEARCH: Icon = Icon {
    name: "waku-settings-search",
    view: 16.0,
    body: r#"<circle cx="7.2" cy="7.2" r="4.3" stroke="currentColor" stroke-width="1.25" fill="none"/><path d="M10.5 10.5l3 3" stroke="currentColor" stroke-width="1.25" stroke-linecap="round"/>"#,
};

static ICON_PACKAGE: Icon = Icon {
    name: "waku-skill-package",
    view: 24.0,
    body: r#"<path d="M11 21.73a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="M12 22V12M3.3 7l7.703 4.734a2 2 0 0 0 1.994 0L20.7 7M7.5 4.27l9 5.15" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_PENCIL: Icon = Icon {
    name: "waku-skill-pencil",
    view: 24.0,
    body: r#"<path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="m15 5 4 4" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_FOLDER: Icon = Icon {
    name: "waku-skill-folder",
    view: 16.0,
    body: r#"<path d="M2.2 4.2a1.2 1.2 0 0 1 1.2-1.2h3.1l1.2 1.3h4.9a1.2 1.2 0 0 1 1.2 1.2v6.3a1.2 1.2 0 0 1-1.2 1.2H3.4a1.2 1.2 0 0 1-1.2-1.2V4.2Z" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linejoin="round"/>"#,
};

static ICON_COPY: Icon = Icon {
    name: "waku-skill-copy",
    view: 24.0,
    body: r#"<rect width="14" height="14" x="8" y="8" rx="2" ry="2" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_ROTATE_CW: Icon = Icon {
    name: "waku-skill-reload",
    view: 24.0,
    body: r#"<path d="M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="M21 3v5h-5" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_NAV_GENERAL: Icon = Icon {
    name: "waku-settings-nav-general",
    view: 24.0,
    body: r#"<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.38a2 2 0 0 0-.73-2.73l-.15-.09a2 2 0 0 1-1-1.74v-.51a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><circle cx="12" cy="12" r="3" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_NAV_APPEARANCE: Icon = Icon {
    name: "waku-settings-nav-appearance",
    view: 24.0,
    body: r#"<circle cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="M12 3a9 9 0 0 1 0 18z" fill="currentColor" stroke="none"/>"#,
};

static ICON_NAV_PROVIDERS: Icon = Icon {
    name: "waku-settings-nav-providers",
    view: 24.0,
    body: r#"<path d="M12 8V4H8" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><rect width="16" height="12" x="4" y="8" rx="2" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="M2 14h2M20 14h2M15 13v2M9 13v2" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_NAV_USAGE: Icon = Icon {
    name: "waku-settings-nav-usage",
    view: 24.0,
    body: r#"<path d="M3 3v16a2 2 0 0 0 2 2h16M13 17V9M18 17V5M8 17v-3" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

fn page_icon(page: SettingsPage) -> &'static Icon {
    match page {
        SettingsPage::General => &ICON_NAV_GENERAL,
        SettingsPage::Appearance => &ICON_NAV_APPEARANCE,
        SettingsPage::Providers => &ICON_NAV_PROVIDERS,
        SettingsPage::Skills => &ICON_PACKAGE,
        SettingsPage::Usage => &ICON_NAV_USAGE,
    }
}

fn render_card_box(title: &str, subtitle: &str, pal: &WakuPalette, body: El, fill_width: bool) -> El {
    let base = if fill_width {
        col().w_full()
    } else {
        col().grow(1.0).min_w(0.0).shrink()
    };
    base.px(20.0)
        .py(14.0)
        .gap(10.0)
        .radius(WakuRadius::COMPOSER)
        .fill(pal.raised)
        .child(
            col()
                .gap(2.0)
                .child(text(title).font(TextStyle::new(13.5, 500.0)).color(pal.text))
                .child(
                    text(subtitle)
                        .font(TextStyle::new(12.5, 400.0))
                        .color(pal.text_secondary),
                ),
        )
        .child(body)
}

fn render_card(title: &str, subtitle: &str, pal: &WakuPalette, body: El) -> El {
    render_card_box(title, subtitle, pal, body, true)
}

fn render_general_toggle_card(
    key: &'static str,
    title: &'static str,
    description: &'static str,
    flag_key: &'static str,
    default_on: bool,
    s: &PiGuiState,
    pal: &WakuPalette,
) -> El {
    let on = s.settings_flags.get(flag_key).copied().unwrap_or(default_on);
    let fk = flag_key.to_string();
    row()
        .w_full()
        .min_h(60.0)
        .px(20.0)
        .py(12.0)
        .gap(24.0)
        .center_y()
        .radius(WakuRadius::COMPOSER)
        .fill(pal.raised)
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .gap(5.0)
                .child(text(title).font(TextStyle::new(13.5, 500.0)).color(pal.text))
                .child(
                    text(description)
                        .font(TextStyle::new(12.5, 400.0).lh(18.0))
                        .color(pal.text_secondary)
                        .wrap(),
                ),
        )
        .child(render_toggle_switch(
            key.to_string(),
            on,
            pal,
            move |st: &mut PiGuiState| st.dispatch(PiGuiAction::ToggleSettingFlag(fk.clone()), None),
        ))
}

fn render_general_page(s: &PiGuiState, pal: &WakuPalette) -> El {
    let cwd_card = render_card(
        "Workspace",
        "Active repository root and default git branch",
        pal,
        row()
            .w_full()
            .justify_between()
            .center_y()
            .child(
                text(&s.config.cwd)
                    .font(TextStyle::new(12.5, 400.0).mono())
                    .color(pal.text_secondary)
                    .truncate(),
            )
            .child(
                text(&s.selected_branch)
                    .font(TextStyle::new(12.0, 500.0).mono())
                    .color(pal.text_secondary),
            ),
    );

    let local_card = render_general_toggle_card(
        "settings-toggle-local-by-default",
        "Local by default",
        "Start new tasks in the workspace root rather than a separate git worktree",
        "local_by_default",
        true,
        s,
        pal,
    );

    let math_card = render_general_toggle_card(
        "settings-toggle-render-math",
        "Render math",
        "Format inline and display LaTeX expressions inside assistant responses",
        "render_math",
        true,
        s,
        pal,
    );

    let thinking_card = render_card(
        "Reasoning",
        "Default thinking effort applied to new tasks",
        pal,
        row()
            .w_full()
            .justify_between()
            .center_y()
            .gap(12.0)
            .child(
                text(format!("Current level: {}", s.context_usage.thinking_level))
                    .font(TextStyle::new(13.0, 400.0))
                    .color(pal.text_secondary),
            )
            .child(
                row()
                    .px(10.0)
                    .h(28.0)
                    .center()
                    .radius(WakuRadius::SM)
                    .fill(pal.surface)
                    .border_layout()
                    .border(1.0, pal.border_strong)
                    .hover(pal.overlay)
                    .cursor(Cursor::Pointer)
                    .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::CycleThinkingLevel, None))
                    .child(text("Cycle Effort").font(TextStyle::new(12.5, 500.0)).color(pal.text)),
            ),
    );

    col()
        .key("settings.content.inner")
        .role("region")
        .w_full()
        .max_w(LayoutMetrics::SETTINGS_CONTENT_MAX_WIDTH)
        .gap(15.0)
        .child(
            text("General")
                .key("settings.content.heading")
                .role("heading")
                .font(TextStyle::new(18.0, 500.0).lh(24.0))
                .color(pal.text),
        )
        .child(cwd_card)
        .child(local_card)
        .child(math_card)
        .child(thinking_card)
}

fn render_appearance_page(s: &PiGuiState, pal: &WakuPalette) -> El {
    let mut theme_row = row().gap(6.0).shrink().center_y();
    for pref in ThemePreference::ALL {
        let selected = s.theme_preference == pref;
        theme_row = theme_row.child(
            row()
                .key(format!("settings-theme-{}", pref.label().to_ascii_lowercase()))
                .role("button")
                .label(pref.label())
                .px(12.0)
                .h(30.0)
                .center()
                .radius(7.0)
                .fill(if selected { pal.overlay_strong } else { pal.raised })
                .border_layout()
                .border(1.0, if selected { pal.border_strong } else { pal.border })
                .hover(pal.overlay)
                .cursor(Cursor::Pointer)
                .on_press(move |st: &mut PiGuiState| st.dispatch(PiGuiAction::SetThemePreference(pref), None))
                .child(
                    text(pref.label())
                        .font(TextStyle::new(13.0, if selected { 500.0 } else { 400.0 }))
                        .color(if selected { pal.text } else { pal.text_secondary }),
                ),
        );
    }

    let card = col().w_full().radius(WakuRadius::COMPOSER).fill(pal.raised).child(
        row()
            .w_full()
            .min_h(60.0)
            .px(20.0)
            .py(12.0)
            .gap(24.0)
            .center_y()
            .child(
                col()
                    .grow(1.0)
                    .min_w(0.0)
                    .gap(5.0)
                    .child(text("Theme").font(TextStyle::new(13.5, 500.0)).color(pal.text))
                    .child(
                        text("Select System, Light, or Dark Waku graphite tokens")
                            .font(TextStyle::new(12.5, 400.0).lh(18.0))
                            .color(pal.text_secondary)
                            .wrap(),
                    ),
            )
            .child(theme_row),
    );

    col()
        .key("settings.content.inner")
        .role("region")
        .w_full()
        .max_w(LayoutMetrics::SETTINGS_CONTENT_MAX_WIDTH)
        .gap(15.0)
        .child(
            text("Appearance")
                .key("settings.content.heading")
                .role("heading")
                .font(TextStyle::new(18.0, 500.0).lh(24.0))
                .color(pal.text),
        )
        .child(card)
}

fn render_toggle_switch(key: String, on: bool, pal: &WakuPalette, on_press: impl Fn(&mut PiGuiState) + 'static) -> El {
    let mut pill = row()
        .key(key)
        .role("switch")
        .w(36.0)
        .h(20.0)
        .padding(2.0)
        .shrink()
        .radius(999.0)
        .center_y()
        .cursor(Cursor::Pointer)
        .fill(if on { pal.inverse } else { pal.inset })
        .border_layout()
        .border(1.0, if on { pal.inverse } else { pal.border_strong })
        .on_press(on_press);
    if on {
        pill = pill.justify_end();
    } else {
        pill = pill.justify_start();
    }
    pill.child(
        el().size(14.0, 14.0)
            .radius(999.0)
            .fill(if on { pal.on_inverse } else { pal.text_tertiary }),
    )
}

fn render_providers_page(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let mut inner = col()
        .key("settings.content.inner")
        .role("region")
        .w_full()
        .max_w(LayoutMetrics::SETTINGS_CONTENT_MAX_WIDTH)
        .gap(15.0)
        .child(
            text("Providers")
                .key("settings.content.heading")
                .role("heading")
                .font(TextStyle::new(18.0, 500.0).lh(24.0))
                .color(pal.text),
        );

    let mut card = col()
        .w_full()
        .px(20.0)
        .py(6.0)
        .radius(WakuRadius::COMPOSER)
        .fill(pal.raised);

    let count = s.auth_providers.len();
    for (idx, prov) in s.auth_providers.iter().enumerate() {
        let id_btn = prov.id.clone();
        let id_toggle = prov.id.clone();
        let authed = prov.authenticated;
        let enabled = !s.disabled_providers.contains(&prov.id);
        let btn_label = if authed { "Logout" } else { "Connect" };
        let detail = if authed {
            let src = prov.source_label.as_deref().unwrap_or("configured");
            if enabled {
                format!("Connected · {src}")
            } else {
                format!("Disabled for new tasks · {src}")
            }
        } else {
            "Not configured · API key or OAuth credentials".to_string()
        };
        let dot_color = if !authed {
            pal.text_ghost
        } else if !enabled {
            pal.warning
        } else {
            pal.success
        };
        let icon_tile = el()
            .size(30.0, 30.0)
            .shrink()
            .radius(7.0)
            .fill(pal.overlay)
            .center()
            .child(crate::overlays::provider_mark_view(
                &prov.id,
                16.0,
                pal,
                authed && enabled,
            ))
            .child(
                el().absolute(21.0, 21.0)
                    .size(10.0, 10.0)
                    .radius(5.0)
                    .fill(dot_color)
                    .border_layout()
                    .border(2.0, pal.raised),
            );

        let mut provider_row = row()
            .key(format!("settings-provider-{}", prov.id))
            .w_full()
            .py(12.0)
            .gap(12.0)
            .center_y()
            .child(icon_tile)
            .child(
                col()
                    .grow(1.0)
                    .min_w(0.0)
                    .gap(3.0)
                    .child(text(&prov.name).font(TextStyle::new(13.5, 500.0)).color(if authed {
                        pal.text
                    } else {
                        pal.text_secondary
                    }))
                    .child(
                        text(detail)
                            .font(TextStyle::new(12.5, 400.0))
                            .color(pal.text_tertiary)
                            .truncate(),
                    ),
            )
            .child(
                Button::new(btn_label)
                    .variant(ButtonVariant::Outline)
                    .control_size(ControlSize::Small)
                    .on_press(move |st: &mut PiGuiState| {
                        let act = if authed {
                            PiGuiAction::LogoutProvider(id_btn.clone())
                        } else {
                            PiGuiAction::LoginProvider(id_btn.clone())
                        };
                        st.dispatch(act, None);
                    })
                    .build(k),
            )
            .child(render_toggle_switch(
                format!("provider-enabled-{}", prov.id),
                enabled,
                pal,
                move |st: &mut PiGuiState| st.dispatch(PiGuiAction::ToggleProviderEnabled(id_toggle.clone()), None),
            ));

        if idx + 1 < count {
            provider_row = provider_row
                .border_layout()
                .border_sides([0.0, 0.0, 1.0, 0.0], pal.border);
        }
        card = card.child(provider_row);
    }

    inner = inner.child(card);
    inner
}

fn plugin_kind_label(kind: PluginKind) -> &'static str {
    match kind {
        PluginKind::Builtin => "Built-in",
        PluginKind::QuickJs => "QuickJS",
        PluginKind::Mcp => "MCP",
        PluginKind::Skill => "Skill",
    }
}

fn plugin_origin_label(origin: PluginOrigin) -> &'static str {
    match origin {
        PluginOrigin::Builtin => "Built-in",
        PluginOrigin::Global => "User",
        PluginOrigin::Project => "Workspace",
        PluginOrigin::Inline => "Inline",
    }
}

fn plugin_location_label(plugin: &PluginViewModel) -> String {
    match plugin.origin {
        PluginOrigin::Builtin => format!("builtin://{}", plugin.name),
        PluginOrigin::Global => format!("~/.pi/agent/extensions/{}", plugin.name),
        PluginOrigin::Project => format!(".pi/extensions/{}", plugin.name),
        PluginOrigin::Inline => format!("inline://{}", plugin.name),
    }
}

fn skill_info_row(label: &str, value: &str, mono: bool, pal: &WakuPalette, last: bool) -> El {
    let mut val_text = text(value).font(if mono {
        TextStyle::new(12.5, 400.0).mono()
    } else {
        TextStyle::new(12.5, 400.0)
    });
    val_text = val_text.color(pal.text_secondary).truncate();

    let mut r = row()
        .w_full()
        .py(8.0)
        .gap(12.0)
        .center_y()
        .child(
            el().w(84.0)
                .shrink()
                .child(text(label).font(TextStyle::new(12.5, 400.0)).color(pal.text_tertiary)),
        )
        .child(el().grow(1.0).min_w(0.0).child(val_text));

    if !last {
        r = r.border_layout().border_sides([0.0, 0.0, 1.0, 0.0], pal.border);
    }
    r
}

fn skill_action_button(
    key: String,
    ic: &'static Icon,
    label: &'static str,
    pal: &WakuPalette,
    on_press: impl Fn(&mut PiGuiState) + 'static,
) -> El {
    row()
        .key(key)
        .role("button")
        .label_static(label)
        .h(26.0)
        .px(10.0)
        .gap(5.0)
        .shrink()
        .center_y()
        .radius(6.0)
        .border_layout()
        .border(1.0, pal.border_strong)
        .cursor(Cursor::Pointer)
        .hover(pal.overlay)
        .on_press(on_press)
        .child(icon(ic, 11.0).color(pal.text_tertiary))
        .child(text(label).font(TextStyle::new(12.5, 400.0)).color(pal.text_secondary))
}

fn render_skill_detail_pane(plugin: &PluginViewModel, pal: &WakuPalette) -> El {
    let kind_label = plugin_kind_label(plugin.kind);
    let origin_label = plugin_origin_label(plugin.origin);
    let caption = format!("{kind_label} · {origin_label}");
    let enabled = plugin.enabled;
    let pid = plugin.id.clone();
    let invoke_name = format!(
        "/{}",
        plugin
            .name
            .trim_end_matches(".ts")
            .trim_end_matches(".js")
            .trim_end_matches(".mjs")
    );
    let location = plugin_location_label(plugin);
    let edit_name = plugin.name.clone();
    let reveal_loc = location.clone();
    let copy_loc = location.clone();
    let detail_note = format!(
        "{} ({}) registers {} tool(s) and {} command(s) in {origin_label} scope.",
        plugin.name, kind_label, plugin.tool_count, plugin.command_count
    );

    let mut title_row = row().w_full().center_y().gap(7.0).child(
        text(&plugin.name)
            .font(TextStyle::new(15.0, 500.0))
            .color(if enabled { pal.text } else { pal.text_secondary })
            .truncate()
            .min_w(0.0),
    );
    if !enabled {
        title_row = title_row.child(text("Disabled").font(TextStyle::new(12.5, 400.0)).color(pal.warning));
    }

    let header = row()
        .w_full()
        .center_y()
        .gap(12.0)
        .child(
            el().size(38.0, 38.0)
                .shrink()
                .radius(9.0)
                .fill(pal.overlay)
                .center()
                .child(icon(&ICON_PACKAGE, 18.0).color(pal.text_secondary)),
        )
        .child(
            col().grow(1.0).min_w(0.0).gap(2.0).child(title_row).child(
                text(caption)
                    .font(TextStyle::new(12.5, 400.0))
                    .color(pal.text_tertiary)
                    .truncate(),
            ),
        )
        .child(render_toggle_switch(
            format!("skill-toggle-{}", plugin.id),
            enabled,
            pal,
            move |st: &mut PiGuiState| {
                st.dispatch(
                    PiGuiAction::TogglePlugin {
                        id: pid.clone(),
                        enabled: !enabled,
                    },
                    None,
                );
            },
        ));

    let mut pane = col().w_full().px(24.0).pt(18.0).pb(20.0).child(header).child(
        text(if plugin.description.is_empty() {
            "No description provided."
        } else {
            plugin.description.as_str()
        })
        .mt(14.0)
        .font(TextStyle::new(12.5, 400.0).lh(17.0))
        .color(pal.text_secondary)
        .wrap(),
    );

    if let Some(err) = &plugin.error {
        pane = pane.child(
            row()
                .mt(10.0)
                .w_full()
                .px(10.0)
                .py(8.0)
                .radius(7.0)
                .fill(pal.danger_soft)
                .child(
                    text(err)
                        .font(TextStyle::new(12.0, 400.0).mono())
                        .color(pal.danger)
                        .wrap(),
                ),
        );
    }

    pane = pane
        .child(
            col()
                .mt(16.0)
                .w_full()
                .child(skill_info_row("Invoke", &invoke_name, true, pal, false))
                .child(skill_info_row("Location", &location, true, pal, false))
                .child(skill_info_row("Type", kind_label, false, pal, false))
                .child(skill_info_row(
                    "Status",
                    if enabled { "Enabled" } else { "Disabled" },
                    false,
                    pal,
                    true,
                )),
        )
        .child(
            row()
                .mt(16.0)
                .w_full()
                .gap(7.0)
                .center_y()
                .child(skill_action_button(
                    format!("skill-open-{}", plugin.id),
                    &ICON_PENCIL,
                    "Edit SKILL.md",
                    pal,
                    move |st: &mut PiGuiState| {
                        st.push_toast(format!("Opening {edit_name}"), ToastTone::Info);
                    },
                ))
                .child(skill_action_button(
                    format!("skill-reveal-{}", plugin.id),
                    &ICON_FOLDER,
                    "Reveal",
                    pal,
                    move |st: &mut PiGuiState| {
                        st.push_toast(format!("Path: {reveal_loc}"), ToastTone::Info);
                    },
                ))
                .child(skill_action_button(
                    format!("skill-copy-{}", plugin.id),
                    &ICON_COPY,
                    "Copy Path",
                    pal,
                    move |st: &mut PiGuiState| {
                        st.push_toast(format!("Copied {copy_loc}"), ToastTone::Success);
                    },
                ))
                .child(el().grow(1.0))
                .child(skill_action_button(
                    "skills-reload-btn".to_string(),
                    &ICON_ROTATE_CW,
                    "Reload",
                    pal,
                    |st: &mut PiGuiState| st.dispatch(PiGuiAction::ReloadPlugins, None),
                )),
        )
        .child(
            col()
                .mt(18.0)
                .pt(14.0)
                .w_full()
                .gap(10.0)
                .border_layout()
                .border_sides([1.0, 0.0, 0.0, 0.0], pal.border)
                .child(
                    text("SKILL.md")
                        .font(TextStyle::new(12.5, 400.0).mono())
                        .color(pal.text_ghost),
                )
                .child(
                    text(detail_note)
                        .font(TextStyle::new(12.5, 400.0).lh(18.0))
                        .color(pal.text_secondary)
                        .wrap(),
                ),
        );

    col()
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: false })
        .child(pane)
}

fn render_skills_page(s: &PiGuiState, pal: &WakuPalette) -> El {
    let q = s.skills_search_edit.text.trim().to_ascii_lowercase();
    let visible: Vec<&PluginViewModel> = s
        .plugins
        .iter()
        .filter(|p| {
            s.skills_filter.is_none_or(|f| p.kind == f)
                && (q.is_empty()
                    || p.name.to_ascii_lowercase().contains(&q)
                    || p.description.to_ascii_lowercase().contains(&q)
                    || plugin_kind_label(p.kind).to_ascii_lowercase().contains(&q))
        })
        .collect();

    let effective_id = s
        .selected_skill_id
        .as_deref()
        .filter(|id| visible.iter().any(|p| p.id == *id))
        .or_else(|| visible.first().map(|p| p.id.as_str()));
    let selected_plugin = effective_id.and_then(|id| visible.iter().copied().find(|p| p.id == id));

    let filter_chips = [
        ("All", None),
        ("Built-in", Some(PluginKind::Builtin)),
        ("Skill", Some(PluginKind::Skill)),
        ("QuickJS", Some(PluginKind::QuickJs)),
        ("MCP", Some(PluginKind::Mcp)),
    ];
    let mut chips_row = row().w_full().gap(3.0).center_y();
    for (idx, (label, kind_opt)) in filter_chips.into_iter().enumerate() {
        let active = s.skills_filter == kind_opt;
        chips_row = chips_row.child(
            row()
                .key(format!("skills-filter-{idx}"))
                .role("button")
                .label_static(label)
                .h(22.0)
                .px(5.0)
                .center()
                .shrink()
                .radius(6.0)
                .fill(if active { pal.overlay_strong } else { Color::TRANSPARENT })
                .border_layout()
                .border(1.0, if active { pal.border_strong } else { Color::TRANSPARENT })
                .hover(pal.overlay)
                .cursor(Cursor::Pointer)
                .on_press(move |st: &mut PiGuiState| st.dispatch(PiGuiAction::SetSkillsFilter(kind_opt), None))
                .child(
                    text(label)
                        .font(TextStyle::new(11.0, if active { 500.0 } else { 400.0 }))
                        .color(if active { pal.text } else { pal.text_tertiary }),
                ),
        );
    }

    let mut list_col = col()
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .px(8.0)
        .py(4.0)
        .gap(1.0)
        .scroll(ScrollSpec { stick_bottom: false });

    if visible.is_empty() {
        list_col = list_col.child(
            el().px(10.0).py(16.0).child(
                text(if s.plugins.is_empty() {
                    "No skills or extensions found"
                } else {
                    "No matching skills"
                })
                .font(TextStyle::new(12.5, 400.0))
                .color(pal.text_tertiary),
            ),
        );
    } else {
        for plugin in &visible {
            let selected = effective_id == Some(plugin.id.as_str());
            let pid = plugin.id.clone();
            let mut top_line = row().w_full().center_y().gap(6.0).child(
                text(&plugin.name)
                    .font(TextStyle::new(12.5, 500.0))
                    .color(if plugin.enabled { pal.text } else { pal.text_secondary })
                    .truncate()
                    .grow(1.0)
                    .min_w(0.0),
            );
            if !plugin.enabled {
                top_line = top_line.child(text("Disabled").font(TextStyle::new(11.5, 400.0)).color(pal.warning));
            }
            let subtitle = if plugin.description.is_empty() {
                plugin_kind_label(plugin.kind)
            } else {
                plugin.description.as_str()
            };
            list_col = list_col.child(
                row()
                    .key(format!("settings-skill-{}", plugin.id))
                    .role("button")
                    .label(&plugin.name)
                    .w_full()
                    .px(9.0)
                    .py(7.0)
                    .gap(9.0)
                    .center_y()
                    .radius(8.0)
                    .fill(if selected {
                        pal.sidebar_item_background
                    } else {
                        Color::TRANSPARENT
                    })
                    .hover(pal.overlay)
                    .cursor(Cursor::Pointer)
                    .on_press(move |st: &mut PiGuiState| st.dispatch(PiGuiAction::SelectSkill(pid.clone()), None))
                    .child(
                        el().size(26.0, 26.0)
                            .shrink()
                            .radius(6.0)
                            .fill(pal.overlay)
                            .center()
                            .child(icon(&ICON_PACKAGE, 13.0).color(pal.text_secondary)),
                    )
                    .child(
                        col().grow(1.0).min_w(0.0).gap(1.0).child(top_line).child(
                            text(subtitle)
                                .font(TextStyle::new(12.5, 400.0))
                                .color(pal.text_tertiary)
                                .truncate(),
                        ),
                    ),
            );
        }
    }

    let footer_label = if visible.len() == s.plugins.len() {
        if s.plugins.len() == 1 {
            "1 skill".to_string()
        } else {
            format!("{} skills", s.plugins.len())
        }
    } else {
        format!("{} of {} skills", visible.len(), s.plugins.len())
    };

    let left_pane = col()
        .w(264.0)
        .h_full()
        .shrink()
        .border_layout()
        .border_sides([0.0, 1.0, 0.0, 0.0], pal.border)
        .child(
            col()
                .w_full()
                .px(10.0)
                .pt(18.0)
                .pb(8.0)
                .gap(7.0)
                .shrink()
                .child(
                    row()
                        .w_full()
                        .h(28.0)
                        .px(9.0)
                        .gap(8.0)
                        .center_y()
                        .radius(WakuRadius::SM)
                        .fill(pal.inset)
                        .border_layout()
                        .border(1.0, pal.border)
                        .child(icon(&ICON_SEARCH, 13.0).color(pal.text_tertiary))
                        .child(
                            text_input(&s.skills_search_edit, TextStyle::new(12.5, 400.0))
                                .key("skills-search-input")
                                .label("Search skills")
                                .single_line()
                                .grow(1.0)
                                .min_w(0.0)
                                .color(pal.text)
                                .placeholder("Search skills", pal.text_ghost)
                                .on_edit(|st: &mut PiGuiState| &mut st.skills_search_edit),
                        ),
                )
                .child(chips_row),
        )
        .child(list_col)
        .child(
            row()
                .w_full()
                .h(26.0)
                .px(12.0)
                .center()
                .shrink()
                .border_layout()
                .border_sides([1.0, 0.0, 0.0, 0.0], pal.border)
                .child(
                    text(footer_label)
                        .font(TextStyle::new(12.5, 400.0))
                        .color(pal.text_ghost),
                ),
        );

    let detail_pane = match selected_plugin {
        Some(plugin) => render_skill_detail_pane(plugin, pal),
        None => col()
            .grow(1.0)
            .min_w(0.0)
            .h_full()
            .center()
            .gap(8.0)
            .child(icon(&ICON_PACKAGE, 22.0).color(pal.text_ghost))
            .child(
                text("Select a skill to inspect")
                    .font(TextStyle::new(12.5, 400.0))
                    .color(pal.text_ghost),
            ),
    };

    col()
        .key("settings.content.inner")
        .role("region")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .child(row().w_full().grow(1.0).min_h(0.0).child(left_pane).child(detail_pane))
}

fn render_usage_page(s: &PiGuiState, pal: &WakuPalette) -> El {
    let usage = &s.context_usage.session_usage;
    let stats_row = row()
        .w_full()
        .min_w(0.0)
        .gap(16.0)
        .child(render_card_box(
            "Input Tokens",
            "Session prompt & context volume",
            pal,
            text(format!("{}", usage.input_tokens))
                .font(TextStyle::new(20.0, 500.0).mono())
                .color(pal.text),
            false,
        ))
        .child(render_card_box(
            "Output Tokens",
            "Generated assistant tokens",
            pal,
            text(format!("{}", usage.output_tokens))
                .font(TextStyle::new(20.0, 500.0).mono())
                .color(pal.text),
            false,
        ))
        .child(render_card_box(
            "Estimated Cost",
            "Active session spend",
            pal,
            text(format!("${:.4}", usage.total_cost))
                .font(TextStyle::new(20.0, 500.0).mono())
                .color(pal.text),
            false,
        ));

    let breakdown_card = render_card(
        "Session Breakdown",
        "Token efficiency, model context budget, and cached reads",
        pal,
        col()
            .w_full()
            .gap(8.0)
            .child(
                row()
                    .w_full()
                    .justify_between()
                    .center_y()
                    .child(
                        text("Cache Read Tokens")
                            .font(TextStyle::new(13.0, 400.0))
                            .color(pal.text_secondary),
                    )
                    .child(
                        text(format!("{}", usage.cache_read_tokens))
                            .font(TextStyle::new(13.0, 500.0).mono())
                            .color(pal.text),
                    ),
            )
            .child(
                row()
                    .w_full()
                    .justify_between()
                    .center_y()
                    .child(
                        text("Context Window")
                            .font(TextStyle::new(13.0, 400.0))
                            .color(pal.text_secondary),
                    )
                    .child(
                        text(format!(
                            "{} / {} ({:.1}%)",
                            s.context_usage.used_tokens, s.context_usage.window_tokens, s.context_usage.percent
                        ))
                        .font(TextStyle::new(13.0, 500.0).mono())
                        .color(pal.gauge),
                    ),
            ),
    );

    col()
        .key("settings.content.inner")
        .role("region")
        .w_full()
        .max_w(LayoutMetrics::SETTINGS_USAGE_MAX_WIDTH)
        .gap(15.0)
        .child(
            text("Usage")
                .key("settings.content.heading")
                .role("heading")
                .font(TextStyle::new(18.0, 500.0).lh(24.0))
                .color(pal.text),
        )
        .child(stats_row)
        .child(breakdown_card)
}

/// Renders Waku's full-window 2-pane Settings screen (`settings`).
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let active_page = s.settings_open.unwrap_or(SettingsPage::General);
    let mut sidebar = col()
        .key("settings.sidebar")
        .role("navigation")
        .w(LayoutMetrics::DEFAULT_SIDEBAR_WIDTH)
        .h_full()
        .shrink()
        .fill(pal.sidebar)
        .pt(LayoutMetrics::TITLEBAR_HEIGHT)
        .child(
            row()
                .key("settings.sidebar.back")
                .role("button")
                .label_static("Back")
                .h(34.0)
                .mx(12.0)
                .px(9.0)
                .gap(9.0)
                .center_y()
                .radius(WakuRadius::SM)
                .hover(pal.overlay)
                .cursor(Cursor::Pointer)
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::CloseSettings, None))
                .child(icon(&ICON_ARROW_LEFT, 14.0).color(pal.text_secondary))
                .child(text("Back").font(TextStyle::new(13.0, 500.0).lh(18.0)).color(pal.text)),
        )
        .child(
            row()
                .key("settings.sidebar.search")
                .role("textbox")
                .label_static("Search Settings")
                .h(32.0)
                .mx(12.0)
                .mt(8.0)
                .px(9.0)
                .gap(8.0)
                .center_y()
                .radius(WakuRadius::SM)
                .fill(pal.inset)
                .border_layout()
                .border(1.0, pal.border)
                .child(icon(&ICON_SEARCH, 13.0).color(pal.text_tertiary))
                .child(
                    text_input(&s.settings_search_edit, TextStyle::new(12.5, 400.0))
                        .label("Search settings")
                        .single_line()
                        .grow(1.0)
                        .min_w(0.0)
                        .color(pal.text)
                        .placeholder("Search settings…", pal.text_ghost)
                        .on_edit(|st: &mut PiGuiState| &mut st.settings_search_edit),
                ),
        );

    let q = s.settings_search_edit.text.trim().to_ascii_lowercase();
    let mut nav_col = col().mx(12.0).mt(18.0).gap(3.0);
    for page in SettingsPage::ALL {
        if !q.is_empty() && !page.label().to_ascii_lowercase().contains(&q) {
            continue;
        }
        let selected = active_page == page;
        nav_col = nav_col.child(
            row()
                .key(format!("settings.sidebar.nav.{}", page.id()))
                .role("button")
                .label_static(page.label())
                .w_full()
                .h(36.0)
                .px(11.0)
                .gap(10.0)
                .center_y()
                .radius(WakuRadius::SM)
                .fill(if selected {
                    pal.sidebar_item_background
                } else {
                    Color::TRANSPARENT
                })
                .hover(pal.overlay)
                .cursor(Cursor::Pointer)
                .on_press(move |st: &mut PiGuiState| st.dispatch(PiGuiAction::SelectSettingsPage(page), None))
                .child(
                    icon(page_icon(page), 15.0)
                        .color(if selected { pal.text } else { pal.text_secondary })
                        .shrink(),
                )
                .child(
                    text(page.label())
                        .font(TextStyle::new(13.5, if selected { 500.0 } else { 400.0 }).lh(18.0))
                        .color(if selected { pal.text } else { pal.text_secondary }),
                ),
        );
    }
    sidebar = sidebar.child(nav_col);

    let inner = match active_page {
        SettingsPage::General => render_general_page(s, pal),
        SettingsPage::Appearance => render_appearance_page(s, pal),
        SettingsPage::Providers => render_providers_page(s, k, pal),
        SettingsPage::Skills => render_skills_page(s, pal),
        SettingsPage::Usage => render_usage_page(s, pal),
    };

    let content_wrapper = if active_page == SettingsPage::Skills {
        col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .pt(LayoutMetrics::TITLEBAR_HEIGHT)
            .px(32.0)
            .pb(48.0)
            .items_center()
            .child(inner)
    } else {
        col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .pt(LayoutMetrics::TITLEBAR_HEIGHT)
            .px(32.0)
            .pb(48.0)
            .items_center()
            .scroll(ScrollSpec { stick_bottom: false })
            .child(inner)
    };

    let content = col()
        .key("settings.content")
        .role("region")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .fill(pal.canvas)
        .border_layout()
        .border_sides([0.0, 0.0, 0.0, 1.0], pal.border)
        .clip_rounded(0.0)
        .child(content_wrapper);

    row()
        .key("settings")
        .role("main")
        .w_full()
        .h_full()
        .fill(pal.canvas)
        .clip_rounded(0.0)
        .child(sidebar)
        .child(content)
}
