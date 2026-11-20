//! Port of packages/coding-agent/test/system-theme.test.ts

use pi_coding_agent::modes::interactive::theme::system_theme::{
    SystemThemeInput, generate_system_theme_colors, wcag_contrast,
};
use pi_coding_agent::modes::interactive::theme::theme::{
    ThemeAppearance, ThemeBg, ThemeColor, ThemeStyle, ThemeStyleFg, ThemeToken, get_available_themes,
    get_theme_by_name, get_theme_export_colors, set_terminal_colors,
};
use pi_coding_agent::modes::interactive::theme::theme_schema::ThemeColorValue;
use pi_tui::colors::ParseColorValue;
use pi_tui::{Color, RgbColor, TerminalColors, color_to_oklch, color_to_rgb, parse_color, rgb_color};
use pretty_assertions::assert_eq;
use regex::Regex;
use serial_test::serial;

fn rgb(hex: &str) -> RgbColor {
    color_to_rgb(parse_color(ParseColorValue::Text(hex)).unwrap())
}

fn lightness(c: RgbColor) -> f64 {
    color_to_oklch(Color::Rgb(rgb_color(c.r, c.g, c.b).unwrap())).l
}

fn hue(c: RgbColor) -> f64 {
    color_to_oklch(Color::Rgb(rgb_color(c.r, c.g, c.b).unwrap())).h
}

fn chroma(c: RgbColor) -> f64 {
    color_to_oklch(Color::Rgb(rgb_color(c.r, c.g, c.b).unwrap())).c
}

fn dracula() -> SystemThemeInput {
    let palette_hexes = [
        "#21222c", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd", "#f8f8f2", "#6272a4", "#ff6e6e",
        "#69ff94", "#ffffa5", "#d6acff", "#ff92df", "#a4ffff", "#ffffff",
    ];
    SystemThemeInput {
        background: Some(rgb("#282a36")),
        foreground: Some(rgb("#f8f8f2")),
        palette: Some(palette_hexes.into_iter().map(rgb).collect()),
        saturation: None,
        appearance_hint: None,
    }
}

fn terminals() -> Vec<(&'static str, SystemThemeInput)> {
    vec![
        ("dracula", dracula()),
        (
            "solarizedLight",
            SystemThemeInput {
                background: Some(rgb("#fdf6e3")),
                foreground: Some(rgb("#657b83")),
                palette: None,
                saturation: None,
                appearance_hint: None,
            },
        ),
        (
            "backgroundOnly",
            SystemThemeInput {
                background: Some(rgb("#1e1e1e")),
                foreground: None,
                palette: None,
                saturation: None,
                appearance_hint: None,
            },
        ),
        (
            "midGray",
            SystemThemeInput {
                background: Some(rgb("#808080")),
                foreground: Some(rgb("#ffffff")),
                palette: None,
                saturation: None,
                appearance_hint: None,
            },
        ),
    ]
}

const PANELS: &[ThemeToken] = &[
    ThemeToken::Bg(ThemeBg::UserMessageBg),
    ThemeToken::Bg(ThemeBg::ToolPendingBg),
    ThemeToken::Bg(ThemeBg::ToolSuccessBg),
    ThemeToken::Bg(ThemeBg::ToolErrorBg),
    ThemeToken::Bg(ThemeBg::SelectedBg),
];

fn resolved(input: &SystemThemeInput, token: ThemeToken) -> RgbColor {
    let generated = generate_system_theme_colors(input);
    match generated.colors.get(&token) {
        Some(ThemeColorValue::String(s)) if s.is_empty() => {
            if PANELS.contains(&token) {
                input.background.unwrap()
            } else {
                input.foreground.unwrap()
            }
        }
        Some(ThemeColorValue::String(s)) => rgb(s),
        other => panic!("expected color string for {token:?}, got {other:?}"),
    }
}

struct TerminalColorsGuard;

impl Drop for TerminalColorsGuard {
    fn drop(&mut self) {
        set_terminal_colors(TerminalColors::default());
    }
}

#[test]
fn keeps_body_text_readable_wcag_4_5_on_background_and_panels() {
    for (name, input) in terminals() {
        let text = resolved(&input, ThemeToken::Fg(ThemeColor::Text));
        for surface in [
            input.background.unwrap(),
            resolved(&input, ThemeToken::Bg(ThemeBg::SelectedBg)),
        ] {
            assert!(wcag_contrast(text, surface) >= 4.5, "{name}: contrast < 4.5 on surface");
        }
        assert!(
            wcag_contrast(
                resolved(&input, ThemeToken::Fg(ThemeColor::ToolTitle)),
                resolved(&input, ThemeToken::Bg(ThemeBg::ToolErrorBg)),
            ) >= 4.5,
            "{name}: toolTitle contrast < 4.5 on toolErrorBg"
        );
    }
}

#[test]
fn orders_foreground_roles_by_contrast_and_keeps_panels_close_to_background() {
    for (name, input) in terminals() {
        let background = lightness(input.background.unwrap());
        let offset = |token: ThemeToken| lightness(resolved(&input, token)) - background;
        if name != "midGray" {
            assert!(
                offset(ThemeToken::Fg(ThemeColor::Text)).abs() > offset(ThemeToken::Fg(ThemeColor::Muted)).abs(),
                "{name}: expected |text| > |muted|"
            );
            assert!(
                offset(ThemeToken::Fg(ThemeColor::Muted)).abs() > offset(ThemeToken::Fg(ThemeColor::Dim)).abs(),
                "{name}: expected |muted| > |dim|"
            );
        }
        let lighter = generate_system_theme_colors(&input).appearance == Some(ThemeAppearance::Dark);
        for &panel in PANELS {
            assert!(
                wcag_contrast(resolved(&input, panel), input.background.unwrap()) < 2.0,
                "{name} {panel:?}: contrast >= 2"
            );
            assert_eq!(offset(panel) > 0.0, lighter, "{name} {panel:?}");
        }
    }
}

#[test]
fn uses_the_terminal_foreground_and_palette_hues() {
    let drac = dracula();
    assert_eq!(
        generate_system_theme_colors(&drac)
            .colors
            .get(&ThemeToken::Fg(ThemeColor::Text)),
        Some(&ThemeColorValue::String(String::new()))
    );
    let sol = SystemThemeInput {
        background: Some(rgb("#fdf6e3")),
        foreground: Some(rgb("#657b83")),
        palette: None,
        saturation: None,
        appearance_hint: None,
    };
    assert_ne!(
        generate_system_theme_colors(&sol)
            .colors
            .get(&ThemeToken::Fg(ThemeColor::Text)),
        Some(&ThemeColorValue::String(String::new()))
    );
    let err_rgb = resolved(&drac, ThemeToken::Fg(ThemeColor::Error));
    let pal1 = drac.palette.unwrap()[1];
    assert!((hue(err_rgb) - hue(pal1)).abs() < 8.0);
}

// #10255
#[test]
fn keeps_pastel_palette_colors_pastel_at_other_lightnesses() {
    let palette_hexes = [
        "#51576d", "#e78284", "#a6d189", "#e5c890", "#8caaee", "#f4b8e4", "#81c8be", "#b5bfe2", "#626880", "#e67172",
        "#8ec772", "#d9ba73", "#7b9ef0", "#f2a4db", "#5abfb5", "#a5adce",
    ];
    let frappe = SystemThemeInput {
        background: Some(rgb("#303446")),
        foreground: Some(rgb("#c6d0f5")),
        palette: Some(palette_hexes.into_iter().map(rgb).collect()),
        saturation: None,
        appearance_hint: None,
    };
    let pink = frappe.palette.as_ref().unwrap()[5];
    let accent = resolved(&frappe, ThemeToken::Fg(ThemeColor::Accent));
    assert!(lightness(accent) < lightness(pink) - 0.05);
    assert!(chroma(accent) <= chroma(pink) * 1.03);
    for panel in [ThemeBg::UserMessageBg, ThemeBg::CustomMessageBg] {
        assert!(chroma(resolved(&frappe, ThemeToken::Bg(panel))) <= 0.1);
    }
}

#[test]
fn renders_grayscale_at_zero_saturation() {
    let mut input = dracula();
    input.saturation = Some(0.0);
    let colors = generate_system_theme_colors(&input).colors;
    let ThemeColorValue::String(err_hex) = colors.get(&ThemeToken::Fg(ThemeColor::Error)).unwrap() else {
        panic!("expected hex string");
    };
    assert!(color_to_oklch(parse_color(ParseColorValue::Text(err_hex)).unwrap()).c < 0.005);
}

#[test]
fn falls_back_to_palette_indices_and_faint_text_without_a_background() {
    let generated = generate_system_theme_colors(&SystemThemeInput {
        appearance_hint: Some(ThemeAppearance::Light),
        ..Default::default()
    });
    assert_eq!(generated.appearance, Some(ThemeAppearance::Light));
    assert_eq!(
        generated.colors.get(&ThemeToken::Fg(ThemeColor::Error)),
        Some(&ThemeColorValue::Indexed(1))
    );
    assert_eq!(
        generated.colors.get(&ThemeToken::Fg(ThemeColor::Text)),
        Some(&ThemeColorValue::String(String::new()))
    );
    assert_eq!(
        generated.colors.get(&ThemeToken::Bg(ThemeBg::UserMessageBg)),
        Some(&ThemeColorValue::String(String::new()))
    );
    assert!(generated.dim.contains(&ThemeColor::Muted));

    let zero_sat = generate_system_theme_colors(&SystemThemeInput {
        saturation: Some(0.0),
        ..Default::default()
    });
    assert_eq!(
        zero_sat.colors.get(&ThemeToken::Fg(ThemeColor::Error)),
        Some(&ThemeColorValue::String(String::new()))
    );
}

#[test]
#[serial]
fn system_theme_is_listed_first_has_no_export_colors_and_is_generated_from_terminal_colors() {
    let _guard = TerminalColorsGuard;
    assert_eq!(get_available_themes().first().map(String::as_str), Some("system"));
    assert_eq!(get_theme_export_colors(Some("system")), Default::default());

    let drac = dracula();
    set_terminal_colors(TerminalColors {
        foreground: drac.foreground,
        background: drac.background,
        palette: drac.palette,
    });
    let sys_theme = get_theme_by_name("system").unwrap();
    assert_eq!(sys_theme.appearance(), ThemeAppearance::Dark);
    assert_eq!(sys_theme.get_fg_ansi(ThemeColor::Text), "\x1b[39m");
    let ansi_re = Regex::new(r"^\x1b\[38;").unwrap();
    assert!(ansi_re.is_match(&sys_theme.get_fg_ansi(ThemeColor::Error)));
}

#[test]
#[serial]
fn renders_faint_tokens_with_sgr_2_and_closes_it() {
    let _guard = TerminalColorsGuard;
    set_terminal_colors(TerminalColors::default());
    let sys_theme = get_theme_by_name("system").unwrap();
    assert_eq!(sys_theme.fg(ThemeColor::Muted, "x"), "\x1b[39m\x1b[2mx\x1b[22;39m");
    assert_eq!(
        sys_theme.style(
            "x",
            ThemeStyle {
                fg: Some(ThemeStyleFg::Token(ThemeColor::Muted)),
                ..Default::default()
            }
        ),
        "\x1b[39m\x1b[2mx\x1b[22m\x1b[39m"
    );
}
