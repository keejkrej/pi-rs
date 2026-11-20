//! ASCII LDH, Punycode A-label, and Unicode U-label validation (RFC 5890 / 5891 / 5892).

use super::bidi;
use super::pattern::{
    has_non_ascii, is_combining_mark, is_permitted_category, is_rfc5892_disallowed, is_script_greek, is_script_hebrew,
    is_script_japanese, is_virama,
};
use super::puny;

pub(crate) fn is_ascii_label(value: &str) -> bool {
    if value.starts_with('-') || value.ends_with('-') {
        return false;
    }
    let bytes = value.as_bytes();
    if bytes.len() >= 4 && bytes[2] == b'-' && bytes[3] == b'-' {
        return false;
    }
    bytes.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

pub(crate) fn is_puny_label(value: &str) -> bool {
    if !puny::is_ace_prefixed(value) {
        return false;
    }
    let body = value[4..].to_ascii_lowercase();
    if body.rfind('-') == Some(0) {
        return false;
    }
    let Some(decoded) = puny::decode(&body) else {
        return false;
    };
    if !has_non_ascii(&decoded) {
        return false;
    }
    is_unicode_label(&decoded)
}

fn exceeds_max_a_label_length(value: &str) -> bool {
    has_non_ascii(value) && puny::encode(value).len() + 4 > 63
}

fn has_invalid_hyphens(chars: &[char]) -> bool {
    if chars.first() == Some(&'-') || chars.last() == Some(&'-') {
        return true;
    }
    chars.len() >= 4 && chars[2] == '-' && chars[3] == '-'
}

pub(crate) fn is_unicode_label(value: &str) -> bool {
    if exceeds_max_a_label_length(value) {
        return false;
    }
    if bidi::has_right_to_left_characters(value) && !bidi::satisfies_bidi_rule(value) {
        return false;
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.is_empty() || has_invalid_hyphens(&chars) || is_combining_mark(chars[0]) {
        return false;
    }

    let mut has_japanese = false;
    for (i, &ch) in chars.iter().enumerate() {
        let cp = ch as u32;
        if is_rfc5892_disallowed(ch) || !is_permitted_category(ch) {
            return false;
        }
        if is_script_japanese(ch) {
            has_japanese = true;
        }
        let prev = i.checked_sub(1).map(|idx| chars[idx]);
        let next = chars.get(i + 1).copied();
        match cp {
            0x00b7 => {
                if prev != Some('l') || next != Some('l') {
                    return false;
                }
            }
            0x0375 => {
                if !next.is_some_and(is_script_greek) {
                    return false;
                }
            }
            0x05f3 | 0x05f4 => {
                if !prev.is_some_and(is_script_hebrew) {
                    return false;
                }
            }
            0x200c => {
                let Some(p) = prev else {
                    return false;
                };
                if (p as u32) < 0x0080 && !is_virama(p) {
                    return false;
                }
            }
            0x200d if !prev.is_some_and(is_virama) => {
                return false;
            }
            _ => {}
        }
    }

    !(value.contains('\u{30fb}') && !has_japanese)
}
