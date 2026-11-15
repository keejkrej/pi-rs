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
    pi_js::env::platform() == "darwin"
        && pi_js::env::var("SSH_CONNECTION").is_none()
        && pi_js::env::var("SSH_CLIENT").is_none()
        && pi_js::env::var("SSH_TTY").is_none()
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
        Self {
            inner: Arc::new(WheelScrollInner {
                accelerate: accelerate.unwrap_or_else(|| !terminal_accelerates_wheel()),
                state: Mutex::new(WheelScrollState {
                    lines: lines.unwrap_or_default(),
                    last_time: f64::NEG_INFINITY,
                    last_direction: 0,
                    average_gap: None,
                    carry: 0.0,
                }),
            }),
        }
    }

    pub fn set_lines(&self, lines: WheelScrollLines) {
        {
            self.inner.state.lock().unwrap().lines = lines;
        }
        self.reset();
    }

    /// Return the positive line count for a wheel event in `direction` at time `now` (milliseconds).
    ///
    /// `direction` is `-1` or `1`. `now` is `performance.now()` (`f64` milliseconds).
    pub fn next(&self, direction: i64, now: f64) -> i64 {
        let mut state = self.inner.state.lock().unwrap();
        match state.lines {
            WheelScrollLines::Lines(lines) => {
                return if lines.is_finite() {
                    lines.floor().clamp(1.0, i64::MAX as f64) as i64
                } else {
                    1
                };
            }
            WheelScrollLines::Auto(_) => {}
        }
        if !self.inner.accelerate {
            return 1;
        }

        let gap = now - state.last_time;
        let same_gesture = direction == state.last_direction && gap <= GESTURE_GAP_MS;
        state.last_time = now;
        state.last_direction = direction;
        if !same_gesture {
            state.average_gap = None;
            state.carry = 0.0;
            return 1;
        }
        if gap < BURST_GAP_MS {
            return 1;
        }

        let avg = match state.average_gap {
            None => gap,
            Some(prev) => (prev + gap) / 2.0,
        };
        state.average_gap = Some(avg);
        let lines = (REFERENCE_GAP_MS / avg).clamp(1.0, MAX_AUTO_LINES) + state.carry;
        let whole = lines.floor();
        state.carry = lines - whole;
        whole as i64
    }

    fn reset(&self) {
        let mut state = self.inner.state.lock().unwrap();
        state.last_time = f64::NEG_INFINITY;
        state.last_direction = 0;
        state.average_gap = None;
        state.carry = 0.0;
    }
}
