//! RFC 5893 Bidi rule checks for IDNA labels.

use super::pattern::{
    is_arabic_indic_digit, is_european_number, is_letter, is_mark_nonspacing, is_script_arabic_letter, is_script_hebrew,
};
use super::puny;

#[derive(Clone, Copy, PartialEq, Eq)]
enum BidiClass {
    L,
    R,
    Al,
    An,
    En,
    Nsm,
    On,
}

fn get_bidi_class(ch: char) -> BidiClass {
    if is_european_number(ch) {
        BidiClass::En
    } else if is_arabic_indic_digit(ch) {
        BidiClass::An
    } else if is_mark_nonspacing(ch) {
        BidiClass::Nsm
    } else if is_script_hebrew(ch) {
        BidiClass::R
    } else if is_script_arabic_letter(ch) {
        BidiClass::Al
    } else if is_letter(ch) {
        BidiClass::L
    } else {
        BidiClass::On
    }
}

pub(crate) fn has_bidi_chars(value: &str) -> bool {
    if puny::is_ace_prefixed(value) {
        let body = value[4..].to_ascii_lowercase();
        match puny::decode(&body) {
            Some(decoded) => has_right_to_left_characters(&decoded),
            None => false,
        }
    } else {
        has_right_to_left_characters(value)
    }
}

pub(crate) fn has_right_to_left_characters(value: &str) -> bool {
    value
        .chars()
        .any(|ch| matches!(get_bidi_class(ch), BidiClass::R | BidiClass::Al | BidiClass::An))
}

pub(crate) fn satisfies_bidi_rule(value: &str) -> bool {
    let mut is_rtl = false;
    let mut saw_en = false;
    let mut saw_an = false;
    let mut is_first = true;

    for ch in value.chars() {
        let cls = get_bidi_class(ch);
        if is_first {
            match cls {
                BidiClass::L => is_rtl = false,
                BidiClass::R | BidiClass::Al => is_rtl = true,
                _ => return false,
            }
            is_first = false;
        }
        let allowed = if is_rtl {
            matches!(
                cls,
                BidiClass::R | BidiClass::Al | BidiClass::An | BidiClass::En | BidiClass::On | BidiClass::Nsm
            )
        } else {
            matches!(cls, BidiClass::L | BidiClass::En | BidiClass::On | BidiClass::Nsm)
        };
        if !allowed {
            return false;
        }
        match cls {
            BidiClass::En => saw_en = true,
            BidiClass::An => saw_an = true,
            _ => {}
        }
    }

    !(is_rtl && saw_en && saw_an)
}
