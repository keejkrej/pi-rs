//! Port of packages/durable/test/env-truncate.test.ts

use pi_durable::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, TruncationOptions, format_size,
    truncate_head,
};
use pretty_assertions::assert_eq;

#[test]
fn formats_sizes_in_bytes_kb_and_mb() {
    assert_eq!(format_size(500), "500B");
    assert_eq!(format_size(1024), "1.0KB");
    assert_eq!(format_size(1536), "1.5KB");
    assert_eq!(format_size(1024 * 1024), "1.0MB");
    assert_eq!(format_size(2 * 1024 * 1024 + 512 * 1024), "2.5MB");
}

#[test]
fn does_not_truncate_within_limits() {
    let content = "line1\nline2\nline3";
    let result = truncate_head(content, None);
    assert_eq!(result.content, content);
    assert!(!result.truncated);
    assert_eq!(result.truncated_by, None);
    assert_eq!(result.total_lines, 3);
    assert_eq!(result.output_lines, 3);
}

#[test]
fn truncates_by_line_count_and_preserves_multibyte_boundaries_within_byte_limit() {
    let line_limited = truncate_head(
        "one\ntwo\nthree",
        Some(TruncationOptions {
            max_lines: Some(2),
            max_bytes: Some(1024),
        }),
    );
    assert!(line_limited.truncated);
    assert_eq!(line_limited.truncated_by, Some(TruncatedBy::Lines));
    assert_eq!(line_limited.content, "one\ntwo");
    assert_eq!(line_limited.output_lines, 2);

    let first_line_too_big = truncate_head(
        "abcdef\nsecond",
        Some(TruncationOptions {
            max_lines: Some(10),
            max_bytes: Some(4),
        }),
    );
    assert!(first_line_too_big.truncated);
    assert_eq!(first_line_too_big.truncated_by, Some(TruncatedBy::Bytes));
    assert_eq!(first_line_too_big.content, "");
    assert!(first_line_too_big.first_line_exceeds_limit);

    let byte_limited = truncate_head(
        "abc\n😀😀",
        Some(TruncationOptions {
            max_lines: Some(DEFAULT_MAX_LINES),
            max_bytes: Some(5),
        }),
    );
    assert!(byte_limited.truncated);
    assert_eq!(byte_limited.truncated_by, Some(TruncatedBy::Bytes));
    assert_eq!(byte_limited.content, "abc");
    assert_eq!(byte_limited.output_lines, 1);
    assert_eq!(DEFAULT_MAX_BYTES, 50 * 1024);
}
