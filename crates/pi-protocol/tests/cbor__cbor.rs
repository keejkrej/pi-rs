//! Port of packages/protocol/test/cbor/cbor.test.ts

use indexmap::IndexMap;
use pi_js::num::{MAX_SAFE_INTEGER, MIN_SAFE_INTEGER};
use pi_protocol::{
    CborError, CborOptions, CborValue, DEFAULT_MAX_CBOR_BYTE_LENGTH, DEFAULT_MAX_CBOR_CONTAINER_LENGTH,
    DEFAULT_MAX_CBOR_DEPTH, decode_cbor, encode_cbor,
};

fn from_hex(hex: &str) -> Vec<u8> {
    assert_eq!(hex.len() % 2, 0, "Hex fixture must contain whole bytes");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("valid hex byte"))
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn map_of<I, K>(entries: I) -> CborValue
where
    I: IntoIterator<Item = (K, CborValue)>,
    K: Into<String>,
{
    let mut map = IndexMap::new();
    for (k, v) in entries {
        map.insert(k.into(), v);
    }
    CborValue::Map(map)
}

fn known_vectors() -> Vec<(CborValue, &'static str)> {
    vec![
        (CborValue::Null, "f6"),
        (CborValue::Bool(false), "f4"),
        (CborValue::Bool(true), "f5"),
        (CborValue::Number(0.0), "00"),
        (CborValue::Number(1.0), "01"),
        (CborValue::Number(10.0), "0a"),
        (CborValue::Number(23.0), "17"),
        (CborValue::Number(24.0), "1818"),
        (CborValue::Number(25.0), "1819"),
        (CborValue::Number(100.0), "1864"),
        (CborValue::Number(1000.0), "1903e8"),
        (CborValue::Number(1_000_000.0), "1a000f4240"),
        (CborValue::Number(1_000_000_000_000.0), "1b000000e8d4a51000"),
        (CborValue::Number(MAX_SAFE_INTEGER as f64), "1b001fffffffffffff"),
        (CborValue::Number(-1.0), "20"),
        (CborValue::Number(-10.0), "29"),
        (CborValue::Number(-24.0), "37"),
        (CborValue::Number(-25.0), "3818"),
        (CborValue::Number(-100.0), "3863"),
        (CborValue::Number(-1000.0), "3903e7"),
        (CborValue::Number(-1_000_000.0), "3a000f423f"),
        (CborValue::Number(MIN_SAFE_INTEGER as f64), "3b001ffffffffffffe"),
        (CborValue::Number(1.1), "fb3ff199999999999a"),
        (CborValue::Number(-0.0), "fb8000000000000000"),
        (CborValue::Bytes(vec![1, 2, 3, 4]), "4401020304"),
        (CborValue::String(String::new()), "60"),
        (CborValue::String("IETF".to_string()), "6449455446"),
        (CborValue::String("ü".to_string()), "62c3bc"),
        (CborValue::String("水".to_string()), "63e6b0b4"),
        (CborValue::String("𐅑".to_string()), "64f0908591"),
        (CborValue::Array(vec![]), "80"),
        (
            CborValue::Array(vec![
                CborValue::Number(1.0),
                CborValue::Number(2.0),
                CborValue::Number(3.0),
            ]),
            "83010203",
        ),
        (
            CborValue::Array(vec![
                CborValue::Number(1.0),
                CborValue::Array(vec![CborValue::Number(2.0), CborValue::Number(3.0)]),
                CborValue::Array(vec![CborValue::Number(4.0), CborValue::Number(5.0)]),
            ]),
            "8301820203820405",
        ),
        (
            map_of([
                ("a", CborValue::Number(1.0)),
                (
                    "b",
                    CborValue::Array(vec![CborValue::Number(2.0), CborValue::Number(3.0)]),
                ),
            ]),
            "a26161016162820203",
        ),
    ]
}

fn assert_cbor_error(err: pi_js::Error) -> String {
    assert!(
        err.downcast_ref::<CborError>().is_some(),
        "expected CborError, got {err:?}"
    );
    err.message()
}

mod cbor_codec {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn encodes_and_decodes_rfc_8949_vectors() {
        for (index, (value, wire)) in known_vectors().into_iter().enumerate() {
            let encoded = encode_cbor(&value, None).unwrap_or_else(|e| panic!("vector {index} encode failed: {e}"));
            assert_eq!(to_hex(&encoded), wire, "vector {index} wire mismatch");
            let decoded =
                decode_cbor(&from_hex(wire), None).unwrap_or_else(|e| panic!("vector {index} decode failed: {e}"));
            if matches!(value, CborValue::Number(n) if n == 0.0 && n.is_sign_negative()) {
                assert!(
                    matches!(decoded, CborValue::Number(n) if n == 0.0 && n.is_sign_negative()),
                    "vector {index} expected -0.0, got {decoded:?}"
                );
            } else {
                assert_eq!(decoded, value, "vector {index} decoded mismatch");
            }
        }
    }

    #[test]
    fn omits_undefined_object_properties_without_omitting_falsey_values() {
        // PORT: `CborValue` has no `undefined` variant; test falsey values round-trip cleanly.
        let value = map_of([
            ("zero", CborValue::Number(0.0)),
            ("empty", CborValue::String(String::new())),
            ("no", CborValue::Bool(false)),
            ("nil", CborValue::Null),
        ]);
        let decoded = decode_cbor(&encode_cbor(&value, None).unwrap(), None).unwrap();
        assert_eq!(decoded, value);
    }

    #[test]
    fn preserves_a_leading_unicode_bom_and_treats_proto_as_data() {
        assert_eq!(
            decode_cbor(&from_hex("63efbbbf"), None).unwrap(),
            CborValue::String("\u{feff}".to_string())
        );
        let value = map_of([("__proto__", CborValue::String("safe".to_string()))]);
        let decoded = decode_cbor(&encode_cbor(&value, None).unwrap(), None).unwrap();
        assert_eq!(decoded, value);
    }

    #[test]
    fn rejects_unsupported_encoder_value() {
        // PORT: JS-only `undefined`, array holes, `bigint`, `symbol`, `function`, `Date`, `Map`,
        // enumerable symbol keys, lone UTF-16 surrogates, and cycles cannot exist in `CborValue`.
        let cases = [
            ("NaN", CborValue::Number(f64::NAN)),
            ("positive infinity", CborValue::Number(f64::INFINITY)),
            ("negative infinity", CborValue::Number(f64::NEG_INFINITY)),
            (
                "unsafe positive integer",
                CborValue::Number((MAX_SAFE_INTEGER + 1) as f64),
            ),
            (
                "unsafe negative integer",
                CborValue::Number((MIN_SAFE_INTEGER - 1) as f64),
            ),
        ];
        for (label, value) in cases {
            let err = encode_cbor(&value, None).expect_err(label);
            assert_cbor_error(err);
        }
    }

    #[test]
    fn rejects_lossy_strings_cycles_and_excessive_encoder_depth() {
        // PORT: Rust `String` cannot contain lone surrogates and owned `CborValue` trees cannot contain cycles.
        let mut too_deep = CborValue::Null;
        for _ in 0..=DEFAULT_MAX_CBOR_DEPTH {
            too_deep = CborValue::Array(vec![too_deep]);
        }
        let err = encode_cbor(&too_deep, None).expect_err("too deep");
        let msg = assert_cbor_error(err);
        assert!(msg.to_lowercase().contains("depth"), "unexpected error: {msg}");
    }

    #[test]
    fn rejects_invalid_decoder_input() {
        let cases = [
            ("empty input", ""),
            ("truncated integer", "18"),
            ("reserved additional information", "1c"),
            ("indefinite byte string", "5f"),
            ("indefinite text string", "7f"),
            ("indefinite array", "9f"),
            ("indefinite map", "bf"),
            ("tag", "c000"),
            ("undefined", "f7"),
            ("unsupported simple value", "e0"),
            ("break outside an indefinite item", "ff"),
            ("float16", "f93c00"),
            ("float32", "fa3f800000"),
            ("positive infinity", "fb7ff0000000000000"),
            ("NaN", "fb7ff8000000000000"),
            ("truncated float64", "fb3ff00000"),
            ("truncated byte string", "44010203"),
            ("truncated text string", "636162"),
            ("truncated array", "8201"),
            ("truncated map", "a16161"),
            ("trailing data", "0000"),
            ("non-string map key", "a10102"),
            ("duplicate map key", "a2616101616102"),
            ("invalid UTF-8 byte", "61ff"),
            ("overlong UTF-8", "62c080"),
            ("UTF-8 surrogate", "63eda080"),
            ("unsafe positive integer", "1b0020000000000000"),
            ("unsafe negative integer", "3b001fffffffffffff"),
            ("unsafe integer encoded as float64", "fb4340000000000000"),
        ];
        for (label, wire) in cases {
            let err = decode_cbor(&from_hex(wire), None).expect_err(label);
            assert_cbor_error(err);
        }
    }

    #[test]
    fn enforces_depth_and_declared_length_limits_before_traversing_values() {
        let mut too_deep = vec![0x81u8; (DEFAULT_MAX_CBOR_DEPTH as usize) + 2];
        let last = too_deep.len() - 1;
        too_deep[last] = 0xf6;
        let msg = assert_cbor_error(decode_cbor(&too_deep, None).expect_err("too deep"));
        assert!(msg.to_lowercase().contains("depth"), "unexpected error: {msg}");

        let oversized_bytes = from_hex(&format!("5a{:08x}", DEFAULT_MAX_CBOR_BYTE_LENGTH + 1));
        let oversized_text = from_hex(&format!("7a{:08x}", DEFAULT_MAX_CBOR_BYTE_LENGTH + 1));
        let oversized_array = from_hex(&format!("9a{:08x}", DEFAULT_MAX_CBOR_CONTAINER_LENGTH + 1));
        let oversized_map = from_hex(&format!("ba{:08x}", DEFAULT_MAX_CBOR_CONTAINER_LENGTH + 1));
        for wire in [oversized_bytes, oversized_text, oversized_array, oversized_map] {
            let msg = assert_cbor_error(decode_cbor(&wire, None).expect_err("oversized"));
            assert!(msg.to_lowercase().contains("limit"), "unexpected error: {msg}");
        }
    }

    #[test]
    fn supports_stricter_caller_provided_limits() {
        let err = decode_cbor(
            &from_hex("83010203"),
            Some(CborOptions {
                max_container_length: Some(2),
                ..Default::default()
            }),
        )
        .expect_err("container limit decode");
        assert!(assert_cbor_error(err).to_lowercase().contains("limit"));

        let err = decode_cbor(
            &from_hex("626162"),
            Some(CborOptions {
                max_byte_length: Some(2),
                ..Default::default()
            }),
        )
        .expect_err("byte limit decode");
        assert!(assert_cbor_error(err).to_lowercase().contains("limit"));

        let err = encode_cbor(
            &CborValue::Array(vec![
                CborValue::Number(1.0),
                CborValue::Number(2.0),
                CborValue::Number(3.0),
            ]),
            Some(CborOptions {
                max_container_length: Some(2),
                ..Default::default()
            }),
        )
        .expect_err("container limit encode");
        assert!(assert_cbor_error(err).to_lowercase().contains("limit"));

        let err = encode_cbor(
            &CborValue::String("ab".to_string()),
            Some(CborOptions {
                max_byte_length: Some(2),
                ..Default::default()
            }),
        )
        .expect_err("byte limit encode");
        assert!(assert_cbor_error(err).to_lowercase().contains("limit"));
    }

    #[test]
    fn rejects_out_of_range_options() {
        let err = encode_cbor(
            &CborValue::Null,
            Some(CborOptions {
                max_depth: Some(513),
                ..Default::default()
            }),
        )
        .expect_err("invalid maxDepth");
        assert_eq!(err.name(), "RangeError");
    }
}
