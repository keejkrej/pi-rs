//! Port of packages/tui/src/terminal.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::stdin_buffer::StdinBuffer;

const TERMINAL_PROGRESS_KEEPALIVE_MS: i64 = 1000;
const TERMINAL_PROGRESS_ACTIVE_SEQUENCE: &str = "\x1b]9;4;3\x07";
const TERMINAL_PROGRESS_CLEAR_SEQUENCE: &str = "\x1b]9;4;0\x07";
const NATIVE_SHIFT_ENTER_SEQUENCE: &str = "\x1b[13;2u";
const DESIRED_KITTY_KEYBOARD_PROTOCOL_FLAGS: i64 = 7;
const KEYBOARD_PROTOCOL_RESPONSE_FRAGMENT_TIMEOUT_MS: i64 = 150;
const KITTY_KEYBOARD_PROTOCOL_QUERY: &str = "\x1b[>7u\x1b[?u\x1b[c";

const DEFAULT_ESCAPE_TIMEOUT_MS: i64 = 10;
const DEFAULT_SSH_ESCAPE_TIMEOUT_MS: i64 = 100;

/// `{ type: "kitty-flags", flags } | { type: "device-attributes" }`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum KeyboardProtocolNegotiationSequence {
    #[serde(rename = "kitty-flags")]
    KittyFlags { flags: i64 },
    #[serde(rename = "device-attributes")]
    DeviceAttributes,
}

pub fn parse_keyboard_protocol_negotiation_sequence(sequence: &str) -> Option<KeyboardProtocolNegotiationSequence> {
    todo!("port: parse_keyboard_protocol_negotiation_sequence")
}

fn is_keyboard_protocol_negotiation_sequence_prefix(sequence: &str) -> bool {
    todo!("port: is_keyboard_protocol_negotiation_sequence_prefix")
}

pub fn is_apple_terminal_session() -> bool {
    todo!("port: is_apple_terminal_session")
}

/// Refresh terminal dimensions on POSIX platforms by sending SIGWINCH to this process.
/// Best-effort: some environments (restricted seccomp or LSM policies) return EACCES
/// for `kill(2)`; in that case the dimensions refresh is skipped rather than crashing.
pub fn refresh_terminal_dimensions() {
    todo!("port: refresh_terminal_dimensions")
}

pub fn normalize_native_shift_enter_input(
    data: &str,
    should_detect_native_shift_enter: bool,
    is_shift_pressed: bool,
) -> String {
    todo!("port: normalize_native_shift_enter_input")
}

pub fn normalize_apple_terminal_input(data: &str, is_apple_terminal: bool, is_shift_pressed: bool) -> String {
    todo!("port: normalize_apple_terminal_input")
}

pub type TerminalInputHandler = Arc<dyn Fn(&str) + Send + Sync>;
pub type TerminalResizeHandler = Arc<dyn Fn() + Send + Sync>;

/// Minimal terminal interface for TUI.
#[async_trait]
pub trait Terminal: Send + Sync {
    /// Start the terminal with input and resize handlers.
    fn start(&self, on_input: TerminalInputHandler, on_resize: TerminalResizeHandler);

    /// Stop the terminal and restore state.
    fn stop(&self);

    /// Drain stdin before exiting to prevent Kitty key release events from
    /// leaking to the parent shell over slow SSH connections.
    ///
    /// `max_ms` is the maximum time to drain. `idle_ms` exits early if no input arrives within this time.
    ///
    /// PORT: `None` selects the TS defaults, 1000ms and 50ms.
    async fn drain_input(&self, max_ms: Option<i64>, idle_ms: Option<i64>);

    /// Write output to terminal.
    fn write(&self, data: &str);

    /// Get terminal dimensions.
    fn columns(&self) -> i64;

    /// Get terminal dimensions.
    fn rows(&self) -> i64;

    /// Whether Kitty keyboard protocol is active.
    fn kitty_protocol_active(&self) -> bool;

    /// Cursor positioning (relative to current position).
    /// Move cursor up (negative) or down (positive) by N lines.
    fn move_by(&self, lines: i64);

    /// Hide the cursor.
    fn hide_cursor(&self);

    /// Show the cursor.
    fn show_cursor(&self);

    /// Clear current line.
    fn clear_line(&self);

    /// Clear from cursor to end of screen.
    fn clear_from_cursor(&self);

    /// Clear entire screen and move cursor to (0,0).
    fn clear_screen(&self);

    /// Set terminal window title.
    fn set_title(&self, title: &str);

    /// Progress indicator (OSC 9;4).
    fn set_progress(&self, active: bool);
}

/// Resolve how long to wait for the rest of an escape sequence before
/// dispatching a lone ESC as the Escape key. Legacy Alt+key input is ESC plus
/// another byte, so high-latency transports need a longer reassembly window.
///
/// PORT: `None` selects `process.env` (`pi_js::env::vars()`), the TS default.
pub fn resolve_escape_timeout_ms(env: Option<&IndexMap<String, String>>) -> f64 {
    todo!("port: resolve_escape_timeout_ms")
}

/// `{ parsed, sequence } | "pending"`. `None` from the reader is TS `undefined`.
enum KeyboardProtocolNegotiationRead {
    /// `"pending"` — wait briefly for the rest of a split Kitty response.
    Pending,
    Ready {
        parsed: KeyboardProtocolNegotiationSequence,
        sequence: String,
    },
}

/// Real terminal using process.stdin/stdout.
#[derive(Clone)]
pub struct ProcessTerminal {
    inner: Arc<ProcessTerminalInner>,
}

struct ProcessTerminalInner {
    write_log_path: String,
    state: Mutex<ProcessTerminalState>,
}

struct ProcessTerminalState {
    was_raw: bool,
    input_handler: Option<TerminalInputHandler>,
    resize_handler: Option<TerminalResizeHandler>,
    kitty_protocol_active: bool,
    modify_other_keys_active: bool,
    keyboard_protocol_pushed: bool,
    /// DA1 replies owed to keyboard protocol queries. Later DA1 replies answer other queries and are forwarded.
    pending_keyboard_protocol_device_attributes: i64,
    keyboard_protocol_negotiation_buffer: String,
    keyboard_protocol_buffer_flush_timer: Option<pi_js::time::Timeout>,
    stdin_buffer: Option<StdinBuffer>,
    stdin_data_handler: Option<TerminalInputHandler>,
    progress_interval: Option<pi_js::time::Interval>,
}

impl ProcessTerminal {
    pub fn new() -> Self {
        todo!("port: ProcessTerminal::new")
    }

    pub fn modify_other_keys_active(&self) -> bool {
        self.inner.state.lock().unwrap().modify_other_keys_active
    }

    /// Set up StdinBuffer to split batched input into individual sequences.
    /// This ensures components receive single events, making matchesKey/isKeyRelease work correctly.
    ///
    /// Also watches for Kitty protocol response and enables it when detected.
    /// This is done here (after stdinBuffer parsing) rather than on raw stdin
    /// to handle the case where the response arrives split across multiple events.
    fn setup_stdin_buffer(&self) {
        todo!("port: ProcessTerminal::setup_stdin_buffer")
    }

    /// Query terminal for Kitty keyboard protocol support and enable it if available.
    ///
    /// Kitty's progressive enhancement detection requires requesting the desired
    /// flags before querying them. The trailing DA query is a sentinel supported by
    /// terminals that do not know Kitty keyboard protocol; receiving DA before a
    /// Kitty response enables modifyOtherKeys fallback without a startup timeout.
    ///
    /// The requested flags are:
    /// - 1 = disambiguate escape codes
    /// - 2 = report event types (press/repeat/release)
    /// - 4 = report alternate keys (shifted key, base layout key)
    fn query_and_enable_kitty_protocol(&self) {
        todo!("port: ProcessTerminal::query_and_enable_kitty_protocol")
    }

    fn handle_keyboard_protocol_negotiation_sequence(
        &self,
        negotiation_sequence: KeyboardProtocolNegotiationSequence,
    ) -> bool {
        todo!("port: ProcessTerminal::handle_keyboard_protocol_negotiation_sequence")
    }

    /// Returns the parsed negotiation reply with its full (possibly reassembled) sequence.
    fn read_keyboard_protocol_negotiation_sequence(&self, sequence: &str) -> Option<KeyboardProtocolNegotiationRead> {
        todo!("port: ProcessTerminal::read_keyboard_protocol_negotiation_sequence")
    }

    fn set_keyboard_protocol_negotiation_buffer(&self, sequence: &str) {
        todo!("port: ProcessTerminal::set_keyboard_protocol_negotiation_buffer")
    }

    fn clear_keyboard_protocol_negotiation_buffer(&self) {
        todo!("port: ProcessTerminal::clear_keyboard_protocol_negotiation_buffer")
    }

    fn flush_keyboard_protocol_negotiation_buffer_as_input(&self) {
        todo!("port: ProcessTerminal::flush_keyboard_protocol_negotiation_buffer_as_input")
    }

    fn schedule_keyboard_protocol_negotiation_buffer_flush(&self) {
        todo!("port: ProcessTerminal::schedule_keyboard_protocol_negotiation_buffer_flush")
    }

    fn clear_keyboard_protocol_negotiation_buffer_flush_timer(&self) {
        todo!("port: ProcessTerminal::clear_keyboard_protocol_negotiation_buffer_flush_timer")
    }

    fn forward_input_sequence(&self, sequence: &str) {
        todo!("port: ProcessTerminal::forward_input_sequence")
    }

    fn enable_modify_other_keys(&self) {
        todo!("port: ProcessTerminal::enable_modify_other_keys")
    }

    fn disable_modify_other_keys(&self) {
        todo!("port: ProcessTerminal::disable_modify_other_keys")
    }

    /// On Windows, add ENABLE_VIRTUAL_TERMINAL_INPUT (0x0200) to the stdin
    /// console handle so the terminal sends VT sequences for modified keys
    /// (e.g. \x1b[Z for Shift+Tab). Without this, libuv's ReadConsoleInputW
    /// discards modifier state and Shift+Tab arrives as plain \t.
    fn enable_windows_vt_input(&self) {
        todo!("port: ProcessTerminal::enable_windows_vt_input")
    }

    fn clear_progress_interval(&self) -> bool {
        todo!("port: ProcessTerminal::clear_progress_interval")
    }
}

#[async_trait]
impl Terminal for ProcessTerminal {
    fn start(&self, on_input: TerminalInputHandler, on_resize: TerminalResizeHandler) {
        todo!("port: ProcessTerminal::start")
    }

    fn stop(&self) {
        todo!("port: ProcessTerminal::stop")
    }

    async fn drain_input(&self, max_ms: Option<i64>, idle_ms: Option<i64>) {
        todo!("port: ProcessTerminal::drain_input")
    }

    fn write(&self, data: &str) {
        todo!("port: ProcessTerminal::write")
    }

    fn columns(&self) -> i64 {
        todo!("port: ProcessTerminal::columns")
    }

    fn rows(&self) -> i64 {
        todo!("port: ProcessTerminal::rows")
    }

    fn kitty_protocol_active(&self) -> bool {
        self.inner.state.lock().unwrap().kitty_protocol_active
    }

    fn move_by(&self, lines: i64) {
        todo!("port: ProcessTerminal::move_by")
    }

    fn hide_cursor(&self) {
        todo!("port: ProcessTerminal::hide_cursor")
    }

    fn show_cursor(&self) {
        todo!("port: ProcessTerminal::show_cursor")
    }

    fn clear_line(&self) {
        todo!("port: ProcessTerminal::clear_line")
    }

    fn clear_from_cursor(&self) {
        todo!("port: ProcessTerminal::clear_from_cursor")
    }

    fn clear_screen(&self) {
        todo!("port: ProcessTerminal::clear_screen")
    }

    fn set_title(&self, title: &str) {
        todo!("port: ProcessTerminal::set_title")
    }

    fn set_progress(&self, active: bool) {
        todo!("port: ProcessTerminal::set_progress")
    }
}
