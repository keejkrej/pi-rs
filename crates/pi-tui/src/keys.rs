//! Port of packages/tui/src/keys.ts
//!
//! Keyboard input handling for terminal applications.
//!
//! Supports both legacy terminal sequences and Kitty keyboard protocol.
//! See: <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>
//! Reference: <https://github.com/sst/opentui/blob/7da92b4088aebfe27b9f691c04163a48821e49fd/packages/core/src/lib/parse.keypress.ts>
//!
//! Symbol keys are also supported, however some ctrl+symbol combos
//! overlap with ASCII codes, e.g. ctrl+[ = ESC.
//! See: <https://sw.kovidgoyal.net/kitty/keyboard-protocol/#legacy-ctrl-mapping-of-ascii-keys>
//! Those can still be used for ctrl+shift combos.

#![allow(dead_code, unused_variables)]

use std::sync::{LazyLock, Mutex};

use indexmap::{IndexMap, IndexSet};
use regex::Regex;
use serde::{Deserialize, Serialize};

// =============================================================================
// Global Kitty Protocol State
// =============================================================================

static KITTY_PROTOCOL_ACTIVE: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

// Store the last parsed event type for isKeyRelease() to query.
static LAST_EVENT_TYPE: LazyLock<Mutex<KeyEventType>> = LazyLock::new(|| Mutex::new(KeyEventType::Press));

/// Set the global Kitty keyboard protocol state.
/// Called by ProcessTerminal after detecting protocol support.
pub fn set_kitty_protocol_active(active: bool) {
    *KITTY_PROTOCOL_ACTIVE.lock().unwrap() = active;
}

/// Query whether Kitty keyboard protocol is currently active.
pub fn is_kitty_protocol_active() -> bool {
    *KITTY_PROTOCOL_ACTIVE.lock().unwrap()
}

// =============================================================================
// Type-Safe Key Identifiers
// =============================================================================

/// Union of all valid key identifiers.
///
/// Provides autocomplete and catches typos at compile time.
///
/// PORT: TS `KeyId` is `BaseKey | ModifiedKeyId<BaseKey>`, a mapped string-literal
/// union of every modifier permutation. Order is significant (`"ctrl+shift+a"` is
/// not `"shift+ctrl+a"`), and the set is open, so this is a `String` rather than an enum.
/// Parameters take `&str`.
pub type KeyId = String;

/// Event types from Kitty keyboard protocol (flag 2).
/// 1 = key press, 2 = key repeat, 3 = key release.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyEventType {
    #[serde(rename = "press")]
    Press,
    #[serde(rename = "repeat")]
    Repeat,
    #[serde(rename = "release")]
    Release,
}

/// Helper object for creating typed key identifiers with autocomplete.
///
/// Usage:
/// - `Key::escape`, `Key::enter`, `Key::tab`, etc. for special keys
/// - `Key::backtick`, `Key::comma`, `Key::period`, etc. for symbol keys
/// - `Key::ctrl("c")`, `Key::alt("x")`, `Key::super_("k")` for single modifiers
/// - `Key::ctrl_shift("p")`, `Key::ctrl_alt("x")`, `Key::ctrl_super("k")` for combined modifiers
///
/// PORT: TS fields are camelCase properties. Associated consts use `snake` names
/// (`pageUp` → `page_up`); the string values are the TS key ids. `return` is `r#return`.
/// `super` is `super_` (keyword).
pub struct Key;

#[allow(non_upper_case_globals)]
impl Key {
    pub const escape: &'static str = "escape";
    pub const esc: &'static str = "esc";
    pub const enter: &'static str = "enter";
    pub const r#return: &'static str = "return";
    pub const tab: &'static str = "tab";
    pub const space: &'static str = "space";
    pub const backspace: &'static str = "backspace";
    pub const delete: &'static str = "delete";
    pub const insert: &'static str = "insert";
    pub const clear: &'static str = "clear";
    pub const home: &'static str = "home";
    pub const end: &'static str = "end";
    pub const page_up: &'static str = "pageUp";
    pub const page_down: &'static str = "pageDown";
    pub const up: &'static str = "up";
    pub const down: &'static str = "down";
    pub const left: &'static str = "left";
    pub const right: &'static str = "right";
    pub const f1: &'static str = "f1";
    pub const f2: &'static str = "f2";
    pub const f3: &'static str = "f3";
    pub const f4: &'static str = "f4";
    pub const f5: &'static str = "f5";
    pub const f6: &'static str = "f6";
    pub const f7: &'static str = "f7";
    pub const f8: &'static str = "f8";
    pub const f9: &'static str = "f9";
    pub const f10: &'static str = "f10";
    pub const f11: &'static str = "f11";
    pub const f12: &'static str = "f12";

    pub const backtick: &'static str = "`";
    pub const hyphen: &'static str = "-";
    pub const equals: &'static str = "=";
    pub const leftbracket: &'static str = "[";
    pub const rightbracket: &'static str = "]";
    pub const backslash: &'static str = "\\";
    pub const semicolon: &'static str = ";";
    pub const quote: &'static str = "'";
    pub const comma: &'static str = ",";
    pub const period: &'static str = ".";
    pub const slash: &'static str = "/";
    pub const exclamation: &'static str = "!";
    pub const at: &'static str = "@";
    pub const hash: &'static str = "#";
    pub const dollar: &'static str = "$";
    pub const percent: &'static str = "%";
    pub const caret: &'static str = "^";
    pub const ampersand: &'static str = "&";
    pub const asterisk: &'static str = "*";
    pub const leftparen: &'static str = "(";
    pub const rightparen: &'static str = ")";
    pub const underscore: &'static str = "_";
    pub const plus: &'static str = "+";
    pub const pipe: &'static str = "|";
    pub const tilde: &'static str = "~";
    pub const leftbrace: &'static str = "{";
    pub const rightbrace: &'static str = "}";
    pub const colon: &'static str = ":";
    pub const lessthan: &'static str = "<";
    pub const greaterthan: &'static str = ">";
    pub const question: &'static str = "?";

    pub fn ctrl(key: &str) -> KeyId {
        format!("ctrl+{key}")
    }
    pub fn shift(key: &str) -> KeyId {
        format!("shift+{key}")
    }
    pub fn alt(key: &str) -> KeyId {
        format!("alt+{key}")
    }
    pub fn super_(key: &str) -> KeyId {
        format!("super+{key}")
    }
    pub fn ctrl_shift(key: &str) -> KeyId {
        format!("ctrl+shift+{key}")
    }
    pub fn shift_ctrl(key: &str) -> KeyId {
        format!("shift+ctrl+{key}")
    }
    pub fn ctrl_alt(key: &str) -> KeyId {
        format!("ctrl+alt+{key}")
    }
    pub fn alt_ctrl(key: &str) -> KeyId {
        format!("alt+ctrl+{key}")
    }
    pub fn shift_alt(key: &str) -> KeyId {
        format!("shift+alt+{key}")
    }
    pub fn alt_shift(key: &str) -> KeyId {
        format!("alt+shift+{key}")
    }
    pub fn ctrl_super(key: &str) -> KeyId {
        format!("ctrl+super+{key}")
    }
    pub fn super_ctrl(key: &str) -> KeyId {
        format!("super+ctrl+{key}")
    }
    pub fn shift_super(key: &str) -> KeyId {
        format!("shift+super+{key}")
    }
    pub fn super_shift(key: &str) -> KeyId {
        format!("super+shift+{key}")
    }
    pub fn alt_super(key: &str) -> KeyId {
        format!("alt+super+{key}")
    }
    pub fn super_alt(key: &str) -> KeyId {
        format!("super+alt+{key}")
    }
    pub fn ctrl_shift_alt(key: &str) -> KeyId {
        format!("ctrl+shift+alt+{key}")
    }
    pub fn ctrl_shift_super(key: &str) -> KeyId {
        format!("ctrl+shift+super+{key}")
    }
}

// =============================================================================
// Constants
// =============================================================================

// PORT: JS `&` / `~` coerce to int32. Matcher bodies must use int32 bitwise (`pi_js::num::to_int32`).
#[derive(Clone, Copy)]
struct Modifiers {
    shift: i64,
    alt: i64,
    ctrl: i64,
    super_: i64,
}

const MODIFIERS: Modifiers = Modifiers {
    shift: 1,
    alt: 2,
    ctrl: 4,
    super_: 8,
};

const LOCK_MASK: i64 = 64 + 128; // Caps Lock + Num Lock

#[derive(Clone, Copy)]
struct Codepoints {
    escape: i64,
    tab: i64,
    enter: i64,
    space: i64,
    backspace: i64,
    kp_enter: i64,
}

const CODEPOINTS: Codepoints = Codepoints {
    escape: 27,
    tab: 9,
    enter: 13,
    space: 32,
    backspace: 127,
    kp_enter: 57414, // Numpad Enter (Kitty protocol)
};

#[derive(Clone, Copy)]
struct ArrowCodepoints {
    up: i64,
    down: i64,
    right: i64,
    left: i64,
}

const ARROW_CODEPOINTS: ArrowCodepoints = ArrowCodepoints {
    up: -1,
    down: -2,
    right: -3,
    left: -4,
};

#[derive(Clone, Copy)]
struct FunctionalCodepoints {
    delete: i64,
    insert: i64,
    page_up: i64,
    page_down: i64,
    home: i64,
    end: i64,
}

const FUNCTIONAL_CODEPOINTS: FunctionalCodepoints = FunctionalCodepoints {
    delete: -10,
    insert: -11,
    page_up: -12,
    page_down: -13,
    home: -14,
    end: -15,
};

static SYMBOL_KEYS: LazyLock<IndexSet<&'static str>> = LazyLock::new(|| {
    IndexSet::from([
        "`", "-", "=", "[", "]", "\\", ";", "'", ",", ".", "/", "!", "@", "#", "$", "%", "^", "&", "*", "(", ")", "_",
        "+", "|", "~", "{", "}", ":", "<", ">", "?",
    ])
});

static KITTY_FUNCTIONAL_KEY_EQUIVALENTS: LazyLock<IndexMap<i64, i64>> = LazyLock::new(|| {
    IndexMap::from([
        (57399, 48), // KP_0 -> 0
        (57400, 49), // KP_1 -> 1
        (57401, 50), // KP_2 -> 2
        (57402, 51), // KP_3 -> 3
        (57403, 52), // KP_4 -> 4
        (57404, 53), // KP_5 -> 5
        (57405, 54), // KP_6 -> 6
        (57406, 55), // KP_7 -> 7
        (57407, 56), // KP_8 -> 8
        (57408, 57), // KP_9 -> 9
        (57409, 46), // KP_DECIMAL -> .
        (57410, 47), // KP_DIVIDE -> /
        (57411, 42), // KP_MULTIPLY -> *
        (57412, 45), // KP_SUBTRACT -> -
        (57413, 43), // KP_ADD -> +
        (57415, 61), // KP_EQUAL -> =
        (57416, 44), // KP_SEPARATOR -> ,
        (57417, ARROW_CODEPOINTS.left),
        (57418, ARROW_CODEPOINTS.right),
        (57419, ARROW_CODEPOINTS.up),
        (57420, ARROW_CODEPOINTS.down),
        (57421, FUNCTIONAL_CODEPOINTS.page_up),
        (57422, FUNCTIONAL_CODEPOINTS.page_down),
        (57423, FUNCTIONAL_CODEPOINTS.home),
        (57424, FUNCTIONAL_CODEPOINTS.end),
        (57425, FUNCTIONAL_CODEPOINTS.insert),
        (57426, FUNCTIONAL_CODEPOINTS.delete),
    ])
});

#[derive(Clone, Copy)]
struct LegacyKeySequences {
    up: &'static [&'static str],
    down: &'static [&'static str],
    right: &'static [&'static str],
    left: &'static [&'static str],
    home: &'static [&'static str],
    end: &'static [&'static str],
    insert: &'static [&'static str],
    delete: &'static [&'static str],
    page_up: &'static [&'static str],
    page_down: &'static [&'static str],
    clear: &'static [&'static str],
    f1: &'static [&'static str],
    f2: &'static [&'static str],
    f3: &'static [&'static str],
    f4: &'static [&'static str],
    f5: &'static [&'static str],
    f6: &'static [&'static str],
    f7: &'static [&'static str],
    f8: &'static [&'static str],
    f9: &'static [&'static str],
    f10: &'static [&'static str],
    f11: &'static [&'static str],
    f12: &'static [&'static str],
}

const LEGACY_KEY_SEQUENCES: LegacyKeySequences = LegacyKeySequences {
    up: &["\x1b[A", "\x1bOA"],
    down: &["\x1b[B", "\x1bOB"],
    right: &["\x1b[C", "\x1bOC"],
    left: &["\x1b[D", "\x1bOD"],
    home: &["\x1b[H", "\x1bOH", "\x1b[1~", "\x1b[7~"],
    end: &["\x1b[F", "\x1bOF", "\x1b[4~", "\x1b[8~"],
    insert: &["\x1b[2~"],
    delete: &["\x1b[3~"],
    page_up: &["\x1b[5~", "\x1b[[5~"],
    page_down: &["\x1b[6~", "\x1b[[6~"],
    clear: &["\x1b[E", "\x1bOE"],
    f1: &["\x1bOP", "\x1b[11~", "\x1b[[A"],
    f2: &["\x1bOQ", "\x1b[12~", "\x1b[[B"],
    f3: &["\x1bOR", "\x1b[13~", "\x1b[[C"],
    f4: &["\x1bOS", "\x1b[14~", "\x1b[[D"],
    f5: &["\x1b[15~", "\x1b[[E"],
    f6: &["\x1b[17~"],
    f7: &["\x1b[18~"],
    f8: &["\x1b[19~"],
    f9: &["\x1b[20~"],
    f10: &["\x1b[21~"],
    f11: &["\x1b[23~"],
    f12: &["\x1b[24~"],
};

#[derive(Clone, Copy)]
struct LegacyModifierSequences {
    up: &'static [&'static str],
    down: &'static [&'static str],
    right: &'static [&'static str],
    left: &'static [&'static str],
    clear: &'static [&'static str],
    insert: &'static [&'static str],
    delete: &'static [&'static str],
    page_up: &'static [&'static str],
    page_down: &'static [&'static str],
    home: &'static [&'static str],
    end: &'static [&'static str],
}

const LEGACY_SHIFT_SEQUENCES: LegacyModifierSequences = LegacyModifierSequences {
    up: &["\x1b[a"],
    down: &["\x1b[b"],
    right: &["\x1b[c"],
    left: &["\x1b[d"],
    clear: &["\x1b[e"],
    insert: &["\x1b[2$"],
    delete: &["\x1b[3$"],
    page_up: &["\x1b[5$"],
    page_down: &["\x1b[6$"],
    home: &["\x1b[7$"],
    end: &["\x1b[8$"],
};

const LEGACY_CTRL_SEQUENCES: LegacyModifierSequences = LegacyModifierSequences {
    up: &["\x1bOa"],
    down: &["\x1bOb"],
    right: &["\x1bOc"],
    left: &["\x1bOd"],
    clear: &["\x1bOe"],
    insert: &["\x1b[2^"],
    delete: &["\x1b[3^"],
    page_up: &["\x1b[5^"],
    page_down: &["\x1b[6^"],
    home: &["\x1b[7^"],
    end: &["\x1b[8^"],
};

static LEGACY_SEQUENCE_KEY_IDS: LazyLock<IndexMap<&'static str, &'static str>> = LazyLock::new(|| {
    IndexMap::from([
        ("\x1bOA", "up"),
        ("\x1bOB", "down"),
        ("\x1bOC", "right"),
        ("\x1bOD", "left"),
        ("\x1bOH", "home"),
        ("\x1bOF", "end"),
        ("\x1b[E", "clear"),
        ("\x1bOE", "clear"),
        ("\x1bOe", "ctrl+clear"),
        ("\x1b[e", "shift+clear"),
        ("\x1b[2~", "insert"),
        ("\x1b[2$", "shift+insert"),
        ("\x1b[2^", "ctrl+insert"),
        ("\x1b[3$", "shift+delete"),
        ("\x1b[3^", "ctrl+delete"),
        ("\x1b[[5~", "pageUp"),
        ("\x1b[[6~", "pageDown"),
        ("\x1b[a", "shift+up"),
        ("\x1b[b", "shift+down"),
        ("\x1b[c", "shift+right"),
        ("\x1b[d", "shift+left"),
        ("\x1bOa", "ctrl+up"),
        ("\x1bOb", "ctrl+down"),
        ("\x1bOc", "ctrl+right"),
        ("\x1bOd", "ctrl+left"),
        ("\x1b[5$", "shift+pageUp"),
        ("\x1b[6$", "shift+pageDown"),
        ("\x1b[7$", "shift+home"),
        ("\x1b[8$", "shift+end"),
        ("\x1b[5^", "ctrl+pageUp"),
        ("\x1b[6^", "ctrl+pageDown"),
        ("\x1b[7^", "ctrl+home"),
        ("\x1b[8^", "ctrl+end"),
        ("\x1bOP", "f1"),
        ("\x1bOQ", "f2"),
        ("\x1bOR", "f3"),
        ("\x1bOS", "f4"),
        ("\x1b[11~", "f1"),
        ("\x1b[12~", "f2"),
        ("\x1b[13~", "f3"),
        ("\x1b[14~", "f4"),
        ("\x1b[[A", "f1"),
        ("\x1b[[B", "f2"),
        ("\x1b[[C", "f3"),
        ("\x1b[[D", "f4"),
        ("\x1b[[E", "f5"),
        ("\x1b[15~", "f5"),
        ("\x1b[17~", "f6"),
        ("\x1b[18~", "f7"),
        ("\x1b[19~", "f8"),
        ("\x1b[20~", "f9"),
        ("\x1b[21~", "f10"),
        ("\x1b[23~", "f11"),
        ("\x1b[24~", "f12"),
        ("\x1bb", "alt+left"),
        ("\x1bf", "alt+right"),
        ("\x1bp", "alt+up"),
        ("\x1bn", "alt+down"),
    ])
});

/// `keyof typeof LEGACY_SHIFT_SEQUENCES`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LegacyModifierKey {
    Up,
    Down,
    Right,
    Left,
    Clear,
    Insert,
    Delete,
    PageUp,
    PageDown,
    Home,
    End,
}

static KITTY_CSI_U_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b\[(\d+)(?::(\d*))?(?::(\d+))?(?:;(\d+))?(?::(\d+))?u$").unwrap());

const KITTY_PRINTABLE_ALLOWED_MODIFIERS: i64 = MODIFIERS.shift | LOCK_MASK;

struct ParsedKittySequence {
    codepoint: i64,
    shifted_key: Option<i64>,
    base_layout_key: Option<i64>,
    modifier: i64,
    event_type: KeyEventType,
}

struct ParsedModifyOtherKeysSequence {
    codepoint: i64,
    modifier: i64,
}

struct ParsedKeyId {
    key: String,
    ctrl: bool,
    shift: bool,
    alt: bool,
    super_: bool,
}

fn normalize_kitty_functional_codepoint(codepoint: i64) -> i64 {
    todo!("port: normalize_kitty_functional_codepoint")
}

fn normalize_shifted_letter_identity_codepoint(codepoint: i64, modifier: i64) -> i64 {
    todo!("port: normalize_shifted_letter_identity_codepoint")
}

fn matches_legacy_sequence(data: &str, sequences: &[&str]) -> bool {
    todo!("port: matches_legacy_sequence")
}

fn matches_legacy_modifier_sequence(data: &str, key: LegacyModifierKey, modifier: i64) -> bool {
    todo!("port: matches_legacy_modifier_sequence")
}

fn parse_event_type(event_type_str: Option<&str>) -> KeyEventType {
    todo!("port: parse_event_type")
}

fn parse_kitty_sequence(data: &str) -> Option<ParsedKittySequence> {
    todo!("port: parse_kitty_sequence")
}

/// Alternate match uses `baseLayoutKey` only when the codepoint is not already a
/// recognized Latin letter (a-z) or known symbol. A recognized codepoint is
/// authoritative regardless of physical key position (Dvorak, Colemak, xremap).
fn matches_kitty_sequence(data: &str, expected_codepoint: i64, expected_modifier: i64) -> bool {
    todo!("port: matches_kitty_sequence")
}

fn parse_modify_other_keys_sequence(data: &str) -> Option<ParsedModifyOtherKeysSequence> {
    todo!("port: parse_modify_other_keys_sequence")
}

/// Match xterm modifyOtherKeys format: CSI 27 ; modifiers ; keycode ~
/// This is used by terminals when Kitty protocol is not enabled.
/// Modifier values are 1-indexed: 2=shift, 3=alt, 5=ctrl, etc.
fn matches_modify_other_keys(data: &str, expected_keycode: i64, expected_modifier: i64) -> bool {
    todo!("port: matches_modify_other_keys")
}

fn is_windows_terminal_session() -> bool {
    todo!("port: is_windows_terminal_session")
}

/// Raw 0x08 (BS) is ambiguous in legacy terminals.
///
/// - Windows Terminal uses it for Ctrl+Backspace.
/// - Some legacy terminals and tmux setups send it for plain Backspace.
///
/// Prefer explicit Kitty / CSI-u / modifyOtherKeys sequences whenever they are
/// available. Fall back to a Windows Terminal heuristic only for raw BS bytes.
fn matches_raw_backspace(data: &str, expected_modifier: i64) -> bool {
    todo!("port: matches_raw_backspace")
}

/// Get the control character for a key.
/// Uses the universal formula: code & 0x1f (mask to lower 5 bits).
///
/// Works for:
/// - Letters a-z → 1-26
/// - Symbols [\]_ → 27, 28, 29, 31
/// - Also maps - to same as _ (same physical key on US keyboards)
fn raw_ctrl_char(key: &str) -> Option<String> {
    todo!("port: raw_ctrl_char")
}

fn is_digit_key(key: &str) -> bool {
    todo!("port: is_digit_key")
}

fn matches_printable_modify_other_keys(data: &str, expected_keycode: i64, expected_modifier: i64) -> bool {
    todo!("port: matches_printable_modify_other_keys")
}

fn format_key_name_with_modifiers(key_name: &str, modifier: i64) -> Option<String> {
    todo!("port: format_key_name_with_modifiers")
}

fn parse_key_id(key_id: &str) -> Option<ParsedKeyId> {
    todo!("port: parse_key_id")
}

/// Use base layout key only when codepoint is not a recognized Latin letter (a-z),
/// digit (0-9), or symbol. For those, the codepoint is authoritative regardless of
/// physical key position.
fn format_parsed_key(codepoint: i64, modifier: i64, base_layout_key: Option<i64>) -> Option<String> {
    todo!("port: format_parsed_key")
}

fn decode_modify_other_keys_printable(data: &str) -> Option<String> {
    todo!("port: decode_modify_other_keys_printable")
}

/// Check if the last parsed key event was a key release.
/// Only meaningful when Kitty keyboard protocol with flag 2 is active.
///
/// Bracketed paste (`\x1b[200~`) is never a release, even when it contains `:3F`.
pub fn is_key_release(data: &str) -> bool {
    todo!("port: is_key_release")
}

/// Check if the last parsed key event was a key repeat.
/// Only meaningful when Kitty keyboard protocol with flag 2 is active.
///
/// Bracketed paste (`\x1b[200~`) is never a repeat, even when it contains `:2F`.
pub fn is_key_repeat(data: &str) -> bool {
    todo!("port: is_key_repeat")
}

/// Match input data against a key identifier string.
///
/// Supported key identifiers:
/// - Single keys: "escape", "tab", "enter", "backspace", "delete", "home", "end", "space"
/// - Arrow keys: "up", "down", "left", "right"
/// - Ctrl combinations: "ctrl+c", "ctrl+z", etc.
/// - Shift combinations: "shift+tab", "shift+enter"
/// - Alt combinations: "alt+enter", "alt+backspace"
/// - Super combinations: "super+k", "super+enter"
/// - Combined modifiers: "shift+ctrl+p", "ctrl+alt+x", "ctrl+super+k"
///
/// Use the Key helper for autocomplete: `Key::ctrl("c")`, `Key::escape`, `Key::ctrl_shift("p")`, `Key::super_("k")`.
///
/// * `data` - Raw input data from terminal
/// * `key_id` - Key identifier (e.g., "ctrl+c", "escape", `Key::ctrl("c")`)
pub fn matches_key(data: &str, key_id: &str) -> bool {
    todo!("port: matches_key")
}

/// Parse input data and return the key identifier if recognized.
///
/// * `data` - Raw input data from terminal
///
/// Returns a key identifier string (e.g., "ctrl+c") or `None`.
///
/// PORT: TS return type is `string | undefined`, not `KeyId`.
pub fn parse_key(data: &str) -> Option<String> {
    todo!("port: parse_key")
}

/// Decode a Kitty CSI-u sequence into a printable character, if applicable.
///
/// When Kitty keyboard protocol flag 1 (disambiguate) is active, terminals send
/// CSI-u sequences for all keys, including plain printable characters. This
/// function extracts the printable character from such sequences.
///
/// Only accepts plain or Shift-modified keys. Rejects Ctrl, Alt, and unsupported
/// modifier combinations (those are handled by keybinding matching instead).
/// Prefers the shifted keycode when Shift is held and a shifted key is reported.
///
/// * `data` - Raw input data from terminal
///
/// Returns the printable character, or `None` if not a printable CSI-u sequence.
pub fn decode_kitty_printable(data: &str) -> Option<String> {
    todo!("port: decode_kitty_printable")
}

pub fn decode_printable_key(data: &str) -> Option<String> {
    todo!("port: decode_printable_key")
}
