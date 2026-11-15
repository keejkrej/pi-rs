//! Port of packages/tui/test/truncated-text.test.ts

use std::sync::LazyLock;

use pi_tui::{TruncatedText, visible_width};

static ANSI_SGR_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\x1b\[[0-9;]*m").expect("ANSI_SGR_REGEX"));

fn red(text: &str) -> String {
    format!("\x1b[31m{text}\x1b[39m")
}

fn blue(text: &str) -> String {
    format!("\x1b[34m{text}\x1b[39m")
}

#[test]
fn pads_output_lines_to_exactly_match_width() {
    let text = TruncatedText::new("Hello world", Some(1), Some(0));
    let lines = text.render(50);

    // Should have exactly one content line (no vertical padding)
    assert_eq!(lines.len(), 1);

    // Line should be exactly 50 visible characters
    let visible_len = visible_width(&lines[0]);
    assert_eq!(visible_len, 50);
}

#[test]
fn pads_output_with_vertical_padding_lines_to_width() {
    let text = TruncatedText::new("Hello", Some(0), Some(2));
    let lines = text.render(40);

    // Should have 2 padding lines + 1 content line + 2 padding lines = 5 total
    assert_eq!(lines.len(), 5);

    // All lines should be exactly 40 characters
    for line in &lines {
        assert_eq!(visible_width(line), 40);
    }
}

#[test]
fn truncates_long_text_and_pads_to_width() {
    let long_text = "This is a very long piece of text that will definitely exceed the available width";
    let text = TruncatedText::new(long_text, Some(1), Some(0));
    let lines = text.render(30);

    assert_eq!(lines.len(), 1);

    // Should be exactly 30 characters
    assert_eq!(visible_width(&lines[0]), 30);

    // Should contain ellipsis
    let stripped = ANSI_SGR_REGEX.replace_all(&lines[0], "");
    assert!(stripped.contains("..."));
}

#[test]
fn preserves_ansi_codes_in_output_and_pads_correctly() {
    let styled_text = format!("{} {}", red("Hello"), blue("world"));
    let text = TruncatedText::new(&styled_text, Some(1), Some(0));
    let lines = text.render(40);

    assert_eq!(lines.len(), 1);

    // Should be exactly 40 visible characters (ANSI codes don't count)
    assert_eq!(visible_width(&lines[0]), 40);

    // Should preserve the color codes
    assert!(lines[0].contains("\x1b["));
}

#[test]
fn truncates_styled_text_and_adds_reset_code_before_ellipsis() {
    let long_styled_text = red("This is a very long red text that will be truncated");
    let text = TruncatedText::new(&long_styled_text, Some(1), Some(0));
    let lines = text.render(20);

    assert_eq!(lines.len(), 1);

    // Should be exactly 20 visible characters
    assert_eq!(visible_width(&lines[0]), 20);

    // Should contain reset code before ellipsis
    assert!(lines[0].contains("\x1b[0m..."));
}

#[test]
fn handles_text_that_fits_exactly() {
    // With paddingX=1, available width is 30-2=28
    // "Hello world" is 11 chars, fits comfortably
    let text = TruncatedText::new("Hello world", Some(1), Some(0));
    let lines = text.render(30);

    assert_eq!(lines.len(), 1);
    assert_eq!(visible_width(&lines[0]), 30);

    // Should NOT contain ellipsis
    let stripped = ANSI_SGR_REGEX.replace_all(&lines[0], "");
    assert!(!stripped.contains("..."));
}

#[test]
fn handles_empty_text() {
    let text = TruncatedText::new("", Some(1), Some(0));
    let lines = text.render(30);

    assert_eq!(lines.len(), 1);
    assert_eq!(visible_width(&lines[0]), 30);
}

#[test]
fn stops_at_newline_and_only_shows_first_line() {
    let multiline_text = "First line\nSecond line\nThird line";
    let text = TruncatedText::new(multiline_text, Some(1), Some(0));
    let lines = text.render(40);

    assert_eq!(lines.len(), 1);
    assert_eq!(visible_width(&lines[0]), 40);

    // Should only contain "First line"
    let stripped = ANSI_SGR_REGEX.replace_all(&lines[0], "");
    let trimmed = pi_js::str16::trim(&stripped);
    assert!(trimmed.contains("First line"));
    assert!(!trimmed.contains("Second line"));
    assert!(!trimmed.contains("Third line"));
}

#[test]
fn truncates_first_line_even_with_newlines_in_text() {
    let long_multiline_text = "This is a very long first line that needs truncation\nSecond line";
    let text = TruncatedText::new(long_multiline_text, Some(1), Some(0));
    let lines = text.render(25);

    assert_eq!(lines.len(), 1);
    assert_eq!(visible_width(&lines[0]), 25);

    // Should contain ellipsis and not second line
    let stripped = ANSI_SGR_REGEX.replace_all(&lines[0], "");
    assert!(stripped.contains("..."));
    assert!(!stripped.contains("Second line"));
}
