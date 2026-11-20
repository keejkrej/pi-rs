//! JSON Schema 2020-12 standard string format validators (adapted from TypeBox 1.3.27).
//!
//! Includes all 20 standard JSON Schema formats plus `url` and `json-pointer-uri-fragment`.

pub mod idna;

use std::sync::OnceLock;

use indexmap::IndexMap;
use regress::Regex;

use crate::types::FormatCheck;

pub use idna::{is_hostname, is_idn_hostname};

fn compile_re(pattern: &str, flags: &str) -> Regex {
    pi_js::regex::ecma(pattern, flags).unwrap_or_else(|e| panic!("pi-schema format regex: {e}"))
}

fn matches_re(re: &Regex, value: &str) -> bool {
    pi_js::regex::test(re, value)
}

/// Returns a map containing every format validator supported by `@earendil-works/pi-schema`.
pub fn formats() -> IndexMap<String, FormatCheck> {
    let mut map = IndexMap::new();
    map.insert("date".to_string(), FormatCheck::from_fn(is_date));
    map.insert("date-time".to_string(), FormatCheck::from_fn(is_date_time));
    map.insert("duration".to_string(), FormatCheck::from_fn(is_duration));
    map.insert("email".to_string(), FormatCheck::from_fn(is_email));
    map.insert("hostname".to_string(), FormatCheck::from_fn(is_hostname));
    map.insert("idn-email".to_string(), FormatCheck::from_fn(is_idn_email));
    map.insert("idn-hostname".to_string(), FormatCheck::from_fn(is_idn_hostname));
    map.insert("ipv4".to_string(), FormatCheck::from_fn(is_ipv4));
    map.insert("ipv6".to_string(), FormatCheck::from_fn(is_ipv6));
    map.insert("iri".to_string(), FormatCheck::from_fn(is_iri));
    map.insert("iri-reference".to_string(), FormatCheck::from_fn(is_iri_reference));
    map.insert("json-pointer".to_string(), FormatCheck::from_fn(is_json_pointer));
    map.insert(
        "json-pointer-uri-fragment".to_string(),
        FormatCheck::from_fn(is_json_pointer_uri_fragment),
    );
    map.insert("regex".to_string(), FormatCheck::from_fn(is_regex));
    map.insert(
        "relative-json-pointer".to_string(),
        FormatCheck::from_fn(is_relative_json_pointer),
    );
    map.insert("time".to_string(), FormatCheck::from_fn(is_time));
    map.insert("uri".to_string(), FormatCheck::from_fn(is_uri));
    map.insert("uri-reference".to_string(), FormatCheck::from_fn(is_uri_reference));
    map.insert("uri-template".to_string(), FormatCheck::from_fn(is_uri_template));
    map.insert("url".to_string(), FormatCheck::from_fn(is_url));
    map.insert("uuid".to_string(), FormatCheck::from_fn(is_uuid));
    map
}

/// Looks up a built-in format validator by name.
pub fn get(name: &str) -> Option<FormatCheck> {
    match name {
        "date" => Some(FormatCheck::from_fn(is_date)),
        "date-time" => Some(FormatCheck::from_fn(is_date_time)),
        "duration" => Some(FormatCheck::from_fn(is_duration)),
        "email" => Some(FormatCheck::from_fn(is_email)),
        "hostname" => Some(FormatCheck::from_fn(is_hostname)),
        "idn-email" => Some(FormatCheck::from_fn(is_idn_email)),
        "idn-hostname" => Some(FormatCheck::from_fn(is_idn_hostname)),
        "ipv4" => Some(FormatCheck::from_fn(is_ipv4)),
        "ipv6" => Some(FormatCheck::from_fn(is_ipv6)),
        "iri" => Some(FormatCheck::from_fn(is_iri)),
        "iri-reference" => Some(FormatCheck::from_fn(is_iri_reference)),
        "json-pointer" => Some(FormatCheck::from_fn(is_json_pointer)),
        "json-pointer-uri-fragment" => Some(FormatCheck::from_fn(is_json_pointer_uri_fragment)),
        "regex" => Some(FormatCheck::from_fn(is_regex)),
        "relative-json-pointer" => Some(FormatCheck::from_fn(is_relative_json_pointer)),
        "time" => Some(FormatCheck::from_fn(is_time)),
        "uri" => Some(FormatCheck::from_fn(is_uri)),
        "uri-reference" => Some(FormatCheck::from_fn(is_uri_reference)),
        "uri-template" => Some(FormatCheck::from_fn(is_uri_template)),
        "url" => Some(FormatCheck::from_fn(is_url)),
        "uuid" => Some(FormatCheck::from_fn(is_uuid)),
        _ => None,
    }
}

fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// RFC 3339 full-date (`YYYY-MM-DD`).
pub fn is_date(value: &str) -> bool {
    let b = value.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    if !b
        .iter()
        .enumerate()
        .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return false;
    }
    let Ok(year) = value[0..4].parse::<i32>() else {
        return false;
    };
    let Ok(month) = value[5..7].parse::<i32>() else {
        return false;
    };
    let Ok(day) = value[8..10].parse::<i32>() else {
        return false;
    };
    if !(1..=12).contains(&month) {
        return false;
    }
    const DAYS: [i32; 13] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let max_day = if month == 2 && is_leap_year(year) {
        29
    } else {
        DAYS[month as usize]
    };
    (1..=max_day).contains(&day)
}

/// RFC 3339 full-time with required timezone offset (`Z`/`z` or `±HH:MM`).
pub fn is_time(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| compile_re(r"^(\d\d):(\d\d):(\d\d)(?:\.\d+)?(?:([Zz])|([+-])(\d\d):(\d\d))?$", ""));
    let Some(m) = pi_js::regex::exec(re, value) else {
        return false;
    };
    let cap = |i: usize| m.captures.get(i).and_then(|c| c.as_deref());
    if cap(3).is_none() && cap(4).is_none() {
        return false;
    }
    let hr: i32 = cap(0).unwrap_or("0").parse().unwrap_or(99);
    let min: i32 = cap(1).unwrap_or("0").parse().unwrap_or(99);
    let sec: i32 = cap(2).unwrap_or("0").parse().unwrap_or(99);
    if hr > 23 || min > 59 || sec > 60 {
        return false;
    }
    if cap(4).is_some() {
        let tzh: i32 = cap(5).unwrap_or("0").parse().unwrap_or(99);
        let tzm: i32 = cap(6).unwrap_or("0").parse().unwrap_or(99);
        if tzh > 23 || tzm > 59 {
            return false;
        }
    }
    if sec < 60 {
        return true;
    }
    let tz_sign = if cap(4) == Some("-") { -1 } else { 1 };
    let tzh: i32 = cap(5).unwrap_or("0").parse().unwrap_or(0);
    let tzm: i32 = cap(6).unwrap_or("0").parse().unwrap_or(0);
    let total_utc_min = hr * 60 + min - tz_sign * (tzh * 60 + tzm);
    total_utc_min.rem_euclid(1440) == 1439
}

/// RFC 3339 date-time (`YYYY-MM-DDTHH:MM:SS...`).
pub fn is_date_time(value: &str) -> bool {
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, ch) in value.char_indices() {
        if ch == 'T' || ch == 't' {
            parts.push(&value[start..i]);
            start = i + ch.len_utf8();
        }
    }
    parts.push(&value[start..]);
    parts.len() == 2 && is_date(parts[0]) && is_time(parts[1])
}

/// RFC 3339 / ISO-8601 duration.
pub fn is_duration(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        compile_re(
            r"^P((\d+Y(\d+M(\d+D)?)?|\d+M(\d+D)?|\d+D)(T(\d+H(\d+M(\d+S)?)?|\d+M(\d+S)?|\d+S))?|T(\d+H(\d+M(\d+S)?)?|\d+M(\d+S)?|\d+S)|\d+W)$",
            "",
        )
    });
    matches_re(re, value)
}

/// RFC 5322 email address.
pub fn is_email(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        compile_re(
            r###"^(?:[a-z0-9!#$%&'*+/=?^_`{|}~-]+(?:\.[a-z0-9!#$%&'*+/=?^_`{|}~-]+)*|"(?:[^"\\]|\\[\x20-\x7e])*")@(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)*|\[(?:IPv6:[a-f0-9:]+|(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])(?:\.(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])){3})\])$"###,
            "i",
        )
    });
    matches_re(re, value)
}

/// RFC 4952 / RFC 6531 internationalized email address.
pub fn is_idn_email(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        compile_re(
            r###"^(?:[A-Za-z0-9!#$%&'*+/=?^_`{|}~\u{0080}-\u{10FFFF}-]+(?:\.[A-Za-z0-9!#$%&'*+/=?^_`{|}~\u{0080}-\u{10FFFF}-]+)*|"(?:[^"\\]|\\.)*")@[\p{L}\p{N}](?:[\p{L}\p{N}-]{0,62})(?<!-)(?:\.[\p{L}\p{N}](?:[\p{L}\p{N}-]{0,62})(?<!-))*$"###,
            "iu",
        )
    });
    let normalized = pi_js::str16::normalize_nfc(value);
    matches_re(re, &normalized)
}

/// RFC 2673 IPv4 address.
pub fn is_ipv4(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        compile_re(
            r"^(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)$",
            "",
        )
    });
    matches_re(re, value)
}

/// RFC 2373 / RFC 4291 IPv6 address.
pub fn is_ipv6(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        compile_re(
            r"^(?:(?:(?:[0-9a-f]{1,4}:){6}|::(?:[0-9a-f]{1,4}:){5}|(?:[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){4}|(?:(?:[0-9a-f]{1,4}:)?[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){3}|(?:(?:[0-9a-f]{1,4}:){0,2}[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){2}|(?:(?:[0-9a-f]{1,4}:){0,3}[0-9a-f]{1,4})?::[0-9a-f]{1,4}:|(?:(?:[0-9a-f]{1,4}:){0,4}[0-9a-f]{1,4})?::)(?:[0-9a-f]{1,4}:[0-9a-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[0-9a-f]{1,4}:){0,5}[0-9a-f]{1,4})?::[0-9a-f]{1,4}|(?:(?:[0-9a-f]{1,4}:){0,6}[0-9a-f]{1,4})?::)$",
            "i",
        )
    });
    matches_re(re, value)
}

/// RFC 3987 Internationalized Resource Identifier (IRI).
pub fn is_iri(value: &str) -> bool {
    static INVALID_CHARS: OnceLock<Regex> = OnceLock::new();
    static INVALID_PCT: OnceLock<Regex> = OnceLock::new();
    static IPV_FUTURE: OnceLock<Regex> = OnceLock::new();

    let invalid_chars = INVALID_CHARS.get_or_init(|| compile_re(r"[\x00-\x20<>^`{|}\\]", ""));
    if matches_re(invalid_chars, value) {
        return false;
    }
    let invalid_pct = INVALID_PCT.get_or_init(|| compile_re(r"%(?![0-9a-fA-F]{2})", ""));
    if matches_re(invalid_pct, value) {
        return false;
    }
    if pi_js::str16::len(value) < 2048 {
        let ipv_future = IPV_FUTURE.get_or_init(|| compile_re(r"\[[vV][0-9a-fA-F]+\.[^\]]+\]", ""));
        let narrowed = pi_js::regex::replace(ipv_future, value, "[::1]", false);
        url::Url::parse(&narrowed).is_ok()
    } else {
        url::Url::parse(value).is_ok()
    }
}

/// RFC 3987 IRI reference.
pub fn is_iri_reference(value: &str) -> bool {
    static INVALID_CHARS: OnceLock<Regex> = OnceLock::new();
    static MALFORMED_SCHEME: OnceLock<Regex> = OnceLock::new();

    let invalid_chars = INVALID_CHARS.get_or_init(|| compile_re(r"[\x00-\x20\x7F\\]|%(?![0-9a-fA-F]{2})", ""));
    if matches_re(invalid_chars, value) {
        return false;
    }
    let malformed_scheme = MALFORMED_SCHEME.get_or_init(|| compile_re(r"^[a-zA-Z][a-zA-Z0-9+\-.]*//", ""));
    if matches_re(malformed_scheme, value) {
        return false;
    }
    let Ok(base) = url::Url::parse("http://example.com") else {
        return false;
    };
    base.join(value).is_ok()
}

/// RFC 6901 JSON Pointer.
pub fn is_json_pointer(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| compile_re(r"^(?:/(?:[^~/]|~0|~1)*)*$", ""));
    matches_re(re, value)
}

/// RFC 6901 JSON Pointer URI fragment.
pub fn is_json_pointer_uri_fragment(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| compile_re(r"^#(?:/(?:[a-z0-9_\-.!$&'()*+,;:=@]|%[0-9a-f]{2}|~0|~1)*)*$", "i"));
    matches_re(re, value)
}

/// ECMA-262 regular expression pattern (`u` flag).
pub fn is_regex(value: &str) -> bool {
    pi_js::regex::ecma(value, "u").is_ok()
}

/// Relative JSON Pointer.
pub fn is_relative_json_pointer(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| compile_re(r"^(?:0|[1-9][0-9]*)(?:#|(?:/(?:[^~/]|~0|~1)*)*)$", ""));
    matches_re(re, value)
}

/// RFC 3986 Uniform Resource Identifier (URI).
pub fn is_uri(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        compile_re(
            r"^[a-z][a-z0-9+\-.]*:(?://(?:(?:[-a-z0-9._~!$&'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\[(?:(?:(?:[\da-f]{1,4}:){6}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|::(?:[\da-f]{1,4}:){5}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:[\da-f]{1,4})?::(?:[\da-f]{1,4}:){4}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,1}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){3}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,2}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){2}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,3}[\da-f]{1,4})?::[\da-f]{1,4}:(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,4}[\da-f]{1,4})?::(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,5}[\da-f]{1,4})?::[\da-f]{1,4}|(?:(?:[\da-f]{1,4}:){0,6}[\da-f]{1,4})?::)|v[0-9a-f]+\.[-a-z0-9._~!$&'()*+,;=:]+)\]|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)|(?:[-a-z0-9._~!$&'()*+,;=]|%[0-9a-f]{2})*)(?::\d*)?(?:/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*|/(?:(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?(?:\?(?:[-a-z0-9._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?(?:#(?:[-a-z0-9._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?$",
            "i",
        )
    });
    matches_re(re, value)
}

/// RFC 3986 URI reference.
pub fn is_uri_reference(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        compile_re(
            r"^(?:[a-z][a-z0-9+\-.]*:(?://(?:(?:[-a-z0-9._~!$&'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\[(?:(?:(?:[\da-f]{1,4}:){6}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|::(?:[\da-f]{1,4}:){5}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:[\da-f]{1,4})?::(?:[\da-f]{1,4}:){4}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,1}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){3}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,2}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){2}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,3}[\da-f]{1,4})?::[\da-f]{1,4}:(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,4}[\da-f]{1,4})?::(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,5}[\da-f]{1,4})?::[\da-f]{1,4}|(?:(?:[\da-f]{1,4}:){0,6}[\da-f]{1,4})?::)|v[0-9a-f]+\.[-a-z0-9._~!$&'()*+,;=:]+)\]|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)|(?:[-a-z0-9._~!$&'()*+,;=]|%[0-9a-f]{2})*)(?::\d*)?(?:/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*|/(?:(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?://(?:(?:[-a-z0-9._~!$&'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\[(?:(?:(?:[\da-f]{1,4}:){6}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|::(?:[\da-f]{1,4}:){5}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:[\da-f]{1,4})?::(?:[\da-f]{1,4}:){4}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,1}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){3}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,2}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){2}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,3}[\da-f]{1,4})?::[\da-f]{1,4}:(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,4}[\da-f]{1,4})?::(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,5}[\da-f]{1,4})?::[\da-f]{1,4}|(?:(?:[\da-f]{1,4}:){0,6}[\da-f]{1,4})?::)|v[0-9a-f]+\.[-a-z0-9._~!$&'()*+,;=:]+)\]|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)|(?:[-a-z0-9._~!$&'()*+,;=]|%[0-9a-f]{2})*)(?::\d*)?(?:/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*|/(?:(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:[-a-z0-9._~!$&'()*+,;=@]|%[0-9a-f]{2})+(?:/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?)(?:\?(?:[-a-z0-9._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?(?:#(?:[-a-z0-9._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?$",
            "i",
        )
    });
    matches_re(re, value)
}

/// RFC 6570 URI template.
pub fn is_uri_template(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        compile_re(
            r###"^(?:(?:[^\x00-\x20"<>%\\^`{|}\x7f]|%[0-9a-f]{2})|\{[+#./;?&=,!@|]?(?:[a-z0-9_]|%[0-9a-f]{2})+(?:\.(?:[a-z0-9_]|%[0-9a-f]{2})+)*(?::[1-9]\d{0,3}|\*)?(?:,(?:[a-z0-9_]|%[0-9a-f]{2})+(?:\.(?:[a-z0-9_]|%[0-9a-f]{2})+)*(?::[1-9]\d{0,3}|\*)?)*\})*$"###,
            "i",
        )
    });
    matches_re(re, value)
}

/// WHATWG URL (`URL.canParse`).
pub fn is_url(value: &str) -> bool {
    url::Url::parse(value).is_ok()
}

/// RFC 4122 / RFC 9562 UUID.
pub fn is_uuid(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| compile_re(r"^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$", "i"));
    matches_re(re, value)
}
