//! Port of packages/tui/test/colors.test.ts

use pi_tui::colors::ParseColorValue;
use pi_tui::{
    Color, OklchColorValue, RgbColor, RgbColorValue, TerminalColorMode, TextStyle, color_to_hex, color_to_okhsl,
    color_to_rgb, indexed_color, okhsl_color, oklch_color, parse_color, rgb_color, style_text,
};
use regex::Regex;

#[test]
fn parses_hex_and_oklch_colors_and_rejects_everything_else() {
    assert_eq!(
        parse_color(ParseColorValue::Text("#abc")).unwrap(),
        Color::Rgb(RgbColorValue {
            r: 170.0,
            g: 187.0,
            b: 204.0,
        })
    );
    assert_eq!(
        parse_color(ParseColorValue::Text("oklch(62% 0.1 200)")).unwrap(),
        Color::Oklch(OklchColorValue {
            l: 0.62,
            c: 0.1,
            h: 200.0,
        })
    );
    let err_empty = parse_color(ParseColorValue::Text("")).unwrap_err().to_string();
    assert!(err_empty.contains("Invalid color value"), "{err_empty}");
    let err_red = parse_color(ParseColorValue::Text("red")).unwrap_err().to_string();
    assert!(err_red.contains("Invalid color value"), "{err_red}");
}

#[test]
fn gamut_maps_oklch_to_srgb_including_the_lightness_limits() {
    assert_eq!(
        color_to_rgb(Color::Oklch(oklch_color(0.627955, 0.257683, 29.2339).unwrap())),
        RgbColor {
            r: 255.0,
            g: 0.0,
            b: 0.0,
        }
    );
    assert_eq!(
        color_to_rgb(Color::Oklch(oklch_color(1.0, 0.3, 150.0).unwrap())),
        RgbColor {
            r: 255.0,
            g: 255.0,
            b: 255.0,
        }
    );
    assert_eq!(
        color_to_rgb(Color::Oklch(oklch_color(0.0, 0.3, 150.0).unwrap())),
        RgbColor { r: 0.0, g: 0.0, b: 0.0 }
    );
}

#[test]
fn parses_okhsl_colors_and_round_trips_them() {
    // Full saturation at the red cusp is pure sRGB red.
    assert_eq!(
        parse_color(ParseColorValue::Text("okhsl(29.23 100% 56.8%)")).unwrap(),
        Color::Rgb(rgb_color(255.0, 0.0, 0.0).unwrap())
    );
    assert_eq!(
        parse_color(ParseColorValue::Text("OKHSL(250deg 60% 55%)")).unwrap(),
        Color::Rgb(okhsl_color(250.0, 0.6, 0.55).unwrap())
    );
    let err_sat = parse_color(ParseColorValue::Text("okhsl(250 160% 55%)"))
        .unwrap_err()
        .to_string();
    assert!(err_sat.contains("s must be between 0 and 1"), "{err_sat}");
    for hex in ["#4f8eb3", "#20242a", "#f8f9fa"] {
        let parsed = parse_color(ParseColorValue::Text(hex)).unwrap();
        let channels = color_to_okhsl(parsed);
        assert_eq!(
            color_to_hex(Color::Rgb(okhsl_color(channels.h, channels.s, channels.l).unwrap())),
            hex
        );
    }
}

#[test]
fn styles_text_and_closes_sequences_in_reverse_order() {
    assert_eq!(
        style_text(
            "Ready",
            TextStyle {
                fg: Some(Color::Rgb(rgb_color(18.0, 52.0, 86.0).unwrap())),
                bg: Some(Color::Indexed(indexed_color(9.0).unwrap())),
                bold: Some(true),
                italic: Some(true),
                ..Default::default()
            },
            TerminalColorMode::Truecolor,
        ),
        "\x1b[38;2;18;52;86m\x1b[48;5;9m\x1b[1m\x1b[3mReady\x1b[23m\x1b[22m\x1b[49m\x1b[39m"
    );
    let ansi_256 = style_text(
        "Ready",
        TextStyle {
            fg: Some(Color::Rgb(rgb_color(18.0, 52.0, 86.0).unwrap())),
            ..Default::default()
        },
        TerminalColorMode::N256color,
    );
    assert!(
        Regex::new(r"^\x1b\[38;5;[0-9]+mReady\x1b\[39m$")
            .unwrap()
            .is_match(&ansi_256),
        "{ansi_256:?}"
    );
}
