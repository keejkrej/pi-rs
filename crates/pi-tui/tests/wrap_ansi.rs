//! Port of packages/tui/test/wrap-ansi.test.ts

use pi_tui::{visible_width, wrap_text_with_ansi};
use regex::Regex;

mod underline_styling {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn should_not_apply_underline_style_before_the_styled_text() {
        let underline_on = "\x1b[4m";
        let underline_off = "\x1b[24m";
        let url = "https://example.com/very/long/path/that/will/wrap";
        let text = format!("read this thread {underline_on}{url}{underline_off}");

        let wrapped = wrap_text_with_ansi(&text, 40);

        // First line should NOT contain underline code - it's just "read this thread"
        assert_eq!(wrapped[0], "read this thread");

        // Second line should start with underline, have URL content
        assert_eq!(wrapped[1].starts_with(underline_on), true);
        assert!(wrapped[1].contains("https://"));
    }

    #[test]
    fn should_not_have_whitespace_before_underline_reset_code() {
        let underline_on = "\x1b[4m";
        let underline_off = "\x1b[24m";
        let text_with_underlined_trailing_space = format!("{underline_on}underlined text here {underline_off}more");

        let wrapped = wrap_text_with_ansi(&text_with_underlined_trailing_space, 18);

        assert!(!wrapped[0].contains(&format!(" {underline_off}")));
    }

    #[test]
    fn should_not_bleed_underline_to_padding_each_line_should_end_with_reset_for_underline_only() {
        let underline_on = "\x1b[4m";
        let underline_off = "\x1b[24m";
        let url = "https://example.com/very/long/path/that/will/definitely/wrap";
        let text = format!("prefix {underline_on}{url}{underline_off} suffix");

        let wrapped = wrap_text_with_ansi(&text, 30);

        // Middle lines (with underlined content) should end with underline-off, not full reset
        for line in &wrapped[1..wrapped.len() - 1] {
            if line.contains(underline_on) {
                assert_eq!(line.ends_with(underline_off), true);
                assert_eq!(line.ends_with("\x1b[0m"), false);
            }
        }
    }
}

// #10925
mod styled_words_at_the_wrap_boundary {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn keeps_the_closing_code_on_the_wrapped_line_and_drops_the_following_space() {
        assert_eq!(
            wrap_text_with_ansi("aaaa \x1b[1mbold\x1b[22m next word", 9),
            vec!["aaaa \x1b[1mbold\x1b[22m", "next word"]
        );
    }

    #[test]
    fn does_not_re_open_a_style_closed_by_the_dropped_space() {
        assert_eq!(
            wrap_text_with_ansi("aaaa \x1b[31mred\x1b[39m  next", 8),
            vec!["aaaa \x1b[31mred\x1b[39m", "next"]
        );
    }

    #[test]
    fn does_not_exceed_width_for_styled_whitespace_wider_than_the_line() {
        for text in [
            format!("\x1b[41m{}\x1b[0m", " ".repeat(20)),
            format!("ab\x1b[41m{}\x1b[49mcd", " ".repeat(20)),
            format!("\x1b[33m{}x\x1b[39m", " ".repeat(20)),
        ] {
            for line in wrap_text_with_ansi(&text, 10) {
                assert!(visible_width(&line) <= 10, "{line:?}");
            }
        }
    }

    #[test]
    fn trims_trailing_spaces_before_the_line_end_reset_when_a_long_word_follows() {
        assert_eq!(
            wrap_text_with_ansi("aaa\x1b[9m    abcdefghij\x1b[29m", 9),
            vec!["aaa\x1b[9m\x1b[29m", "\x1b[9mabcdefghi\x1b[29m", "\x1b[9mj\x1b[29m"]
        );
    }

    #[test]
    fn resets_strikethrough_at_line_end_and_restores_it_on_the_next_line() {
        assert_eq!(
            wrap_text_with_ansi("aaaa \x1b[9mstrike me\x1b[29m next", 12),
            vec!["aaaa \x1b[9mstrike\x1b[29m", "\x1b[9mme\x1b[29m next"]
        );
    }

    #[test]
    fn preserves_background_when_resetting_strikethrough_at_line_end() {
        assert_eq!(
            wrap_text_with_ansi("\x1b[41;9mstrike me\x1b[0m", 6),
            vec!["\x1b[41;9mstrike\x1b[29m", "\x1b[9;41mme\x1b[0m"]
        );
    }
}

mod background_color_preservation {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn should_preserve_background_color_across_wrapped_lines_without_full_reset() {
        let bg_blue = "\x1b[44m";
        let reset = "\x1b[0m";
        let text = format!("{bg_blue}hello world this is blue background text{reset}");

        let wrapped = wrap_text_with_ansi(&text, 15);

        // Each line should have background color
        for line in &wrapped {
            assert!(line.contains(bg_blue));
        }

        // Middle lines should NOT end with full reset (kills background for padding)
        for line in &wrapped[..wrapped.len() - 1] {
            assert_eq!(line.ends_with("\x1b[0m"), false);
        }
    }

    #[test]
    fn should_reset_underline_but_preserve_background_when_wrapping_underlined_text_inside_background() {
        let underline_on = "\x1b[4m";
        let underline_off = "\x1b[24m";
        let reset = "\x1b[0m";

        let text = format!("\x1b[41mprefix {underline_on}UNDERLINED_CONTENT_THAT_WRAPS{underline_off} suffix{reset}");

        let wrapped = wrap_text_with_ansi(&text, 20);

        // All lines should have background color 41 (either as \x1b[41m or combined like \x1b[4;41m)
        for line in &wrapped {
            let has_bg_color = line.contains("[41m") || line.contains(";41m") || line.contains("[41;");
            assert!(has_bg_color);
        }

        // Lines with underlined content should use underline-off at end, not full reset
        for line in &wrapped[..wrapped.len() - 1] {
            if (line.contains("[4m") || line.contains("[4;") || line.contains(";4m")) && !line.contains(underline_off) {
                assert_eq!(line.ends_with(underline_off), true);
                assert_eq!(line.ends_with("\x1b[0m"), false);
            }
        }
    }
}

mod basic_wrapping {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn should_handle_lf_crlf_and_cr_line_endings() {
        assert_eq!(
            wrap_text_with_ansi("first\nsecond\r\nthird\rfourth", 80),
            vec!["first", "second", "third", "fourth"]
        );
    }

    #[test]
    fn should_preserve_ansi_state_across_crlf_and_cr_line_endings() {
        let red = "\x1b[31m";
        let reset = "\x1b[0m";

        assert_eq!(
            wrap_text_with_ansi(&format!("{red}first\r\nsecond\rthird{reset}"), 80),
            vec![
                format!("{red}first"),
                format!("{red}second"),
                format!("{red}third{reset}")
            ]
        );
    }

    #[test]
    fn should_wrap_plain_text_correctly() {
        let text = "hello world this is a test";
        let wrapped = wrap_text_with_ansi(text, 10);

        assert!(wrapped.len() > 1);
        for line in &wrapped {
            assert!(visible_width(line) <= 10);
        }
    }

    #[test]
    fn should_break_cjk_runs_at_grapheme_boundaries_after_latin_text() {
        let text = "This is an example 中文汉字测试段落内容中文汉字测试段落内容.";
        let wrapped = wrap_text_with_ansi(text, 40);

        assert_eq!(
            wrapped,
            vec!["This is an example 中文汉字测试段落内容", "中文汉字测试段落内容."]
        );
        for line in &wrapped {
            assert!(visible_width(line) <= 40);
        }
    }

    #[test]
    fn should_preserve_color_codes_when_wrapping_cjk_runs() {
        let red = "\x1b[31m";
        let reset = "\x1b[0m";
        let text = format!("{red}This is an example 中文汉字测试段落内容中文汉字测试段落内容.{reset}");
        let wrapped = wrap_text_with_ansi(&text, 40);

        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0], format!("{red}This is an example 中文汉字测试段落内容"));
        assert_eq!(wrapped[1], format!("{red}中文汉字测试段落内容.{reset}"));
        for line in &wrapped {
            assert!(visible_width(line) <= 40);
        }
    }

    #[test]
    fn should_ignore_osc_133_semantic_markers_in_visible_width() {
        let text = "\x1b]133;A\x07hello\x1b]133;B\x07";
        assert_eq!(visible_width(text), 5);
    }

    #[test]
    fn should_ignore_osc_sequences_terminated_with_st_in_visible_width() {
        let text = "\x1b]133;A\x1b\\hello\x1b]133;B\x1b\\";
        assert_eq!(visible_width(text), 5);
    }

    #[test]
    fn should_treat_isolated_regional_indicators_as_width_2() {
        assert_eq!(visible_width("🇨"), 2);
        assert_eq!(visible_width("🇨🇳"), 2);
    }

    #[test]
    fn should_truncate_trailing_whitespace_that_exceeds_width() {
        let two_spaces_wrapped_to_width1 = wrap_text_with_ansi("  ", 1);
        assert!(visible_width(&two_spaces_wrapped_to_width1[0]) <= 1);
    }

    #[test]
    fn should_preserve_color_codes_across_wraps() {
        let red = "\x1b[31m";
        let reset = "\x1b[0m";
        let text = format!("{red}hello world this is red{reset}");

        let wrapped = wrap_text_with_ansi(&text, 10);

        for line in &wrapped[1..] {
            assert_eq!(line.starts_with(red), true);
        }

        for line in &wrapped[..wrapped.len() - 1] {
            assert_eq!(line.ends_with("\x1b[0m"), false);
        }
    }
}

mod wrap_text_with_ansi_with_osc_8_hyperlinks {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn re_emits_osc_8_open_at_the_start_of_continuation_lines() {
        let url = "https://example.com";
        let input = format!("\x1b]8;;{url}\x1b\\0123456789\x1b]8;;\x1b\\");
        let lines = wrap_text_with_ansi(&input, 6);
        let osc8_re = Regex::new(r"\x1b\]8;;[^\x1b\x07]*\x1b\\").unwrap();
        let sgr_re = Regex::new(r"\x1b\[[0-9;]*m").unwrap();

        for line in &lines {
            let s1 = osc8_re.replace_all(line, "");
            let stripped = sgr_re.replace_all(&s1, "");
            if !stripped.trim().is_empty() {
                assert!(
                    line.starts_with(&format!("\x1b]8;;{url}\x1b\\")) || line.contains(&format!("\x1b]8;;{url}\x1b\\")),
                    "Line \"{line}\" has visible text but no OSC 8 re-open"
                );
            }
        }
    }

    #[test]
    fn closes_osc_8_before_each_line_break() {
        let url = "https://example.com";
        let input = format!("\x1b]8;;{url}\x1b\\0123456789\x1b]8;;\x1b\\");
        let lines = wrap_text_with_ansi(&input, 6);

        for line in &lines[..lines.len() - 1] {
            if line.contains(&format!("\x1b]8;;{url}\x1b\\")) {
                assert!(
                    line.ends_with("\x1b]8;;\x1b\\"),
                    "Non-final line \"{line}\" is inside a hyperlink but does not close it"
                );
            }
        }
    }

    #[test]
    fn preserves_bel_terminators_when_wrapping_oauth_style_hyperlinks() {
        let url = format!("https://example.com/oauth/{}", "a".repeat(32));
        let input = format!("\x1b]8;;{url}\x07{url}\x1b]8;;\x07");
        let lines = wrap_text_with_ansi(&input, 20);

        assert!(lines.len() > 1);
        for line in &lines {
            assert!(
                line.contains(&format!("\x1b]8;;{url}\x07")),
                "Line \"{line}\" does not reopen the hyperlink with BEL"
            );
            assert!(
                !line.contains(&format!("\x1b]8;;{url}\x1b\\")),
                "Line \"{line}\" reopens the hyperlink with ST"
            );
        }
        for line in &lines[..lines.len() - 1] {
            assert!(
                line.ends_with("\x1b]8;;\x07"),
                "Line \"{line}\" does not close the hyperlink with BEL"
            );
        }
    }

    #[test]
    fn does_not_emit_osc_8_sequences_on_lines_that_are_outside_the_hyperlink() {
        let url = "https://example.com";
        let input = format!("before \x1b]8;;{url}\x1b\\link\x1b]8;;\x1b\\ after");
        let lines = wrap_text_with_ansi(&input, 80);

        assert_eq!(lines.len(), 1);
        let open_re = Regex::new(r"\x1b\]8;;https:[^\x1b]+\x1b\\").unwrap();
        let close_re = Regex::new(r"\x1b\]8;;\x1b\\").unwrap();
        let open_count = open_re.find_iter(&lines[0]).count();
        let close_count = close_re.find_iter(&lines[0]).count();
        assert_eq!(open_count, 1);
        assert_eq!(close_count, 1);
    }
}
