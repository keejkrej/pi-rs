//! Behavior tests for the pi-js data modules that the Node vectors (`pijs_data_vectors.rs`) do
//! not cover: Rust-side value shapes, documented deviations, streaming state and test seams.

use std::collections::BTreeMap;

use pi_js::{b64, crypto, json, num, str16, text, uri};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// ---- json ----

#[test]
fn parse_normalizes_numbers_like_js() {
    // Integral doubles within 2^53 become integer Numbers, so they equal `json!` literals and
    // deserialize into integer types.
    assert_eq!(json::parse("1.0").unwrap(), json!(1));
    assert_eq!(json::parse("1e3").unwrap(), json!(1000));
    assert_eq!(json::parse("-5").unwrap(), json!(-5));
    assert_eq!(json::parse("-0").unwrap(), json!(0));
    assert_eq!(json::parse("1.5").unwrap(), json!(1.5));
    // 9007199254740993 is not a double; JS holds 9007199254740992.
    assert_eq!(json::parse("9007199254740993").unwrap(), json!(9007199254740992u64));
    assert_eq!(json::parse("1e300").unwrap().as_f64(), Some(1e300));
    assert_eq!(json::parse_as::<u8>("2.0").unwrap(), 2);
}

#[test]
fn parse_port_deviations() {
    // PORT: Infinity does not fit a Value; JSON.stringify would print `null` for it anyway.
    assert_eq!(json::parse("1e400").unwrap(), Value::Null);
    assert_eq!(json::parse("[-1e400]").unwrap(), json!([null]));
    // PORT: lone surrogate escapes become U+FFFD; pairs combine.
    assert_eq!(json::parse(r#""\ud800""#).unwrap(), json!("\u{FFFD}"));
    assert_eq!(json::parse(r#""a\udc00b""#).unwrap(), json!("a\u{FFFD}b"));
    assert_eq!(json::parse(r#""😀""#).unwrap(), json!("😀"));
    assert_eq!(json::parse(r#""\ud83d😀""#).unwrap(), json!("\u{FFFD}😀"));
}

#[test]
fn parse_orders_index_keys_first() {
    let v = json::parse(r#"{"b":1,"10":2,"a":3,"2":4,"01":5,"4294967295":6,"4294967294":7}"#).unwrap();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["2", "10", "4294967294", "b", "a", "01", "4294967295"]);
    // Duplicate keys keep the first position and the last value.
    assert_eq!(
        json::stringify(&json::parse(r#"{"b":1,"a":2,"b":3}"#).unwrap()),
        r#"{"b":3,"a":2}"#
    );
}

#[test]
fn parse_handles_deep_nesting() {
    // V8's parser is iterative; so is ours. Run on a big stack because dropping and
    // serializing a deep `Value` recurse.
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(|| {
            let depth = 100_000;
            let src = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
            let v = json::parse(&src).unwrap();
            let mut cur = &v;
            let mut n = 1;
            while let Some(inner) = cur.as_array().and_then(|a| a.first()) {
                cur = inner;
                n += 1;
            }
            assert_eq!(n, depth);
            let unterminated = "[".repeat(depth);
            let err = json::parse(&unterminated).unwrap_err();
            assert_eq!(err.to_js_string(), "SyntaxError: Unexpected end of JSON input");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn parse_errors_are_syntax_errors() {
    let err = json::parse("{\"a\":1,}").unwrap_err();
    assert_eq!(err.name(), "SyntaxError");
    assert_eq!(
        err.message(),
        "Expected double-quoted property name in JSON at position 7 (line 1 column 8)"
    );
    assert_eq!(err.to_string(), err.message());
    // A value that parses but does not fit the target type is a TypeError.
    let err = json::parse_as::<u32>("\"x\"").unwrap_err();
    assert_eq!(err.name(), "TypeError");
    assert_eq!(json::parse_as::<u32>("x").unwrap_err().name(), "SyntaxError");
}

#[derive(Serialize)]
struct Record {
    b: u32,
    #[serde(rename = "1")]
    one: bool,
    a: Option<String>,
    n: f64,
    nested: BTreeMap<String, i64>,
}

#[test]
fn stringify_rust_values_like_js_objects() {
    let nested = BTreeMap::from([("x".to_string(), 1), ("3".to_string(), 2), ("0".to_string(), 3)]);
    let r = Record {
        b: 1,
        one: true,
        a: None,
        n: 1.0,
        nested,
    };
    assert_eq!(
        json::stringify(&r),
        r#"{"1":true,"b":1,"a":null,"n":1,"nested":{"0":3,"3":2,"x":1}}"#
    );
    assert_eq!(
        json::stringify_pretty(&r, "  "),
        "{\n  \"1\": true,\n  \"b\": 1,\n  \"a\": null,\n  \"n\": 1,\n  \"nested\": {\n    \"0\": 3,\n    \"3\": 2,\n    \"x\": 1\n  }\n}"
    );
    assert_eq!(json::stringify(&f64::NAN), "null");
    assert_eq!(json::stringify(&f64::NEG_INFINITY), "null");
    assert_eq!(json::stringify(&-0.0f64), "0");
    assert_eq!(json::stringify(&u64::MAX), "18446744073709552000");
    assert_eq!(json::stringify(&i64::MIN), "-9223372036854776000");
    assert_eq!(json::stringify(&(1u128 << 70)), "1.1805916207174113e+21");
    assert_eq!(json::stringify(&0.1f32), "0.10000000149011612");
    assert_eq!(json::stringify(&b"ab"[..]), "[97,98]");
    assert_eq!(
        json::stringify("\u{7f}\u{2028}</script>"),
        "\"\u{7f}\u{2028}</script>\""
    );
    assert_eq!(
        json::stringify_pretty(&json!({"a": [], "b": {}}), "\t"),
        "{\n\t\"a\": [],\n\t\"b\": {}\n}"
    );
    assert_eq!(json::stringify_pretty(&json!([1]), ""), "[1]");
}

#[test]
fn stringify_reports_unserializable_values() {
    let bad = BTreeMap::from([((1u8, 2u8), 3u8)]);
    let err = json::try_stringify(&bad).unwrap_err();
    assert_eq!(err.name(), "TypeError");
    assert!(json::try_stringify_pretty(&bad, "  ").is_err());
    assert!(std::panic::catch_unwind(|| json::stringify(&bad)).is_err());
}

#[test]
fn number_value_matches_parse() {
    assert_eq!(json::number_value(1.0), json!(1));
    assert_eq!(json::number_value(-0.0), json!(0));
    assert_eq!(json::number_value(-3.0), json!(-3));
    assert_eq!(json::number_value(0.5), json!(0.5));
    assert_eq!(
        json::number_value(9007199254740994.0).as_f64(),
        Some(9007199254740994.0)
    );
    assert_eq!(json::number_value(f64::NAN), Value::Null);
    assert_eq!(json::number_value(f64::INFINITY), Value::Null);
    assert_eq!(json::number_to_string(1e21), "1e+21");
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Patch {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "pi_js::json::double_option"
    )]
    x: Option<Option<u32>>,
}

#[test]
fn double_option_distinguishes_absent_null_and_value() {
    assert_eq!(json::parse_as::<Patch>("{}").unwrap(), Patch { x: None });
    assert_eq!(
        json::parse_as::<Patch>(r#"{"x":null}"#).unwrap(),
        Patch { x: Some(None) }
    );
    assert_eq!(
        json::parse_as::<Patch>(r#"{"x":7}"#).unwrap(),
        Patch { x: Some(Some(7)) }
    );
    assert_eq!(json::stringify(&Patch { x: None }), "{}");
    assert_eq!(json::stringify(&Patch { x: Some(None) }), r#"{"x":null}"#);
    assert_eq!(json::stringify(&Patch { x: Some(Some(7)) }), r#"{"x":7}"#);
}

// ---- num ----

#[test]
fn number_constants_and_clamped_arguments() {
    assert_eq!(num::MAX_SAFE_INTEGER, 9007199254740991);
    assert_eq!(num::MIN_SAFE_INTEGER, -9007199254740991);
    // JS throws a RangeError here; the port clamps.
    assert_eq!(num::to_fixed(1.0, 101), num::to_fixed(1.0, 100));
    assert_eq!(num::to_precision(123.0, 0), "1e+2");
    assert_eq!(num::to_precision(1.0, 101), num::to_precision(1.0, 100));
    assert_eq!(num::to_string_radix(255.0, 1), "11111111");
    assert_eq!(num::to_string_radix(35.0, 99), "z");
    // `parseInt(s, 4294967295)`: ToInt32 makes the radix -1.
    assert!(num::parse_int("1", Some(u32::MAX)).is_nan());
    assert_eq!(num::parse_int("ff", Some(16)), 255.0);
    assert_eq!(num::parse_int("-0", None).to_bits(), (-0.0f64).to_bits());
}

// ---- str16 ----

#[test]
fn utf16_and_byte_offsets_convert() {
    let s = "a😀b"; // bytes: a=0, 😀=1..5, b=5; units: a=0, 😀=1..3, b=3
    let units: Vec<usize> = [0, 1, 2, 3, 4, 5, 6, 100]
        .iter()
        .map(|&b| str16::byte_to_utf16(s, b))
        .collect();
    assert_eq!(units, [0, 1, 1, 1, 1, 3, 4, 4]);
    let bytes: Vec<usize> = [0, 1, 2, 3, 4, 100]
        .iter()
        .map(|&u| str16::utf16_to_byte(s, u))
        .collect();
    assert_eq!(bytes, [0, 1, 1, 5, 6, 6]);
    assert_eq!(str16::to_utf16(s), [0x61, 0xD83D, 0xDE00, 0x62]);
    assert_eq!(str16::from_utf16_lossy(&[0x61, 0xD83D]), "a\u{FFFD}");
    assert_eq!(str16::from_utf16_lossy(&[0xD83D, 0xDE00]), "😀");
}

// ---- text ----

#[test]
fn fatal_stream_decoder_errors_at_flush_and_recovers() {
    let options = text::TextDecoderOptions {
        fatal: true,
        ignore_bom: false,
    };
    let mut d = text::Utf8StreamDecoder::with_options(options);
    assert_eq!(d.try_decode(&[0xE2, 0x82], true).unwrap(), "");
    let err = d.try_decode(&[], false).unwrap_err();
    assert_eq!(err.name(), "TypeError");
    assert_eq!(err.code(), Some("ERR_ENCODING_INVALID_ENCODED_DATA"));
    assert_eq!(err.message(), "The encoded data was not valid for encoding utf-8");
    // The failed call reset the decoder: a new stream starts (BOM stripped again).
    assert_eq!(d.try_decode(&[0xEF, 0xBB, 0xBF, 0x41], false).unwrap(), "A");
    // `decode` ignores `fatal` and substitutes U+FFFD.
    assert_eq!(d.decode(&[0xFF], false), "\u{FFFD}");
}

#[test]
fn stream_decoder_keeps_bom_after_stream_start() {
    let mut d = text::Utf8StreamDecoder::new();
    assert_eq!(d.decode(b"a", true), "a");
    assert_eq!(d.decode(&[0xEF, 0xBB, 0xBF], true), "\u{FEFF}");
    assert_eq!(d.decode(&[0xF0, 0x9F], true), "");
    assert_eq!(d.decode(&[0x98, 0x80], false), "😀");
    assert_eq!(text::utf8_byte_length("é😀a"), 7);
    // `Buffer#toString` keeps a BOM; `TextDecoder` strips it.
    assert_eq!(text::buffer_to_string(&[0xEF, 0xBB, 0xBF, 0x61]), "\u{FEFF}a");
    assert_eq!(text::decode_utf8(&[0xEF, 0xBB, 0xBF, 0x61]), "a");
}

// ---- b64 / uri ----

#[test]
fn base64_round_trips() {
    let all: Vec<u8> = (0..=255).collect();
    assert_eq!(b64::decode(&b64::encode(&all)), all);
    assert_eq!(b64::decode_url(&b64::encode_url(&all)), all);
    // Both decoders accept both alphabets, like Node.
    assert_eq!(b64::decode("-_-_"), b64::decode("+/+/"));
    assert_eq!(b64::atob_bytes("/w==").unwrap(), [0xFF]);
    assert_eq!(b64::btoa(&b64::atob("AP+A").unwrap()).unwrap(), "AP+A");
}

#[test]
fn decode_uri_keeps_reserved_escapes_verbatim() {
    assert_eq!(uri::decode_uri("%2f%3A%41%e2%82%ac").unwrap(), "%2f%3AA€");
    assert_eq!(uri::decode_uri_component("%2f%3A%41").unwrap(), "/:A");
    let err = uri::decode_uri_component("%E2%82").unwrap_err();
    assert_eq!(err.to_js_string(), "URIError: URI malformed");
}

// ---- crypto ----

#[test]
fn random_uuid_is_lowercase_v4() {
    let a = crypto::random_uuid();
    assert_eq!(a.len(), 36);
    for (i, c) in a.char_indices() {
        match i {
            8 | 13 | 18 | 23 => assert_eq!(c, '-', "{a}"),
            14 => assert_eq!(c, '4', "{a}"),
            19 => assert!("89ab".contains(c), "{a}"),
            _ => assert!(c.is_ascii_hexdigit() && !c.is_ascii_uppercase(), "{a}"),
        }
    }
    assert_ne!(a, crypto::random_uuid());
}

#[test]
fn random_bytes_have_requested_length() {
    assert!(crypto::random_bytes(0).is_empty());
    let (a, b) = (crypto::random_bytes(32), crypto::random_bytes(32));
    assert_eq!(a.len(), 32);
    assert_ne!(a, b);
}

#[test]
fn math_random_is_in_unit_interval() {
    for _ in 0..10_000 {
        let x = crypto::math_random();
        assert!((0.0..1.0).contains(&x), "{x}");
    }
}

#[test]
fn math_random_override_is_scoped_and_thread_local() {
    let outer = crypto::testing::override_math_random(|| 0.0);
    assert_eq!(crypto::math_random(), 0.0);
    {
        let _inner = crypto::testing::override_math_random(|| 0.75);
        assert_eq!(crypto::math_random(), 0.75);
        // Other threads keep the real generator.
        let other = std::thread::spawn(|| (0..64).map(|_| crypto::math_random()).collect::<Vec<_>>())
            .join()
            .unwrap();
        assert!(other.iter().any(|&x| x != 0.75 && x != 0.0));
    }
    assert_eq!(crypto::math_random(), 0.0);
    drop(outer);
    let samples: Vec<f64> = (0..64).map(|_| crypto::math_random()).collect();
    assert!(samples.iter().any(|&x| x != 0.0));
}
