//! Port of packages/codemode/test/source.test.ts

use pi_codemode::{
    CODEMODE_SOURCE_GRAMMAR, CodemodeSourceError, CodemodeSourceOptions, ParsedCodemodeSource, parse_codemode_source,
};

#[test]
fn returns_plain_code_unchanged() {
    assert_eq!(
        parse_codemode_source("text('hi')").expect("plain"),
        ParsedCodemodeSource {
            code: "text('hi')".to_string(),
            options: CodemodeSourceOptions::default(),
        }
    );
    assert_eq!(
        parse_codemode_source("// just a comment\nreturn 1").expect("comment"),
        ParsedCodemodeSource {
            code: "// just a comment\nreturn 1".to_string(),
            options: CodemodeSourceOptions::default(),
        }
    );
}

#[test]
fn parses_the_options_line_and_keeps_line_numbers() {
    assert_eq!(
        parse_codemode_source("// @options: {\"timeout_ms\": 10}\nconst a = 1;\ntext(a)").expect("timeout"),
        ParsedCodemodeSource {
            code: "\nconst a = 1;\ntext(a)".to_string(),
            options: CodemodeSourceOptions {
                max_output_tokens: None,
                timeout_ms: Some(10),
            },
        }
    );
    assert_eq!(
        parse_codemode_source("  // @options:{\"max_output_tokens\":0,\"timeout_ms\":1500}\r\ntext(1)")
            .expect("both")
            .options,
        CodemodeSourceOptions {
            max_output_tokens: Some(0),
            timeout_ms: Some(1500),
        }
    );
    assert_eq!(
        parse_codemode_source("// @options: {}\ntext(1)").expect("empty options"),
        ParsedCodemodeSource {
            code: "\ntext(1)".to_string(),
            options: CodemodeSourceOptions::default(),
        }
    );
}

#[test]
fn only_treats_the_first_line_as_an_options_line() {
    let input = "text(1)\n// @options: {\"timeout_ms\": 1}";
    assert_eq!(
        parse_codemode_source(input).expect("second line ignored"),
        ParsedCodemodeSource {
            code: input.to_string(),
            options: CodemodeSourceOptions::default(),
        }
    );
    assert_eq!(
        parse_codemode_source("// @optionsx {}\ntext(1)")
            .expect("non-prefix")
            .options,
        CodemodeSourceOptions::default()
    );
}

#[test]
fn rejects_empty_input_and_invalid_options() {
    let cases = [
        ("", "Expected JavaScript source text (non-empty)"),
        ("  \n", "Expected JavaScript source text (non-empty)"),
        (
            "// @options:\ntext(1)",
            "@options must be a JSON object with supported fields",
        ),
        (
            "// @options: {timeout_ms: 1}\ntext(1)",
            "@options must be valid JSON with supported fields",
        ),
        (
            "// @options: [1]\ntext(1)",
            "@options must be a JSON object with supported fields",
        ),
        (
            "// @options: {\"yield\": 1}\ntext(1)",
            "@options only supports `max_output_tokens` and `timeout_ms`; got `yield`",
        ),
        (
            "// @options: {\"max_output_tokens\": 1.5}\ntext(1)",
            "@options field `max_output_tokens` must be a non-negative safe integer",
        ),
        (
            "// @options: {\"timeout_ms\": 0}\ntext(1)",
            "@options field `timeout_ms` must be a positive integer",
        ),
        (
            "// @options: {\"timeout_ms\": 1}",
            "The @options line must be followed by JavaScript source on subsequent lines",
        ),
        (
            "// @options: {\"timeout_ms\": 1}\n  \n",
            "The @options line must be followed by JavaScript source on subsequent lines",
        ),
    ];

    for (input, expected) in cases {
        let err = parse_codemode_source(input).expect_err(&format!("expected error for: {input}"));
        assert!(
            err.downcast_ref::<CodemodeSourceError>().is_some(),
            "expected CodemodeSourceError for {input}, got: {err}"
        );
        assert!(
            err.message().contains(expected),
            "for {input:?}: expected message containing {expected:?}, got {:?}",
            err.message()
        );
    }

    assert!(CODEMODE_SOURCE_GRAMMAR.contains("@options"));
}
