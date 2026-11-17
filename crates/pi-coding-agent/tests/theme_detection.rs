//! Port of packages/coding-agent/test/theme-detection.test.ts

use pi_coding_agent::modes::interactive::theme::theme::{
    TerminalTheme, ThemeColor, detect_color_fg_bg_theme, detect_terminal_theme, get_theme_by_name,
    parse_auto_theme_setting, resolve_theme_setting,
};
use pi_tui::terminal_image::{TerminalCapabilities, reset_capabilities_cache, set_capabilities};
use pi_tui::{RgbColor, TerminalColorMode, TerminalColors};
use pretty_assertions::assert_eq;
use regex::Regex;
use serial_test::serial;

struct CapsResetGuard;

impl Drop for CapsResetGuard {
    fn drop(&mut self) {
        reset_capabilities_cache();
    }
}

#[test]
fn classifies_the_last_field_by_palette_index_like_vim() {
    assert_eq!(detect_color_fg_bg_theme(Some("15;0")), Some(TerminalTheme::Dark));
    assert_eq!(detect_color_fg_bg_theme(Some("0;7;15")), Some(TerminalTheme::Light));
    assert_eq!(detect_color_fg_bg_theme(Some("12;8")), Some(TerminalTheme::Dark));
    assert_eq!(detect_color_fg_bg_theme(Some("15;default")), None);
    assert_eq!(detect_color_fg_bg_theme(Some("")), None);
}

#[test]
fn prefers_background_then_reported_scheme_then_colorfgbg_then_dark() {
    let dark_bg = TerminalColors {
        background: Some(RgbColor { r: 8.0, g: 8.0, b: 8.0 }),
        ..Default::default()
    };
    assert_eq!(
        detect_terminal_theme(&dark_bg, Some(TerminalTheme::Light), Some("0;15")),
        TerminalTheme::Dark
    );
    assert_eq!(
        detect_terminal_theme(&TerminalColors::default(), Some(TerminalTheme::Dark), Some("0;15")),
        TerminalTheme::Dark
    );
    assert_eq!(
        detect_terminal_theme(&TerminalColors::default(), None, Some("0;15")),
        TerminalTheme::Light
    );
    assert_eq!(
        detect_terminal_theme(&TerminalColors::default(), None, Some("")),
        TerminalTheme::Dark
    );
}

#[test]
fn follows_the_foreground_when_text_is_readable_that_way() {
    let background = RgbColor {
        r: 118.0,
        g: 118.0,
        b: 118.0,
    };
    assert_eq!(
        detect_terminal_theme(
            &TerminalColors {
                background: Some(background),
                ..Default::default()
            },
            None,
            None
        ),
        TerminalTheme::Light
    );
    assert_eq!(
        detect_terminal_theme(
            &TerminalColors {
                background: Some(background),
                foreground: Some(RgbColor {
                    r: 255.0,
                    g: 255.0,
                    b: 255.0
                }),
                ..Default::default()
            },
            None,
            None
        ),
        TerminalTheme::Dark
    );
    let mid_gray = RgbColor {
        r: 128.0,
        g: 128.0,
        b: 128.0,
    };
    assert_eq!(
        detect_terminal_theme(
            &TerminalColors {
                background: Some(mid_gray),
                foreground: Some(RgbColor {
                    r: 255.0,
                    g: 255.0,
                    b: 255.0
                }),
                ..Default::default()
            },
            None,
            None
        ),
        TerminalTheme::Light
    );
}

#[test]
#[serial]
fn uses_terminal_capabilities_for_color_mode() {
    let _guard = CapsResetGuard;
    set_capabilities(TerminalCapabilities {
        images: None,
        true_color: false,
        hyperlinks: false,
    });
    let ansi256_theme = get_theme_by_name("dark").unwrap();
    assert_eq!(ansi256_theme.get_color_mode(), TerminalColorMode::N256color);
    let re256 = Regex::new(r"^\x1b\[38;5;\d+m$").unwrap();
    assert!(re256.is_match(&ansi256_theme.get_fg_ansi(ThemeColor::Accent)));

    set_capabilities(TerminalCapabilities {
        images: None,
        true_color: true,
        hyperlinks: false,
    });
    let truecolor_theme = get_theme_by_name("dark").unwrap();
    assert_eq!(truecolor_theme.get_color_mode(), TerminalColorMode::Truecolor);
    let retc = Regex::new(r"^\x1b\[38;2;\d+;\d+;\d+m$").unwrap();
    assert!(retc.is_match(&truecolor_theme.get_fg_ansi(ThemeColor::Accent)));
}

#[test]
fn parses_and_resolves_automatic_theme_settings() {
    assert_eq!(
        parse_auto_theme_setting(Some("light/dark")),
        Some(("light".to_string(), "dark".to_string()))
    );
    assert_eq!(
        resolve_theme_setting(Some("dark"), TerminalTheme::Light),
        Some("dark".to_string())
    );
    assert_eq!(
        resolve_theme_setting(Some("light/dark"), TerminalTheme::Light),
        Some("light".to_string())
    );
    assert_eq!(
        resolve_theme_setting(Some("light/dark"), TerminalTheme::Dark),
        Some("dark".to_string())
    );
    assert_eq!(
        resolve_theme_setting(Some("light/dark/extra"), TerminalTheme::Dark),
        None
    );
}
