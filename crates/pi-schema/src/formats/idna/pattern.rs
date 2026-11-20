//! Unicode property and character class predicates for IDNA validation.

use std::sync::OnceLock;

use regress::Regex;

fn compile_u(pattern: &str) -> Regex {
    pi_js::regex::ecma(pattern, "u").unwrap_or_else(|e| panic!("idna pattern error: {e}"))
}

fn test_char(re: &Regex, ch: char) -> bool {
    let mut buf = [0u8; 4];
    let s = ch.encode_utf8(&mut buf);
    pi_js::regex::test(re, s)
}

pub(crate) fn is_ascii_only(s: &str) -> bool {
    s.is_ascii()
}

pub(crate) fn has_non_ascii(s: &str) -> bool {
    !s.is_ascii()
}

pub(crate) fn is_combining_mark(ch: char) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| compile_u(r"[\p{Mn}\p{Mc}\p{Me}]"));
    test_char(re, ch)
}

pub(crate) fn is_mark_nonspacing(ch: char) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| compile_u(r"\p{Mn}"));
    test_char(re, ch)
}

pub(crate) fn is_letter(ch: char) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| compile_u(r"\p{L}"));
    test_char(re, ch)
}

pub(crate) fn is_script_greek(ch: char) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| compile_u(r"\p{Script=Greek}"));
    test_char(re, ch)
}

pub(crate) fn is_script_hebrew(ch: char) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| compile_u(r"\p{Script=Hebrew}"));
    test_char(re, ch)
}

pub(crate) fn is_script_japanese(ch: char) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| compile_u(r"[\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Han}]"));
    test_char(re, ch)
}

pub(crate) fn is_script_arabic_letter(ch: char) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| compile_u(r"[\p{Script=Arabic}\p{Script=Syriac}\p{Script=Thaana}\p{Script=Mandaic}]"));
    test_char(re, ch)
}

pub(crate) fn is_european_number(ch: char) -> bool {
    ch.is_ascii_digit() || ('\u{06F0}'..='\u{06F9}').contains(&ch)
}

pub(crate) fn is_arabic_indic_digit(ch: char) -> bool {
    ('\u{0660}'..='\u{0669}').contains(&ch)
}

pub(crate) fn is_virama(ch: char) -> bool {
    matches!(
        ch as u32,
        0x094d
            | 0x09cd
            | 0x0a4d
            | 0x0acd
            | 0x0b4d
            | 0x0bcd
            | 0x0c4d
            | 0x0ccd
            | 0x0d3b
            | 0x0d3c
            | 0x0d4d
            | 0x0dca
            | 0x1b44
            | 0x1baa
            | 0x1bab
            | 0xa9c0
            | 0x11046
            | 0x1107f
            | 0x110b9
            | 0x11133
            | 0x11134
            | 0x111c0
            | 0x11235
            | 0x1134d
            | 0x11442
            | 0x114c2
            | 0x115bf
            | 0x1163f
            | 0x116b6
            | 0x11c3f
            | 0x11d44
            | 0x11d45
    )
}

pub(crate) fn is_rfc5892_disallowed(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0640 | 0x07fa | 0x302e | 0x302f | 0x3031 | 0x3032 | 0x3033 | 0x3034 | 0x3035 | 0x303b
    )
}

pub(crate) fn is_permitted_category(ch: char) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        compile_u(
            r"\p{L}|[\u{002d}\u{002b}]|[\u{002e}\u{002c}\u{003a}\u{002f}]|\p{Nd}|\p{Mn}|\p{Mc}|[\u{00b7}\u{0375}\u{05f3}\u{05f4}\u{200c}\u{200d}\u{30fb}]|[\u{00df}\u{03c2}\u{06fd}\u{06fe}\u{0f0b}\u{3007}]",
        )
    });
    test_char(re, ch)
}
