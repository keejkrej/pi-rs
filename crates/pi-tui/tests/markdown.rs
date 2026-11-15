//! Port of packages/tui/test/markdown.test.ts

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use pi_tui::components::markdown::{DefaultTextStyle, Markdown, MarkdownOptions, MarkdownTheme};
use pi_tui::terminal_image::{TerminalCapabilities, reset_capabilities_cache, set_capabilities};
use pi_tui::tui::{Component, TuiMouseEvent, TuiMouseEventResult};
use pi_tui::tui_main_screen::TuiMainScreen;
use regex::Regex;
use serial_test::serial;

mod support;

use support::test_themes::default_markdown_theme;
use support::virtual_terminal::{VirtualCell, VirtualTerminal};

static ANSI_SGR_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*m").expect("ANSI_SGR_REGEX"));
static ORDERED_ITEM_PREFIX_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]+\.").expect("ORDERED_ITEM_PREFIX_REGEX"));
static TABLE_CHARS_OR_WS_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[│├┤─ \t\r\n]").expect("TABLE_CHARS_OR_WS_REGEX"));
static OSC8_SEQUENCE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\]8;;[^\x1b]*\x1b\\").expect("OSC8_SEQUENCE_REGEX"));
static OSC8_OR_CSI_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b[^a-zA-Z]*[a-zA-Z]|\x1b\][^\n\r]*?\x1b\\").expect("OSC8_OR_CSI_REGEX"));

fn get_cell(terminal: &VirtualTerminal, row: usize, col: usize) -> VirtualCell {
    terminal
        .get_cell(row, col)
        .unwrap_or_else(|| panic!("Missing cell at row {row} col {col}"))
}

fn strip_ansi(line: &str) -> String {
    ANSI_SGR_REGEX.replace_all(line, "").into_owned()
}

fn col_of(line: &str, needle: &str, from_col: usize) -> Option<usize> {
    let start_byte = line
        .char_indices()
        .nth(from_col)
        .map(|(idx, _)| idx)
        .unwrap_or(line.len());
    let rel_byte = line[start_byte..].find(needle)?;
    let abs_byte = start_byte + rel_byte;
    Some(line[..abs_byte].chars().count())
}

fn last_col_of(line: &str, needle: &str) -> Option<usize> {
    let abs_byte = line.rfind(needle)?;
    Some(line[..abs_byte].chars().count())
}

fn chalk_wrap(open: &str, close: &str, text: &str) -> String {
    if text.is_empty() {
        String::new()
    } else {
        format!("{open}{text}{close}")
    }
}

fn chalk_gray(text: &str) -> String {
    chalk_wrap("\x1b[90m", "\x1b[39m", text)
}

fn chalk_magenta(text: &str) -> String {
    chalk_wrap("\x1b[35m", "\x1b[39m", text)
}

fn chalk_cyan(text: &str) -> String {
    chalk_wrap("\x1b[36m", "\x1b[39m", text)
}

fn chalk_yellow(text: &str) -> String {
    chalk_wrap("\x1b[33m", "\x1b[39m", text)
}

struct ResetCapabilitiesGuard;

impl Drop for ResetCapabilitiesGuard {
    fn drop(&mut self) {
        reset_capabilities_cache();
    }
}

struct StopTuiGuard<'a>(&'a TuiMainScreen);

impl Drop for StopTuiGuard<'_> {
    fn drop(&mut self) {
        self.0.stop(None);
    }
}

mod transforms {
    use super::*;
    use pretty_assertions::assert_eq;

    #[derive(Debug, PartialEq, Eq)]
    struct TransformCall {
        source: String,
        available_width: i64,
    }

    #[test]
    fn caches_transformed_markdown_by_source_and_available_width() {
        let calls: Arc<Mutex<Vec<TransformCall>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_ref = Arc::clone(&calls);
        let markdown = Markdown::new(
            "source",
            2,
            0,
            default_markdown_theme(),
            None,
            Some(MarkdownOptions {
                transform: Some(Arc::new(move |source, available_width| {
                    calls_ref.lock().unwrap().push(TransformCall {
                        source: source.to_string(),
                        available_width,
                    });
                    format!("{source} {available_width}")
                })),
                ..Default::default()
            }),
        );

        assert_eq!(
            markdown
                .render(80)
                .into_iter()
                .map(|line| strip_ansi(&line).trim().to_string())
                .collect::<Vec<_>>(),
            vec!["source 76"]
        );
        markdown.render(80);
        assert_eq!(
            markdown
                .render(60)
                .into_iter()
                .map(|line| strip_ansi(&line).trim().to_string())
                .collect::<Vec<_>>(),
            vec!["source 56"]
        );
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                TransformCall {
                    source: "source".to_string(),
                    available_width: 76,
                },
                TransformCall {
                    source: "source".to_string(),
                    available_width: 56,
                },
            ]
        );

        markdown.set_text("updated");
        assert_eq!(
            markdown
                .render(60)
                .into_iter()
                .map(|line| strip_ansi(&line).trim().to_string())
                .collect::<Vec<_>>(),
            vec!["updated 56"]
        );
        assert_eq!(
            calls.lock().unwrap().last(),
            Some(&TransformCall {
                source: "updated".to_string(),
                available_width: 56,
            })
        );

        markdown.invalidate();
        markdown.render(60);
        let guard = calls.lock().unwrap();
        assert_eq!(
            guard.last(),
            Some(&TransformCall {
                source: "updated".to_string(),
                available_width: 56,
            })
        );
        assert_eq!(guard.len(), 4);
    }
}

mod lists {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn should_render_simple_nested_list() {
        let markdown = Markdown::new(
            "- Item 1\n  - Nested 1.1\n  - Nested 1.2\n- Item 2",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        assert!(!lines.is_empty());

        let plain_lines: Vec<String> = lines.into_iter().map(|line| strip_ansi(&line)).collect();
        assert!(plain_lines.iter().any(|line| line.contains("- Item 1")));
        assert!(plain_lines.iter().any(|line| line.contains("    - Nested 1.1")));
        assert!(plain_lines.iter().any(|line| line.contains("    - Nested 1.2")));
        assert!(plain_lines.iter().any(|line| line.contains("- Item 2")));
    }

    #[test]
    fn should_render_deeply_nested_list() {
        let markdown = Markdown::new(
            "- Level 1\n  - Level 2\n    - Level 3\n      - Level 4",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines.into_iter().map(|line| strip_ansi(&line)).collect();

        assert!(plain_lines.iter().any(|line| line.contains("- Level 1")));
        assert!(plain_lines.iter().any(|line| line.contains("    - Level 2")));
        assert!(plain_lines.iter().any(|line| line.contains("        - Level 3")));
        assert!(plain_lines.iter().any(|line| line.contains("            - Level 4")));
    }

    #[test]
    fn should_render_ordered_nested_list() {
        let markdown = Markdown::new(
            "1. First\n   1. Nested first\n   2. Nested second\n2. Second",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines.into_iter().map(|line| strip_ansi(&line)).collect();

        assert!(plain_lines.iter().any(|line| line.contains("1. First")));
        assert!(plain_lines.iter().any(|line| line.contains("    1. Nested first")));
        assert!(plain_lines.iter().any(|line| line.contains("    2. Nested second")));
        assert!(plain_lines.iter().any(|line| line.contains("2. Second")));
    }

    #[test]
    fn should_normalize_ordered_list_markers_by_default() {
        let markdown = Markdown::new(
            "1. alpha\n1. beta\n1. gamma",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines: Vec<String> = markdown
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(lines, vec!["1. alpha", "2. beta", "3. gamma"]);
    }

    #[test]
    fn should_preserve_source_list_markers_when_configured() {
        let markdown = Markdown::new(
            "  4. forth\n  3. third\n\n10) ten\n7) seven\n\n+ plus\n* star\n- minus\n+",
            0,
            0,
            default_markdown_theme(),
            None,
            Some(MarkdownOptions {
                preserve_ordered_list_markers: Some(true),
                ..Default::default()
            }),
        );

        let lines: Vec<String> = markdown
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(
            lines,
            vec![
                "4. forth", "3. third", "", "10) ten", "7) seven", "", "+ plus", "* star", "- minus", "+",
            ]
        );
    }

    #[test]
    fn should_render_mixed_ordered_and_unordered_nested_lists() {
        let markdown = Markdown::new(
            "1. Ordered item\n   - Unordered nested\n   - Another nested\n2. Second ordered\n   - More nested",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines.into_iter().map(|line| strip_ansi(&line)).collect();

        assert!(plain_lines.iter().any(|line| line.contains("1. Ordered item")));
        assert!(plain_lines.iter().any(|line| line.contains("    - Unordered nested")));
        assert!(plain_lines.iter().any(|line| line.contains("2. Second ordered")));
    }

    #[test]
    fn should_render_blank_lines_between_loose_list_items() {
        let markdown = Markdown::new(
            "1. Lorem ipsum dolor sit amet.\n\n   Ut enim ad minim veniam.\n\n2. Duis aute irure dolor.\n\n   Excepteur sint occaecat cupidatat.\n\n3. Beep boop",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines: Vec<String> = markdown
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(
            lines,
            vec![
                "1. Lorem ipsum dolor sit amet.",
                "",
                "   Ut enim ad minim veniam.",
                "",
                "2. Duis aute irure dolor.",
                "",
                "   Excepteur sint occaecat cupidatat.",
                "",
                "3. Beep boop",
            ]
        );
    }

    #[test]
    fn should_render_task_list_markers() {
        let markdown = Markdown::new("- [ ] beep\n- [x] boop", 0, 0, default_markdown_theme(), None, None);

        let lines: Vec<String> = markdown
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(lines, vec!["- [ ] beep", "- [x] boop"]);
    }

    #[test]
    fn should_maintain_numbering_when_code_blocks_are_not_indented_llm_output() {
        let markdown = Markdown::new(
            "1. First item\n\n```typescript\n// code block\n```\n\n2. Second item\n\n```typescript\n// another code block\n```\n\n3. Third item",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim().to_string())
            .collect();

        let numbered_lines: Vec<String> = plain_lines
            .into_iter()
            .filter(|line| ORDERED_ITEM_PREFIX_REGEX.is_match(line))
            .collect();

        assert_eq!(
            numbered_lines.len(),
            3,
            "Expected 3 numbered items, got: {}",
            numbered_lines.join(", ")
        );
        assert!(
            numbered_lines[0].starts_with("1."),
            "First item should be \"1.\", got: {}",
            numbered_lines[0]
        );
        assert!(
            numbered_lines[1].starts_with("2."),
            "Second item should be \"2.\", got: {}",
            numbered_lines[1]
        );
        assert!(
            numbered_lines[2].starts_with("3."),
            "Third item should be \"3.\", got: {}",
            numbered_lines[2]
        );
    }

    #[test]
    fn should_indent_wrapped_unordered_list_lines() {
        let markdown = Markdown::new(
            "- alpha beta gamma delta epsilon",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines: Vec<String> = markdown
            .render(20)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(lines, vec!["- alpha beta gamma", "  delta epsilon"]);
    }

    #[test]
    fn should_indent_wrapped_ordered_list_lines() {
        let markdown = Markdown::new(
            "1. alpha beta gamma delta epsilon",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines: Vec<String> = markdown
            .render(20)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(lines, vec!["1. alpha beta gamma", "   delta epsilon"]);
    }

    #[test]
    fn should_indent_wrapped_ordered_list_lines_with_multi_digit_markers() {
        let markdown = Markdown::new(
            "10. alpha beta gamma delta epsilon",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines: Vec<String> = markdown
            .render(21)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(lines, vec!["10. alpha beta gamma", "    delta epsilon"]);
    }

    #[test]
    fn should_indent_wrapped_nested_list_lines() {
        let markdown = Markdown::new(
            "- parent\n  - alpha beta gamma delta epsilon",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines: Vec<String> = markdown
            .render(24)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(lines, vec!["- parent", "    - alpha beta gamma", "      delta epsilon"]);
    }

    #[test]
    fn should_indent_wrapped_nested_list_lines_under_ordered_parents() {
        let markdown = Markdown::new(
            "1. parent\n   - alpha beta gamma delta epsilon",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines: Vec<String> = markdown
            .render(24)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(
            lines,
            vec!["1. parent", "    - alpha beta gamma", "      delta epsilon"]
        );
    }

    #[test]
    fn should_render_and_wrap_blockquotes_inside_list_items() {
        let markdown = Markdown::new(
            "- > alpha beta gamma delta epsilon zeta",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines: Vec<String> = markdown
            .render(24)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(lines, vec!["- │ alpha beta gamma", "  │ delta epsilon zeta"]);
    }

    #[test]
    fn should_render_and_wrap_code_blocks_inside_list_items() {
        let markdown = Markdown::new(
            "- ```ts\n  alpha beta gamma delta epsilon zeta\n  ```",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines: Vec<String> = markdown
            .render(24)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(
            lines,
            vec!["- ```ts", "    alpha beta gamma", "  delta epsilon zeta", "  ```"]
        );
    }
}

mod tables {
    use super::*;
    use pretty_assertions::{assert_eq, assert_ne};

    #[test]
    fn should_render_simple_table() {
        let markdown = Markdown::new(
            "| Name | Age |\n| --- | --- |\n| Alice | 30 |\n| Bob | 25 |",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines.into_iter().map(|line| strip_ansi(&line)).collect();

        assert!(plain_lines.iter().any(|line| line.contains("Name")));
        assert!(plain_lines.iter().any(|line| line.contains("Age")));
        assert!(plain_lines.iter().any(|line| line.contains("Alice")));
        assert!(plain_lines.iter().any(|line| line.contains("Bob")));
        assert!(plain_lines.iter().any(|line| line.contains("│")));
        assert!(plain_lines.iter().any(|line| line.contains("─")));
    }

    #[test]
    fn should_render_row_dividers_between_data_rows() {
        let markdown = Markdown::new(
            "| Name | Age |\n| --- | --- |\n| Alice | 30 |\n| Bob | 25 |",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines.into_iter().map(|line| strip_ansi(&line)).collect();
        let divider_lines: Vec<&String> = plain_lines.iter().filter(|line| line.contains("┼")).collect();

        assert_eq!(divider_lines.len(), 2, "Expected header + row divider");
    }

    #[test]
    fn should_keep_column_width_at_least_the_longest_word() {
        let longest_word = "superlongword";
        let source = format!(
            "| Column One | Column Two |\n| --- | --- |\n| {longest_word} short | otherword |\n| small | tiny |"
        );
        let markdown = Markdown::new(&source, 0, 0, default_markdown_theme(), None, None);

        let lines = markdown.render(32);
        let plain_lines: Vec<String> = lines.into_iter().map(|line| strip_ansi(&line)).collect();
        let data_line = plain_lines
            .iter()
            .find(|line| line.contains(longest_word))
            .expect("Expected data row containing longest word");

        let segments: Vec<&str> = data_line.split('│').collect();
        assert!(segments.len() >= 3, "Expected table segments");
        let first_segment = segments[1];
        let first_column_width = pi_js::str16::len(first_segment) - 2;

        assert!(
            first_column_width >= longest_word.len(),
            "Expected first column width >= {}, got {first_column_width}",
            longest_word.len()
        );
    }

    #[test]
    fn should_render_table_with_alignment() {
        let markdown = Markdown::new(
            "| Left | Center | Right |\n| :--- | :---: | ---: |\n| A | B | C |\n| Long text | Middle | End |",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines.into_iter().map(|line| strip_ansi(&line)).collect();

        assert!(plain_lines.iter().any(|line| line.contains("Left")));
        assert!(plain_lines.iter().any(|line| line.contains("Center")));
        assert!(plain_lines.iter().any(|line| line.contains("Right")));
        assert!(plain_lines.iter().any(|line| line.contains("Long text")));
    }

    #[test]
    fn should_handle_tables_with_varying_column_widths() {
        let markdown = Markdown::new(
            "| Short | Very long column header |\n| --- | --- |\n| A | This is a much longer cell content |\n| B | Short |",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        assert!(!lines.is_empty());

        let plain_lines: Vec<String> = lines.into_iter().map(|line| strip_ansi(&line)).collect();
        assert!(plain_lines.iter().any(|line| line.contains("Very long column header")));
        assert!(
            plain_lines
                .iter()
                .any(|line| line.contains("This is a much longer cell content"))
        );
    }

    #[test]
    fn should_wrap_table_cells_when_table_exceeds_available_width() {
        let markdown = Markdown::new(
            "| Command | Description | Example |\n| --- | --- | --- |\n| npm install | Install all dependencies | npm install |\n| npm run build | Build the project | npm run build |",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(50);
        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        for line in &plain_lines {
            let len = pi_js::str16::len(line);
            assert!(len <= 50, "Line exceeds width 50: \"{line}\" (length: {len})");
        }

        let all_text = plain_lines.join(" ");
        assert!(all_text.contains("Command"), "Should contain 'Command'");
        assert!(all_text.contains("Description"), "Should contain 'Description'");
        assert!(all_text.contains("npm install"), "Should contain 'npm install'");
        assert!(all_text.contains("Install"), "Should contain 'Install'");
    }

    #[tokio::test]
    #[serial]
    async fn should_not_leak_wrapped_link_styles_into_table_borders_or_plain_cells() {
        let _caps_guard = ResetCapabilitiesGuard;
        let source =
            "| Link | Plain |\n| --- | --- |\n| [**one two three four five six**](https://example.com) | normal text |";

        for hyperlinks in [true, false] {
            set_capabilities(TerminalCapabilities {
                images: None,
                true_color: false,
                hyperlinks,
            });
            let terminal = Arc::new(VirtualTerminal::new(24, 16));
            let tui = TuiMainScreen::new(terminal.clone(), None, None);
            tui.add_child(Arc::new(Markdown::new(
                source,
                0,
                0,
                default_markdown_theme(),
                None,
                None,
            )));
            tui.start();

            {
                let _stop_guard = StopTuiGuard(&tui);
                terminal.wait_for_render().await;
                let viewport = terminal.get_viewport();
                let row = viewport
                    .iter()
                    .position(|line| line.contains("one") && line.contains("norm"))
                    .unwrap_or_else(|| panic!("Missing wrapped table row: {viewport:?}"));
                let line = &viewport[row];
                let link_col = col_of(line, "one", 0).unwrap();
                let separator_col = col_of(line, "│", link_col).unwrap();
                let plain_col = col_of(line, "norm", 0).unwrap();
                assert!(separator_col > link_col && plain_col > separator_col);

                assert!(!get_cell(&terminal, row, link_col).is_fg_default());
                assert!(get_cell(&terminal, row, separator_col).is_fg_default());
                assert!(get_cell(&terminal, row, plain_col).is_fg_default());
                assert_ne!(get_cell(&terminal, row, link_col).is_bold(), 0);
                assert_eq!(get_cell(&terminal, row, separator_col).is_bold(), 0);
                assert_eq!(get_cell(&terminal, row, plain_col).is_bold(), 0);

                if !hyperlinks {
                    let url_row = viewport
                        .iter()
                        .position(|viewport_line| viewport_line.contains("https"))
                        .unwrap_or_else(|| panic!("Missing fallback URL row: {viewport:?}"));
                    let url_line = &viewport[url_row];
                    let url_col = col_of(url_line, "https", 0).unwrap();
                    let url_separator_col = col_of(url_line, "│", url_col).unwrap();
                    let url_border_col = last_col_of(url_line, "│").unwrap();
                    assert!(url_separator_col > url_col && url_border_col > url_separator_col);
                    assert_ne!(get_cell(&terminal, url_row, url_col).is_dim(), 0);
                    assert_eq!(get_cell(&terminal, url_row, url_separator_col).is_dim(), 0);
                    assert_eq!(get_cell(&terminal, url_row, url_border_col).is_dim(), 0);
                }
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn should_restore_the_enclosing_style_after_a_wrapped_table_link() {
        let _caps_guard = ResetCapabilitiesGuard;
        let quote_color: i64 = 0x123456;
        let base_theme = default_markdown_theme();
        let theme = MarkdownTheme {
            quote: Arc::new(|text| format!("\x1b[38;2;18;52;86m{text}\x1b[39m")),
            link: Arc::new(|text| format!("\x1b[38;2;129;162;190m{text}\x1b[39m")),
            ..base_theme
        };
        let source = "> | Link | Plain |\n> | --- | --- |\n> | [one two three four five six](https://example.com) | normal text |";

        set_capabilities(TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        });
        let terminal = Arc::new(VirtualTerminal::new(28, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        tui.add_child(Arc::new(Markdown::new(source, 0, 0, theme, None, None)));
        tui.start();
        let _stop_guard = StopTuiGuard(&tui);

        terminal.wait_for_render().await;
        let viewport = terminal.get_viewport();
        let row = viewport
            .iter()
            .position(|line| line.contains("one") && line.contains("normal"))
            .unwrap_or_else(|| panic!("Missing wrapped blockquote table row: {viewport:?}"));
        let line = &viewport[row];
        let link_col = col_of(line, "one", 0).unwrap();
        let separator_col = col_of(line, "│", link_col).unwrap();
        let plain_col = col_of(line, "normal", 0).unwrap();
        assert!(separator_col > link_col && plain_col > separator_col);

        assert_ne!(get_cell(&terminal, row, link_col).get_fg_color(), quote_color);
        assert_eq!(get_cell(&terminal, row, separator_col).get_fg_color(), quote_color);
        assert_eq!(get_cell(&terminal, row, plain_col).get_fg_color(), quote_color);

        let final_row = viewport
            .iter()
            .position(|viewport_line| viewport_line.contains("five six"))
            .unwrap_or_else(|| panic!("Missing final wrapped link row: {viewport:?}"));
        let final_line = &viewport[final_row];
        let final_link_col = col_of(final_line, "five six", 0).unwrap();
        let final_separator_col = col_of(final_line, "│", final_link_col).unwrap();
        let final_border_col = last_col_of(final_line, "│").unwrap();
        assert!(final_separator_col > final_link_col && final_border_col > final_separator_col);
        assert_eq!(
            get_cell(&terminal, final_row, final_separator_col).get_fg_color(),
            quote_color
        );
        assert_eq!(
            get_cell(&terminal, final_row, final_border_col).get_fg_color(),
            quote_color
        );
    }

    #[test]
    fn should_wrap_long_cell_content_to_multiple_lines() {
        let markdown = Markdown::new(
            "| Header |\n| --- |\n| This is a very long cell content that should wrap |",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(25);
        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        let data_rows: Vec<&String> = plain_lines
            .iter()
            .filter(|line| line.starts_with('│') && !line.contains('─'))
            .collect();
        assert!(
            data_rows.len() > 2,
            "Expected wrapped rows, got {} rows",
            data_rows.len()
        );

        let all_text = plain_lines.join(" ");
        assert!(all_text.contains("very long"), "Should preserve 'very long'");
        assert!(all_text.contains("cell content"), "Should preserve 'cell content'");
        assert!(all_text.contains("should wrap"), "Should preserve 'should wrap'");
    }

    #[test]
    #[serial]
    fn should_wrap_long_unbroken_tokens_inside_table_cells_not_only_at_line_start() {
        let _caps_guard = ResetCapabilitiesGuard;
        set_capabilities(TerminalCapabilities {
            images: None,
            true_color: false,
            hyperlinks: false,
        });
        let url = "https://example.com/this/is/a/very/long/url/that/should/wrap";
        let source = format!("| Value |\n| --- |\n| prefix {url} |");
        let markdown = Markdown::new(&source, 0, 0, default_markdown_theme(), None, None);

        let width = 30usize;
        let lines = markdown.render(width);
        reset_capabilities_cache();
        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        for line in &plain_lines {
            let len = pi_js::str16::len(line);
            assert!(len <= width, "Line exceeds width {width}: \"{line}\" (length: {len})");
        }

        let table_lines: Vec<&String> = plain_lines.iter().filter(|line| line.starts_with('│')).collect();
        assert!(!table_lines.is_empty(), "Expected table rows to render");
        for line in table_lines {
            let border_count = line.matches('│').count();
            assert_eq!(border_count, 2, "Expected 2 borders, got {border_count}: \"{line}\"");
        }

        let joined = plain_lines.join("");
        let extracted = TABLE_CHARS_OR_WS_REGEX.replace_all(&joined, "");
        assert!(extracted.contains("prefix"), "Should preserve 'prefix'");
        assert!(extracted.contains(url), "Should preserve URL");
    }

    #[test]
    fn should_wrap_styled_inline_code_inside_table_cells_without_breaking_borders() {
        let markdown = Markdown::new(
            "| Code |\n| --- |\n| `averyveryveryverylongidentifier` |",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let width = 20usize;
        let lines = markdown.render(width);
        let joined_output = lines.join("\n");
        assert!(
            joined_output.contains("\x1b[33m"),
            "Inline code should be styled (yellow)"
        );

        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();
        for line in &plain_lines {
            let len = pi_js::str16::len(line);
            assert!(len <= width, "Line exceeds width {width}: \"{line}\" (length: {len})");
        }

        let table_lines: Vec<&String> = plain_lines.iter().filter(|line| line.starts_with('│')).collect();
        for line in table_lines {
            let border_count = line.matches('│').count();
            assert_eq!(border_count, 2, "Expected 2 borders, got {border_count}: \"{line}\"");
        }
    }

    #[test]
    fn should_handle_extremely_narrow_width_gracefully() {
        let markdown = Markdown::new(
            "| A | B | C |\n| --- | --- | --- |\n| 1 | 2 | 3 |",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(15);
        assert!(!lines.is_empty(), "Should produce output");

        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();
        for line in &plain_lines {
            let len = pi_js::str16::len(line);
            assert!(len <= 15, "Line exceeds width 15: \"{line}\" (length: {len})");
        }
    }

    #[test]
    fn should_render_table_correctly_when_it_fits_naturally() {
        let markdown = Markdown::new(
            "| A | B |\n| --- | --- |\n| 1 | 2 |",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        let header_line = plain_lines
            .iter()
            .find(|line| line.contains('A') && line.contains('B'))
            .expect("Should have header row");
        assert!(header_line.contains('│'), "Header should have borders");

        let separator_line = plain_lines.iter().find(|line| line.contains('├') && line.contains('┼'));
        assert!(separator_line.is_some(), "Should have separator row");

        let data_line = plain_lines.iter().find(|line| line.contains('1') && line.contains('2'));
        assert!(data_line.is_some(), "Should have data row");
    }

    #[test]
    fn should_respect_padding_x_when_calculating_table_width() {
        let markdown = Markdown::new(
            "| Column One | Column Two |\n| --- | --- |\n| Data 1 | Data 2 |",
            2,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(40);
        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        for line in &plain_lines {
            let len = pi_js::str16::len(line);
            assert!(len <= 40, "Line exceeds width 40: \"{line}\" (length: {len})");
        }

        let table_row = plain_lines.iter().find(|line| line.contains('│'));
        assert!(
            table_row.is_some_and(|row| row.starts_with("  ")),
            "Table should have left padding"
        );
    }

    #[test]
    fn should_not_add_a_trailing_blank_line_when_table_is_the_last_rendered_block() {
        let markdown = Markdown::new(
            "| Name |\n| --- |\n| Alice |",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_ne!(
            plain_lines.last().map(String::as_str),
            Some(""),
            "Expected table to end without a blank line: {plain_lines:?}"
        );
    }
}

mod combined_features {
    use super::*;

    #[test]
    fn should_render_lists_and_tables_together() {
        let markdown = Markdown::new(
            "# Test Document\n\n- Item 1\n  - Nested item\n- Item 2\n\n| Col1 | Col2 |\n| --- | --- |\n| A | B |",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines.into_iter().map(|line| strip_ansi(&line)).collect();

        assert!(plain_lines.iter().any(|line| line.contains("Test Document")));
        assert!(plain_lines.iter().any(|line| line.contains("- Item 1")));
        assert!(plain_lines.iter().any(|line| line.contains("    - Nested item")));
        assert!(plain_lines.iter().any(|line| line.contains("Col1")));
        assert!(plain_lines.iter().any(|line| line.contains('│')));
    }
}

mod latex_math {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn renders_inline_dollar_and_parenthesis_delimiters() {
        let markdown = Markdown::new(
            r"A map $\mathbb{C}^3 \to \mathbb{C}^3$, $xy$, $x-y$, $-x$, $\frac{1}{2}$, $\rightarrow$, and \(s \to \infty\).",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines: Vec<String> = markdown
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(lines, vec!["A map ℂ³ → ℂ³, xy, x-y, -x, 1/2, →, and s → ∞."]);
    }

    #[test]
    fn renders_display_dollar_delimiters_without_markdown_escape_corruption() {
        let markdown = Markdown::new(
            "Before\n\n$$\\{3x+2y,\\; x \\in \\{0, \\pm 1\\}\\}$$\n\nafter",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines: Vec<String> = markdown
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(lines, vec!["Before", "", "{3x+2y, x ∈ {0, ± 1}}", "", "after"]);
    }

    #[test]
    fn renders_display_bracket_delimiters() {
        let markdown = Markdown::new(
            "Before\n\n\\[\nE \\approx \\frac{0.1\\ \\text{lux}}{100\\ \\text{lm/W}}\n\\]\n\nafter",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines: Vec<String> = markdown
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(
            lines,
            vec!["Before", "", "    0.1 lux", "E ≈ ────────", "    100 lm/W", "", "after"]
        );
    }

    #[test]
    fn aligns_matrix_rows_with_the_opening_delimiter() {
        let markdown = Markdown::new(
            "Consider the matrix\n\n\\[\nA=\n\\begin{pmatrix}\n\\pi & 0\\\\\n0 & \\frac{1}{\\pi}\n\\end{pmatrix}.\n\\]",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines: Vec<String> = markdown
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(
            lines,
            vec!["Consider the matrix", "", "A = ⎛ π │ 0   ⎞", "    ⎝ 0 │ 1/π ⎠."]
        );
    }

    #[test]
    fn renders_lower_limits_beneath_display_operators() {
        let markdown = Markdown::new(
            "\\[\n\\lim_{x\\to 0}\\frac{\\frac{\\sin x}{x}-1}{\\frac{e^x-1}{x}-1}=0\n\\]",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines: Vec<String> = markdown
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(
            lines,
            vec!["     (sin x)/x-1", "lim  ─────────── = 0", "x→0  (eˣ-1)/x-1"]
        );
    }

    #[test]
    fn renders_math_inside_lists_and_tables() {
        let markdown = Markdown::new(
            "- Formula: $F_1 = u^2$\n\n| Value |\n| --- |\n| $\\mathbb{C}^3$ |",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines: Vec<String> = markdown
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();
        let output = lines.join("\n");

        assert!(output.contains("- Formula: F₁ = u²"));
        assert!(output.contains("│ ℂ³"));
    }

    #[test]
    fn does_not_treat_currency_shell_variables_or_code_spans_as_math() {
        let source = "Costs $5 and $10 or $8k–$12k; use `$x$`, $HOME, and ${PATH}.";
        let markdown = Markdown::new(source, 0, 0, default_markdown_theme(), None, None);

        let lines: Vec<String> = markdown
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(
            lines,
            vec!["Costs $5 and $10 or $8k–$12k; use $x$, $HOME, and ${PATH}."]
        );

        let shell_variables = "Paths: $HOME/$USER and $XDG_CONFIG_HOME/$APP_CONFIG";
        let shell_lines: Vec<String> = Markdown::new(shell_variables, 0, 0, default_markdown_theme(), None, None)
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();
        assert_eq!(shell_lines, vec![shell_variables]);
    }

    #[test]
    fn preserves_unsupported_and_incomplete_latex_exactly() {
        let cases = [r"Unknown $x + \unknown{y}$ after", r"Streaming $\mathbb{C}^3"];

        for source in cases {
            let markdown = Markdown::new(source, 0, 0, default_markdown_theme(), None, None);
            let lines: Vec<String> = markdown
                .render(80)
                .into_iter()
                .map(|line| strip_ansi(&line).trim_end().to_string())
                .collect();
            assert_eq!(lines, vec![source]);
        }
    }

    #[test]
    fn preserves_incomplete_backslash_delimiters_while_streaming() {
        let inline = Markdown::new(r"Map \(\mathbb{C}^3", 0, 0, default_markdown_theme(), None, None);
        assert_eq!(
            inline
                .render(80)
                .into_iter()
                .map(|line| strip_ansi(&line).trim_end().to_string())
                .collect::<Vec<_>>(),
            vec![r"Map \(\mathbb{C}^3"]
        );

        let display = Markdown::new("\\[\nx^2", 0, 0, default_markdown_theme(), None, None);
        assert_eq!(
            display
                .render(80)
                .into_iter()
                .map(|line| strip_ansi(&line).trim_end().to_string())
                .collect::<Vec<_>>(),
            vec!["\\[", "x^2"]
        );
    }

    #[test]
    fn does_not_render_latex_inside_escaped_delimiters_or_code_fences() {
        let source = [r"Escaped \$x-y\$.", "", "```text", r"$\mathbb{C}^3$", "```"].join("\n");
        let markdown = Markdown::new(&source, 0, 0, default_markdown_theme(), None, None);
        let lines: Vec<String> = markdown
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(lines, vec!["Escaped $x-y$.", "", "```text", "  $\\mathbb{C}^3$", "```"]);
    }

    #[test]
    fn allows_latex_rendering_to_be_disabled() {
        let markdown = Markdown::new(
            "$$\n\\widetilde Y_{sf}\n=\n(1-w_{sf})\\mu_{sf}^{\\mathrm{MAR}}\n$$\n\nInline \\(A_{sf}\\)",
            0,
            0,
            default_markdown_theme(),
            None,
            Some(MarkdownOptions {
                render_latex: Some(false),
                ..Default::default()
            }),
        );

        assert_eq!(
            markdown
                .render(80)
                .into_iter()
                .map(|line| strip_ansi(&line).trim_end().to_string())
                .collect::<Vec<_>>(),
            vec![
                "$$",
                r"\widetilde Y_{sf}",
                "=",
                r"(1-w_{sf})\mu_{sf}^{\mathrm{MAR}}",
                "$$",
                "",
                r"Inline \(A_{sf}\)",
            ]
        );
    }

    #[test]
    fn switches_from_raw_to_rendered_math_when_a_streamed_delimiter_closes() {
        let markdown = Markdown::new(r"Map $\mathbb{C}^3", 0, 0, default_markdown_theme(), None, None);
        assert_eq!(
            markdown
                .render(80)
                .into_iter()
                .map(|line| strip_ansi(&line).trim_end().to_string())
                .collect::<Vec<_>>(),
            vec![r"Map $\mathbb{C}^3"]
        );

        markdown.set_text(r"Map $\mathbb{C}^3$");

        assert_eq!(
            markdown
                .render(80)
                .into_iter()
                .map(|line| strip_ansi(&line).trim_end().to_string())
                .collect::<Vec<_>>(),
            vec!["Map ℂ³"]
        );
    }
}

mod backslash_escapes {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn should_normalize_escaped_punctuation_by_default() {
        let markdown = Markdown::new(r#""\""#, 0, 0, default_markdown_theme(), None, None);

        let lines: Vec<String> = markdown
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(lines, vec![r#""""#]);
    }

    #[test]
    fn should_preserve_source_backslash_escapes_when_configured() {
        let markdown = Markdown::new(
            r#""\""#,
            0,
            0,
            default_markdown_theme(),
            None,
            Some(MarkdownOptions {
                preserve_backslash_escapes: Some(true),
                ..Default::default()
            }),
        );

        let lines: Vec<String> = markdown
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_eq!(lines, vec![r#""\""#]);
    }
}

mod pre_styled_text_thinking_traces {
    use super::*;
    use pretty_assertions::assert_eq;

    struct MarkdownWithInput {
        markdown_line_count: AtomicUsize,
        markdown: Markdown,
    }

    impl Component for MarkdownWithInput {
        fn render(&self, width: usize) -> Vec<String> {
            let mut lines = self.markdown.render(width);
            self.markdown_line_count.store(lines.len(), Ordering::Relaxed);
            lines.push("INPUT".to_string());
            lines
        }

        fn handle_input(&self, _data: &str) {}

        fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
            None
        }

        fn wants_key_release(&self) -> bool {
            false
        }

        fn invalidate(&self) {
            self.markdown.invalidate();
        }
    }

    #[test]
    fn should_preserve_gray_italic_styling_after_inline_code() {
        let markdown = Markdown::new(
            "This is thinking with `inline code` and more text after",
            1,
            0,
            default_markdown_theme(),
            Some(DefaultTextStyle {
                color: Some(Arc::new(chalk_gray)),
                italic: Some(true),
                ..Default::default()
            }),
            None,
        );

        let lines = markdown.render(80);
        let joined_output = lines.join("\n");

        assert!(joined_output.contains("inline code"));
        assert!(joined_output.contains("\x1b[90m"), "Should have gray color code");
        assert!(joined_output.contains("\x1b[3m"), "Should have italic code");
        assert!(joined_output.contains("\x1b[33m"), "Should style inline code");
    }

    #[test]
    fn should_preserve_gray_italic_styling_after_bold_text() {
        let markdown = Markdown::new(
            "This is thinking with **bold text** and more after",
            1,
            0,
            default_markdown_theme(),
            Some(DefaultTextStyle {
                color: Some(Arc::new(chalk_gray)),
                italic: Some(true),
                ..Default::default()
            }),
            None,
        );

        let lines = markdown.render(80);
        let joined_output = lines.join("\n");

        assert!(joined_output.contains("bold text"));
        assert!(joined_output.contains("\x1b[90m"), "Should have gray color code");
        assert!(joined_output.contains("\x1b[3m"), "Should have italic code");
        assert!(joined_output.contains("\x1b[1m"), "Should have bold code");
    }

    #[tokio::test]
    async fn should_not_leak_styles_into_following_lines_when_rendered_in_tui() {
        let markdown = Markdown::new(
            "This is thinking with `inline code`",
            1,
            0,
            default_markdown_theme(),
            Some(DefaultTextStyle {
                color: Some(Arc::new(chalk_gray)),
                italic: Some(true),
                ..Default::default()
            }),
            None,
        );

        let terminal = Arc::new(VirtualTerminal::new(80, 6));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(MarkdownWithInput {
            markdown_line_count: AtomicUsize::new(0),
            markdown,
        });
        tui.add_child(component.clone());
        tui.start();
        let _stop_guard = StopTuiGuard(&tui);
        terminal.wait_for_render().await;

        let line_count = component.markdown_line_count.load(Ordering::Relaxed);
        assert!(line_count > 0);
        assert_eq!(get_cell(&terminal, line_count, 0).is_italic(), 0);
    }
}

mod spacing_after_code_blocks {
    use super::*;
    use pretty_assertions::{assert_eq, assert_ne};

    #[test]
    fn should_have_only_one_blank_line_between_code_block_and_following_paragraph() {
        let markdown = Markdown::new(
            "hello world\n\n```js\nconst hello = \"world\";\n```\n\nagain, hello world",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        let closing_backticks_index = plain_lines
            .iter()
            .position(|line| line == "```")
            .expect("Should have closing backticks");

        let after_backticks = &plain_lines[closing_backticks_index + 1..];
        let empty_line_count = after_backticks.iter().position(|line| !line.is_empty());

        assert_eq!(
            empty_line_count,
            Some(1),
            "Expected 1 empty line after code block, but found {empty_line_count:?}. Lines after backticks: {:?}",
            &after_backticks[..after_backticks.len().min(5)]
        );
    }

    #[test]
    fn should_normalize_paragraph_and_code_block_spacing_to_one_blank_line() {
        let cases = [
            "hello this is text\n```\ncode block\n```\nmore text",
            "hello this is text\n\n```\ncode block\n```\n\nmore text",
        ];
        let expected_lines = vec!["hello this is text", "", "```", "  code block", "```", "", "more text"];

        for text in cases {
            let markdown = Markdown::new(text, 0, 0, default_markdown_theme(), None, None);
            let lines = markdown.render(80);
            let plain_lines: Vec<String> = lines
                .into_iter()
                .map(|line| strip_ansi(&line).trim_end().to_string())
                .collect();

            assert_eq!(plain_lines, expected_lines, "Unexpected spacing for markdown: {text:?}");
        }
    }

    #[test]
    fn should_not_add_a_trailing_blank_line_when_code_block_is_the_last_rendered_block() {
        let cases = [
            "```js\nconst hello = 'world';\n```",
            "hello world\n\n```js\nconst hello = 'world';\n```",
        ];

        for text in cases {
            let markdown = Markdown::new(text, 0, 0, default_markdown_theme(), None, None);
            let lines = markdown.render(80);
            let plain_lines: Vec<String> = lines
                .into_iter()
                .map(|line| strip_ansi(&line).trim_end().to_string())
                .collect();

            assert_ne!(
                plain_lines.last().map(String::as_str),
                Some(""),
                "Expected code block to end without a blank line: {plain_lines:?}"
            );
        }
    }
}

mod spacing_after_dividers {
    use super::*;
    use pretty_assertions::{assert_eq, assert_ne};

    #[test]
    fn should_have_only_one_blank_line_between_divider_and_following_paragraph() {
        let markdown = Markdown::new(
            "hello world\n\n---\n\nagain, hello world",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        let divider_index = plain_lines
            .iter()
            .position(|line| line.contains('─'))
            .expect("Should have divider");

        let after_divider = &plain_lines[divider_index + 1..];
        let empty_line_count = after_divider.iter().position(|line| !line.is_empty());

        assert_eq!(
            empty_line_count,
            Some(1),
            "Expected 1 empty line after divider, but found {empty_line_count:?}. Lines after divider: {:?}",
            &after_divider[..after_divider.len().min(5)]
        );
    }

    #[test]
    fn should_not_add_a_trailing_blank_line_when_divider_is_the_last_rendered_block() {
        let markdown = Markdown::new("---", 0, 0, default_markdown_theme(), None, None);
        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_ne!(
            plain_lines.last().map(String::as_str),
            Some(""),
            "Expected divider to end without a blank line: {plain_lines:?}"
        );
    }
}

mod spacing_after_headings {
    use super::*;
    use pretty_assertions::{assert_eq, assert_ne};

    #[test]
    fn should_have_only_one_blank_line_between_heading_and_following_paragraph() {
        let markdown = Markdown::new(
            "# Hello\n\nThis is a paragraph",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        let heading_index = plain_lines
            .iter()
            .position(|line| line.contains("Hello"))
            .expect("Should have heading");

        let after_heading = &plain_lines[heading_index + 1..];
        let empty_line_count = after_heading.iter().position(|line| !line.is_empty());

        assert_eq!(
            empty_line_count,
            Some(1),
            "Expected 1 empty line after heading, but found {empty_line_count:?}. Lines after heading: {:?}",
            &after_heading[..after_heading.len().min(5)]
        );
    }

    #[test]
    fn should_not_add_a_trailing_blank_line_when_heading_is_the_last_rendered_block() {
        let markdown = Markdown::new("# Hello", 0, 0, default_markdown_theme(), None, None);
        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_ne!(
            plain_lines.last().map(String::as_str),
            Some(""),
            "Expected heading to end without a blank line: {plain_lines:?}"
        );
    }
}

mod spacing_after_blockquotes {
    use super::*;
    use pretty_assertions::{assert_eq, assert_ne};

    #[test]
    fn should_have_only_one_blank_line_between_blockquote_and_following_paragraph() {
        let markdown = Markdown::new(
            "hello world\n\n> This is a quote\n\nagain, hello world",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        let quote_index = plain_lines
            .iter()
            .position(|line| line.contains("This is a quote"))
            .expect("Should have blockquote");

        let after_quote = &plain_lines[quote_index + 1..];
        let empty_line_count = after_quote.iter().position(|line| !line.is_empty());

        assert_eq!(
            empty_line_count,
            Some(1),
            "Expected 1 empty line after blockquote, but found {empty_line_count:?}. Lines after quote: {:?}",
            &after_quote[..after_quote.len().min(5)]
        );
    }

    #[test]
    fn should_not_add_a_trailing_blank_line_when_blockquote_is_the_last_rendered_block() {
        let markdown = Markdown::new("> This is a quote", 0, 0, default_markdown_theme(), None, None);
        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        assert_ne!(
            plain_lines.last().map(String::as_str),
            Some(""),
            "Expected blockquote to end without a blank line: {plain_lines:?}"
        );
    }
}

mod blockquotes_with_multiline_content {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn should_apply_consistent_styling_to_all_lines_in_lazy_continuation_blockquote() {
        let markdown = Markdown::new(
            ">Foo\nbar",
            0,
            0,
            default_markdown_theme(),
            Some(DefaultTextStyle {
                color: Some(Arc::new(chalk_magenta)),
                ..Default::default()
            }),
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines.iter().map(|line| strip_ansi(line)).collect();
        let quoted_lines: Vec<&String> = plain_lines.iter().filter(|line| line.starts_with("│ ")).collect();
        assert_eq!(quoted_lines.len(), 2, "Expected 2 quoted lines, got: {plain_lines:?}");

        let foo_line = lines
            .iter()
            .find(|line| line.contains("Foo"))
            .expect("Should have Foo line");
        let bar_line = lines
            .iter()
            .find(|line| line.contains("bar"))
            .expect("Should have bar line");

        assert!(foo_line.contains("\x1b[3m"), "Foo line should have italic: {foo_line}");
        assert!(bar_line.contains("\x1b[3m"), "bar line should have italic: {bar_line}");
        assert!(
            !foo_line.contains("\x1b[35m"),
            "Foo line should NOT have magenta color: {foo_line}"
        );
        assert!(
            !bar_line.contains("\x1b[35m"),
            "bar line should NOT have magenta color: {bar_line}"
        );
    }

    #[test]
    fn should_apply_consistent_styling_to_explicit_multiline_blockquote() {
        let markdown = Markdown::new(
            ">Foo\n>bar",
            0,
            0,
            default_markdown_theme(),
            Some(DefaultTextStyle {
                color: Some(Arc::new(chalk_cyan)),
                ..Default::default()
            }),
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines.iter().map(|line| strip_ansi(line)).collect();
        let quoted_lines: Vec<&String> = plain_lines.iter().filter(|line| line.starts_with("│ ")).collect();
        assert_eq!(quoted_lines.len(), 2, "Expected 2 quoted lines, got: {plain_lines:?}");

        let foo_line = lines
            .iter()
            .find(|line| line.contains("Foo"))
            .expect("Should have Foo line");
        let bar_line = lines
            .iter()
            .find(|line| line.contains("bar"))
            .expect("Should have bar line");
        assert!(foo_line.contains("\x1b[3m"), "Foo line should have italic: {foo_line}");
        assert!(bar_line.contains("\x1b[3m"), "bar line should have italic: {bar_line}");
        assert!(
            !foo_line.contains("\x1b[36m"),
            "Foo line should NOT have cyan color: {foo_line}"
        );
        assert!(
            !bar_line.contains("\x1b[36m"),
            "bar line should NOT have cyan color: {bar_line}"
        );
    }

    #[test]
    fn should_render_list_content_inside_blockquotes() {
        let markdown = Markdown::new(
            "> 1. bla bla\n> - nested bullet",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines.into_iter().map(|line| strip_ansi(&line)).collect();
        let quoted_lines: Vec<&String> = plain_lines.iter().filter(|line| line.starts_with("│ ")).collect();

        assert!(
            quoted_lines.iter().any(|line| line.contains("1. bla bla")),
            "Missing ordered list item: {quoted_lines:?}"
        );
        assert!(
            quoted_lines.iter().any(|line| line.contains("- nested bullet")),
            "Missing unordered list item: {quoted_lines:?}"
        );
    }

    #[test]
    fn should_wrap_long_blockquote_lines_and_add_border_to_each_wrapped_line() {
        let long_text = "This is a very long blockquote line that should wrap to multiple lines when rendered";
        let markdown = Markdown::new(&format!("> {long_text}"), 0, 0, default_markdown_theme(), None, None);

        let lines = markdown.render(30);
        let plain_lines: Vec<String> = lines
            .into_iter()
            .map(|line| strip_ansi(&line).trim_end().to_string())
            .collect();

        let content_lines: Vec<&String> = plain_lines.iter().filter(|line| !line.is_empty()).collect();
        assert!(
            content_lines.len() > 1,
            "Expected multiple wrapped lines, got: {content_lines:?}"
        );

        for line in &content_lines {
            assert!(
                line.starts_with("│ "),
                "Wrapped line should have quote border: \"{line}\""
            );
        }

        let all_text = content_lines.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
        assert!(all_text.contains("very long"), "Should preserve 'very long'");
        assert!(all_text.contains("blockquote"), "Should preserve 'blockquote'");
        assert!(all_text.contains("multiple"), "Should preserve 'multiple'");
    }

    #[test]
    fn should_properly_indent_wrapped_blockquote_lines_with_styling() {
        let markdown = Markdown::new(
            "> This is styled text that is long enough to wrap",
            0,
            0,
            default_markdown_theme(),
            Some(DefaultTextStyle {
                color: Some(Arc::new(chalk_yellow)),
                italic: Some(true),
                ..Default::default()
            }),
            None,
        );

        let lines = markdown.render(25);
        let plain_lines: Vec<String> = lines
            .iter()
            .map(|line| strip_ansi(line).trim_end().to_string())
            .collect();

        let content_lines: Vec<&String> = plain_lines.iter().filter(|line| !line.is_empty()).collect();
        for line in content_lines {
            assert!(line.starts_with("│ "), "Line should have quote border: \"{line}\"");
        }

        let all_output = lines.join("\n");
        assert!(all_output.contains("\x1b[3m"), "Should have italic");
        assert!(
            !all_output.contains("\x1b[33m"),
            "Should NOT have yellow color from default style"
        );
    }

    #[test]
    fn should_render_inline_formatting_inside_blockquotes_and_reapply_quote_styling_after() {
        let markdown = Markdown::new(
            "> Quote with **bold** and `code`",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let plain_lines: Vec<String> = lines.iter().map(|line| strip_ansi(line)).collect();

        assert!(
            plain_lines.iter().any(|line| line.starts_with("│ ")),
            "Should have quote border"
        );

        let all_plain = plain_lines.join(" ");
        assert!(all_plain.contains("Quote with"), "Should preserve 'Quote with'");
        assert!(all_plain.contains("bold"), "Should preserve 'bold'");
        assert!(all_plain.contains("code"), "Should preserve 'code'");

        let all_output = lines.join("\n");
        assert!(all_output.contains("\x1b[1m"), "Should have bold styling");
        assert!(all_output.contains("\x1b[33m"), "Should have code styling (yellow)");
        assert!(all_output.contains("\x1b[3m"), "Should have italic from quote styling");
    }
}

mod heading_with_inline_code {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn should_preserve_heading_styling_after_inline_code() {
        let markdown = Markdown::new(
            "### Why `sourceInfo` should not be optional",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let joined_output = lines.join("\n");

        assert!(joined_output.contains("\x1b[33m"), "Should have yellow for inline code");

        let after_code_index = joined_output
            .find("should not be optional")
            .expect("Should contain text after inline code");
        assert!(after_code_index > 0);

        let preceding_chunk = &joined_output[after_code_index.saturating_sub(40)..after_code_index];
        assert!(
            preceding_chunk.contains("\x1b[1m"),
            "Should re-apply bold before text after code: {preceding_chunk}"
        );
        assert!(
            preceding_chunk.contains("\x1b[36m"),
            "Should re-apply cyan before text after code: {preceding_chunk}"
        );
    }

    #[test]
    fn should_preserve_heading_styling_after_inline_code_for_h1() {
        let markdown = Markdown::new("# Title with `code` inside", 0, 0, default_markdown_theme(), None, None);

        let lines = markdown.render(80);
        let joined_output = lines.join("\n");

        let after_code_index = joined_output
            .find("inside")
            .expect("Should contain text after inline code");
        assert!(after_code_index > 0);

        let preceding_chunk = &joined_output[after_code_index.saturating_sub(40)..after_code_index];
        assert!(
            preceding_chunk.contains("\x1b[1m"),
            "Should re-apply bold for h1: {preceding_chunk}"
        );
        assert!(
            preceding_chunk.contains("\x1b[36m"),
            "Should re-apply cyan for h1: {preceding_chunk}"
        );
        assert!(
            preceding_chunk.contains("\x1b[4m"),
            "Should re-apply underline for h1: {preceding_chunk}"
        );
    }

    #[tokio::test]
    async fn should_not_leak_h1_underline_into_padding_when_inline_code_is_the_last_token() {
        let markdown = Markdown::new(
            "# Important distinction from `open()`",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );
        let terminal = Arc::new(VirtualTerminal::new(80, 4));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        tui.add_child(Arc::new(markdown.clone()));
        tui.start();
        let _stop_guard = StopTuiGuard(&tui);
        terminal.wait_for_render().await;

        let lines = markdown.render(80);
        let rendered_line = lines.first().expect("Should render heading line");
        let content_width = strip_ansi(rendered_line).trim_end().chars().count();
        assert!(content_width > 0, "Should have visible heading content");

        for col in content_width..80 {
            assert_eq!(
                get_cell(&terminal, 0, col).is_underline(),
                0,
                "Expected no underline in padding at col {col}"
            );
        }
    }

    #[test]
    fn should_preserve_heading_styling_after_bold_text() {
        let markdown = Markdown::new(
            "## Heading with **bold** and more",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let joined_output = lines.join("\n");

        let after_bold_index = joined_output.find("and more").expect("Should contain text after bold");
        assert!(after_bold_index > 0);

        let preceding_chunk = &joined_output[after_bold_index.saturating_sub(40)..after_bold_index];
        assert!(
            preceding_chunk.contains("\x1b[1m"),
            "Should re-apply bold for h2: {preceding_chunk}"
        );
        assert!(
            preceding_chunk.contains("\x1b[36m"),
            "Should re-apply cyan for h2: {preceding_chunk}"
        );
    }
}

mod strikethrough_syntax {
    use super::*;

    #[test]
    fn should_render_strikethrough_text_as_strikethrough() {
        let markdown = Markdown::new("Use ~~strikethrough~~ here", 0, 0, default_markdown_theme(), None, None);

        let lines = markdown.render(80);
        let joined_output = lines.join("\n");
        let joined_plain = lines
            .into_iter()
            .map(|line| strip_ansi(&line))
            .collect::<Vec<_>>()
            .join(" ");

        assert!(joined_output.contains("\x1b[9m"), "Should apply strikethrough styling");
        assert!(
            joined_plain.contains("strikethrough"),
            "Should include struck text content"
        );
        assert!(
            !joined_plain.contains("~~strikethrough~~"),
            "Should not render delimiters as text"
        );
    }

    #[test]
    fn should_keep_single_tilde_text_as_plain_text() {
        let markdown = Markdown::new(
            "Use ~strikethrough~ literally",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let joined_output = lines.join("\n");
        let joined_plain = lines
            .into_iter()
            .map(|line| strip_ansi(&line))
            .collect::<Vec<_>>()
            .join(" ");

        assert!(
            joined_plain.contains("~strikethrough~"),
            "Single-tilde delimiters should remain visible"
        );
        assert!(
            !joined_output.contains("\x1b[9m"),
            "Single-tilde text should not use strikethrough styling"
        );
    }
}

mod links {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    #[serial]
    fn should_not_duplicate_url_for_autolinked_emails() {
        let _caps_guard = ResetCapabilitiesGuard;
        set_capabilities(TerminalCapabilities {
            images: None,
            true_color: false,
            hyperlinks: false,
        });
        let markdown = Markdown::new(
            "Contact user@example.com for help",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let joined_plain = lines
            .into_iter()
            .map(|line| strip_ansi(&line))
            .collect::<Vec<_>>()
            .join(" ");

        assert!(joined_plain.contains("user@example.com"), "Should contain email");
        assert!(
            !joined_plain.contains("mailto:"),
            "Should not show mailto: prefix for autolinked emails"
        );
    }

    #[test]
    #[serial]
    fn should_not_duplicate_url_for_bare_urls() {
        let _caps_guard = ResetCapabilitiesGuard;
        set_capabilities(TerminalCapabilities {
            images: None,
            true_color: false,
            hyperlinks: false,
        });
        let markdown = Markdown::new(
            "Visit https://example.com for more",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let joined_plain = lines
            .into_iter()
            .map(|line| strip_ansi(&line))
            .collect::<Vec<_>>()
            .join(" ");

        let url_count = joined_plain.matches("https://example.com").count();
        assert_eq!(url_count, 1, "URL should appear exactly once");
    }

    #[test]
    #[serial]
    fn should_show_url_in_parentheses_when_hyperlinks_are_not_supported() {
        let _caps_guard = ResetCapabilitiesGuard;
        set_capabilities(TerminalCapabilities {
            images: None,
            true_color: false,
            hyperlinks: false,
        });
        let markdown = Markdown::new(
            "[click here](https://example.com)",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let joined_plain = lines
            .into_iter()
            .map(|line| strip_ansi(&line))
            .collect::<Vec<_>>()
            .join(" ");

        assert!(joined_plain.contains("click here"), "Should contain link text");
        assert!(
            joined_plain.contains("(https://example.com)"),
            "Should show URL in parentheses"
        );
    }

    #[test]
    #[serial]
    fn should_show_mailto_url_in_parentheses_when_hyperlinks_are_not_supported() {
        let _caps_guard = ResetCapabilitiesGuard;
        set_capabilities(TerminalCapabilities {
            images: None,
            true_color: false,
            hyperlinks: false,
        });
        let markdown = Markdown::new(
            "[Email me](mailto:test@example.com)",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let joined_plain = lines
            .into_iter()
            .map(|line| strip_ansi(&line))
            .collect::<Vec<_>>()
            .join(" ");

        assert!(joined_plain.contains("Email me"), "Should contain link text");
        assert!(
            joined_plain.contains("(mailto:test@example.com)"),
            "Should show mailto URL in parentheses"
        );
    }

    #[test]
    #[serial]
    fn should_emit_osc_8_hyperlink_sequence_when_terminal_supports_hyperlinks() {
        let _caps_guard = ResetCapabilitiesGuard;
        set_capabilities(TerminalCapabilities {
            images: None,
            true_color: false,
            hyperlinks: true,
        });
        let markdown = Markdown::new(
            "[click here](https://example.com)",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let joined = lines.join("");

        assert!(
            joined.contains("\x1b]8;;https://example.com\x1b\\"),
            "Should contain OSC 8 open sequence"
        );
        assert!(joined.contains("\x1b]8;;\x1b\\"), "Should contain OSC 8 close sequence");

        let plain_lines: Vec<String> = lines
            .iter()
            .map(|line| OSC8_OR_CSI_REGEX.replace_all(line, "").into_owned())
            .collect();
        assert!(plain_lines.join("").contains("click here"), "Should contain link text");

        let raw_plain: Vec<String> = lines
            .iter()
            .map(|line| {
                let without_osc = OSC8_SEQUENCE_REGEX.replace_all(line, "");
                strip_ansi(&without_osc)
            })
            .collect();
        assert!(
            !raw_plain.join("").contains("(https://example.com)"),
            "URL should not appear inline in parentheses"
        );
    }

    #[test]
    #[serial]
    fn should_use_osc_8_for_mailto_links_when_terminal_supports_hyperlinks() {
        let _caps_guard = ResetCapabilitiesGuard;
        set_capabilities(TerminalCapabilities {
            images: None,
            true_color: false,
            hyperlinks: true,
        });
        let markdown = Markdown::new(
            "[Email me](mailto:test@example.com)",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let joined = lines.join("");

        assert!(
            joined.contains("\x1b]8;;mailto:test@example.com\x1b\\"),
            "Should contain OSC 8 open with mailto URL"
        );
        assert!(joined.contains("\x1b]8;;\x1b\\"), "Should contain OSC 8 close sequence");
    }

    #[test]
    #[serial]
    fn should_use_osc_8_for_bare_urls_when_terminal_supports_hyperlinks() {
        let _caps_guard = ResetCapabilitiesGuard;
        set_capabilities(TerminalCapabilities {
            images: None,
            true_color: false,
            hyperlinks: true,
        });
        let markdown = Markdown::new(
            "Visit https://example.com for more",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let joined = lines.join("");

        assert!(
            joined.contains("\x1b]8;;https://example.com\x1b\\"),
            "Should contain OSC 8 hyperlink"
        );
        let raw_plain: Vec<String> = lines
            .iter()
            .map(|line| {
                let without_osc = OSC8_SEQUENCE_REGEX.replace_all(line, "");
                strip_ansi(&without_osc)
            })
            .collect();
        assert!(
            !raw_plain.join("").contains("(https://example.com)"),
            "URL should not appear twice"
        );
    }
}

mod html_like_tags_in_text {
    use super::*;

    #[test]
    fn should_render_content_with_html_like_tags_as_text() {
        let markdown = Markdown::new(
            "This is text with <thinking>hidden content</thinking> that should be visible",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let joined_plain = lines
            .into_iter()
            .map(|line| strip_ansi(&line))
            .collect::<Vec<_>>()
            .join(" ");

        assert!(
            joined_plain.contains("hidden content") || joined_plain.contains("<thinking>"),
            "Should render HTML-like tags or their content as text, not hide them"
        );
    }

    #[test]
    fn should_render_html_tags_in_code_blocks_correctly() {
        let markdown = Markdown::new(
            "```html\n<div>Some HTML</div>\n```",
            0,
            0,
            default_markdown_theme(),
            None,
            None,
        );

        let lines = markdown.render(80);
        let joined_plain = lines
            .into_iter()
            .map(|line| strip_ansi(&line))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            joined_plain.contains("<div>") && joined_plain.contains("</div>"),
            "Should render HTML in code blocks"
        );
    }
}

mod streaming_code_fences {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn stabilizes_partial_closing_fence_rendering() {
        let cases: [(&str, &[&str]); 6] = [
            ("```ts\nconst x = 1;\n``", &["```ts", "  const x = 1;", "```"]),
            (
                "```md\nnot a closing fence:\n``\n```",
                &["```md", "  not a closing fence:", "  ``", "```"],
            ),
            ("```ts\n``", &["```ts", "", "```"]),
            ("````\n```", &["```", "", "```"]),
            ("~~~~~\n~~~~", &["```", "", "```"]),
            (
                "```md\nnot a closing fence:\n``\n```\n\nafter",
                &["```md", "  not a closing fence:", "  ``", "```", "", "after"],
            ),
        ];

        for (input, expected) in cases {
            let markdown = Markdown::new(input, 0, 0, default_markdown_theme(), None, None);
            let lines: Vec<String> = markdown
                .render(80)
                .into_iter()
                .map(|line| strip_ansi(&line).trim_end().to_string())
                .collect();

            assert_eq!(lines, expected);
        }

        let partial = Markdown::new("```ts\nconst x = 1;\n``", 0, 0, default_markdown_theme(), None, None);
        let complete = Markdown::new("```ts\nconst x = 1;\n```", 0, 0, default_markdown_theme(), None, None);

        assert_eq!(partial.render(80).len(), complete.render(80).len());
    }
}
