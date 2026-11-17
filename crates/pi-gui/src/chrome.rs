//! Top-level Rune AppSpec, window chrome, keyboard routing, and split-pane composition.

use rune::{App, AppSpec, Chrome, Color, Cursor, Cx, El, Icon, Key, KeyEvent, Tx, col, el, icon, row, text};
use rune_kit::shell::{INSPECTOR, SidebarToggle, SplitView, Toolbar};
use rune_kit::{Badge, IconButton, IconButtonVariant, Kit};

use crate::bridge::GuiBridge;
use crate::composer;
use crate::overlays;
use crate::right_panel;
use crate::sidebar;
use crate::state::{LaunchConfig, OverlayKind, PiGuiAction, PiGuiState, SCENE_ENTRIES};
use crate::theme::{LayoutMetrics, WakuPalette, palette, waku_kit};
use crate::transcript;

static ICON_COMMAND: Icon = Icon {
    name: "pi-command",
    view: 16.0,
    body: r#"<path d="M5 5a1.5 1.5 0 1 1 1.5 1.5h-1.5V5Zm4.5 1.5V5A1.5 1.5 0 1 1 11 6.5h-1.5ZM6.5 9.5V11A1.5 1.5 0 1 1 5 9.5h1.5Zm3 0H11A1.5 1.5 0 1 1 9.5 11V9.5ZM6.5 6.5h3v3h-3v-3Z" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linejoin="round"/>"#,
};

static ICON_CPU: Icon = Icon {
    name: "pi-model",
    view: 16.0,
    body: r#"<rect x="4" y="4" width="8" height="8" rx="1.5" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M6.5 2v2M9.5 2v2M6.5 12v2M9.5 12v2M2 6.5h2M2 9.5h2M12 6.5h2M12 9.5h2" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};

static ICON_SETTINGS: Icon = Icon {
    name: "pi-settings",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="2.2" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M8 2.2v1.3M8 12.5v1.3M2.2 8h1.3M12.5 8h1.3M3.9 3.9l.9.9M11.2 11.2l.9.9M12.1 3.9l-.9.9M4.8 11.2l-.9.9" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};

static ICON_BRANCH: Icon = Icon {
    name: "pi-branch",
    view: 16.0,
    body: r#"<circle cx="5" cy="4" r="1.5" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="5" cy="12" r="1.5" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="11" cy="6" r="1.5" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M5 5.5v5M11 7.5c0 2-1.5 3-6 3" stroke="currentColor" stroke-width="1.2" fill="none"/>"#,
};

static ICON_CLOSE_SMALL: Icon = Icon {
    name: "pi-close-sm",
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

/// Global keyboard shortcuts (Escape, Cmd/Ctrl+B/K/P/N/Shift+I/,).
pub fn on_key(s: &mut PiGuiState, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if ev.key == Key::Escape {
        if s.overlay.is_some() {
            s.dispatch(PiGuiAction::CloseOverlay, Some(tx));
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
            toggle_overlay(s, OverlayKind::CommandPalette, tx);
            return true;
        }
        if ev.is_command('p') && !ev.mods.shift {
            toggle_overlay(s, OverlayKind::ModelPicker, tx);
            return true;
        }
        if ev.is_command(',') && !ev.mods.shift {
            toggle_overlay(s, OverlayKind::Settings, tx);
            return true;
        }
        if ev.is_command('n') && !ev.mods.shift {
            s.dispatch(PiGuiAction::CloseOverlay, Some(tx));
            s.dispatch(PiGuiAction::NewThread, Some(tx));
            return true;
        }
    }
    false
}

fn toggle_overlay(s: &mut PiGuiState, kind: OverlayKind, tx: &mut Tx) {
    let same = match (&s.overlay, &kind) {
        (Some(OverlayKind::CommandPalette), OverlayKind::CommandPalette)
        | (Some(OverlayKind::ModelPicker), OverlayKind::ModelPicker)
        | (Some(OverlayKind::Settings), OverlayKind::Settings) => true,
        _ => false,
    };
    let next = if same {
        PiGuiAction::CloseOverlay
    } else {
        PiGuiAction::OpenOverlay(kind)
    };
    s.dispatch(next, Some(tx));
}

fn workspace_name(cwd: &str) -> &str {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    trimmed
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(if cwd.is_empty() { "workspace" } else { cwd })
}

fn build_toolbar(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> Toolbar {
    let title = s
        .active_thread_id
        .as_deref()
        .and_then(|id| s.threads.iter().find(|t| t.id == id))
        .map_or("New thread", |t| t.title.as_str());
    let ws_label = workspace_name(&s.config.cwd);

    let mut leading = row()
        .key("toolbar-leading-crumb")
        .gap(8.0)
        .center_y()
        .min_w(0.0)
        .child(
            el().key("toolbar-workspace-pill")
                .px(8.0)
                .py(3.0)
                .radius(k.radius.sm)
                .fill(pal.raised)
                .border(1.0, pal.border)
                .help(s.config.cwd.clone())
                .child(text(ws_label).font(k.ty.small_medium).color(pal.text_secondary)),
        )
        .child(text("/").font(k.ty.small).color(pal.text_ghost))
        .child(
            text(title)
                .key("toolbar-thread-title")
                .font(k.ty.ui_medium)
                .color(pal.text)
                .truncate()
                .shrink(),
        );

    if let Some(branch) = s.diff.branch.as_deref().filter(|b| !b.is_empty()) {
        let mut chip = row()
            .key("toolbar-branch-chip")
            .px(7.0)
            .py(3.0)
            .gap(4.0)
            .center_y()
            .radius(k.radius.sm)
            .fill(pal.inset)
            .border(1.0, pal.border)
            .cursor(Cursor::Pointer)
            .hover(pal.overlay)
            .on_press(|st: &mut PiGuiState| {
                st.dispatch(PiGuiAction::SelectInspectorTab(crate::state::InspectorTab::Diff), None);
            })
            .child(icon(&ICON_BRANCH, 12.0).color(pal.text_tertiary))
            .child(text(branch).font(k.ty.small_medium).color(pal.text_secondary));
        if s.diff.total_additions > 0 || s.diff.total_deletions > 0 {
            chip = chip
                .child(
                    text(format!("+{}", s.diff.total_additions))
                        .font(k.ty.small)
                        .color(pal.success),
                )
                .child(
                    text(format!("-{}", s.diff.total_deletions))
                        .font(k.ty.small)
                        .color(pal.danger),
                );
        }
        leading = leading.child(chip);
    }

    if s.active_stream.is_some() {
        leading = leading.child(Badge::new("Live").variant(rune_kit::BadgeVariant::Accent).build(k));
    }

    let cmd_btn = IconButton::new(&ICON_COMMAND)
        .variant(IconButtonVariant::GhostMuted)
        .label("Command Palette")
        .shortcut(if cfg!(target_os = "macos") { "⌘K" } else { "Ctrl+K" })
        .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::OpenOverlay(OverlayKind::CommandPalette), None))
        .build(k)
        .key("toolbar-cmd-palette");

    let model_btn = IconButton::new(&ICON_CPU)
        .variant(IconButtonVariant::GhostMuted)
        .label("Model Picker")
        .shortcut(if cfg!(target_os = "macos") { "⌘P" } else { "Ctrl+P" })
        .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::OpenOverlay(OverlayKind::ModelPicker), None))
        .build(k)
        .key("toolbar-model-picker");

    let inspector_btn = IconButton::new(&INSPECTOR)
        .variant(IconButtonVariant::GhostMuted)
        .expanded(s.inspector_pane.is_open())
        .label(if s.inspector_pane.is_open() {
            "Hide Inspector"
        } else {
            "Show Inspector"
        })
        .shortcut(if cfg!(target_os = "macos") {
            "⇧⌘I"
        } else {
            "Ctrl+Shift+I"
        })
        .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::ToggleInspector, None))
        .build(k)
        .key("toolbar-inspector-toggle");

    let settings_btn = IconButton::new(&ICON_SETTINGS)
        .variant(IconButtonVariant::GhostMuted)
        .label("Settings")
        .shortcut(if cfg!(target_os = "macos") { "⌘," } else { "Ctrl+," })
        .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::OpenOverlay(OverlayKind::Settings), None))
        .build(k)
        .key("toolbar-settings");

    Toolbar::new()
        .leading(leading)
        .trailing(cmd_btn)
        .trailing(model_btn)
        .trailing(inspector_btn)
        .trailing(settings_btn)
}

/// Renders the primary split-view workspace frame inside Rune's window root.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, cx: &mut Cx<PiGuiState>) -> El {
    let mut main_pane = col().key("main-pane").grow(1.0).min_w(0.0).min_h(0.0).fill(pal.surface);

    if let Some(banner) = s.status_banner.as_deref().filter(|b| !b.is_empty()) {
        main_pane = main_pane.child(
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
                .child(
                    IconButton::new(&ICON_CLOSE_SMALL)
                        .variant(IconButtonVariant::GhostMuted)
                        .label("Dismiss banner")
                        .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::SetStatusBanner(None), None))
                        .build(k),
                ),
        );
    }

    main_pane = main_pane
        .child(transcript::view(s, k, pal, cx))
        .child(composer::view(s, k, pal, cx));

    let unread = s.threads.iter().any(|t| t.has_unread);
    let toolbar = build_toolbar(s, k, pal);

    SplitView::new(main_pane)
        .sidebar(
            &s.sidebar_pane,
            |st: &mut PiGuiState| &mut st.sidebar_pane,
            sidebar::view(s, k, pal, cx),
        )
        .inspector(
            &s.inspector_pane,
            |st: &mut PiGuiState| &mut st.inspector_pane,
            right_panel::view(s, k, pal, cx),
        )
        .toolbar(toolbar)
        .sidebar_toggle(
            SidebarToggle::new()
                .past_lights(10.0)
                .in_header()
                .unread(unread)
                .shortcut(if cfg!(target_os = "macos") { "⌘B" } else { "Ctrl+B" }),
        )
        .frame_inset(0.0)
        .titlebar_inset(cx.titlebar_inset())
        .reduced_motion(s.reduced_motion)
        .build(k)
}

/// Top-level Rune view callback resolving theme, reduced motion, split chrome, and overlays.
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
    let overlay_layers = overlays::view(s, k, &pal, cx);
    rune_kit::root(base, overlay_layers)
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
        let nav_tab = st.nav_tab;
        let rt = test_runtime(st, (1280.0, 820.0));
        let mut keys = vec![
            "main-pane",
            "transcript-scroll-host",
            "composer-card",
            "composer-status-footer",
            "sidebar",
            "sidebar-nav-rail",
        ];
        if nav_tab == crate::state::NavRailTab::Sessions {
            keys.push("new-thread-btn");
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
        let (scroll_rect, _) = rt.presented("transcript-scroll-host").expect("scroll host");
        let (composer_rect, _) = rt.presented("composer-card").expect("composer");
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
        assert!(!rt.state.inspector_pane.is_open());
    }
}
