//! Port of packages/coding-agent/test/frontmatter.test.ts

use indexmap::IndexMap;
use pi_coding_agent::utils::frontmatter::{parse_frontmatter, strip_frontmatter, try_parse_frontmatter};
use serde_json::{Value, json};

mod parse_frontmatter_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_keys_strips_quotes_and_returns_body() {
        let input = "---\nname: \"skill-name\"\ndescription: 'A desc'\nfoo-bar: value\n---\n\nBody text";
        let parsed = parse_frontmatter(input);
        assert_eq!(parsed.frontmatter.get("name"), Some(&json!("skill-name")));
        assert_eq!(parsed.frontmatter.get("description"), Some(&json!("A desc")));
        assert_eq!(parsed.frontmatter.get("foo-bar"), Some(&json!("value")));
        assert_eq!(parsed.body, "Body text");
    }

    #[test]
    fn normalizes_newlines_and_handles_crlf() {
        let input = "---\r\nname: test\r\n---\r\nLine one\r\nLine two";
        let parsed = parse_frontmatter(input);
        assert_eq!(parsed.body, "Line one\nLine two");
    }

    #[test]
    fn throws_on_invalid_yaml_frontmatter() {
        let input = "---\nfoo: [bar\n---\nBody";
        let err = try_parse_frontmatter(input).expect_err("invalid YAML should error");
        let msg = err.to_string();
        assert!(msg.contains("YAMLParseError"), "unexpected error: {msg}");
        assert!(
            msg.contains("line 1") || msg.contains("line 2"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parses_pipe_multiline_yaml_syntax() {
        let input = "---\ndescription: |\n  Line one\n  Line two\n---\n\nBody";
        let parsed = parse_frontmatter(input);
        // PORT: `extract_frontmatter` slices `4..end_index` before `\n---`, so `serde_yaml_ng`
        // sees no final newline on the literal block scalar and omits a trailing `\n`.
        assert_eq!(
            parsed.frontmatter.get("description"),
            Some(&json!("Line one\nLine two"))
        );
        assert_eq!(parsed.body, "Body");
    }

    #[test]
    fn returns_original_content_when_frontmatter_is_missing_or_unterminated() {
        let no_frontmatter = "Just text\nsecond line";
        let missing_end = "---\nname: test\nBody without terminator";
        let res_no = parse_frontmatter(no_frontmatter);
        let res_missing = parse_frontmatter(missing_end);
        assert_eq!(res_no.body, "Just text\nsecond line");
        assert_eq!(res_missing.body, "---\nname: test\nBody without terminator");
    }

    #[test]
    fn returns_empty_object_for_empty_or_comment_only_frontmatter() {
        let input = "---\n# just a comment\n---\nBody";
        let parsed = parse_frontmatter(input);
        assert_eq!(parsed.frontmatter, IndexMap::<String, Value>::new());
    }
}

mod strip_frontmatter_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn removes_frontmatter_and_trims_body() {
        let input = "---\nkey: value\n---\n\nBody\n";
        assert_eq!(strip_frontmatter(input), "Body");
    }

    #[test]
    fn returns_body_when_no_frontmatter_present() {
        let input = "\n  No frontmatter body  \n";
        assert_eq!(strip_frontmatter(input), "\n  No frontmatter body  \n");
    }
}
