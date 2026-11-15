//! Port of packages/tui/src/colors.ts

#![allow(dead_code)]

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::oklab::{linear_srgb_to_rgb, okhsl_to_rgb, oklab_to_linear_srgb, rgb_to_okhsl, rgb_to_oklab};
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
    if !value.is_finite() {
        return Err(pi_js::Error::msg(format!("{name} must be finite")));
    }
    Ok(())
}

pub fn indexed_color(index: f64) -> pi_js::Result<IndexedColor> {
    if !pi_js::num::is_integer(index) || index < 0.0 || index > 255.0 {
        return Err(pi_js::Error::msg(format!(
            "ANSI color index must be an integer from 0 to 255: {}",
            pi_js::num::to_js_string(index)
        )));
    }
    Ok(IndexedColor { index: index as i64 })
}

pub fn rgb_color(r: f64, g: f64, b: f64) -> pi_js::Result<RgbColorValue> {
    for (name, value) in [("r", r), ("g", g), ("b", b)] {
        require_finite(value, name)?;
        if value < 0.0 || value > 255.0 {
            return Err(pi_js::Error::msg(format!(
                "{name} must be between 0 and 255: {}",
                pi_js::num::to_js_string(value)
            )));
        }
    }
    Ok(RgbColorValue { r, g, b })
}

pub fn oklch_color(l: f64, c: f64, h: f64) -> pi_js::Result<OklchColorValue> {
    require_finite(l, "l")?;
    require_finite(c, "c")?;
    require_finite(h, "h")?;
    if l < 0.0 || l > 1.0 {
        return Err(pi_js::Error::msg(format!(
            "l must be between 0 and 1: {}",
            pi_js::num::to_js_string(l)
        )));
    }
    if c < 0.0 {
        return Err(pi_js::Error::msg(format!(
            "c must not be negative: {}",
            pi_js::num::to_js_string(c)
        )));
    }
    Ok(OklchColorValue {
        l,
        c,
        h: ((h % 360.0) + 360.0) % 360.0,
    })
}

const NUMBER_PATTERN: &str = r"[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?";
// PORT: second `RegExp` argument is `"i"`. `\d` is JS `[0-9]` (translate when compiling).
const OKLCH_PATTERN: &str = r"^oklch\(\s*([+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?)(%)?\s+([+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?)\s+([+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?)(?:deg)?\s*\)$";
// PORT: second `RegExp` argument is `"i"`. `\d` is JS `[0-9]` (translate when compiling).
const OKHSL_PATTERN: &str = r"^okhsl\(\s*([+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?)(?:deg)?\s+([+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?)(%)?\s+([+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?)(%)?\s*\)$";

static OKLCH_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    let translated = format!(
        "(?i){}",
        OKLCH_PATTERN
            .replace("\\d", "[0-9]")
            .replace("\\s", pi_js::regex::JS_WS_CLASS)
    );
    regex::Regex::new(&translated).expect("OKLCH_PATTERN compiles")
});

static OKHSL_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    let translated = format!(
        "(?i){}",
        OKHSL_PATTERN
            .replace("\\d", "[0-9]")
            .replace("\\s", pi_js::regex::JS_WS_CLASS)
    );
    regex::Regex::new(&translated).expect("OKHSL_PATTERN compiles")
});

/// An OKHSL color, converted to sRGB. Saturation is relative to the sRGB gamut at the hue and lightness,
/// so equal saturation looks equally colorful across hues and lightness.
/// @param h Hue in degrees.
/// @param s Saturation, 0-1.
/// @param l Lightness, 0-1.
pub fn okhsl_color(h: f64, s: f64, l: f64) -> pi_js::Result<RgbColorValue> {
    require_finite(h, "h")?;
    require_finite(s, "s")?;
    require_finite(l, "l")?;
    if s < 0.0 || s > 1.0 {
        return Err(pi_js::Error::msg(format!(
            "s must be between 0 and 1: {}",
            pi_js::num::to_js_string(s)
        )));
    }
    if l < 0.0 || l > 1.0 {
        return Err(pi_js::Error::msg(format!(
            "l must be between 0 and 1: {}",
            pi_js::num::to_js_string(l)
        )));
    }
    let RgbColor { r, g, b } = okhsl_to_rgb(h, s, l);
    rgb_color(r, g, b)
}

pub fn color_to_okhsl(color: Color) -> OkhslChannels {
    rgb_to_okhsl(color_to_rgb(color))
}

pub fn parse_color(value: ParseColorValue<'_>) -> pi_js::Result<Color> {
    let value = match value {
        ParseColorValue::Number(index) => return indexed_color(index).map(Color::Indexed),
        ParseColorValue::Text(s) => s,
    };

    if let Some(hex_body) = value.strip_prefix('#') {
        if (hex_body.len() == 3 || hex_body.len() == 6) && hex_body.bytes().all(|b| b.is_ascii_hexdigit()) {
            let digits = if hex_body.len() == 3 {
                hex_body.chars().flat_map(|c| [c, c]).collect::<String>()
            } else {
                hex_body.to_string()
            };
            return rgb_color(
                pi_js::num::parse_int(&digits[0..2], Some(16)),
                pi_js::num::parse_int(&digits[2..4], Some(16)),
                pi_js::num::parse_int(&digits[4..6], Some(16)),
            )
            .map(Color::Rgb);
        }
    }

    if let Some(oklch) = OKLCH_RE.captures(value) {
        let divisor = if oklch.get(2).is_some() { 100.0 } else { 1.0 };
        let lightness = pi_js::num::parse_float(&oklch[1]) / divisor;
        return oklch_color(
            lightness,
            pi_js::num::parse_float(&oklch[3]),
            pi_js::num::parse_float(&oklch[4]),
        )
        .map(Color::Oklch);
    }

    if let Some(okhsl) = OKHSL_RE.captures(value) {
        let sat_div = if okhsl.get(3).is_some() { 100.0 } else { 1.0 };
        let light_div = if okhsl.get(5).is_some() { 100.0 } else { 1.0 };
        let saturation = pi_js::num::parse_float(&okhsl[2]) / sat_div;
        let lightness = pi_js::num::parse_float(&okhsl[4]) / light_div;
        return okhsl_color(pi_js::num::parse_float(&okhsl[1]), saturation, lightness).map(Color::Rgb);
    }

    Err(pi_js::Error::msg(format!("Invalid color value: {value}")))
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
    if (0..16).contains(&index) {
        return BASIC_COLORS[index as usize];
    }
    if (16..232).contains(&index) {
        let cube_index = (index - 16) as usize;
        return RgbColor {
            r: CUBE_VALUES[cube_index / 36] as f64,
            g: CUBE_VALUES[(cube_index % 36) / 6] as f64,
            b: CUBE_VALUES[cube_index % 6] as f64,
        };
    }
    let gray = (8 + (index - 232) * 10) as f64;
    RgbColor {
        r: gray,
        g: gray,
        b: gray,
    }
}

fn is_in_srgb_gamut(linear: &[f64]) -> bool {
    let epsilon = 1e-7;
    linear
        .iter()
        .all(|&channel| channel >= -epsilon && channel <= 1.0 + epsilon)
}

fn oklch_to_rgb(color: OklchChannels) -> RgbColor {
    // Gamut mapping keeps the hue fixed, so its direction is computed once and scaled by chroma.
    let radians = (color.h * std::f64::consts::PI) / 180.0;
    let cos = radians.cos();
    let sin = radians.sin();
    let at_chroma = |chroma: f64| oklab_to_linear_srgb([color.l, chroma * cos, chroma * sin]);

    let direct = at_chroma(color.c);
    if is_in_srgb_gamut(&direct) {
        return linear_srgb_to_rgb(direct);
    }

    // Reduce chroma until the color fits. The achromatic color is always in gamut, so it is the
    // fallback when no bisection step fits, e.g. `oklch(100% 0.3 150)` must map to white.
    let mut linear = at_chroma(0.0);
    let mut low = 0.0;
    let mut high = color.c;
    for _ in 0..20 {
        let chroma = (low + high) / 2.0;
        let candidate = at_chroma(chroma);
        if is_in_srgb_gamut(&candidate) {
            low = chroma;
            linear = candidate;
        } else {
            high = chroma;
        }
    }
    linear_srgb_to_rgb(linear)
}

pub fn color_to_rgb(color: Color) -> RgbColor {
    match color {
        Color::Indexed(c) => indexed_to_rgb(c.index),
        Color::Rgb(c) => RgbColor { r: c.r, g: c.g, b: c.b },
        Color::Oklch(c) => oklch_to_rgb(OklchChannels { l: c.l, c: c.c, h: c.h }),
    }
}

pub fn color_to_oklch(color: Color) -> OklchChannels {
    if let Color::Oklch(c) = color {
        return OklchChannels { l: c.l, c: c.c, h: c.h };
    }
    let [l, a, b] = rgb_to_oklab(color_to_rgb(color));
    OklchChannels {
        l,
        c: a.hypot(b),
        h: ((b.atan2(a) * 180.0) / std::f64::consts::PI + 360.0) % 360.0,
    }
}

pub fn color_to_hex(color: Color) -> String {
    let RgbColor { r, g, b } = color_to_rgb(color);
    let channel = |value: f64| {
        let hex = pi_js::num::to_string_radix(pi_js::num::math_round(value), 16);
        pi_js::str16::pad_start(&hex, 2, "0")
    };
    format!("#{}{}{}", channel(r), channel(g), channel(b))
}

/// PORT: `space: None` selects `"oklch"`, the TS default.
pub fn mix_colors(first: Color, second: Color, amount: f64, space: Option<ColorMixSpace>) -> pi_js::Result<Color> {
    require_finite(amount, "amount")?;
    if amount < 0.0 || amount > 1.0 {
        return Err(pi_js::Error::msg(format!(
            "amount must be between 0 and 1: {}",
            pi_js::num::to_js_string(amount)
        )));
    }

    if space.unwrap_or(ColorMixSpace::Oklch) == ColorMixSpace::Srgb {
        let a = color_to_rgb(first);
        let b = color_to_rgb(second);
        return rgb_color(
            a.r + (b.r - a.r) * amount,
            a.g + (b.g - a.g) * amount,
            a.b + (b.b - a.b) * amount,
        )
        .map(Color::Rgb);
    }

    let a = color_to_oklch(first);
    let b = color_to_oklch(second);
    let first_hue = if a.c < 1e-7 { b.h } else { a.h };
    let second_hue = if b.c < 1e-7 { first_hue } else { b.h };
    let hue_delta = ((second_hue - first_hue + 540.0) % 360.0) - 180.0;
    oklch_color(
        a.l + (b.l - a.l) * amount,
        a.c + (b.c - a.c) * amount,
        first_hue + hue_delta * amount,
    )
    .map(Color::Oklch)
}

fn find_closest(values: &[i64], target: f64) -> i64 {
    let mut closest_index = 0usize;
    let mut closest_distance = f64::INFINITY;
    for (index, &val) in values.iter().enumerate() {
        let distance = (target - val as f64).abs();
        if distance < closest_distance {
            closest_index = index;
            closest_distance = distance;
        }
    }
    closest_index as i64
}

fn color_distance(first: RgbColor, second: RgbColor) -> f64 {
    let dr = first.r - second.r;
    let dg = first.g - second.g;
    let db = first.b - second.b;
    dr * dr * 0.299 + dg * dg * 0.587 + db * db * 0.114
}

fn rgb_to_ansi256(color: RgbColor) -> i64 {
    let r_index = find_closest(&CUBE_VALUES, color.r) as usize;
    let g_index = find_closest(&CUBE_VALUES, color.g) as usize;
    let b_index = find_closest(&CUBE_VALUES, color.b) as usize;
    let cube_color = RgbColor {
        r: CUBE_VALUES[r_index] as f64,
        g: CUBE_VALUES[g_index] as f64,
        b: CUBE_VALUES[b_index] as f64,
    };
    let cube_index = 16 + 36 * r_index as i64 + 6 * g_index as i64 + b_index as i64;

    let gray = pi_js::num::math_round(0.299 * color.r + 0.587 * color.g + 0.114 * color.b);
    let gray_offset = find_closest(&GRAY_VALUES, gray) as usize;
    let gray_value = GRAY_VALUES[gray_offset] as f64;
    let spread = if color.r.is_nan() || color.g.is_nan() || color.b.is_nan() {
        f64::NAN
    } else {
        color.r.max(color.g).max(color.b) - color.r.min(color.g).min(color.b)
    };
    if spread < 10.0
        && color_distance(
            color,
            RgbColor {
                r: gray_value,
                g: gray_value,
                b: gray_value,
            },
        ) < color_distance(color, cube_color)
    {
        return 232 + gray_offset as i64;
    }
    cube_index
}

fn color_ansi(color: Color, mode: TerminalColorMode, background: bool) -> String {
    let code = if background { 48 } else { 38 };
    if let Color::Indexed(c) = color {
        return format!("\x1b[{code};5;{}m", c.index);
    }

    let rgb = color_to_rgb(color);
    if mode == TerminalColorMode::Truecolor {
        return format!(
            "\x1b[{code};2;{};{};{}m",
            pi_js::num::to_js_string(pi_js::num::math_round(rgb.r)),
            pi_js::num::to_js_string(pi_js::num::math_round(rgb.g)),
            pi_js::num::to_js_string(pi_js::num::math_round(rgb.b)),
        );
    }
    format!("\x1b[{code};5;{}m", rgb_to_ansi256(rgb))
}

pub fn foreground_ansi(color: Color, mode: TerminalColorMode) -> String {
    color_ansi(color, mode, false)
}

pub fn background_ansi(color: Color, mode: TerminalColorMode) -> String {
    color_ansi(color, mode, true)
}

pub fn style_text(text: &str, options: TextStyle, mode: TerminalColorMode) -> String {
    let fg = options.fg.map(|c| foreground_ansi(c, mode));
    let bg = options.bg.map(|c| background_ansi(c, mode));
    style_text_with_ansi(
        text,
        fg.as_deref(),
        bg.as_deref(),
        TextAttributes {
            bold: options.bold,
            dim: options.dim,
            italic: options.italic,
            underline: options.underline,
            inverse: options.inverse,
            strikethrough: options.strikethrough,
        },
    )
}

/// Like `styleText()`, but with precomputed color escape sequences, e.g. cached theme colors.
/// Colors in `options` are ignored.
pub fn style_text_with_ansi(
    text: &str,
    fg_ansi: Option<&str>,
    bg_ansi: Option<&str>,
    options: TextAttributes,
) -> String {
    // Resets are prepended so they close in reverse order of the opening sequences.
    let mut prefix = String::new();
    let mut suffix = String::new();
    if let Some(fg) = fg_ansi.filter(|s| !s.is_empty()) {
        prefix.push_str(fg);
        suffix = "\x1b[39m".to_string();
    }
    if let Some(bg) = bg_ansi.filter(|s| !s.is_empty()) {
        prefix.push_str(bg);
        suffix = format!("\x1b[49m{suffix}");
    }
    let bold = options.bold.unwrap_or(false);
    let dim = options.dim.unwrap_or(false);
    if bold {
        prefix.push_str("\x1b[1m");
    }
    if dim {
        prefix.push_str("\x1b[2m");
    }
    if bold || dim {
        suffix = format!("\x1b[22m{suffix}");
    }
    if options.italic.unwrap_or(false) {
        prefix.push_str("\x1b[3m");
        suffix = format!("\x1b[23m{suffix}");
    }
    if options.underline.unwrap_or(false) {
        prefix.push_str("\x1b[4m");
        suffix = format!("\x1b[24m{suffix}");
    }
    if options.inverse.unwrap_or(false) {
        prefix.push_str("\x1b[7m");
        suffix = format!("\x1b[27m{suffix}");
    }
    if options.strikethrough.unwrap_or(false) {
        prefix.push_str("\x1b[9m");
        suffix = format!("\x1b[29m{suffix}");
    }
    format!("{prefix}{text}{suffix}")
}
