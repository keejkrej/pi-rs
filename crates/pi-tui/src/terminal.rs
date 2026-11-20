//! Port of packages/tui/src/terminal.ts

use std::io::IsTerminal as _;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_js::time::{Interval, Timeout};
use pi_js::{env, num, time};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt as _;

use crate::keys::set_kitty_protocol_active;
use crate::native_modifiers::{ModifierKey, is_native_modifier_pressed};
use crate::native_platform::get_native_platform_helper;
use crate::program_status::{
    PROGRAM_STATUS_QUERY, ProgramStatus, ProgramStatusState, format_program_status, is_program_status_reply,
};
use crate::stdin_buffer::{StdinBuffer, StdinBufferData, StdinBufferOptions};
use crate::utils::replace_control_characters;

const TERMINAL_PROGRESS_KEEPALIVE_MS: i64 = 1000;
const TERMINAL_PROGRESS_ACTIVE_SEQUENCE: &str = "\x1b]9;4;3\x07";
const TERMINAL_PROGRESS_CLEAR_SEQUENCE: &str = "\x1b]9;4;0\x07";
const NATIVE_SHIFT_ENTER_SEQUENCE: &str = "\x1b[13;2u";
#[allow(dead_code)]
const DESIRED_KITTY_KEYBOARD_PROTOCOL_FLAGS: i64 = 7;
const KEYBOARD_PROTOCOL_RESPONSE_FRAGMENT_TIMEOUT_MS: i64 = 150;
const KITTY_KEYBOARD_PROTOCOL_QUERY: &str = "\x1b[>7u\x1b[?u";
const DEVICE_ATTRIBUTES_QUERY: &str = "\x1b[c";

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

static KITTY_FLAGS_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\x1b\[\?(\d+)u$").unwrap());
static DEVICE_ATTRIBUTES_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\x1b\[\?[\d;]*c$").unwrap());
static NEGOTIATION_PREFIX_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\x1b\[\?[\d;]*$").unwrap());

pub fn parse_keyboard_protocol_negotiation_sequence(sequence: &str) -> Option<KeyboardProtocolNegotiationSequence> {
    if let Some(caps) = KITTY_FLAGS_REGEX.captures(sequence) {
        let flags = caps.get(1)?.as_str().parse::<i64>().ok()?;
        return Some(KeyboardProtocolNegotiationSequence::KittyFlags { flags });
    }
    if DEVICE_ATTRIBUTES_REGEX.is_match(sequence) {
        return Some(KeyboardProtocolNegotiationSequence::DeviceAttributes);
    }
    None
}

fn is_keyboard_protocol_negotiation_sequence_prefix(sequence: &str) -> bool {
    sequence == "\x1b[" || NEGOTIATION_PREFIX_REGEX.is_match(sequence)
}

pub fn is_apple_terminal_session() -> bool {
    env::platform() == "darwin" && env::var("TERM_PROGRAM").as_deref() == Some("Apple_Terminal")
}

/// Refresh terminal dimensions on POSIX platforms by sending SIGWINCH to this process.
/// Best-effort: some environments (restricted seccomp or LSM policies) return EACCES
/// for `kill(2)`; in that case the dimensions refresh is skipped rather than crashing.
pub fn refresh_terminal_dimensions() {
    if env::platform() == "win32" {
        return;
    }
    let pid = std::process::id();
    if pid == 0 {
        return;
    }
    if let Some(f) = testing::OVERRIDE_KILL.with(|s| s.get()) {
        let _ = f(pid, "SIGWINCH");
        return;
    }
    #[cfg(unix)]
    {
        if let Ok(raw_pid) = i32::try_from(pid) {
            if raw_pid > 0 {
                let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw_pid), nix::sys::signal::Signal::SIGWINCH);
            }
        }
    }
}

// Some Apple Terminal setups send ESC+CR (Alt+Enter) for Shift+Enter (#11559).
fn is_native_shift_enter_candidate(data: &str) -> bool {
    data == "\r" || data == "\x1b\r"
}

pub fn normalize_native_shift_enter_input(
    data: &str,
    should_detect_native_shift_enter: bool,
    is_shift_pressed: bool,
) -> String {
    if should_detect_native_shift_enter && is_native_shift_enter_candidate(data) && is_shift_pressed {
        return NATIVE_SHIFT_ENTER_SEQUENCE.to_string();
    }
    data.to_string()
}

pub fn normalize_apple_terminal_input(data: &str, is_apple_terminal: bool, is_shift_pressed: bool) -> String {
    normalize_native_shift_enter_input(data, is_apple_terminal, is_shift_pressed)
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

    /// Synchronously restore terminal modes from a process exit handler when `stop()` did not run.
    /// Writes `before`, then this terminal's own mode resets, then `after`.
    fn restore_on_exit(&self, _before: &str, _after: &str) {}

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

    /// Report what the program is doing (OSC 7501). Sent only to terminals that support it; the latest
    /// status is re-sent when support is confirmed or the terminal restarts.
    fn set_program_status(&self, _status: ProgramStatus) {}
}

/// Resolve how long to wait for the rest of an escape sequence before
/// dispatching a lone ESC as the Escape key. Legacy Alt+key input is ESC plus
/// another byte, so high-latency transports need a longer reassembly window.
///
/// PORT: `None` selects `process.env` (`pi_js::env::vars()`), the TS default.
pub fn resolve_escape_timeout_ms(env_map: Option<&IndexMap<String, String>>) -> f64 {
    let get = |key: &str| -> Option<String> {
        match env_map {
            Some(m) => m.get(key).cloned(),
            None => env::var(key),
        }
    };
    if let Some(raw) = get("PI_TUI_ESC_TIMEOUT") {
        let configured = num::number(&raw);
        if configured.is_finite() && configured > 0.0 {
            return configured;
        }
    }
    if get("SSH_CONNECTION").is_some_and(|v| !v.is_empty()) || get("SSH_TTY").is_some_and(|v| !v.is_empty()) {
        return DEFAULT_SSH_ESCAPE_TIMEOUT_MS as f64;
    }
    DEFAULT_ESCAPE_TIMEOUT_MS as f64
}

fn write_stdout(data: &str) {
    if let Some(f) = testing::OVERRIDE_STDOUT_WRITE.with(|s| s.get()) {
        f(data);
        return;
    }
    pi_js::console::stdout_write(data);
}

fn parse_positive_dim_env(name: &str) -> Option<i64> {
    let raw = env::var(name)?;
    let parsed = num::number(&raw);
    if parsed.is_finite() && parsed != 0.0 {
        Some(parsed as i64)
    } else {
        None
    }
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
    keyboard_protocol_buffer_flush_timer: Option<Timeout>,
    stdin_buffer: Option<StdinBuffer>,
    stdin_data_handler: Option<TerminalInputHandler>,
    progress_interval: Option<Interval>,
    /// Latest program status, kept across stop() so start() can report it again.
    program_status: Option<ProgramStatus>,
    /// Whether the terminal confirmed OSC 7501 support since the last start(), or `PI_PROGRAM_STATUS=1`.
    program_status_supported: bool,
    /// The support query was sent and its DA1 sentinel has not arrived yet.
    program_status_query_pending: bool,
    session_id: u64,
    stdin_abort: Option<tokio::task::AbortHandle>,
    resize_abort: Option<tokio::task::AbortHandle>,
    drain_activity_ms: Option<Arc<AtomicI64>>,
    drain_epoch: Option<tokio::time::Instant>,
}

impl Default for ProcessTerminal {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessTerminal {
    pub fn new() -> Self {
        let write_log_path = {
            let env_path = env::var("PI_TUI_WRITE_LOG").unwrap_or_default();
            if env_path.is_empty() {
                String::new()
            } else if std::fs::metadata(&env_path).is_ok_and(|m| m.is_dir()) {
                let ts = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
                let filename = format!("tui-{ts}-{}.log", std::process::id());
                Path::new(&env_path).join(filename).to_string_lossy().into_owned()
            } else {
                env_path
            }
        };

        Self {
            inner: Arc::new(ProcessTerminalInner {
                write_log_path,
                state: Mutex::new(ProcessTerminalState {
                    was_raw: false,
                    input_handler: None,
                    resize_handler: None,
                    kitty_protocol_active: false,
                    modify_other_keys_active: false,
                    keyboard_protocol_pushed: false,
                    pending_keyboard_protocol_device_attributes: 0,
                    keyboard_protocol_negotiation_buffer: String::new(),
                    keyboard_protocol_buffer_flush_timer: None,
                    stdin_buffer: None,
                    stdin_data_handler: None,
                    progress_interval: None,
                    program_status: None,
                    program_status_supported: false,
                    program_status_query_pending: false,
                    session_id: 0,
                    stdin_abort: None,
                    resize_abort: None,
                    drain_activity_ms: None,
                    drain_epoch: None,
                }),
            }),
        }
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
        let escape_timeout = resolve_escape_timeout_ms(None).round() as i64;
        let stdin_buffer = StdinBuffer::new(Some(StdinBufferOptions {
            timeout: None,
            escape_timeout: Some(escape_timeout),
        }));

        let this = self.clone();
        stdin_buffer.on_data(Arc::new(move |sequence| {
            if is_program_status_reply(sequence) {
                let should_write = {
                    let mut state = this.inner.state.lock().unwrap();
                    if state.program_status_query_pending {
                        state.program_status_query_pending = false;
                        state.program_status_supported = true;
                        true
                    } else {
                        false
                    }
                };
                if should_write {
                    this.write_program_status();
                }
                return;
            }
            let negotiation = this.read_keyboard_protocol_negotiation_sequence(sequence);
            match negotiation {
                Some(KeyboardProtocolNegotiationRead::Pending) => {
                    this.schedule_keyboard_protocol_negotiation_buffer_flush();
                }
                Some(KeyboardProtocolNegotiationRead::Ready {
                    parsed,
                    sequence: full_seq,
                }) => {
                    if !this.handle_keyboard_protocol_negotiation_sequence(parsed) {
                        this.forward_input_sequence(&full_seq);
                    }
                }
                None => {
                    this.forward_input_sequence(sequence);
                }
            }
        }));

        let this = self.clone();
        stdin_buffer.on_paste(Arc::new(move |content| {
            let handler = this.inner.state.lock().unwrap().input_handler.clone();
            if let Some(handler) = handler {
                handler(&format!("\x1b[200~{content}\x1b[201~"));
            }
        }));

        let buf_for_handler = stdin_buffer.clone();
        let this = self.clone();
        let data_handler: TerminalInputHandler = Arc::new(move |data: &str| {
            {
                let state = this.inner.state.lock().unwrap();
                if let (Some(activity), Some(epoch)) = (&state.drain_activity_ms, state.drain_epoch) {
                    let elapsed = tokio::time::Instant::now().saturating_duration_since(epoch).as_millis() as i64;
                    activity.store(elapsed, Ordering::SeqCst);
                }
            }
            buf_for_handler.process(StdinBufferData::Text(data));
        });

        let old_buf = {
            let mut state = self.inner.state.lock().unwrap();
            let old = state.stdin_buffer.take();
            state.stdin_buffer = Some(stdin_buffer);
            state.stdin_data_handler = Some(data_handler);
            old
        };
        if let Some(old) = old_buf {
            old.destroy();
        }
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
    ///
    /// The OSC 7501 program status query shares the DA sentinel: a terminal that supports it replies
    /// before DA. `PI_PROGRAM_STATUS=1` or `0` skips the query.
    fn query_and_enable_kitty_protocol(&self) {
        self.setup_stdin_buffer();
        let program_status_override = env::var("PI_PROGRAM_STATUS");
        let supported = program_status_override.as_deref() == Some("1");
        let query_pending =
            program_status_override.as_deref() != Some("1") && program_status_override.as_deref() != Some("0");
        {
            let mut state = self.inner.state.lock().unwrap();
            state.keyboard_protocol_pushed = true;
            state.pending_keyboard_protocol_device_attributes += 1;
            state.program_status_supported = supported;
            state.program_status_query_pending = query_pending;
        }
        self.clear_keyboard_protocol_negotiation_buffer();
        let program_status_query = if query_pending { PROGRAM_STATUS_QUERY } else { "" };
        write_stdout(&format!(
            "{KITTY_KEYBOARD_PROTOCOL_QUERY}{program_status_query}{DEVICE_ATTRIBUTES_QUERY}"
        ));
        self.write_program_status();
    }

    fn handle_keyboard_protocol_negotiation_sequence(
        &self,
        negotiation_sequence: KeyboardProtocolNegotiationSequence,
    ) -> bool {
        self.clear_keyboard_protocol_negotiation_buffer();
        match negotiation_sequence {
            KeyboardProtocolNegotiationSequence::DeviceAttributes => {
                let is_active = {
                    let mut state = self.inner.state.lock().unwrap();
                    if state.pending_keyboard_protocol_device_attributes == 0 {
                        return false;
                    }
                    state.pending_keyboard_protocol_device_attributes -= 1;
                    // The last owed DA answers the latest query, which got no program status reply first. Earlier DA
                    // replies belong to queries from before a restart.
                    if state.pending_keyboard_protocol_device_attributes == 0 {
                        state.program_status_query_pending = false;
                    }
                    state.kitty_protocol_active
                };
                if !is_active {
                    self.enable_modify_other_keys();
                }
                true
            }
            KeyboardProtocolNegotiationSequence::KittyFlags { flags } => {
                if flags != 0 {
                    self.disable_modify_other_keys();
                    let became_active = {
                        let mut state = self.inner.state.lock().unwrap();
                        if !state.kitty_protocol_active {
                            state.kitty_protocol_active = true;
                            true
                        } else {
                            false
                        }
                    };
                    if became_active {
                        set_kitty_protocol_active(true);
                    }
                } else {
                    self.enable_modify_other_keys();
                }
                true
            }
        }
    }

    /// Returns the parsed negotiation reply with its full (possibly reassembled) sequence.
    fn read_keyboard_protocol_negotiation_sequence(&self, sequence: &str) -> Option<KeyboardProtocolNegotiationRead> {
        let existing_buf = {
            let state = self.inner.state.lock().unwrap();
            if state.keyboard_protocol_negotiation_buffer.is_empty() {
                None
            } else {
                Some(state.keyboard_protocol_negotiation_buffer.clone())
            }
        };

        if let Some(buf) = existing_buf {
            let buffered_sequence = format!("{buf}{sequence}");
            if let Some(parsed) = parse_keyboard_protocol_negotiation_sequence(&buffered_sequence) {
                self.clear_keyboard_protocol_negotiation_buffer();
                return Some(KeyboardProtocolNegotiationRead::Ready {
                    parsed,
                    sequence: buffered_sequence,
                });
            }
            if is_keyboard_protocol_negotiation_sequence_prefix(&buffered_sequence) {
                self.set_keyboard_protocol_negotiation_buffer(&buffered_sequence);
                return Some(KeyboardProtocolNegotiationRead::Pending);
            }
            self.flush_keyboard_protocol_negotiation_buffer_as_input();
        }

        if let Some(parsed) = parse_keyboard_protocol_negotiation_sequence(sequence) {
            return Some(KeyboardProtocolNegotiationRead::Ready {
                parsed,
                sequence: sequence.to_string(),
            });
        }
        if is_keyboard_protocol_negotiation_sequence_prefix(sequence) {
            self.set_keyboard_protocol_negotiation_buffer(sequence);
            return Some(KeyboardProtocolNegotiationRead::Pending);
        }
        None
    }

    fn set_keyboard_protocol_negotiation_buffer(&self, sequence: &str) {
        self.clear_keyboard_protocol_negotiation_buffer_flush_timer();
        let mut state = self.inner.state.lock().unwrap();
        state.keyboard_protocol_negotiation_buffer = sequence.to_string();
    }

    fn clear_keyboard_protocol_negotiation_buffer(&self) {
        self.clear_keyboard_protocol_negotiation_buffer_flush_timer();
        let mut state = self.inner.state.lock().unwrap();
        state.keyboard_protocol_negotiation_buffer.clear();
    }

    fn flush_keyboard_protocol_negotiation_buffer_as_input(&self) {
        let sequence = {
            let state = self.inner.state.lock().unwrap();
            if state.keyboard_protocol_negotiation_buffer.is_empty() {
                return;
            }
            state.keyboard_protocol_negotiation_buffer.clone()
        };
        self.clear_keyboard_protocol_negotiation_buffer();
        self.forward_input_sequence(&sequence);
    }

    fn schedule_keyboard_protocol_negotiation_buffer_flush(&self) {
        let mut state = self.inner.state.lock().unwrap();
        if state.keyboard_protocol_negotiation_buffer.is_empty() || state.keyboard_protocol_buffer_flush_timer.is_some()
        {
            return;
        }
        let this = self.clone();
        let delay = u64::try_from(KEYBOARD_PROTOCOL_RESPONSE_FRAGMENT_TIMEOUT_MS.max(0)).unwrap_or(0);
        state.keyboard_protocol_buffer_flush_timer = Some(time::set_timeout(delay, move || {
            {
                let mut st = this.inner.state.lock().unwrap();
                st.keyboard_protocol_buffer_flush_timer = None;
            }
            this.flush_keyboard_protocol_negotiation_buffer_as_input();
        }));
    }

    fn clear_keyboard_protocol_negotiation_buffer_flush_timer(&self) {
        let timer = {
            let mut state = self.inner.state.lock().unwrap();
            state.keyboard_protocol_buffer_flush_timer.take()
        };
        if let Some(timer) = timer {
            timer.clear();
        }
    }

    fn forward_input_sequence(&self, sequence: &str) {
        let handler = self.inner.state.lock().unwrap().input_handler.clone();
        let Some(handler) = handler else {
            return;
        };
        let should_detect_native_shift_enter =
            is_native_shift_enter_candidate(sequence) && (is_apple_terminal_session() || env::platform() == "win32");
        let is_shift = should_detect_native_shift_enter && is_native_modifier_pressed(ModifierKey::Shift);
        let input = normalize_native_shift_enter_input(sequence, should_detect_native_shift_enter, is_shift);
        handler(&input);
    }

    fn enable_modify_other_keys(&self) {
        {
            let mut state = self.inner.state.lock().unwrap();
            if state.kitty_protocol_active || state.modify_other_keys_active {
                return;
            }
            state.modify_other_keys_active = true;
        }
        write_stdout("\x1b[>4;2m");
    }

    fn disable_modify_other_keys(&self) {
        {
            let mut state = self.inner.state.lock().unwrap();
            if !state.modify_other_keys_active {
                return;
            }
            state.modify_other_keys_active = false;
        }
        write_stdout("\x1b[>4;0m");
    }

    /// On Windows, add ENABLE_VIRTUAL_TERMINAL_INPUT (0x0200) to the stdin
    /// console handle so the terminal sends VT sequences for modified keys
    /// (e.g. \x1b[Z for Shift+Tab). Without this, libuv's ReadConsoleInputW
    /// discards modifier state and Shift+Tab arrives as plain \t.
    fn enable_windows_vt_input(&self) {
        if env::platform() != "win32" {
            return;
        }
        if let Some(helper) = get_native_platform_helper() {
            let _ = helper.enable_virtual_terminal_input();
        }
    }

    fn clear_progress_interval(&self) -> bool {
        let interval = {
            let mut state = self.inner.state.lock().unwrap();
            state.progress_interval.take()
        };
        if let Some(interval) = interval {
            interval.clear();
            true
        } else {
            false
        }
    }

    fn write_program_status(&self) {
        let status = {
            let state = self.inner.state.lock().unwrap();
            if state.program_status_supported {
                state.program_status.clone()
            } else {
                None
            }
        };
        if let Some(status) = status {
            write_stdout(&format_program_status(&status));
        }
    }
}

#[async_trait]
impl Terminal for ProcessTerminal {
    fn start(&self, on_input: TerminalInputHandler, on_resize: TerminalResizeHandler) {
        let sid = {
            let mut state = self.inner.state.lock().unwrap();
            state.session_id += 1;
            if let Some(handle) = state.stdin_abort.take() {
                handle.abort();
            }
            if let Some(handle) = state.resize_abort.take() {
                handle.abort();
            }
            state.input_handler = Some(on_input);
            state.resize_handler = Some(on_resize);
            state.was_raw = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
            state.session_id
        };

        let _ = crossterm::terminal::enable_raw_mode();

        // Enable bracketed paste mode - terminal will wrap pastes in \x1b[200~ ... \x1b[201~
        write_stdout("\x1b[?2004h");

        // Refresh terminal dimensions - they may be stale after suspend/resume.
        refresh_terminal_dimensions();

        // Enable Windows VT input after enabling raw mode.
        self.enable_windows_vt_input();

        // Query Kitty keyboard protocol and set up StdinBuffer before spawning stdin task.
        self.query_and_enable_kitty_protocol();

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let this_in = self.clone();
            let stdin_task = handle.spawn(async move {
                let mut stdin = tokio::io::stdin();
                let mut buf = [0u8; 4096];
                let mut pending = Vec::<u8>::new();
                loop {
                    match stdin.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let handler = {
                                let state = this_in.inner.state.lock().unwrap();
                                if state.session_id != sid {
                                    break;
                                }
                                state.stdin_data_handler.clone()
                            };
                            let Some(handler) = handler else {
                                break;
                            };
                            pending.extend_from_slice(&buf[..n]);
                            let mut text = String::new();
                            loop {
                                match std::str::from_utf8(&pending) {
                                    Ok(valid) => {
                                        text.push_str(valid);
                                        pending.clear();
                                        break;
                                    }
                                    Err(err) => {
                                        let valid_up_to = err.valid_up_to();
                                        if valid_up_to > 0 {
                                            if let Ok(valid) = std::str::from_utf8(&pending[..valid_up_to]) {
                                                text.push_str(valid);
                                            }
                                            pending.drain(..valid_up_to);
                                        }
                                        if let Some(error_len) = err.error_len() {
                                            text.push('\u{FFFD}');
                                            pending.drain(..error_len);
                                        } else {
                                            break;
                                        }
                                    }
                                }
                            }
                            if !text.is_empty() {
                                handler(&text);
                            }
                        }
                    }
                }
            });

            let mut resize_abort = None;
            #[cfg(unix)]
            {
                if let Ok(mut sigwinch) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
                {
                    let this_res = self.clone();
                    let resize_task = handle.spawn(async move {
                        while sigwinch.recv().await.is_some() {
                            let handler = {
                                let state = this_res.inner.state.lock().unwrap();
                                if state.session_id != sid {
                                    break;
                                }
                                state.resize_handler.clone()
                            };
                            if let Some(handler) = handler {
                                handler();
                            }
                        }
                    });
                    resize_abort = Some(resize_task.abort_handle());
                }
            }

            let mut state = self.inner.state.lock().unwrap();
            if state.session_id == sid {
                state.stdin_abort = Some(stdin_task.abort_handle());
                state.resize_abort = resize_abort;
            } else {
                stdin_task.abort();
                if let Some(ra) = resize_abort {
                    ra.abort();
                }
            }
        }
    }

    fn stop(&self) {
        if self.clear_progress_interval() {
            write_stdout(TERMINAL_PROGRESS_CLEAR_SEQUENCE);
        }
        // Remove the status while stopped, for example after exit or while suspended. start() reports it again.
        let clear_status = {
            let mut state = self.inner.state.lock().unwrap();
            let should_clear = state.program_status_supported && state.program_status.is_some();
            state.program_status_supported = false;
            state.program_status_query_pending = false;
            should_clear
        };
        if clear_status {
            write_stdout(&format_program_status(&ProgramStatus {
                state: ProgramStatusState::Clear,
                ..Default::default()
            }));
        }

        // Disable bracketed paste mode
        write_stdout("\x1b[?2004l");

        let should_disable_kitty = {
            let state = self.inner.state.lock().unwrap();
            state.keyboard_protocol_pushed || state.kitty_protocol_active
        };
        self.clear_keyboard_protocol_negotiation_buffer();

        if should_disable_kitty {
            write_stdout("\x1b[<u");
            {
                let mut state = self.inner.state.lock().unwrap();
                state.keyboard_protocol_pushed = false;
                state.kitty_protocol_active = false;
            }
            set_kitty_protocol_active(false);
        }
        self.disable_modify_other_keys();

        let (stdin_buffer, stdin_abort, resize_abort, was_raw) = {
            let mut state = self.inner.state.lock().unwrap();
            state.session_id += 1;
            state.stdin_data_handler = None;
            state.input_handler = None;
            state.resize_handler = None;
            (
                state.stdin_buffer.take(),
                state.stdin_abort.take(),
                state.resize_abort.take(),
                state.was_raw,
            )
        };

        if let Some(buf) = stdin_buffer {
            buf.destroy();
        }
        if let Some(handle) = stdin_abort {
            handle.abort();
        }
        if let Some(handle) = resize_abort {
            handle.abort();
        }

        if !was_raw {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }

    fn restore_on_exit(&self, before: &str, after: &str) {
        let (has_progress, kitty_reset, modify_reset) = {
            let state = self.inner.state.lock().unwrap();
            (
                state.progress_interval.is_some(),
                state.keyboard_protocol_pushed || state.kitty_protocol_active,
                state.modify_other_keys_active,
            )
        };
        let mut sequence = String::from(before);
        if has_progress {
            sequence.push_str(TERMINAL_PROGRESS_CLEAR_SEQUENCE);
        }
        sequence.push_str("\x1b[?2004l");
        if kitty_reset {
            sequence.push_str("\x1b[<u");
        }
        if modify_reset {
            sequence.push_str("\x1b[>4;0m");
        }
        sequence.push_str(after);

        if let Some(f) = testing::OVERRIDE_STDOUT_WRITE.try_with(|s| s.get()).ok().flatten() {
            f(&sequence);
            return;
        }
        let mut stdout = std::io::stdout().lock();
        let _ = std::io::Write::write_all(&mut stdout, sequence.as_bytes());
        let _ = std::io::Write::flush(&mut stdout);
    }

    async fn drain_input(&self, max_ms: Option<i64>, idle_ms: Option<i64>) {
        let max_ms = max_ms.unwrap_or(1000).max(0) as u64;
        let idle_ms = idle_ms.unwrap_or(50).max(0) as u64;

        let should_disable_kitty = {
            let state = self.inner.state.lock().unwrap();
            state.keyboard_protocol_pushed || state.kitty_protocol_active
        };
        self.clear_keyboard_protocol_negotiation_buffer();
        if should_disable_kitty {
            write_stdout("\x1b[<u");
            {
                let mut state = self.inner.state.lock().unwrap();
                state.keyboard_protocol_pushed = false;
                state.kitty_protocol_active = false;
            }
            set_kitty_protocol_active(false);
        }
        self.disable_modify_other_keys();

        let epoch = tokio::time::Instant::now();
        let activity = Arc::new(AtomicI64::new(0));
        let previous_handler = {
            let mut state = self.inner.state.lock().unwrap();
            state.drain_activity_ms = Some(Arc::clone(&activity));
            state.drain_epoch = Some(epoch);
            state.input_handler.take()
        };

        let end_time = epoch + tokio::time::Duration::from_millis(max_ms);
        loop {
            let now = tokio::time::Instant::now();
            if now >= end_time {
                break;
            }
            let elapsed_ms = now.saturating_duration_since(epoch).as_millis() as i64;
            let last_ms = activity.load(Ordering::SeqCst);
            if elapsed_ms - last_ms >= idle_ms as i64 {
                break;
            }
            let time_left = end_time.saturating_duration_since(now).as_millis() as u64;
            let wait_ms = idle_ms.min(time_left).max(1);
            tokio::time::sleep(tokio::time::Duration::from_millis(wait_ms)).await;
        }

        let mut state = self.inner.state.lock().unwrap();
        state.drain_activity_ms = None;
        state.drain_epoch = None;
        state.input_handler = previous_handler;
    }

    fn write(&self, data: &str) {
        write_stdout(data);
        if !self.inner.write_log_path.is_empty() {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.inner.write_log_path)
            {
                let _ = std::io::Write::write_all(&mut file, data.as_bytes());
            }
        }
    }

    fn columns(&self) -> i64 {
        if let Some(f) = testing::OVERRIDE_STDOUT_SIZE.with(|s| s.get()) {
            if let Some((cols, _)) = f() {
                if cols > 0 {
                    return cols as i64;
                }
            }
        } else if std::io::stdout().is_terminal() {
            if let Ok((cols, _)) = crossterm::terminal::size() {
                if cols > 0 {
                    return cols as i64;
                }
            }
        }
        parse_positive_dim_env("COLUMNS").unwrap_or(80)
    }

    fn rows(&self) -> i64 {
        if let Some(f) = testing::OVERRIDE_STDOUT_SIZE.with(|s| s.get()) {
            if let Some((_, rows)) = f() {
                if rows > 0 {
                    return rows as i64;
                }
            }
        } else if std::io::stdout().is_terminal() {
            if let Ok((_, rows)) = crossterm::terminal::size() {
                if rows > 0 {
                    return rows as i64;
                }
            }
        }
        parse_positive_dim_env("LINES").unwrap_or(24)
    }

    fn kitty_protocol_active(&self) -> bool {
        self.inner.state.lock().unwrap().kitty_protocol_active
    }

    fn move_by(&self, lines: i64) {
        if lines > 0 {
            write_stdout(&format!("\x1b[{lines}B"));
        } else if lines < 0 {
            write_stdout(&format!("\x1b[{}A", lines.unsigned_abs()));
        }
    }

    fn hide_cursor(&self) {
        write_stdout("\x1b[?25l");
    }

    fn show_cursor(&self) {
        write_stdout("\x1b[?25h");
    }

    fn clear_line(&self) {
        write_stdout("\x1b[K");
    }

    fn clear_from_cursor(&self) {
        write_stdout("\x1b[J");
    }

    fn clear_screen(&self) {
        write_stdout("\x1b[2J\x1b[H");
    }

    fn set_title(&self, title: &str) {
        // OSC 0;title BEL - set terminal window title. A BEL or ESC in the title would end the sequence early and
        // send the rest to the terminal as escape sequences (session names come from session files).
        write_stdout(&format!("\x1b]0;{}\x07", replace_control_characters(title)));
    }

    fn set_progress(&self, active: bool) {
        if active {
            write_stdout(TERMINAL_PROGRESS_ACTIVE_SEQUENCE);
            let mut state = self.inner.state.lock().unwrap();
            if state.progress_interval.is_none() {
                let period = u64::try_from(TERMINAL_PROGRESS_KEEPALIVE_MS.max(1)).unwrap_or(1000);
                state.progress_interval = Some(time::set_interval(period, || {
                    write_stdout(TERMINAL_PROGRESS_ACTIVE_SEQUENCE);
                }));
            }
        } else {
            self.clear_progress_interval();
            write_stdout(TERMINAL_PROGRESS_CLEAR_SEQUENCE);
        }
    }

    fn set_program_status(&self, status: ProgramStatus) {
        let should_write = {
            let mut state = self.inner.state.lock().unwrap();
            state.program_status = (status.state != ProgramStatusState::Clear).then_some(status.clone());
            state.program_status_supported
        };
        if should_write {
            write_stdout(&format_program_status(&status));
        }
    }
}

pub mod testing {
    use super::*;
    use pi_js::seam::{Guard, Slot};

    thread_local! {
        pub(super) static OVERRIDE_STDOUT_WRITE: Slot<dyn Fn(&str) + Send + Sync> = Slot::new();
        pub(super) static OVERRIDE_STDOUT_SIZE: Slot<dyn Fn() -> Option<(u16, u16)> + Send + Sync> = Slot::new();
        pub(super) static OVERRIDE_KILL: Slot<dyn Fn(u32, &str) -> std::result::Result<(), String> + Send + Sync> = Slot::new();
    }

    pub fn override_stdout_write(f: impl Fn(&str) + Send + Sync + 'static) -> Guard {
        OVERRIDE_STDOUT_WRITE.with(|s| s.set(Arc::new(f)))
    }

    pub fn override_stdout_size(f: impl Fn() -> Option<(u16, u16)> + Send + Sync + 'static) -> Guard {
        OVERRIDE_STDOUT_SIZE.with(|s| s.set(Arc::new(f)))
    }

    pub fn override_kill(f: impl Fn(u32, &str) -> std::result::Result<(), String> + Send + Sync + 'static) -> Guard {
        OVERRIDE_KILL.with(|s| s.set(Arc::new(f)))
    }

    pub fn start_negotiation_test(terminal: &ProcessTerminal, on_input: TerminalInputHandler) {
        {
            let mut state = terminal.inner.state.lock().unwrap();
            state.input_handler = Some(on_input);
        }
        terminal.query_and_enable_kitty_protocol();
    }

    pub fn feed_stdin_data(terminal: &ProcessTerminal, data: &str) {
        let handler = terminal.inner.state.lock().unwrap().stdin_data_handler.clone();
        if let Some(handler) = handler {
            handler(data);
        }
    }
}
