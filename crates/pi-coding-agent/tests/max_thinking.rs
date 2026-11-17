//! Port of packages/coding-agent/test/max-thinking.test.ts (unit coding-agent-interactive-theme)

// @generated-mods begin (scaffold-owned, do not edit)
#[path = "max_thinking/part_2.rs"]
mod part_2;
// @generated-mods end

use pi_coding_agent::modes::interactive::theme::theme::load_theme_from_path;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

#[test]
fn falls_back_to_thinking_xhigh_for_legacy_themes() {
    let dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-max-theme-"])).unwrap();
    let mut dark_theme: Value = serde_json::from_str(include_str!("../src/modes/interactive/theme/dark.json")).unwrap();
    dark_theme["name"] = json!("legacy-theme");
    dark_theme["colors"]
        .as_object_mut()
        .unwrap()
        .shift_remove("thinkingMax");
    let theme_path = pi_js::path::join(&[&dir, "legacy-theme.json"]);
    pi_js::fs::write_file(&theme_path, pi_js::json::stringify(&dark_theme).as_bytes(), None).unwrap();

    let legacy_theme = load_theme_from_path(&theme_path, None).unwrap();
    assert_eq!(
        legacy_theme.get_thinking_border_color_by_name("max")("border"),
        legacy_theme.get_thinking_border_color_by_name("xhigh")("border"),
    );

    let _ = pi_js::fs::rm(&dir, true, true);
}
