//! Port of packages/tui/src/terminal-colors.ts

#![allow(dead_code, unused_variables)]

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
    todo!("port: hex_to_rgb")
}

fn parse_osc_hex_channel(channel: &str) -> Option<f64> {
    todo!("port: parse_osc_hex_channel")
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

/// Parse an OSC 10, 11, or 4 color reply. Returns undefined when `data` is not such a reply;
/// `rgb` is undefined when it is a reply with an unparseable color.
pub fn parse_osc_color_response(data: &str) -> Option<OscColorResponse> {
    todo!("port: parse_osc_color_response")
}

fn parse_osc_color_value(raw_value: &str) -> Option<RgbColor> {
    todo!("port: parse_osc_color_value")
}

pub fn parse_terminal_color_scheme_report(data: &str) -> Option<TerminalColorScheme> {
    todo!("port: parse_terminal_color_scheme_report")
}
