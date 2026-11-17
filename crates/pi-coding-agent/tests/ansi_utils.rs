//! Port of packages/coding-agent/test/ansi-utils.test.ts

use fancy_regex::Regex;
use pi_coding_agent::utils::ansi::{strip_ansi, try_strip_ansi};
use serde_json::{Value, json};

fn reference_ansi_regex() -> Regex {
    let st = r"(?:\u{0007}|\u{001B}\x5C|\u{009C})";
    let osc = format!(r"(?:\u{{001B}}\][\s\S]*?{st})");
    let csi = r"[\u{001B}\u{009B}][\[\]()#;?]*(?:\d{1,4}(?:[;:]\d{0,4})*)?[\dA-PR-TZcf-nq-uy=><~]";
    Regex::new(&format!("{osc}|{csi}")).expect("valid reference regex")
}

fn reference_strip_ansi(value: &str) -> String {
    if !value.contains('\u{001B}') && !value.contains('\u{009B}') {
        return value.to_owned();
    }
    reference_ansi_regex().replace_all(value, "").into_owned()
}

fn get_compatibility_inputs() -> Vec<String> {
    let mut inputs = vec![
        "plain".to_owned(),
        "a\x1b[31mred\x1b[0mz".to_owned(),
        "a\x1b]8;;https://example.com\x07link\x1b]8;;\x07z".to_owned(),
        "a\x1b]unterminated".to_owned(),
        "a\x1b]funterminated".to_owned(),
        "a\x1bPabc\x1b\\z".to_owned(),
        "a\x1b^abc\x07z".to_owned(),
        "a\x1b_abc\u{009c}z".to_owned(),
        "a\u{0090}abc\u{009c}z".to_owned(),
        "a\u{009d}abc\u{009c}z".to_owned(),
        "a\u{009b}31mred".to_owned(),
        "a\x1b(0x".to_owned(),
        "a\x1b*0x".to_owned(),
        "a\x1b+c".to_owned(),
        "a\x1b/0x".to_owned(),
        "a\x1bcok".to_owned(),
        "a\x1b\\ok".to_owned(),
    ];
    let chars = [
        "a", "f", "0", "1", ";", ":", "[", "]", "(", ")", "#", "?", "m", "P", "_", "\\", "\x07", "\x1b", "\u{009b}",
        "\u{009c}", "\u{0090}", "\u{009d}",
    ];

    for c in chars {
        inputs.push(format!("x\x1b{c}y"));
        inputs.push(format!("x\u{009b}{c}y"));
        for idx in (0..chars.len()).step_by(3) {
            inputs.push(format!("x\x1b{c}{}y", chars[idx]));
        }
    }

    inputs
}

mod strip_ansi_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn matches_chalk_strip_ansi_for_generated_compatibility_inputs() {
        for input in get_compatibility_inputs() {
            assert_eq!(strip_ansi(&input), reference_strip_ansi(&input));
        }
    }

    #[test]
    fn throws_the_same_type_error_as_chalk_strip_ansi_for_non_string_values() {
        for (value, expected_typeof) in [
            (Value::Null, "object"),
            (json!(123), "number"),
            (json!({}), "object"),
            (json!(true), "boolean"),
        ] {
            let err = try_strip_ansi(&value).expect_err("non-string should fail");
            assert_eq!(err.name(), "TypeError");
            assert_eq!(err.message(), format!("Expected a `string`, got `{expected_typeof}`"));
        }
    }

    #[test]
    fn strips_ris_without_leaking_the_final_byte() {
        assert_eq!(strip_ansi("\x1bcdone"), "done");
    }

    #[test]
    fn strips_single_byte_esc_sequences_without_leaking_final_bytes() {
        for code in b'g'..=b'm' {
            let ch = char::from(code);
            assert_eq!(strip_ansi(&format!("\x1b{ch}ok")), "ok");
        }
        for code in b'r'..=b't' {
            let ch = char::from(code);
            assert_eq!(strip_ansi(&format!("\x1b{ch}ok")), "ok");
        }
    }

    #[test]
    fn strips_common_ansi_sequences_used_in_tool_output() {
        let input = "a\x1b[31mred\x1b[0m\x1b]8;;https://example.com\x07link\x1b]8;;\x07z";
        assert_eq!(strip_ansi(input), "aredlinkz");
    }
}
