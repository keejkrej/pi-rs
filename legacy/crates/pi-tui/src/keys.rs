use std::sync::atomic::{AtomicBool, Ordering};

static KITTY_PROTOCOL_ACTIVE: AtomicBool = AtomicBool::new(false);

pub type KeyId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventType {
    Press,
    Repeat,
    Release,
}

pub struct Key;

impl Key {
    pub const ESCAPE: &'static str = "escape";
    pub const ESC: &'static str = "esc";
    pub const ENTER: &'static str = "enter";
    pub const RETURN: &'static str = "return";
    pub const TAB: &'static str = "tab";
    pub const SPACE: &'static str = "space";
    pub const BACKSPACE: &'static str = "backspace";
    pub const DELETE: &'static str = "delete";
    pub const INSERT: &'static str = "insert";
    pub const HOME: &'static str = "home";
    pub const END: &'static str = "end";
    pub const PAGE_UP: &'static str = "pageUp";
    pub const PAGE_DOWN: &'static str = "pageDown";
    pub const UP: &'static str = "up";
    pub const DOWN: &'static str = "down";
    pub const LEFT: &'static str = "left";
    pub const RIGHT: &'static str = "right";

    pub fn ctrl(key: impl AsRef<str>) -> String {
        format!("ctrl+{}", key.as_ref())
    }
    pub fn shift(key: impl AsRef<str>) -> String {
        format!("shift+{}", key.as_ref())
    }
    pub fn alt(key: impl AsRef<str>) -> String {
        format!("alt+{}", key.as_ref())
    }
    pub fn super_key(key: impl AsRef<str>) -> String {
        format!("super+{}", key.as_ref())
    }
    pub fn ctrl_shift(key: impl AsRef<str>) -> String {
        format!("ctrl+shift+{}", key.as_ref())
    }
    pub fn ctrl_alt(key: impl AsRef<str>) -> String {
        format!("ctrl+alt+{}", key.as_ref())
    }
    pub fn ctrl_super(key: impl AsRef<str>) -> String {
        format!("ctrl+super+{}", key.as_ref())
    }
    pub fn ctrl_shift_super(key: impl AsRef<str>) -> String {
        format!("ctrl+shift+super+{}", key.as_ref())
    }
}

pub fn set_kitty_protocol_active(active: bool) {
    KITTY_PROTOCOL_ACTIVE.store(active, Ordering::Relaxed);
}

pub fn is_kitty_protocol_active() -> bool {
    KITTY_PROTOCOL_ACTIVE.load(Ordering::Relaxed)
}

pub fn is_key_release(data: &str) -> bool {
    !data.contains("\u{1b}[200~")
        && (data.contains(":3u")
            || data.contains(":3~")
            || data.contains(":3A")
            || data.contains(":3B")
            || data.contains(":3C")
            || data.contains(":3D")
            || data.contains(":3H")
            || data.contains(":3F"))
}

pub fn is_key_repeat(data: &str) -> bool {
    !data.contains("\u{1b}[200~")
        && (data.contains(":2u")
            || data.contains(":2~")
            || data.contains(":2A")
            || data.contains(":2B")
            || data.contains(":2C")
            || data.contains(":2D")
            || data.contains(":2H")
            || data.contains(":2F"))
}

fn modifier_mask(parts: &[&str]) -> u8 {
    let mut mask = 0;
    for part in parts {
        match *part {
            "shift" => mask |= 1,
            "alt" => mask |= 2,
            "ctrl" => mask |= 4,
            "super" => mask |= 8,
            _ => {}
        }
    }
    mask
}

fn parse_key_id(key_id: &str) -> Option<(String, u8)> {
    let parts = key_id.split('+').collect::<Vec<_>>();
    let key = parts.last()?.to_lowercase();
    Some((key, modifier_mask(&parts[..parts.len().saturating_sub(1)])))
}

fn raw_ctrl_char(key: &str) -> Option<char> {
    let ch = key.chars().next()?.to_ascii_lowercase();
    match ch {
        'a'..='z' | '[' | '\\' | ']' | '_' => char::from_u32((ch as u32) & 0x1f),
        '-' => Some('\u{1f}'),
        _ => None,
    }
}

fn legacy_sequence(data: &str) -> Option<&'static str> {
    Some(match data {
        "\u{1b}OA" | "\u{1b}[A" => "up",
        "\u{1b}OB" | "\u{1b}[B" => "down",
        "\u{1b}OC" | "\u{1b}[C" => "right",
        "\u{1b}OD" | "\u{1b}[D" => "left",
        "\u{1b}OH" | "\u{1b}[H" | "\u{1b}[1~" | "\u{1b}[7~" => "home",
        "\u{1b}OF" | "\u{1b}[F" | "\u{1b}[4~" | "\u{1b}[8~" => "end",
        "\u{1b}[2~" => "insert",
        "\u{1b}[3~" => "delete",
        "\u{1b}[5~" | "\u{1b}[[5~" => "pageUp",
        "\u{1b}[6~" | "\u{1b}[[6~" => "pageDown",
        "\u{1b}[Z" => "shift+tab",
        "\u{1b}OP" | "\u{1b}[11~" | "\u{1b}[[A" => "f1",
        "\u{1b}OQ" | "\u{1b}[12~" | "\u{1b}[[B" => "f2",
        "\u{1b}OR" | "\u{1b}[13~" | "\u{1b}[[C" => "f3",
        "\u{1b}OS" | "\u{1b}[14~" | "\u{1b}[[D" => "f4",
        "\u{1b}[15~" | "\u{1b}[[E" => "f5",
        "\u{1b}[17~" => "f6",
        "\u{1b}[18~" => "f7",
        "\u{1b}[19~" => "f8",
        "\u{1b}[20~" => "f9",
        "\u{1b}[21~" => "f10",
        "\u{1b}[23~" => "f11",
        "\u{1b}[24~" => "f12",
        "\u{1b}b" => "alt+left",
        "\u{1b}f" => "alt+right",
        "\u{1b}p" => "alt+up",
        "\u{1b}n" => "alt+down",
        "\u{1b}[1;5D" => "ctrl+left",
        "\u{1b}[1;5C" => "ctrl+right",
        "\u{1b}[1;3D" => "alt+left",
        "\u{1b}[1;3C" => "alt+right",
        _ => return None,
    })
}

fn parse_csi_u_parts(data: &str) -> Option<(u32, Option<u32>, Option<u32>, u8)> {
    let body = data.strip_prefix("\u{1b}[")?.strip_suffix('u')?;
    let (code_part, mod_str) = body.split_once(';').unwrap_or((body, "1"));
    let mut code_parts = code_part.split(':');
    let codepoint = code_parts.next()?.parse::<u32>().ok()?;
    let shifted = code_parts.next().and_then(|part| {
        (!part.is_empty())
            .then(|| part.parse::<u32>().ok())
            .flatten()
    });
    let base = code_parts.next().and_then(|part| {
        (!part.is_empty())
            .then(|| part.parse::<u32>().ok())
            .flatten()
    });
    let modifier = mod_str
        .split(':')
        .next()
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(1)
        .saturating_sub(1);
    Some((codepoint, shifted, base, modifier))
}

fn normalize_kitty_functional_codepoint(codepoint: u32) -> u32 {
    match codepoint {
        57399 => b'0' as u32,
        57400 => b'1' as u32,
        57401 => b'2' as u32,
        57402 => b'3' as u32,
        57403 => b'4' as u32,
        57404 => b'5' as u32,
        57405 => b'6' as u32,
        57406 => b'7' as u32,
        57407 => b'8' as u32,
        57408 => b'9' as u32,
        57409 => b'.' as u32,
        57410 => b'/' as u32,
        57411 => b'*' as u32,
        57412 => b'-' as u32,
        57413 => b'+' as u32,
        57415 => b'=' as u32,
        57416 => b',' as u32,
        other => other,
    }
}

fn kitty_navigation_key(codepoint: u32) -> Option<&'static str> {
    Some(match codepoint {
        57417 => "left",
        57418 => "right",
        57419 => "up",
        57420 => "down",
        57421 => "pageUp",
        57422 => "pageDown",
        57423 => "home",
        57424 => "end",
        57425 => "insert",
        57426 => "delete",
        _ => return None,
    })
}

fn is_known_symbol(codepoint: u32) -> bool {
    matches!(
        codepoint,
        33..=47 | 58..=64 | 91..=96 | 123..=126
    )
}

fn parse_csi_u_key(data: &str) -> Option<String> {
    let (codepoint, _shifted, base, modifier) = parse_csi_u_parts(data)?;
    let normalized = normalize_kitty_functional_codepoint(codepoint);
    if let Some(key) = kitty_navigation_key(normalized).or_else(|| kitty_navigation_key(codepoint))
    {
        return Some(format_key_with_modifier(key, modifier));
    }

    let identity = if modifier & 1 != 0 && (65..=90).contains(&normalized) {
        normalized + 32
    } else {
        normalized
    };
    let is_latin = (b'a' as u32..=b'z' as u32).contains(&identity);
    let is_digit = (b'0' as u32..=b'9' as u32).contains(&identity);
    let effective = if is_latin || is_digit || is_known_symbol(identity) {
        identity
    } else {
        base.unwrap_or(identity)
    };

    let key = match effective {
        27 => "escape".to_string(),
        9 => "tab".to_string(),
        13 | 57414 => "enter".to_string(),
        32 => "space".to_string(),
        127 => "backspace".to_string(),
        cp if (b'A' as u32..=b'Z' as u32).contains(&cp) => char::from_u32(cp + 32)?.to_string(),
        cp if (b'a' as u32..=b'z' as u32).contains(&cp)
            || (b'0' as u32..=b'9' as u32).contains(&cp)
            || is_known_symbol(cp) =>
        {
            char::from_u32(cp)?.to_string()
        }
        _ => return None,
    };
    Some(format_key_with_modifier(&key, modifier))
}

fn parse_modify_other_keys(data: &str) -> Option<String> {
    let body = data.strip_prefix("\u{1b}[27;")?.strip_suffix('~')?;
    let mut parts = body.split(';');
    let modifier = parts.next()?.parse::<u8>().ok()?.saturating_sub(1);
    let codepoint = parts.next()?.parse::<u32>().ok()?;
    let key = match codepoint {
        27 => "escape".to_string(),
        9 => "tab".to_string(),
        13 => "enter".to_string(),
        32 => "space".to_string(),
        127 => "backspace".to_string(),
        cp if (b'A' as u32..=b'Z' as u32).contains(&cp) => char::from_u32(cp + 32)?.to_string(),
        cp if (b'a' as u32..=b'z' as u32).contains(&cp)
            || (b'0' as u32..=b'9' as u32).contains(&cp)
            || is_known_symbol(cp) =>
        {
            char::from_u32(cp)?.to_string()
        }
        _ => return None,
    };
    Some(format_key_with_modifier(&key, modifier))
}

pub fn decode_kitty_printable(data: &str) -> Option<String> {
    let (codepoint, shifted, _, modifier) = parse_csi_u_parts(data)?;
    const SHIFT: u8 = 1;
    const LOCK_MASK: u8 = 64 | 128;
    if modifier & !(SHIFT | LOCK_MASK) != 0 {
        return None;
    }
    let effective = if modifier & SHIFT != 0 {
        shifted.unwrap_or(codepoint)
    } else {
        codepoint
    };
    if effective < 32 {
        return None;
    }
    Some(char::from_u32(normalize_kitty_functional_codepoint(effective))?.to_string())
}

fn decode_modify_other_keys_printable(data: &str) -> Option<String> {
    let body = data.strip_prefix("\u{1b}[27;")?.strip_suffix('~')?;
    let mut parts = body.split(';');
    let modifier = parts.next()?.parse::<u8>().ok()?.saturating_sub(1);
    let codepoint = parts.next()?.parse::<u32>().ok()?;
    if modifier & !1 != 0 || codepoint < 32 {
        return None;
    }
    Some(char::from_u32(codepoint)?.to_string())
}

pub fn decode_printable_key(data: &str) -> Option<String> {
    decode_kitty_printable(data)
        .or_else(|| decode_modify_other_keys_printable(data))
        .or_else(|| {
            if data.chars().count() == 1 && data.chars().next().is_some_and(|ch| !ch.is_control()) {
                Some(data.to_string())
            } else {
                None
            }
        })
}

fn format_key_with_modifier(key: &str, modifier: u8) -> String {
    let mods = [(1, "shift"), (4, "ctrl"), (2, "alt"), (8, "super")]
        .into_iter()
        .filter_map(|(bit, name)| (modifier & bit != 0).then_some(name))
        .collect::<Vec<_>>();
    if mods.is_empty() {
        key.to_string()
    } else {
        format!("{}+{key}", mods.join("+"))
    }
}

fn parse_kitty_functional(data: &str) -> Option<String> {
    let body = data.strip_prefix("\u{1b}[")?;
    let final_char = body.chars().last()?;
    match final_char {
        'A' | 'B' | 'C' | 'D' | 'H' | 'F' => {
            let body = &body[..body.len() - final_char.len_utf8()];
            let (_, rest) = body.split_once(';')?;
            let modifier = rest
                .split(':')
                .next()?
                .parse::<u8>()
                .ok()?
                .saturating_sub(1);
            let key = match final_char {
                'A' => "up",
                'B' => "down",
                'C' => "right",
                'D' => "left",
                'H' => "home",
                'F' => "end",
                _ => unreachable!(),
            };
            Some(format_key_with_modifier(key, modifier))
        }
        '~' => {
            let body = body.strip_suffix('~')?;
            let (num, rest) = body.split_once(';').unwrap_or((body, "1"));
            let modifier = rest
                .split(':')
                .next()
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(1)
                .saturating_sub(1);
            let key = match num {
                "2" => "insert",
                "3" => "delete",
                "5" => "pageUp",
                "6" => "pageDown",
                "7" => "home",
                "8" => "end",
                "11" => "f1",
                "12" => "f2",
                "13" => "f3",
                "14" => "f4",
                "15" => "f5",
                "17" => "f6",
                "18" => "f7",
                "19" => "f8",
                "20" => "f9",
                "21" => "f10",
                "23" => "f11",
                "24" => "f12",
                _ => return None,
            };
            Some(format_key_with_modifier(key, modifier))
        }
        _ => None,
    }
}

pub fn parse_key(data: &str) -> Option<String> {
    if let Some(key) = parse_csi_u_key(data) {
        return Some(key);
    }

    if let Some(key) = parse_modify_other_keys(data) {
        return Some(key);
    }

    if let Some(key) = parse_kitty_functional(data) {
        return Some(key);
    }

    if let Some(key) = legacy_sequence(data) {
        return Some(key.to_string());
    }
    if is_kitty_protocol_active() && (data == "\u{1b}\r" || data == "\n") {
        return Some("shift+enter".to_string());
    }

    Some(match data {
        "\u{1b}" => "escape".to_string(),
        "\t" => "tab".to_string(),
        "\r" => "enter".to_string(),
        "\n" if !is_kitty_protocol_active() => "enter".to_string(),
        " " => "space".to_string(),
        "\u{7f}" | "\u{8}" => "backspace".to_string(),
        "\u{0}" => "ctrl+space".to_string(),
        "\u{1c}" => "ctrl+\\".to_string(),
        "\u{1d}" => "ctrl+]".to_string(),
        "\u{1f}" => "ctrl+-".to_string(),
        "\u{1b}\r" if !is_kitty_protocol_active() => "alt+enter".to_string(),
        "\u{1b} " if !is_kitty_protocol_active() => "alt+space".to_string(),
        "\u{1b}\u{7f}" | "\u{1b}\u{8}" => "alt+backspace".to_string(),
        _ if data.len() == 1 => {
            let ch = data.chars().next()?;
            let code = ch as u32;
            if (1..=26).contains(&code) {
                format!("ctrl+{}", char::from_u32(code + 96)?)
            } else if !ch.is_control() {
                data.to_string()
            } else {
                return None;
            }
        }
        _ if data.len() == 2 && data.starts_with('\u{1b}') && !is_kitty_protocol_active() => {
            let ch = data.chars().nth(1)?;
            let code = ch as u32;
            if (1..=26).contains(&code) {
                format!("ctrl+alt+{}", char::from_u32(code + 96)?)
            } else if ch.is_ascii_alphanumeric() {
                format!("alt+{ch}")
            } else {
                return None;
            }
        }
        _ => return None,
    })
}

pub fn matches_key(data: &str, key_id: &str) -> bool {
    let Some((key, modifier)) = parse_key_id(key_id) else {
        return false;
    };

    if let Some(parsed) = parse_key(data) {
        if let Some((parsed_key, parsed_modifier)) = parse_key_id(&parsed) {
            if parsed_key == key && parsed_modifier == modifier {
                return true;
            }
        }
    }

    if key.len() == 1 && modifier & 4 != 0 {
        if let Some(ctrl) = raw_ctrl_char(&key) {
            if data == ctrl.to_string() && modifier == 4 {
                return true;
            }
            if data == format!("\u{1b}{ctrl}") && modifier == 6 && !is_kitty_protocol_active() {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_keys() {
        assert_eq!(parse_key("\u{1b}[A").as_deref(), Some("up"));
        assert_eq!(parse_key("\u{1b}[1;5D").as_deref(), Some("ctrl+left"));
        assert_eq!(parse_key("\u{1b}[3;1:3~").as_deref(), Some("delete"));
        assert!(matches_key("\u{3}", "ctrl+c"));
    }

    #[test]
    fn parses_kitty_base_layout_and_keypad() {
        assert!(matches_key("\u{1b}[1089::99;5u", "ctrl+c"));
        assert!(matches_key("\u{1b}[1079::112;6u", "ctrl+shift+p"));
        assert_eq!(parse_key("\u{1b}[107;13u").as_deref(), Some("ctrl+super+k"));
        assert_eq!(parse_key("\u{1b}[57400u").as_deref(), Some("1"));
        assert_eq!(parse_key("\u{1b}[57417u").as_deref(), Some("left"));
    }

    #[test]
    fn parses_modify_other_keys() {
        assert!(matches_key("\u{1b}[27;5;99~", "ctrl+c"));
        assert_eq!(parse_key("\u{1b}[27;2;9~").as_deref(), Some("shift+tab"));
        assert_eq!(parse_key("\u{1b}[27;5;13~").as_deref(), Some("ctrl+enter"));
        assert_eq!(
            parse_key("\u{1b}[27;3;127~").as_deref(),
            Some("alt+backspace")
        );
    }

    #[test]
    fn decodes_printable_key_sequences() {
        assert_eq!(
            decode_kitty_printable("\u{1b}[49:33;2u").as_deref(),
            Some("!")
        );
        assert_eq!(decode_kitty_printable("\u{1b}[97;5u"), None);
        assert_eq!(
            decode_printable_key("\u{1b}[27;2;65~").as_deref(),
            Some("A")
        );
        assert_eq!(decode_printable_key("x").as_deref(), Some("x"));
    }
}
