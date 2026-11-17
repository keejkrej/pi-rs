//! Port of packages/durable/test/harness-output.test.ts

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_durable::harness::output::{OutputBuffer, OutputLimits, Progress, bound_output, sanitize_output};
use pi_durable::harness::types::OutputRetainMode;
use pretty_assertions::assert_eq;
use tokio::sync::Notify;

fn head(max_lines: usize, max_bytes: Option<usize>) -> OutputLimits {
    OutputLimits {
        max_bytes: max_bytes.unwrap_or(1000),
        max_lines,
        retain: OutputRetainMode::Head,
    }
}

fn tail(max_lines: usize, max_bytes: Option<usize>) -> OutputLimits {
    OutputLimits {
        max_bytes: max_bytes.unwrap_or(1000),
        max_lines,
        retain: OutputRetainMode::Tail,
    }
}

fn buffer_tail(content: &str, max_bytes: usize) -> String {
    let bytes = content.as_bytes();
    if bytes.len() <= max_bytes {
        return content.to_owned();
    }
    let mut start = bytes.len() - max_bytes;
    while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// A single line longer than the byte limit is cut like Buffer's tail slice on a character boundary.
fn assert_matches_buffer_tail(input: &str, max_byte_values: Option<&[usize]>) {
    let total_bytes = input.len();
    let owned_values;
    let values = match max_byte_values {
        Some(v) => v,
        None => {
            owned_values = (0..total_bytes + 5).collect::<Vec<_>>();
            &owned_values
        }
    };
    for &max_bytes in values {
        let kept = bound_output(
            input,
            OutputLimits {
                max_bytes,
                max_lines: 10,
                retain: OutputRetainMode::Tail,
            },
        )
        .text;
        let expected = buffer_tail(input, max_bytes);
        assert_eq!(
            kept, expected,
            "tail mismatch input={input:?} maxBytes={max_bytes} expected={expected:?} actual={kept:?}"
        );
        assert!(
            kept.len() <= max_bytes,
            "tail output exceeded {max_bytes} bytes: {}",
            kept.len()
        );
    }
}

fn sampled_byte_limits(input: &str) -> Vec<usize> {
    let total = input.len();
    let mut set = BTreeSet::new();
    for candidate in [
        Some(0usize),
        Some(1),
        Some(2),
        Some(3),
        Some(4),
        Some(5),
        Some(8),
        Some(total / 2),
        total.checked_sub(4),
        total.checked_sub(1),
        Some(total),
        Some(total + 1),
    ]
    .into_iter()
    .flatten()
    {
        set.insert(candidate);
    }
    set.into_iter().collect()
}

fn bound(text: &str, limits: OutputLimits) -> (String, usize, usize) {
    let slice = bound_output(text, limits);
    (slice.text, slice.dropped_bytes, slice.dropped_lines)
}

struct ClearSystemTime;

impl Drop for ClearSystemTime {
    fn drop(&mut self) {
        pi_js::time::testing::clear_system_time();
    }
}

#[test]
fn removes_control_characters_but_keeps_tabs_newlines_and_other_text() {
    assert_eq!(
        sanitize_output("a\0b\tc\nd\re\u{0007}f\u{fff9}g\u{fffb}h😀"),
        "ab\tc\ndefgh😀"
    );
}

#[test]
fn keeps_output_within_the_limits_unchanged() {
    assert_eq!(bound("a\nb\n", head(2, None)), ("a\nb\n".to_owned(), 0, 0));
    assert_eq!(bound("a\nb", tail(2, None)), ("a\nb".to_owned(), 0, 0));
    assert_eq!(bound("", tail(2, None)), (String::new(), 0, 0));
}

#[test]
fn keeps_nothing_with_a_zero_limit() {
    assert_eq!(bound("ab\ncd\n", head(10, Some(0))), (String::new(), 6, 2));
    assert_eq!(bound("ab\ncd\n", tail(0, None)), (String::new(), 6, 2));
}

#[test]
fn keeps_exact_slices_of_whole_lines_trailing_newline_included() {
    assert_eq!(bound("a\nb\nc\n", head(2, None)), ("a\nb\n".to_owned(), 2, 1));
    assert_eq!(bound("a\nb\nc\n", tail(2, None)), ("b\nc\n".to_owned(), 2, 1));
    assert_eq!(bound("a\nb\nc", tail(2, None)), ("b\nc".to_owned(), 2, 1));
    // Blank lines are lines.
    assert_eq!(bound("a\nb\nc\n\n", tail(3, None)), ("b\nc\n\n".to_owned(), 2, 1));
}

#[test]
fn cuts_at_the_byte_limit_on_whole_lines_when_possible() {
    assert_eq!(bound("aa\nbb\ncc\n", head(10, Some(7))), ("aa\nbb\n".to_owned(), 3, 1));
    assert_eq!(bound("aa\nbb\ncc\n", tail(10, Some(7))), ("bb\ncc\n".to_owned(), 3, 1));
}

#[test]
fn cuts_a_single_line_longer_than_the_byte_limit_on_a_character_boundary() {
    // "é" is two bytes; five bytes hold two whole characters.
    assert_eq!(bound("ééé\n", head(10, Some(5))), ("éé".to_owned(), 3, 0));
    assert_eq!(bound("x\néééé", tail(10, Some(5))), ("éé".to_owned(), 6, 1));
}

#[test]
fn cuts_tails_of_surrogate_edge_cases_exactly_like_buffer() {
    let inputs = [
        String::from_utf16_lossy(&[b'a' as u16, 0xd83d]),
        String::from_utf16_lossy(&[0xde42, b'b' as u16]),
        String::from_utf16_lossy(&[b'a' as u16, 0xde42, b'b' as u16]),
        String::from_utf16_lossy(&[0xd83d, 0xd83d, 0xde42]),
        String::from_utf16_lossy(&[0xd83d, 0xde42, 0xde42]),
        "👩‍💻".to_owned(),
    ];
    for input in &inputs {
        assert_matches_buffer_tail(input, None);
    }
}

#[test]
fn cuts_tails_exactly_like_buffer_across_deterministic_fuzz_cases() {
    let alphabet = [
        "a", "\u{007f}", "\u{0080}", "é", "\u{07ff}", "\u{0800}", "中", "\u{d7ff}", "\u{fffd}", "🙂", "\u{e000}",
        "\u{ffff}",
    ];
    fn check_exhaustive(prefix: &str, depth: usize, alphabet: &[&str]) {
        let sampled = sampled_byte_limits(prefix);
        assert_matches_buffer_tail(prefix, Some(&sampled));
        if depth == 0 {
            return;
        }
        for character in alphabet {
            let mut next = String::with_capacity(prefix.len() + character.len());
            next.push_str(prefix);
            next.push_str(character);
            check_exhaustive(&next, depth - 1, alphabet);
        }
    }
    check_exhaustive("", 3, &alphabet);

    let mut seed: u32 = 0x1234_5678;
    let mut random = || -> f64 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from(seed) / f64::from(u32::MAX as f64 + 1.0)
    };
    for _ in 0..1_000 {
        let mut input = String::new();
        let length = (random() * 80.0).floor() as usize;
        for _ in 0..length {
            let idx = (random() * alphabet.len() as f64).floor() as usize;
            input.push_str(alphabet[idx]);
        }
        let sampled = sampled_byte_limits(&input);
        assert_matches_buffer_tail(&input, Some(&sampled));
    }
}

#[test]
fn keeps_exact_totals_across_chunks_and_decodes_utf8_split_across_byte_chunks() {
    let mut buffer = OutputBuffer::new(tail(2, None));
    let bytes = "😀\n".as_bytes();
    buffer.push_str("a\nb\n");
    buffer.push_bytes(&bytes[..2]);
    buffer.push_bytes(&bytes[2..]);
    let snap = buffer.snapshot();
    assert_eq!(snap.text, "b\n😀\n");
    assert_eq!(snap.dropped_bytes, 2);
    assert_eq!(snap.dropped_lines, 1);
}

#[test]
fn sanitizes_the_retained_text_but_counts_the_raw_stream() {
    let mut buffer = OutputBuffer::new(tail(1, None));
    buffer.push_str("a\u{0007}\n");
    let snap1 = buffer.snapshot();
    assert_eq!(snap1.text, "a\n");
    assert_eq!(snap1.dropped_bytes, 0);
    assert_eq!(snap1.dropped_lines, 0);

    buffer.push_str("b\u{001b}\n");
    let snap2 = buffer.snapshot();
    assert_eq!(snap2.text, "b\n");
    assert_eq!(snap2.dropped_bytes, 3);
    assert_eq!(snap2.dropped_lines, 1);
}

#[test]
fn flushes_an_incomplete_character_before_a_string_chunk_and_at_the_end() {
    let mut buffer = OutputBuffer::new(tail(10, None));
    let euro = "€".as_bytes();
    buffer.push_bytes(&euro[..1]);
    buffer.push_str("x");
    buffer.push_bytes(&euro[..2]);
    buffer.end();
    assert_eq!(buffer.snapshot().text, "\u{fffd}x\u{fffd}");
}

#[test]
fn matches_bounding_the_whole_stream_when_several_chunks_arrive_between_snapshots() {
    for limits in [
        head(3, Some(40)),
        tail(3, Some(40)),
        head(50, Some(25)),
        tail(50, Some(25)),
    ] {
        let mut buffer = OutputBuffer::new(limits);
        let mut stream = String::new();
        for index in 0..300 {
            let chunk = if index % 7 == 0 {
                format!("{}\n", "é".repeat(index % 30))
            } else {
                format!("line {index}\n")
            };
            stream.push_str(&chunk);
            buffer.push_str(&chunk);
            if index % 5 != 4 {
                continue;
            }
            let expected = bound_output(&stream, limits);
            let snap = buffer.snapshot();
            assert_eq!(snap.text, expected.text);
            assert_eq!(snap.dropped_bytes, expected.dropped_bytes);
            assert_eq!(snap.dropped_lines, expected.dropped_lines);
        }
    }
}

#[test]
fn stops_storing_head_output_once_the_window_is_full() {
    let mut buffer = OutputBuffer::new(head(2, None));
    for index in 0..1000 {
        buffer.push_str(&format!("line {index}\n"));
    }
    assert!(buffer.stored_bytes() < 20);
    let snap = buffer.snapshot();
    assert_eq!(snap.text, "line 0\nline 1\n");
    assert_eq!(snap.dropped_bytes, 8876);
    assert_eq!(snap.dropped_lines, 998);
}

#[test]
fn stores_only_the_tail_window_after_each_snapshot() {
    let mut buffer = OutputBuffer::new(tail(3, Some(100)));
    let mut stream = String::new();
    for index in 0..2000 {
        let chunk = format!("line {index}\n\n");
        stream.push_str(&chunk);
        buffer.push_str(&chunk);
        let snap = buffer.snapshot();
        assert!(buffer.stored_bytes() <= 100);
        assert_eq!(snap.text, bound_output(&stream, tail(3, Some(100))).text);
    }
}

#[tokio::test(start_paused = true)]
async fn commits_the_first_change_at_once_then_waits_at_least_100ms_and_written_size_at_100kib_per_s() {
    pi_js::time::testing::set_system_time(0);
    let _guard = ClearSystemTime;

    let commits = Arc::new(Mutex::new(Vec::<i64>::new()));
    let size = Arc::new(Mutex::new(50 * 1024usize));
    let commits_cb = Arc::clone(&commits);
    let size_cb = Arc::clone(&size);
    let progress = Progress::new(
        Arc::new(move || {
            let commits_cb = Arc::clone(&commits_cb);
            let size_cb = Arc::clone(&size_cb);
            Box::pin(async move {
                commits_cb.lock().expect("lock").push(pi_js::time::now_ms());
                Ok(*size_cb.lock().expect("lock"))
            })
        }),
        Arc::new(|_| {}),
    );

    progress.mark();
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    assert_eq!(*commits.lock().expect("lock"), vec![0]);

    // 50 KiB buys 500 ms; changes meanwhile coalesce into one commit.
    *size.lock().expect("lock") = 10;
    progress.mark();
    progress.mark();
    tokio::time::advance(Duration::from_millis(499)).await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    assert_eq!(*commits.lock().expect("lock"), vec![0]);

    tokio::time::advance(Duration::from_millis(1)).await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    assert_eq!(*commits.lock().expect("lock"), vec![0, 500]);

    // A small commit still waits the minimum 100 ms.
    progress.mark();
    tokio::time::advance(Duration::from_millis(99)).await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    assert_eq!(*commits.lock().expect("lock"), vec![0, 500]);

    tokio::time::advance(Duration::from_millis(1)).await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    assert_eq!(*commits.lock().expect("lock"), vec![0, 500, 600]);
}

#[tokio::test(start_paused = true)]
async fn rejects_the_waiters_of_a_failed_commit_and_reports_its_error() {
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let errors_cb = Arc::clone(&errors);
    let progress = Progress::new(
        Arc::new(|| Box::pin(async { Err(pi_js::Error::msg("commit failed")) })),
        Arc::new(move |error| {
            errors_cb.lock().expect("lock").push(error.to_string());
        }),
    );
    let err = progress.mark_and_wait().await.expect_err("should fail");
    assert!(err.to_string().contains("commit failed"));
    let errs = errors.lock().expect("lock");
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("commit failed"));
}

#[tokio::test(start_paused = true)]
async fn stops_waits_for_the_commit_in_flight_and_hands_back_waiters_no_commit_covered_yet() {
    pi_js::time::testing::set_system_time(0);
    let _guard = ClearSystemTime;

    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let gate_cb = Arc::clone(&gate);
    let started_cb = Arc::clone(&started);
    let progress = Progress::new(
        Arc::new(move || {
            let gate_cb = Arc::clone(&gate_cb);
            let started_cb = Arc::clone(&started_cb);
            Box::pin(async move {
                started_cb.notify_waiters();
                gate_cb.notified().await;
                Ok(0)
            })
        }),
        Arc::new(|_| {}),
    );

    let first = progress.mark_and_wait();
    // Wait until the first commit is in flight before adding the second waiter and stopping.
    started.notified().await;
    let second = progress.mark_and_wait();
    let stopped = progress.stop();
    gate.notify_one();
    let pending = stopped.await;
    first.await.expect("first settled");
    assert_eq!(pending.len(), 1);
    pending.into_iter().next().expect("waiter").send(Ok(())).expect("send");
    second.await.expect("second settled");
}
