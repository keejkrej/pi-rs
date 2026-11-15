//! Port of packages/tui/test/regression-overlay-cjk-boundary.test.ts

use pi_tui::tui::composite_tui_line;
use pi_tui::utils::{extract_segments, slice_by_column, visible_width};

#[test]
fn extract_segments_truncates_wide_cjk_char_overlapping_before_end() {
    let line = "abcd让EFGH";
    let segs = extract_segments(line, 5, 9, 11, Some(true));

    assert_eq!(segs.before, "abcd");
    assert_eq!(segs.before_width, 4);
    assert_eq!(visible_width(&segs.before), segs.before_width);
    assert_eq!(segs.after, "H");
    assert_eq!(segs.after_width, 1);

    let ascii = "abcdG EFGH";
    let ascii_segs = extract_segments(ascii, 5, 9, 11, Some(true));
    assert_eq!(ascii_segs.before, "abcdG");
    assert_eq!(ascii_segs.before_width, 5);
    assert_eq!(visible_width(&ascii_segs.before), ascii_segs.before_width);
}

#[test]
fn composite_line_at_does_not_leak_trailing_half_of_overlaid_wide_cjk_char() {
    let base = "abcd让EFGH";
    let overlay = "│XX│";

    let out = composite_tui_line(base, overlay, 5, 4, 20);
    let prefix = slice_by_column(&out, 0, 5, Some(true));
    let overlaid = slice_by_column(&out, 5, 4, Some(true));

    assert!(!out.contains('让'));
    assert_eq!(visible_width(&out), 20);
    assert_eq!(visible_width(&prefix), 5);
    assert_eq!(visible_width(&overlaid), 4);
    assert!(overlaid.contains(overlay));

    let out2 = composite_tui_line(base, overlay, 4, 4, 20);
    let overlaid2 = slice_by_column(&out2, 4, 4, Some(true));

    assert!(!out2.contains('让'));
    assert_eq!(visible_width(&out2), 20);
    assert_eq!(visible_width(&overlaid2), 4);
    assert!(overlaid2.contains(overlay));
}
