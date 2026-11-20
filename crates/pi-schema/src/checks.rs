//! Compiled check primitives, inlineable leaf checks, code-point string bounds, exact decimal multiples, and
//! JSON structural equality.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use serde_json::{Number, Value};

use crate::arena::{deep_equal_values, is_finite_number, is_integer_number, num_as_exact_i128, number_cmp};

/// Thread-safe compiled validation check taking `(value, dynamic_scope)`.
pub(crate) type Check = Arc<dyn Fn(&Value, &mut Vec<String>) -> bool + Send + Sync>;

/// Guard mode for combined object and array checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Guard {
    None,
    Strict,
    Loose,
}

/// Bitmask constants for JSON Schema primitive and extension types.
pub(crate) const TYPE_STRING: u16 = 1 << 0;
pub(crate) const TYPE_NUMBER: u16 = 1 << 1;
pub(crate) const TYPE_INTEGER: u16 = 1 << 2;
pub(crate) const TYPE_BOOLEAN: u16 = 1 << 3;
pub(crate) const TYPE_NULL: u16 = 1 << 4;
pub(crate) const TYPE_ARRAY: u16 = 1 << 5;
pub(crate) const TYPE_OBJECT: u16 = 1 << 6;

pub(crate) fn type_bit(name: &str) -> u16 {
    match name {
        "string" => TYPE_STRING,
        "number" => TYPE_NUMBER,
        "integer" => TYPE_INTEGER,
        "boolean" => TYPE_BOOLEAN,
        "null" => TYPE_NULL,
        "array" => TYPE_ARRAY,
        "object" => TYPE_OBJECT,
        _ => 0,
    }
}

fn value_type_bits(v: &Value) -> u16 {
    match v {
        Value::String(_) => TYPE_STRING,
        Value::Number(n) => {
            if is_integer_number(n) {
                TYPE_NUMBER | TYPE_INTEGER
            } else if is_finite_number(n) {
                TYPE_NUMBER
            } else {
                0
            }
        }
        Value::Bool(_) => TYPE_BOOLEAN,
        Value::Null => TYPE_NULL,
        Value::Array(_) => TYPE_ARRAY,
        Value::Object(_) => TYPE_OBJECT,
    }
}

/// Fast leaf or general check, avoiding extra virtual calls in object/array loops.
#[derive(Clone)]
pub(crate) enum FastCheck {
    Always,
    Never,
    TypeMask(u16),
    ConstPrim(PrimValue),
    EnumPrim(Vec<PrimValue>),
    StringLen {
        min: f64,
        max: f64,
    },
    StringRegex(Arc<regress::Regex>),
    NumBounds {
        integer_only: bool,
        min: Option<Number>,
        max: Option<Number>,
        xmin: Option<Number>,
        xmax: Option<Number>,
    },
    Closure(Check),
}

/// Primitive JSON value for fast inline `const` / `enum` comparisons.
#[derive(Clone, Debug)]
pub(crate) enum PrimValue {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
}

impl PrimValue {
    pub(crate) fn from_value(v: &Value) -> Option<Self> {
        match v {
            Value::Null => Some(Self::Null),
            Value::Bool(b) => Some(Self::Bool(*b)),
            Value::Number(n) => Some(Self::Number(n.clone())),
            Value::String(s) => Some(Self::String(s.clone())),
            Value::Array(_) | Value::Object(_) => None,
        }
    }

    pub(crate) fn matches(&self, v: &Value) -> bool {
        match (self, v) {
            (Self::Null, Value::Null) => true,
            (Self::Bool(a), Value::Bool(b)) => a == b,
            (Self::Number(a), Value::Number(b)) => crate::arena::number_eq(a, b),
            (Self::String(a), Value::String(b)) => a == b,
            _ => false,
        }
    }
}

impl FastCheck {
    #[inline]
    pub(crate) fn run(&self, v: &Value, scope: &mut Vec<String>) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::TypeMask(mask) => (value_type_bits(v) & *mask) != 0,
            Self::ConstPrim(p) => p.matches(v),
            Self::EnumPrim(list) => list.iter().any(|p| p.matches(v)),
            Self::StringLen { min, max } => matches!(v, Value::String(s) if length_ok(s, *min, *max)),
            Self::StringRegex(re) => matches!(v, Value::String(s) if pi_js::regex::test(re, s)),
            Self::NumBounds {
                integer_only,
                min,
                max,
                xmin,
                xmax,
            } => match v {
                Value::Number(n) => {
                    if *integer_only {
                        if !is_integer_number(n) {
                            return false;
                        }
                    } else if !is_finite_number(n) {
                        return false;
                    }
                    in_number_bounds(n, min.as_ref(), max.as_ref(), xmin.as_ref(), xmax.as_ref())
                }
                _ => false,
            },
            Self::Closure(c) => c(v, scope),
        }
    }

    pub(crate) fn into_check(self) -> Check {
        match self {
            Self::Always => always_check(),
            Self::Never => never_check(),
            Self::TypeMask(mask) => {
                let m = mask;
                Arc::new(move |v, _| (value_type_bits(v) & m) != 0)
            }
            Self::ConstPrim(p) => Arc::new(move |v, _| p.matches(v)),
            Self::EnumPrim(list) => Arc::new(move |v, _| list.iter().any(|p| p.matches(v))),
            Self::StringLen { min, max } => {
                Arc::new(move |v, _| matches!(v, Value::String(s) if length_ok(s, min, max)))
            }
            Self::StringRegex(re) => Arc::new(move |v, _| matches!(v, Value::String(s) if pi_js::regex::test(&re, s))),
            Self::NumBounds {
                integer_only,
                min,
                max,
                xmin,
                xmax,
            } => Arc::new(move |v, _| match v {
                Value::Number(n) => {
                    if integer_only {
                        if !is_integer_number(n) {
                            return false;
                        }
                    } else if !is_finite_number(n) {
                        return false;
                    }
                    in_number_bounds(n, min.as_ref(), max.as_ref(), xmin.as_ref(), xmax.as_ref())
                }
                _ => false,
            }),
            Self::Closure(c) => c,
        }
    }
}

pub(crate) fn always_check() -> Check {
    static ALWAYS: OnceLock<Check> = OnceLock::new();
    ALWAYS.get_or_init(|| Arc::new(|_, _| true)).clone()
}

pub(crate) fn never_check() -> Check {
    static NEVER: OnceLock<Check> = OnceLock::new();
    NEVER.get_or_init(|| Arc::new(|_, _| false)).clone()
}

pub(crate) fn is_object_check() -> Check {
    static OBJ: OnceLock<Check> = OnceLock::new();
    OBJ.get_or_init(|| Arc::new(|v, _| v.is_object())).clone()
}

pub(crate) fn is_array_check() -> Check {
    static ARR: OnceLock<Check> = OnceLock::new();
    ARR.get_or_init(|| Arc::new(|v, _| v.is_array())).clone()
}

pub(crate) fn bare_type_check(name: &str) -> Option<FastCheck> {
    let bit = type_bit(name);
    if bit != 0 { Some(FastCheck::TypeMask(bit)) } else { None }
}

pub(crate) fn type_check_from_list(types: &[Option<&str>]) -> FastCheck {
    let mut mask = 0u16;
    for t in types.iter().flatten() {
        mask |= type_bit(t);
    }
    if mask == 0 {
        FastCheck::Never
    } else {
        FastCheck::TypeMask(mask)
    }
}

pub(crate) fn and_fast(mut parts: Vec<FastCheck>) -> FastCheck {
    match parts.len() {
        0 => FastCheck::Always,
        1 => parts.pop().unwrap_or(FastCheck::Always),
        2 => {
            let b = parts.pop().unwrap_or(FastCheck::Always);
            let a = parts.pop().unwrap_or(FastCheck::Always);
            FastCheck::Closure(Arc::new(move |v, s| a.run(v, s) && b.run(v, s)))
        }
        3 => {
            let c = parts.pop().unwrap_or(FastCheck::Always);
            let b = parts.pop().unwrap_or(FastCheck::Always);
            let a = parts.pop().unwrap_or(FastCheck::Always);
            FastCheck::Closure(Arc::new(move |v, s| a.run(v, s) && b.run(v, s) && c.run(v, s)))
        }
        _ => FastCheck::Closure(Arc::new(move |v, s| parts.iter().all(|p| p.run(v, s)))),
    }
}

pub(crate) fn indirect_check(slot: Arc<OnceLock<Check>>) -> Check {
    Arc::new(move |v, s| slot.get().is_some_and(|c| c(v, s)))
}

pub(crate) fn scoped_check(uri: String, inner: Check) -> Check {
    Arc::new(move |v, s| {
        s.push(uri.clone());
        let ok = inner(v, s);
        s.pop();
        ok
    })
}

pub(crate) fn masked_any_check(mask: u16, checks: Vec<FastCheck>) -> FastCheck {
    if checks.is_empty() {
        return FastCheck::TypeMask(mask);
    }
    if checks.len() == 1 {
        let rest = checks.into_iter().next().unwrap_or(FastCheck::Never);
        return FastCheck::Closure(Arc::new(move |v, s| (value_type_bits(v) & mask) != 0 || rest.run(v, s)));
    }
    FastCheck::Closure(Arc::new(move |v, s| {
        (value_type_bits(v) & mask) != 0 || checks.iter().any(|c| c.run(v, s))
    }))
}

/// Number of Unicode code points in `s` (JSON Schema string lengths count code points).
pub fn code_points(s: &str) -> usize {
    if s.is_ascii() { s.len() } else { s.chars().count() }
}

/// Fast bounds check using UTF-8 byte length (`bytes / 4 <= code_points <= bytes`) before counting.
#[inline]
pub(crate) fn length_ok(s: &str, min: f64, max: f64) -> bool {
    let bytes = s.len() as f64;
    if s.is_ascii() {
        return bytes >= min && bytes <= max;
    }
    if bytes < min {
        return false;
    }
    if bytes >= 4.0 * min && bytes <= max {
        return true;
    }
    let cp = s.chars().count() as f64;
    cp >= min && cp <= max
}

pub(crate) fn string_check(
    strict: bool,
    min: f64,
    max: f64,
    re: Option<Arc<regress::Regex>>,
    format: Option<crate::types::FormatCheck>,
) -> FastCheck {
    if min <= 0.0
        && max.is_infinite()
        && format.is_none()
        && let Some(re) = re
    {
        if strict {
            return FastCheck::StringRegex(re);
        }
        return FastCheck::Closure(Arc::new(move |v, _| match v {
            Value::String(s) => pi_js::regex::test(&re, s),
            _ => true,
        }));
    }
    if re.is_none() && format.is_none() {
        if strict {
            return FastCheck::StringLen { min, max };
        }
        return FastCheck::Closure(Arc::new(move |v, _| match v {
            Value::String(s) => length_ok(s, min, max),
            _ => true,
        }));
    }
    FastCheck::Closure(Arc::new(move |v, _| {
        let Value::String(s) = v else {
            return !strict;
        };
        length_ok(s, min, max)
            && re.as_ref().is_none_or(|r| pi_js::regex::test(r, s))
            && format.as_ref().is_none_or(|f| f.test(s))
    }))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NumMode {
    Loose = 0,
    Number = 1,
    Integer = 2,
    BigInt = 3,
}

#[inline]
fn in_number_bounds(
    n: &Number,
    min: Option<&Number>,
    max: Option<&Number>,
    xmin: Option<&Number>,
    xmax: Option<&Number>,
) -> bool {
    if let Some(m) = min
        && number_cmp(n, m) == Ordering::Less
    {
        return false;
    }
    if let Some(m) = max
        && number_cmp(n, m) == Ordering::Greater
    {
        return false;
    }
    if let Some(m) = xmin
        && number_cmp(n, m) != Ordering::Greater
    {
        return false;
    }
    if let Some(m) = xmax
        && number_cmp(n, m) != Ordering::Less
    {
        return false;
    }
    true
}

pub(crate) fn number_check(
    mode: NumMode,
    min: Option<Number>,
    max: Option<Number>,
    xmin: Option<Number>,
    xmax: Option<Number>,
    multiple_of: Option<Number>,
) -> FastCheck {
    if mode == NumMode::BigInt {
        // JSON has no native bigint values.
        return FastCheck::Never;
    }
    if (mode == NumMode::Number || mode == NumMode::Integer) && multiple_of.is_none() {
        return FastCheck::NumBounds {
            integer_only: mode == NumMode::Integer,
            min,
            max,
            xmin,
            xmax,
        };
    }
    FastCheck::Closure(Arc::new(move |v, _| {
        let Value::Number(n) = v else {
            return mode == NumMode::Loose;
        };
        if mode != NumMode::Loose && !is_finite_number(n) {
            return false;
        }
        if mode == NumMode::Integer && !is_integer_number(n) {
            return false;
        }
        in_number_bounds(n, min.as_ref(), max.as_ref(), xmin.as_ref(), xmax.as_ref())
            && multiple_of.as_ref().is_none_or(|d| is_multiple(n, d))
    }))
}

/// Exact decimal `multipleOf` check across integers and IEEE-754 floats (with overflow handling for `1e308 / 0.5`).
pub(crate) fn is_multiple(v: &Number, d: &Number) -> bool {
    if let (Some(vi), Some(di)) = (num_as_exact_i128(v), num_as_exact_i128(d)) {
        return di != 0 && vi % di == 0;
    }
    let (Some(vf), Some(df)) = (v.as_f64(), d.as_f64()) else {
        return false;
    };
    if !vf.is_finite() || !df.is_finite() || df == 0.0 {
        return false;
    }
    let q = vf / df;
    if q.is_finite() && q.fract() == 0.0 {
        return true;
    }
    if !q.is_finite() {
        return vf % df == 0.0;
    }
    let (vc, ve) = decimal_parts(vf);
    let (dc, de) = decimal_parts(df);
    if dc == 0 {
        return false;
    }
    if vc == 0 {
        return true;
    }
    if ve >= de {
        let diff = (ve - de) as u64;
        let rem = ((vc % dc) * mod_pow_u128(10, diff, dc)) % dc;
        rem == 0
    } else {
        let diff = (de - ve) as u64;
        if diff > 24 {
            return false;
        }
        let Some(pow10) = 10u128.checked_pow(diff as u32) else {
            return false;
        };
        let Some(denom) = dc.checked_mul(pow10) else {
            return false;
        };
        vc % denom == 0
    }
}

fn mod_pow_u128(mut base: u128, mut exp: u64, modulus: u128) -> u128 {
    if modulus == 1 {
        return 0;
    }
    let mut result = 1u128;
    base %= modulus;
    while exp > 0 {
        if exp & 1 == 1 {
            result = (result * base) % modulus;
        }
        exp >>= 1;
        base = (base * base) % modulus;
    }
    result
}

fn decimal_parts(x: f64) -> (u128, i32) {
    let s = pi_js::num::to_js_string(x.abs());
    let (mantissa, exp) = match s.split_once('e') {
        Some((m, e)) => (m, e.parse::<i32>().unwrap_or(0)),
        None => (s.as_str(), 0),
    };
    let (digits, fraction) = match mantissa.find('.') {
        Some(dot) => {
            let frac = (mantissa.len() - dot - 1) as i32;
            let mut stripped = String::with_capacity(mantissa.len() - 1);
            stripped.push_str(&mantissa[..dot]);
            stripped.push_str(&mantissa[dot + 1..]);
            (stripped, frac)
        }
        None => (mantissa.to_string(), 0),
    };
    let coeff = digits.parse::<u128>().unwrap_or(0);
    (coeff, exp - fraction)
}

pub(crate) fn guard_object(inner: FastCheck, strict: bool) -> FastCheck {
    FastCheck::Closure(Arc::new(
        move |v, s| {
            if v.is_object() { inner.run(v, s) } else { !strict }
        },
    ))
}

pub(crate) fn guard_array(inner: FastCheck, strict: bool) -> FastCheck {
    FastCheck::Closure(Arc::new(
        move |v, s| {
            if v.is_array() { inner.run(v, s) } else { !strict }
        },
    ))
}

#[derive(Clone)]
pub(crate) struct PropEntry {
    pub(crate) key: String,
    pub(crate) check: FastCheck,
    pub(crate) required: bool,
}

pub(crate) fn declared_object(entries: Vec<PropEntry>, required_count: usize, closed: bool, guard: Guard) -> FastCheck {
    FastCheck::Closure(Arc::new(move |v, s| {
        let Some(map) = v.as_object() else {
            return guard == Guard::Loose;
        };
        let n = entries.len();
        let mut required = 0usize;
        let mut next = 0usize;
        for (k, val) in map {
            let mut idx = next;
            if idx >= n || entries[idx].key != *k {
                idx = 0;
                while idx < n && entries[idx].key != *k {
                    idx += 1;
                }
                if idx == n {
                    if closed {
                        return false;
                    }
                    continue;
                }
            }
            next = idx + 1;
            let entry = &entries[idx];
            if !entry.check.run(val, s) {
                return false;
            }
            if entry.required {
                required += 1;
            }
        }
        required == required_count
    }))
}

pub(crate) fn open_object(
    entries: Vec<PropEntry>,
    extra_required: Vec<String>,
    patterns: Vec<(Arc<regress::Regex>, FastCheck)>,
    additional: Option<FastCheck>,
    guard: Guard,
) -> FastCheck {
    let known: Option<HashSet<String>> = if !patterns.is_empty() || additional.is_some() {
        Some(entries.iter().map(|e| e.key.clone()).collect())
    } else {
        None
    };
    FastCheck::Closure(Arc::new(move |v, s| {
        let Some(map) = v.as_object() else {
            return guard == Guard::Loose;
        };
        for entry in &entries {
            match map.get(&entry.key) {
                None => {
                    if entry.required {
                        return false;
                    }
                }
                Some(val) => {
                    if !entry.check.run(val, s) {
                        return false;
                    }
                }
            }
        }
        for req in &extra_required {
            if !map.contains_key(req) {
                return false;
            }
        }
        let Some(known_keys) = &known else {
            return true;
        };
        for (k, val) in map {
            let mut matched = known_keys.contains(k);
            for (re, check) in &patterns {
                if pi_js::regex::test(re, k) {
                    matched = true;
                    if !check.run(val, s) {
                        return false;
                    }
                }
            }
            if !matched
                && let Some(add) = &additional
                && !add.run(val, s)
            {
                return false;
            }
        }
        true
    }))
}

pub(crate) fn property_names_check(check: FastCheck) -> FastCheck {
    FastCheck::Closure(Arc::new(move |v, s| {
        let Some(map) = v.as_object() else {
            return true;
        };
        for k in map.keys() {
            let key_val = Value::String(k.clone());
            if !check.run(&key_val, s) {
                return false;
            }
        }
        true
    }))
}

pub(crate) fn property_count_check(min: f64, max: f64) -> FastCheck {
    FastCheck::Closure(Arc::new(move |v, _| {
        let Some(map) = v.as_object() else {
            return true;
        };
        let n = map.len() as f64;
        n >= min && n <= max
    }))
}

pub(crate) fn dependent_required_check(entries: Vec<(String, Vec<String>)>) -> FastCheck {
    FastCheck::Closure(Arc::new(move |v, _| {
        let Some(map) = v.as_object() else {
            return true;
        };
        for (k, reqs) in &entries {
            if map.contains_key(k) {
                for r in reqs {
                    if !map.contains_key(r) {
                        return false;
                    }
                }
            }
        }
        true
    }))
}

pub(crate) fn dependent_schemas_check(entries: Vec<(String, FastCheck)>) -> FastCheck {
    FastCheck::Closure(Arc::new(move |v, s| {
        let Some(map) = v.as_object() else {
            return true;
        };
        for (k, check) in &entries {
            if map.contains_key(k) && !check.run(v, s) {
                return false;
            }
        }
        true
    }))
}

pub(crate) fn list_check(
    guard: Guard,
    min: f64,
    max: f64,
    prefix: Vec<FastCheck>,
    rest: Option<FastCheck>,
) -> FastCheck {
    FastCheck::Closure(Arc::new(move |v, s| {
        let Some(arr) = v.as_array() else {
            return guard == Guard::Loose;
        };
        let len = arr.len();
        let len_f = len as f64;
        if len_f < min || len_f > max {
            return false;
        }
        let m = len.min(prefix.len());
        for i in 0..m {
            if !prefix[i].run(&arr[i], s) {
                return false;
            }
        }
        if let Some(r) = &rest {
            for item in arr.iter().skip(prefix.len()) {
                if !r.run(item, s) {
                    return false;
                }
            }
        }
        true
    }))
}

pub(crate) fn items_check(prefix: Vec<FastCheck>, rest: Option<FastCheck>) -> FastCheck {
    list_check(Guard::None, 0.0, f64::INFINITY, prefix, rest)
}

pub(crate) fn array_length_check(min: f64, max: f64) -> FastCheck {
    FastCheck::Closure(Arc::new(move |v, _| {
        let Some(arr) = v.as_array() else {
            return true;
        };
        let len = arr.len() as f64;
        len >= min && len <= max
    }))
}

pub(crate) fn contains_check(check: FastCheck, min: f64, max: f64) -> FastCheck {
    FastCheck::Closure(Arc::new(move |v, s| {
        let Some(arr) = v.as_array() else {
            return true;
        };
        let mut count = 0usize;
        for item in arr {
            if check.run(item, s) {
                count += 1;
                if (count as f64) > max {
                    return false;
                }
            }
        }
        (count as f64) >= min
    }))
}

pub(crate) fn unique_check_fn(v: &Value) -> bool {
    let Some(arr) = v.as_array() else {
        return true;
    };
    for i in 1..arr.len() {
        for j in 0..i {
            if deep_equal_values(&arr[i], &arr[j]) {
                return false;
            }
        }
    }
    true
}

pub(crate) fn unique_check() -> FastCheck {
    FastCheck::Closure(Arc::new(|v, _| unique_check_fn(v)))
}

pub(crate) fn enum_check(values: Vec<Value>) -> FastCheck {
    let prims: Option<Vec<PrimValue>> = values.iter().map(PrimValue::from_value).collect();
    if let Some(list) = prims
        && list.len() <= 8
    {
        return FastCheck::EnumPrim(list);
    }
    FastCheck::Closure(Arc::new(move |v, _| {
        values.iter().any(|item| deep_equal_values(v, item))
    }))
}

pub(crate) fn const_check(value: Value) -> FastCheck {
    if let Some(p) = PrimValue::from_value(&value) {
        return FastCheck::ConstPrim(p);
    }
    FastCheck::Closure(Arc::new(move |v, _| deep_equal_values(v, &value)))
}

pub(crate) fn any_check(checks: Vec<FastCheck>) -> FastCheck {
    FastCheck::Closure(Arc::new(move |v, s| checks.iter().any(|c| c.run(v, s))))
}

pub(crate) fn one_check(checks: Vec<FastCheck>) -> FastCheck {
    FastCheck::Closure(Arc::new(move |v, s| {
        let mut n = 0usize;
        for c in &checks {
            if c.run(v, s) {
                n += 1;
                if n > 1 {
                    return false;
                }
            }
        }
        n == 1
    }))
}

pub(crate) fn not_check(check: FastCheck) -> FastCheck {
    FastCheck::Closure(Arc::new(move |v, s| !check.run(v, s)))
}

pub(crate) fn if_check(cond: FastCheck, then: FastCheck, otherwise: FastCheck) -> FastCheck {
    FastCheck::Closure(Arc::new(move |v, s| {
        if cond.run(v, s) {
            then.run(v, s)
        } else {
            otherwise.run(v, s)
        }
    }))
}

pub(crate) fn tagged_check(key: String, tags: Vec<PrimValue>, checks: Vec<FastCheck>) -> FastCheck {
    FastCheck::Closure(Arc::new(move |v, s| {
        let Some(map) = v.as_object() else {
            return false;
        };
        let Some(tag_val) = map.get(&key) else {
            return false;
        };
        for (i, tag) in tags.iter().enumerate() {
            if tag.matches(tag_val) {
                return checks[i].run(v, s);
            }
        }
        false
    }))
}

/// JSON structural equality matching JSON Schema `const` / `enum` / `uniqueItems` and TypeBox's `Value.Equal`.
pub fn equal(a: &Value, b: &Value) -> bool {
    deep_equal_values(a, b)
}
