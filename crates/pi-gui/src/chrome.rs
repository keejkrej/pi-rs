//! Top-level Rune AppSpec, Waku 3-pane window chrome, borderless titlebar, keyboard routing, and toasts.

use rune::{App, AppSpec, Chrome, Color, Cursor, Cx, El, Icon, Key, KeyEvent, TextStyle, Tx, col, el, icon, row, text};
use rune_kit::Kit;

use crate::bridge::GuiBridge;
use crate::composer;
use crate::overlays;
use crate::right_panel;
use crate::settings;
use crate::sidebar;
use crate::state::{LaunchConfig, OverlayKind, PiGuiAction, PiGuiState, PopoverKind, SCENE_ENTRIES, SettingsPage};
use crate::theme::{LayoutMetrics, WakuPalette, WakuRadius, palette, waku_kit};
use crate::transcript;

static ICON_PANEL_LEFT: Icon = Icon {
    name: "waku-hdr-panel-left",
    view: 16.0,
    body: r#"<rect x="2.2" y="2.8" width="11.6" height="10.4" rx="2" stroke="currentColor" stroke-width="1.25" fill="none"/><path d="M6.2 2.8v10.4" stroke="currentColor" stroke-width="1.25"/>"#,
};

static ICON_PANEL_RIGHT: Icon = Icon {
    name: "waku-hdr-panel-right",
    view: 16.0,
    body: r#"<rect x="2.2" y="2.8" width="11.6" height="10.4" rx="2" stroke="currentColor" stroke-width="1.25" fill="none"/><path d="M9.8 2.8v10.4" stroke="currentColor" stroke-width="1.25"/>"#,
};

static ICON_ARROW_LEFT: Icon = Icon {
    name: "waku-hdr-arrow-left",
    view: 56.0,
    body: r#"<path fill="currentColor" d="M6.238 28c0 .586.258 1.125.727 1.57l15.562 15.54c.492.445.985.656 1.524.656c1.172 0 2.062-.844 2.062-2.016c0-.562-.21-1.125-.586-1.477l-5.226-5.343l-7.922-7.196l5.695.352H47.7c1.219 0 2.063-.867 2.063-2.086s-.844-2.086-2.063-2.086H18.074l-5.672.352l7.899-7.196l5.226-5.343c.399-.376.586-.915.586-1.477c0-1.172-.89-2.016-2.062-2.016c-.54 0-1.055.188-1.57.704L6.964 26.43c-.469.445-.727.984-.727 1.57"/>"#,
};

static ICON_ARROW_RIGHT: Icon = Icon {
    name: "waku-hdr-arrow-right",
    view: 56.0,
    body: r#"<path fill="currentColor" d="M49.762 28c0-.586-.234-1.125-.703-1.57L33.543 10.938c-.516-.516-1.031-.704-1.594-.704c-1.148 0-2.039.844-2.039 2.016c0 .563.188 1.101.563 1.477l5.25 5.343l7.898 7.196l-5.672-.352H8.301c-1.195 0-2.063.867-2.063 2.086s.867 2.086 2.063 2.086h29.648l5.696-.352l-7.922 7.196l-5.25 5.343c-.375.352-.563.914-.563 1.477c0 1.172.89 2.016 2.04 2.016c.562 0 1.054-.211 1.523-.657L49.059 29.57c.469-.445.703-.984.703-1.57"/>"#,
};

static ICON_TOAST_CHECK: Icon = Icon {
    name: "waku-toast-check",
    view: 16.0,
    body: r#"<path d="M3.5 8.3l3 3 6-6.2" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_TOAST_ALERT: Icon = Icon {
    name: "waku-toast-alert",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="5.6" stroke="currentColor" stroke-width="1.35" fill="none"/><path d="M8 4.8v3.6M8 11v.5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"#,
};

static ICON_CLOSE_SMALL: Icon = Icon {
    name: "waku-toast-close",
    view: 16.0,
    body: r#"<path d="M4.5 4.5l7 7M11.5 4.5l-7 7" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>"#,
};

/// Builds the initial [`PiGuiState`] from Rune CLI arguments (`--cwd`, `--session`, etc.).
#[must_use]
pub fn init_state() -> PiGuiState {
    PiGuiState::new(LaunchConfig::from_rune_args())
}

/// Starts the background `pi-coding-agent` bridge once Rune's window waker is ready.
pub fn setup_state(s: &mut PiGuiState, waker: rune::Waker) {
    if !s.config.demo && s.demo_scene.is_none() {
        let bridge = GuiBridge::spawn(s.config.clone(), waker);
        s.bridge = Some(bridge.sender());
    }
}

/// Drains background bridge events before each frame without touching disk or network.
pub fn poll_state(s: &mut PiGuiState, _clock: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    if let Some(bridge) = &s.bridge {
        let events = bridge.drain_events();
        for ev in events {
            if s.apply_bridge_event(ev) {
                changed = true;
            }
        }
    }
    if !s.pending_events.is_empty() {
        let queued = std::mem::take(&mut s.pending_events);
        for ev in queued {
            if s.apply_bridge_event(ev) {
                changed = true;
            }
        }
    }
    if changed {
        tx.settle();
    }
    changed
}

/// Global keyboard shortcuts (`Escape`, `Cmd/Ctrl+B/K/P/L/N/Shift+I/,`).
pub fn on_key(s: &mut PiGuiState, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if ev.key == Key::Escape {
        if s.handle_escape() {
            tx.settle();
            return true;
        }
        if s.active_stream.is_some() {
            s.dispatch(PiGuiAction::AbortRun, Some(tx));
            return true;
        }
        return false;
    }

    if ev.mods.cmd && !ev.mods.alt {
        if ev.is_command('b') && !ev.mods.shift {
            s.dispatch(PiGuiAction::ToggleSidebar, Some(tx));
            return true;
        }
        if ev.is_command('i') && ev.mods.shift {
            s.dispatch(PiGuiAction::ToggleInspector, Some(tx));
            return true;
        }
        if ev.is_command('k') && !ev.mods.shift {
            if matches!(s.overlay, Some(OverlayKind::CommandPalette)) {
                s.dispatch(PiGuiAction::CloseOverlay, Some(tx));
            } else {
                s.dispatch(PiGuiAction::OpenOverlay(OverlayKind::CommandPalette), Some(tx));
            }
            return true;
        }
        if (ev.is_command('p') || ev.is_command('l')) && !ev.mods.shift {
            s.dispatch(PiGuiAction::TogglePopover(PopoverKind::ModelPicker), Some(tx));
            return true;
        }
        if ev.is_command(',') && !ev.mods.shift {
            if s.settings_open.is_some() {
                s.dispatch(PiGuiAction::CloseSettings, Some(tx));
            } else {
                s.dispatch(PiGuiAction::OpenSettings(SettingsPage::General), Some(tx));
            }
            return true;
        }
        if ev.is_command('n') && !ev.mods.shift {
            s.dispatch(PiGuiAction::ClosePopover, Some(tx));
            s.dispatch(PiGuiAction::CloseOverlay, Some(tx));
            s.dispatch(PiGuiAction::CloseSettings, Some(tx));
            s.dispatch(PiGuiAction::NewThread, Some(tx));
            return true;
        }
    }
    false
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

fn build_main_header(s: &PiGuiState, sidebar_open: bool, right_open: bool, pal: &WakuPalette) -> El {
    let title = s
        .active_thread_id
        .as_deref()
        .and_then(|id| s.threads.iter().find(|t| t.id == id))
        .map_or("New task", |t| t.title.as_str());

    let pl = if sidebar_open {
        14.0
    } else {
        LayoutMetrics::traffic_light_clearance()
    };

    let mut hdr = row()
        .key("workspace.main.header")
        .role("region")
        .w_full()
        .h(LayoutMetrics::TITLEBAR_HEIGHT)
        .pl(pl)
        .pr(14.0)
        .justify_between()
        .center_y()
        .gap(8.0)
        .shrink()
        .clip_rounded(0.0);

    if !sidebar_open {
        let nav_row = row()
            .center_y()
            .min_w(0.0)
            .grow(1.0)
            .child(header_icon_btn(
                "workspace.main.header.sidebar_toggle",
                "Toggle Sidebar",
                &ICON_PANEL_LEFT,
                pal,
                PiGuiAction::ToggleSidebar,
            ))
            .child(
                header_icon_btn(
                    "workspace.main.header.nav_back",
                    "Previous Session",
                    &ICON_ARROW_LEFT,
                    pal,
                    PiGuiAction::NavigateSessionHistory(-1),
                )
                .ml(6.0),
            )
            .child(
                header_icon_btn(
                    "workspace.main.header.nav_forward",
                    "Next Session",
                    &ICON_ARROW_RIGHT,
                    pal,
                    PiGuiAction::NavigateSessionHistory(1),
                )
                .ml(2.0),
            )
            .child(
                text(title)
                    .key("workspace.main.header.title")
                    .role("heading")
                    .ml(8.0)
                    .font(TextStyle::new(13.5, 500.0).lh(18.0))
                    .color(pal.text)
                    .truncate()
                    .shrink(),
            );
        hdr = hdr.child(nav_row);
    } else {
        hdr = hdr.child(
            text(title)
                .key("workspace.main.header.title")
                .role("heading")
                .font(TextStyle::new(13.5, 500.0).lh(18.0))
                .color(pal.text)
                .truncate()
                .shrink(),
        );
    }

    if !right_open {
        hdr = hdr.child(header_icon_btn(
            "workspace.main.header.right_panel_toggle",
            "Toggle Right Panel",
            &ICON_PANEL_RIGHT,
            pal,
            PiGuiAction::ToggleInspector,
        ));
    }
    hdr
}

fn build_toast_layer(s: &PiGuiState, pal: &WakuPalette) -> Option<El> {
    let toast = s.toasts.last()?;
    let toast_id = toast.id;
    let (status_ic, accent) = match toast.tone {
        crate::state::ToastTone::Info => (&ICON_TOAST_ALERT, pal.gauge),
        crate::state::ToastTone::Success => (&ICON_TOAST_CHECK, pal.success),
        crate::state::ToastTone::Warning => (&ICON_TOAST_ALERT, pal.warning),
        crate::state::ToastTone::Error => (&ICON_TOAST_ALERT, pal.danger),
    };
    let shadow_color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: if pal.is_dark { 0.45 } else { 0.14 },
    };

    let card = row()
        .key("workspace.toast")
        .role("status")
        .max_w(LayoutMetrics::TOAST_MAX_WIDTH)
        .min_w(0.0)
        .py(7.0)
        .px(10.0)
        .gap(8.0)
        .center_y()
        .radius(WakuRadius::TOAST)
        .fill(pal.raised)
        .border_layout()
        .border(1.0, pal.border_strong)
        .shadow(8.0, 24.0, shadow_color)
        .clip_rounded(WakuRadius::TOAST)
        .child(icon(status_ic, 14.0).color(accent).shrink())
        .child(
            text(&toast.message)
                .font(TextStyle::new(12.5, 400.0).lh(16.0))
                .color(pal.text)
                .wrap_anywhere()
                .grow(1.0)
                .min_w(0.0)
                .shrink(),
        )
        .child(
            el().key("workspace.toast.dismiss")
                .role("button")
                .label_static("Dismiss notification")
                .size(26.0, 26.0)
                .shrink()
                .radius(WakuRadius::SM)
                .center()
                .cursor(Cursor::Pointer)
                .hover(pal.overlay)
                .on_press(move |st: &mut PiGuiState| st.dispatch(PiGuiAction::DismissToast(toast_id), None))
                .child(icon(&ICON_CLOSE_SMALL, 12.0).color(pal.text_tertiary)),
        );

    Some(
        row()
            .key("workspace.toast.host")
            .absolute(0.0, LayoutMetrics::TOAST_TOP)
            .w_full()
            .px(16.0)
            .justify_center()
            .child(card),
    )
}

/// Renders Waku's exact 3-pane workspace or full-window 2-pane Settings screen.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, cx: &mut Cx<PiGuiState>) -> El {
    if s.settings_open.is_some() {
        return settings::view(s, k, pal, cx);
    }

    let sidebar_open = s.sidebar_pane.is_open();
    let right_open = s.inspector_pane.is_open();

    let mut main_col = col()
        .key("workspace.main")
        .role("region")
        .grow(1.0)
        .min_w(0.0)
        .min_h(0.0)
        .h_full()
        .fill(pal.canvas)
        .clip_rounded(0.0);

    if sidebar_open {
        main_col = main_col
            .border_layout()
            .border_sides([0.0, 0.0, 0.0, 1.0], pal.sidebar_border);
    }

    main_col = main_col.child(build_main_header(s, sidebar_open, right_open, pal));

    if let Some(banner) = s.status_banner.as_deref().filter(|b| !b.is_empty()) {
        main_col = main_col.child(
            row()
                .key("status-banner")
                .w_full()
                .px(16.0)
                .py(6.0)
                .justify_between()
                .center_y()
                .fill(pal.raised)
                .border_sides([0.0, 0.0, 1.0, 0.0], pal.border)
                .child(text(banner).font(k.ty.small).color(pal.warning).truncate().shrink())
                .child(header_icon_btn(
                    "status-banner-dismiss",
                    "Dismiss banner",
                    &ICON_CLOSE_SMALL,
                    pal,
                    PiGuiAction::SetStatusBanner(None),
                )),
        );
    }

    main_col = main_col
        .child(transcript::view(s, k, pal, cx))
        .child(composer::view(s, k, pal, cx));

    if let Some(toast_layer) = build_toast_layer(s, pal) {
        main_col = main_col.child(toast_layer);
    }

    let mut workspace = row()
        .key("workspace")
        .role("main")
        .w_full()
        .h_full()
        .fill(pal.canvas)
        .clip_rounded(0.0);

    if sidebar_open {
        workspace = workspace.child(sidebar::view(s, k, pal, cx));
    }
    workspace = workspace.child(main_col);
    if right_open {
        workspace = workspace.child(right_panel::view(s, k, pal, cx));
    }
    workspace
}

/// Top-level Rune view callback resolving theme, reduced motion, Waku workspace/settings, and overlays.
pub fn app_view(s: &PiGuiState, cx: &mut Cx<PiGuiState>) -> El {
    if s.reduced_motion {
        cx.reduced_motion(true);
    }
    let dark = s.theme_preference.is_dark(cx.system_dark());
    let pal = palette(dark);
    let k = waku_kit(dark);
    let base = el()
        .key("pi-gui-root")
        .w_full()
        .h_full()
        .min_h(0.0)
        .fill(Color::TRANSPARENT)
        .child(view(s, k, &pal, cx));
    let modal_layer = overlays::view(s, k, &pal, cx);
    let popover_layer = overlays::view_popover(s, k, &pal, cx);
    rune_kit::root(base, modal_layer.or(popover_layer))
}

/// Builds the Rune [`AppSpec`] for `pi-gui`.
#[must_use]
pub fn app_spec(_config: LaunchConfig) -> AppSpec<PiGuiState> {
    AppSpec {
        min_size: Some((LayoutMetrics::MIN_WINDOW_WIDTH, LayoutMetrics::MIN_WINDOW_HEIGHT)),
        scenes: SCENE_ENTRIES,
        fonts: rune_theme_opencode::FONTS,
        families: rune_theme_opencode::FAMILIES,
        setup: Some(setup_state),
        poll: Some(poll_state),
        on_key: Some(on_key),
        ..AppSpec::new(
            "pi",
            (
                LayoutMetrics::DEFAULT_WINDOW_WIDTH,
                LayoutMetrics::DEFAULT_WINDOW_HEIGHT,
            ),
            init_state,
            app_view,
        )
    }
}

/// Launches the `pi-gui` Rune window runtime.
pub fn run() -> anyhow::Result<()> {
    rune::set_app_id("dev.pi.gui");
    let config = LaunchConfig::from_rune_args();
    App::new(app_spec(config))
        .chrome(Chrome {
            full_size_content: true,
            traffic_lights: Some((16.0, 17.0)),
            drag_regions: true,
        })
        .try_run()
        .map_err(|err| anyhow::anyhow!("{err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        ContentBlockView, FileTreeEntryView, MessageRoleView, MessageViewModel, ThreadGroup, ThreadSummary,
        ToolCallStatus, ToolCallViewModel,
    };
    use crate::state::DemoScene;
    use rune::{Env, Mods, Runtime};

    fn test_runtime(state: PiGuiState, size: (f32, f32)) -> Runtime<PiGuiState> {
        let mut rt = Runtime::new(state, app_view, size, Env::new(true, 1.0));
        rt.poll = Some(poll_state);
        rt.on_key = Some(on_key);
        rt.frame(0.0);
        rt
    }

    fn cmd_mods(shift: bool) -> Mods {
        Mods {
            cmd: true,
            shift,
            ..Mods::default()
        }
    }

    fn assert_scene_layout(scene_id: &str, st: PiGuiState) {
        let is_settings = st.settings_open.is_some();
        let sidebar_open = st.sidebar_pane.is_open();
        let is_empty = st.messages.is_empty() && st.active_stream.is_none();
        let rt = test_runtime(st, (1280.0, 820.0));
        let mut keys: Vec<&str> = Vec::new();
        if is_settings {
            keys.extend([
                "settings",
                "settings.sidebar",
                "settings.sidebar.back",
                "settings.content",
                "settings.content.inner",
            ]);
        } else {
            keys.extend([
                "workspace",
                "workspace.main",
                "workspace.main.header",
                "workspace.main.composer",
                "workspace.main.footer",
            ]);
            keys.push(if is_empty {
                "workspace.main.empty_state"
            } else {
                "workspace.main.transcript"
            });
            if sidebar_open {
                keys.extend([
                    "workspace.sidebar",
                    "workspace.sidebar.new_task",
                    "workspace.sidebar.settings",
                ]);
            } else {
                keys.push("workspace.main.header.sidebar_toggle");
            }
        }
        for key in keys {
            let (rect, opacity) = rt
                .presented(key)
                .unwrap_or_else(|| panic!("scene {scene_id} missing element {key}"));
            assert!(
                rect[2] > 0.0 && rect[3] > 0.0,
                "scene {scene_id} collapsed rect for {key}: {rect:?}"
            );
            assert!(opacity > 0.0, "scene {scene_id} zero opacity for {key}");
        }
    }

    #[test]
    fn all_demo_scenes_layout_with_nonzero_bounds() {
        for scene in DemoScene::ALL {
            let mut st = PiGuiState::new(LaunchConfig::parse_args(&["--demo"], "/tmp/pi-workspace"));
            scene.apply(&mut st);
            assert_scene_layout(scene.id(), st);
        }
        for (scene_id, apply_fn) in SCENE_ENTRIES {
            let mut st = PiGuiState::new(LaunchConfig::parse_args(&["--demo"], "/tmp/pi-workspace"));
            apply_fn(&mut st);
            assert_scene_layout(scene_id, st);
        }
    }

    #[test]
    fn long_transcript_scroll_viewport_never_collapses_on_min_window() {
        let mut st = PiGuiState::new(LaunchConfig::parse_args(&["--demo"], "/tmp/pi-workspace"));
        for i in 0..24 {
            st.messages.push(MessageViewModel {
                id: format!("m-{i}"),
                role: if i % 2 == 0 {
                    MessageRoleView::User
                } else {
                    MessageRoleView::Assistant
                },
                timestamp_ms: 1_700_000_000_000 + i * 1000,
                model_label: Some("claude-sonnet".to_string()),
                blocks: vec![
                    ContentBlockView::Thinking("Checking crate layout and Rune flex bounds".to_string()),
                    ContentBlockView::Text(format!(
                        "Message {i}\n\n```rust\nfn line_{i}() -> usize {{ {i} }}\n```\n\n```mermaid\nflowchart LR\n  A --> B\n```"
                    )),
                    ContentBlockView::ToolCall(ToolCallViewModel {
                        id: format!("tc-{i}"),
                        name: "read".to_string(),
                        arguments_preview: "src/lib.rs".to_string(),
                        output_preview: Some("pub mod chrome;".to_string()),
                        status: ToolCallStatus::Success,
                        duration_ms: Some(12),
                        expanded: i == 1,
                    }),
                ],
                is_error: false,
                error_message: None,
            });
        }
        for i in 0..45 {
            st.threads.push(ThreadSummary {
                id: format!("thr-{i}"),
                title: format!("Session thread {i}"),
                cwd: "/tmp/pi-workspace".to_string(),
                model: Some("claude-sonnet".to_string()),
                updated_at_ms: 1_700_000_000_000 + i * 60_000,
                message_count: 4,
                is_streaming: i == 0,
                has_unread: i == 1,
                group: if i < 15 {
                    ThreadGroup::Today
                } else if i < 30 {
                    ThreadGroup::Yesterday
                } else {
                    ThreadGroup::ThisWeek
                },
            });
        }

        let rt = test_runtime(st, (LayoutMetrics::MIN_WINDOW_WIDTH, LayoutMetrics::MIN_WINDOW_HEIGHT));
        let (scroll_rect, _) = rt.presented("workspace.main.transcript").expect("transcript viewport");
        let (composer_rect, _) = rt.presented("workspace.main.composer").expect("composer");
        assert!(scroll_rect[3] > 80.0, "scroll host collapsed: {scroll_rect:?}");
        assert!(composer_rect[3] > 60.0, "composer collapsed: {composer_rect:?}");
    }

    #[test]
    fn keyboard_shortcuts_and_composer_autocomplete_work_headless() {
        let mut st = PiGuiState::new(LaunchConfig::parse_args(&["--demo"], "/tmp/pi-workspace"));
        st.files.push(FileTreeEntryView {
            path: "crates/pi-gui/src/chrome.rs".to_string(),
            name: "chrome.rs".to_string(),
            is_dir: false,
            depth: 3,
            expanded: false,
            modified: true,
        });
        st.composer_edit.replace_text("Inspect @chro");
        st.composer_edit.set_caret("Inspect @chro".len());
        let mut rt = test_runtime(st, (1280.0, 820.0));
        assert!(rt.presented("composer-autocomplete").is_some());

        rt.key(KeyEvent {
            key: Key::Command('k'),
            mods: cmd_mods(false),
        });
        rt.frame(0.016);
        assert!(matches!(rt.state.overlay, Some(OverlayKind::CommandPalette)));

        rt.key(KeyEvent {
            key: Key::Escape,
            mods: Mods::default(),
        });
        rt.frame(0.016);
        assert!(rt.state.overlay.is_none());

        rt.key(KeyEvent {
            key: Key::Command('b'),
            mods: cmd_mods(false),
        });
        rt.frame(0.016);
        assert!(!rt.state.sidebar_pane.is_open());

        rt.key(KeyEvent {
            key: Key::Command('i'),
            mods: cmd_mods(true),
        });
        rt.frame(0.016);
        assert!(rt.state.inspector_pane.is_open());

        rt.key(KeyEvent {
            key: Key::Command(','),
            mods: cmd_mods(false),
        });
        rt.frame(0.016);
        assert!(rt.state.settings_open.is_some());
    }
}
