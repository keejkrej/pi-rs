//! Port of packages/coding-agent/test/rpc-jsonl.test.ts

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use pi_coding_agent::modes::rpc::jsonl::{attach_jsonl_line_reader, serialize_json_line};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, ReadBuf};

struct ChunkedReader {
    chunks: Vec<Vec<u8>>,
    index: usize,
}

impl AsyncRead for ChunkedReader {
    fn poll_read(mut self: Pin<&mut Self>, _cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        if self.index >= self.chunks.len() {
            return Poll::Ready(Ok(()));
        }
        let chunk = &self.chunks[self.index];
        let to_copy = chunk.len().min(buf.remaining());
        buf.put_slice(&chunk[..to_copy]);
        if to_copy == chunk.len() {
            self.index += 1;
        } else {
            let idx = self.index;
            self.chunks[idx].drain(..to_copy);
        }
        Poll::Ready(Ok(()))
    }
}

async fn read_chunks(chunks: Vec<Vec<u8>>) -> Vec<String> {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let lines_c = lines.clone();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let done_tx = Arc::new(Mutex::new(Some(done_tx)));
    let _handle = attach_jsonl_line_reader(
        ChunkedReader { chunks, index: 0 },
        Arc::new(move |line| lines_c.lock().unwrap().push(line.to_string())),
        Some(Arc::new(move || {
            if let Some(tx) = done_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
        })),
    );
    done_rx.await.unwrap();
    let guard = lines.lock().unwrap();
    guard.clone()
}

#[test]
fn serializes_strict_jsonl_records_without_escaping_unicode_separators() {
    let line = serialize_json_line(&json!({ "text": "a\u{2028}b\u{2029}c" }));

    assert!(line.contains("a\u{2028}b\u{2029}c"));
    assert!(line.ends_with('\n'));
    let parsed: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(parsed, json!({ "text": "a\u{2028}b\u{2029}c" }));
}

#[tokio::test]
async fn splits_on_lf_only_and_preserves_u2028_u2029_inside_payloads() {
    let serialized = serialize_json_line(&json!({ "text": "a\u{2028}b\u{2029}c" }));
    let bytes = serialized.as_bytes();
    // Also split mid-UTF-8 sequence to verify multi-byte reassembly.
    let split_at = bytes.len() / 2;
    let lines = read_chunks(vec![bytes[..split_at].to_vec(), bytes[split_at..].to_vec()]).await;

    assert_eq!(lines.len(), 1);
    let parsed: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(parsed, json!({ "text": "a\u{2028}b\u{2029}c" }));
}

#[tokio::test]
async fn handles_crlf_delimited_input() {
    let lines = read_chunks(vec![b"{\"a\":1}\r\n{\"b\":2}\r\n".to_vec()]).await;
    assert_eq!(lines, vec!["{\"a\":1}", "{\"b\":2}"]);
}

#[tokio::test]
async fn emits_a_final_line_without_trailing_lf() {
    let lines = read_chunks(vec![b"{\"a\":1}".to_vec()]).await;
    assert_eq!(lines, vec!["{\"a\":1}"]);
}
