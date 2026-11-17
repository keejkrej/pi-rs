//! Port of packages/coding-agent/test/scrollbar-theme.test.ts

use std::sync::Arc;

use pi_coding_agent::modes::interactive::theme::theme::{
    ThemeBg, ThemeColor, load_theme_from_path, set_theme_json_validator,
};
use pi_coding_agent::modes::interactive::theme::theme_json::validate_theme_json;
use pi_tui::TerminalColorMode;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use serial_test::serial;

fn load_dark_theme_json() -> Value {
    let raw = include_str!("../src/modes/interactive/theme/dark.json");
    serde_json::from_str(raw).unwrap()
}

struct TempDirGuard(String);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = pi_js::fs::rm(&self.0, true, true);
    }
}

fn write_temp_theme(theme_json: &Value) -> (TempDirGuard, String) {
    let dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-scrollbar-theme-"])).unwrap();
    let name = theme_json.get("name").and_then(Value::as_str).unwrap();
    let path = pi_js::path::join(&[&dir, &format!("{name}.json")]);
    pi_js::fs::write_file(&path, pi_js::json::stringify(theme_json).as_bytes(), None).unwrap();
    (TempDirGuard(dir), path)
}

fn install_validator() {
    set_theme_json_validator(Some(Arc::new(validate_theme_json)));
}

#[test]
#[serial]
fn falls_back_to_muted_and_text_when_scrollbar_tokens_omitted() {
    install_validator();
    for (token, fallback) in [
        (ThemeColor::ScrollbarTrack, ThemeColor::Muted),
        (ThemeColor::ScrollbarThumb, ThemeColor::Text),
    ] {
        let mut theme_json = load_dark_theme_json();
        theme_json["name"] = json!(format!("missing-{}-theme", token.as_str()));
        theme_json["colors"]
            .as_object_mut()
            .unwrap()
            .shift_remove(token.as_str());

        let (_guard, path) = write_temp_theme(&theme_json);
        let loaded = load_theme_from_path(&path, Some(TerminalColorMode::Truecolor)).unwrap();
        assert_eq!(loaded.get_fg_ansi(token), loaded.get_fg_ansi(fallback));
    }
}

#[test]
#[serial]
fn uses_explicitly_configured_scrollbar_colors() {
    install_validator();
    let mut theme_json = load_dark_theme_json();
    theme_json["name"] = json!("custom-scrollbar-theme");
    theme_json["colors"]["scrollbarTrack"] = json!("#654321");
    theme_json["colors"]["scrollbarThumb"] = json!("#123456");

    let (_guard, path) = write_temp_theme(&theme_json);
    let loaded = load_theme_from_path(&path, Some(TerminalColorMode::Truecolor)).unwrap();
    assert_eq!(loaded.get_fg_ansi(ThemeColor::ScrollbarTrack), "\x1b[38;2;101;67;33m");
    assert_eq!(loaded.get_fg_ansi(ThemeColor::ScrollbarThumb), "\x1b[38;2;18;52;86m");
}

#[test]
#[serial]
fn falls_back_to_existing_selection_and_text_colors_for_search_highlights() {
    install_validator();
    let mut theme_json = load_dark_theme_json();
    theme_json["name"] = json!("legacy-search-theme");
    let colors = theme_json["colors"].as_object_mut().unwrap();
    colors.shift_remove("searchMatchBg");
    colors.shift_remove("searchMatchText");

    let (_guard, path) = write_temp_theme(&theme_json);
    let loaded = load_theme_from_path(&path, Some(TerminalColorMode::Truecolor)).unwrap();
    assert_eq!(
        loaded.get_bg_ansi(ThemeBg::SearchMatchBg),
        loaded.get_bg_ansi(ThemeBg::SelectedBg)
    );
    assert_eq!(
        loaded.get_fg_ansi(ThemeColor::SearchMatchText),
        loaded.get_fg_ansi(ThemeColor::Text)
    );
}

#[test]
#[serial]
fn uses_explicitly_configured_search_highlight_colors() {
    install_validator();
    let mut theme_json = load_dark_theme_json();
    theme_json["name"] = json!("custom-search-theme");
    theme_json["colors"]["searchMatchBg"] = json!("#112233");
    theme_json["colors"]["searchMatchText"] = json!("#223344");

    let (_guard, path) = write_temp_theme(&theme_json);
    let loaded = load_theme_from_path(&path, Some(TerminalColorMode::Truecolor)).unwrap();
    assert_eq!(loaded.get_bg_ansi(ThemeBg::SearchMatchBg), "\x1b[48;2;17;34;51m");
    assert_eq!(loaded.get_fg_ansi(ThemeColor::SearchMatchText), "\x1b[38;2;34;51;68m");
}
