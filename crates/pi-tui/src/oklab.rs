//! Port of packages/tui/src/oklab.ts
//!
//! Oklab and OKHSL <-> sRGB conversion. `colors.ts` builds its OKLCH, OKHSL, and color mixing on it.
//!
//! Oklab and OKHSL are Björn Ottosson's color spaces; OKHSL's saturation is relative to the sRGB gamut at
//! each hue and lightness. This is a port of his reference implementation (https://bottosson.github.io/posts/colorpicker/),
//! Copyright (c) 2021 Björn Ottosson, used under the MIT license:
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
//! associated documentation files (the "Software"), to deal in the Software without restriction, including
//! without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the
//! following conditions: The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY
//! KIND, EXPRESS OR IMPLIED.

#![allow(dead_code, unused_variables)]

use crate::colors::OkhslChannels;
use crate::terminal_colors::RgbColor;

type Vector = [f64; 3];
type Matrix = [Vector; 3];

fn multiply(m: Matrix, [x, y, z]: Vector) -> Vector {
    [
        m[0][0] * x + m[0][1] * y + m[0][2] * z,
        m[1][0] * x + m[1][1] * y + m[1][2] * z,
        m[2][0] * x + m[2][1] * y + m[2][2] * z,
    ]
}

const LINEAR_SRGB_TO_LMS: Matrix = [
    [0.4122214694707629, 0.5363325372617349, 0.0514459932675022],
    [0.2119034958178251, 0.6806995506452344, 0.1073969535369405],
    [0.0883024591900564, 0.2817188391361215, 0.6299787016738222],
];
const LMS_TO_LAB: Matrix = [
    [0.210454268309314, 0.793617774702305, -0.0040720430116193],
    [1.9779985324311684, -2.42859224204858, 0.450593709617411],
    [0.0259040424655478, 0.7827717124575296, -0.8086757549230774],
];
const LAB_TO_LMS: Matrix = [
    [1.0, 0.3963377773761749, 0.2158037573099136],
    [1.0, -0.1055613458156586, -0.0638541728258133],
    [1.0, -0.0894841775298119, -1.2914855480194092],
];
const LMS_TO_LINEAR_SRGB: Matrix = [
    [4.0767416360759583, -3.3077115392580629, 0.2309699031821043],
    [-1.2684379732850315, 2.6097573492876882, -0.341319376002657],
    [-0.0041960761386756, -0.7034186179359362, 1.7076146940746117],
];

/// Per sRGB channel (red, green, blue): the (a, b) half-plane where that channel clips
/// first, and the polynomial approximating the maximum saturation there.
const SATURATION_FIT: [([f64; 2], [f64; 5]); 3] = [
    (
        [-1.8817031, -0.80936501],
        [1.19086277, 1.76576728, 0.59662641, 0.75515197, 0.56771245],
    ),
    (
        [1.8144408, -1.19445267],
        [0.73956515, -0.45954404, 0.08285427, 0.12541073, -0.14503204],
    ),
    (
        [0.13110758, 1.81333971],
        [1.35733652, -0.00915799, -1.1513021, -0.50559606, 0.00692167],
    ),
];
const K1: f64 = 0.206;
const K2: f64 = 0.03;
const K3: f64 = (1.0 + K1) / (1.0 + K2);

/// Oklab lightness to OKHSL lightness.
pub fn oklab_to_okhsl_lightness(x: f64) -> f64 {
    let t = K3 * x - K1;
    0.5 * (t + (t * t + 4.0 * K2 * K3 * x).sqrt())
}

/// OKHSL lightness to Oklab lightness.
fn okhsl_to_oklab_lightness(x: f64) -> f64 {
    (x * x + K1 * x) / (K3 * (x + K2))
}

/// sRGB transfer function: linear to encoded channel, both 0-1.
fn linear_to_srgb(value: f64) -> f64 {
    todo!("port: linear_to_srgb")
}

/// Inverse sRGB transfer function: encoded to linear channel, both 0-1.
fn srgb_to_linear(value: f64) -> f64 {
    todo!("port: srgb_to_linear")
}

/// Oklab [L, a, b] to linear sRGB [r, g, b] (0-1, may leave the gamut).
pub fn oklab_to_linear_srgb(lab: [f64; 3]) -> [f64; 3] {
    let lms = multiply(LAB_TO_LMS, lab);
    multiply(LMS_TO_LINEAR_SRGB, [lms[0].powi(3), lms[1].powi(3), lms[2].powi(3)])
}

/// Linear sRGB [r, g, b] (0-1) to Oklab [L, a, b].
fn linear_srgb_to_oklab(rgb: [f64; 3]) -> [f64; 3] {
    let lms = multiply(LINEAR_SRGB_TO_LMS, rgb);
    multiply(LMS_TO_LAB, [lms[0].cbrt(), lms[1].cbrt(), lms[2].cbrt()])
}

/// sRGB channels (0-255) to Oklab [L, a, b].
pub fn rgb_to_oklab(rgb: RgbColor) -> [f64; 3] {
    linear_srgb_to_oklab([
        srgb_to_linear(rgb.r / 255.0),
        srgb_to_linear(rgb.g / 255.0),
        srgb_to_linear(rgb.b / 255.0),
    ])
}

/// Linear sRGB [r, g, b] to sRGB channels (0-255, rounded), clipping out-of-gamut channels.
pub fn linear_srgb_to_rgb(linear: [f64; 3]) -> RgbColor {
    let channel = |value: f64| pi_js::num::math_round(linear_to_srgb(value).clamp(0.0, 1.0) * 255.0);
    RgbColor {
        r: channel(linear[0]),
        g: channel(linear[1]),
        b: channel(linear[2]),
    }
}

/// Rate of change of each cube-root LMS component along a chroma direction (a, b).
fn lms_slopes(a: f64, b: f64) -> [f64; 3] {
    [
        LAB_TO_LMS[0][1] * a + LAB_TO_LMS[0][2] * b,
        LAB_TO_LMS[1][1] * a + LAB_TO_LMS[1][2] * b,
        LAB_TO_LMS[2][1] * a + LAB_TO_LMS[2][2] * b,
    ]
}

/// Largest saturation (C/L) inside sRGB for hue (a, b): polynomial fit plus one Halley step.
fn max_saturation(a: f64, b: f64) -> f64 {
    todo!("port: max_saturation")
}

/// Oklab lightness and chroma of the most saturated sRGB color of hue (a, b).
fn cusp(a: f64, b: f64) -> [f64; 2] {
    let saturation = max_saturation(a, b);
    let linear = oklab_to_linear_srgb([1.0, saturation * a, saturation * b]);
    let lightness = (1.0 / linear[0].max(linear[1]).max(linear[2])).cbrt();
    [lightness, lightness * saturation]
}

/// Chroma where the constant-lightness line at `lightness` leaves the sRGB gamut.
fn max_chroma(a: f64, b: f64, lightness: f64, [cusp_l, cusp_c]: [f64; 2]) -> f64 {
    todo!("port: max_chroma")
}

/// OKHSL's chroma reference points at lightness L and hue (a, b): [c0, cMid, cMax].
fn chroma_stops(l: f64, a: f64, b: f64) -> [f64; 3] {
    let peak = cusp(a, b);
    let c_max = max_chroma(a, b, l, peak);
    let k = c_max / (l * (peak[1] / peak[0])).min((1.0 - l) * (peak[1] / (1.0 - peak[0])));
    let mid_s = 0.11516993
        + 1.0
            / (7.4477897
                + 4.1590124 * b
                + a * (-2.19557347
                    + 1.75198401 * b
                    + a * (-2.13704948 - 10.02301043 * b + a * (-4.24894561 + 5.38770819 * b + 4.69891013 * a))));
    let mid_t = 0.11239642
        + 1.0
            / (1.6132032 - 0.68124379 * b
                + a * (0.40370612
                    + 0.90148123 * b
                    + a * (-0.27087943 + 0.6122399 * b + a * (0.00299215 - 0.45399568 * b - 0.14661872 * a))));
    let c_mid = 0.9
        * k
        * (1.0 / ((1.0 / (l * mid_s)).powi(4) + (1.0 / ((1.0 - l) * mid_t)).powi(4)))
            .sqrt()
            .sqrt();
    let c0 = (1.0 / ((1.0 / (l * 0.4)).powi(2) + (1.0 / ((1.0 - l) * 0.8)).powi(2))).sqrt();
    [c0, c_mid, c_max]
}

/// Convert OKHSL to sRGB channels (0-255, rounded), clipping out-of-gamut channels.
/// @param hue Hue in degrees.
/// @param saturation Saturation, 0-1.
/// @param lightness Lightness, 0-1.
pub fn okhsl_to_rgb(hue: f64, saturation: f64, lightness: f64) -> RgbColor {
    todo!("port: okhsl_to_rgb")
}

/// Convert sRGB channels (0-255) to OKHSL.
/// @returns Hue `h` in degrees (0 for grays), saturation `s` and lightness `l` 0-1.
pub fn rgb_to_okhsl(rgb: RgbColor) -> OkhslChannels {
    todo!("port: rgb_to_okhsl")
}
