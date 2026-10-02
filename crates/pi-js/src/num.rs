//! pi_js::num (Rust-only; API contract: PORTING.md Appendix A).
//!
//! JS `Number` semantics: `Number.prototype.toString` / `toFixed` / `toPrecision`, `parseInt`,
//! `parseFloat`, `Number(string)`, `Math.round`, `Math.imul`, `ToInt32` / `ToUint32`.
//! Parsing and rounding follow V8 (Node 24) bit for bit.

use crate::str16;

/// `Number.MAX_SAFE_INTEGER`.
pub const MAX_SAFE_INTEGER: i64 = 9007199254740991;

/// `Number.MIN_SAFE_INTEGER`.
pub const MIN_SAFE_INTEGER: i64 = -9007199254740991;

/// `Number.prototype.toString()` / `String(x)`: shortest round-trip digits, `1e+21` style
/// exponents, `-0` prints as `0`, non-finite values print as `NaN` / `Infinity` / `-Infinity`.
pub fn to_js_string(x: f64) -> String {
    ryu_js::Buffer::new().format(x).to_string()
}

/// `x.toFixed(digits)`. Ties round away from zero on the exact binary value
/// (`(1.25).toFixed(1)` is `"1.3"`, `(1.005).toFixed(2)` is `"1.00"`); `|x| >= 1e21` falls back
/// to `toString()`.
///
/// JS throws a `RangeError` for `digits > 100`; here `digits` is clamped to 100.
pub fn to_fixed(x: f64, digits: u32) -> String {
    let digits = digits.min(100) as u8;
    ryu_js::Buffer::new().format_to_fixed(x, digits).to_string()
}

/// `x.toString(radix)` (port of V8 `DoubleToRadixCString`): integer digits, then fraction
/// digits only up to the precision of `x` (`(0.5).toString(2)` is `"0.1"`,
/// `(255).toString(16)` is `"ff"`). Radix 10 is [`to_js_string`].
///
/// JS throws a `RangeError` for a radix outside `2..=36`; here it is clamped to that range.
pub fn to_string_radix(x: f64, radix: u32) -> String {
    const CHARS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let radix = radix.clamp(2, 36);
    if radix == 10 || !x.is_finite() || x == 0.0 {
        return to_js_string(x);
    }
    let rf = f64::from(radix);
    let negative = x < 0.0;
    let value = x.abs();
    let mut integer = value.floor();
    let mut fraction = value - integer;
    // Only compute fraction digits up to the input double's precision.
    let next = f64::from_bits(value.to_bits() + 1);
    let mut delta = (0.5 * (next - value)).max(f64::from_bits(1));
    let mut frac_digits: Vec<u8> = Vec::new();
    if fraction >= delta {
        loop {
            // Shift up by one digit.
            fraction *= rf;
            delta *= rf;
            let digit = fraction as usize;
            frac_digits.push(CHARS[digit]);
            fraction -= digit as f64;
            // Round to even.
            if (fraction > 0.5 || (fraction == 0.5 && digit & 1 == 1)) && fraction + delta > 1.0 {
                // Carry into the digits already written.
                loop {
                    let Some(c) = frac_digits.pop() else {
                        // Carry over into the integer part.
                        integer += 1.0;
                        break;
                    };
                    let d = digit_value(c) as usize;
                    if d + 1 < radix as usize {
                        frac_digits.push(CHARS[d + 1]);
                        break;
                    }
                }
                break;
            }
            if fraction < delta {
                break;
            }
        }
    }
    // Integer digits, least significant first. Digits below the double's precision are 0.
    let mut int_digits: Vec<u8> = Vec::new();
    // V8: `Double(integer / radix).Exponent() > 0`, i.e. `integer / radix >= 2^53`.
    while integer / rf >= 9007199254740992.0 {
        integer /= rf;
        int_digits.push(b'0');
    }
    loop {
        let remainder = integer % rf;
        int_digits.push(CHARS[remainder as usize]);
        integer = (integer - remainder) / rf;
        if integer <= 0.0 {
            break;
        }
    }
    let mut out = String::with_capacity(int_digits.len() + frac_digits.len() + 2);
    if negative {
        out.push('-');
    }
    out.extend(int_digits.iter().rev().map(|&b| char::from(b)));
    if !frac_digits.is_empty() {
        out.push('.');
        out.extend(frac_digits.iter().map(|&b| char::from(b)));
    }
    out
}

/// Exact decimal expansion of a positive finite `x`: significant digits (no trailing zeros) and
/// the exponent `e` such that `x = d0.d1d2… × 10^e`.
fn exact_digits(x: f64) -> (Vec<u8>, i32) {
    // A double has at most 767 significant decimal digits, so 800 fractional digits in
    // scientific notation are exact (Rust only rounds beyond the exact expansion).
    let s = format!("{:.800e}", x);
    let (mantissa, exp) = s.split_once('e').unwrap_or((s.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let mut digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).collect();
    while digits.len() > 1 && digits.last() == Some(&b'0') {
        digits.pop();
    }
    (digits, exp)
}

/// `x.toPrecision(precision)`. Ties round up on the exact binary value.
///
/// JS throws a `RangeError` for precision outside `1..=100`; here it is clamped to that range.
pub fn to_precision(x: f64, precision: u32) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    let mut s = String::new();
    let mut x = x;
    if x < 0.0 {
        s.push('-');
        x = -x;
    }
    if x.is_infinite() {
        s.push_str("Infinity");
        return s;
    }
    let p = precision.clamp(1, 100) as usize;
    let (m, e): (Vec<u8>, i32) = if x == 0.0 {
        (vec![b'0'; p], 0)
    } else {
        let (digits, mut e) = exact_digits(x);
        let mut n: Vec<u8> = digits.iter().copied().take(p).collect();
        n.resize(p, b'0');
        if digits.len() > p && digits[p] >= b'5' {
            // Round the p-digit prefix up; a carry out of the first digit bumps the exponent.
            let mut i = p;
            loop {
                if i == 0 {
                    n.insert(0, b'1');
                    n.truncate(p);
                    e += 1;
                    break;
                }
                i -= 1;
                if n[i] == b'9' {
                    n[i] = b'0';
                } else {
                    n[i] += 1;
                    break;
                }
            }
        }
        (n, e)
    };
    let m = String::from_utf8(m).unwrap_or_default();
    if x != 0.0 && (e < -6 || e >= p as i32) {
        let mut out = s;
        out.push_str(&m[..1]);
        if p != 1 {
            out.push('.');
            out.push_str(&m[1..]);
        }
        out.push('e');
        out.push(if e > 0 { '+' } else { '-' });
        out.push_str(&e.unsigned_abs().to_string());
        return out;
    }
    if e == p as i32 - 1 {
        s.push_str(&m);
        return s;
    }
    if e >= 0 {
        let k = e as usize + 1;
        s.push_str(&m[..k]);
        s.push('.');
        s.push_str(&m[k..]);
    } else {
        s.push_str("0.");
        for _ in 0..(-(e + 1)) {
            s.push('0');
        }
        s.push_str(&m);
    }
    s
}

/// Value of an ASCII radix digit (`0-9`, `a-z`, `A-Z`), or 99 for anything else.
fn digit_value(b: u8) -> u32 {
    match b {
        b'0'..=b'9' => u32::from(b - b'0'),
        b'a'..=b'z' => u32::from(b - b'a') + 10,
        b'A'..=b'Z' => u32::from(b - b'A') + 10,
        _ => 99,
    }
}

fn signed_zero(negative: bool) -> f64 {
    if negative { -0.0 } else { 0.0 }
}

/// `std::ldexp` for the values produced below (exponent >= 0).
fn ldexp(v: f64, mut exponent: i32) -> f64 {
    let mut v = v;
    while exponent > 0 {
        let step = exponent.min(1000);
        v *= 2f64.powi(step);
        exponent -= step;
    }
    v
}

/// Port of V8 `InternalStringToIntDouble<radix_log_2>`: exact (round-half-even) conversion of
/// a run of power-of-two radix digits. `digits` holds only valid digits.
fn power_of_two_radix_to_double(digits: &[u8], radix_log_2: u32, negative: bool) -> f64 {
    let radix = 1i64 << radix_log_2;
    let mut i = 0;
    // Skip leading 0s.
    while i < digits.len() && digits[i] == b'0' {
        i += 1;
    }
    if i == digits.len() {
        return signed_zero(negative);
    }
    let mut number: i64 = 0;
    let mut exponent: i32 = 0;
    while i < digits.len() {
        let digit = i64::from(digit_value(digits[i]));
        number = number * radix + digit;
        let mut overflow = (number >> 53) as i32;
        if overflow != 0 {
            // Overflow occurred. Need to determine which direction to round the result.
            let mut overflow_bits_count = 1;
            while overflow > 1 {
                overflow_bits_count += 1;
                overflow >>= 1;
            }
            let dropped_bits_mask = (1i32 << overflow_bits_count) - 1;
            let dropped_bits = (number as i32) & dropped_bits_mask;
            number >>= overflow_bits_count;
            exponent = overflow_bits_count;

            let mut zero_tail = true;
            i += 1;
            while i < digits.len() {
                zero_tail = zero_tail && digits[i] == b'0';
                exponent += radix_log_2 as i32;
                i += 1;
            }

            let middle_value = 1i32 << (overflow_bits_count - 1);
            if dropped_bits > middle_value {
                number += 1; // Rounding up.
            } else if dropped_bits == middle_value {
                // Rounding to even to consistency with decimals: half-way case rounds up if
                // significant part is odd and down otherwise.
                if (number & 1) != 0 || !zero_tail {
                    number += 1; // Rounding up.
                }
            }

            // Rounding up may cause overflow.
            if (number & (1i64 << 53)) != 0 {
                exponent += 1;
                number >>= 1;
            }
            break;
        }
        i += 1;
    }

    if exponent == 0 {
        if negative {
            if number == 0 {
                return -0.0;
            }
            return -(number as f64);
        }
        return number as f64;
    }
    ldexp((if negative { -number } else { number }) as f64, exponent)
}

/// Port of V8 `NumberParseIntHelper::HandleGenericCase` (radices other than 10 and powers of
/// two). Accumulates rounding error past ~2^56, as the spec allows and V8 does.
fn generic_radix_to_double(digits: &[u8], radix: u32) -> f64 {
    const MAXIMUM_MULTIPLIER: u32 = 0xFFFF_FFFF / 36;
    let mut result = 0f64;
    let mut i = 0;
    let mut done = false;
    while !done {
        // Parse the longest part of the string starting at `i` possible while keeping the
        // multiplier, and thus the part itself, within 32 bits.
        let mut part: u32 = 0;
        let mut multiplier: u32 = 1;
        loop {
            if i >= digits.len() {
                done = true;
                break;
            }
            let d = digit_value(digits[i]);
            if d >= radix {
                done = true;
                break;
            }
            // Update the value of the part as long as the multiplier fits in 32 bits. When we
            // can't guarantee that the next iteration will not overflow the multiplier, we stop
            // parsing the part by leaving the loop.
            let m = multiplier.wrapping_mul(radix);
            if m > MAXIMUM_MULTIPLIER {
                break;
            }
            part = part.wrapping_mul(radix).wrapping_add(d);
            multiplier = m;
            i += 1;
            if i == digits.len() {
                done = true;
                break;
            }
        }
        // Update the value and skip the part in the string.
        result = multiply_add(result, f64::from(multiplier), f64::from(part));
    }
    result
}

/// V8's `result_ * multiplier + dpart`. C++ compilers contract it into a fused multiply-add
/// on arm64 (one rounding), but not on x86-64 builds without FMA, so Node's results for
/// values above 2^53 differ by platform; this follows the platform's Node build.
#[cfg(target_arch = "aarch64")]
fn multiply_add(a: f64, b: f64, c: f64) -> f64 {
    a.mul_add(b, c)
}

/// See the aarch64 variant: separate multiply and add (two roundings).
#[cfg(not(target_arch = "aarch64"))]
fn multiply_add(a: f64, b: f64, c: f64) -> f64 {
    a * b + c
}

/// `parseInt(s, radix)`: leading JS whitespace, optional sign, optional `0x` prefix (radix 0 /
/// 16), then the longest run of radix digits. `NaN` when no digit is found or the radix is
/// outside `2..=36`. `radix` `None` (or `Some(0)`) means "detect".
pub fn parse_int(s: &str, radix: Option<u32>) -> f64 {
    let t = str16::trim_start(s).as_bytes();
    let mut i = 0;
    let mut negative = false;
    if i < t.len() && (t[i] == b'+' || t[i] == b'-') {
        negative = t[i] == b'-';
        i += 1;
    }
    // ToInt32(radix).
    let mut r = radix.map_or(0, |r| r as i32);
    let mut strip_prefix = true;
    if r != 0 {
        if !(2..=36).contains(&r) {
            return f64::NAN;
        }
        if r != 16 {
            strip_prefix = false;
        }
    } else {
        r = 10;
    }
    if strip_prefix && t.len() >= i + 2 && t[i] == b'0' && (t[i + 1] == b'x' || t[i + 1] == b'X') {
        i += 2;
        r = 16;
    }
    let r = r as u32;
    let start = i;
    while i < t.len() && digit_value(t[i]) < r {
        i += 1;
    }
    if i == start {
        return f64::NAN;
    }
    // V8 skips leading zeros before converting; for generic radices that shifts the 32-bit
    // chunk boundaries and therefore the rounding.
    let mut digits = &t[start..i];
    while let [b'0', rest @ ..] = digits {
        digits = rest;
    }
    if digits.is_empty() {
        return signed_zero(negative);
    }
    let magnitude = match r {
        10 => std::str::from_utf8(digits)
            .ok()
            .and_then(|d| d.parse::<f64>().ok())
            .unwrap_or(f64::NAN),
        2 => power_of_two_radix_to_double(digits, 1, false),
        4 => power_of_two_radix_to_double(digits, 2, false),
        8 => power_of_two_radix_to_double(digits, 3, false),
        16 => power_of_two_radix_to_double(digits, 4, false),
        32 => power_of_two_radix_to_double(digits, 5, false),
        _ => generic_radix_to_double(digits, r),
    };
    if negative { -magnitude } else { magnitude }
}

/// Length of the longest `StrDecimalLiteral`-shaped prefix of `b` (digits, optional fraction,
/// optional exponent; at least one digit in the mantissa), or 0 if there is none. `b` has no
/// sign.
fn decimal_prefix_len(b: &[u8]) -> usize {
    let mut i = 0;
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut mantissa_digits = i - int_start;
    if i < b.len() && b[i] == b'.' {
        let frac_start = i + 1;
        let mut j = frac_start;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        mantissa_digits += j - frac_start;
        if mantissa_digits > 0 {
            i = j;
        }
    }
    if mantissa_digits == 0 {
        return 0;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let exp_start = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > exp_start {
            i = j;
        }
    }
    i
}

/// `parseFloat(s)`: leading JS whitespace, then the longest prefix that is a decimal literal
/// (or `Infinity`), otherwise `NaN`.
pub fn parse_float(s: &str) -> f64 {
    let t = str16::trim_start(s);
    let b = t.as_bytes();
    let mut i = 0;
    let mut negative = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        negative = b[i] == b'-';
        i += 1;
    }
    if b[i..].starts_with(b"Infinity") {
        return if negative { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let n = decimal_prefix_len(&b[i..]);
    if n == 0 {
        return f64::NAN;
    }
    t[..i + n].parse::<f64>().unwrap_or(f64::NAN)
}

/// `Number(s)` (`StringToNumber`): surrounding JS whitespace is ignored, the empty string is 0,
/// `0x` / `0o` / `0b` prefixes (unsigned), `Infinity` with an optional sign, otherwise the
/// whole string must be a decimal literal or the result is `NaN`.
pub fn number(s: &str) -> f64 {
    let t = str16::trim(s);
    if t.is_empty() {
        return 0.0;
    }
    let b = t.as_bytes();
    if b.len() >= 2 && b[0] == b'0' {
        let radix_log_2 = match b[1] {
            b'x' | b'X' => Some(4),
            b'o' | b'O' => Some(3),
            b'b' | b'B' => Some(1),
            _ => None,
        };
        if let Some(log2) = radix_log_2 {
            let digits = &b[2..];
            let radix = 1u32 << log2;
            if digits.is_empty() || digits.iter().any(|&d| digit_value(d) >= radix) {
                return f64::NAN;
            }
            return power_of_two_radix_to_double(digits, log2, false);
        }
    }
    let mut i = 0;
    let mut negative = false;
    if b[0] == b'+' || b[0] == b'-' {
        negative = b[0] == b'-';
        i = 1;
    }
    if &b[i..] == b"Infinity" {
        return if negative { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let n = decimal_prefix_len(&b[i..]);
    if n == 0 || i + n != b.len() {
        return f64::NAN;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

/// `Math.round(x)`: the closest integer, ties toward +Infinity; keeps `-0` for
/// `-0.5 <= x <= -0`. Uses V8's `ceil(x)` correction rather than the naive `floor(x + 0.5)`,
/// which is wrong for `0.49999999999999994` and odd values above 2^52.
pub fn math_round(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    let t = x.ceil();
    if t - 0.5 > x { t - 1.0 } else { t }
}

/// `Math.imul(a, b)`.
pub fn imul(a: i32, b: i32) -> i32 {
    a.wrapping_mul(b)
}

/// `ToInt32(x)` (`x | 0`).
pub fn to_int32(x: f64) -> i32 {
    to_uint32(x) as i32
}

/// `ToUint32(x)` (`x >>> 0`).
pub fn to_uint32(x: f64) -> u32 {
    if !x.is_finite() {
        return 0;
    }
    x.trunc().rem_euclid(4_294_967_296.0) as u32
}

/// `Number.isInteger(x)`.
pub fn is_integer(x: f64) -> bool {
    x.is_finite() && x.trunc() == x
}

/// `Number.isSafeInteger(x)`.
pub fn is_safe_integer(x: f64) -> bool {
    is_integer(x) && x.abs() <= MAX_SAFE_INTEGER as f64
}
