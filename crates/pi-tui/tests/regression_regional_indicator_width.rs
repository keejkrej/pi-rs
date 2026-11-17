//! Port of packages/tui/test/regression-regional-indicator-width.test.ts

use pi_tui::{visible_width, wrap_text_with_ansi};
use pretty_assertions::assert_eq;

#[test]
fn treats_partial_flag_grapheme_as_full_width_to_avoid_streaming_render_drift() {
    // Repro context:
    // During streaming, "🇨🇳" often appears as an intermediate "🇨" first.
    // If "🇨" is measured as width 1 while terminal renders it as width 2,
    // differential rendering can drift and leave stale characters on screen.
    let partial_flag = "🇨";
    let list_line = "      - 🇨";

    assert_eq!(visible_width(partial_flag), 2);
    assert_eq!(visible_width(list_line), 10);
}

#[test]
fn wraps_intermediate_partial_flag_list_line_before_overflow() {
    // Width 9 cannot fit "      - 🇨" if 🇨 is width 2 (8 + 2 = 10).
    // This must wrap to avoid terminal auto-wrap mismatch.
    let wrapped = wrap_text_with_ansi("      - 🇨", 9);

    assert_eq!(wrapped.len(), 2);
    assert_eq!(visible_width(&wrapped[0]), 7);
    assert_eq!(visible_width(&wrapped[1]), 2);
}

#[test]
fn treats_all_regional_indicator_singleton_graphemes_as_width_2() {
    for cp in 0x1f1e6_u32..=0x1f1ff {
        let ch = char::from_u32(cp).unwrap();
        let regional_indicator = ch.to_string();
        assert_eq!(
            visible_width(&regional_indicator),
            2,
            "Expected {regional_indicator} (U+{cp:X}) to be width 2"
        );
    }
}

#[test]
fn keeps_full_flag_pairs_at_width_2() {
    let samples = ["🇯🇵", "🇺🇸", "🇬🇧", "🇨🇳", "🇩🇪", "🇫🇷"];
    for flag in samples {
        assert_eq!(visible_width(flag), 2, "Expected {flag} to be width 2");
    }
}

#[test]
fn keeps_common_streaming_emoji_intermediates_at_stable_width() {
    let samples = ["👍", "👍🏻", "✅", "⚡", "⚡️", "👨", "👨‍💻", "🏳️‍🌈"];
    for sample in samples {
        assert_eq!(visible_width(sample), 2, "Expected {sample} to be width 2");
    }
}
