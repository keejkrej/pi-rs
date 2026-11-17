//! Port of packages/coding-agent/test/theme-style.test.ts

use pi_coding_agent::modes::interactive::theme::theme::{
    Theme, ThemeAppearance, ThemeBg, ThemeColor, ThemeStyle, ThemeStyleBg, ThemeStyleFg, ThemeToken,
    load_theme_from_path, set_terminal_colors,
};
use pi_tui::{
    Color, OklchColorValue, RgbColor, TerminalColorMode, TerminalColors, TextAttributes, TextStyle, color_to_hex,
    okhsl_color, style_text,
};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use serial_test::serial;

struct ThemeStyleGuard;

impl Drop for ThemeStyleGuard {
    fn drop(&mut self) {
        set_terminal_colors(TerminalColors::default());
    }
}

fn load_edited_theme(base: &str, edit: impl FnOnce(&mut Value)) -> (ThemeStyleGuard, String, Theme) {
    let raw = match base {
        "light" => include_str!("../src/modes/interactive/theme/light.json"),
        _ => include_str!("../src/modes/interactive/theme/dark.json"),
    };
    let mut json_val: Value = serde_json::from_str(raw).unwrap();
    edit(&mut json_val);
    let dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-theme-style-"])).unwrap();
    let name = json_val.get("name").and_then(Value::as_str).unwrap_or("test");
    let path = pi_js::path::join(&[&dir, &format!("{name}.json")]);
    pi_js::fs::write_file(&path, pi_js::json::stringify(&json_val).as_bytes(), None).unwrap();
    let theme = load_theme_from_path(&path, Some(TerminalColorMode::Truecolor)).unwrap();
    let _ = pi_js::fs::rm(&dir, true, true);
    (ThemeStyleGuard, path, theme)
}

#[test]
#[serial]
fn renders_theme_tokens_the_same_as_the_generic_text_styler() {
    let (_guard, _, t) = load_edited_theme("dark", |_| {});
    let actual = t.style(
        "Ready",
        ThemeStyle {
            fg: Some(ThemeStyleFg::Token(ThemeColor::Success)),
            bg: Some(ThemeStyleBg::Token(ThemeBg::ToolSuccessBg)),
            bold: Some(true),
            ..Default::default()
        },
    );
    let expected = style_text(
        "Ready",
        TextStyle {
            fg: Some(t.color(ThemeToken::Fg(ThemeColor::Success)).unwrap()),
            bg: Some(t.color(ThemeToken::Bg(ThemeBg::ToolSuccessBg)).unwrap()),
            bold: Some(true),
            ..Default::default()
        },
        TerminalColorMode::Truecolor,
    );
    assert_eq!(actual, expected);
}

#[test]
#[serial]
fn rejects_unknown_tokens_and_tokens_in_the_wrong_slot() {
    let (_guard, _, t) = load_edited_theme("dark", |_| {});
    let err1 = t
        .try_style_tokens("x", Some("notAToken"), None, TextAttributes::default())
        .unwrap_err();
    assert!(err1.to_string().contains("Unknown theme color: notAToken"));

    let err2 = t
        .try_style_tokens("x", Some("userMessageBg"), None, TextAttributes::default())
        .unwrap_err();
    assert!(err2.to_string().contains("Unknown theme color: userMessageBg"));
}

#[test]
#[serial]
fn loads_oklch_theme_values() {
    let (_guard, _, t) = load_edited_theme("dark", |v| {
        v["colors"]["accent"] = json!("oklch(62% 0.1 200)");
    });
    assert_eq!(
        t.color(ThemeToken::Fg(ThemeColor::Accent)),
        Some(Color::Oklch(OklchColorValue {
            l: 0.62,
            c: 0.1,
            h: 200.0,
        }))
    );
}

#[test]
#[serial]
fn loads_okhsl_theme_values_including_through_variables() {
    let (_guard, _, t) = load_edited_theme("dark", |v| {
        v["vars"]["brand"] = json!("okhsl(250 60% 55%)");
        v["colors"]["accent"] = json!("brand");
        v["colors"]["error"] = json!("okhsl(20 90% 60%)");
    });
    assert_eq!(
        color_to_hex(t.color(ThemeToken::Fg(ThemeColor::Accent)).unwrap()),
        color_to_hex(Color::Rgb(okhsl_color(250.0, 0.6, 0.55).unwrap()))
    );
    assert_eq!(
        color_to_hex(t.color(ThemeToken::Fg(ThemeColor::Error)).unwrap()),
        color_to_hex(Color::Rgb(okhsl_color(20.0, 0.9, 0.6).unwrap()))
    );
}

#[test]
#[serial]
fn detects_the_appearance_unless_it_is_declared() {
    let (_g1, _, dark) = load_edited_theme("dark", |_| {});
    let (_g2, _, light) = load_edited_theme("light", |_| {});
    assert_eq!(dark.appearance(), ThemeAppearance::Dark);
    assert_eq!(light.appearance(), ThemeAppearance::Light);

    for (base, expected) in [("dark", ThemeAppearance::Dark), ("light", ThemeAppearance::Light)] {
        let (_g, _, t) = load_edited_theme(base, |v| {
            v.as_object_mut().unwrap().shift_remove("appearance");
        });
        assert_eq!(t.appearance(), expected);
    }

    let (_g3, _, forced) = load_edited_theme("dark", |v| {
        v["appearance"] = json!("light");
    });
    assert_eq!(forced.appearance(), ThemeAppearance::Light);

    let (_g4, _, palette_only) = load_edited_theme("dark", |v| {
        v.as_object_mut().unwrap().shift_remove("appearance");
        let colors = v["colors"].as_object_mut().unwrap();
        for (k, val) in colors.iter_mut() {
            *val = if k.ends_with("Bg") { json!(0) } else { json!(7) };
        }
    });
    assert_eq!(palette_only.appearance(), ThemeAppearance::Dark);
    set_terminal_colors(TerminalColors {
        background: Some(RgbColor {
            r: 250.0,
            g: 250.0,
            b: 250.0,
        }),
        ..Default::default()
    });
    assert_eq!(palette_only.appearance(), ThemeAppearance::Light);
}

#[test]
#[serial]
fn renders_empty_tokens_as_terminal_defaults_and_reports_concrete_colors_for_them() {
    let (_guard, _, t) = load_edited_theme("dark", |v| {
        v["colors"]["text"] = json!("");
        v["colors"]["userMessageBg"] = json!("");
    });
    assert_eq!(t.fg(ThemeColor::Text, "x"), "\x1b[39mx\x1b[39m");
    assert_eq!(t.bg(ThemeBg::UserMessageBg, "x"), "\x1b[49mx\x1b[49m");
    assert_eq!(
        color_to_hex(t.color(ThemeToken::Fg(ThemeColor::Text)).unwrap()),
        "#e5e5e7"
    );
    assert_eq!(
        color_to_hex(t.color(ThemeToken::Bg(ThemeBg::UserMessageBg)).unwrap()),
        "#000000"
    );

    set_terminal_colors(TerminalColors {
        foreground: Some(RgbColor {
            r: 200.0,
            g: 210.0,
            b: 220.0,
        }),
        background: Some(RgbColor {
            r: 10.0,
            g: 20.0,
            b: 30.0,
        }),
        palette: None,
    });
    assert_eq!(
        color_to_hex(t.color(ThemeToken::Fg(ThemeColor::Text)).unwrap()),
        "#c8d2dc"
    );
    assert_eq!(
        color_to_hex(t.color(ThemeToken::Bg(ThemeBg::UserMessageBg)).unwrap()),
        "#0a141e"
    );
}
