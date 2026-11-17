//! Port of packages/tui/src/terminal-colors.ts

#![allow(dead_code)]

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RgbColor {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalColorScheme {
    #[serde(rename = "dark")]
    Dark,
    #[serde(rename = "light")]
    Light,
}

/// Colors the terminal reports for its current theme.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalColors {
    /// Default foreground (OSC 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground: Option<RgbColor>,
    /// Default background (OSC 11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<RgbColor>,
    /// ANSI colors 0-15 (OSC 4). Only set when the terminal reported all 16.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<Vec<RgbColor>>,
}

fn hex_to_rgb(hex: &str) -> RgbColor {
    let normalized = hex.strip_prefix('#').unwrap_or(hex);
    let r = pi_js::num::parse_int(&pi_js::str16::slice(normalized, 0, Some(2)), Some(16));
    let g = pi_js::num::parse_int(&pi_js::str16::slice(normalized, 2, Some(4)), Some(16));
    let b = pi_js::num::parse_int(&pi_js::str16::slice(normalized, 4, Some(6)), Some(16));
    RgbColor { r, g, b }
}

fn parse_osc_hex_channel(channel: &str) -> Option<f64> {
    if channel.is_empty() || !channel.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let max = 16.0_f64.powf(channel.len() as f64) - 1.0;
    if max <= 0.0 {
        return None;
    }
    Some(pi_js::num::math_round(
        (pi_js::num::parse_int(channel, Some(16)) / max) * 255.0,
    ))
}

/// What an OSC color reply reports: the default foreground (OSC 10), background (OSC 11), or a palette index (OSC 4).
///
/// PORT: `"foreground" | "background" | number`. A palette index is [`OscColorTarget::Index`].
/// TS `String(target)` is `"foreground"`, `"background"`, or the decimal index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OscColorTarget {
    Foreground,
    Background,
    Index(i64),
}

/// `{ target, rgb }` from [`parse_osc_color_response`]. `rgb` is `None` when the reply's color is unparseable.
///
/// PORT: TS returns this anonymous object, or `undefined` (`None`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OscColorResponse {
    pub target: OscColorTarget,
    pub rgb: Option<RgbColor>,
}

// PORT: `i` flag. `\d` is JS `[0-9]` (translate when compiling).
const OSC_COLOR_RESPONSE_PATTERN: &str = r"^\x1b\](?:(1[01])|4;(\d{1,3}));([^\x07\x1b]*)(?:\x07|\x1b\\)$";
const COLOR_SCHEME_REPORT_PATTERN: &str = r"^(?:\x1b\[\?997;(1|2)n)+$";

static OSC_COLOR_RESPONSE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    let pattern = format!("(?i){}", OSC_COLOR_RESPONSE_PATTERN.replace("\\d", "[0-9]"));
    regex::Regex::new(&pattern).expect("OSC_COLOR_RESPONSE_PATTERN compiles")
});

static COLOR_SCHEME_REPORT_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(COLOR_SCHEME_REPORT_PATTERN).expect("COLOR_SCHEME_REPORT_PATTERN compiles"));

static RGBA_PREFIX_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new("(?i)^rgba?:").expect("RGBA_PREFIX_RE compiles"));

/// Parse an OSC 10, 11, or 4 color reply. Returns undefined when `data` is not such a reply;
/// `rgb` is undefined when it is a reply with an unparseable color.
pub fn parse_osc_color_response(data: &str) -> Option<OscColorResponse> {
    let caps = OSC_COLOR_RESPONSE_RE.captures(data)?;
    let g1 = caps.get(1).map(|m| m.as_str());
    let g2 = caps.get(2).map(|m| m.as_str());
    let g3 = caps.get(3).map(|m| m.as_str()).unwrap_or("");
    let target = match g1 {
        Some("10") => OscColorTarget::Foreground,
        Some("11") => OscColorTarget::Background,
        _ => OscColorTarget::Index(pi_js::num::parse_int(g2.unwrap_or(""), Some(10)) as i64),
    };
    Some(OscColorResponse {
        target,
        rgb: parse_osc_color_value(g3),
    })
}

fn parse_osc_color_value(raw_value: &str) -> Option<RgbColor> {
    let value = pi_js::str16::trim(raw_value);
    if let Some(hex) = value.strip_prefix('#') {
        if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(hex_to_rgb(value));
        }
        if hex.len() == 12 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let r = parse_osc_hex_channel(&hex[0..4]);
            let g = parse_osc_hex_channel(&hex[4..8]);
            let b = parse_osc_hex_channel(&hex[8..12]);
            return match (r, g, b) {
                (Some(r), Some(g), Some(b)) => Some(RgbColor { r, g, b }),
                _ => None,
            };
        }
        return None;
    }

    let rgb_value = RGBA_PREFIX_RE.replace(value, "");
    let mut parts = rgb_value.split('/');
    let (Some(red), Some(green), Some(blue)) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    let (Some(r), Some(g), Some(b)) = (
        parse_osc_hex_channel(red),
        parse_osc_hex_channel(green),
        parse_osc_hex_channel(blue),
    ) else {
        return None;
    };
    Some(RgbColor { r, g, b })
}

pub fn parse_terminal_color_scheme_report(data: &str) -> Option<TerminalColorScheme> {
    let caps = COLOR_SCHEME_REPORT_RE.captures(data)?;
    let g1 = caps.get(1).map(|m| m.as_str());
    Some(if g1 == Some("2") {
        TerminalColorScheme::Light
    } else {
        TerminalColorScheme::Dark
    })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn parses_hex_and_osc_responses_and_values() {
        assert_eq!(
            hex_to_rgb("#0f80ff"),
            RgbColor {
                r: 15.0,
                g: 128.0,
                b: 255.0
            }
        );
        assert_eq!(parse_osc_hex_channel("f"), Some(255.0));
        assert_eq!(parse_osc_hex_channel("8000"), Some(128.0));
        assert_eq!(parse_osc_hex_channel(""), None);
        assert_eq!(parse_osc_hex_channel("xyz"), None);

        assert_eq!(
            parse_osc_color_response("\x1b]11;rgb:1e1e/2e2e/3e3e\x07"),
            Some(OscColorResponse {
                target: OscColorTarget::Background,
                rgb: Some(RgbColor {
                    r: 30.0,
                    g: 46.0,
                    b: 62.0
                }),
            })
        );
        assert_eq!(
            parse_osc_color_response("\x1b]10;#abcdef\x1b\\"),
            Some(OscColorResponse {
                target: OscColorTarget::Foreground,
                rgb: Some(RgbColor {
                    r: 171.0,
                    g: 205.0,
                    b: 239.0
                }),
            })
        );
        assert_eq!(
            parse_osc_color_response("\x1b]4;12;rgba:1000/2000/3000/ffff\x07"),
            Some(OscColorResponse {
                target: OscColorTarget::Index(12),
                rgb: Some(RgbColor {
                    r: 16.0,
                    g: 32.0,
                    b: 48.0
                }),
            })
        );
        assert_eq!(
            parse_osc_color_response("\x1b]10;invalid\x07"),
            Some(OscColorResponse {
                target: OscColorTarget::Foreground,
                rgb: None,
            })
        );
        assert_eq!(parse_osc_color_response("not an osc reply"), None);

        assert_eq!(
            parse_osc_color_value("#120034005600"),
            Some(RgbColor {
                r: 18.0,
                g: 52.0,
                b: 86.0
            })
        );
        assert_eq!(parse_osc_color_value("invalid"), None);
    }

    #[test]
    fn parses_terminal_color_scheme_reports() {
        assert_eq!(
            parse_terminal_color_scheme_report("\x1b[?997;1n"),
            Some(TerminalColorScheme::Dark)
        );
        assert_eq!(
            parse_terminal_color_scheme_report("\x1b[?997;2n"),
            Some(TerminalColorScheme::Light)
        );
        assert_eq!(parse_terminal_color_scheme_report("\x1b[?997;3n"), None);
    }
}
