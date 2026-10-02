//! TypeBox 1.3.27 default string formats used by pi validation.
//! Unregistered names accept every string, matching `Format.Test`.

use std::sync::OnceLock;

use regress::Regex;

include!("format_patterns.rs");

pub(crate) fn test(format: &str, value: &str) -> bool {
    match format {
        "date-time" => is_date_time(value),
        "date" => is_date(value),
        "time" => is_time(value),
        "duration" => matches_re(duration_re(), value),
        "email" => matches_re(email_re(), value),
        "uuid" => matches_re(uuid_re(), value),
        "ipv4" => matches_re(ipv4_re(), value),
        "ipv6" => matches_re(ipv6_re(), value),
        "uri" => matches_re(uri_re(), value),
        "uri-reference" => matches_re(uri_reference_re(), value),
        "uri-template" => matches_re(uri_template_re(), value),
        "json-pointer" => matches_re(json_pointer_re(), value),
        "relative-json-pointer" => matches_re(relative_json_pointer_re(), value),
        "json-pointer-uri-fragment" => matches_re(json_pointer_uri_fragment_re(), value),
        "regex" => crate::regex::ecma(value, "u").is_ok(),
        _ => true,
    }
}

fn matches_re(re: &Regex, value: &str) -> bool {
    crate::regex::test(re, value)
}

fn compile(pattern: &str, flags: &str) -> Regex {
    crate::regex::ecma(pattern, flags).unwrap_or_else(|e| panic!("typebox format regex: {e}"))
}

fn email_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(EMAIL, EMAIL_FLAGS))
}
fn uuid_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(UUID, UUID_FLAGS))
}
fn ipv4_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(IPV4, IPV4_FLAGS))
}
fn ipv6_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(IPV6, IPV6_FLAGS))
}
fn uri_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(URI, URI_FLAGS))
}
fn uri_reference_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(URI_REFERENCE, URI_REFERENCE_FLAGS))
}
fn uri_template_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(URI_TEMPLATE, URI_TEMPLATE_FLAGS))
}
fn duration_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(DURATION, DURATION_FLAGS))
}
fn json_pointer_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(JSON_POINTER, JSON_POINTER_FLAGS))
}
fn relative_json_pointer_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(RELATIVE_JSON_POINTER, RELATIVE_JSON_POINTER_FLAGS))
}
fn json_pointer_uri_fragment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(JSON_POINTER_URI_FRAGMENT, JSON_POINTER_URI_FRAGMENT_FLAGS))
}
fn time_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(TIME, TIME_FLAGS))
}

fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn is_date(value: &str) -> bool {
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
    let days = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let max = if month == 2 && is_leap(year) {
        29
    } else {
        days[month as usize]
    };
    (1..=max).contains(&day)
}

/// ISO time with a required timezone (`strictTimeZone` default).
fn is_time(value: &str) -> bool {
    let Some(m) = crate::regex::exec(time_re(), value) else {
        return false;
    };
    let g = |i: usize| m.captures.get(i).and_then(|c| c.as_deref());
    if g(3).is_none() && g(4).is_none() {
        return false;
    }
    let hr: f64 = g(0).unwrap_or("0").parse().unwrap_or(99.0);
    let min: f64 = g(1).unwrap_or("0").parse().unwrap_or(99.0);
    let sec: f64 = g(2).unwrap_or("0").parse().unwrap_or(99.0);
    if hr > 23.0 || min > 59.0 || sec > 60.0 {
        return false;
    }
    if g(4).is_some() {
        let tzh: f64 = g(5).unwrap_or("0").parse().unwrap_or(99.0);
        let tzm: f64 = g(6).unwrap_or("0").parse().unwrap_or(99.0);
        if tzh > 23.0 || tzm > 59.0 {
            return false;
        }
    }
    if sec < 60.0 {
        return true;
    }
    let tz_sign = if g(4) == Some("-") { -1.0 } else { 1.0 };
    let tzh: f64 = g(5).unwrap_or("0").parse().unwrap_or(0.0);
    let tzm: f64 = g(6).unwrap_or("0").parse().unwrap_or(0.0);
    let total = (hr * 60.0 + min) - tz_sign * (tzh * 60.0 + tzm);
    let norm = (total % 1440.0 + 1440.0) % 1440.0;
    norm == 1439.0
}

fn is_date_time(value: &str) -> bool {
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, c) in value.char_indices() {
        if c == 'T' || c == 't' {
            parts.push(&value[start..i]);
            start = i + c.len_utf8();
        }
    }
    parts.push(&value[start..]);
    parts.len() == 2 && is_date(parts[0]) && is_time(parts[1])
}
