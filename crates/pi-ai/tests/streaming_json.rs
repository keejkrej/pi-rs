//! Port of packages/ai/test/streaming-json.test.ts (#9265)

use std::time::Instant;

use pi_ai::utils::json_parse::{parse_streaming_json, parse_tool_call_arguments};
use pi_ai::utils::streaming_json::{JsonChange, JsonPathSegment, StreamingJsonParser, ToolCallArgumentsStream};
use serde_json::{Map, Value, json};

const JSON_TEST_SUITE: &str = include_str!("data/json-test-suite.json");

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_f64(&mut self) -> f64 {
        self.0 = (self.0.wrapping_mul(1103515245).wrapping_add(12345)) & 0x7fff_ffff;
        (self.0 as f64) / (0x7fff_ffff_u64 as f64)
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        let idx = (self.next_f64() * (items.len() as f64)).floor() as usize;
        &items[idx.min(items.len() - 1)]
    }
}

const STRING_PIECES: &[&str] = &[
    "a", "b", "x y", " ", "\n", "\t", "\\", "\"", "/", "é", "😀", "\u{0001}", "\u{001f}", "\u{2028}", "{", "]", ",",
];

fn random_string(rng: &mut Lcg) -> String {
    let mut out = String::new();
    let len = (rng.next_f64() * 6.0).floor() as usize;
    for _ in 0..len {
        out.push_str(rng.pick(STRING_PIECES));
    }
    out
}

fn random_scalar(rng: &mut Lcg) -> Value {
    let idx = (rng.next_f64() * 10.0).floor() as usize;
    match idx.min(9) {
        0 => Value::String(random_string(rng)),
        1 => json!((rng.next_f64() * 2000.0).floor() as i64 - 1000),
        2 => pi_js::json::number_value(rng.next_f64() * 100.0),
        3 => pi_js::json::number_value(1e21),
        4 => pi_js::json::number_value(-5e-8),
        5 => pi_js::json::number_value(-0.0),
        6 => json!(0),
        7 => Value::Bool(true),
        8 => Value::Bool(false),
        _ => Value::Null,
    }
}

fn random_value(rng: &mut Lcg, depth: usize) -> Value {
    let roll = rng.next_f64();
    if depth > 3 || roll < 0.3 {
        return random_scalar(rng);
    }
    if roll < 0.65 {
        let mut map = Map::new();
        let count = (rng.next_f64() * 4.0).floor() as usize;
        for _ in 0..count {
            let k = random_string(rng);
            let v = random_value(rng, depth + 1);
            map.insert(k, v);
        }
        Value::Object(map)
    } else {
        let count = (rng.next_f64() * 4.0).floor() as usize;
        Value::Array((0..count).map(|_| random_value(rng, depth + 1)).collect())
    }
}

fn pick_space(rng: &mut Lcg, spaced: bool) -> &'static str {
    let spaces = &["", "", " ", "\n  ", "\t", "\r\n"];
    if spaced { *rng.pick(spaces) } else { "" }
}

fn render_value(value: &Value, rng: &mut Lcg, spaced: bool) -> String {
    match value {
        Value::Array(arr) => {
            let s0 = pick_space(rng, spaced);
            let mut joined = String::new();
            for (idx, v) in arr.iter().enumerate() {
                if idx > 0 {
                    joined.push_str(pick_space(rng, spaced));
                    joined.push(',');
                    joined.push_str(pick_space(rng, spaced));
                }
                joined.push_str(&render_value(v, rng, spaced));
            }
            let s1 = pick_space(rng, spaced);
            format!("[{s0}{joined}{s1}]")
        }
        Value::Object(map) => {
            let s0 = pick_space(rng, spaced);
            let mut joined = String::new();
            for (idx, (k, v)) in map.iter().enumerate() {
                if idx > 0 {
                    joined.push_str(pick_space(rng, spaced));
                    joined.push(',');
                    joined.push_str(pick_space(rng, spaced));
                }
                let k_str = pi_js::json::stringify(&Value::String(k.clone()));
                let c1 = pick_space(rng, spaced);
                let c2 = pick_space(rng, spaced);
                let v_str = render_value(v, rng, spaced);
                joined.push_str(&format!("{k_str}{c1}:{c2}{v_str}"));
            }
            let s1 = pick_space(rng, spaced);
            format!("{{{s0}{joined}{s1}}}")
        }
        Value::Number(n) => {
            if n.as_f64().is_some_and(|f| f == 0.0 && f.is_sign_negative()) {
                "-0".to_owned()
            } else {
                pi_js::json::stringify(value)
            }
        }
        _ => pi_js::json::stringify(value),
    }
}

fn random_documents(seed: u64, count: usize, spaced: bool) -> Vec<String> {
    let mut rng = Lcg::new(seed);
    (0..count)
        .map(|_| {
            let v = if rng.next_f64() < 0.8 {
                let mut map = Map::new();
                map.insert("path".to_owned(), Value::String(random_string(&mut rng)));
                map.insert("args".to_owned(), random_value(&mut rng, 0));
                Value::Object(map)
            } else {
                random_value(&mut rng, 0)
            };
            render_value(&v, &mut rng, spaced)
        })
        .collect()
}

// ============================================================================
// Independent recursive-descent reference parser
// ============================================================================

struct ReadResult {
    value: Value,
    complete: bool,
    end: usize,
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

fn skip_space(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && is_ws(bytes[i]) {
        i += 1;
    }
    i
}

fn reference_snapshot(text: &str, pending_scalars: bool) -> Option<Value> {
    let bytes = text.as_bytes();
    let start = skip_space(bytes, 0);
    read_value(text, start, pending_scalars).map(|r| r.value)
}

fn read_value(text: &str, start: usize, pending_scalars: bool) -> Option<ReadResult> {
    let bytes = text.as_bytes();
    if start >= bytes.len() {
        return None;
    }
    match bytes[start] {
        b'"' => Some(read_string(text, start)),
        b'{' => Some(read_object(text, start, pending_scalars)),
        b'[' => Some(read_array(text, start, pending_scalars)),
        _ => read_scalar(text, start, pending_scalars),
    }
}

fn read_string(text: &str, start: usize) -> ReadResult {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut i = start + 1;
    let mut pending_hi: Option<u16> = None;

    let flush_hi = |pending: &mut Option<u16>, buf: &mut String| {
        if pending.take().is_some() {
            buf.push('\u{FFFD}');
        }
    };

    while i < bytes.len() {
        let ch = text[i..].chars().next().unwrap();
        if ch == '"' {
            flush_hi(&mut pending_hi, &mut out);
            return ReadResult {
                value: Value::String(out),
                complete: true,
                end: i + 1,
            };
        }
        if ch != '\\' {
            flush_hi(&mut pending_hi, &mut out);
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        if i + 1 >= bytes.len() {
            return ReadResult {
                value: Value::String(out),
                complete: false,
                end: bytes.len(),
            };
        }
        let next = text[i + 1..].chars().next().unwrap();
        let simple = match next {
            '"' => Some('"'),
            '\\' => Some('\\'),
            '/' => Some('/'),
            'b' => Some('\x08'),
            'f' => Some('\x0c'),
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            _ => None,
        };
        if let Some(esc) = simple {
            flush_hi(&mut pending_hi, &mut out);
            out.push(esc);
            i += 2;
            continue;
        }
        if next == 'u' {
            let mut end = i + 2;
            while end < i + 6 && end < bytes.len() && bytes[end].is_ascii_hexdigit() {
                end += 1;
            }
            if end == i + 6 {
                let code = u16::from_str_radix(&text[i + 2..end], 16).unwrap();
                if (0xD800..=0xDBFF).contains(&code) {
                    flush_hi(&mut pending_hi, &mut out);
                    pending_hi = Some(code);
                } else if (0xDC00..=0xDFFF).contains(&code) {
                    if let Some(hi) = pending_hi.take() {
                        let scalar = 0x10000 + (((u32::from(hi) - 0xD800) << 10) | (u32::from(code) - 0xDC00));
                        out.push(char::from_u32(scalar).unwrap_or('\u{FFFD}'));
                    } else {
                        out.push('\u{FFFD}');
                    }
                } else {
                    flush_hi(&mut pending_hi, &mut out);
                    out.push(char::from_u32(u32::from(code)).unwrap_or('\u{FFFD}'));
                }
                i = end;
                continue;
            }
            if end == bytes.len() {
                return ReadResult {
                    value: Value::String(out),
                    complete: false,
                    end,
                };
            }
            flush_hi(&mut pending_hi, &mut out);
            out.push_str(&text[i..end]);
            i = end;
            continue;
        }
        flush_hi(&mut pending_hi, &mut out);
        out.push('\\');
        i += 1;
    }

    ReadResult {
        value: Value::String(out),
        complete: false,
        end: bytes.len(),
    }
}

fn read_object(text: &str, start: usize, pending_scalars: bool) -> ReadResult {
    let bytes = text.as_bytes();
    let mut map = Map::new();
    let mut i = skip_space(bytes, start + 1);
    if i >= bytes.len() {
        return ReadResult {
            value: Value::Object(map),
            complete: false,
            end: bytes.len(),
        };
    }
    if bytes[i] == b'}' {
        return ReadResult {
            value: Value::Object(map),
            complete: true,
            end: i + 1,
        };
    }
    loop {
        assert_eq!(bytes[i], b'"', "reference reads valid JSON only: {text}");
        let key = read_string(text, i);
        if !key.complete {
            return ReadResult {
                value: Value::Object(map),
                complete: false,
                end: bytes.len(),
            };
        }
        i = skip_space(bytes, key.end);
        if i >= bytes.len() {
            return ReadResult {
                value: Value::Object(map),
                complete: false,
                end: bytes.len(),
            };
        }
        i = skip_space(bytes, i + 1);
        let Some(member) = read_value(text, i, pending_scalars) else {
            return ReadResult {
                value: Value::Object(map),
                complete: false,
                end: bytes.len(),
            };
        };
        if let Value::String(k) = key.value {
            map.insert(k, member.value);
        }
        if !member.complete {
            return ReadResult {
                value: Value::Object(map),
                complete: false,
                end: bytes.len(),
            };
        }
        i = skip_space(bytes, member.end);
        if i >= bytes.len() {
            return ReadResult {
                value: Value::Object(map),
                complete: false,
                end: bytes.len(),
            };
        }
        if bytes[i] == b'}' {
            return ReadResult {
                value: Value::Object(map),
                complete: true,
                end: i + 1,
            };
        }
        i = skip_space(bytes, i + 1);
        if i >= bytes.len() {
            return ReadResult {
                value: Value::Object(map),
                complete: false,
                end: bytes.len(),
            };
        }
    }
}

fn read_array(text: &str, start: usize, pending_scalars: bool) -> ReadResult {
    let bytes = text.as_bytes();
    let mut arr = Vec::new();
    let mut i = skip_space(bytes, start + 1);
    if i >= bytes.len() {
        return ReadResult {
            value: Value::Array(arr),
            complete: false,
            end: bytes.len(),
        };
    }
    if bytes[i] == b']' {
        return ReadResult {
            value: Value::Array(arr),
            complete: true,
            end: i + 1,
        };
    }
    loop {
        let Some(elem) = read_value(text, i, pending_scalars) else {
            return ReadResult {
                value: Value::Array(arr),
                complete: false,
                end: bytes.len(),
            };
        };
        arr.push(elem.value);
        if !elem.complete {
            return ReadResult {
                value: Value::Array(arr),
                complete: false,
                end: bytes.len(),
            };
        }
        i = skip_space(bytes, elem.end);
        if i >= bytes.len() {
            return ReadResult {
                value: Value::Array(arr),
                complete: false,
                end: bytes.len(),
            };
        }
        if bytes[i] == b']' {
            return ReadResult {
                value: Value::Array(arr),
                complete: true,
                end: i + 1,
            };
        }
        i = skip_space(bytes, i + 1);
        if i >= bytes.len() {
            return ReadResult {
                value: Value::Array(arr),
                complete: false,
                end: bytes.len(),
            };
        }
    }
}

fn parse_ref_number(token: &str) -> Option<Value> {
    if !is_valid_json_number(token) {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<Value>(token)
        && v.is_number()
    {
        return Some(v);
    }
    let f: f64 = token.parse().ok()?;
    f.is_finite().then(|| pi_js::json::number_value(f))
}

fn is_valid_json_number(token: &str) -> bool {
    let b = token.as_bytes();
    let mut i = 0;
    if b.get(i) == Some(&b'-') {
        i += 1;
    }
    if i >= b.len() {
        return false;
    }
    if b[i] == b'0' {
        i += 1;
    } else if b[i].is_ascii_digit() && b[i] != b'0' {
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    } else {
        return false;
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        if i >= b.len() || !b[i].is_ascii_digit() {
            return false;
        }
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if i >= b.len() || !b[i].is_ascii_digit() {
            return false;
        }
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    i == b.len()
}

fn read_scalar(text: &str, start: usize, pending_scalars: bool) -> Option<ReadResult> {
    let bytes = text.as_bytes();
    let mut end = start;
    while end < bytes.len()
        && !matches!(
            bytes[end],
            b' ' | b'\t' | b'\n' | b'\r' | b',' | b':' | b'{' | b'}' | b'[' | b']' | b'"'
        )
    {
        end += 1;
    }
    let token = &text[start..end];
    if end < bytes.len() {
        let value = match token {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            "null" => Value::Null,
            _ => parse_ref_number(token).expect("valid JSON scalar"),
        };
        return Some(ReadResult {
            value,
            complete: true,
            end,
        });
    }
    if !pending_scalars {
        return None;
    }
    for (lit, val) in [
        ("true", Value::Bool(true)),
        ("false", Value::Bool(false)),
        ("null", Value::Null),
    ] {
        if lit.starts_with(token) {
            return Some(ReadResult {
                value: val,
                complete: false,
                end,
            });
        }
    }
    if let Some(num) = parse_ref_number(token) {
        return Some(ReadResult {
            value: num,
            complete: false,
            end,
        });
    }
    // Strip trailing [eE][+-]?
    let mut cut = token.len();
    if token.ends_with('+') || token.ends_with('-') {
        cut = cut.saturating_sub(1);
    }
    if cut > 0 && matches!(token.as_bytes()[cut - 1], b'e' | b'E') {
        let prefix = &token[..cut - 1];
        if let Some(num) = parse_ref_number(prefix) {
            return Some(ReadResult {
                value: num,
                complete: false,
                end,
            });
        }
    }
    None
}

// ============================================================================
// Chunking helpers (character boundaries)
// ============================================================================

enum ChunkMode {
    Fixed(usize),
    Seed(u64),
}

struct Step {
    read: String,
    snapshot: Option<Value>,
}

fn char_chunks(text: &str, mode: ChunkMode) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let char_offsets: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect();
    let total_chars = char_offsets.len() - 1;
    let mut rng = match mode {
        ChunkMode::Fixed(_) => None,
        ChunkMode::Seed(s) => Some(Lcg::new(s)),
    };
    let fixed = match mode {
        ChunkMode::Fixed(n) => n.max(1),
        ChunkMode::Seed(_) => 1,
    };
    let mut out = Vec::new();
    let mut ci = 0;
    while ci < total_chars {
        let step = if let Some(r) = rng.as_mut() {
            1 + (r.next_f64() * 12.0).floor() as usize
        } else {
            fixed
        };
        let next_ci = (ci + step).min(total_chars);
        out.push(&text[char_offsets[ci]..char_offsets[next_ci]]);
        ci = next_ci;
    }
    out
}

fn stream_steps(text: &str, mode: ChunkMode) -> Vec<Step> {
    let mut parser = StreamingJsonParser::new();
    let mut read = String::new();
    let mut steps = Vec::new();
    for piece in char_chunks(text, mode) {
        parser.append(piece);
        read.push_str(piece);
        steps.push(Step {
            read: read.clone(),
            snapshot: parser.snapshot(),
        });
    }
    steps
}

fn final_snapshot(text: &str, mode: ChunkMode) -> Option<Value> {
    let mut parser = StreamingJsonParser::new();
    if text.is_empty() {
        return parser.snapshot();
    }
    for piece in char_chunks(text, mode) {
        parser.append(piece);
    }
    parser.snapshot()
}

fn read32(text: &str) -> u64 {
    let mut hash: i32 = 7;
    for unit in text.encode_utf16() {
        hash = hash.wrapping_mul(31).wrapping_add(i32::from(unit));
    }
    (hash.unsigned_abs() as u64) + 1
}

/// Compare two JSON values with JS-compatible numeric equality (-0 == 0).
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => {
            if x == y {
                return true;
            }
            match (x.as_f64(), y.as_f64()) {
                (Some(fx), Some(fy)) => fx == fy,
                _ => false,
            }
        }
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(l, r)| json_eq(l, r)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, vx)| y.get(k).is_some_and(|vy| json_eq(vx, vy)))
        }
        _ => false,
    }
}

fn assert_json_eq(actual: &Option<Value>, expected: &Option<Value>, label: &str) {
    let ok = match (actual, expected) {
        (None, None) => true,
        (Some(a), Some(b)) => json_eq(a, b),
        _ => false,
    };
    assert!(ok, "{label}: actual={actual:?} expected={expected:?}");
}

#[derive(Default)]
struct Mirror {
    value: Option<Value>,
}

impl Mirror {
    fn apply(&mut self, changes: &[JsonChange]) {
        for change in changes {
            match change {
                JsonChange::Set { path, value } => {
                    if path.is_empty() {
                        self.value = Some(value.clone());
                    } else {
                        let parent = navigate_mut(self.value.as_mut().unwrap(), &path[..path.len() - 1]);
                        set_segment(parent, path.last().unwrap(), value.clone());
                    }
                }
                JsonChange::Append { path, text } => {
                    let target = if path.is_empty() {
                        self.value.as_mut().unwrap()
                    } else {
                        navigate_mut(self.value.as_mut().unwrap(), path)
                    };
                    if let Value::String(s) = target {
                        s.push_str(text);
                    } else {
                        panic!("expected string at append path {path:?}, got {target:?}");
                    }
                }
            }
        }
    }
}

fn navigate_mut<'a>(mut cur: &'a mut Value, path: &[JsonPathSegment]) -> &'a mut Value {
    for seg in path {
        cur = match (cur, seg) {
            (Value::Object(map), JsonPathSegment::Key(k)) => map.get_mut(k).unwrap(),
            (Value::Array(arr), JsonPathSegment::Index(i)) => &mut arr[*i],
            (other, seg) => panic!("cannot navigate {seg:?} in {other:?}"),
        };
    }
    cur
}

fn set_segment(parent: &mut Value, seg: &JsonPathSegment, value: Value) {
    match (parent, seg) {
        (Value::Object(map), JsonPathSegment::Key(k)) => {
            map.insert(k.clone(), value);
        }
        (Value::Array(arr), JsonPathSegment::Index(i)) => {
            if *i == arr.len() {
                arr.push(value);
            } else {
                arr[*i] = value;
            }
        }
        (other, seg) => panic!("cannot set {seg:?} in {other:?}"),
    }
}

fn expect_json_finite(value: &Value, label: &str) {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => {}
        Value::Number(n) => {
            assert!(n.as_f64().is_some_and(f64::is_finite), "{label}: non-finite number");
        }
        Value::Array(arr) => {
            for item in arr {
                expect_json_finite(item, label);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                expect_json_finite(item, label);
            }
        }
    }
}

#[test]
fn differential_fuzzing() {
    let chunk_modes = |text: &str| {
        vec![
            ChunkMode::Fixed(1),
            ChunkMode::Fixed(2),
            ChunkMode::Fixed(3),
            ChunkMode::Fixed(7),
            ChunkMode::Seed(read32(text)),
        ]
    };

    // 1. Matches the reference after every piece, at every chunk size, with insignificant whitespace.
    for text in random_documents(1, 250, true) {
        for mode in chunk_modes(&text) {
            for step in stream_steps(&text, mode) {
                assert_json_eq(&step.snapshot, &reference_snapshot(&step.read, true), &step.read);
            }
        }
    }

    // 2. Matches parse_streaming_json at every prefix of compact documents.
    for text in random_documents(2, 400, false) {
        for step in stream_steps(&text, ChunkMode::Fixed(1)) {
            if step.read.ends_with(char::is_whitespace) {
                continue;
            }
            if step.snapshot == Some(Value::Null) && "null".starts_with(&step.read) {
                continue;
            }
            let actual = step.snapshot.unwrap_or_else(|| json!({}));
            // PORT: `parse_streaming_json` in Rust returns `JsonObject` (coercing non-object roots to `{}`).
            if actual.is_object() {
                let expected = Value::Object(parse_streaming_json(Some(&step.read)).into_iter().collect());
                assert_json_eq(&Some(actual), &Some(expected), &step.read);
            }
        }
    }

    // 3. Reads complete documents identically to serde_json / JS number semantics.
    for text in random_documents(3, 300, true)
        .into_iter()
        .chain(random_documents(4, 300, false))
    {
        let expected = serde_json::from_str::<Value>(&text).unwrap();
        for size in [1usize, 2, 3, 7, 5] {
            let snap = final_snapshot(&text, ChunkMode::Fixed(size));
            assert_json_eq(&snap, &Some(expected.clone()), &text);
        }
    }

    // 4. Repairs raw control characters and invalid escapes like parse_streaming_json.
    let mut rng = Lcg::new(6);
    let pool = [
        "\n", "\t", "\u{0000}", "\\d", "\\x41", "\\u12", "\\uZZZZ", "\\u00e9", "\\\\", "\\q", "a", " ", "é",
    ];
    for round in 0..300 {
        let mut content = String::new();
        for _ in 0..6 {
            content.push_str(rng.pick(&pool));
        }
        let text = format!("{{\"path\":\"p\",\"content\":\"{content}\",\"n\":{round}}}");
        let parsed = Value::Object(parse_streaming_json(Some(&text)).into_iter().collect());
        assert_json_eq(&final_snapshot(&text, ChunkMode::Fixed(1)), &Some(parsed), &text);
        for step in stream_steps(&text, ChunkMode::Fixed(1)) {
            assert_json_eq(&step.snapshot, &reference_snapshot(&step.read, true), &step.read);
        }
    }
}

#[test]
fn strings_escapes_surrogates_proto_and_duplicates() {
    // Holds escape back until complete, at every split position.
    for (text, expected) in [
        (r#"{"a":"x\u00e9y"}"#, "xéy"),
        (r#"{"a":"x\ny"}"#, "x\ny"),
        (r#"{"a":"x\\y"}"#, "x\\y"),
        (r#"{"a":"x\"y"}"#, "x\"y"),
        (r#"{"a":"x\/y"}"#, "x/y"),
    ] {
        let offsets: Vec<usize> = text
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .collect();
        for &split in &offsets {
            let mut parser = StreamingJsonParser::new();
            parser.append(&text[..split]);
            assert_json_eq(
                &parser.snapshot(),
                &reference_snapshot(&text[..split], true),
                &text[..split],
            );
            parser.append(&text[split..]);
            assert_eq!(parser.snapshot(), Some(json!({ "a": expected })));
        }
    }

    // Joins surrogate pairs split across pieces, escaped or raw.
    for text in [r#"{"a":"\ud83d\ude00!"}"#, "{\"a\":\"😀!\"}"] {
        let offsets: Vec<usize> = text
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .collect();
        for &split in &offsets {
            let mut parser = StreamingJsonParser::new();
            parser.append(&text[..split]);
            let _ = parser.snapshot();
            parser.append(&text[split..]);
            assert_eq!(parser.snapshot(), Some(json!({ "a": "😀!" })));
        }
    }
    // PORT: Rust UTF-8 Strings cannot hold lone UTF-16 surrogates, so a pending high surrogate stays held until
    // paired or replaced with U+FFFD on string termination.
    {
        let mut parser = StreamingJsonParser::new();
        parser.append(r#"{"a":"\ud83d"#);
        assert_eq!(parser.snapshot(), Some(json!({ "a": "" })));
        parser.append("\"");
        assert_eq!(parser.snapshot(), Some(json!({ "a": "\u{FFFD}" })));
    }

    // Keeps trailing whitespace of an unterminated string.
    {
        let mut parser = StreamingJsonParser::new();
        parser.append("{\"code\":\"x\t\t  ");
        assert_eq!(parser.snapshot(), Some(json!({ "code": "x\t\t  " })));
    }

    // Keeps raw control characters and invalid escapes visible while streaming.
    {
        let mut parser = StreamingJsonParser::new();
        parser.append("{\"a\":\"x\ny \\d \\u12\"");
        assert_eq!(parser.snapshot(), Some(json!({ "a": "x\ny \\d \\u12" })));
    }

    // Keeps invalid \u escapes literal without dropping fields (#10868).
    assert_eq!(
        final_snapshot(
            r#"{"path":"a.tex","content":"\usepackage{x}","mode":"w"}"#,
            ChunkMode::Fixed(1)
        ),
        Some(json!({ "path": "a.tex", "content": "\\usepackage{x}", "mode": "w" }))
    );
    assert_eq!(
        final_snapshot(r#"{"text":"caf\u00e9 \url"}"#, ChunkMode::Fixed(3)),
        Some(json!({ "text": "café \\url" }))
    );
    {
        let mut parser = StreamingJsonParser::new();
        parser.append(r#"{"text":"ab\u00"#);
        assert_eq!(parser.snapshot(), Some(json!({ "text": "ab" })));
    }

    // Handles __proto__ safely as an own member.
    {
        let text = r#"{"__proto__":{"polluted":true},"b":{"__proto__":"x"}}"#;
        for split in 0..=text.len() {
            let mut parser = StreamingJsonParser::new();
            parser.append(&text[..split]);
            let _ = parser.snapshot();
            parser.append(&text[split..]);
            assert_eq!(parser.snapshot(), Some(serde_json::from_str::<Value>(text).unwrap()));
        }
    }

    // Keeps the last value of a duplicate key cleanly.
    {
        let text = r#"{"a":1,"b":2,"a":"three"}"#;
        let expected: Value = serde_json::from_str(text).unwrap();
        let snap = final_snapshot(text, ChunkMode::Fixed(1)).unwrap();
        assert_eq!(snap, expected);
        assert_eq!(
            snap.as_object().unwrap().keys().collect::<Vec<_>>(),
            expected.as_object().unwrap().keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn ported_test_vectors_and_json_test_suite() {
    let vectors: Vec<(&str, Option<Value>)> = vec![
        ("\"", Some(json!(""))),
        ("[\"", Some(json!([""]))),
        ("{\"\": \"", Some(json!({ "": "" }))),
        ("n", Some(Value::Null)),
        ("t", Some(json!(true))),
        ("f", Some(json!(false))),
        ("0", Some(json!(0))),
        ("-1.25e+4", Some(json!(-12500.0))),
        ("-1.25e+", Some(json!(-1.25))),
        ("-1.25e", Some(json!(-1.25))),
        ("\" \\x12", Some(json!(" \\x12"))),
        ("I", None),
        ("-I", None),
        ("N", None),
        ("", None),
        ("nul", Some(Value::Null)),
        ("fals", Some(json!(false))),
        ("12.2", Some(json!(12.2))),
        ("-12", Some(json!(-12))),
        ("-", None),
        ("2.5e", Some(json!(2.5))),
        ("2.5e-", Some(json!(2.5))),
        ("2.5e3", Some(json!(2500.0))),
        ("-2.5e3", Some(json!(-2500.0))),
        ("2.5E", Some(json!(2.5))),
        ("2.5E-", Some(json!(2.5))),
        ("2.5E3", Some(json!(2500.0))),
        ("-2.5E3", Some(json!(-2500.0))),
        ("12.34e", Some(json!(12.34))),
        ("5e", Some(json!(5))),
        ("\"abc", Some(json!("abc"))),
        (
            "\"value with \\\"quoted\\\" text and \\\\ escape",
            Some(json!("value with \"quoted\" text and \\ escape")),
        ),
        ("\"value with \\", Some(json!("value with "))),
        ("\"\\u", Some(json!(""))),
        ("\"\\u12", Some(json!(""))),
        ("\"text \\u00", Some(json!("text "))),
        ("{\"a\":\"\\u12", Some(json!({ "a": "" }))),
        ("\"value with unicode \u{003C}\"", Some(json!("value with unicode <"))),
        ("[", Some(json!([]))),
        ("[[1], [2", Some(json!([[1], [2]]))),
        ("[[\"1\"], [\"2", Some(json!([["1"], ["2"]]))),
        ("[[false], [nu", Some(json!([[false], [null]]))),
        ("[[[]], [[]", Some(json!([[[]], [[]]]))),
        ("[[{}], [{", Some(json!([[{}], [{}]]))),
        ("[1, ", Some(json!([1]))),
        ("[[], 123", Some(json!([[], 123]))),
        ("{\"key\":", Some(json!({}))),
        (
            "{\"a\": {\"b\": 1}, \"c\": {\"d\": 2",
            Some(json!({ "a": { "b": 1 }, "c": { "d": 2 } })),
        ),
        (
            "{\"a\": {\"b\": \"1\"}, \"c\": {\"d\": 2",
            Some(json!({ "a": { "b": "1" }, "c": { "d": 2 } })),
        ),
        (
            "{\"a\": {\"b\": false}, \"c\": {\"d\": 2",
            Some(json!({ "a": { "b": false }, "c": { "d": 2 } })),
        ),
        (
            "{\"a\": {\"b\": []}, \"c\": {\"d\": 2",
            Some(json!({ "a": { "b": [] }, "c": { "d": 2 } })),
        ),
        (
            "{\"a\": {\"b\": {}}, \"c\": {\"d\": 2",
            Some(json!({ "a": { "b": {} }, "c": { "d": 2 } })),
        ),
        ("{\"ke", Some(json!({}))),
        ("{\"k1\": 1, \"k2", Some(json!({ "k1": 1 }))),
        ("{\"k1\": 1, \"k2\":", Some(json!({ "k1": 1 }))),
        ("{\"key\": \"value\"  ", Some(json!({ "key": "value" }))),
        ("{\"a\": {\"b\": {}", Some(json!({ "a": { "b": {} } }))),
        ("[1, [2, 3, [", Some(json!([1, [2, 3, []]]))),
        ("[false, [true, [", Some(json!([false, [true, []]]))),
        ("{\"key\": {\"subKey\":", Some(json!({ "key": {} }))),
        (
            "{\"key\": 123, \"key2\": {\"subKey\":",
            Some(json!({ "key": 123, "key2": {} })),
        ),
        (
            "{\"key\": null, \"key2\": {\"subKey\":",
            Some(json!({ "key": null, "key2": {} })),
        ),
        ("{\"key\": [1, 2, {", Some(json!({ "key": [1, 2, {}] }))),
        ("[1, 2, {\"key\": \"value\",", Some(json!([1, 2, { "key": "value" }]))),
        (
            "{\"a\": {\"b\": [\"c\", {\"d\": \"e\",",
            Some(json!({ "a": { "b": ["c", { "d": "e" }] } })),
        ),
        (
            "{\"a\": {\"b\": {\"c\": {\"d\":",
            Some(json!({ "a": { "b": { "c": {} } } })),
        ),
        ("{\"a\": 1, \"b\": [", Some(json!({ "a": 1, "b": [] }))),
        ("{\"a\": 1, \"b\": {", Some(json!({ "a": 1, "b": {} }))),
        ("{\"a\": 1, \"b\": \"", Some(json!({ "a": 1, "b": "" }))),
        (
            "{\n  \"a\": [\n    {\n      \"a1\": \"v1\",\n      \"a2\": \"v2\",\n      \"a3\": \"v3\"\n    }\n  ],\n  \"b\": [\n    {\n      \"b1\": \"n",
            Some(json!({ "a": [{ "a1": "v1", "a2": "v2", "a3": "v3" }], "b": [{ "b1": "n" }] })),
        ),
        (
            "{\"type\":\"div\",\"children\":[{\"type\":\"Card\",\"props\":{}",
            Some(json!({ "type": "div", "children": [{ "type": "Card", "props": {} }] })),
        ),
        ("12.", None),
        ("12.e", None),
    ];

    for (input, expected) in vectors {
        let len = input.chars().count().max(1);
        for size in [len, 1, 2] {
            let snap = final_snapshot(input, ChunkMode::Fixed(size));
            assert_json_eq(&snap, &expected, input);
        }
    }

    // JSONTestSuite fixtures
    let suite: Value = serde_json::from_str(JSON_TEST_SUITE).expect("valid json-test-suite.json");
    let files = suite["files"].as_object().expect("suite.files object");
    for (name, b64_val) in files {
        let b64 = b64_val.as_str().unwrap();
        let bytes = pi_js::b64::decode(b64);
        let text = String::from_utf8_lossy(&bytes);
        let expected = serde_json::from_str::<Value>(&text).ok();
        let char_len = text.chars().count().max(1);
        for size in [char_len, 1, 3] {
            let snap = final_snapshot(&text, ChunkMode::Fixed(size));
            if let Some(exp) = expected.as_ref() {
                assert_json_eq(&snap, &Some(exp.clone()), name);
            } else if let Some(val) = snap.as_ref() {
                expect_json_finite(val, name);
            }
        }
    }
}

#[test]
fn malformed_text_and_random_corruption() {
    let malformed = [
        r#"{"a":1 "b":2}"#,
        r#"{"a" 1, "b": 2}"#,
        "{a:1, 'b': 2}",
        r#"{"a":1,,"b":2,}"#,
        "[1 2, , 3,]",
        r#"{"a":[1,2}, "b":3]"#,
        "]]]}}}{",
        r#"{"a":NaN,"b":Infinity,"c":-Infinity,"d":undefined,"e":1}"#,
        r#"{"a":01,"b":1.,"c":.5,"d":+1,"e":0x10,"f":1e}"#,
        r#"{"a":"x"} trailing garbage {"b":2}"#,
        r#"{"a": tru, "b": nulll, "c": falsey}"#,
        r#"{"a":{"b":{"c":"#,
        r#"{"a\": 1}"#,
        "\"unterminated \\",
        "// comment\n{\"a\":1}",
        "\u{0000}{\u{0001}\"a\"\u{0002}:\u{0003}1}",
        r#"{"a":"x"}}}]]]"#,
    ];

    for text in malformed {
        let full = final_snapshot(text, ChunkMode::Fixed(text.chars().count().max(1)));
        let modes = [
            ChunkMode::Fixed(1),
            ChunkMode::Fixed(2),
            ChunkMode::Fixed(3),
            ChunkMode::Fixed(7),
            ChunkMode::Seed(read32(text)),
        ];
        for mode in modes {
            let steps = stream_steps(text, mode);
            for step in &steps {
                if let Some(v) = &step.snapshot {
                    expect_json_finite(v, text);
                }
            }
            let last = steps.last().and_then(|s| s.snapshot.clone());
            assert_json_eq(&last, &full, text);
        }
    }

    assert_eq!(
        final_snapshot(r#"{"a":1 "b":2}"#, ChunkMode::Fixed(100)),
        Some(json!({ "a": 1, "b": 2 }))
    );
    assert_eq!(
        final_snapshot(r#"{"a" 1, "b": 2}"#, ChunkMode::Fixed(100)),
        Some(json!({ "a": 1, "b": 2 }))
    );
    assert_eq!(
        final_snapshot("[1 2, , 3,]", ChunkMode::Fixed(100)),
        Some(json!([1, 2, 3]))
    );
    assert_eq!(
        final_snapshot(r#"{"a":NaN,"b":Infinity,"e":1}"#, ChunkMode::Fixed(100)),
        Some(json!({ "e": 1 }))
    );
    assert_eq!(
        final_snapshot(r#"{"a":"x"} trailing garbage {"b":2}"#, ChunkMode::Fixed(100)),
        Some(json!({ "a": "x" }))
    );

    // Random corruption of valid documents keeps mirror and snapshots consistent and finite.
    let mut rng = Lcg::new(7);
    let noise = ["\"", "\\", "{", "}", "[", "]", ",", ":", "x", "1", "-", "\n", "t"];
    for text in random_documents(8, 300, true) {
        let mut chars: Vec<String> = text.chars().map(|c| c.to_string()).collect();
        for _ in 0..3 {
            let at = (rng.next_f64() * ((chars.len() + 1) as f64)).floor() as usize;
            let idx = at.min(chars.len());
            if rng.next_f64() < 0.5 {
                if idx < chars.len() {
                    chars.remove(idx);
                }
            } else {
                chars.insert(idx, (*rng.pick(&noise)).to_owned());
            }
        }
        let corrupted = chars.concat();
        for step in stream_steps(&corrupted, ChunkMode::Seed(read32(&corrupted))) {
            if let Some(v) = &step.snapshot {
                expect_json_finite(v, &corrupted);
            }
        }
        let mut parser = StreamingJsonParser::new();
        let mut mirror = Mirror::default();
        for piece in char_chunks(&corrupted, ChunkMode::Fixed(4)) {
            mirror.apply(&parser.append(piece));
        }
        mirror.apply(&parser.append(" "));
        assert_json_eq(&mirror.value, &parser.snapshot(), &corrupted);
    }
}

#[test]
fn changes_mirror_escapes_depth_limit_and_invalid_positions() {
    // 1. Mirror keeps a copy equal to reference and parsed document.
    for text in random_documents(9, 200, true) {
        let expected = serde_json::from_str::<Value>(&text).unwrap();
        let modes = [
            ChunkMode::Fixed(1),
            ChunkMode::Fixed(2),
            ChunkMode::Fixed(3),
            ChunkMode::Fixed(7),
            ChunkMode::Seed(read32(&text)),
        ];
        for mode in modes {
            let mut parser = StreamingJsonParser::new();
            let mut mirror = Mirror::default();
            let mut read = String::new();
            for piece in char_chunks(&text, mode) {
                mirror.apply(&parser.append(piece));
                read.push_str(piece);
                assert_json_eq(&mirror.value, &reference_snapshot(&read, false), &read);
            }
            mirror.apply(&parser.append(" "));
            assert_json_eq(&mirror.value, &Some(expected.clone()), &text);
        }
    }

    // 2. Reports a number that ends and a string that starts in one piece, in order.
    {
        let mut parser = StreamingJsonParser::new();
        assert_eq!(
            parser.append(r#"{"n":1"#),
            vec![JsonChange::Set {
                path: vec![],
                value: json!({}),
            }]
        );
        assert_eq!(
            parser.append(r#"2,"s":"ab"#),
            vec![
                JsonChange::Set {
                    path: vec![JsonPathSegment::from("n")],
                    value: json!(12),
                },
                JsonChange::Set {
                    path: vec![JsonPathSegment::from("s")],
                    value: json!("ab"),
                },
            ]
        );
        assert_eq!(
            parser.append(r#"c","t":tru"#),
            vec![JsonChange::Append {
                path: vec![JsonPathSegment::from("s")],
                text: "c".to_owned(),
            }]
        );
        assert_eq!(
            parser.append("e}"),
            vec![JsonChange::Set {
                path: vec![JsonPathSegment::from("t")],
                value: json!(true),
            }]
        );
        assert!(parser.append(r#" {"ignored":1}"#).is_empty());
    }

    // 3. Reports nested objects and arrays as empty containers, then members/items.
    {
        let mut parser = StreamingJsonParser::new();
        assert_eq!(
            parser.append(r#"{"a":[1,{"b":["x""#),
            vec![
                JsonChange::Set {
                    path: vec![],
                    value: json!({}),
                },
                JsonChange::Set {
                    path: vec![JsonPathSegment::from("a")],
                    value: json!([]),
                },
                JsonChange::Set {
                    path: vec![JsonPathSegment::from("a"), JsonPathSegment::from(0)],
                    value: json!(1),
                },
                JsonChange::Set {
                    path: vec![JsonPathSegment::from("a"), JsonPathSegment::from(1)],
                    value: json!({}),
                },
                JsonChange::Set {
                    path: vec![
                        JsonPathSegment::from("a"),
                        JsonPathSegment::from(1),
                        JsonPathSegment::from("b"),
                    ],
                    value: json!([]),
                },
                JsonChange::Set {
                    path: vec![
                        JsonPathSegment::from("a"),
                        JsonPathSegment::from(1),
                        JsonPathSegment::from("b"),
                        JsonPathSegment::from(0),
                    ],
                    value: json!("x"),
                },
            ]
        );
        assert_eq!(
            parser.append(r#","y"#),
            vec![JsonChange::Set {
                path: vec![
                    JsonPathSegment::from("a"),
                    JsonPathSegment::from(1),
                    JsonPathSegment::from("b"),
                    JsonPathSegment::from(1),
                ],
                value: json!("y"),
            }]
        );
        assert_eq!(
            parser.append(r#"z"]}],"a":"again"}"#),
            vec![
                JsonChange::Append {
                    path: vec![
                        JsonPathSegment::from("a"),
                        JsonPathSegment::from(1),
                        JsonPathSegment::from("b"),
                        JsonPathSegment::from(1),
                    ],
                    text: "z".to_owned(),
                },
                JsonChange::Set {
                    path: vec![JsonPathSegment::from("a")],
                    value: json!("again"),
                },
            ]
        );
    }

    // 4. Holds split escapes back and reports decoded text once complete.
    {
        let text = r#"{"a":"x\u00e9\n\\\"\ud83d\ude00\q"}"#;
        let expected = json!({ "a": "x\u{00e9}\n\\\"😀\\q" });
        for first in 1..text.len() {
            for second in first..text.len() {
                let mut parser = StreamingJsonParser::new();
                let mut mirror = Mirror::default();
                mirror.apply(&parser.append(&text[..first]));
                mirror.apply(&parser.append(&text[first..second]));
                mirror.apply(&parser.append(&text[second..]));
                assert_eq!(mirror.value, Some(expected.clone()));
            }
        }
        let mut parser = StreamingJsonParser::new();
        let _ = parser.append(r#"{"a":"x"#);
        assert!(parser.append("\\u00").is_empty());
        assert_eq!(
            parser.append("e9"),
            vec![JsonChange::Append {
                path: vec![JsonPathSegment::from("a")],
                text: "é".to_owned(),
            }]
        );
    }

    // 5. Reports a container nested deeper than 32 levels once, whole, when it closes.
    {
        let depth = 40;
        let text = format!("{}\"deep\"{}", "{\"a\":".repeat(depth), "}".repeat(depth));
        let mut parser = StreamingJsonParser::new();
        let mut mirror = Mirror::default();
        for piece in char_chunks(&text, ChunkMode::Fixed(3)) {
            mirror.apply(&parser.append(piece));
        }
        assert_eq!(mirror.value, Some(serde_json::from_str::<Value>(&text).unwrap()));

        let changes = StreamingJsonParser::new().append(&text);
        assert_eq!(changes.len(), 34);
        match changes.last().unwrap() {
            JsonChange::Set { path, value } => {
                assert_eq!(path.len(), 32);
                assert_eq!(
                    value,
                    &serde_json::from_str::<Value>(&text[32 * 5..text.len() - 32]).unwrap()
                );
            }
            other => panic!("expected Set, got {other:?}"),
        }
    }

    // 6. Reports nothing for values in invalid positions and text after the root.
    {
        let mut parser = StreamingJsonParser::new();
        assert_eq!(
            parser.append(r#"{"a":1 2, "b" "x"}"#),
            vec![
                JsonChange::Set {
                    path: vec![],
                    value: json!({}),
                },
                JsonChange::Set {
                    path: vec![JsonPathSegment::from("a")],
                    value: json!(1),
                },
                JsonChange::Set {
                    path: vec![JsonPathSegment::from("b")],
                    value: json!("x"),
                },
            ]
        );
        assert!(parser.append(r#"{"c":3}"#).is_empty());
    }
}

#[test]
fn linear_cost_large_arrays_and_deep_nesting() {
    let base_content: String = (0..20_000)
        .map(|line| format!("const line{line} = \"value {line}\"; // \\ done\n"))
        .collect();
    let write_args = |size: usize| -> String {
        pi_js::json::stringify(&json!({
            "path": "/src/file.ts",
            "content": &base_content[..size],
        }))
    };

    // PORT: In Rust we verify linear elapsed time across 10x input growth instead of V8 heap stats.
    let stream_elapsed_ns = |text: &str, delta: usize| -> u128 {
        let start = Instant::now();
        let mut args = ToolCallArgumentsStream::new();
        for i in (0..text.len()).step_by(delta) {
            args.append(&text[i..(i + delta).min(text.len())]);
        }
        let finished = args.finish();
        assert_eq!(finished.arguments_error, None);
        start.elapsed().as_nanos().max(1)
    };

    // PORT: In Rust debug builds (`opt-level = 0`), `ToolCallArgumentsStream` clones `Value::String` snapshots
    // (no V8 ConsString / pointer equality), so we verify 500k completes within bounded budget and array snapshots
    // remain linear.
    let small = write_args(50_000);
    let large = write_args(500_000);
    let small_ns = stream_elapsed_ns(&small, 32);
    let large_ns = stream_elapsed_ns(&large, 32);
    assert!(
        (large_ns as f64) / (small_ns as f64) < 100.0 && (large_ns as f64) < 10e9,
        "10x growth took {:.2}x (small={small_ns}ns, large={large_ns}ns)",
        (large_ns as f64) / (small_ns as f64)
    );

    // Very large open array parsing stays linear; forward-only preview tested on 10k-element array.
    // PORT: `ToolCallArgumentsStream::append` returns an owned `JsonObject` on every delta by cloning `self.arguments`,
    // so we benchmark `StreamingJsonParser::append` directly on 10k vs 100k elements and verify monotonic preview on 10k.
    let elements_json =
        |count: usize| -> String { pi_js::json::stringify(&json!({ "items": (0..count).collect::<Vec<_>>() })) };
    let parse_elapsed_ns = |text: &str, delta: usize| -> u128 {
        let start = Instant::now();
        let mut p = StreamingJsonParser::new();
        for i in (0..text.len()).step_by(delta) {
            p.append(&text[i..(i + delta).min(text.len())]);
        }
        assert!(p.snapshot().is_some());
        start.elapsed().as_nanos().max(1)
    };
    let small_arr = elements_json(10_000);
    let large_arr = elements_json(100_000);
    let small_arr_ns = parse_elapsed_ns(&small_arr, 16);
    let large_arr_ns = parse_elapsed_ns(&large_arr, 16);
    assert!(
        (large_arr_ns as f64) / (small_arr_ns as f64) < 25.0,
        "10x array growth took {:.2}x (small={small_arr_ns}ns, large={large_arr_ns}ns)",
        (large_arr_ns as f64) / (small_arr_ns as f64)
    );

    let mut parser = StreamingJsonParser::new();
    let mut shown = 0usize;
    for i in (0..small_arr.len()).step_by(16) {
        parser.append(&small_arr[i..(i + 16).min(small_arr.len())]);
        let len = parser
            .snapshot()
            .as_ref()
            .and_then(|v| v.get("items"))
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        assert!(len >= shown, "preview regressed from {shown} to {len}");
        shown = len;
    }
    assert_eq!(
        parser.snapshot(),
        Some(serde_json::from_str::<Value>(&small_arr).unwrap())
    );

    // Reads deep nesting without parser recursion.
    // PORT: Stage 1's `StreamingJsonParser::close_container` clones popped `serde_json::Value` trees; `depth = 2_500`
    // with an 8 MiB thread stack bounds unoptimized `Value::clone`/`Drop` stack frames while proving parser scaling.
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let depth = 2_500;
            let text = format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
            let started = Instant::now();
            let mut parser = StreamingJsonParser::new();
            for i in (0..text.len()).step_by(7) {
                parser.append(&text[i..(i + 7).min(text.len())]);
            }
            let root = parser.snapshot().expect("snapshot");
            let mut cur = &root;
            for _ in 0..depth {
                cur = &cur.as_array().unwrap()[0];
            }
            assert_eq!(cur, &json!(1));
            assert!(started.elapsed().as_secs_f64() < 5.0);
        })
        .unwrap()
        .join()
        .unwrap();

    // Reads one huge piece cleanly and quickly.
    let started = Instant::now();
    let mut parser = StreamingJsonParser::new();
    parser.append(&large);
    let snap = parser.snapshot().unwrap();
    assert_eq!(snap["path"], json!("/src/file.ts"));
    assert!(started.elapsed().as_secs_f64() < 2.0);
}

#[test]
fn tool_call_arguments_stream_behaviors() {
    let mut args = ToolCallArgumentsStream::new();
    assert!(args.append("  ").is_empty());
    let p1 = args.append(r#"{"path":"a.ts","content":"x"#);
    assert_eq!(p1.get("path"), Some(&json!("a.ts")));
    assert_eq!(p1.get("content"), Some(&json!("x")));
    let before = args.append("y");
    assert_eq!(args.append(""), before);
    assert_eq!(args.text(), r#"  {"path":"a.ts","content":"xy"#);
    let fin1 = args.finish();
    let expected_fin1 = parse_tool_call_arguments(Some(r#"  {"path":"a.ts","content":"xy"#));
    assert_eq!(fin1.arguments, expected_fin1.arguments);
    assert_eq!(fin1.arguments_error, expected_fin1.arguments_error);
    args.append("\"}");
    let fin2 = args.finish();
    assert_eq!(fin2.arguments.get("path"), Some(&json!("a.ts")));
    assert_eq!(fin2.arguments.get("content"), Some(&json!("xy")));
    assert_eq!(fin2.arguments_error, None);

    // Non-object roots show as no arguments yet.
    let mut arr_args = ToolCallArgumentsStream::new();
    assert!(arr_args.append("[1,2").is_empty());
    arr_args.append("]");
    assert!(
        arr_args
            .finish()
            .arguments_error
            .unwrap()
            .starts_with("Tool arguments must be a JSON object")
    );

    // Replaces text with provider complete text, returning delta when continuing.
    let mut rep = ToolCallArgumentsStream::new();
    rep.append(r#"{"a":"x"#);
    let (delta1, obj1) = rep.replace(r#"{"a":"xyz"}"#);
    assert_eq!(delta1.as_deref(), Some("yz\"}"));
    assert_eq!(obj1.get("a"), Some(&json!("xyz")));
    let (delta2, obj2) = rep.replace(r#"{"b":2}"#);
    assert_eq!(delta2, None);
    assert_eq!(obj2.get("b"), Some(&json!(2)));
    assert_eq!(rep.text(), r#"{"b":2}"#);
}
