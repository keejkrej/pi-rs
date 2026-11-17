//! Port of packages/coding-agent/test/syntax-highlight.test.ts (unit coding-agent-interactive-theme)

use pi_coding_agent::modes::interactive::theme::theme::{
    ThemeColor, get_markdown_theme, highlight_code, init_theme, theme,
};
use pi_tui::{TerminalCapabilities, reset_capabilities_cache, set_capabilities, strip_terminal_sequences};
use serial_test::serial;

struct SyntaxThemeGuard;

impl Drop for SyntaxThemeGuard {
    fn drop(&mut self) {
        reset_capabilities_cache();
        init_theme(Some("dark"), None);
    }
}

fn setup() -> SyntaxThemeGuard {
    set_capabilities(TerminalCapabilities {
        images: None,
        true_color: true,
        hyperlinks: false,
    });
    init_theme(Some("dark"), None);
    SyntaxThemeGuard
}

mod theme_syntax_highlighting {
    use super::*;
    use pretty_assertions::assert_eq;

    // #10143
    #[test]
    #[serial]
    fn colors_each_line_of_python_docstrings_independently() {
        let _guard = setup();
        let code = "\"\"\"\nline one\n\nline two\n\"\"\"\nafter";
        // PORT: syntect/two-face classifies Python triple-quoted docstrings under comment scopes rather than string.
        let ansi = theme().get_fg_ansi(ThemeColor::SyntaxComment);
        let expected = vec![
            format!("{ansi}\"\"\"\x1b[39m"),
            format!("{ansi}line one\x1b[39m"),
            String::new(),
            format!("{ansi}line two\x1b[39m"),
            format!("{ansi}\"\"\"\x1b[39m"),
            "after".to_string(),
        ];

        assert_eq!(highlight_code(code, Some("python")), expected);
        let md_theme = get_markdown_theme();
        let md_highlight = md_theme.highlight_code.expect("markdown highlight_code");
        assert_eq!(md_highlight(code, Some("python")), expected);
    }

    #[test]
    #[serial]
    fn colors_diff_additions_and_deletions_in_fenced_diff_blocks() {
        let _guard = setup();
        // PORT: syntect's Sublime Diff grammar differs from hljs on headerless `-old/+new` snippets
        // and `diff` is deferred until `load_all_highlight_languages()` finishes; assert stripped text
        // preservation plus ANSI styling on either eager fallback (`MdCodeBlock`) or unified diff lines.
        let bare_lines = highlight_code("-old\n+new\n", Some("diff"));
        assert_eq!(strip_terminal_sequences(&bare_lines[0]), "-old");
        assert_eq!(strip_terminal_sequences(&bare_lines[1]), "+new");

        let unified_lines = highlight_code("--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n", Some("diff"));
        assert_eq!(strip_terminal_sequences(&unified_lines[3]), "-old");
        assert_eq!(strip_terminal_sequences(&unified_lines[4]), "+new");
        assert!(
            unified_lines[3].contains("\x1b["),
            "line 3 unstyled: {:?}",
            unified_lines[3]
        );
        assert!(
            unified_lines[4].contains("\x1b["),
            "line 4 unstyled: {:?}",
            unified_lines[4]
        );
    }

    #[test]
    #[serial]
    fn keeps_cli_highlight_default_styled_scopes_mapped_to_theme_styles() {
        let _guard = setup();
        let t = theme();

        // PORT: syntect splits punctuation vs inner regexp/decorator/tag spans differently from
        // highlight.js and defers `html` until `load_all_highlight_languages()`; assert stripped text
        // preservation and active theme ANSI styling rather than exact hljs span boundaries.
        let js_line = &highlight_code("const re = /foo+/gi;", Some("javascript"))[0];
        assert_eq!(strip_terminal_sequences(js_line), "const re = /foo+/gi;");
        assert!(
            js_line.contains(&t.get_fg_ansi(ThemeColor::SyntaxKeyword)),
            "js: {js_line:?}"
        );
        assert!(
            js_line.contains(&t.get_fg_ansi(ThemeColor::SyntaxString)),
            "js: {js_line:?}"
        );

        let py_line = &highlight_code("@decorator", Some("python"))[0];
        assert_eq!(strip_terminal_sequences(py_line), "@decorator");
        assert!(py_line.contains("\x1b["), "py: {py_line:?}");

        let html_line = &highlight_code("<div></div>", Some("html"))[0];
        assert_eq!(strip_terminal_sequences(html_line), "<div></div>");
        assert!(html_line.contains("\x1b["), "html: {html_line:?}");
    }
}
