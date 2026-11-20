//! Port of packages/coding-agent/test/truncate-surrogates.test.ts

use pi_coding_agent::core::tools::truncate::{slice_head, slice_tail, truncate_line};
use pretty_assertions::assert_eq;

const EMOJI: &str = "\u{1F600}";

#[test]
fn truncate_line_does_not_cut_an_emoji_in_half_at_the_limit() {
    let line = format!("{}{}tail", "a".repeat(499), EMOJI);
    let res = truncate_line(&line, Some(500));
    assert!(res.was_truncated);
    assert_eq!(res.text, format!("{}... [truncated]", "a".repeat(499)));
}

#[test]
fn truncate_line_keeps_an_emoji_that_ends_exactly_at_the_limit() {
    let line = format!("{}{}tail", "a".repeat(498), EMOJI);
    assert_eq!(
        truncate_line(&line, Some(500)).text,
        format!("{}{}... [truncated]", "a".repeat(498), EMOJI)
    );
}

#[test]
fn slice_head_and_slice_tail_never_split_a_pair() {
    let text = format!("{EMOJI}x{EMOJI}");
    let total_units = pi_js::str16::len(&text);
    for units in 0..=total_units + 1 {
        let head = slice_head(&text, units);
        let tail = slice_tail(&text, units);
        assert!(text.starts_with(head));
        assert!(text.ends_with(tail));
        assert!(pi_js::str16::len(head) >= units.min(total_units).saturating_sub(1));
        assert!(pi_js::str16::len(tail) >= units.min(total_units).saturating_sub(1));
    }
}

#[test]
fn slice_head_and_slice_tail_match_slice_on_text_without_surrogates() {
    assert_eq!(slice_head("abcdef", 3), "abc");
    assert_eq!(slice_tail("abcdef", 3), "def");
    assert_eq!(slice_head("abc", 10), "abc");
    assert_eq!(slice_tail("abc", 10), "abc");
    assert_eq!(slice_head("abc", 0), "");
    assert_eq!(slice_tail("abc", 0), "");
}
