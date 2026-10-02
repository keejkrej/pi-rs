//! Port of packages/tui/src/wheel-scroll.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// Lines moved per mouse-wheel event, or `"auto"` to accelerate fast wheel spins.
///
/// PORT: untagged `number | "auto"`. [`WheelScrollAuto`] exists so the unit variant
/// serializes as the string `"auto"`. `Default` is `"auto"` (the accelerator default,
/// not `TuiAltScreen`'s omitted-option default of `1`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WheelScrollLines {
    Lines(f64),
    Auto(WheelScrollAuto),
}

/// The `"auto"` string in [`WheelScrollLines`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WheelScrollAuto {
    #[serde(rename = "auto")]
    Auto,
}

impl WheelScrollLines {
    pub const AUTO: Self = Self::Auto(WheelScrollAuto::Auto);
}

impl Default for WheelScrollLines {
    fn default() -> Self {
        Self::AUTO
    }
}

// Several events closer than this belong to one physical notch (Ghostty emits them ~4 ms apart)
// or come from a high-resolution source. They move one line each and do not accelerate.
const BURST_GAP_MS: f64 = 5.0;
// A pause longer than this ends a scroll gesture.
const GESTURE_GAP_MS: f64 = 200.0;
// Average event gap that maps to one line per event. Faster events scale up proportionally.
const REFERENCE_GAP_MS: f64 = 100.0;
const MAX_AUTO_LINES: f64 = 6.0;

/// Local macOS terminals receive wheel and trackpad deltas that the OS has already accelerated,
/// and they emit one event per line. Other platforms, and SSH sessions where the client platform
/// is unknown, usually send one event per wheel notch.
fn terminal_accelerates_wheel() -> bool {
    todo!("port: terminal_accelerates_wheel")
}

struct WheelScrollState {
    lines: WheelScrollLines,
    /// `performance.now()` milliseconds. Starts at `-Infinity`.
    last_time: f64,
    last_direction: i64,
    average_gap: Option<f64>,
    carry: f64,
}

struct WheelScrollInner {
    accelerate: bool,
    state: Mutex<WheelScrollState>,
}

/// Converts wheel events into line counts.
///
/// In `"auto"` mode on terminals that do not accelerate wheel input, the count follows event
/// velocity: an isolated notch moves one line, while a fast spin moves up to six lines per event.
/// For example, notches 100 ms apart move 1 line each, 50 ms apart move 2, and 20 ms apart move 5.
///
/// PORT: TS class with identity. Handle; methods take `&self`.
#[derive(Clone)]
pub struct WheelScrollAccelerator {
    inner: Arc<WheelScrollInner>,
}

impl WheelScrollAccelerator {
    /// `lines` `None` is `"auto"`. `accelerate` `None` is `!terminal_accelerates_wheel()`.
    pub fn new(lines: Option<WheelScrollLines>, accelerate: Option<bool>) -> Self {
        todo!("port: WheelScrollAccelerator::new")
    }

    pub fn set_lines(&self, lines: WheelScrollLines) {
        todo!("port: WheelScrollAccelerator::set_lines")
    }

    /// Return the positive line count for a wheel event in `direction` at time `now` (milliseconds).
    ///
    /// `direction` is `-1` or `1`. `now` is `performance.now()` (`f64` milliseconds).
    pub fn next(&self, direction: i64, now: f64) -> i64 {
        todo!("port: WheelScrollAccelerator::next")
    }

    fn reset(&self) {
        todo!("port: WheelScrollAccelerator::reset")
    }
}
