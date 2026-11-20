//! Punycode encoder and decoder (RFC 3492) for IDNA hostname validation.

use super::pattern::is_ascii_only;

const PUNYCODE_BASE: u32 = 36;
const PUNYCODE_TMIN: u32 = 1;
const PUNYCODE_TMAX: u32 = 26;
const PUNYCODE_SKEW: u32 = 38;
const PUNYCODE_DAMP: u32 = 700;
const PUNYCODE_INITIAL_BIAS: u32 = 72;
const PUNYCODE_INITIAL_N: u32 = 128;

pub(crate) fn is_ace_prefixed(value: &str) -> bool {
    value
        .as_bytes()
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"xn--"))
}

fn adapt(mut delta: u32, num_points: u32, first_time: bool) -> u32 {
    delta = if first_time { delta / PUNYCODE_DAMP } else { delta >> 1 };
    delta += delta / num_points;
    let mut k = 0u32;
    while delta > ((PUNYCODE_BASE - PUNYCODE_TMIN) * PUNYCODE_TMAX) >> 1 {
        delta /= PUNYCODE_BASE - PUNYCODE_TMIN;
        k += PUNYCODE_BASE;
    }
    k + ((PUNYCODE_BASE - PUNYCODE_TMIN + 1) * delta) / (delta + PUNYCODE_SKEW)
}

pub(crate) fn decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut output: Vec<u32> = Vec::new();
    let mut n = PUNYCODE_INITIAL_N;
    let mut i: u32 = 0;
    let mut bias = PUNYCODE_INITIAL_BIAS;

    let delim_idx = value.rfind('-');
    if let Some(d) = delim_idx
        && d > 0
    {
        for &b in &bytes[..d] {
            if b >= 128 {
                return None;
            }
            output.push(u32::from(b));
        }
    }

    let mut in_idx = match delim_idx {
        Some(d) => d + 1,
        None => 0,
    };
    while in_idx < bytes.len() {
        let old_i = i;
        let mut w: u32 = 1;
        let mut k = PUNYCODE_BASE;
        loop {
            if in_idx >= bytes.len() {
                return None;
            }
            let ch = bytes[in_idx];
            in_idx += 1;
            let digit = match ch {
                b'a'..=b'z' => u32::from(ch - b'a'),
                b'0'..=b'9' => u32::from(ch - b'0') + 26,
                _ => return None,
            };
            i = i.checked_add(digit.checked_mul(w)?)?;
            let t = if k <= bias {
                PUNYCODE_TMIN
            } else if k >= bias + PUNYCODE_TMAX {
                PUNYCODE_TMAX
            } else {
                k - bias
            };
            if digit < t {
                break;
            }
            w = w.checked_mul(PUNYCODE_BASE - t)?;
            k += PUNYCODE_BASE;
        }
        let out_len = (output.len() + 1) as u32;
        bias = adapt(i - old_i, out_len, old_i == 0);
        n = n.checked_add(i / out_len)?;
        i %= out_len;
        char::from_u32(n)?;
        output.insert(i as usize, n);
        i += 1;
    }

    let mut s = String::with_capacity(output.len());
    for cp in output {
        s.push(char::from_u32(cp)?);
    }
    Some(s)
}

fn digit_to_char(digit: u32) -> char {
    if digit < 26 {
        char::from(b'a' + digit as u8)
    } else {
        char::from(b'0' + (digit - 26) as u8)
    }
}

pub(crate) fn encode(input: &str) -> String {
    if is_ascii_only(input) {
        return input.to_string();
    }
    let mut out = String::new();
    let mut basic_len = 0u32;
    for ch in input.chars() {
        if ch.is_ascii() {
            out.push(ch);
            basic_len += 1;
        }
    }
    if basic_len > 0 {
        out.push('-');
    }

    let code_points: Vec<u32> = input.chars().map(|c| c as u32).collect();
    let mut n = PUNYCODE_INITIAL_N;
    let mut delta: u32 = 0;
    let mut bias = PUNYCODE_INITIAL_BIAS;
    let mut handled = basic_len;
    let total = code_points.len() as u32;

    while handled < total {
        let mut m = u32::MAX;
        for &cp in &code_points {
            if cp >= n && cp < m {
                m = cp;
            }
        }
        delta = delta.saturating_add((m - n).saturating_mul(handled + 1));
        n = m;
        for &cp in &code_points {
            if cp < n {
                delta = delta.saturating_add(1);
            }
            if cp == n {
                let mut q = delta;
                let mut k = PUNYCODE_BASE;
                loop {
                    let t = if k <= bias {
                        PUNYCODE_TMIN
                    } else if k >= bias + PUNYCODE_TMAX {
                        PUNYCODE_TMAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    out.push(digit_to_char(t + ((q - t) % (PUNYCODE_BASE - t))));
                    q = (q - t) / (PUNYCODE_BASE - t);
                    k += PUNYCODE_BASE;
                }
                out.push(digit_to_char(q));
                bias = adapt(delta, handled + 1, handled == basic_len);
                delta = 0;
                handled += 1;
            }
        }
        delta += 1;
        n += 1;
    }

    out
}
