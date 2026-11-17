//! Port of packages/coding-agent/src/modes/interactive/components/diff.ts

use std::sync::LazyLock;

use pi_js::regex::{JS_DOT_CLASS, JS_WS_CHARS, JS_WS_CLASS};
use pi_js::vendor::jsdiff::diff_words;
use regex::Regex;

use super::super::theme::theme::{ThemeColor, theme};

static DIFF_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = format!(r"^([+\-{JS_WS_CHARS}])({JS_WS_CLASS}*[0-9]*){JS_WS_CLASS}({JS_DOT_CLASS}*)$");
    Regex::new(&pattern).expect("DIFF_LINE_RE compiles")
});

fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

struct ParsedDiffLine<'a> {
    prefix: &'a str,
    line_num: &'a str,
    content: &'a str,
}

/// Parse diff line to extract prefix, line number, and content.
/// Format: "+123 content" or "-123 content" or " 123 content" or "     ..."
fn parse_diff_line(line: &str) -> Option<ParsedDiffLine<'_>> {
    let caps = DIFF_LINE_RE.captures(line)?;
    Some(ParsedDiffLine {
        prefix: caps.get(1)?.as_str(),
        line_num: caps.get(2)?.as_str(),
        content: caps.get(3)?.as_str(),
    })
}

/// Replace tabs with spaces for consistent rendering.
fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

/// Compute word-level diff and render with inverse on changed parts.
/// Separates common indentation before diffing so a leading insertion cannot consume it.
fn render_intra_line_diff(old_content: &str, new_content: &str) -> (String, String) {
    let mut common_indent_bytes = 0usize;
    for ((byte_idx, a), b) in old_content.char_indices().zip(new_content.chars()) {
        if a == b && is_js_whitespace(a) {
            common_indent_bytes = byte_idx + a.len_utf8();
        } else {
            break;
        }
    }

    let common_indent = &old_content[..common_indent_bytes];
    let word_diff = diff_words(
        &old_content[common_indent_bytes..],
        &new_content[common_indent_bytes..],
        None,
    );
    let t = theme();
    let mut removed_line = common_indent.to_string();
    let mut added_line = common_indent.to_string();
    let mut is_first_removed = true;
    let mut is_first_added = true;

    for part in word_diff {
        if part.removed {
            let mut value = part.value.as_str();
            // Strip leading whitespace from the first removed part
            if is_first_removed {
                let rest = value.trim_start_matches(is_js_whitespace);
                let leading_len = value.len() - rest.len();
                removed_line.push_str(&value[..leading_len]);
                value = rest;
                is_first_removed = false;
            }
            if !value.is_empty() {
                removed_line.push_str(&t.inverse(value));
            }
        } else if part.added {
            let mut value = part.value.as_str();
            // Strip leading whitespace from the first added part
            if is_first_added {
                let rest = value.trim_start_matches(is_js_whitespace);
                let leading_len = value.len() - rest.len();
                added_line.push_str(&value[..leading_len]);
                value = rest;
                is_first_added = false;
            }
            if !value.is_empty() {
                added_line.push_str(&t.inverse(value));
            }
        } else {
            removed_line.push_str(&part.value);
            added_line.push_str(&part.value);
        }
    }

    (removed_line, added_line)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderDiffOptions {
    /// File path (unused, kept for API compatibility)
    pub file_path: Option<String>,
}

/// Render a diff string with colored lines and intra-line change highlighting.
/// - Context lines: dim/gray
/// - Removed lines: red, with inverse on changed tokens
/// - Added lines: green, with inverse on changed tokens
pub fn render_diff(diff_text: &str, _options: Option<RenderDiffOptions>) -> String {
    let lines: Vec<&str> = diff_text.split('\n').collect();
    let mut result = Vec::with_capacity(lines.len());
    let t = theme();

    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let Some(parsed) = parse_diff_line(line) else {
            result.push(t.fg(ThemeColor::ToolDiffContext, line));
            i += 1;
            continue;
        };

        if parsed.prefix == "-" {
            // Collect consecutive removed lines
            let mut removed_lines = Vec::new();
            while i < lines.len() {
                let Some(p) = parse_diff_line(lines[i]) else {
                    break;
                };
                if p.prefix != "-" {
                    break;
                }
                removed_lines.push((p.line_num, p.content));
                i += 1;
            }

            // Collect consecutive added lines
            let mut added_lines = Vec::new();
            while i < lines.len() {
                let Some(p) = parse_diff_line(lines[i]) else {
                    break;
                };
                if p.prefix != "+" {
                    break;
                }
                added_lines.push((p.line_num, p.content));
                i += 1;
            }

            // Only do intra-line diffing when there's exactly one removed and one added line
            // (indicating a single line modification). Otherwise, show lines as-is.
            if removed_lines.len() == 1 && added_lines.len() == 1 {
                let removed = removed_lines[0];
                let added = added_lines[0];

                let (removed_line, added_line) =
                    render_intra_line_diff(&replace_tabs(removed.1), &replace_tabs(added.1));

                result.push(t.fg(ThemeColor::ToolDiffRemoved, &format!("-{} {removed_line}", removed.0)));
                result.push(t.fg(ThemeColor::ToolDiffAdded, &format!("+{} {added_line}", added.0)));
            } else {
                // Show all removed lines first, then all added lines
                for removed in removed_lines {
                    result.push(t.fg(
                        ThemeColor::ToolDiffRemoved,
                        &format!("-{} {}", removed.0, replace_tabs(removed.1)),
                    ));
                }
                for added in added_lines {
                    result.push(t.fg(
                        ThemeColor::ToolDiffAdded,
                        &format!("+{} {}", added.0, replace_tabs(added.1)),
                    ));
                }
            }
        } else if parsed.prefix == "+" {
            // Standalone added line
            result.push(t.fg(
                ThemeColor::ToolDiffAdded,
                &format!("+{} {}", parsed.line_num, replace_tabs(parsed.content)),
            ));
            i += 1;
        } else {
            // Context line
            result.push(t.fg(
                ThemeColor::ToolDiffContext,
                &format!(" {} {}", parsed.line_num, replace_tabs(parsed.content)),
            ));
            i += 1;
        }
    }

    result.join("\n")
}

#[cfg(test)]
mod tests {
    use pi_tui::strip_terminal_sequences;
    use pretty_assertions::assert_eq;
    use serial_test::serial;

    use super::super::super::theme::theme::init_theme;
    use super::*;

    #[test]
    #[serial]
    fn preserves_indentation_for_start_insertions_and_replacements() {
        init_theme(Some("dark"), Some(false));

        let cases = [
            (
                "-83   await commitRecordingChunk({\n+83   const result = await commitRecordingChunk({",
                vec![
                    "-83   await commitRecordingChunk({",
                    "+83   const result = await commitRecordingChunk({",
                ],
            ),
            (
                "-12     const value = oldValue\n+12     const value = newValue",
                vec!["-12     const value = oldValue", "+12     const value = newValue"],
            ),
        ];

        for (diff, expected) in cases {
            let rendered = render_diff(diff, None);
            let stripped: Vec<String> = strip_terminal_sequences(&rendered)
                .split('\n')
                .map(ToString::to_string)
                .collect();
            assert_eq!(stripped, expected);
        }
    }

    #[test]
    #[serial]
    fn does_not_highlight_shared_indentation_as_insertion() {
        init_theme(Some("dark"), Some(false));

        let rendered = render_diff(
            "-83   await commitRecordingChunk({\n+83   const result = await commitRecordingChunk({",
            None,
        );
        let inverse_insertion = theme().inverse("const result = ");

        let prefix_pos = rendered.find("+83   ").expect("prefix present");
        let inverse_pos = rendered.find(&inverse_insertion).expect("inverse insertion present");
        assert!(prefix_pos < inverse_pos);
    }

    #[test]
    #[serial]
    fn intra_line_word_inverse_highlighting_for_replaced_token() {
        init_theme(Some("dark"), Some(false));

        let rendered = render_diff("-12     const value = oldValue\n+12     const value = newValue", None);
        let t = theme();
        assert!(rendered.contains(&t.inverse("oldValue")));
        assert!(rendered.contains(&t.inverse("newValue")));
        assert!(!rendered.contains(&t.inverse("const")));
    }

    #[test]
    #[serial]
    fn context_lines_standalone_adds_multi_line_hunks_and_tab_replacement() {
        init_theme(Some("dark"), Some(false));
        let t = theme();

        let diff = [
            " 10 \tconst a = 1;",
            "-11 \tconst b = 2;",
            "-12 \tconst c = 3;",
            "+11 \tconst b = 20;",
            "+13 \tconst d = 4;",
            "    ...",
            "unparsed header",
        ]
        .join("\n");

        let rendered = render_diff(
            &diff,
            Some(RenderDiffOptions {
                file_path: Some("a.ts".into()),
            }),
        );
        let lines: Vec<&str> = rendered.split('\n').collect();
        assert_eq!(lines.len(), 7);
        assert_eq!(lines[0], t.fg(ThemeColor::ToolDiffContext, " 10    const a = 1;"));
        assert_eq!(lines[1], t.fg(ThemeColor::ToolDiffRemoved, "-11    const b = 2;"));
        assert_eq!(lines[2], t.fg(ThemeColor::ToolDiffRemoved, "-12    const c = 3;"));
        assert_eq!(lines[3], t.fg(ThemeColor::ToolDiffAdded, "+11    const b = 20;"));
        assert_eq!(lines[4], t.fg(ThemeColor::ToolDiffAdded, "+13    const d = 4;"));
        assert_eq!(lines[5], t.fg(ThemeColor::ToolDiffContext, "    ..."));
        assert_eq!(lines[6], t.fg(ThemeColor::ToolDiffContext, "unparsed header"));

        let standalone_add = render_diff("+ 5 \thello\tworld", None);
        assert_eq!(standalone_add, t.fg(ThemeColor::ToolDiffAdded, "+ 5    hello   world"));
    }
}
