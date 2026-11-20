//! IDNA `hostname` and `idn-hostname` format checks adapted from TypeBox 1.3.27.

mod bidi;
mod label;
mod pattern;
mod puny;

fn is_valid_label_length(value: &str) -> bool {
    let len = pi_js::str16::len(value);
    len > 0 && len <= 63
}

fn is_ascii_or_puny_label(value: &str) -> bool {
    is_valid_label_length(value) && (label::is_puny_label(value) || label::is_ascii_label(value))
}

fn is_idn_label(value: &str) -> bool {
    is_valid_label_length(value) && (label::is_puny_label(value) || label::is_unicode_label(value))
}

/// Validates an ASCII/Punycode hostname (RFC 1123 / 5891 / 5892).
pub fn is_hostname(value: &str) -> bool {
    let len = pi_js::str16::len(value);
    if len == 0 || len > 253 || value.ends_with('.') {
        return false;
    }
    value.split('.').all(is_ascii_or_puny_label)
}

fn normalize_hostname(value: &str) -> String {
    let mapped_width: String = value
        .chars()
        .map(|ch| {
            let cp = ch as u32;
            if (0xff01..=0xff5e).contains(&cp) {
                char::from_u32(cp - 0xfee0).unwrap_or(ch)
            } else {
                ch
            }
        })
        .collect();
    let nfc = pi_js::str16::normalize_nfc(&mapped_width);
    let mut out = String::with_capacity(nfc.len());
    for ch in nfc.chars() {
        let cp = ch as u32;
        if matches!(cp, 0x00ad | 0x034f | 0x180b..=0x180d | 0x200b | 0xfe00..=0xfe0f | 0xe0100..=0xe01ef) {
            continue;
        }
        if matches!(cp, 0x002e | 0x3002 | 0xff0e | 0xff61) {
            out.push('.');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Validates an internationalized domain name (IDN) hostname (RFC 3490 / 5891 / 5892 / 5893).
pub fn is_idn_hostname(value: &str) -> bool {
    if value.is_empty() || value.contains(' ') {
        return false;
    }
    let normalized = normalize_hostname(value);
    if pi_js::str16::len(&normalized) > 253 {
        return false;
    }
    let labels: Vec<&str> = normalized.split('.').collect();
    let has_bidi = labels.iter().any(|l| bidi::has_bidi_chars(l));
    labels
        .iter()
        .all(|l| is_idn_label(l) && (!has_bidi || bidi::satisfies_bidi_rule(l)))
}
