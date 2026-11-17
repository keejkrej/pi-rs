//! Port of packages/tui/test/visible-width.test.ts

use pi_tui::visible_width;
use pretty_assertions::assert_eq;

#[test]
fn measures_styled_ascii_without_counting_escape_sequences() {
    assert_eq!(visible_width("\x1b[38;5;4mhello\x1b[39m world"), 11);
    assert_eq!(visible_width("\x1b]8;;https://example.com\x07link\x1b]8;;\x07"), 4);
    assert_eq!(visible_width("\x1b]133;A\x1b\\prompt"), 6);
    assert_eq!(visible_width("\x1b_pi:c\x07cursor"), 6);
}

#[test]
fn counts_tabs_as_three_columns_in_styled_text() {
    assert_eq!(visible_width("\x1b[1ma\tb\x1b[22m"), 5);
}

#[test]
fn measures_styled_non_ascii_text() {
    assert_eq!(visible_width("\x1b[31m日本\x1b[39m ok"), 7);
    assert_eq!(visible_width("\x1b[31m─→\x1b[39m"), 2);
}

#[test]
fn treats_unterminated_escape_sequences_as_zero_width_control_characters() {
    assert_eq!(visible_width("\x1b[31"), 3);
    assert_eq!(visible_width("a\x1b"), 1);
}
