use std::path::PathBuf;

use pi_gui::replication::{SURFACE_TARGETS, write_all_artifacts};
use pi_gui::{
    DemoScene, LaunchConfig, OverlayKind, PiGuiAction, PiGuiState, PopoverKind, app_view, on_key, poll_state,
};
use rune::{Env, Key, KeyEvent, Mods, Runtime};

fn boot_scene(scene: DemoScene, size: (f32, f32), dark: bool) -> Runtime<PiGuiState> {
    let mut cfg = LaunchConfig::parse_args(&["--demo"], "/workspace/pi-rs");
    cfg.reduced_motion = true;
    let mut st = PiGuiState::new(cfg);
    scene.apply(&mut st);
    let mut rt = Runtime::new(st, app_view, size, Env::new(dark, 2.0));
    rt.on_key = Some(on_key);
    rt.poll = Some(poll_state);
    for _ in 0..4 {
        rt.frame(0.016);
    }
    rt
}

#[test]
fn waku_replication_gate_checks_all_35_targets_and_emits_artifacts() {
    assert_eq!(SURFACE_TARGETS.len(), 35);

    for spec in SURFACE_TARGETS {
        let rt = boot_scene(spec.scene, (spec.width, spec.height), spec.dark);
        assert!(
            !rt.elements().is_empty(),
            "missing elements for {}/{}@{}",
            spec.surface,
            spec.state,
            spec.viewport_str()
        );
    }

    // Gate 6 state transitions: ModelPicker first Escape clears query and stays open
    let mut rt = boot_scene(DemoScene::PopoverModelPicker, (1280.0, 820.0), true);
    rt.state.model_search_edit.replace_text("sonnet");
    rt.invalidate();
    rt.frame(0.016);
    rt.key(KeyEvent {
        key: Key::Escape,
        mods: Mods::default(),
    });
    rt.frame(0.016);
    assert!(rt.state.model_search_edit.text.is_empty());
    assert_eq!(rt.state.popover, Some(PopoverKind::ModelPicker));
    rt.key(KeyEvent {
        key: Key::Escape,
        mods: Mods::default(),
    });
    rt.frame(0.016);
    assert_eq!(rt.state.popover, None);

    // CommandPalette filtering and empty state transition
    rt.state
        .dispatch(PiGuiAction::OpenOverlay(OverlayKind::CommandPalette), None);
    rt.invalidate();
    rt.frame(0.016);
    rt.state.palette_edit.replace_text("Settings");
    rt.invalidate();
    rt.frame(0.016);
    assert!(rt.presented("command_palette.row.0").is_some());
    rt.state.palette_edit.replace_text("nonexistent-command-xyz");
    rt.invalidate();
    rt.frame(0.016);
    assert!(rt.presented("command_palette.empty").is_some());

    // Emit live measured artifacts when PI_UI_REPLICATION_DIR is provided (or fallback relative to CARGO_MANIFEST_DIR)
    let out_dir = pi_js::env::var("PI_UI_REPLICATION_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.agents/ui-replication"));
    let count = write_all_artifacts(&out_dir).expect("write replication artifacts");
    assert_eq!(count, 35);
}
