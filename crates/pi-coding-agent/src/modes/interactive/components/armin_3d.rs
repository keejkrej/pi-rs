//! Port of packages/coding-agent/src/modes/interactive/components/easter-egg-3d.ts
//! and packages/coding-agent/src/modes/interactive/components/armin-3d.ts
//!
//! Fullscreen 3D easter eggs: the pi logo (header logo click) and Armin (/arminsayshi). Both are bitmaps built from
//! one block per pixel. The current screen dissolves into braille dust while the model spins in the center and its
//! blocks play a sliding puzzle. Leaving plays the same timeline backwards, so the screen reassembles.
//!
//! The pi logo lifts off the header, flies to the center, and grows; the dust spreads out from the header logo.
//! Armin grows out of a speck at the center; the dust spreads out from the center.
//!
//! The blocks are ray cast per braille dot. A braille cell holds 2x4 roughly square dots, so one pixel of a
//! half-block bitmap is exactly 2x2 dots, and the header logo (4x2 cells) is 8x8 dots when it lifts off.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;
use pi_js::time::{Interval, performance_now, set_interval};
use pi_tui::tui::QueryTerminalColorsOptions;
use pi_tui::{
    Color, Component, IndexedColor, OverlayAnchor, OverlayHandle, OverlayOptions, RgbColor, RgbColorValue, SizeValue,
    TUI, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType, background_ansi, color_to_rgb, foreground_ansi,
    get_keybindings, visible_width,
};

use super::super::theme::theme::{ThemeAppearance, ThemeColor, ThemeToken, theme};
use super::armin::{ARMIN_HEIGHT, ARMIN_WIDTH, is_armin_pixel};
use super::keybinding_hints::{KeyTextFormatOptions, format_key_text};

pub type Rgb = [f64; 3];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EasterEgg3d {
    PiLogo { column: usize, row: usize },
    Armin,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EasterEgg3dColors {
    pub foreground: Rgb,
    pub background: Rgb,
}

#[derive(Clone)]
struct ScreenCell {
    text: String,
    /// 1 or 2 for a grapheme, 0 for the second column of a wide grapheme.
    width: usize,
    fg: Rgb,
    /// None when the cell uses the terminal's default background.
    bg: Option<Rgb>,
    /// Seconds until the cell turns into braille dust.
    delay: f64,
    /// Number of braille dots the glyph turns into.
    ink: usize,
    seed: u64,
}

#[derive(Clone)]
struct Star {
    /// Braille character with the single dot the star occupies.
    glyph: char,
    /// Color the star leans toward; it only ever shows a small step from the background toward it.
    tint: Rgb,
    /// Largest step from the background toward the tint, from 0 to 1.
    strength: f64,
    phase: f64,
    /// Flicker speed in radians per second.
    speed: f64,
}

struct Box3d {
    min: [f64; 3],
    max: [f64; 3],
    color: Rgb,
}

struct Pose {
    center_x: f64,
    center_y: f64,
    /// Braille dots per model pixel at depth 0.
    scale: f64,
    yaw: f64,
    pitch: f64,
    roll: f64,
}

const DEPTH: f64 = 0.7;
const FLY_START: f64 = 0.1;
const FLY_DURATION: f64 = 1.3;
/// Braille dots per pixel when a model without an origin appears, growing from a speck at the center.
const START_SCALE: f64 = 0.1;

/// Grid position of a block: column and row in the bitmap, and layer (-1, 0, 1) in depth.
type Cell3 = [i32; 3];

#[derive(Clone, Copy)]
struct ModelOrigin {
    column: usize,
    row: usize,
}

struct ModelBlock {
    home: Cell3,
    color: Rgb,
}

#[derive(Clone, Copy)]
enum PuzzleMovesKind {
    PiLogo,
    Armin,
}

/// A bitmap built from one block per foreground pixel. Each is a block that the puzzle slides around.
struct Model {
    /// Bitmap size in pixels.
    columns: usize,
    rows: usize,
    blocks: Vec<ModelBlock>,
    /// Camera distance from the model's center, in pixels.
    camera_distance: f64,
    /// Farthest distance of any block corner from the center, with blocks on the outer depth layers.
    radius: f64,
    /// Largest share of the screen width the spinning model covers.
    width_share: f64,
    puzzle_moves_kind: PuzzleMovesKind,
    /// Top-left cell of the model's half-block rendering on screen. The model lifts off from there and replaces it.
    /// None to grow out of the center.
    origin: Option<ModelOrigin>,
}

impl Model {
    fn puzzle_moves(&self, random: f64) -> usize {
        match self.puzzle_moves_kind {
            PuzzleMovesKind::PiLogo => {
                if random < 0.4 {
                    2
                } else {
                    1
                }
            }
            PuzzleMovesKind::Armin => {
                pi_js::num::math_round((self.blocks.len() as f64) * (0.03 + random * 0.04)) as usize
            }
        }
    }
}

fn create_model(
    columns: usize,
    rows: usize,
    pixel: impl Fn(usize, usize) -> Option<Rgb>,
    camera_distance: f64,
    width_share: f64,
    puzzle_moves_kind: PuzzleMovesKind,
    origin: Option<ModelOrigin>,
) -> Model {
    let mut blocks = Vec::new();
    for row in 0..rows {
        for column in 0..columns {
            if let Some(color) = pixel(column, row) {
                blocks.push(ModelBlock {
                    home: [column as i32, row as i32, 0],
                    color,
                });
            }
        }
    }
    let half_cols = (columns as f64) / 2.0;
    let half_rows = (rows as f64) / 2.0;
    let half_depth = 1.0 + DEPTH / 2.0;
    let radius = (half_cols * half_cols + half_rows * half_rows + half_depth * half_depth).sqrt();
    Model {
        columns,
        rows,
        blocks,
        camera_distance,
        radius,
        width_share,
        puzzle_moves_kind,
        origin,
    }
}

const CORAL: Rgb = [228.0, 138.0, 122.0];
const BLUE: Rgb = [79.0, 142.0, 179.0];
const YELLOW: Rgb = [234.0, 182.0, 93.0];
const PI_LOGO_PIXELS: [&[u8]; 4] = [b"ccc.", b"b.c.", b"bb.y", b"b..y"];

fn pi_logo_model(origin: ModelOrigin) -> Model {
    create_model(
        4,
        4,
        |column, row| match PI_LOGO_PIXELS[row][column] {
            b'c' => Some(CORAL),
            b'b' => Some(BLUE),
            b'y' => Some(YELLOW),
            _ => None,
        },
        10.0,
        0.35,
        PuzzleMovesKind::PiLogo,
        Some(origin),
    )
}

fn armin_model(color: Rgb) -> Model {
    create_model(
        ARMIN_WIDTH,
        ARMIN_HEIGHT,
        |column, row| is_armin_pixel(column, row).then_some(color),
        // Far enough that the perspective stays mild for a figure about 36 blocks tall.
        80.0,
        0.45,
        PuzzleMovesKind::Armin,
        None,
    )
}

// The sliding puzzle: after the model spun for a while, blocks slide into free neighboring cells, a few at a time.
// Each cycle shuffles, flies every block back home, and holds the model briefly.
const PUZZLE_START: f64 = FLY_START + FLY_DURATION + 3.0;
const PUZZLE_STEP: f64 = 0.3;
const PUZZLE_STEPS: usize = 12;
const PUZZLE_RETURN: f64 = 1.0;
const PUZZLE_HOLD: f64 = 0.6;
const PUZZLE_CYCLE: f64 = PUZZLE_STEP * (PUZZLE_STEPS as f64) + PUZZLE_RETURN + PUZZLE_HOLD;
const PUZZLE_MOVES: [Cell3; 10] = [
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [0, 0, 1],
    [0, 0, -1],
];

/// Block positions after each step of one shuffle cycle, starting at home. Deterministic per cycle.
fn shuffle_steps(model: &Model, cycle: usize) -> Vec<Vec<Cell3>> {
    let columns = model.columns as i32;
    let rows = model.rows as i32;
    let mut positions: Vec<Cell3> = model.blocks.iter().map(|block| block.home).collect();
    let mut steps = Vec::with_capacity(PUZZLE_STEPS + 1);
    steps.push(positions.clone());

    let block_count = positions.len();
    let grid_size = (3 * model.rows * model.columns).max(1);
    let mut last_moved = vec![false; block_count];
    let mut random = (cycle as u64).wrapping_mul(7_919).wrapping_add(1);
    let mut next = || -> f64 {
        let val = hash(random as u32);
        random += 1;
        val
    };
    let key = |[x, y, z]: Cell3| -> usize { (((z + 1) * rows + y) * columns + x) as usize };

    for _step in 0..PUZZLE_STEPS {
        let mut occupied = vec![false; grid_size];
        for &pos in &positions {
            occupied[key(pos)] = true;
        }
        let mut next_positions = positions.clone();
        let mut moved = vec![false; block_count];
        let move_count = model.puzzle_moves(next());
        for _mv in 0..move_count {
            let mut candidates: Vec<(usize, Cell3)> = Vec::new();
            let mut fresh_count = 0usize;
            for (block, &[x, y, z]) in positions.iter().enumerate() {
                if moved[block] {
                    continue;
                }
                for &[dx, dy, dz] in &PUZZLE_MOVES {
                    let target = [x + dx, y + dy, z + dz];
                    if target[0] < 0 || target[0] >= columns || target[1] < 0 || target[1] >= rows {
                        continue;
                    }
                    if target[2] < -1 || target[2] > 1 {
                        continue;
                    }
                    if occupied[key(target)] {
                        continue;
                    }
                    if !last_moved[block] {
                        fresh_count += 1;
                    }
                    candidates.push((block, target));
                }
            }
            if candidates.is_empty() {
                break;
            }
            // Prefer blocks that did not just move, so the puzzle does not look like one block jittering.
            let choice = if fresh_count > 0 {
                let target_idx = ((next() * (fresh_count as f64)).floor() as usize).min(fresh_count - 1);
                candidates
                    .iter()
                    .filter(|&&(block, _)| !last_moved[block])
                    .nth(target_idx)
                    .copied()
            } else {
                let target_idx = ((next() * (candidates.len() as f64)).floor() as usize).min(candidates.len() - 1);
                candidates.get(target_idx).copied()
            };
            let Some((block, target)) = choice else {
                break;
            };
            occupied[key(target)] = true;
            next_positions[block] = target;
            moved[block] = true;
        }
        last_moved = moved;
        positions = next_positions;
        steps.push(positions.clone());
    }
    steps
}

const FRAME_MS: u64 = 1000 / 30;
const DUST_DURATION: f64 = 0.35;
const WAVE_SPREAD: f64 = 0.55;
const WAVE_JITTER: f64 = 0.12;
const DISSOLVE_END: f64 = FLY_START + WAVE_SPREAD + WAVE_JITTER + DUST_DURATION;
const BACKGROUND_FADE: f64 = 0.5;
const EXIT_DURATION: f64 = 1.1;
// A faint starfield fades in behind the logo once the screen has dissolved.
const STARS_START: f64 = 2.5;
const STARS_FADE: f64 = 2.0;
const STAR_DENSITY: f64 = 0.018;
// Star tints: neutral, pale blue, pale gold, and pale rose, each mixed half with the terminal's foreground.
const STAR_TINTS: [Rgb; 4] = [
    [255.0, 255.0, 255.0],
    [170.0, 195.0, 255.0],
    [255.0, 225.0, 170.0],
    [255.0, 190.0, 205.0],
];
const LOGO_COLOR_STEP: f64 = 5.0;
// On light backgrounds most of each braille cell shows the bright background, which washes the logo out. There,
// the logo gets a halo: its colors and coverage blurred into the cell backgrounds, strongest under the logo and
// fading out over HALO_RADIUS_X columns and HALO_RADIUS_Y rows (cells are about twice as tall as wide).
const LIGHT_HALO_STRENGTH: f64 = 0.3;
const HALO_RADIUS_X: usize = 6;
const HALO_RADIUS_Y: usize = 3;
const HALO_LEVELS: f64 = 32.0;
const STAR_ALPHA_LEVELS: f64 = 8.0;
const STAR_FLICKER_LEVELS: f64 = 3.0;
const HINT_FADE: f64 = 0.5;

// Braille dot bits, indexed by row * 2 + column within the 2x4 cell.
const DOT_BITS: [u8; 8] = [0x01, 0x08, 0x02, 0x10, 0x04, 0x20, 0x40, 0x80];

static BRAILLE: LazyLock<[char; 256]> = LazyLock::new(|| {
    let mut table = ['\u{2800}'; 256];
    for (bits, slot) in table.iter_mut().enumerate() {
        *slot = char::from_u32(0x2800 + bits as u32).unwrap_or('\u{2800}');
    }
    table
});

static LIGHT: LazyLock<[f64; 3]> = LazyLock::new(|| normalize([-0.45, -0.6, 0.75]));
static HALF_VECTOR: LazyLock<[f64; 3]> = LazyLock::new(|| {
    let l = *LIGHT;
    normalize([l[0], l[1], l[2] + 1.0])
});

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / length, v[1] / length, v[2] / length]
}

fn clamp01(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}

/// Quintic smoothstep.
fn smooth(value: f64) -> f64 {
    let t = clamp01(value);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn ease_out(value: f64) -> f64 {
    let t = 1.0 - clamp01(value);
    1.0 - t * t * t
}

fn mix(a: Rgb, b: Rgb, amount: f64) -> Rgb {
    [
        a[0] + (b[0] - a[0]) * amount,
        a[1] + (b[1] - a[1]) * amount,
        a[2] + (b[2] - a[2]) * amount,
    ]
}

fn hash(value: u32) -> f64 {
    let mut x = (value ^ 0x9e37_79b9).wrapping_mul(0x85eb_ca6b);
    x ^= x >> 13;
    x = x.wrapping_mul(0xc2b2_ae35);
    x ^= x >> 16;
    (x as f64) / 4_294_967_296.0
}

/// Whether a background is light, by relative luminance.
fn is_light([r, g, b]: Rgb) -> bool {
    0.2126 * r + 0.7152 * g + 0.0722 * b > 128.0
}

fn to_rgb(color: Color) -> Rgb {
    let rgb = color_to_rgb(color);
    [rgb.r, rgb.g, rgb.b]
}

fn default_text_color() -> Color {
    Color::Rgb(RgbColorValue {
        r: 229.0,
        g: 229.0,
        b: 231.0,
    })
}

fn default_accent_color() -> Color {
    Color::Rgb(RgbColorValue {
        r: 79.0,
        g: 142.0,
        b: 179.0,
    })
}

fn theme_color_rgb(colors: &IndexMap<ThemeToken, Color>, tok: ThemeColor, fallback: Color) -> Rgb {
    to_rgb(colors.get(&ThemeToken::Fg(tok)).copied().unwrap_or(fallback))
}

/// Braille dots a glyph turns into, a rough measure of how much ink it has.
fn glyph_ink(text: &str, seed: f64) -> usize {
    if pi_js::str16::trim(text).is_empty() {
        return 0;
    }
    let mut chars = text.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if matches!(c, '.' | ',' | ':' | ';' | '\'' | '`' | '-' | '_' | '·') {
            return 2;
        }
        if ('\u{2500}'..='\u{257f}').contains(&c) {
            return 3;
        }
    }
    4 + ((seed * 3.0).floor() as usize)
}

fn parse_int_base10(s: &str) -> Option<i64> {
    let v = pi_js::num::parse_int(s, Some(10));
    if v.is_finite() { Some(v as i64) } else { None }
}

#[derive(Default)]
struct SgrState {
    fg: Option<Rgb>,
    bg: Option<Rgb>,
    dim: bool,
    inverse: bool,
}

fn palette_color(index: i64) -> Option<Rgb> {
    if (0..=255).contains(&index) {
        Some(to_rgb(Color::Indexed(IndexedColor { index })))
    } else {
        None
    }
}

fn apply_sgr(sgr: &mut SgrState, params: &str) {
    let codes: Vec<&str> = if params.is_empty() {
        vec!["0"]
    } else {
        params.split(';').collect()
    };
    let mut i = 0usize;
    while i < codes.len() {
        let parts: Vec<Option<i64>> = codes[i].split(':').map(parse_int_base10).collect();
        let code = parts.first().copied().flatten().unwrap_or(0);
        if code == 38 || code == 48 {
            // Extended color, either as `38;5;n` / `38;2;r;g;b` or colon sub-parameters.
            let args: Vec<Option<i64>> = if parts.len() > 1 {
                parts[1..].iter().copied().filter(Option::is_some).collect()
            } else {
                let kind = codes.get(i + 1).and_then(|s| parse_int_base10(s));
                let count = match kind {
                    Some(5) => 2usize,
                    Some(2) => 4usize,
                    _ => 1usize,
                };
                let end = (i + 1 + count).min(codes.len());
                let slice: Vec<Option<i64>> = codes[i + 1..end].iter().map(|part| parse_int_base10(part)).collect();
                i += count;
                slice
            };
            let mut color: Option<Rgb> = None;
            if args.first() == Some(&Some(5)) {
                if let Some(Some(idx)) = args.get(1) {
                    color = palette_color(*idx);
                }
            } else if args.first() == Some(&Some(2))
                && let (Some(Some(r)), Some(Some(g)), Some(Some(b))) = (args.get(1), args.get(2), args.get(3))
            {
                color = Some([*r as f64, *g as f64, *b as f64]);
            }
            if let Some(c) = color {
                if code == 38 {
                    sgr.fg = Some(c);
                } else {
                    sgr.bg = Some(c);
                }
            }
        } else if code == 0 {
            sgr.fg = None;
            sgr.bg = None;
            sgr.dim = false;
            sgr.inverse = false;
        } else if code == 2 {
            sgr.dim = true;
        } else if code == 22 {
            sgr.dim = false;
        } else if code == 7 {
            sgr.inverse = true;
        } else if code == 27 {
            sgr.inverse = false;
        } else if (30..=37).contains(&code) {
            sgr.fg = palette_color(code - 30);
        } else if (90..=97).contains(&code) {
            sgr.fg = palette_color(code - 90 + 8);
        } else if code == 39 {
            sgr.fg = None;
        } else if (40..=47).contains(&code) {
            sgr.bg = palette_color(code - 40);
        } else if (100..=107).contains(&code) {
            sgr.bg = palette_color(code - 100 + 8);
        } else if code == 49 {
            sgr.bg = None;
        }
        i += 1;
    }
}

fn push_screen_text(
    cells: &mut Vec<ScreenCell>,
    text: &str,
    width: usize,
    foreground: Rgb,
    background: Rgb,
    sgr: &SgrState,
) {
    for seg in pi_js::intl::graphemes(text) {
        let glyph_width = visible_width(seg.segment).max(0) as usize;
        if glyph_width == 0 || cells.len() + glyph_width > width {
            continue;
        }
        let mut cell_fg = sgr.fg.unwrap_or(foreground);
        let mut cell_bg = sgr.bg;
        if sgr.inverse {
            cell_fg = sgr.bg.unwrap_or(background);
            cell_bg = Some(sgr.fg.unwrap_or(foreground));
        }
        if sgr.dim {
            cell_fg = mix(cell_fg, cell_bg.unwrap_or(background), 0.4);
        }
        let cell = ScreenCell {
            text: seg.segment.to_string(),
            width: glyph_width,
            fg: cell_fg,
            bg: cell_bg,
            delay: 0.0,
            ink: 0,
            seed: 0,
        };
        cells.push(cell.clone());
        if glyph_width == 2 {
            cells.push(ScreenCell {
                text: String::new(),
                width: 0,
                ..cell
            });
        }
    }
}

/// Parse rendered lines into cells with resolved colors. Other escape sequences (OSC, APC, cursor) are skipped.
fn parse_screen(lines: &[String], width: usize, foreground: Rgb, background: Rgb) -> Vec<Vec<ScreenCell>> {
    lines
        .iter()
        .map(|line| {
            let mut cells = Vec::new();
            let mut sgr = SgrState::default();
            let bytes = line.as_bytes();
            let mut i = 0usize;
            while i < line.len() {
                let Some(rel_start) = line[i..].find('\x1b') else {
                    push_screen_text(&mut cells, &line[i..], width, foreground, background, &sgr);
                    break;
                };
                let sequence_start = i + rel_start;
                if sequence_start > i {
                    push_screen_text(
                        &mut cells,
                        &line[i..sequence_start],
                        width,
                        foreground,
                        background,
                        &sgr,
                    );
                }
                let Some(kind) = line[sequence_start + 1..].chars().next() else {
                    break;
                };
                if kind == '[' {
                    let mut end = sequence_start + 2;
                    while end < bytes.len() && (bytes[end] < 0x40 || bytes[end] > 0x7e) {
                        end += 1;
                    }
                    if end >= bytes.len() {
                        break;
                    }
                    if bytes[end] == b'm'
                        && let Ok(params) = std::str::from_utf8(&bytes[sequence_start + 2..end])
                    {
                        apply_sgr(&mut sgr, params);
                    }
                    i = end + 1;
                } else if kind == ']' || kind == '_' || kind == 'P' || kind == '^' {
                    // String sequences end at BEL or ST (ESC \).
                    let rest = &line[sequence_start + 2..];
                    let bell = rest.find('\x07').map(|p| sequence_start + 2 + p);
                    let st = rest.find("\x1b\\").map(|p| sequence_start + 2 + p);
                    match (bell, st) {
                        (None, None) => break,
                        (Some(b), Some(s)) => i = if b < s { b + 1 } else { s + 2 },
                        (Some(b), None) => i = b + 1,
                        (None, Some(s)) => i = s + 2,
                    }
                } else {
                    i = sequence_start + 1 + kind.len_utf8();
                }
            }
            cells
        })
        .collect()
}

/// Row-major 3x3 rotation matrix for yaw (y), then pitch (x), then roll (z).
fn rotation(yaw: f64, pitch: f64, roll: f64) -> [f64; 9] {
    let (sy, cy) = yaw.sin_cos();
    let (sx, cx) = pitch.sin_cos();
    let (sz, cz) = roll.sin_cos();
    // Rz * Rx * Ry
    let ry = [cy, 0.0, sy, 0.0, 1.0, 0.0, -sy, 0.0, cy];
    let rx = [1.0, 0.0, 0.0, 0.0, cx, -sx, 0.0, sx, cx];
    let rz = [cz, -sz, 0.0, sz, cz, 0.0, 0.0, 0.0, 1.0];
    multiply(&rz, &multiply(&rx, &ry))
}

fn multiply(a: &[f64; 9], b: &[f64; 9]) -> [f64; 9] {
    let mut result = [0.0; 9];
    for row in 0..3 {
        for column in 0..3 {
            result[row * 3 + column] =
                a[row * 3] * b[column] + a[row * 3 + 1] * b[3 + column] + a[row * 3 + 2] * b[6 + column];
        }
    }
    result
}

// The spin is locked to the puzzle: one turn per cycle, facing the camera in the middle of each hold.
const SPIN_RAMP: f64 = 1.4;
const SPIN_SPEED: f64 = std::f64::consts::TAU / PUZZLE_CYCLE;
// How much the spin slows down while facing the camera (and speeds up while facing away), from 0 to 1.
const SPIN_LINGER: f64 = 0.6;
const FIRST_FRONT_VIEW: f64 = PUZZLE_START + PUZZLE_STEP * (PUZZLE_STEPS as f64) + PUZZLE_RETURN + PUZZLE_HOLD / 2.0;

/// Uniform spin that accelerates from rest over SPIN_RAMP, then turns at SPIN_SPEED.
const fn base_spin_phase(time: f64) -> f64 {
    let elapsed = time - FLY_START;
    if elapsed <= 0.0 {
        return 0.0;
    }
    let u = elapsed / SPIN_RAMP;
    // Integral of the quintic smoothstep ramp.
    if u < 1.0 {
        let u4 = u * u * u * u;
        let u5 = u4 * u;
        let u6 = u5 * u;
        return SPIN_SPEED * SPIN_RAMP * (u6 - 3.0 * u5 + 2.5 * u4);
    }
    SPIN_SPEED * (SPIN_RAMP * 0.5 + elapsed - SPIN_RAMP)
}

// Extra rotation added during the flight, so the phase is a whole number of turns at every assembled front view.
const SPIN_ALIGNMENT: f64 = std::f64::consts::TAU - (base_spin_phase(FIRST_FRONT_VIEW) % std::f64::consts::TAU);

/// Spin phase: whole turns exactly when the reassembled logo faces the camera.
fn spin_phase(time: f64) -> f64 {
    base_spin_phase(time) + SPIN_ALIGNMENT * smooth((time - FLY_START) / FLY_DURATION)
}

/// Yaw of the logo. It matches the phase at whole turns but lingers there, so the logo reads from the front.
fn spin_angle(time: f64) -> f64 {
    let phase = spin_phase(time);
    phase - SPIN_LINGER * phase.sin()
}

struct Face {
    axis: usize,
    /// Plane coordinate on `axis` in object space.
    plane: f64,
    u_min: f64,
    u_max: f64,
    v_min: f64,
    v_max: f64,
    /// Projected bounds in braille dots, inclusive.
    min_x: usize,
    min_y: usize,
    max_x: usize,
    max_y: usize,
    red: f64,
    green: f64,
    blue: f64,
}

#[derive(Clone, Copy)]
struct DirtyRect {
    min_x: usize,
    min_y: usize,
    max_x: usize,
    max_y: usize,
}

/// Renders the model's blocks into braille cells. Only faces that point at the camera and are not covered by a
/// touching block are drawn. Each face is rasterized over its projected bounds by intersecting each dot's ray
/// with the face's plane, with a depth buffer resolving overlaps. Buffers are reused between frames.
struct BlockRaster {
    /// Braille dot bits per cell.
    bits: Vec<u8>,
    /// Lit dots per cell.
    counts: Vec<u8>,
    /// Summed RGB of the lit dots per cell.
    rgb: Vec<f32>,
    /// Halo tint strength per cell, from 0 to 1. Only set when a halo was requested.
    halo_amount: Vec<f32>,
    /// Halo color per cell.
    halo_rgb: Vec<f32>,
    width: usize,
    height: usize,
    /// Ray parameter of the nearest hit per dot; smaller is nearer.
    depth: Vec<f32>,
    /// Face index + 1 of the nearest hit per dot, 0 for none.
    face_ids: Vec<u16>,
    /// Cells written by the previous frame, cleared before the next one.
    dirty: Option<DirtyRect>,
    /// Cells with a halo from the previous frame, cleared before the next one.
    halo_dirty: Option<DirtyRect>,
    camera_distance: f64,
}

impl BlockRaster {
    fn new(camera_distance: f64) -> Self {
        Self {
            bits: Vec::new(),
            counts: Vec::new(),
            rgb: Vec::new(),
            halo_amount: Vec::new(),
            halo_rgb: Vec::new(),
            width: 0,
            height: 0,
            depth: Vec::new(),
            face_ids: Vec::new(),
            dirty: None,
            halo_dirty: None,
            camera_distance,
        }
    }

    fn render(
        &mut self,
        width: usize,
        height: usize,
        pose: &Pose,
        boxes: &[Box3d],
        background: Rgb,
        halo_strength: f64,
    ) {
        let dot_width = width * 2;
        let dot_height = height * 4;
        if width != self.width || height != self.height {
            self.width = width;
            self.height = height;
            let cell_count = width * height;
            let dot_count = dot_width * dot_height;
            self.bits = vec![0; cell_count];
            self.counts = vec![0; cell_count];
            self.rgb = vec![0.0; cell_count * 3];
            self.halo_amount = vec![0.0; cell_count];
            self.halo_rgb = vec![0.0; cell_count * 3];
            self.halo_dirty = None;
            self.depth = vec![f32::INFINITY; dot_count];
            self.face_ids = vec![0; dot_count];
            self.dirty = None;
        } else if let Some(d) = self.dirty.take() {
            for row in d.min_y..=d.max_y {
                let start = row * width + d.min_x;
                let end = row * width + d.max_x + 1;
                self.bits[start..end].fill(0);
                self.counts[start..end].fill(0);
                self.rgb[start * 3..end * 3].fill(0.0);
            }
        }
        if let Some(d) = self.halo_dirty.take() {
            for row in d.min_y..=d.max_y {
                let start = row * width + d.min_x;
                let end = row * width + d.max_x + 1;
                self.halo_amount[start..end].fill(0.0);
                self.halo_rgb[start * 3..end * 3].fill(0.0);
            }
        }

        let m = rotation(pose.yaw, pose.pitch, pose.roll);
        let center_x = pose.center_x;
        let center_y = pose.center_y;
        let scale = pose.scale;
        let camera_distance = self.camera_distance;
        // The camera sits at (0, 0, cameraDistance) in camera space; object space is the transpose rotation.
        let origin = [m[6] * camera_distance, m[7] * camera_distance, m[8] * camera_distance];
        let light = is_light(background);
        let faces = self.visible_faces(&m, origin, pose, boxes, dot_width, dot_height, light);
        if faces.is_empty() {
            return;
        }
        let mut min_x = dot_width;
        let mut min_y = dot_height;
        let mut max_x = 0usize;
        let mut max_y = 0usize;
        let mut has_bounds = false;
        for face in &faces {
            min_x = min_x.min(face.min_x);
            min_y = min_y.min(face.min_y);
            max_x = max_x.max(face.max_x);
            max_y = max_y.max(face.max_y);
            has_bounds = true;
        }
        if !has_bounds || max_x < min_x || max_y < min_y {
            return;
        }

        for (face_index, face) in faces.iter().enumerate() {
            let a = face.axis;
            let u = (a + 1) % 3;
            let v = (a + 2) % 3;
            let origin_a = origin[a];
            let origin_u = origin[u];
            let origin_v = origin[v];
            // The ray direction in object space is linear in the dot position, so it is stepped per dot.
            let step_a = m[a] / scale;
            let step_u = m[u] / scale;
            let step_v = m[v] / scale;
            let sx = (face.min_x as f64 + 0.5 - center_x) / scale;
            for dot_y in face.min_y..=face.max_y {
                let sy = (dot_y as f64 + 0.5 - center_y) / scale;
                let mut direction_a = m[a] * sx + m[3 + a] * sy - m[6 + a] * camera_distance;
                let mut direction_u = m[u] * sx + m[3 + u] * sy - m[6 + u] * camera_distance;
                let mut direction_v = m[v] * sx + m[3 + v] * sy - m[6 + v] * camera_distance;
                let mut index = dot_y * dot_width + face.min_x;
                for _dot_x in face.min_x..=face.max_x {
                    let t = (face.plane - origin_a) / direction_a;
                    if t > 0.0 && t < (self.depth[index] as f64) {
                        let hit_u = origin_u + t * direction_u;
                        let hit_v = origin_v + t * direction_v;
                        if hit_u >= face.u_min && hit_u <= face.u_max && hit_v >= face.v_min && hit_v <= face.v_max {
                            self.depth[index] = t as f32;
                            self.face_ids[index] = (face_index + 1) as u16;
                        }
                    }
                    direction_a += step_a;
                    direction_u += step_u;
                    direction_v += step_v;
                    index += 1;
                }
            }
        }

        // Shade the hit dots, pack them into braille cells, and reset the dot buffers for the next frame.
        let mut cell_min_x = width;
        let mut cell_min_y = height;
        let mut cell_max_x = 0usize;
        let mut cell_max_y = 0usize;
        let mut any_cell = false;
        for dot_y in min_y..=max_y {
            let mut index = dot_y * dot_width + min_x;
            for dot_x in min_x..=max_x {
                let id = self.face_ids[index];
                if id == 0 {
                    index += 1;
                    continue;
                }
                let face = &faces[(id - 1) as usize];
                // Points farther from the camera fade slightly toward the background for depth.
                let fog = clamp01(0.15 - camera_distance * (1.0 - self.depth[index] as f64) * 0.12)
                    * if light { 0.5 } else { 1.0 };
                self.face_ids[index] = 0;
                self.depth[index] = f32::INFINITY;
                let cell_x = dot_x >> 1;
                let cell_y = dot_y >> 2;
                let cell = cell_y * width + cell_x;
                self.bits[cell] |= DOT_BITS[(dot_y & 3) * 2 + (dot_x & 1)];
                self.counts[cell] = self.counts[cell].saturating_add(1);
                self.rgb[cell * 3] += (face.red + (background[0] - face.red) * fog) as f32;
                self.rgb[cell * 3 + 1] += (face.green + (background[1] - face.green) * fog) as f32;
                self.rgb[cell * 3 + 2] += (face.blue + (background[2] - face.blue) * fog) as f32;
                cell_min_x = cell_min_x.min(cell_x);
                cell_max_x = cell_max_x.max(cell_x);
                cell_min_y = cell_min_y.min(cell_y);
                cell_max_y = cell_max_y.max(cell_y);
                any_cell = true;
                index += 1;
            }
        }
        if any_cell {
            self.dirty = Some(DirtyRect {
                min_x: cell_min_x,
                min_y: cell_min_y,
                max_x: cell_max_x,
                max_y: cell_max_y,
            });
        }
        if halo_strength > 0.0 {
            self.render_halo(halo_strength);
        }
    }

    /// Blur the logo's coverage and color over neighboring cells with a separable tent filter, so the tint falls
    /// off smoothly past the logo's edges.
    fn render_halo(&mut self, strength: f64) {
        let Some(cells) = self.dirty else {
            return;
        };
        let width = self.width;
        let height = self.height;
        let min_x = cells.min_x.saturating_sub(HALO_RADIUS_X);
        let max_x = (cells.max_x + HALO_RADIUS_X).min(width - 1);
        let min_y = cells.min_y.saturating_sub(HALO_RADIUS_Y);
        let max_y = (cells.max_y + HALO_RADIUS_Y).min(height - 1);
        let region_width = max_x - min_x + 1;

        let tent = |radius: usize| -> Vec<f64> {
            let weights: Vec<f64> = (0..=radius * 2)
                .map(|i| (radius + 1) as f64 - (i as isize - radius as isize).unsigned_abs() as f64)
                .collect();
            let total: f64 = weights.iter().sum();
            weights.into_iter().map(|w| w / total).collect()
        };
        let weights_x = tent(HALO_RADIUS_X);
        let weights_y = tent(HALO_RADIUS_Y);

        // Horizontal pass over the rows that contain the logo: coverage and premultiplied color per cell.
        let rows = cells.max_y - cells.min_y + 1;
        let mut horizontal = vec![0.0_f32; rows * region_width * 4];
        for y in cells.min_y..=cells.max_y {
            for x in min_x..=max_x {
                let mut coverage = 0.0;
                let mut red = 0.0;
                let mut green = 0.0;
                let mut blue = 0.0;
                for (wi, weight_x) in weights_x.iter().enumerate() {
                    let source_x = x as isize + (wi as isize - HALO_RADIUS_X as isize);
                    if source_x < cells.min_x as isize || source_x > cells.max_x as isize {
                        continue;
                    }
                    let source = y * width + (source_x as usize);
                    let count = self.counts[source] as f64;
                    if count == 0.0 {
                        continue;
                    }
                    let weight = weight_x / 8.0;
                    coverage += count * weight;
                    red += (self.rgb[source * 3] as f64) * weight;
                    green += (self.rgb[source * 3 + 1] as f64) * weight;
                    blue += (self.rgb[source * 3 + 2] as f64) * weight;
                }
                let target = ((y - cells.min_y) * region_width + (x - min_x)) * 4;
                horizontal[target] = coverage as f32;
                horizontal[target + 1] = red as f32;
                horizontal[target + 2] = green as f32;
                horizontal[target + 3] = blue as f32;
            }
        }

        // Vertical pass into the halo buffers.
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let mut coverage = 0.0;
                let mut red = 0.0;
                let mut green = 0.0;
                let mut blue = 0.0;
                for (wi, weight) in weights_y.iter().enumerate() {
                    let source_y = y as isize + (wi as isize - HALO_RADIUS_Y as isize);
                    if source_y < cells.min_y as isize || source_y > cells.max_y as isize {
                        continue;
                    }
                    let source = (((source_y as usize) - cells.min_y) * region_width + (x - min_x)) * 4;
                    coverage += (horizontal[source] as f64) * weight;
                    red += (horizontal[source + 1] as f64) * weight;
                    green += (horizontal[source + 2] as f64) * weight;
                    blue += (horizontal[source + 3] as f64) * weight;
                }
                let target = y * width + x;
                // Cells under the logo keep at least their own coverage, so faces stay solidly tinted.
                let count = self.counts[target] as f64;
                if count > 0.0 && count / 8.0 > coverage {
                    coverage = count / 8.0;
                    red = ((self.rgb[target * 3] as f64) / count) * coverage;
                    green = ((self.rgb[target * 3 + 1] as f64) / count) * coverage;
                    blue = ((self.rgb[target * 3 + 2] as f64) / count) * coverage;
                }
                if coverage <= 0.0 {
                    continue;
                }
                // Quantized so neighboring cells usually share a background and its escape sequence.
                let amount = pi_js::num::math_round(strength * coverage.min(1.0).powf(0.7) * HALO_LEVELS) / HALO_LEVELS;
                if amount <= 0.0 {
                    continue;
                }
                self.halo_amount[target] = amount as f32;
                self.halo_rgb[target * 3] =
                    (pi_js::num::math_round(red / coverage / LOGO_COLOR_STEP) * LOGO_COLOR_STEP) as f32;
                self.halo_rgb[target * 3 + 1] =
                    (pi_js::num::math_round(green / coverage / LOGO_COLOR_STEP) * LOGO_COLOR_STEP) as f32;
                self.halo_rgb[target * 3 + 2] =
                    (pi_js::num::math_round(blue / coverage / LOGO_COLOR_STEP) * LOGO_COLOR_STEP) as f32;
            }
        }
        self.halo_dirty = Some(DirtyRect {
            min_x,
            min_y,
            max_x,
            max_y,
        });
    }

    fn visible_faces(
        &self,
        m: &[f64; 9],
        origin: [f64; 3],
        pose: &Pose,
        boxes: &[Box3d],
        dot_width: usize,
        dot_height: usize,
        light_background: bool,
    ) -> Vec<Face> {
        let mut faces = Vec::new();
        let camera_distance = self.camera_distance;
        let light_dir = *LIGHT;
        let half_vec = *HALF_VECTOR;

        for (box_idx, b) in boxes.iter().enumerate() {
            for a in 0..3 {
                let u = (a + 1) % 3;
                let v = (a + 2) % 3;
                for side in [-1.0_f64, 1.0_f64] {
                    let plane = if side > 0.0 { b.max[a] } else { b.min[a] };
                    // Back faces point away from the camera.
                    if (origin[a] - plane) * side <= 0.0 {
                        continue;
                    }
                    // Faces pressed against a neighboring block are hidden. Positions are exact while blocks rest.
                    let covered = boxes.iter().enumerate().any(|(other_idx, other)| {
                        other_idx != box_idx
                            && (if side > 0.0 { other.min[a] } else { other.max[a] }) == plane
                            && other.min[u] <= b.min[u]
                            && other.max[u] >= b.max[u]
                            && other.min[v] <= b.min[v]
                            && other.max[v] >= b.max[v]
                    });
                    if covered {
                        continue;
                    }

                    let mut min_x = f64::INFINITY;
                    let mut min_y = f64::INFINITY;
                    let mut max_x = f64::NEG_INFINITY;
                    let mut max_y = f64::NEG_INFINITY;
                    let mut corner = [0.0_f64; 3];
                    for corner_u in [b.min[u], b.max[u]] {
                        for corner_v in [b.min[v], b.max[v]] {
                            corner[a] = plane;
                            corner[u] = corner_u;
                            corner[v] = corner_v;
                            let x = m[0] * corner[0] + m[1] * corner[1] + m[2] * corner[2];
                            let y = m[3] * corner[0] + m[4] * corner[1] + m[5] * corner[2];
                            let z = m[6] * corner[0] + m[7] * corner[1] + m[8] * corner[2];
                            let perspective = (pose.scale * camera_distance) / (camera_distance - z);
                            let screen_x = pose.center_x + x * perspective;
                            let screen_y = pose.center_y + y * perspective;
                            min_x = min_x.min(screen_x);
                            min_y = min_y.min(screen_y);
                            max_x = max_x.max(screen_x);
                            max_y = max_y.max(screen_y);
                        }
                    }

                    let f_min_x = min_x.floor().max(0.0) as isize;
                    let f_min_y = min_y.floor().max(0.0) as isize;
                    let f_max_x = max_x.ceil().min((dot_width as f64) - 1.0) as isize;
                    let f_max_y = max_y.ceil().min((dot_height as f64) - 1.0) as isize;
                    if f_max_x < f_min_x || f_max_y < f_min_y {
                        continue;
                    }

                    // Faces are flat, so lighting is computed once per face. The normal in camera space is a
                    // column of the rotation matrix.
                    let nx = m[a] * side;
                    let ny = m[3 + a] * side;
                    let nz = m[6 + a] * side;
                    let diffuse = (nx * light_dir[0] + ny * light_dir[1] + nz * light_dir[2]).max(0.0);
                    let rim = (nx * 0.8 - nz * 0.3).max(0.0);
                    // On light backgrounds, highlights toward white would vanish, so faces only get darker than
                    // the brand colors there.
                    let specular = if light_background {
                        0.0
                    } else {
                        (nx * half_vec[0] + ny * half_vec[1] + nz * half_vec[2])
                            .max(0.0)
                            .powi(24)
                            * 0.6
                            * 255.0
                    };
                    let light = if light_background {
                        (0.55 + diffuse * 0.45 + rim * 0.1).min(1.0)
                    } else {
                        0.45 + diffuse * 0.78 + rim * 0.25
                    };
                    faces.push(Face {
                        axis: a,
                        plane,
                        u_min: b.min[u],
                        u_max: b.max[u],
                        v_min: b.min[v],
                        v_max: b.max[v],
                        min_x: f_min_x as usize,
                        min_y: f_min_y as usize,
                        max_x: f_max_x as usize,
                        max_y: f_max_y as usize,
                        red: (b.color[0] * light + specular).min(255.0),
                        green: (b.color[1] * light + specular).min(255.0),
                        blue: (b.color[2] * light + specular).min(255.0),
                    });
                }
            }
        }
        faces
    }
}

static PLAYING: AtomicBool = AtomicBool::new(false);

struct PlayingResetGuard(bool);

impl Drop for PlayingResetGuard {
    fn drop(&mut self) {
        if self.0 {
            PLAYING.store(false, Ordering::SeqCst);
        }
    }
}

fn model_from_egg(egg: EasterEgg3d, accent: Rgb) -> Model {
    match egg {
        EasterEgg3d::Armin => armin_model(accent),
        EasterEgg3d::PiLogo { column, row } => pi_logo_model(ModelOrigin { column, row }),
    }
}

/// Show the animation as a fullscreen overlay until it is dismissed. The overlay takes focus and mouse input and
/// returns focus when hidden, so the rest of the UI keeps running underneath untouched.
pub async fn play_easter_egg_3d(tui: Arc<dyn TUI + Send + Sync>, screen: Vec<String>, egg: EasterEgg3d) {
    if PLAYING.swap(true, Ordering::SeqCst) {
        return;
    }
    let playing_guard = PlayingResetGuard(true);

    // Fading needs the terminal's actual default colors; the theme only knows its own.
    let reported = tui
        .query_terminal_colors(QueryTerminalColorsOptions {
            timeout_ms: 100,
            on_late_reply: None,
        })
        .await;
    let t = theme();
    let dark = t.appearance() == ThemeAppearance::Dark;
    let theme_colors = t.colors();
    let to_rgb_tuple = |rgb: Option<RgbColor>, fallback: Rgb| -> Rgb { rgb.map_or(fallback, |c| [c.r, c.g, c.b]) };
    let colors = EasterEgg3dColors {
        foreground: to_rgb_tuple(
            reported.foreground,
            theme_color_rgb(&theme_colors, ThemeColor::Text, default_text_color()),
        ),
        background: to_rgb_tuple(
            reported.background,
            if dark { [0.0, 0.0, 0.0] } else { [255.0, 255.0, 255.0] },
        ),
    };
    if tui.has_overlay() {
        return;
    }

    let accent = theme_color_rgb(&theme_colors, ThemeColor::Accent, default_accent_color());
    let model = model_from_egg(egg, accent);

    let active_run: Arc<Mutex<Option<PlayingResetGuard>>> = Arc::new(Mutex::new(Some(playing_guard)));
    let active_run_cb = Arc::clone(&active_run);
    let overlay_slot: Arc<Mutex<Option<OverlayHandle>>> = Arc::new(Mutex::new(None));
    let overlay_slot_cb = Arc::clone(&overlay_slot);
    let on_done: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let guard = active_run_cb.lock().unwrap().take();
        drop(guard);
        if let Some(handle) = overlay_slot_cb.lock().unwrap().take() {
            handle.hide();
        }
    });

    let animation =
        EasterEgg3dAnimation::new_with_model(Arc::clone(&tui), screen, model, colors, on_done, Some(active_run));
    let handle = tui.show_overlay(
        Arc::new(animation),
        Some(OverlayOptions {
            anchor: Some(OverlayAnchor::TopLeft),
            width: Some(SizeValue::Percent("100%".to_string())),
            max_height: Some(SizeValue::Percent("100%".to_string())),
            ..Default::default()
        }),
    );
    *overlay_slot.lock().unwrap() = Some(handle);
}

/// Plays the 3D Armin easter egg (convenience wrapper over [`play_easter_egg_3d`]).
pub async fn play_armin_3d(tui: Arc<dyn TUI + Send + Sync>, screen: Vec<String>) {
    play_easter_egg_3d(tui, screen, EasterEgg3d::Armin).await;
}

struct ExitState {
    start: f64,
    time: f64,
    yaw: f64,
    target_yaw: f64,
    offsets: Vec<[f64; 3]>,
}

struct ShuffleCache {
    cycle: usize,
    steps: Vec<Vec<Cell3>>,
}

struct HintLayout {
    row: usize,
    start: usize,
    chars: Vec<char>,
    key_length: usize,
    key_color: Rgb,
    color: Rgb,
}

struct EasterEgg3dState {
    last_render: f64,
    timer: Option<Interval>,
    exit: Option<ExitState>,
    shuffle: Option<ShuffleCache>,
    screen_width: Option<usize>,
    screen_height: Option<usize>,
    stars: Vec<Option<Star>>,
    cells: Vec<Vec<ScreenCell>>,
    ansi_cache: IndexMap<u32, String>,
    raster: BlockRaster,
}

struct EasterEgg3dInner {
    tui: Arc<dyn TUI + Send + Sync>,
    /// The screen to dissolve, as rendered lines.
    screen: Vec<String>,
    model: Model,
    foreground: Rgb,
    background: Rgb,
    on_done: Arc<dyn Fn() + Send + Sync>,
    start_time: f64,
    active_run: Option<Arc<Mutex<Option<PlayingResetGuard>>>>,
    state: Mutex<EasterEgg3dState>,
}

impl Drop for EasterEgg3dInner {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock()
            && let Some(timer) = state.timer.take()
        {
            timer.clear();
        }
        if let Some(ref active_run) = self.active_run
            && let Ok(mut guard) = active_run.lock()
        {
            drop(guard.take());
        }
    }
}

#[derive(Clone)]
pub struct EasterEgg3dAnimation {
    inner: Arc<EasterEgg3dInner>,
}

impl EasterEgg3dAnimation {
    pub fn new(
        tui: Arc<dyn TUI + Send + Sync>,
        screen: Vec<String>,
        egg: EasterEgg3d,
        colors: EasterEgg3dColors,
        on_done: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let theme_colors = theme().colors();
        let accent = theme_color_rgb(&theme_colors, ThemeColor::Accent, default_accent_color());
        let model = model_from_egg(egg, accent);
        Self::new_with_model(tui, screen, model, colors, on_done, None)
    }

    fn new_with_model(
        tui: Arc<dyn TUI + Send + Sync>,
        screen: Vec<String>,
        model: Model,
        colors: EasterEgg3dColors,
        on_done: Arc<dyn Fn() + Send + Sync>,
        active_run: Option<Arc<Mutex<Option<PlayingResetGuard>>>>,
    ) -> Self {
        let now = performance_now();
        let camera_distance = model.camera_distance;
        let inner = Arc::new(EasterEgg3dInner {
            tui,
            screen,
            model,
            foreground: colors.foreground,
            background: colors.background,
            on_done,
            start_time: now,
            active_run,
            state: Mutex::new(EasterEgg3dState {
                last_render: now,
                timer: None,
                exit: None,
                shuffle: None,
                screen_width: None,
                screen_height: None,
                stars: Vec::new(),
                cells: Vec::new(),
                ansi_cache: IndexMap::new(),
                raster: BlockRaster::new(camera_distance),
            }),
        });

        let weak = Arc::downgrade(&inner);
        let timer = set_interval(FRAME_MS, move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let finish_timer = {
                let mut state = inner.state.lock().unwrap();
                // Also stop when no longer rendered, e.g. when pi hides all overlays on exit.
                let expired = state.exit.as_ref().is_some_and(|ex| exit_progress(ex) >= 1.0)
                    || performance_now() - state.last_render > 1000.0;
                if expired { state.timer.take() } else { None }
            };
            if let Some(timer) = finish_timer {
                timer.clear();
                (inner.on_done)();
            } else {
                inner.tui.request_render(None);
            }
        });
        timer.unref();
        inner.state.lock().unwrap().timer = Some(timer);

        Self { inner }
    }

    /// Play the exit animation. A second call skips it.
    pub fn close(&self) {
        let finish_timer = {
            let mut state = self.inner.state.lock().unwrap();
            if state.exit.is_some() {
                state.timer.take()
            } else {
                let time = self.elapsed();
                let yaw = spin_angle(time);
                let turn = std::f64::consts::TAU;
                let offsets = block_offsets(&self.inner.model, &mut state.shuffle, time);
                state.exit = Some(ExitState {
                    start: performance_now(),
                    time,
                    yaw,
                    target_yaw: (yaw / turn).ceil() * turn,
                    offsets,
                });
                None
            }
        };
        if let Some(timer) = finish_timer {
            timer.clear();
            (self.inner.on_done)();
        }
    }

    pub fn handle_input(&self, data: &str) {
        let keybindings = get_keybindings();
        if keybindings.matches(data, "tui.select.cancel") || keybindings.matches(data, "app.clear") {
            self.close();
        }
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> TuiMouseEventResult {
        if event.r#type == TuiMouseEventType::Click {
            self.close();
        }
        TuiMouseEventResult {
            handled: Some(true),
            render: Some(false),
            ..Default::default()
        }
    }

    pub fn invalidate(&self) {
        let mut state = self.inner.state.lock().unwrap();
        state.screen_width = None;
        state.screen_height = None;
        state.ansi_cache.clear();
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let height = self.inner.tui.terminal().rows().max(1) as usize;
        let foreground = self.inner.foreground;
        let background = self.inner.background;
        let now_ms = performance_now();

        let mut state = self.inner.state.lock().unwrap();
        state.last_render = now_ms;
        if state.screen_width != Some(width) || state.screen_height != Some(height) {
            state.screen_width = Some(width);
            state.screen_height = Some(height);
            state.cells = prepare_cells(
                &self.inner.screen,
                &self.inner.model,
                width,
                height,
                foreground,
                background,
            );
            state.stars = prepare_stars(width, height);
        }

        // Dissolve time runs forward on entry and backward on exit, so the screen reassembles in reverse order.
        let dissolve_time: f64;
        let mut pose: Pose;
        let hint_alpha: f64;
        let star_alpha: f64;
        let offsets: Vec<[f64; 3]>;
        if let Some(ref exit) = state.exit {
            let progress = exit_progress(exit);
            let landing = clamp01(progress / 0.8);
            let settle = 1.0 - smooth(landing);
            dissolve_time = exit.time.min(DISSOLVE_END) * (1.0 - progress);
            pose = compute_pose(
                &self.inner.model,
                width,
                height,
                fly_progress(exit.time) * settle,
                exit.time,
            );
            pose.yaw = exit.yaw + (exit.target_yaw - exit.yaw) * ease_out(landing);
            pose.pitch *= settle;
            pose.roll *= settle;
            hint_alpha = compute_hint_alpha(exit.time) * (1.0 - smooth(progress / 0.2));
            star_alpha = compute_star_alpha(exit.time) * (1.0 - smooth(progress / 0.3));
            // Blocks are home well before the logo lands.
            let gather = 1.0 - smooth(progress / 0.5);
            offsets = exit
                .offsets
                .iter()
                .map(|&[x, y, z]| [x * gather, y * gather, z * gather])
                .collect();
        } else {
            let time = (now_ms - self.inner.start_time) / 1000.0;
            dissolve_time = time;
            pose = compute_pose(&self.inner.model, width, height, fly_progress(time), time);
            hint_alpha = compute_hint_alpha(time);
            star_alpha = compute_star_alpha(time);
            offsets = block_offsets(&self.inner.model, &mut state.shuffle, time);
        }

        let columns = self.inner.model.columns as f64;
        let rows = self.inner.model.rows as f64;
        let boxes: Vec<Box3d> = self
            .inner
            .model
            .blocks
            .iter()
            .zip(offsets.iter())
            .map(|(block, &[dx, dy, dz])| {
                let x = (block.home[0] as f64) - columns / 2.0 + dx;
                let y = (block.home[1] as f64) - rows / 2.0 + dy;
                Box3d {
                    min: [x, y, dz - DEPTH / 2.0],
                    max: [x + 1.0, y + 1.0, dz + DEPTH / 2.0],
                    color: block.color,
                }
            })
            .collect();

        let halo_strength = if is_light(background) { LIGHT_HALO_STRENGTH } else { 0.0 };
        state
            .raster
            .render(width, height, &pose, &boxes, background, halo_strength);

        let background_fade = smooth(dissolve_time / BACKGROUND_FADE);
        let text_fade = smooth(dissolve_time / 0.6) * 0.3;
        // Once the screen has fully dissolved, the text layer is empty and can be skipped.
        let text_active = dissolve_time < DISSOLVE_END || background_fade < 1.0;
        let hint = build_hint(width, height, hint_alpha, background);
        let frame = (dissolve_time * 14.0).floor() as i64;
        let now_secs = (now_ms - self.inner.start_time) / 1000.0;
        // Star brightness is quantized, so a row only changes when one of its stars steps to another level
        // instead of on every frame.
        let star_level = pi_js::num::math_round(star_alpha * STAR_ALPHA_LEVELS) / STAR_ALPHA_LEVELS;
        let braille = &*BRAILLE;
        let mode = theme().get_color_mode();

        let EasterEgg3dState {
            ref cells,
            ref stars,
            ref raster,
            ref mut ansi_cache,
            ..
        } = *state;

        let mut lines = Vec::with_capacity(height);
        for row in 0..height {
            let row_cells = cells.get(row).map(Vec::as_slice).unwrap_or(&[]);
            let mut line = String::new();
            let mut current_fg: i64 = -1;
            let mut current_bg: i64 = -1;

            let mut column = 0usize;
            while column < width {
                let index = row * width + column;
                let cell = if text_active { row_cells.get(column) } else { None };
                let base_bg = cell
                    .and_then(|c| c.bg)
                    .filter(|_| background_fade < 1.0)
                    .map(|c| mix(c, background, background_fade));
                let cell_bg = halo_bg(raster, background, index, base_bg);

                if let Some(ref h) = hint
                    && row == h.row
                    && column >= h.start
                    && column < h.start + h.chars.len()
                {
                    let offset = column - h.start;
                    let ch = h.chars[offset];
                    let mut utf8_buf = [0u8; 4];
                    let ch_str = ch.encode_utf8(&mut utf8_buf);
                    let fg = if offset < h.key_length { h.key_color } else { h.color };
                    emit(
                        &mut line,
                        &mut current_fg,
                        &mut current_bg,
                        ansi_cache,
                        mode,
                        ch_str,
                        Some(fg),
                        cell_bg,
                    );
                    column += 1;
                    continue;
                }

                let dots = raster.bits[index];
                if dots != 0 {
                    // Quantized so neighboring cells on one face usually share a color and its escape sequence.
                    let step = LOGO_COLOR_STEP * (raster.counts[index] as f64);
                    let rgb = &raster.rgb;
                    let color: Rgb = [
                        pi_js::num::math_round((rgb[index * 3] as f64) / step) * LOGO_COLOR_STEP,
                        pi_js::num::math_round((rgb[index * 3 + 1] as f64) / step) * LOGO_COLOR_STEP,
                        pi_js::num::math_round((rgb[index * 3 + 2] as f64) / step) * LOGO_COLOR_STEP,
                    ];
                    let mut utf8_buf = [0u8; 4];
                    let ch_str = braille[dots as usize].encode_utf8(&mut utf8_buf);
                    emit(
                        &mut line,
                        &mut current_fg,
                        &mut current_bg,
                        ansi_cache,
                        mode,
                        ch_str,
                        Some(color),
                        cell_bg,
                    );
                    column += 1;
                    continue;
                }

                let Some(cell) = cell else {
                    if let Some((star_char, star_fg)) =
                        starfield(stars, index, star_level, now_secs, foreground, background)
                    {
                        let mut utf8_buf = [0u8; 4];
                        let ch_str = star_char.encode_utf8(&mut utf8_buf);
                        emit(
                            &mut line,
                            &mut current_fg,
                            &mut current_bg,
                            ansi_cache,
                            mode,
                            ch_str,
                            Some(star_fg),
                            cell_bg,
                        );
                    } else {
                        emit(
                            &mut line,
                            &mut current_fg,
                            &mut current_bg,
                            ansi_cache,
                            mode,
                            " ",
                            None,
                            cell_bg,
                        );
                    }
                    column += 1;
                    continue;
                };

                let dust = (dissolve_time - cell.delay) / DUST_DURATION;
                if dust < 0.0 && cell.width == 2 && raster.bits.get(index + 1).copied().unwrap_or(0) == 0 {
                    emit(
                        &mut line,
                        &mut current_fg,
                        &mut current_bg,
                        ansi_cache,
                        mode,
                        &cell.text,
                        Some(mix(cell.fg, background, text_fade)),
                        cell_bg,
                    );
                    column += 2;
                    continue;
                }
                if dust < 0.0 && cell.width == 1 {
                    emit(
                        &mut line,
                        &mut current_fg,
                        &mut current_bg,
                        ansi_cache,
                        mode,
                        &cell.text,
                        Some(mix(cell.fg, background, text_fade)),
                        cell_bg,
                    );
                    column += 1;
                    continue;
                }
                let dot_count = if dust < 0.0 {
                    cell.ink
                } else {
                    pi_js::num::math_round((cell.ink as f64) * (1.0 - clamp01(dust))) as usize
                };
                if dot_count == 0 {
                    if let Some((star_char, star_fg)) =
                        starfield(stars, index, star_level, now_secs, foreground, background)
                    {
                        let mut utf8_buf = [0u8; 4];
                        let ch_str = star_char.encode_utf8(&mut utf8_buf);
                        emit(
                            &mut line,
                            &mut current_fg,
                            &mut current_bg,
                            ansi_cache,
                            mode,
                            ch_str,
                            Some(star_fg),
                            cell_bg,
                        );
                    } else {
                        emit(
                            &mut line,
                            &mut current_fg,
                            &mut current_bg,
                            ansi_cache,
                            mode,
                            " ",
                            None,
                            cell_bg,
                        );
                    }
                    column += 1;
                    continue;
                }

                // Pick `dotCount` distinct dots; the choice changes a few times per second so the dust shimmers.
                let mut bits: u8 = 0;
                let mut placed = 0usize;
                let mut attempt = 0u64;
                let base_hash = cell
                    .seed
                    .wrapping_mul(7919)
                    .wrapping_add((frame as u64).wrapping_mul(131));
                while placed < dot_count && attempt < 32 {
                    let dot_idx = ((hash(base_hash.wrapping_add(attempt) as u32) * 8.0).floor() as usize).min(7);
                    let bit = DOT_BITS[dot_idx];
                    attempt += 1;
                    if (bits & bit) != 0 {
                        continue;
                    }
                    bits |= bit;
                    placed += 1;
                }
                let fade = text_fade + (1.0 - text_fade) * clamp01(dust).powf(0.8);
                let mut utf8_buf = [0u8; 4];
                let ch_str = braille[bits as usize].encode_utf8(&mut utf8_buf);
                emit(
                    &mut line,
                    &mut current_fg,
                    &mut current_bg,
                    ansi_cache,
                    mode,
                    ch_str,
                    Some(mix(cell.fg, background, fade)),
                    cell_bg,
                );
                column += 1;
            }
            line.push_str("\x1b[0m");
            lines.push(line);
        }
        lines
    }

    fn elapsed(&self) -> f64 {
        (performance_now() - self.inner.start_time) / 1000.0
    }
}

impl Component for EasterEgg3dAnimation {
    fn render(&self, width: usize) -> Vec<String> {
        EasterEgg3dAnimation::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        EasterEgg3dAnimation::handle_input(self, data);
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        Some(EasterEgg3dAnimation::handle_mouse(self, event))
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        EasterEgg3dAnimation::invalidate(self);
    }

    fn has_handle_input(&self) -> bool {
        true
    }
}

fn exit_progress(exit: &ExitState) -> f64 {
    clamp01((performance_now() - exit.start) / 1000.0 / EXIT_DURATION)
}

fn halo_bg(raster: &BlockRaster, background: Rgb, index: usize, base: Option<Rgb>) -> Option<Rgb> {
    let amount = raster.halo_amount[index] as f64;
    if amount <= 0.0 {
        return base;
    }
    let color = &raster.halo_rgb;
    Some(mix(
        base.unwrap_or(background),
        [
            color[index * 3] as f64,
            color[index * 3 + 1] as f64,
            color[index * 3 + 2] as f64,
        ],
        amount,
    ))
}

fn starfield(
    stars: &[Option<Star>],
    index: usize,
    star_level: f64,
    now_secs: f64,
    foreground: Rgb,
    background: Rgb,
) -> Option<(char, Rgb)> {
    if star_level <= 0.0 {
        return None;
    }
    let star = stars.get(index)?.as_ref()?;
    let flicker =
        pi_js::num::math_round((0.5 + 0.5 * (now_secs * star.speed + star.phase).sin()) * STAR_FLICKER_LEVELS)
            / STAR_FLICKER_LEVELS;
    Some((
        star.glyph,
        mix(
            background,
            mix(foreground, star.tint, 0.5),
            star.strength * (0.7 + 0.3 * flicker) * star_level,
        ),
    ))
}

fn pack_rgb(color: Rgb) -> u32 {
    let r = (pi_js::num::math_round(color[0]) as i64).clamp(0, 255) as u32;
    let g = (pi_js::num::math_round(color[1]) as i64).clamp(0, 255) as u32;
    let b = (pi_js::num::math_round(color[2]) as i64).clamp(0, 255) as u32;
    (r << 16) | (g << 8) | b
}

fn cached_ansi(
    ansi_cache: &mut IndexMap<u32, String>,
    mode: pi_tui::TerminalColorMode,
    key: u32,
    is_background: bool,
) -> String {
    let cache_key = key * 2 + u32::from(is_background);
    if let Some(existing) = ansi_cache.get(&cache_key) {
        return existing.clone();
    }
    let color = Color::Rgb(RgbColorValue {
        r: ((key >> 16) & 255) as f64,
        g: ((key >> 8) & 255) as f64,
        b: (key & 255) as f64,
    });
    let value = if is_background {
        background_ansi(color, mode)
    } else {
        foreground_ansi(color, mode)
    };
    ansi_cache.insert(cache_key, value.clone());
    value
}

#[allow(clippy::too_many_arguments)]
fn emit(
    line: &mut String,
    current_fg: &mut i64,
    current_bg: &mut i64,
    ansi_cache: &mut IndexMap<u32, String>,
    mode: pi_tui::TerminalColorMode,
    text: &str,
    fg: Option<Rgb>,
    bg: Option<Rgb>,
) {
    let fg_key = fg.map_or(-1, |c| i64::from(pack_rgb(c)));
    let bg_key = bg.map_or(-1, |c| i64::from(pack_rgb(c)));
    if fg_key != *current_fg && text != " " {
        if fg_key >= 0 {
            line.push_str(&cached_ansi(ansi_cache, mode, fg_key as u32, false));
        } else {
            line.push_str("\x1b[39m");
        }
        *current_fg = fg_key;
    }
    if bg_key != *current_bg {
        if bg_key >= 0 {
            line.push_str(&cached_ansi(ansi_cache, mode, bg_key as u32, true));
        } else {
            line.push_str("\x1b[49m");
        }
        *current_bg = bg_key;
    }
    line.push_str(text);
}

/// Each block's displacement from its place in the model at `time`, in grid units.
fn block_offsets(model: &Model, shuffle: &mut Option<ShuffleCache>, time: f64) -> Vec<[f64; 3]> {
    let blocks = &model.blocks;
    if time < PUZZLE_START {
        return vec![[0.0, 0.0, 0.0]; blocks.len()];
    }
    let cycle = ((time - PUZZLE_START) / PUZZLE_CYCLE).floor() as usize;
    let local = time - PUZZLE_START - (cycle as f64) * PUZZLE_CYCLE;
    let steps = match shuffle {
        Some(cache) if cache.cycle == cycle => &cache.steps,
        _ => {
            *shuffle = Some(ShuffleCache {
                cycle,
                steps: shuffle_steps(model, cycle),
            });
            match shuffle {
                Some(cache) => &cache.steps,
                None => return vec![[0.0, 0.0, 0.0]; blocks.len()],
            }
        }
    };

    let offset = |position: Cell3, index: usize| -> [f64; 3] {
        let block = blocks[index].home;
        [
            (position[0] - block[0]) as f64,
            (position[1] - block[1]) as f64,
            (position[2] - block[2]) as f64,
        ]
    };

    let shuffle_end = PUZZLE_STEP * (PUZZLE_STEPS as f64);
    if local < shuffle_end {
        let step = ((local / PUZZLE_STEP).floor() as usize).min(PUZZLE_STEPS - 1);
        let progress = smooth((local - (step as f64) * PUZZLE_STEP) / PUZZLE_STEP);
        return steps[step]
            .iter()
            .enumerate()
            .map(|(index, &from)| {
                let a = offset(from, index);
                let b = offset(steps[step + 1][index], index);
                [
                    a[0] + (b[0] - a[0]) * progress,
                    a[1] + (b[1] - a[1]) * progress,
                    a[2] + (b[2] - a[2]) * progress,
                ]
            })
            .collect();
    }
    if local < shuffle_end + PUZZLE_RETURN {
        // All blocks fly home at once. Alternating arcs in depth keep them from passing through each other.
        let u = (local - shuffle_end) / PUZZLE_RETURN;
        let remaining = 1.0 - smooth(u);
        let arc = (std::f64::consts::PI * clamp01(u)).sin() * 0.8;
        return steps[PUZZLE_STEPS]
            .iter()
            .enumerate()
            .map(|(index, &position)| {
                let [x, y, z] = offset(position, index);
                [
                    x * remaining,
                    y * remaining,
                    z * remaining + if index.is_multiple_of(2) { arc } else { -arc },
                ]
            })
            .collect();
    }
    vec![[0.0, 0.0, 0.0]; blocks.len()]
}

fn fly_progress(time: f64) -> f64 {
    smooth((time - FLY_START) / FLY_DURATION)
}

fn compute_star_alpha(time: f64) -> f64 {
    smooth((time - STARS_START) / STARS_FADE)
}

fn compute_hint_alpha(time: f64) -> f64 {
    smooth((time - FLY_START - FLY_DURATION) / HINT_FADE)
}

fn compute_pose(model: &Model, width: usize, height: usize, progress: f64, time: f64) -> Pose {
    let columns = model.columns as f64;
    let rows = model.rows as f64;
    let camera_distance = model.camera_distance;
    let radius = model.radius;
    let width_share = model.width_share;

    let reach = radius * (camera_distance / (camera_distance - radius));
    // Lifting off, the front face must cover exactly the half-block cells (2x2 dots per pixel) despite the
    // perspective.
    let start_scale = if model.origin.is_some() {
        (2.0 * (camera_distance - DEPTH / 2.0)) / camera_distance
    } else {
        START_SCALE
    };
    let end_scale =
        start_scale.max(((width as f64) * 2.0 * width_share).min(((height as f64) * 4.0 - 8.0) * 0.48) / reach);
    let end_x = width as f64;
    let end_y = (height as f64) * 2.0 - 2.0;
    let (start_x, start_y) = match model.origin {
        Some(origin) => ((origin.column as f64) * 2.0 + columns, (origin.row as f64) * 4.0 + rows),
        None => (end_x, end_y),
    };
    let phase = spin_phase(time);
    Pose {
        center_x: start_x + (end_x - start_x) * progress,
        center_y: start_y + (end_y - start_y) * progress,
        // Interpolate the zoom geometrically so it feels uniform.
        scale: start_scale * (end_scale / start_scale).powf(progress),
        yaw: spin_angle(time),
        // Tilts are zero at whole turns, so the camera looks straight at the front of the reassembled logo.
        pitch: 0.3 * phase.sin() * progress,
        roll: 0.06 * (2.0 * phase).sin() * progress,
    }
}

fn build_hint(width: usize, height: usize, alpha: f64, background: Rgb) -> Option<HintLayout> {
    if alpha <= 0.0 || height < 2 {
        return None;
    }
    let keys = get_keybindings().get_keys("tui.select.cancel");
    let raw_key = keys.first().map(String::as_str).unwrap_or("escape");
    let key = format_key_text(raw_key, KeyTextFormatOptions::default());
    let text = format!("{key} to return");
    let chars: Vec<char> = text.chars().collect();
    if chars.len() > width {
        return None;
    }
    let colors = theme().colors();
    let muted = theme_color_rgb(
        &colors,
        ThemeColor::Muted,
        Color::Rgb(RgbColorValue {
            r: 128.0,
            g: 128.0,
            b: 128.0,
        }),
    );
    let dim = theme_color_rgb(
        &colors,
        ThemeColor::Dim,
        Color::Rgb(RgbColorValue {
            r: 100.0,
            g: 100.0,
            b: 100.0,
        }),
    );
    Some(HintLayout {
        row: height - 2,
        start: (width - chars.len()) / 2,
        chars,
        key_length: key.chars().count(),
        key_color: mix(background, muted, alpha),
        color: mix(background, dim, alpha),
    })
}

/// A sparse, deterministic starfield: one braille dot in about STAR_DENSITY of all cells.
fn prepare_stars(width: usize, height: usize) -> Vec<Option<Star>> {
    let total = width * height;
    let mut stars = Vec::with_capacity(total);
    let braille = &*BRAILLE;
    for index in 0..total {
        if hash((index as u64 * 3 + 0x51ed) as u32) >= STAR_DENSITY {
            stars.push(None);
            continue;
        }
        let random = |salt: u64| hash((index as u64 * 7 + salt * 0x9e37) as u32);
        let dot_idx = ((random(1) * 8.0).floor() as usize).min(7);
        let tint_idx = ((random(2) * (STAR_TINTS.len() as f64)).floor() as usize).min(STAR_TINTS.len() - 1);
        stars.push(Some(Star {
            glyph: braille[DOT_BITS[dot_idx] as usize],
            tint: STAR_TINTS[tint_idx],
            strength: 0.1 + random(3) * 0.12,
            phase: random(4) * std::f64::consts::TAU,
            speed: 0.4 + random(5) * 0.8,
        }));
    }
    stars
}

fn prepare_cells(
    screen: &[String],
    model: &Model,
    width: usize,
    height: usize,
    foreground: Rgb,
    background: Rgb,
) -> Vec<Vec<ScreenCell>> {
    let mut cells = parse_screen(screen, width, foreground, background);
    let (center_x, center_y) = match model.origin {
        Some(origin) => (
            (origin.column as f64) + (model.columns as f64) / 2.0,
            (origin.row as f64) + (model.rows as f64) / 4.0,
        ),
        None => ((width as f64) / 2.0, (height as f64) / 2.0),
    };
    let dx_max = center_x.max((width as f64) - center_x);
    let dy_max = center_y.max((height as f64) - center_y) * 2.0;
    let farthest = dx_max.hypot(dy_max);

    for (row, line) in cells.iter_mut().enumerate() {
        for column in 0..line.len() {
            let cell_width = line[column].width;
            let (owner_text, owner_delay) = if cell_width == 0 && column > 0 {
                (line[column - 1].text.clone(), line[column - 1].delay)
            } else {
                (line[column].text.clone(), line[column].delay)
            };
            let cell_seed = (row as u64) * 65_537 + (column as u64);
            let seed = hash(cell_seed as u32);
            let delay = if cell_width != 0 {
                let distance = if farthest > 0.0 {
                    ((column as f64) - center_x).hypot(((row as f64) - center_y) * 2.0) / farthest
                } else {
                    0.0
                };
                FLY_START + distance * WAVE_SPREAD + seed * WAVE_JITTER
            } else {
                owner_delay
            };
            let cell = &mut line[column];
            cell.delay = delay;
            cell.seed = cell_seed;
            cell.ink = glyph_ink(&owner_text, seed);
        }
    }

    // The 3D model replaces its half-block rendering on screen.
    if let Some(origin) = model.origin {
        let end_row = origin.row + model.rows.div_ceil(2);
        for row in origin.row..end_row {
            let Some(line) = cells.get_mut(row) else {
                continue;
            };
            for column in origin.column..(origin.column + model.columns) {
                let Some(cell) = line.get_mut(column) else {
                    continue;
                };
                cell.text = " ".to_string();
                cell.width = 1;
                cell.ink = 0;
                cell.bg = None;
            }
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_trait::async_trait;
    use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
    use pi_tui::tui_alt_screen::TuiAltScreen;
    use serial_test::serial;

    use super::*;

    struct TestTerminal {
        on_input: Mutex<Option<TerminalInputHandler>>,
    }

    impl TestTerminal {
        fn new() -> Self {
            Self {
                on_input: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl Terminal for TestTerminal {
        fn start(&self, on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {
            *self.on_input.lock().unwrap() = Some(on_input);
        }
        fn stop(&self) {
            *self.on_input.lock().unwrap() = None;
        }
        async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
        fn write(&self, _data: &str) {}
        fn columns(&self) -> i64 {
            80
        }
        fn rows(&self) -> i64 {
            24
        }
        fn kitty_protocol_active(&self) -> bool {
            false
        }
        fn move_by(&self, _lines: i64) {}
        fn hide_cursor(&self) {}
        fn show_cursor(&self) {}
        fn clear_line(&self) {}
        fn clear_from_cursor(&self) {}
        fn clear_screen(&self) {}
        fn set_title(&self, _title: &str) {}
        fn set_progress(&self, _active: bool) {}
    }

    #[test]
    fn models_and_hash_properties() {
        let logo = pi_logo_model(ModelOrigin { column: 2, row: 1 });
        assert_eq!(logo.columns, 4);
        assert_eq!(logo.rows, 4);
        assert_eq!(logo.blocks.len(), 10);
        assert_eq!(logo.origin.map(|o| (o.column, o.row)), Some((2, 1)));

        let armin = armin_model([255.0, 255.0, 255.0]);
        assert_eq!(armin.columns, ARMIN_WIDTH);
        assert_eq!(armin.rows, ARMIN_HEIGHT);
        assert!(!armin.blocks.is_empty());
        assert!(armin.origin.is_none());

        // Deterministic hash in [0, 1)
        for v in 0..1000u32 {
            let h = hash(v);
            assert!((0.0..1.0).contains(&h));
        }

        // Shuffle steps are deterministic per cycle and preserve block count
        let steps_a = shuffle_steps(&logo, 0);
        let steps_b = shuffle_steps(&logo, 0);
        assert_eq!(steps_a, steps_b);
        assert_eq!(steps_a.len(), PUZZLE_STEPS + 1);
        for step in &steps_a {
            assert_eq!(step.len(), logo.blocks.len());
        }
    }

    #[test]
    fn parse_screen_handles_sgr_wide_chars_and_osc() {
        let lines = vec!["\x1b[38;2;10;20;30mA\x1b[7m界\x1b]8;;https://example.com\x07!\x1b[0m".to_string()];
        let parsed = parse_screen(&lines, 10, [200.0, 200.0, 200.0], [0.0, 0.0, 0.0]);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].len(), 4);
        assert_eq!(parsed[0][0].text, "A");
        assert_eq!(parsed[0][0].fg, [10.0, 20.0, 30.0]);
        assert_eq!(parsed[0][1].text, "界");
        assert_eq!(parsed[0][1].width, 2);
        assert_eq!(parsed[0][1].fg, [0.0, 0.0, 0.0]);
        assert_eq!(parsed[0][1].bg, Some([10.0, 20.0, 30.0]));
        assert_eq!(parsed[0][2].width, 0);
        assert_eq!(parsed[0][3].text, "!");
    }

    #[tokio::test(start_paused = true)]
    async fn animation_renders_and_closes_on_escape_and_click() {
        let term = Arc::new(TestTerminal::new());
        let tui: Arc<dyn TUI + Send + Sync> = Arc::new(TuiAltScreen::new(term, None, None, None));
        let done_count = Arc::new(Mutex::new(0usize));
        let done_cb = Arc::clone(&done_count);
        let anim = EasterEgg3dAnimation::new(
            tui,
            vec!["hello world".to_string(), "second line".to_string()],
            EasterEgg3d::PiLogo { column: 1, row: 0 },
            EasterEgg3dColors {
                foreground: [220.0, 220.0, 220.0],
                background: [240.0, 240.0, 240.0], // light background exercises halo pass
            },
            Arc::new(move || {
                *done_cb.lock().unwrap() += 1;
            }),
        );
        assert!(anim.has_handle_input());

        // Entry frames
        let frame0 = anim.render(80);
        assert_eq!(frame0.len(), 24);
        tokio::time::advance(Duration::from_millis(2000)).await;
        let frame_mid = anim.render(80);
        assert_eq!(frame_mid.len(), 24);

        // First escape starts exit animation
        anim.handle_input("\x1b");
        assert_eq!(*done_count.lock().unwrap(), 0);
        let frame_exit = anim.render(80);
        assert_eq!(frame_exit.len(), 24);

        // Second click skips exit animation and finishes
        let res = anim.handle_mouse(&TuiMouseEvent {
            r#type: TuiMouseEventType::Click,
            button: pi_tui::TuiMouseButton::Left,
            x: 0,
            y: 0,
            screen_x: 0,
            screen_y: 0,
            width: 80,
            height: 24,
            shift: false,
            alt: false,
            ctrl: false,
            wheel_delta: None,
            click_count: Some(1),
        });
        assert_eq!(res.handled, Some(true));
        assert_eq!(*done_count.lock().unwrap(), 1);
    }

    #[tokio::test(start_paused = true)]
    #[serial]
    async fn play_easter_egg_3d_mounts_overlay_and_cleans_playing_state() {
        let term = Arc::new(TestTerminal::new());
        let alt = Arc::new(TuiAltScreen::new(term, None, None, None));
        let tui: Arc<dyn TUI + Send + Sync> = Arc::clone(&alt) as Arc<dyn TUI + Send + Sync>;
        let tui_task = Arc::clone(&tui);
        let handle = tokio::spawn(async move {
            play_easter_egg_3d(tui_task, vec!["line 1".to_string()], EasterEgg3d::Armin).await;
        });

        // Advance past queryTerminalColors 100ms timeout
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(120)).await;
        handle.await.unwrap();

        assert!(alt.has_overlay());
        assert!(PLAYING.load(Ordering::SeqCst));

        // Advancing >1000ms with no render triggers auto-finish and hides overlay
        tokio::time::advance(Duration::from_millis(1100)).await;
        tokio::task::yield_now().await;

        assert!(!alt.has_overlay());
        assert!(!PLAYING.load(Ordering::SeqCst));
    }
}
