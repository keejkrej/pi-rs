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

#![allow(dead_code)]

use std::sync::{LazyLock, Mutex};

use indexmap::{IndexMap, IndexSet};
use pi_js::env;
use pi_js::num::to_int32;
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

static KITTY_ARROW_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b\[1;(\d+)(?::(\d+))?([ABCD])$").unwrap());
static KITTY_FUNC_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b\[(\d+)(?:;(\d+))?(?::(\d+))?~$").unwrap());
static KITTY_HOME_END_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b\[1;(\d+)(?::(\d+))?([HF])$").unwrap());
static MODIFY_OTHER_KEYS_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\x1b\[27;(\d+);(\d+)~$").unwrap());

#[inline]
fn js_bitand(a: i64, b: i64) -> i64 {
    (to_int32(a as f64) & to_int32(b as f64)) as i64
}

#[inline]
fn js_bitnot(a: i64) -> i64 {
    (!to_int32(a as f64)) as i64
}

#[inline]
fn from_char_code_str(cp: i64) -> String {
    let code_unit = cp.rem_euclid(65536) as u16;
    String::from_utf16_lossy(&[code_unit])
}

#[inline]
fn is_non_empty_env(name: &str) -> bool {
    env::var(name).is_some_and(|v| !v.is_empty())
}

fn normalize_kitty_functional_codepoint(codepoint: i64) -> i64 {
    KITTY_FUNCTIONAL_KEY_EQUIVALENTS
        .get(&codepoint)
        .copied()
        .unwrap_or(codepoint)
}

fn normalize_shifted_letter_identity_codepoint(codepoint: i64, modifier: i64) -> i64 {
    let effective_modifier = js_bitand(modifier, js_bitnot(LOCK_MASK));
    if js_bitand(effective_modifier, MODIFIERS.shift) != 0 && (65..=90).contains(&codepoint) {
        return codepoint + 32;
    }
    codepoint
}

fn matches_legacy_sequence(data: &str, sequences: &[&str]) -> bool {
    sequences.contains(&data)
}

fn matches_legacy_modifier_sequence(data: &str, key: LegacyModifierKey, modifier: i64) -> bool {
    let table = if modifier == MODIFIERS.shift {
        &LEGACY_SHIFT_SEQUENCES
    } else if modifier == MODIFIERS.ctrl {
        &LEGACY_CTRL_SEQUENCES
    } else {
        return false;
    };
    let seqs = match key {
        LegacyModifierKey::Up => table.up,
        LegacyModifierKey::Down => table.down,
        LegacyModifierKey::Right => table.right,
        LegacyModifierKey::Left => table.left,
        LegacyModifierKey::Clear => table.clear,
        LegacyModifierKey::Insert => table.insert,
        LegacyModifierKey::Delete => table.delete,
        LegacyModifierKey::PageUp => table.page_up,
        LegacyModifierKey::PageDown => table.page_down,
        LegacyModifierKey::Home => table.home,
        LegacyModifierKey::End => table.end,
    };
    matches_legacy_sequence(data, seqs)
}

fn parse_event_type(event_type_str: Option<&str>) -> KeyEventType {
    let Some(s) = event_type_str else {
        return KeyEventType::Press;
    };
    match s.parse::<i64>() {
        Ok(2) => KeyEventType::Repeat,
        Ok(3) => KeyEventType::Release,
        _ => KeyEventType::Press,
    }
}

fn parse_kitty_sequence(data: &str) -> Option<ParsedKittySequence> {
    if let Some(caps) = KITTY_CSI_U_REGEX.captures(data) {
        let codepoint = caps.get(1)?.as_str().parse::<i64>().ok()?;
        let shifted_key = caps
            .get(2)
            .map(|m| m.as_str())
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<i64>().ok());
        let base_layout_key = caps.get(3).and_then(|m| m.as_str().parse::<i64>().ok());
        let mod_value = caps.get(4).and_then(|m| m.as_str().parse::<i64>().ok()).unwrap_or(1);
        let event_type = parse_event_type(caps.get(5).map(|m| m.as_str()));
        *LAST_EVENT_TYPE.lock().unwrap() = event_type;
        return Some(ParsedKittySequence {
            codepoint,
            shifted_key,
            base_layout_key,
            modifier: mod_value - 1,
            event_type,
        });
    }

    if let Some(caps) = KITTY_ARROW_REGEX.captures(data) {
        let mod_value = caps.get(1)?.as_str().parse::<i64>().ok()?;
        let event_type = parse_event_type(caps.get(2).map(|m| m.as_str()));
        let codepoint = match caps.get(3)?.as_str() {
            "A" => ARROW_CODEPOINTS.up,
            "B" => ARROW_CODEPOINTS.down,
            "C" => ARROW_CODEPOINTS.right,
            "D" => ARROW_CODEPOINTS.left,
            _ => return None,
        };
        *LAST_EVENT_TYPE.lock().unwrap() = event_type;
        return Some(ParsedKittySequence {
            codepoint,
            shifted_key: None,
            base_layout_key: None,
            modifier: mod_value - 1,
            event_type,
        });
    }

    if let Some(caps) = KITTY_FUNC_REGEX.captures(data) {
        let key_num = caps.get(1)?.as_str().parse::<i64>().ok()?;
        let mod_value = caps.get(2).and_then(|m| m.as_str().parse::<i64>().ok()).unwrap_or(1);
        let event_type = parse_event_type(caps.get(3).map(|m| m.as_str()));
        let codepoint = match key_num {
            2 => FUNCTIONAL_CODEPOINTS.insert,
            3 => FUNCTIONAL_CODEPOINTS.delete,
            5 => FUNCTIONAL_CODEPOINTS.page_up,
            6 => FUNCTIONAL_CODEPOINTS.page_down,
            7 => FUNCTIONAL_CODEPOINTS.home,
            8 => FUNCTIONAL_CODEPOINTS.end,
            _ => -1,
        };
        if codepoint != -1 {
            *LAST_EVENT_TYPE.lock().unwrap() = event_type;
            return Some(ParsedKittySequence {
                codepoint,
                shifted_key: None,
                base_layout_key: None,
                modifier: mod_value - 1,
                event_type,
            });
        }
    }

    if let Some(caps) = KITTY_HOME_END_REGEX.captures(data) {
        let mod_value = caps.get(1)?.as_str().parse::<i64>().ok()?;
        let event_type = parse_event_type(caps.get(2).map(|m| m.as_str()));
        let codepoint = if caps.get(3)?.as_str() == "H" {
            FUNCTIONAL_CODEPOINTS.home
        } else {
            FUNCTIONAL_CODEPOINTS.end
        };
        *LAST_EVENT_TYPE.lock().unwrap() = event_type;
        return Some(ParsedKittySequence {
            codepoint,
            shifted_key: None,
            base_layout_key: None,
            modifier: mod_value - 1,
            event_type,
        });
    }

    None
}

/// Alternate match uses `baseLayoutKey` only when the codepoint is not already a
/// recognized Latin letter (a-z) or known symbol. A recognized codepoint is
/// authoritative regardless of physical key position (Dvorak, Colemak, xremap).
fn matches_kitty_sequence(data: &str, expected_codepoint: i64, expected_modifier: i64) -> bool {
    let Some(parsed) = parse_kitty_sequence(data) else {
        return false;
    };
    let actual_mod = js_bitand(parsed.modifier, js_bitnot(LOCK_MASK));
    let expected_mod = js_bitand(expected_modifier, js_bitnot(LOCK_MASK));

    if actual_mod != expected_mod {
        return false;
    }

    let normalized_codepoint = normalize_shifted_letter_identity_codepoint(
        normalize_kitty_functional_codepoint(parsed.codepoint),
        parsed.modifier,
    );
    let normalized_expected_codepoint = normalize_shifted_letter_identity_codepoint(
        normalize_kitty_functional_codepoint(expected_codepoint),
        expected_modifier,
    );

    if normalized_codepoint == normalized_expected_codepoint {
        return true;
    }

    if parsed.base_layout_key == Some(expected_codepoint) {
        let cp = normalized_codepoint;
        let is_latin_letter = (97..=122).contains(&cp);
        let sym = from_char_code_str(cp);
        let is_known_symbol = SYMBOL_KEYS.contains(sym.as_str());
        if !is_latin_letter && !is_known_symbol {
            return true;
        }
    }

    false
}

fn parse_modify_other_keys_sequence(data: &str) -> Option<ParsedModifyOtherKeysSequence> {
    let caps = MODIFY_OTHER_KEYS_REGEX.captures(data)?;
    let mod_value = caps.get(1)?.as_str().parse::<i64>().ok()?;
    let codepoint = caps.get(2)?.as_str().parse::<i64>().ok()?;
    Some(ParsedModifyOtherKeysSequence {
        codepoint,
        modifier: mod_value - 1,
    })
}

/// Match xterm modifyOtherKeys format: CSI 27 ; modifiers ; keycode ~
/// This is used by terminals when Kitty protocol is not enabled.
/// Modifier values are 1-indexed: 2=shift, 3=alt, 5=ctrl, etc.
fn matches_modify_other_keys(data: &str, expected_keycode: i64, expected_modifier: i64) -> bool {
    let Some(parsed) = parse_modify_other_keys_sequence(data) else {
        return false;
    };
    parsed.codepoint == expected_keycode && parsed.modifier == expected_modifier
}

fn is_windows_terminal_session() -> bool {
    is_non_empty_env("WT_SESSION")
        && !is_non_empty_env("SSH_CONNECTION")
        && !is_non_empty_env("SSH_CLIENT")
        && !is_non_empty_env("SSH_TTY")
}

/// Raw 0x08 (BS) is ambiguous in legacy terminals.
///
/// - Windows Terminal uses it for Ctrl+Backspace.
/// - Some legacy terminals and tmux setups send it for plain Backspace.
///
/// Prefer explicit Kitty / CSI-u / modifyOtherKeys sequences whenever they are
/// available. Fall back to a Windows Terminal heuristic only for raw BS bytes.
fn matches_raw_backspace(data: &str, expected_modifier: i64) -> bool {
    if data == "\x7f" {
        return expected_modifier == 0;
    }
    if data != "\x08" {
        return false;
    }
    if is_windows_terminal_session() {
        expected_modifier == MODIFIERS.ctrl
    } else {
        expected_modifier == 0
    }
}

/// Get the control character for a key.
/// Uses the universal formula: code & 0x1f (mask to lower 5 bits).
///
/// Works for:
/// - Letters a-z → 1-26
/// - Symbols [\]_ → 27, 28, 29, 31
/// - Also maps - to same as _ (same physical key on US keyboards)
fn raw_ctrl_char(key: &str) -> Option<String> {
    let lower = key.to_lowercase();
    let ch = lower.chars().next()?;
    let code = ch as u32;
    if (97..=122).contains(&code) || ch == '[' || ch == '\\' || ch == ']' || ch == '_' {
        return Some(from_char_code_str((code & 0x1f) as i64));
    }
    if ch == '-' {
        return Some(from_char_code_str(31));
    }
    None
}

fn is_digit_key(key: &str) -> bool {
    key >= "0" && key <= "9"
}

fn matches_printable_modify_other_keys(data: &str, expected_keycode: i64, expected_modifier: i64) -> bool {
    if expected_modifier == 0 {
        return false;
    }
    let Some(parsed) = parse_modify_other_keys_sequence(data) else {
        return false;
    };
    if parsed.modifier != expected_modifier {
        return false;
    }
    normalize_shifted_letter_identity_codepoint(parsed.codepoint, parsed.modifier)
        == normalize_shifted_letter_identity_codepoint(expected_keycode, expected_modifier)
}

fn format_key_name_with_modifiers(key_name: &str, modifier: i64) -> Option<String> {
    let mut mods: Vec<&'static str> = Vec::new();
    let effective_mod = js_bitand(modifier, js_bitnot(LOCK_MASK));
    let supported_modifier_mask = MODIFIERS.shift | MODIFIERS.ctrl | MODIFIERS.alt | MODIFIERS.super_;
    if js_bitand(effective_mod, js_bitnot(supported_modifier_mask)) != 0 {
        return None;
    }
    if js_bitand(effective_mod, MODIFIERS.shift) != 0 {
        mods.push("shift");
    }
    if js_bitand(effective_mod, MODIFIERS.ctrl) != 0 {
        mods.push("ctrl");
    }
    if js_bitand(effective_mod, MODIFIERS.alt) != 0 {
        mods.push("alt");
    }
    if js_bitand(effective_mod, MODIFIERS.super_) != 0 {
        mods.push("super");
    }
    if mods.is_empty() {
        Some(key_name.to_string())
    } else {
        Some(format!("{}+{}", mods.join("+"), key_name))
    }
}

fn parse_key_id(key_id: &str) -> Option<ParsedKeyId> {
    let lower = key_id.to_lowercase();
    let parts: Vec<&str> = lower.split('+').collect();
    let key = *parts.last()?;
    if key.is_empty() {
        return None;
    }
    Some(ParsedKeyId {
        key: key.to_string(),
        ctrl: parts.contains(&"ctrl"),
        shift: parts.contains(&"shift"),
        alt: parts.contains(&"alt"),
        super_: parts.contains(&"super"),
    })
}

/// Use base layout key only when codepoint is not a recognized Latin letter (a-z),
/// digit (0-9), or symbol. For those, the codepoint is authoritative regardless of
/// physical key position.
fn format_parsed_key(codepoint: i64, modifier: i64, base_layout_key: Option<i64>) -> Option<String> {
    let normalized_codepoint = normalize_kitty_functional_codepoint(codepoint);
    let identity_codepoint = normalize_shifted_letter_identity_codepoint(normalized_codepoint, modifier);

    let is_latin_letter = (97..=122).contains(&identity_codepoint);
    let is_digit = (48..=57).contains(&identity_codepoint);
    let identity_str = from_char_code_str(identity_codepoint);
    let is_known_symbol = SYMBOL_KEYS.contains(identity_str.as_str());
    let effective_codepoint = if is_latin_letter || is_digit || is_known_symbol {
        identity_codepoint
    } else {
        base_layout_key.unwrap_or(identity_codepoint)
    };

    let effective_str;
    let key_name: Option<&str> = if effective_codepoint == CODEPOINTS.escape {
        Some("escape")
    } else if effective_codepoint == CODEPOINTS.tab {
        Some("tab")
    } else if effective_codepoint == CODEPOINTS.enter || effective_codepoint == CODEPOINTS.kp_enter {
        Some("enter")
    } else if effective_codepoint == CODEPOINTS.space {
        Some("space")
    } else if effective_codepoint == CODEPOINTS.backspace {
        Some("backspace")
    } else if effective_codepoint == FUNCTIONAL_CODEPOINTS.delete {
        Some("delete")
    } else if effective_codepoint == FUNCTIONAL_CODEPOINTS.insert {
        Some("insert")
    } else if effective_codepoint == FUNCTIONAL_CODEPOINTS.home {
        Some("home")
    } else if effective_codepoint == FUNCTIONAL_CODEPOINTS.end {
        Some("end")
    } else if effective_codepoint == FUNCTIONAL_CODEPOINTS.page_up {
        Some("pageUp")
    } else if effective_codepoint == FUNCTIONAL_CODEPOINTS.page_down {
        Some("pageDown")
    } else if effective_codepoint == ARROW_CODEPOINTS.up {
        Some("up")
    } else if effective_codepoint == ARROW_CODEPOINTS.down {
        Some("down")
    } else if effective_codepoint == ARROW_CODEPOINTS.left {
        Some("left")
    } else if effective_codepoint == ARROW_CODEPOINTS.right {
        Some("right")
    } else if (48..=57).contains(&effective_codepoint) || (97..=122).contains(&effective_codepoint) {
        effective_str = from_char_code_str(effective_codepoint);
        Some(effective_str.as_str())
    } else {
        effective_str = from_char_code_str(effective_codepoint);
        if SYMBOL_KEYS.contains(effective_str.as_str()) {
            Some(effective_str.as_str())
        } else {
            None
        }
    };

    format_key_name_with_modifiers(key_name?, modifier)
}

fn decode_modify_other_keys_printable(data: &str) -> Option<String> {
    let parsed = parse_modify_other_keys_sequence(data)?;
    let modifier = js_bitand(parsed.modifier, js_bitnot(LOCK_MASK));
    if js_bitand(modifier, js_bitnot(MODIFIERS.shift)) != 0 {
        return None;
    }
    if parsed.codepoint < 32 {
        return None;
    }
    let ch = u32::try_from(parsed.codepoint).ok().and_then(char::from_u32)?;
    Some(ch.to_string())
}

/// Check if the last parsed key event was a key release.
/// Only meaningful when Kitty keyboard protocol with flag 2 is active.
///
/// Bracketed paste (`\x1b[200~`) is never a release, even when it contains `:3F`.
pub fn is_key_release(data: &str) -> bool {
    if data.contains("\x1b[200~") {
        return false;
    }
    data.contains(":3u")
        || data.contains(":3~")
        || data.contains(":3A")
        || data.contains(":3B")
        || data.contains(":3C")
        || data.contains(":3D")
        || data.contains(":3H")
        || data.contains(":3F")
}

/// Check if the last parsed key event was a key repeat.
/// Only meaningful when Kitty keyboard protocol with flag 2 is active.
///
/// Bracketed paste (`\x1b[200~`) is never a repeat, even when it contains `:2F`.
pub fn is_key_repeat(data: &str) -> bool {
    if data.contains("\x1b[200~") {
        return false;
    }
    data.contains(":2u")
        || data.contains(":2~")
        || data.contains(":2A")
        || data.contains(":2B")
        || data.contains(":2C")
        || data.contains(":2D")
        || data.contains(":2H")
        || data.contains(":2F")
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
    let Some(parsed) = parse_key_id(key_id) else {
        return false;
    };

    let mut modifier: i64 = 0;
    if parsed.shift {
        modifier |= MODIFIERS.shift;
    }
    if parsed.alt {
        modifier |= MODIFIERS.alt;
    }
    if parsed.ctrl {
        modifier |= MODIFIERS.ctrl;
    }
    if parsed.super_ {
        modifier |= MODIFIERS.super_;
    }

    let kitty_active = is_kitty_protocol_active();
    let key = parsed.key.as_str();

    match key {
        "escape" | "esc" => {
            if modifier != 0 {
                return false;
            }
            return data == "\x1b"
                || matches_kitty_sequence(data, CODEPOINTS.escape, 0)
                || matches_modify_other_keys(data, CODEPOINTS.escape, 0);
        }
        "space" => {
            if !kitty_active {
                if modifier == MODIFIERS.ctrl && data == "\x00" {
                    return true;
                }
                if modifier == MODIFIERS.alt && data == "\x1b " {
                    return true;
                }
            }
            if modifier == 0 {
                return data == " "
                    || matches_kitty_sequence(data, CODEPOINTS.space, 0)
                    || matches_modify_other_keys(data, CODEPOINTS.space, 0);
            }
            return matches_kitty_sequence(data, CODEPOINTS.space, modifier)
                || matches_modify_other_keys(data, CODEPOINTS.space, modifier);
        }
        "tab" => {
            if modifier == MODIFIERS.shift {
                return data == "\x1b[Z"
                    || matches_kitty_sequence(data, CODEPOINTS.tab, MODIFIERS.shift)
                    || matches_modify_other_keys(data, CODEPOINTS.tab, MODIFIERS.shift);
            }
            if modifier == 0 {
                return data == "\t" || matches_kitty_sequence(data, CODEPOINTS.tab, 0);
            }
            return matches_kitty_sequence(data, CODEPOINTS.tab, modifier)
                || matches_modify_other_keys(data, CODEPOINTS.tab, modifier);
        }
        "enter" | "return" => {
            if modifier == MODIFIERS.shift {
                if matches_kitty_sequence(data, CODEPOINTS.enter, MODIFIERS.shift)
                    || matches_kitty_sequence(data, CODEPOINTS.kp_enter, MODIFIERS.shift)
                {
                    return true;
                }
                if matches_modify_other_keys(data, CODEPOINTS.enter, MODIFIERS.shift) {
                    return true;
                }
                if kitty_active {
                    return data == "\x1b\r" || data == "\n";
                }
                return false;
            }
            if modifier == MODIFIERS.alt {
                if matches_kitty_sequence(data, CODEPOINTS.enter, MODIFIERS.alt)
                    || matches_kitty_sequence(data, CODEPOINTS.kp_enter, MODIFIERS.alt)
                {
                    return true;
                }
                if matches_modify_other_keys(data, CODEPOINTS.enter, MODIFIERS.alt) {
                    return true;
                }
                if !kitty_active {
                    return data == "\x1b\r";
                }
                return false;
            }
            if modifier == 0 {
                return data == "\r"
                    || (!kitty_active && data == "\n")
                    || data == "\x1bOM"
                    || matches_kitty_sequence(data, CODEPOINTS.enter, 0)
                    || matches_kitty_sequence(data, CODEPOINTS.kp_enter, 0);
            }
            return matches_kitty_sequence(data, CODEPOINTS.enter, modifier)
                || matches_kitty_sequence(data, CODEPOINTS.kp_enter, modifier)
                || matches_modify_other_keys(data, CODEPOINTS.enter, modifier);
        }
        "backspace" => {
            if modifier == MODIFIERS.alt {
                if data == "\x1b\x7f" || data == "\x1b\x08" {
                    return true;
                }
                return matches_kitty_sequence(data, CODEPOINTS.backspace, MODIFIERS.alt)
                    || matches_modify_other_keys(data, CODEPOINTS.backspace, MODIFIERS.alt);
            }
            if modifier == MODIFIERS.ctrl {
                if matches_raw_backspace(data, MODIFIERS.ctrl) {
                    return true;
                }
                return matches_kitty_sequence(data, CODEPOINTS.backspace, MODIFIERS.ctrl)
                    || matches_modify_other_keys(data, CODEPOINTS.backspace, MODIFIERS.ctrl);
            }
            if modifier == 0 {
                return matches_raw_backspace(data, 0)
                    || matches_kitty_sequence(data, CODEPOINTS.backspace, 0)
                    || matches_modify_other_keys(data, CODEPOINTS.backspace, 0);
            }
            return matches_kitty_sequence(data, CODEPOINTS.backspace, modifier)
                || matches_modify_other_keys(data, CODEPOINTS.backspace, modifier);
        }
        "insert" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, LEGACY_KEY_SEQUENCES.insert)
                    || matches_kitty_sequence(data, FUNCTIONAL_CODEPOINTS.insert, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::Insert, modifier) {
                return true;
            }
            return matches_kitty_sequence(data, FUNCTIONAL_CODEPOINTS.insert, modifier);
        }
        "delete" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, LEGACY_KEY_SEQUENCES.delete)
                    || matches_kitty_sequence(data, FUNCTIONAL_CODEPOINTS.delete, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::Delete, modifier) {
                return true;
            }
            return matches_kitty_sequence(data, FUNCTIONAL_CODEPOINTS.delete, modifier);
        }
        "clear" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, LEGACY_KEY_SEQUENCES.clear);
            }
            return matches_legacy_modifier_sequence(data, LegacyModifierKey::Clear, modifier);
        }
        "home" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, LEGACY_KEY_SEQUENCES.home)
                    || matches_kitty_sequence(data, FUNCTIONAL_CODEPOINTS.home, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::Home, modifier) {
                return true;
            }
            return matches_kitty_sequence(data, FUNCTIONAL_CODEPOINTS.home, modifier);
        }
        "end" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, LEGACY_KEY_SEQUENCES.end)
                    || matches_kitty_sequence(data, FUNCTIONAL_CODEPOINTS.end, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::End, modifier) {
                return true;
            }
            return matches_kitty_sequence(data, FUNCTIONAL_CODEPOINTS.end, modifier);
        }
        "pageup" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, LEGACY_KEY_SEQUENCES.page_up)
                    || matches_kitty_sequence(data, FUNCTIONAL_CODEPOINTS.page_up, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::PageUp, modifier) {
                return true;
            }
            return matches_kitty_sequence(data, FUNCTIONAL_CODEPOINTS.page_up, modifier);
        }
        "pagedown" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, LEGACY_KEY_SEQUENCES.page_down)
                    || matches_kitty_sequence(data, FUNCTIONAL_CODEPOINTS.page_down, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::PageDown, modifier) {
                return true;
            }
            return matches_kitty_sequence(data, FUNCTIONAL_CODEPOINTS.page_down, modifier);
        }
        "up" => {
            if modifier == MODIFIERS.alt {
                return data == "\x1bp" || matches_kitty_sequence(data, ARROW_CODEPOINTS.up, MODIFIERS.alt);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, LEGACY_KEY_SEQUENCES.up)
                    || matches_kitty_sequence(data, ARROW_CODEPOINTS.up, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::Up, modifier) {
                return true;
            }
            return matches_kitty_sequence(data, ARROW_CODEPOINTS.up, modifier);
        }
        "down" => {
            if modifier == MODIFIERS.alt {
                return data == "\x1bn" || matches_kitty_sequence(data, ARROW_CODEPOINTS.down, MODIFIERS.alt);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, LEGACY_KEY_SEQUENCES.down)
                    || matches_kitty_sequence(data, ARROW_CODEPOINTS.down, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::Down, modifier) {
                return true;
            }
            return matches_kitty_sequence(data, ARROW_CODEPOINTS.down, modifier);
        }
        "left" => {
            if modifier == MODIFIERS.alt {
                return data == "\x1b[1;3D"
                    || (!kitty_active && data == "\x1bB")
                    || data == "\x1bb"
                    || matches_kitty_sequence(data, ARROW_CODEPOINTS.left, MODIFIERS.alt);
            }
            if modifier == MODIFIERS.ctrl {
                return data == "\x1b[1;5D"
                    || matches_legacy_modifier_sequence(data, LegacyModifierKey::Left, MODIFIERS.ctrl)
                    || matches_kitty_sequence(data, ARROW_CODEPOINTS.left, MODIFIERS.ctrl);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, LEGACY_KEY_SEQUENCES.left)
                    || matches_kitty_sequence(data, ARROW_CODEPOINTS.left, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::Left, modifier) {
                return true;
            }
            return matches_kitty_sequence(data, ARROW_CODEPOINTS.left, modifier);
        }
        "right" => {
            if modifier == MODIFIERS.alt {
                return data == "\x1b[1;3C"
                    || (!kitty_active && data == "\x1bF")
                    || data == "\x1bf"
                    || matches_kitty_sequence(data, ARROW_CODEPOINTS.right, MODIFIERS.alt);
            }
            if modifier == MODIFIERS.ctrl {
                return data == "\x1b[1;5C"
                    || matches_legacy_modifier_sequence(data, LegacyModifierKey::Right, MODIFIERS.ctrl)
                    || matches_kitty_sequence(data, ARROW_CODEPOINTS.right, MODIFIERS.ctrl);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, LEGACY_KEY_SEQUENCES.right)
                    || matches_kitty_sequence(data, ARROW_CODEPOINTS.right, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::Right, modifier) {
                return true;
            }
            return matches_kitty_sequence(data, ARROW_CODEPOINTS.right, modifier);
        }
        "f1" | "f2" | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12" => {
            if modifier != 0 {
                return false;
            }
            let seqs = match key {
                "f1" => LEGACY_KEY_SEQUENCES.f1,
                "f2" => LEGACY_KEY_SEQUENCES.f2,
                "f3" => LEGACY_KEY_SEQUENCES.f3,
                "f4" => LEGACY_KEY_SEQUENCES.f4,
                "f5" => LEGACY_KEY_SEQUENCES.f5,
                "f6" => LEGACY_KEY_SEQUENCES.f6,
                "f7" => LEGACY_KEY_SEQUENCES.f7,
                "f8" => LEGACY_KEY_SEQUENCES.f8,
                "f9" => LEGACY_KEY_SEQUENCES.f9,
                "f10" => LEGACY_KEY_SEQUENCES.f10,
                "f11" => LEGACY_KEY_SEQUENCES.f11,
                "f12" => LEGACY_KEY_SEQUENCES.f12,
                _ => return false,
            };
            return matches_legacy_sequence(data, seqs);
        }
        _ => {}
    }

    if key.len() == 1 && ((key >= "a" && key <= "z") || is_digit_key(key) || SYMBOL_KEYS.contains(key)) {
        let codepoint = key.as_bytes()[0] as i64;
        let raw_ctrl = raw_ctrl_char(key);
        let is_letter = key >= "a" && key <= "z";
        let is_digit = is_digit_key(key);

        if modifier == MODIFIERS.ctrl + MODIFIERS.alt && !kitty_active {
            if let Some(ref rc) = raw_ctrl {
                if data == format!("\x1b{rc}") {
                    return true;
                }
            }
        }

        if modifier == MODIFIERS.alt && !kitty_active && (is_letter || is_digit || SYMBOL_KEYS.contains(key)) {
            if data == format!("\x1b{key}") {
                return true;
            }
        }

        if modifier == MODIFIERS.ctrl {
            if let Some(ref rc) = raw_ctrl {
                if data == rc {
                    return true;
                }
            }
            return matches_kitty_sequence(data, codepoint, MODIFIERS.ctrl)
                || matches_printable_modify_other_keys(data, codepoint, MODIFIERS.ctrl);
        }

        if modifier == MODIFIERS.shift + MODIFIERS.ctrl {
            return matches_kitty_sequence(data, codepoint, MODIFIERS.shift + MODIFIERS.ctrl)
                || matches_printable_modify_other_keys(data, codepoint, MODIFIERS.shift + MODIFIERS.ctrl);
        }

        if modifier == MODIFIERS.shift {
            if is_letter && data == key.to_ascii_uppercase() {
                return true;
            }
            return matches_kitty_sequence(data, codepoint, MODIFIERS.shift)
                || matches_printable_modify_other_keys(data, codepoint, MODIFIERS.shift);
        }

        if modifier != 0 {
            return matches_kitty_sequence(data, codepoint, modifier)
                || matches_printable_modify_other_keys(data, codepoint, modifier);
        }

        return data == key || matches_kitty_sequence(data, codepoint, 0);
    }

    false
}

/// Parse input data and return the key identifier if recognized.
///
/// * `data` - Raw input data from terminal
///
/// Returns a key identifier string (e.g., "ctrl+c") or `None`.
///
/// PORT: TS return type is `string | undefined`, not `KeyId`.
pub fn parse_key(data: &str) -> Option<String> {
    if let Some(kitty) = parse_kitty_sequence(data) {
        return format_parsed_key(kitty.codepoint, kitty.modifier, kitty.base_layout_key);
    }

    if let Some(modify_other_keys) = parse_modify_other_keys_sequence(data) {
        return format_parsed_key(modify_other_keys.codepoint, modify_other_keys.modifier, None);
    }

    let kitty_active = is_kitty_protocol_active();
    if kitty_active {
        if data == "\x1b\r" || data == "\n" {
            return Some("shift+enter".to_string());
        }
    }

    if let Some(&legacy_id) = LEGACY_SEQUENCE_KEY_IDS.get(data) {
        return Some(legacy_id.to_string());
    }

    match data {
        "\x1b" => return Some("escape".to_string()),
        "\x1c" => return Some("ctrl+\\".to_string()),
        "\x1d" => return Some("ctrl+]".to_string()),
        "\x1f" => return Some("ctrl+-".to_string()),
        "\x1b\x1b" => return Some("ctrl+alt+[".to_string()),
        "\x1b\x1c" => return Some("ctrl+alt+\\".to_string()),
        "\x1b\x1d" => return Some("ctrl+alt+]".to_string()),
        "\x1b\x1f" => return Some("ctrl+alt+-".to_string()),
        "\t" => return Some("tab".to_string()),
        "\x00" => return Some("ctrl+space".to_string()),
        " " => return Some("space".to_string()),
        "\x7f" => return Some("backspace".to_string()),
        "\x08" => {
            return Some(if is_windows_terminal_session() {
                "ctrl+backspace".to_string()
            } else {
                "backspace".to_string()
            });
        }
        "\x1b[Z" => return Some("shift+tab".to_string()),
        "\x1b\x7f" | "\x1b\x08" => return Some("alt+backspace".to_string()),
        "\x1b[A" => return Some("up".to_string()),
        "\x1b[B" => return Some("down".to_string()),
        "\x1b[C" => return Some("right".to_string()),
        "\x1b[D" => return Some("left".to_string()),
        "\x1b[H" | "\x1bOH" => return Some("home".to_string()),
        "\x1b[F" | "\x1bOF" => return Some("end".to_string()),
        "\x1b[3~" => return Some("delete".to_string()),
        "\x1b[5~" => return Some("pageUp".to_string()),
        "\x1b[6~" => return Some("pageDown".to_string()),
        _ => {}
    }

    if data == "\r" || (!kitty_active && data == "\n") || data == "\x1bOM" {
        return Some("enter".to_string());
    }
    if !kitty_active && data == "\x1b\r" {
        return Some("alt+enter".to_string());
    }
    if !kitty_active && data == "\x1b " {
        return Some("alt+space".to_string());
    }
    if !kitty_active && data == "\x1bB" {
        return Some("alt+left".to_string());
    }
    if !kitty_active && data == "\x1bF" {
        return Some("alt+right".to_string());
    }
    if !kitty_active {
        let bytes = data.as_bytes();
        if bytes.len() == 2 && bytes[0] == 0x1b {
            let code = bytes[1];
            if (1..=26).contains(&code) {
                return Some(format!("ctrl+alt+{}", char::from(code + 96)));
            }
            let key = from_char_code_str(code as i64);
            if (97..=122).contains(&code) || (48..=57).contains(&code) || SYMBOL_KEYS.contains(key.as_str()) {
                return Some(format!("alt+{key}"));
            }
        }
    }

    let bytes = data.as_bytes();
    if bytes.len() == 1 {
        let code = bytes[0];
        if (1..=26).contains(&code) {
            return Some(format!("ctrl+{}", char::from(code + 96)));
        }
        if (32..=126).contains(&code) {
            return Some(data.to_string());
        }
    }

    None
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
    let caps = KITTY_CSI_U_REGEX.captures(data)?;

    let codepoint = caps.get(1)?.as_str().parse::<i64>().ok()?;
    let shifted_key = caps
        .get(2)
        .map(|m| m.as_str())
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<i64>().ok());
    let mod_value = caps.get(4).and_then(|m| m.as_str().parse::<i64>().ok()).unwrap_or(1);
    let modifier = mod_value - 1;

    if js_bitand(modifier, js_bitnot(KITTY_PRINTABLE_ALLOWED_MODIFIERS)) != 0 {
        return None;
    }
    if js_bitand(modifier, MODIFIERS.alt | MODIFIERS.ctrl) != 0 {
        return None;
    }

    let mut effective_codepoint = codepoint;
    if js_bitand(modifier, MODIFIERS.shift) != 0 {
        if let Some(sk) = shifted_key {
            effective_codepoint = sk;
        }
    }
    effective_codepoint = normalize_kitty_functional_codepoint(effective_codepoint);
    if effective_codepoint < 32 {
        return None;
    }

    let ch = u32::try_from(effective_codepoint).ok().and_then(char::from_u32)?;
    Some(ch.to_string())
}

pub fn decode_printable_key(data: &str) -> Option<String> {
    decode_kitty_printable(data).or_else(|| decode_modify_other_keys_printable(data))
}
