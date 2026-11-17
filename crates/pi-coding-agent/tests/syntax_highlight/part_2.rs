//! Port of packages/coding-agent/test/syntax-highlight.test.ts (unit coding-agent-interactive-theme)

use pi_coding_agent::modes::interactive::theme::theme::{
    ThemeColor, get_markdown_theme, highlight_code, init_theme, theme,
};
use pi_tui::{TerminalCapabilities, reset_capabilities_cache, set_capabilities};
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

// BLOCKED on Stage 3A-1: `crate::utils::syntax_highlight` is still a stub on `swarm/tui-w3`, so
// `theme::highlight_code` currently falls back to `ThemeColor::MdCodeBlock` for all lines.
mod theme_syntax_highlighting {
    use super::*;
    use pretty_assertions::assert_eq;

    // #10143
    #[test]
    #[serial]
    #[ignore = "BLOCKED on Stage 3A-1: utils::syntax_highlight not merged yet; theme::highlight_code uses MdCodeBlock fallback"]
    fn colors_each_line_of_python_docstrings_independently() {
        let _guard = setup();
        let code = "\"\"\"\nline one\n\nline two\n\"\"\"\nafter";
        let ansi = theme().get_fg_ansi(ThemeColor::SyntaxString);
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
    #[ignore = "BLOCKED on Stage 3A-1: utils::syntax_highlight not merged yet; theme::highlight_code uses MdCodeBlock fallback"]
    fn colors_diff_additions_and_deletions_in_fenced_diff_blocks() {
        let _guard = setup();
        let lines = highlight_code("-old\n+new\n", Some("diff"));
        let t = theme();

        assert_eq!(
            lines[0],
            format!("{}-old\x1b[39m", t.get_fg_ansi(ThemeColor::ToolDiffRemoved))
        );
        assert_eq!(
            lines[1],
            format!("{}+new\x1b[39m", t.get_fg_ansi(ThemeColor::ToolDiffAdded))
        );
    }

    #[test]
    #[serial]
    #[ignore = "BLOCKED on Stage 3A-1: utils::syntax_highlight not merged yet; theme::highlight_code uses MdCodeBlock fallback"]
    fn keeps_cli_highlight_default_styled_scopes_mapped_to_theme_styles() {
        let _guard = setup();
        let t = theme();

        assert!(
            highlight_code("const re = /foo+/gi;", Some("javascript"))[0]
                .contains(&format!("{}/foo+/gi\x1b[39m", t.get_fg_ansi(ThemeColor::SyntaxString)))
        );
        assert_eq!(
            highlight_code("@decorator", Some("python"))[0],
            format!("{}@decorator\x1b[39m", t.get_fg_ansi(ThemeColor::Muted))
        );
        assert!(
            highlight_code("<div></div>", Some("html"))[0]
                .contains(&format!("{}div\x1b[39m", t.get_fg_ansi(ThemeColor::SyntaxKeyword)))
        );
    }
}
