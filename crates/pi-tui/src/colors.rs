//! Port of packages/tui/src/colors.ts

#![allow(dead_code, unused_variables)]

use serde::{Deserialize, Serialize};

use crate::oklab::rgb_to_okhsl;
use crate::terminal_colors::RgbColor;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedColor {
    pub index: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RgbColorValue {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct OklchColorValue {
    pub l: f64,
    pub c: f64,
    pub h: f64,
}

/// A concrete color. Every color can be converted to sRGB, so color math never fails.
///
/// PORT: TS union `IndexedColor | RgbColorValue | OklchColorValue`. `kind` is the serde tag.
/// [`From`] wraps each struct, which is what `indexed_color` / `rgb_color` / `oklch_color` return.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Color {
    #[serde(rename = "indexed")]
    Indexed(IndexedColor),
    #[serde(rename = "rgb")]
    Rgb(RgbColorValue),
    #[serde(rename = "oklch")]
    Oklch(OklchColorValue),
}

impl From<IndexedColor> for Color {
    fn from(value: IndexedColor) -> Self {
        Self::Indexed(value)
    }
}

impl From<RgbColorValue> for Color {
    fn from(value: RgbColorValue) -> Self {
        Self::Rgb(value)
    }
}

impl From<OklchColorValue> for Color {
    fn from(value: OklchColorValue) -> Self {
        Self::Oklch(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalColorMode {
    /// PORT: variant cannot start with a digit. Wire value is `"256color"`.
    #[serde(rename = "256color")]
    N256color,
    #[serde(rename = "truecolor")]
    Truecolor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorMixSpace {
    #[serde(rename = "oklch")]
    Oklch,
    #[serde(rename = "srgb")]
    Srgb,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct OklchChannels {
    pub l: f64,
    pub c: f64,
    pub h: f64,
}

/// OKHSL channels: hue in degrees, saturation and lightness 0-1. Saturation is relative to the most the
/// sRGB gamut allows at that hue and lightness, so every value is in gamut.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct OkhslChannels {
    pub h: f64,
    pub s: f64,
    pub l: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextAttributes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dim: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underline: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inverse: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strikethrough: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dim: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underline: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inverse: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strikethrough: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<Color>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<Color>,
}

/// `string | number` argument of [`parse_color`].
///
/// PORT: variants follow the TS check order (`number`, then `string`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParseColorValue<'a> {
    Number(f64),
    Text(&'a str),
}

fn require_finite(value: f64, name: &str) -> pi_js::Result<()> {
    todo!("port: require_finite")
}

pub fn indexed_color(index: f64) -> pi_js::Result<IndexedColor> {
    todo!("port: indexed_color")
}

pub fn rgb_color(r: f64, g: f64, b: f64) -> pi_js::Result<RgbColorValue> {
    todo!("port: rgb_color")
}

pub fn oklch_color(l: f64, c: f64, h: f64) -> pi_js::Result<OklchColorValue> {
    todo!("port: oklch_color")
}

const NUMBER_PATTERN: &str = r"[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?";
// PORT: second `RegExp` argument is `"i"`. `\d` is JS `[0-9]` (translate when compiling).
const OKLCH_PATTERN: &str = r"^oklch\(\s*([+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?)(%)?\s+([+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?)\s+([+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?)(?:deg)?\s*\)$";
// PORT: second `RegExp` argument is `"i"`. `\d` is JS `[0-9]` (translate when compiling).
const OKHSL_PATTERN: &str = r"^okhsl\(\s*([+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?)(?:deg)?\s+([+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?)(%)?\s+([+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?)(%)?\s*\)$";

/// An OKHSL color, converted to sRGB. Saturation is relative to the sRGB gamut at the hue and lightness,
/// so equal saturation looks equally colorful across hues and lightness.
/// @param h Hue in degrees.
/// @param s Saturation, 0-1.
/// @param l Lightness, 0-1.
pub fn okhsl_color(h: f64, s: f64, l: f64) -> pi_js::Result<RgbColorValue> {
    todo!("port: okhsl_color")
}

pub fn color_to_okhsl(color: Color) -> OkhslChannels {
    rgb_to_okhsl(color_to_rgb(color))
}

pub fn parse_color(value: ParseColorValue<'_>) -> pi_js::Result<Color> {
    todo!("port: parse_color")
}

const BASIC_COLORS: [RgbColor; 16] = [
    RgbColor { r: 0.0, g: 0.0, b: 0.0 },
    RgbColor {
        r: 128.0,
        g: 0.0,
        b: 0.0,
    },
    RgbColor {
        r: 0.0,
        g: 128.0,
        b: 0.0,
    },
    RgbColor {
        r: 128.0,
        g: 128.0,
        b: 0.0,
    },
    RgbColor {
        r: 0.0,
        g: 0.0,
        b: 128.0,
    },
    RgbColor {
        r: 128.0,
        g: 0.0,
        b: 128.0,
    },
    RgbColor {
        r: 0.0,
        g: 128.0,
        b: 128.0,
    },
    RgbColor {
        r: 192.0,
        g: 192.0,
        b: 192.0,
    },
    RgbColor {
        r: 128.0,
        g: 128.0,
        b: 128.0,
    },
    RgbColor {
        r: 255.0,
        g: 0.0,
        b: 0.0,
    },
    RgbColor {
        r: 0.0,
        g: 255.0,
        b: 0.0,
    },
    RgbColor {
        r: 255.0,
        g: 255.0,
        b: 0.0,
    },
    RgbColor {
        r: 0.0,
        g: 0.0,
        b: 255.0,
    },
    RgbColor {
        r: 255.0,
        g: 0.0,
        b: 255.0,
    },
    RgbColor {
        r: 0.0,
        g: 255.0,
        b: 255.0,
    },
    RgbColor {
        r: 255.0,
        g: 255.0,
        b: 255.0,
    },
];
const CUBE_VALUES: [i64; 6] = [0, 95, 135, 175, 215, 255];
const GRAY_VALUES: [i64; 24] = [
    8, 18, 28, 38, 48, 58, 68, 78, 88, 98, 108, 118, 128, 138, 148, 158, 168, 178, 188, 198, 208, 218, 228, 238,
];

fn indexed_to_rgb(index: i64) -> RgbColor {
    todo!("port: indexed_to_rgb")
}

fn is_in_srgb_gamut(linear: &[f64]) -> bool {
    todo!("port: is_in_srgb_gamut")
}

fn oklch_to_rgb(color: OklchChannels) -> RgbColor {
    todo!("port: oklch_to_rgb")
}

pub fn color_to_rgb(color: Color) -> RgbColor {
    todo!("port: color_to_rgb")
}

pub fn color_to_oklch(color: Color) -> OklchChannels {
    todo!("port: color_to_oklch")
}

pub fn color_to_hex(color: Color) -> String {
    todo!("port: color_to_hex")
}

/// PORT: `space: None` selects `"oklch"`, the TS default.
pub fn mix_colors(first: Color, second: Color, amount: f64, space: Option<ColorMixSpace>) -> pi_js::Result<Color> {
    todo!("port: mix_colors")
}

fn find_closest(values: &[i64], target: f64) -> i64 {
    todo!("port: find_closest")
}

fn color_distance(first: RgbColor, second: RgbColor) -> f64 {
    let dr = first.r - second.r;
    let dg = first.g - second.g;
    let db = first.b - second.b;
    dr * dr * 0.299 + dg * dg * 0.587 + db * db * 0.114
}

fn rgb_to_ansi256(color: RgbColor) -> i64 {
    todo!("port: rgb_to_ansi256")
}

fn color_ansi(color: Color, mode: TerminalColorMode, background: bool) -> String {
    todo!("port: color_ansi")
}

pub fn foreground_ansi(color: Color, mode: TerminalColorMode) -> String {
    color_ansi(color, mode, false)
}

pub fn background_ansi(color: Color, mode: TerminalColorMode) -> String {
    color_ansi(color, mode, true)
}

pub fn style_text(text: &str, options: TextStyle, mode: TerminalColorMode) -> String {
    todo!("port: style_text")
}

/// Like `styleText()`, but with precomputed color escape sequences, e.g. cached theme colors.
/// Colors in `options` are ignored.
pub fn style_text_with_ansi(
    text: &str,
    fg_ansi: Option<&str>,
    bg_ansi: Option<&str>,
    options: TextAttributes,
) -> String {
    todo!("port: style_text_with_ansi")
}
