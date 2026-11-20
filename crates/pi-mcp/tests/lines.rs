use std::sync::{Arc, Mutex};

use pi_mcp::JsonRpcMessage;
use pi_mcp::transports::lines::{JsonRpcLineReader, JsonRpcLineReaderOptions};
use pretty_assertions::assert_eq;
use serde_json::json;

struct ReaderHarness {
    reader: JsonRpcLineReader,
    messages: Arc<Mutex<Vec<JsonRpcMessage>>>,
    errors: Arc<Mutex<Vec<String>>>,
}

fn start_reader(max_message_bytes: Option<i64>) -> ReaderHarness {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let msgs_cb = messages.clone();
    let errs_cb = errors.clone();
    let reader = JsonRpcLineReader::new(JsonRpcLineReaderOptions {
        max_message_bytes,
        max_stderr_bytes: None,
        on_stderr: None,
        on_message: Arc::new(move |m| msgs_cb.lock().unwrap().push(m.clone())),
        on_error: Arc::new(move |e| errs_cb.lock().unwrap().push(e.to_string())),
    });
    ReaderHarness {
        reader,
        messages,
        errors,
    }
}

fn feed(reader: &JsonRpcLineReader, bytes: &[u8], chunk_size: usize) {
    for chunk in bytes.chunks(chunk_size) {
        reader.read_stdout(chunk);
    }
}

#[test]
fn joins_a_message_split_across_many_chunks_when_its_newline_arrives() {
    let h = start_reader(None);
    let text = "x".repeat(1024 * 1024);
    let msg: JsonRpcMessage = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": { "text": text }
    }))
    .unwrap();
    let line = format!("{}\n", pi_js::json::stringify(&msg));
    feed(&h.reader, line.as_bytes(), 4096);
    assert!(h.errors.lock().unwrap().is_empty());
    assert_eq!(*h.messages.lock().unwrap(), vec![msg]);
}

#[test]
fn handles_multibyte_characters_crlf_blank_lines_and_multiple_messages_per_chunk() {
    let m1: JsonRpcMessage = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": { "text": "héllo 🌍" }
    }))
    .unwrap();
    let m2: JsonRpcMessage = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "method": "notifications/x"
    }))
    .unwrap();
    let m3: JsonRpcMessage = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": {}
    }))
    .unwrap();
    let input = [
        pi_js::json::stringify(&m1),
        String::new(),
        format!("{}\r", pi_js::json::stringify(&m2)),
        "not json".to_string(),
        pi_js::json::stringify(&m3),
        String::new(),
    ]
    .join("\n");

    for chunk_size in 1..=input.len() {
        let h = start_reader(None);
        feed(&h.reader, input.as_bytes(), chunk_size);
        h.reader.end();
        assert_eq!(*h.messages.lock().unwrap(), vec![m1.clone(), m2.clone(), m3.clone()]);
        assert_eq!(h.errors.lock().unwrap().len(), 1);
    }
}

#[test]
fn drops_an_oversized_message_and_keeps_reading_the_next_one() {
    let h = start_reader(Some(64));
    let next: JsonRpcMessage = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": {}
    }))
    .unwrap();
    feed(&h.reader, "a".repeat(50).as_bytes(), 10);
    let second_part = format!("{}\n{}\n", "a".repeat(50), pi_js::json::stringify(&next));
    feed(&h.reader, second_part.as_bytes(), 10);
    assert_eq!(h.errors.lock().unwrap()[0], "MCP stdio message exceeds 64 bytes");
    assert_eq!(*h.messages.lock().unwrap(), vec![next]);
}

#[test]
fn reports_a_message_without_its_newline_at_the_end() {
    let h = start_reader(None);
    feed(&h.reader, br#"{"jsonrpc":"2.0","#, 4);
    h.reader.end();
    assert_eq!(
        *h.errors.lock().unwrap(),
        vec!["MCP stdio server closed with an incomplete JSON-RPC message"]
    );
}
