//! Port of packages/tui/test/word-navigation.test.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_tui::utils::SegmentData;
use pi_tui::word_navigation::{WordNavigationOptions, find_word_backward, find_word_forward};

mod find_word_backward_tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn basic_words_hello_world() {
        let text = "hello world";
        assert_eq!(find_word_backward(text, 11, None), 6);
        assert_eq!(find_word_backward(text, 6, None), 0);
    }

    #[test]
    fn dotted_foo_bar() {
        let text = "foo.bar";
        assert_eq!(find_word_backward(text, 7, None), 4);
        assert_eq!(find_word_backward(text, 4, None), 3);
        assert_eq!(find_word_backward(text, 3, None), 0);
    }

    #[test]
    fn colon_foo_bar() {
        let text = "foo:bar";
        assert_eq!(find_word_backward(text, 7, None), 4);
        assert_eq!(find_word_backward(text, 4, None), 3);
        assert_eq!(find_word_backward(text, 3, None), 0);
    }

    #[test]
    fn path_path_to_file() {
        let text = "path/to/file";
        assert_eq!(find_word_backward(text, 12, None), 8);
        assert_eq!(find_word_backward(text, 8, None), 7);
        // "/to" is one word-like segment with "/" as punctuation boundary
        assert_eq!(find_word_backward(text, 7, None), 5);
        assert_eq!(find_word_backward(text, 5, None), 4);
        assert_eq!(find_word_backward(text, 4, None), 0);
    }

    #[test]
    fn cjk_mixed() {
        let text = "你好世界 test";
        let text_len = pi_js::str16::len(text) as i64;
        assert_eq!(find_word_backward(text, text_len, None), 5);
        // Intl.Segmenter treats each CJK char as a separate word-like segment
        assert_eq!(find_word_backward(text, 5, None), 2);
        assert_eq!(find_word_backward(text, 2, None), 0);
    }

    #[test]
    fn whitespace_at_boundaries() {
        let text = "  hello  ";
        assert_eq!(find_word_backward(text, 9, None), 2);
        assert_eq!(find_word_backward(text, 2, None), 0);
    }

    #[test]
    fn punctuation_run_foo_bar() {
        let text = "foo...bar";
        assert_eq!(find_word_backward(text, 9, None), 6);
        assert_eq!(find_word_backward(text, 6, None), 3);
        assert_eq!(find_word_backward(text, 3, None), 0);
    }

    #[test]
    fn cursor_at_0_returns_0() {
        assert_eq!(find_word_backward("hello", 0, None), 0);
    }
}

mod find_word_forward_tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn basic_words_hello_world() {
        let text = "hello world";
        assert_eq!(find_word_forward(text, 0, None), 5);
        assert_eq!(find_word_forward(text, 5, None), 11);
    }

    #[test]
    fn dotted_foo_bar() {
        let text = "foo.bar";
        assert_eq!(find_word_forward(text, 0, None), 3);
        assert_eq!(find_word_forward(text, 3, None), 4);
        assert_eq!(find_word_forward(text, 4, None), 7);
    }

    #[test]
    fn colon_foo_bar() {
        let text = "foo:bar";
        assert_eq!(find_word_forward(text, 0, None), 3);
        assert_eq!(find_word_forward(text, 3, None), 4);
        assert_eq!(find_word_forward(text, 4, None), 7);
    }

    #[test]
    fn path_path_to_file() {
        let text = "path/to/file";
        assert_eq!(find_word_forward(text, 0, None), 4);
        assert_eq!(find_word_forward(text, 4, None), 5);
        assert_eq!(find_word_forward(text, 5, None), 7);
        assert_eq!(find_word_forward(text, 7, None), 8);
        assert_eq!(find_word_forward(text, 8, None), 12);
    }

    #[test]
    fn cjk_mixed() {
        let text = "你好世界 test";
        let text_len = pi_js::str16::len(text) as i64;
        let first_end = find_word_forward(text, 0, None);
        assert!(first_end > 0);
        assert!(first_end <= 4);
        // Walk to end
        let mut pos = 0;
        while pos < text_len {
            let next = find_word_forward(text, pos, None);
            if next == pos {
                break;
            }
            pos = next;
        }
        assert_eq!(pos, text_len);
    }

    #[test]
    fn whitespace_at_boundaries() {
        let text = "  hello  ";
        assert_eq!(find_word_forward(text, 0, None), 7);
        assert_eq!(find_word_forward(text, 7, None), 9);
    }

    #[test]
    fn punctuation_run_foo_bar() {
        let text = "foo...bar";
        assert_eq!(find_word_forward(text, 0, None), 3);
        assert_eq!(find_word_forward(text, 3, None), 6);
        assert_eq!(find_word_forward(text, 6, None), 9);
    }

    #[test]
    fn cursor_at_end_returns_end() {
        assert_eq!(find_word_forward("hello", 5, None), 5);
    }
}

mod atomic_segments_tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn make_options() -> (String, String, WordNavigationOptions) {
        let marker = "[paste #1 +5 lines]".to_string();
        let text = format!("hello {marker} world");
        let text_len = pi_js::str16::len(&text) as i64;
        let mut segment_map: IndexMap<String, Vec<SegmentData>> = IndexMap::new();

        // backward from end: slice(0, 31) = full text
        segment_map.insert(
            pi_js::str16::slice(&text, 0, Some(text_len)).to_string(),
            vec![
                SegmentData {
                    segment: "hello".to_string(),
                    index: 0,
                    input: text.clone(),
                    is_word_like: Some(true),
                },
                SegmentData {
                    segment: " ".to_string(),
                    index: 5,
                    input: text.clone(),
                    is_word_like: Some(false),
                },
                SegmentData {
                    segment: marker.clone(),
                    index: 6,
                    input: text.clone(),
                    is_word_like: Some(true),
                },
                SegmentData {
                    segment: " ".to_string(),
                    index: 25,
                    input: text.clone(),
                    is_word_like: Some(false),
                },
                SegmentData {
                    segment: "world".to_string(),
                    index: 26,
                    input: text.clone(),
                    is_word_like: Some(true),
                },
            ],
        );

        // backward from 26: slice(0, 26) = "hello [paste #1 +5 lines] "
        segment_map.insert(
            pi_js::str16::slice(&text, 0, Some(26)).to_string(),
            vec![
                SegmentData {
                    segment: "hello".to_string(),
                    index: 0,
                    input: text.clone(),
                    is_word_like: Some(true),
                },
                SegmentData {
                    segment: " ".to_string(),
                    index: 5,
                    input: text.clone(),
                    is_word_like: Some(false),
                },
                SegmentData {
                    segment: marker.clone(),
                    index: 6,
                    input: text.clone(),
                    is_word_like: Some(true),
                },
                SegmentData {
                    segment: " ".to_string(),
                    index: 25,
                    input: text.clone(),
                    is_word_like: Some(false),
                },
            ],
        );

        // forward from 6: slice(6) = "[paste #1 +5 lines] world"
        segment_map.insert(
            pi_js::str16::slice(&text, 6, None).to_string(),
            vec![
                SegmentData {
                    segment: marker.clone(),
                    index: 0,
                    input: text.clone(),
                    is_word_like: Some(true),
                },
                SegmentData {
                    segment: " ".to_string(),
                    index: 19,
                    input: text.clone(),
                    is_word_like: Some(false),
                },
                SegmentData {
                    segment: "world".to_string(),
                    index: 20,
                    input: text.clone(),
                    is_word_like: Some(true),
                },
            ],
        );

        let marker_cmp = marker.clone();
        let opts = WordNavigationOptions {
            segment: Some(Arc::new(move |input: &str| {
                segment_map.get(input).cloned().unwrap_or_default()
            })),
            is_atomic_segment: Some(Arc::new(move |s: &str| s == marker_cmp)),
        };

        (marker, text, opts)
    }

    #[test]
    fn backward_skips_word_then_stops_before_atomic_marker() {
        let (_marker, text, opts) = make_options();
        let text_len = pi_js::str16::len(&text) as i64;
        assert_eq!(find_word_backward(&text, text_len, Some(opts)), 26);
    }

    #[test]
    fn backward_skips_whitespace_then_atomic_marker_as_one_unit() {
        let (_marker, text, opts) = make_options();
        assert_eq!(find_word_backward(&text, 26, Some(opts)), 6);
    }

    #[test]
    fn forward_skips_atomic_marker_as_one_unit() {
        let (marker, text, opts) = make_options();
        let marker_len = pi_js::str16::len(&marker) as i64;
        assert_eq!(find_word_forward(&text, 6, Some(opts)), 6 + marker_len);
    }
}
