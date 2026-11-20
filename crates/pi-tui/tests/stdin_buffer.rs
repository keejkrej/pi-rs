//! Port of packages/tui/test/stdin-buffer.test.ts

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_tui::keys::matches_key;
use pi_tui::stdin_buffer::{StdinBuffer, StdinBufferData, StdinBufferOptions};

fn setup_buffer(
    options: Option<StdinBufferOptions>,
) -> (StdinBuffer, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>) {
    let buffer = StdinBuffer::new(options.or(Some(StdinBufferOptions {
        timeout: Some(10),
        escape_timeout: None,
    })));
    let emitted_sequences = Arc::new(Mutex::new(Vec::new()));
    let seq_clone = Arc::clone(&emitted_sequences);
    let _ = buffer.on_data(Arc::new(move |seq| {
        seq_clone.lock().unwrap().push(seq.to_string());
    }));

    let emitted_paste = Arc::new(Mutex::new(Vec::new()));
    let paste_clone = Arc::clone(&emitted_paste);
    let _ = buffer.on_paste(Arc::new(move |data| {
        paste_clone.lock().unwrap().push(data.to_string());
    }));

    (buffer, emitted_sequences, emitted_paste)
}

async fn wait_ms(ms: u64) {
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(ms)).await;
    tokio::task::yield_now().await;
}

// ============================================================================
// Regular Characters
// ============================================================================

#[test]
fn passes_through_regular_characters_immediately() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("a"));
    assert_eq!(*emitted.lock().unwrap(), vec!["a"]);
}

#[test]
fn passes_through_multiple_regular_characters() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("abc"));
    assert_eq!(*emitted.lock().unwrap(), vec!["a", "b", "c"]);
}

#[test]
fn handles_unicode_characters() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("hello 世界"));
    assert_eq!(*emitted.lock().unwrap(), vec!["h", "e", "l", "l", "o", " ", "世", "界"]);
}

// ============================================================================
// Complete Escape Sequences
// ============================================================================

#[test]
fn passes_through_complete_mouse_sgr_sequences() {
    let (buffer, emitted, _) = setup_buffer(None);
    let mouse_seq = "\x1b[<35;20;5m";
    buffer.process(StdinBufferData::Text(mouse_seq));
    assert_eq!(*emitted.lock().unwrap(), vec![mouse_seq]);
}

#[test]
fn passes_through_complete_arrow_key_sequences() {
    let (buffer, emitted, _) = setup_buffer(None);
    let up_arrow = "\x1b[A";
    buffer.process(StdinBufferData::Text(up_arrow));
    assert_eq!(*emitted.lock().unwrap(), vec![up_arrow]);
}

#[test]
fn passes_through_complete_function_key_sequences() {
    let (buffer, emitted, _) = setup_buffer(None);
    let f1 = "\x1b[11~";
    buffer.process(StdinBufferData::Text(f1));
    assert_eq!(*emitted.lock().unwrap(), vec![f1]);
}

#[test]
fn passes_through_meta_key_sequences() {
    let (buffer, emitted, _) = setup_buffer(None);
    let meta_a = "\x1ba";
    buffer.process(StdinBufferData::Text(meta_a));
    assert_eq!(*emitted.lock().unwrap(), vec![meta_a]);
}

#[test]
fn passes_through_ss3_sequences() {
    let (buffer, emitted, _) = setup_buffer(None);
    let ss3 = "\x1bOA";
    buffer.process(StdinBufferData::Text(ss3));
    assert_eq!(*emitted.lock().unwrap(), vec![ss3]);
}

// ============================================================================
// Partial Escape Sequences
// ============================================================================

#[tokio::test(start_paused = true)]
async fn buffers_incomplete_mouse_sgr_sequence() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b"));
    assert!(emitted.lock().unwrap().is_empty());
    assert_eq!(buffer.get_buffer(), "\x1b");

    buffer.process(StdinBufferData::Text("[<35"));
    assert!(emitted.lock().unwrap().is_empty());
    assert_eq!(buffer.get_buffer(), "\x1b[<35");

    buffer.process(StdinBufferData::Text(";20;5m"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[<35;20;5m"]);
    assert_eq!(buffer.get_buffer(), "");
    buffer.destroy();
}

#[tokio::test(start_paused = true)]
async fn buffers_incomplete_csi_sequence() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b["));
    assert!(emitted.lock().unwrap().is_empty());

    buffer.process(StdinBufferData::Text("1;"));
    assert!(emitted.lock().unwrap().is_empty());

    buffer.process(StdinBufferData::Text("5H"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[1;5H"]);
    buffer.destroy();
}

#[tokio::test(start_paused = true)]
async fn buffers_split_across_many_chunks() {
    let (buffer, emitted, _) = setup_buffer(None);
    for ch in ["\x1b", "[", "<", "3", "5", ";", "2", "0", ";", "5", "m"] {
        buffer.process(StdinBufferData::Text(ch));
    }
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[<35;20;5m"]);
    buffer.destroy();
}

#[tokio::test(start_paused = true)]
async fn flushes_incomplete_sequence_after_timeout() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[<35"));
    assert!(emitted.lock().unwrap().is_empty());

    wait_ms(15).await;

    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[<35"]);
    buffer.destroy();
}

#[tokio::test(start_paused = true)]
async fn flushes_a_lone_esc_as_escape_when_cr_arrives_after_the_timeout() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b"));
    wait_ms(20).await;
    buffer.process(StdinBufferData::Text("\r"));

    let seqs = emitted.lock().unwrap().clone();
    assert_eq!(seqs, vec!["\x1b", "\r"]);
    assert!(matches_key(&seqs[0], "escape"));
    buffer.destroy();
}

#[tokio::test(start_paused = true)]
async fn merges_esc_plus_cr_split_across_chunks_within_a_larger_timeout() {
    let (buffer, emitted, _) = setup_buffer(Some(StdinBufferOptions {
        timeout: None,
        escape_timeout: Some(100),
    }));

    buffer.process(StdinBufferData::Text("\x1b"));
    wait_ms(20).await;
    buffer.process(StdinBufferData::Text("\r"));

    let seqs = emitted.lock().unwrap().clone();
    assert_eq!(seqs, vec!["\x1b\r"]);
    assert!(matches_key(&seqs[0], "alt+enter"));
    buffer.destroy();
}

#[tokio::test(start_paused = true)]
async fn does_not_apply_the_sequence_timeout_to_a_lone_esc() {
    let (buffer, emitted, _) = setup_buffer(Some(StdinBufferOptions {
        timeout: Some(100),
        escape_timeout: None,
    }));

    buffer.process(StdinBufferData::Text("\x1b"));
    wait_ms(20).await;
    buffer.process(StdinBufferData::Text("\r"));

    let seqs = emitted.lock().unwrap().clone();
    assert_eq!(seqs, vec!["\x1b", "\r"]);
    assert!(matches_key(&seqs[0], "escape"));
    buffer.destroy();
}

#[tokio::test(start_paused = true)]
async fn keeps_fragmented_mouse_sequences_buffered_across_delayed_chunks_by_default() {
    let (delayed_buffer, delayed_sequences, _) = setup_buffer(Some(StdinBufferOptions::default()));

    delayed_buffer.process(StdinBufferData::Text("\x1b["));
    wait_ms(20).await;
    assert!(delayed_sequences.lock().unwrap().is_empty());

    delayed_buffer.process(StdinBufferData::Text("<65;48;39M"));
    assert_eq!(*delayed_sequences.lock().unwrap(), vec!["\x1b[<65;48;39M"]);
    delayed_buffer.destroy();
}

// ============================================================================
// Mixed Content
// ============================================================================

#[test]
fn handles_characters_followed_by_escape_sequence() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("abc\x1b[A"));
    assert_eq!(*emitted.lock().unwrap(), vec!["a", "b", "c", "\x1b[A"]);
}

#[test]
fn handles_escape_sequence_followed_by_characters() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[Aabc"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[A", "a", "b", "c"]);
}

#[test]
fn handles_multiple_complete_sequences() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[A\x1b[B\x1b[C"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[A", "\x1b[B", "\x1b[C"]);
}

#[tokio::test(start_paused = true)]
async fn handles_partial_sequence_with_preceding_characters() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("abc\x1b[<35"));
    assert_eq!(*emitted.lock().unwrap(), vec!["a", "b", "c"]);
    assert_eq!(buffer.get_buffer(), "\x1b[<35");

    buffer.process(StdinBufferData::Text(";20;5m"));
    assert_eq!(*emitted.lock().unwrap(), vec!["a", "b", "c", "\x1b[<35;20;5m"]);
    buffer.destroy();
}

// ============================================================================
// Kitty Keyboard Protocol
// ============================================================================

#[test]
fn handles_kitty_csi_u_press_events() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[97u"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[97u"]);
}

#[test]
fn handles_kitty_csi_u_release_events() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[97;1:3u"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[97;1:3u"]);
}

#[test]
fn handles_batched_kitty_press_and_release() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[97u\x1b[97;1:3u"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[97u", "\x1b[97;1:3u"]);
}

#[test]
fn handles_multiple_batched_kitty_events() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[97u\x1b[97;1:3u\x1b[98u\x1b[98;1:3u"));
    assert_eq!(
        *emitted.lock().unwrap(),
        vec!["\x1b[97u", "\x1b[97;1:3u", "\x1b[98u", "\x1b[98;1:3u"]
    );
}

#[test]
fn handles_kitty_arrow_keys_with_event_type() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[1;1:1A"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[1;1:1A"]);
}

#[test]
fn handles_kitty_functional_keys_with_event_type() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[3;1:3~"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[3;1:3~"]);
}

#[test]
fn splits_esc_esc_csi_into_standalone_esc_and_csi_sequence() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b\x1b[27;129:3u"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b", "\x1b[27;129:3u"]);
}

#[test]
fn splits_esc_esc_csi_with_no_modifier() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b\x1b[27;1:3u"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b", "\x1b[27;1:3u"]);
}

#[test]
fn emits_esc_esc_as_single_sequence_when_not_followed_by_new_escape() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b\x1b"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b\x1b"]);
}

#[test]
fn handles_plain_characters_mixed_with_kitty_sequences() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("a\x1b[97;1:3u"));
    assert_eq!(*emitted.lock().unwrap(), vec!["a", "\x1b[97;1:3u"]);
}

#[test]
fn drops_raw_duplicate_character_after_matching_kitty_printable_sequence() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[224uà"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[224u"]);
}

#[test]
fn drops_raw_duplicate_character_after_matching_kitty_printable_sequence_across_chunks() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[64u"));
    buffer.process(StdinBufferData::Text("@"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[64u"]);
}

#[test]
fn keeps_non_matching_plain_character_after_kitty_printable_sequence() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[97ub"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[97u", "b"]);
}

#[test]
fn keeps_raw_character_after_modified_kitty_printable_sequence() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[64;3u@"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[64;3u", "@"]);
}

#[test]
fn handles_rapid_typing_simulation_with_kitty_protocol() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[104u\x1b[104;1:3u\x1b[105u\x1b[105;1:3u"));
    assert_eq!(
        *emitted.lock().unwrap(),
        vec!["\x1b[104u", "\x1b[104;1:3u", "\x1b[105u", "\x1b[105;1:3u"]
    );
}

// ============================================================================
// Mouse Events
// ============================================================================

#[test]
fn handles_mouse_press_event() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[<0;10;5M"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[<0;10;5M"]);
}

#[test]
fn handles_mouse_release_event() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[<0;10;5m"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[<0;10;5m"]);
}

#[test]
fn handles_mouse_move_event() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[<35;20;5m"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[<35;20;5m"]);
}

#[tokio::test(start_paused = true)]
async fn handles_split_mouse_events() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[<3"));
    buffer.process(StdinBufferData::Text("5;1"));
    buffer.process(StdinBufferData::Text("5;"));
    buffer.process(StdinBufferData::Text("10m"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[<35;15;10m"]);
    buffer.destroy();
}

#[test]
fn handles_multiple_mouse_events() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[<35;1;1m\x1b[<35;2;2m\x1b[<35;3;3m"));
    assert_eq!(
        *emitted.lock().unwrap(),
        vec!["\x1b[<35;1;1m", "\x1b[<35;2;2m", "\x1b[<35;3;3m"]
    );
}

#[test]
fn handles_old_style_mouse_sequence() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[M abc"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[M ab", "c"]);
}

#[tokio::test(start_paused = true)]
async fn buffers_incomplete_old_style_mouse_sequence() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[M"));
    assert_eq!(buffer.get_buffer(), "\x1b[M");

    buffer.process(StdinBufferData::Text(" a"));
    assert_eq!(buffer.get_buffer(), "\x1b[M a");

    buffer.process(StdinBufferData::Text("b"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[M ab"]);
    buffer.destroy();
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn handles_empty_input() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text(""));
    assert_eq!(*emitted.lock().unwrap(), vec![""]);
}

#[tokio::test(start_paused = true)]
async fn handles_lone_escape_character_with_timeout() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b"));
    assert!(emitted.lock().unwrap().is_empty());

    wait_ms(15).await;
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b"]);
    buffer.destroy();
}

#[tokio::test(start_paused = true)]
async fn flushes_a_lone_escape_promptly_with_the_longer_default_sequence_timeout() {
    let (default_buffer, default_sequences, _) = setup_buffer(Some(StdinBufferOptions::default()));
    default_buffer.process(StdinBufferData::Text("\x1b"));
    wait_ms(20).await;
    assert_eq!(*default_sequences.lock().unwrap(), vec!["\x1b"]);
    default_buffer.destroy();
}

#[tokio::test(start_paused = true)]
async fn handles_lone_escape_character_with_explicit_flush() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b"));
    assert!(emitted.lock().unwrap().is_empty());

    let flushed = buffer.flush();
    assert_eq!(flushed, vec!["\x1b"]);
    buffer.destroy();
}

#[test]
fn handles_buffer_input() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Buffer(b"\x1b[A"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[A"]);
}

#[test]
fn handles_very_long_sequences() {
    let (buffer, emitted, _) = setup_buffer(None);
    let long_seq = format!("\x1b[{}H", "1;".repeat(50));
    buffer.process(StdinBufferData::Text(&long_seq));
    assert_eq!(*emitted.lock().unwrap(), vec![long_seq]);
}

// ============================================================================
// Flush & Clear
// ============================================================================

#[tokio::test(start_paused = true)]
async fn flush_flushes_incomplete_sequences() {
    let (buffer, _, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[<35"));
    let flushed = buffer.flush();
    assert_eq!(flushed, vec!["\x1b[<35"]);
    assert_eq!(buffer.get_buffer(), "");
    buffer.destroy();
}

#[test]
fn flush_returns_empty_array_if_nothing_to_flush() {
    let (buffer, _, _) = setup_buffer(None);
    let flushed = buffer.flush();
    assert!(flushed.is_empty());
}

#[tokio::test(start_paused = true)]
async fn flush_emits_flushed_data_via_timeout() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[<35"));
    assert!(emitted.lock().unwrap().is_empty());

    wait_ms(15).await;

    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[<35"]);
    buffer.destroy();
}

#[tokio::test(start_paused = true)]
async fn clear_clears_buffered_content_without_emitting() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[<35"));
    assert_eq!(buffer.get_buffer(), "\x1b[<35");

    buffer.clear();
    assert_eq!(buffer.get_buffer(), "");
    assert!(emitted.lock().unwrap().is_empty());
    buffer.destroy();
}

// ============================================================================
// Bracketed Paste
// ============================================================================

#[test]
fn emits_paste_event_for_complete_bracketed_paste() {
    let (buffer, emitted_sequences, emitted_paste) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[200~hello world\x1b[201~"));

    assert_eq!(*emitted_paste.lock().unwrap(), vec!["hello world"]);
    assert!(emitted_sequences.lock().unwrap().is_empty());
}

#[test]
fn handles_paste_arriving_in_chunks() {
    let (buffer, emitted_sequences, emitted_paste) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[200~"));
    assert!(emitted_paste.lock().unwrap().is_empty());

    buffer.process(StdinBufferData::Text("hello "));
    assert!(emitted_paste.lock().unwrap().is_empty());

    buffer.process(StdinBufferData::Text("world\x1b[201~"));
    assert_eq!(*emitted_paste.lock().unwrap(), vec!["hello world"]);
    assert!(emitted_sequences.lock().unwrap().is_empty());
}

#[test]
fn handles_paste_with_input_before_and_after() {
    let (buffer, emitted_sequences, emitted_paste) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("a"));
    buffer.process(StdinBufferData::Text("\x1b[200~pasted\x1b[201~"));
    buffer.process(StdinBufferData::Text("b"));

    assert_eq!(*emitted_sequences.lock().unwrap(), vec!["a", "b"]);
    assert_eq!(*emitted_paste.lock().unwrap(), vec!["pasted"]);
}

#[test]
fn handles_paste_with_newlines() {
    let (buffer, emitted_sequences, emitted_paste) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[200~line1\nline2\nline3\x1b[201~"));

    assert_eq!(*emitted_paste.lock().unwrap(), vec!["line1\nline2\nline3"]);
    assert!(emitted_sequences.lock().unwrap().is_empty());
}

#[test]
fn handles_paste_with_unicode() {
    let (buffer, emitted_sequences, emitted_paste) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[200~Hello 世界 🎉\x1b[201~"));

    assert_eq!(*emitted_paste.lock().unwrap(), vec!["Hello 世界 🎉"]);
    assert!(emitted_sequences.lock().unwrap().is_empty());
}

// ============================================================================
// Destroy
// ============================================================================

#[tokio::test(start_paused = true)]
async fn clears_buffer_on_destroy() {
    let (buffer, _, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[<35"));
    assert_eq!(buffer.get_buffer(), "\x1b[<35");

    buffer.destroy();
    assert_eq!(buffer.get_buffer(), "");
}

#[tokio::test(start_paused = true)]
async fn clears_pending_timeouts_on_destroy() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b[<35"));
    buffer.destroy();

    wait_ms(15).await;

    assert!(emitted.lock().unwrap().is_empty());
}

// ============================================================================
// Terminal Reply Strings
// ============================================================================

#[tokio::test(start_paused = true)]
async fn keeps_delayed_terminal_replies_intact_at_every_split_after_reply_header() {
    let replies = [
        ("\x1b]4;1;", "\x1b]4;1;rgb:eaea/6c6c/6d6d\x07"),
        ("\x1b]10;", "\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
        ("\x1b]11;", "\x1b]11;rgb:0000/0000/0000\x07"),
        ("\x1b]4;1;", "\x1b]4;1;rgb:6464/6464/4d4d\x07"),
        ("\x1bP>|", "\x1bP>|Ghostty 1.3.1\x1b\\"),
        ("\x1b_Gi=1;", "\x1b_Gi=1;OK\x1b\\"),
        ("\x1b]7501;?", "\x1b]7501;?\x1b\\"),
    ];

    for (header, reply) in replies {
        for split in header.len()..reply.len() {
            if !reply.is_char_boundary(split) {
                continue;
            }
            let (delayed_buffer, sequences, _) = setup_buffer(Some(StdinBufferOptions::default()));
            delayed_buffer.process(StdinBufferData::Text(&reply[..split]));
            wait_ms(5000).await;
            let tail = format!("{}x", &reply[split..]);
            delayed_buffer.process(StdinBufferData::Text(&tail));
            let emitted = sequences.lock().unwrap().clone();
            assert!(!emitted.iter().any(|sequence| matches_key(sequence, "ctrl+g")));
            assert_eq!(emitted, vec![reply.to_string(), "x".to_string()]);
            delayed_buffer.destroy();
        }
    }
}

#[tokio::test(start_paused = true)]
async fn keeps_osc_reply_buffered_across_multiple_expired_sequence_timeouts() {
    let (buffer, emitted, _) = setup_buffer(None);
    buffer.process(StdinBufferData::Text("\x1b]4;1;rgb:eaea/6c"));
    wait_ms(100).await;
    assert!(emitted.lock().unwrap().is_empty());
    buffer.process(StdinBufferData::Text("6c/6d6d"));
    wait_ms(100).await;
    assert!(emitted.lock().unwrap().is_empty());
    buffer.process(StdinBufferData::Text("\x07"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b]4;1;rgb:eaea/6c6c/6d6d\x07"]);
    buffer.destroy();
}

#[tokio::test(start_paused = true)]
async fn still_times_out_bare_string_introducers_as_alt_keys() {
    let (buffer, emitted, _) = setup_buffer(None);
    for prefix in ["\x1b]", "\x1bP", "\x1b_"] {
        buffer.process(StdinBufferData::Text(prefix));
        wait_ms(10).await;
    }
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b]", "\x1bP", "\x1b_"]);
    buffer.destroy();
}

#[tokio::test(start_paused = true)]
async fn recovers_batched_legacy_alt_input_within_sequence_timeout() {
    let ambiguous_inputs = [
        "\x1b]a",
        "\x1b]1",
        "\x1b]10",
        "\x1b]0;title",
        "\x1b]4;",
        "\x1b]4;a;",
        "\x1bPa",
        "\x1bP>",
        "\x1b_a",
        "\x1b_G",
        "\x1b_Gi=1",
    ];
    for ambiguous in ambiguous_inputs {
        let (legacy_buffer, sequences, _) = setup_buffer(Some(StdinBufferOptions::default()));
        legacy_buffer.process(StdinBufferData::Text(ambiguous));
        wait_ms(20).await;
        legacy_buffer.process(StdinBufferData::Text("x\r\x03\x04"));
        wait_ms(49).await;
        assert!(sequences.lock().unwrap().is_empty(), "{ambiguous:?}");
        wait_ms(1).await;

        let mut expected = vec![ambiguous[..2].to_string()];
        expected.extend(ambiguous[2..].chars().map(|c| c.to_string()));
        expected.extend(["x", "\r", "\x03", "\x04"].into_iter().map(str::to_string));

        assert_eq!(*sequences.lock().unwrap(), expected, "{ambiguous:?}");
        assert_eq!(legacy_buffer.get_buffer(), "");

        legacy_buffer.process(StdinBufferData::Text("y\r\x03\x04"));
        let tail: Vec<String> = sequences.lock().unwrap().iter().rev().take(4).rev().cloned().collect();
        assert_eq!(tail, vec!["y", "\r", "\x03", "\x04"]);
        legacy_buffer.destroy();
    }
}

#[tokio::test(start_paused = true)]
async fn recognizes_reply_header_assembled_within_ambiguous_input_timeout() {
    let (buffer, emitted, _) = setup_buffer(Some(StdinBufferOptions::default()));
    buffer.process(StdinBufferData::Text("\x1b]1"));
    wait_ms(5).await;
    buffer.process(StdinBufferData::Text("0;"));
    wait_ms(5000).await;
    assert!(emitted.lock().unwrap().is_empty());
    buffer.process(StdinBufferData::Text("rgb:ffff/ffff/ffff\x1b\\"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b]10;rgb:ffff/ffff/ffff\x1b\\"]);
    buffer.destroy();
}

#[tokio::test(start_paused = true)]
async fn recovers_from_unterminated_reply_when_new_escape_sequence_arrives() {
    let (buffer, emitted, _) = setup_buffer(Some(StdinBufferOptions::default()));
    buffer.process(StdinBufferData::Text("\x1b]4;1;rgb:eaea/6c"));
    wait_ms(100).await;
    buffer.process(StdinBufferData::Text("\x1b"));
    wait_ms(100).await;
    buffer.process(StdinBufferData::Text("[Ahello"));
    assert_eq!(*emitted.lock().unwrap(), vec!["\x1b[A", "h", "e", "l", "l", "o"]);
    buffer.destroy();
}

#[test]
fn discards_canceled_reply_without_swallowing_subsequent_input() {
    let (buffer, emitted, _) = setup_buffer(None);
    for cancel in ["\x18", "\x1a"] {
        let input = format!("\x1b]11;rgb:0000{cancel}x");
        buffer.process(StdinBufferData::Text(&input));
    }
    assert_eq!(*emitted.lock().unwrap(), vec!["x", "x"]);
}
