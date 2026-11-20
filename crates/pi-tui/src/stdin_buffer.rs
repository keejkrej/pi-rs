//! Port of packages/tui/src/stdin-buffer.ts
//!
//! StdinBuffer buffers input and emits complete sequences.
//!
//! This is necessary because stdin data events can arrive in partial chunks,
//! especially for escape sequences like mouse events. Without buffering,
//! partial sequences can be misinterpreted as regular keypresses.
//!
//! For example, the mouse SGR sequence `\x1b[<35;20;5m` might arrive as:
//! - Event 1: `\x1b`
//! - Event 2: `[<35`
//! - Event 3: `;20;5m`
//!
//! The buffer accumulates these until a complete sequence is detected.
//! Call the `process()` method to feed input data.
//!
//! Based on code from OpenTUI (<https://github.com/anomalyco/opentui>)
//! MIT License - Copyright (c) 2025 opentui

use std::sync::{Arc, LazyLock, Mutex};

use pi_js::Unsubscribe;
use pi_js::time::Timeout;
use regex::Regex;

const ESC: &str = "\x1b";
const DEFAULT_SEQUENCE_TIMEOUT_MS: i64 = 50;
const DEFAULT_ESCAPE_TIMEOUT_MS: i64 = 10;
const BRACKETED_PASTE_START: &str = "\x1b[200~";
const BRACKETED_PASTE_END: &str = "\x1b[201~";

// Color and program-status OSC replies, XTVersion, and Kitty graphics replies.
// Until their header is complete, the input may be batched legacy Alt keystrokes.
static TERMINAL_REPLY_HEADER_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b(?:\](?:1[01];|4;\d{1,3};|7501;\?)|P>\||_G(?:[a-z]=\d+,)*[a-z]=\d+;)").unwrap());

fn is_control_string(data: &str) -> bool {
    data.starts_with("\x1b]") || data.starts_with("\x1bP") || data.starts_with("\x1b_")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SequenceStatus {
    Complete,
    Incomplete,
    NotEscape,
}

struct ExtractedSequences {
    sequences: Vec<String>,
    remainder: String,
}

static SGR_MOUSE_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^<\d+;\d+;\d+[Mm]$").unwrap());
static KITTY_PRINTABLE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b\[(\d+)(?::\d*)?(?::\d+)?u$").unwrap());

/// Check if a string is a complete escape sequence or needs more data.
fn is_complete_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with(ESC) {
        return SequenceStatus::NotEscape;
    }

    if data.len() == 1 {
        return SequenceStatus::Incomplete;
    }

    let after_esc = &data[1..];

    // CSI sequences: ESC [
    if after_esc.starts_with('[') {
        // Old-style mouse needs ESC[M + 3 bytes = 6 total
        if after_esc.starts_with("[M") {
            return if data.chars().count() >= 6 {
                SequenceStatus::Complete
            } else {
                SequenceStatus::Incomplete
            };
        }
        return is_complete_csi_sequence(data);
    }

    // OSC sequences: ESC ]
    if after_esc.starts_with(']') {
        return is_complete_osc_sequence(data);
    }

    // DCS sequences: ESC P ... ESC \ (includes XTVersion responses)
    if after_esc.starts_with('P') {
        return is_complete_dcs_sequence(data);
    }

    // APC sequences: ESC _ ... ESC \ (includes Kitty graphics responses)
    if after_esc.starts_with('_') {
        return is_complete_apc_sequence(data);
    }

    // SS3 sequences: ESC O
    if after_esc.starts_with('O') {
        return if after_esc.chars().count() >= 2 {
            SequenceStatus::Complete
        } else {
            SequenceStatus::Incomplete
        };
    }

    // Meta key sequences / unknown escape sequences
    SequenceStatus::Complete
}

/// Check if CSI sequence is complete.
/// CSI sequences: ESC [ ... followed by a final byte (0x40-0x7E).
fn is_complete_csi_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\x1b[") {
        return SequenceStatus::Complete;
    }

    // Need at least ESC [ and one more character
    if data.chars().count() < 3 {
        return SequenceStatus::Incomplete;
    }

    let payload = &data[2..];
    let Some(last_char) = payload.chars().next_back() else {
        return SequenceStatus::Incomplete;
    };
    let last_char_code = last_char as u32;

    if (0x40..=0x7e).contains(&last_char_code) {
        // Special handling for SGR mouse sequences: ESC[<B;X;Ym or ESC[<B;X;YM
        if payload.starts_with('<') {
            if SGR_MOUSE_REGEX.is_match(payload) {
                return SequenceStatus::Complete;
            }
            if last_char == 'M' || last_char == 'm' {
                let inner = &payload[1..payload.len() - 1];
                let parts: Vec<&str> = inner.split(';').collect();
                if parts.len() == 3
                    && parts
                        .iter()
                        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                {
                    return SequenceStatus::Complete;
                }
            }
            return SequenceStatus::Incomplete;
        }
        return SequenceStatus::Complete;
    }

    SequenceStatus::Incomplete
}

/// Check if OSC sequence is complete.
/// OSC sequences: ESC ] ... ST (where ST is ESC \ or BEL).
fn is_complete_osc_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\x1b]") {
        return SequenceStatus::Complete;
    }
    if data.ends_with("\x1b\\") || data.ends_with('\x07') {
        SequenceStatus::Complete
    } else {
        SequenceStatus::Incomplete
    }
}

/// Check if DCS (Device Control String) sequence is complete.
/// DCS sequences: ESC P ... ST (where ST is ESC \).
/// Used for XTVersion responses like ESC P >| ... ESC \.
fn is_complete_dcs_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\x1bP") {
        return SequenceStatus::Complete;
    }
    if data.ends_with("\x1b\\") {
        SequenceStatus::Complete
    } else {
        SequenceStatus::Incomplete
    }
}

/// Check if APC (Application Program Command) sequence is complete.
/// APC sequences: ESC _ ... ST (where ST is ESC \).
/// Used for Kitty graphics responses like ESC _ G ... ESC \.
fn is_complete_apc_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\x1b_") {
        return SequenceStatus::Complete;
    }
    if data.ends_with("\x1b\\") {
        SequenceStatus::Complete
    } else {
        SequenceStatus::Incomplete
    }
}

fn parse_unmodified_kitty_printable_codepoint(sequence: &str) -> Option<i64> {
    let caps = KITTY_PRINTABLE_REGEX.captures(sequence)?;
    let codepoint = caps.get(1)?.as_str().parse::<i64>().ok()?;
    if codepoint >= 32 { Some(codepoint) } else { None }
}

/// Split accumulated buffer into complete sequences.
fn extract_complete_sequences(buffer: &str) -> ExtractedSequences {
    let mut sequences = Vec::new();
    let mut pos = 0;

    while pos < buffer.len() {
        let remaining = &buffer[pos..];

        if remaining.starts_with(ESC) {
            let terminal_reply = is_control_string(remaining) && TERMINAL_REPLY_HEADER_PATTERN.is_match(remaining);
            let mut matched = false;
            for (idx, ch) in remaining.char_indices() {
                let seq_end = idx + ch.len_utf8();
                if terminal_reply && seq_end > 2 {
                    if ch == '\x18' || ch == '\x1a' {
                        // CAN and SUB cancel the string, not subsequent keyboard input.
                        pos += seq_end;
                        matched = true;
                        break;
                    }
                    if ch == '\x1b' && seq_end < remaining.len() && !remaining[seq_end..].starts_with('\\') {
                        // A new escape sequence aborts an unterminated reply. Keep a
                        // trailing ESC buffered until we can distinguish it from ST.
                        pos += idx;
                        matched = true;
                        break;
                    }
                }
                let candidate = &remaining[..seq_end];
                match is_complete_sequence(candidate) {
                    SequenceStatus::Complete => {
                        // WezTerm with enable_kitty_keyboard can concatenate raw '\x1b' (press)
                        // with a full CSI/OSC/SS3/DCS/APC sequence (release). Split before the
                        // second ESC when followed by a sequence introducer.
                        if candidate == "\x1b\x1b" {
                            if let Some(next_char) = remaining[seq_end..].chars().next() {
                                if matches!(next_char, '[' | ']' | 'O' | 'P' | '_') {
                                    sequences.push(ESC.to_string());
                                    pos += 1;
                                    matched = true;
                                    break;
                                }
                            }
                        }
                        sequences.push(candidate.to_string());
                        pos += seq_end;
                        matched = true;
                        break;
                    }
                    SequenceStatus::Incomplete => {}
                    SequenceStatus::NotEscape => {
                        sequences.push(candidate.to_string());
                        pos += seq_end;
                        matched = true;
                        break;
                    }
                }
            }

            if !matched {
                return ExtractedSequences {
                    sequences,
                    remainder: remaining.to_string(),
                };
            }
        } else if let Some(ch) = remaining.chars().next() {
            sequences.push(ch.to_string());
            pos += ch.len_utf8();
        } else {
            break;
        }
    }

    ExtractedSequences {
        sequences,
        remainder: String::new(),
    }
}

/// Maximum time to wait for an incomplete sequence such as CSI or mouse (default: 50ms),
/// and maximum time to wait after a lone ESC before treating it as Escape (default: 10ms).
/// Increase `escape_timeout` for high-latency Alt+key input (SSH).
///
/// `None` on either field means the default. Omitted options are [`StdinBufferOptions::default`].
#[derive(Clone, Debug, Default)]
pub struct StdinBufferOptions {
    pub timeout: Option<i64>,
    pub escape_timeout: Option<i64>,
}

/// `string | Buffer` passed to [`StdinBuffer::process`].
///
/// PORT: a Node `Buffer` argument is borrowed bytes. The single-byte `> 127` conversion
/// stays inside `process`.
pub enum StdinBufferData<'a> {
    Text(&'a str),
    Buffer(&'a [u8]),
}

/// Listener argument lists: `data: [string]`, `paste: [string]`.
///
/// PORT: Node `EventEmitter` `on("data" | "paste", cb)` is [`StdinBuffer::on_data`] and
/// [`StdinBuffer::on_paste`]. Each returns [`Unsubscribe`] (drop does not remove the listener).
/// The rest of the EventEmitter surface (`once`, `off`, `emit`) is not declared; `destroy`
/// drops listeners with the buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StdinBufferEventMap {
    Data(String),
    Paste(String),
}

pub type StdinBufferListener = Arc<dyn Fn(&str) + Send + Sync>;

struct StdinBufferState {
    buffer: String,
    timeout: Option<Timeout>,
    paste_mode: bool,
    paste_buffer: String,
    pending_kitty_printable_codepoint: Option<i64>,
    next_listener_id: u64,
    data_listeners: Vec<(u64, StdinBufferListener)>,
    paste_listeners: Vec<(u64, StdinBufferListener)>,
}

struct StdinBufferInner {
    timeout_ms: i64,
    escape_timeout_ms: i64,
    state: Mutex<StdinBufferState>,
}

/// Buffers stdin input and emits complete sequences via the 'data' event.
/// Handles partial escape sequences that arrive across multiple chunks.
#[derive(Clone)]
pub struct StdinBuffer {
    inner: Arc<StdinBufferInner>,
}

impl StdinBuffer {
    /// `options` `None` is the TS default `{}`. Numeric defaults are applied inside.
    pub fn new(options: Option<StdinBufferOptions>) -> Self {
        let opts = options.unwrap_or_default();
        Self {
            inner: Arc::new(StdinBufferInner {
                timeout_ms: opts.timeout.unwrap_or(DEFAULT_SEQUENCE_TIMEOUT_MS),
                escape_timeout_ms: opts.escape_timeout.unwrap_or(DEFAULT_ESCAPE_TIMEOUT_MS),
                state: Mutex::new(StdinBufferState {
                    buffer: String::new(),
                    timeout: None,
                    paste_mode: false,
                    paste_buffer: String::new(),
                    pending_kitty_printable_codepoint: None,
                    next_listener_id: 0,
                    data_listeners: Vec::new(),
                    paste_listeners: Vec::new(),
                }),
            }),
        }
    }

    pub fn on_data(&self, listener: StdinBufferListener) -> Unsubscribe {
        let id = {
            let mut state = self.inner.state.lock().unwrap();
            let id = state.next_listener_id;
            state.next_listener_id += 1;
            state.data_listeners.push((id, listener));
            id
        };
        let weak = Arc::downgrade(&self.inner);
        Unsubscribe::new(move || {
            if let Some(inner) = weak.upgrade() {
                let mut state = inner.state.lock().unwrap();
                state.data_listeners.retain(|(entry_id, _)| *entry_id != id);
            }
        })
    }

    pub fn on_paste(&self, listener: StdinBufferListener) -> Unsubscribe {
        let id = {
            let mut state = self.inner.state.lock().unwrap();
            let id = state.next_listener_id;
            state.next_listener_id += 1;
            state.paste_listeners.push((id, listener));
            id
        };
        let weak = Arc::downgrade(&self.inner);
        Unsubscribe::new(move || {
            if let Some(inner) = weak.upgrade() {
                let mut state = inner.state.lock().unwrap();
                state.paste_listeners.retain(|(entry_id, _)| *entry_id != id);
            }
        })
    }

    fn emit_paste(&self, content: &str) {
        let listeners: Vec<StdinBufferListener> = {
            let state = self.inner.state.lock().unwrap();
            state.paste_listeners.iter().map(|(_, l)| Arc::clone(l)).collect()
        };
        for listener in listeners {
            listener(content);
        }
    }

    pub fn process(&self, data: StdinBufferData<'_>) {
        enum Action {
            Done,
            EmptyData,
            CompletePaste {
                before: Vec<String>,
                pasted: String,
                remaining: String,
            },
            EnterPaste {
                before: Vec<String>,
            },
            Sequences {
                sequences: Vec<String>,
                remaining_len_and_timeout: Option<u64>,
            },
        }

        let str_data = match data {
            StdinBufferData::Text(s) => s.to_string(),
            StdinBufferData::Buffer(bytes) => {
                if bytes.len() == 1 && bytes[0] > 127 {
                    let byte = bytes[0] - 128;
                    format!("\x1b{}", char::from(byte))
                } else {
                    String::from_utf8_lossy(bytes).into_owned()
                }
            }
        };

        let action = {
            let mut state = self.inner.state.lock().unwrap();
            if let Some(timeout) = state.timeout.take() {
                timeout.clear();
            }

            if str_data.is_empty() && state.buffer.is_empty() {
                Action::EmptyData
            } else {
                state.buffer.push_str(&str_data);

                if state.paste_mode {
                    let chunk = std::mem::take(&mut state.buffer);
                    state.paste_buffer.push_str(&chunk);

                    if let Some(end_index) = state.paste_buffer.find(BRACKETED_PASTE_END) {
                        let pasted = state.paste_buffer[..end_index].to_string();
                        let remaining = state.paste_buffer[end_index + BRACKETED_PASTE_END.len()..].to_string();
                        state.paste_mode = false;
                        state.paste_buffer.clear();
                        state.pending_kitty_printable_codepoint = None;
                        Action::CompletePaste {
                            before: Vec::new(),
                            pasted,
                            remaining,
                        }
                    } else {
                        Action::Done
                    }
                } else if let Some(start_index) = state.buffer.find(BRACKETED_PASTE_START) {
                    let before = if start_index > 0 {
                        let before_paste = &state.buffer[..start_index];
                        extract_complete_sequences(before_paste).sequences
                    } else {
                        Vec::new()
                    };

                    state.pending_kitty_printable_codepoint = None;
                    let after_start = state.buffer[start_index + BRACKETED_PASTE_START.len()..].to_string();
                    state.buffer.clear();
                    state.paste_mode = true;
                    state.paste_buffer = after_start;

                    if let Some(end_index) = state.paste_buffer.find(BRACKETED_PASTE_END) {
                        let pasted = state.paste_buffer[..end_index].to_string();
                        let remaining = state.paste_buffer[end_index + BRACKETED_PASTE_END.len()..].to_string();
                        state.paste_mode = false;
                        state.paste_buffer.clear();
                        state.pending_kitty_printable_codepoint = None;
                        Action::CompletePaste {
                            before,
                            pasted,
                            remaining,
                        }
                    } else {
                        Action::EnterPaste { before }
                    }
                } else {
                    let result = extract_complete_sequences(&state.buffer);
                    state.buffer = result.remainder;

                    // Only a recognizable reply header establishes a delimited string.
                    // Flushing it would turn a delayed tail into text and an OSC's BEL
                    // terminator into Ctrl+G (the external editor shortcut). Other input,
                    // including Alt+] followed by text, must retain bounded recovery.
                    let remaining_timeout =
                        if !state.buffer.is_empty() && !TERMINAL_REPLY_HEADER_PATTERN.is_match(&state.buffer) {
                            let timeout_ms = if state.buffer == ESC {
                                self.inner.escape_timeout_ms
                            } else {
                                self.inner.timeout_ms
                            };
                            Some(u64::try_from(timeout_ms.max(0)).unwrap_or(0))
                        } else {
                            None
                        };

                    Action::Sequences {
                        sequences: result.sequences,
                        remaining_len_and_timeout: remaining_timeout,
                    }
                }
            }
        };

        match action {
            Action::Done => {}
            Action::EmptyData => {
                self.emit_data_sequence("");
            }
            Action::EnterPaste { before } => {
                if !before.is_empty() {
                    for sequence in before {
                        self.emit_data_sequence(&sequence);
                    }
                    self.inner.state.lock().unwrap().pending_kitty_printable_codepoint = None;
                }
            }
            Action::CompletePaste {
                before,
                pasted,
                remaining,
            } => {
                if !before.is_empty() {
                    for sequence in before {
                        self.emit_data_sequence(&sequence);
                    }
                    self.inner.state.lock().unwrap().pending_kitty_printable_codepoint = None;
                }
                self.emit_paste(&pasted);
                if !remaining.is_empty() {
                    self.process(StdinBufferData::Text(&remaining));
                }
            }
            Action::Sequences {
                sequences,
                remaining_len_and_timeout,
            } => {
                for sequence in sequences {
                    self.emit_data_sequence(&sequence);
                }
                if let Some(delay) = remaining_len_and_timeout {
                    let mut state = self.inner.state.lock().unwrap();
                    if !state.buffer.is_empty() && state.timeout.is_none() {
                        let buf_clone = self.clone();
                        state.timeout = Some(pi_js::time::set_timeout(delay, move || {
                            let flushed = buf_clone.flush();
                            for sequence in flushed {
                                if sequence.len() > 2 && is_control_string(&sequence) {
                                    // Recover the Alt key and reparse its tail so batched text,
                                    // Enter, and control keys are not swallowed as one event.
                                    buf_clone.emit_data_sequence(&sequence[..2]);
                                    buf_clone.process(StdinBufferData::Text(&sequence[2..]));
                                } else {
                                    buf_clone.emit_data_sequence(&sequence);
                                }
                            }
                        }));
                    }
                }
            }
        }
    }

    fn emit_data_sequence(&self, sequence: &str) {
        let listeners: Vec<StdinBufferListener> = {
            let mut state = self.inner.state.lock().unwrap();
            let mut chars = sequence.chars();
            let raw_codepoint = match (chars.next(), chars.next()) {
                (Some(ch), None) => Some(ch as i64),
                _ => None,
            };
            if raw_codepoint.is_some() && raw_codepoint == state.pending_kitty_printable_codepoint {
                state.pending_kitty_printable_codepoint = None;
                return;
            }
            state.pending_kitty_printable_codepoint = parse_unmodified_kitty_printable_codepoint(sequence);
            state.data_listeners.iter().map(|(_, l)| Arc::clone(l)).collect()
        };
        for listener in listeners {
            listener(sequence);
        }
    }

    pub fn flush(&self) -> Vec<String> {
        let mut state = self.inner.state.lock().unwrap();
        if let Some(timeout) = state.timeout.take() {
            timeout.clear();
        }
        if state.buffer.is_empty() {
            return Vec::new();
        }
        let seq = std::mem::take(&mut state.buffer);
        state.pending_kitty_printable_codepoint = None;
        vec![seq]
    }

    pub fn clear(&self) {
        let mut state = self.inner.state.lock().unwrap();
        if let Some(timeout) = state.timeout.take() {
            timeout.clear();
        }
        state.buffer.clear();
        state.paste_mode = false;
        state.paste_buffer.clear();
        state.pending_kitty_printable_codepoint = None;
    }

    pub fn get_buffer(&self) -> String {
        self.inner.state.lock().unwrap().buffer.clone()
    }

    pub fn destroy(&self) {
        self.clear();
    }
}
