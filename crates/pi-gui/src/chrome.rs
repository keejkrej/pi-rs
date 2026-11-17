//! Top-level Rune AppSpec, window chrome, keyboard routing, and split-pane composition.

use rune::{AppSpec, Cx, El, Key, KeyEvent, Tx, col, el, row, text};
use rune_kit::Kit;
use rune_kit::shell::{SplitView, Toolbar};

use crate::bridge::GuiBridge;
use crate::composer;
use crate::overlays;
use crate::right_panel;
use crate::sidebar;
use crate::state::{DemoScene, LaunchConfig, OverlayKind, PiGuiAction, PiGuiState};
use crate::theme::{LayoutMetrics, WakuPalette, palette, waku_kit};
use crate::transcript;

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

/// Global keyboard shortcuts (Escape closes overlays, Cmd/Ctrl+B toggles sidebar, Cmd/Ctrl+K command palette).
pub fn on_key(s: &mut PiGuiState, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if ev.key == Key::Escape && s.overlay.is_some() {
        s.dispatch(PiGuiAction::CloseOverlay, Some(tx));
        return true;
    }
    if ev.mods.cmd && !ev.mods.alt {
        match ev.key {
            Key::Command('b') if !ev.mods.shift => {
                s.dispatch(PiGuiAction::ToggleSidebar, Some(tx));
                return true;
            }
            Key::Command('k') if !ev.mods.shift => {
                let next = if matches!(s.overlay, Some(OverlayKind::CommandPalette)) {
                    PiGuiAction::CloseOverlay
                } else {
                    PiGuiAction::OpenOverlay(OverlayKind::CommandPalette)
                };
                s.dispatch(next, Some(tx));
                return true;
            }
            Key::Command(',') if !ev.mods.shift => {
                s.dispatch(PiGuiAction::OpenOverlay(OverlayKind::Settings), Some(tx));
                return true;
            }
            _ => {}
        }
    }
    false
}

/// Renders the primary split-view workspace frame inside Rune's window root.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, cx: &mut Cx<PiGuiState>) -> El {
    let main_pane = col()
        .key("main-pane")
        .grow(1.0)
        .min_h(0.0)
        .fill(pal.surface)
        .child(transcript::view(s, k, pal, cx))
        .child(composer::view(s, k, pal, cx));

    let title = s
        .active_thread_id
        .as_deref()
        .and_then(|id| s.threads.iter().find(|t| t.id == id))
        .map_or("pi-gui", |t| t.title.as_str());

    let toolbar = Toolbar::new()
        .leading(
            row()
                .key("toolbar-title")
                .gap(8.0)
                .center_y()
                .child(text(title).font(k.ty.ui_medium).color(pal.text))
                .child(text(&s.config.cwd).font(k.ty.small).color(pal.text_tertiary).truncate()),
        )
        .trailing(
            el().key("toolbar-inspector-toggle")
                .px(8.0)
                .py(4.0)
                .radius(k.radius.sm)
                .hover(pal.overlay)
                .on_press(|s: &mut PiGuiState| s.dispatch(PiGuiAction::ToggleInspector, None))
                .child(text("Inspector").font(k.ty.small_medium).color(pal.text_secondary)),
        );

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
    let base = view(s, k, &pal, cx);
    let overlay_layers = overlays::view(s, k, &pal, cx);
    rune_kit::root(base, overlay_layers)
}

static SCENES: &[(&str, fn(&mut PiGuiState))] = &[
    (DemoScene::Default.id(), |s| DemoScene::Default.apply(s)),
    (DemoScene::Empty.id(), |s| DemoScene::Empty.apply(s)),
    (DemoScene::Streaming.id(), |s| DemoScene::Streaming.apply(s)),
    (DemoScene::DiffReview.id(), |s| DemoScene::DiffReview.apply(s)),
    (DemoScene::Overlays.id(), |s| DemoScene::Overlays.apply(s)),
    (DemoScene::Settings.id(), |s| DemoScene::Settings.apply(s)),
    (DemoScene::Plugins.id(), |s| DemoScene::Plugins.apply(s)),
    (DemoScene::LightTheme.id(), |s| DemoScene::LightTheme.apply(s)),
];

/// Builds the Rune [`AppSpec`] for `pi-gui`.
#[must_use]
pub fn app_spec(_config: LaunchConfig) -> AppSpec<PiGuiState> {
    AppSpec {
        min_size: Some((LayoutMetrics::MIN_WINDOW_WIDTH, LayoutMetrics::MIN_WINDOW_HEIGHT)),
        scenes: SCENES,
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
    rune::run(app_spec(config));
    Ok(())
}
