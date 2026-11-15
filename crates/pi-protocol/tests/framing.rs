//! Port of packages/protocol/test/framing.test.ts

use pi_protocol::{DEFAULT_MAX_FRAME_LENGTH, FrameDecoder, FrameDecoderOptions, FrameError, encode_frame};

fn concatenate(chunks: &[Vec<u8>]) -> Vec<u8> {
    let total: usize = chunks.iter().map(Vec::len).sum();
    let mut result = Vec::with_capacity(total);
    for chunk in chunks {
        result.extend_from_slice(chunk);
    }
    result
}

fn assert_frame_error(err: pi_js::Error) -> String {
    assert!(
        err.downcast_ref::<FrameError>().is_some(),
        "expected FrameError, got {err:?}"
    );
    err.message()
}

mod binary_framing {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn prefixes_payloads_with_a_four_byte_big_endian_length() {
        assert_eq!(
            encode_frame(&[0xaa, 0xbb, 0xcc]).unwrap(),
            vec![0x00, 0x00, 0x00, 0x03, 0xaa, 0xbb, 0xcc]
        );
        assert_eq!(encode_frame(&[]).unwrap(), vec![0, 0, 0, 0]);
    }

    #[test]
    fn decodes_fragmented_coalesced_and_empty_frames_in_order() {
        let wire = concatenate(&[
            encode_frame(&[1, 2, 3]).unwrap(),
            encode_frame(&[]).unwrap(),
            encode_frame(&[4]).unwrap(),
        ]);
        let decoder = FrameDecoder::new(None);
        let mut frames = Vec::new();
        for &byte in &wire {
            frames.extend(decoder.push(&[byte]).unwrap());
        }
        decoder.end().unwrap();
        assert_eq!(frames, vec![vec![1, 2, 3], vec![], vec![4]]);

        let coalesced = FrameDecoder::new(None);
        assert_eq!(coalesced.push(&wire).unwrap(), frames);
        coalesced.end().unwrap();
    }

    #[test]
    fn assembles_payloads_spanning_multiple_internal_blocks() {
        let payload: Vec<u8> = (0..70_000).map(|index| (index % 251) as u8).collect();
        let wire = encode_frame(&payload).unwrap();
        let decoder = FrameDecoder::new(None);
        let mut frames = Vec::new();
        frames.extend(decoder.push(&wire[..101]).unwrap());
        frames.extend(decoder.push(&wire[101..65_541]).unwrap());
        frames.extend(decoder.push(&wire[65_541..]).unwrap());
        decoder.end().unwrap();
        assert_eq!(frames, vec![payload]);
    }

    #[test]
    fn handles_every_split_point_across_a_frame() {
        let wire = encode_frame(&[10, 20, 30, 40]).unwrap();
        for split in 0..=wire.len() {
            let decoder = FrameDecoder::new(None);
            let mut frames = Vec::new();
            frames.extend(decoder.push(&wire[..split]).unwrap());
            frames.extend(decoder.push(&wire[split..]).unwrap());
            decoder.end().unwrap();
            assert_eq!(frames, vec![vec![10, 20, 30, 40]]);
        }
    }

    #[test]
    fn copies_payload_bytes_instead_of_retaining_or_aliasing_input_chunks() {
        let mut chunk = encode_frame(&[1, 2, 3]).unwrap();
        let decoder = FrameDecoder::new(None);
        let frames = decoder.push(&chunk).unwrap();
        chunk.fill(9);
        assert_eq!(frames, vec![vec![1, 2, 3]]);
    }

    #[test]
    fn accepts_empty_chunks_and_a_clean_empty_stream() {
        let decoder = FrameDecoder::new(None);
        assert_eq!(decoder.push(&[]).unwrap(), Vec::<Vec<u8>>::new());
        decoder.end().unwrap();
    }

    #[test]
    fn rejects_a_truncated_stream_at_end() {
        let cases: [(&str, &[u8]); 2] = [("partial header", &[0, 0, 0]), ("partial payload", &[0, 0, 0, 2, 1])];
        for (label, wire) in cases {
            let decoder = FrameDecoder::new(None);
            assert_eq!(decoder.push(wire).unwrap(), Vec::<Vec<u8>>::new(), "{label}");
            let err = decoder.end().expect_err(label);
            assert_frame_error(err);
        }
    }

    #[test]
    fn rejects_an_oversized_declared_length_as_soon_as_its_header_is_complete() {
        let decoder = FrameDecoder::new(Some(FrameDecoderOptions {
            max_frame_length: Some(3),
        }));
        let msg = assert_frame_error(decoder.push(&[0, 0, 0, 4]).expect_err("oversized header"));
        assert!(msg.to_lowercase().contains("limit"), "unexpected error: {msg}");
        let msg = assert_frame_error(decoder.push(&[1]).expect_err("push after failed"));
        assert!(msg.to_lowercase().contains("failed"), "unexpected error: {msg}");
    }

    #[test]
    fn accepts_a_frame_exactly_at_the_configured_maximum() {
        let decoder = FrameDecoder::new(Some(FrameDecoderOptions {
            max_frame_length: Some(3),
        }));
        assert_eq!(
            decoder.push(&encode_frame(&[1, 2, 3]).unwrap()).unwrap(),
            vec![vec![1, 2, 3]]
        );
        decoder.end().unwrap();
    }

    #[test]
    fn cannot_be_pushed_after_end() {
        let decoder = FrameDecoder::new(None);
        decoder.end().unwrap();
        let msg = assert_frame_error(decoder.push(&[]).expect_err("push after end"));
        assert!(msg.to_lowercase().contains("ended"), "unexpected error: {msg}");
        let msg = assert_frame_error(decoder.end().expect_err("end after end"));
        assert!(msg.to_lowercase().contains("ended"), "unexpected error: {msg}");
    }

    #[test]
    fn rejects_invalid_maximum_frame_length() {
        // PORT: `1.5` and `NaN` are TS `number`-only; Rust `max_frame_length` is `Option<i64>`.
        // Test both `try_new` (exact `RangeError`) and `new` (panics).
        for max_frame_length in [-1, DEFAULT_MAX_FRAME_LENGTH * 1_000] {
            let err = FrameDecoder::try_new(Some(FrameDecoderOptions {
                max_frame_length: Some(max_frame_length),
            }))
            .err()
            .expect("invalid max_frame_length must fail");
            assert_eq!(err.name(), "RangeError");

            let panicked = std::panic::catch_unwind(|| {
                let _ = FrameDecoder::new(Some(FrameDecoderOptions {
                    max_frame_length: Some(max_frame_length),
                }));
            });
            assert!(
                panicked.is_err(),
                "FrameDecoder::new should panic on {max_frame_length}"
            );
        }
    }
}
