//! Port of packages/coding-agent/test/read-streaming.test.ts

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use futures::StreamExt;
use pi_agent_core::types::AgentToolResult;
use pi_ai::types::UserContentBlock;
use pi_coding_agent::core::tools::read::{
    ReadByteStream, ReadOperations, ReadToolDetails, ReadToolOptions, create_local_read_operations, create_read_tool,
};
use pi_coding_agent::core::tools::truncate::{DEFAULT_MAX_BYTES, format_size, truncate_head};
use pi_js::BoxFuture;
use pi_js::abort::AbortController;
use serde_json::{Value, json};

#[derive(Debug, PartialEq)]
enum ReadOutcome {
    Ok { text: String, details: Value },
    Err(String),
}

fn reference_read(text_content: &str, path: &str, offset: Option<f64>, limit: Option<f64>) -> ReadOutcome {
    let all_lines: Vec<&str> = text_content.split('\n').collect();
    let total_file_lines = all_lines.len();
    let offset_val = offset.map(|o| o.floor().max(1.0) as usize).unwrap_or(1);
    let start_line = offset_val - 1;
    let start_line_display = start_line + 1;
    if start_line >= total_file_lines {
        return ReadOutcome::Err(format!(
            "Offset {offset_val} is beyond end of file ({total_file_lines} lines total)"
        ));
    }
    let limit_opt = limit.filter(|l| *l >= 0.0).map(|l| l.floor() as usize);
    let (selected_content, user_limited_lines) = if let Some(l) = limit_opt {
        let end_line = (start_line + l).min(total_file_lines);
        (all_lines[start_line..end_line].join("\n"), Some(end_line - start_line))
    } else {
        (all_lines[start_line..].join("\n"), None)
    };
    let truncation = truncate_head(&selected_content, None);
    let mut details = Value::Null;
    let output_text = if truncation.first_line_exceeds_limit {
        let first_line_size = format_size(all_lines[start_line].len());
        let text = format!(
            "[Line {start_line_display} is {first_line_size}, exceeds {} limit. Use bash: sed -n '{start_line_display}p' {path} | head -c {}]",
            format_size(DEFAULT_MAX_BYTES),
            DEFAULT_MAX_BYTES
        );
        details = serde_json::to_value(ReadToolDetails {
            truncation: Some(truncation),
        })
        .unwrap();
        text
    } else if truncation.truncated {
        let end_line_display = start_line_display + truncation.output_lines.max(0) as usize - 1;
        let next_offset = end_line_display + 1;
        let mut text = truncation.content.clone();
        if truncation.truncated_by == Some(pi_coding_agent::core::tools::truncate::TruncatedBy::Lines) {
            text.push_str(&format!(
                "\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines}. Use offset={next_offset} to continue.]"
            ));
        } else {
            text.push_str(&format!(
                "\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines} ({} limit). Use offset={next_offset} to continue.]",
                format_size(DEFAULT_MAX_BYTES)
            ));
        }
        details = serde_json::to_value(ReadToolDetails {
            truncation: Some(truncation),
        })
        .unwrap();
        text
    } else if let Some(ul) = user_limited_lines
        && start_line + ul < total_file_lines
    {
        let remaining = total_file_lines - (start_line + ul);
        let next_offset = start_line + ul + 1;
        format!(
            "{}\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]",
            truncation.content
        )
    } else {
        truncation.content
    };
    ReadOutcome::Ok {
        text: output_text,
        details,
    }
}

fn outcome_from_result(res: pi_js::Result<AgentToolResult>) -> ReadOutcome {
    match res {
        Ok(r) => {
            let text = r
                .content
                .first()
                .and_then(|c| match c {
                    UserContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            ReadOutcome::Ok {
                text,
                details: r.details.unwrap_or(Value::Null),
            }
        }
        Err(e) => ReadOutcome::Err(e.message().to_string()),
    }
}

struct ChunkedStreamOps {
    bytes: Arc<Vec<u8>>,
    chunk_size: usize,
    read_file_calls: Arc<AtomicUsize>,
    stream_calls: Arc<AtomicUsize>,
}

impl ReadOperations for ChunkedStreamOps {
    fn access(&self, _: &str) -> BoxFuture<pi_js::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn read_file(&self, _: &str) -> BoxFuture<pi_js::Result<Vec<u8>>> {
        self.read_file_calls.fetch_add(1, Ordering::SeqCst);
        let b = Arc::clone(&self.bytes);
        Box::pin(async move { Ok((*b).clone()) })
    }

    fn create_read_stream(
        &self,
        _: &str,
        _: Option<pi_js::abort::AbortSignal>,
    ) -> Option<BoxFuture<pi_js::Result<ReadByteStream>>> {
        self.stream_calls.fetch_add(1, Ordering::SeqCst);
        let bytes = Arc::clone(&self.bytes);
        let chunk_size = self.chunk_size;
        Some(Box::pin(async move {
            let s = futures::stream::unfold(0usize, move |pos| {
                let bytes = Arc::clone(&bytes);
                async move {
                    if pos >= bytes.len() {
                        None
                    } else {
                        let end = (pos + chunk_size).min(bytes.len());
                        Some((Ok(bytes[pos..end].to_vec()), end))
                    }
                }
            });
            Ok(Box::pin(s) as ReadByteStream)
        }))
    }
}

fn make_lines(count: usize, make: impl Fn(usize) -> String) -> String {
    (0..count).map(make).collect::<Vec<_>>().join("\n")
}

fn text_cases() -> BTreeMap<&'static str, String> {
    let fifty_kb = format!("{}{}", make_lines(512, |_| "y".repeat(99)), "y");
    BTreeMap::from([
        ("empty", String::new()),
        ("no trailing newline", "alpha\nbeta\ngamma".to_string()),
        ("trailing newline", "alpha\nbeta\n".to_string()),
        ("CRLF", "alpha\r\nbeta\r\n\r\ngamma\r\n".to_string()),
        (
            "multi-byte chars and BOM",
            "\u{FEFF}é€😀\nñ\u{10348}字\n😀😀😀".to_string(),
        ),
        ("2001 lines", make_lines(2001, |i| format!("line {i}"))),
        (
            "2001 lines, empty line and more",
            format!("{}\n\nmore", make_lines(2001, |i| format!("line {i}"))),
        ),
        (
            "first line over 50KB",
            format!("{}\nsecond", "é".repeat(DEFAULT_MAX_BYTES / 2 + 1)),
        ),
        (
            "line over 50KB after short lines",
            format!("a\nb\n{}\nc", "z".repeat(60 * 1024)),
        ),
        ("exactly 50KB and trailing newline", format!("{fifty_kb}\n")),
        ("exactly 50KB, empty line and more", format!("{fifty_kb}\n\nmore")),
        (
            "multi-byte over 50KB",
            make_lines(700, |i| format!("{i} {}", "€😀".repeat(13))),
        ),
    ])
}

#[tokio::test]
async fn gives_same_output_as_whole_file_algorithm() {
    let file_path = "/tmp/pi-read-streaming-test-missing/file.txt";
    for (name, text) in text_cases() {
        let bytes = Arc::new(text.as_bytes().to_vec());
        let total = text.split('\n').count() as f64;
        for offset in [None, Some(2.0), Some(3.0), Some(total), Some(total + 1.0)] {
            for limit in [None, Some(0.0), Some(1.0), Some(total)] {
                let expected = reference_read(&text, file_path, offset, limit);
                for chunk_size in [1usize, 4096] {
                    let read_file_calls = Arc::new(AtomicUsize::new(0));
                    let stream_calls = Arc::new(AtomicUsize::new(0));
                    let def = create_read_tool(
                        ".",
                        Some(ReadToolOptions {
                            operations: Some(Arc::new(ChunkedStreamOps {
                                bytes: Arc::clone(&bytes),
                                chunk_size,
                                read_file_calls: Arc::clone(&read_file_calls),
                                stream_calls: Arc::clone(&stream_calls),
                            })),
                            ..Default::default()
                        }),
                    );
                    let mut args = json!({ "path": file_path });
                    if let Some(o) = offset {
                        args["offset"] = json!(o);
                    }
                    if let Some(l) = limit {
                        args["limit"] = json!(l);
                    }
                    let actual = outcome_from_result((def.execute)("call".into(), args, None, None).await);
                    assert_eq!(
                        actual, expected,
                        "{name}, offset={offset:?}, limit={limit:?}, chunk={chunk_size}"
                    );
                    assert_eq!(read_file_calls.load(Ordering::SeqCst), 0);
                    assert!(stream_calls.load(Ordering::SeqCst) > 0);
                }
            }
        }
    }
}

#[tokio::test]
async fn keeps_details_key_order_which_session_files_serialize() {
    let text = text_cases()["2001 lines"].clone();
    let def = create_read_tool(
        ".",
        Some(ReadToolOptions {
            operations: Some(Arc::new(ChunkedStreamOps {
                bytes: Arc::new(text.as_bytes().to_vec()),
                chunk_size: 4096,
                read_file_calls: Arc::new(AtomicUsize::new(0)),
                stream_calls: Arc::new(AtomicUsize::new(0)),
            })),
            ..Default::default()
        }),
    );
    let ReadOutcome::Ok {
        details: streamed_details,
        ..
    } = outcome_from_result(
        (def.execute)("call".into(), json!({ "path": "/tmp/f.txt", "offset": 1 }), None, None).await,
    )
    else {
        panic!("expected ok");
    };
    let ReadOutcome::Ok {
        details: expected_details,
        ..
    } = reference_read(&text, "/tmp/f.txt", Some(1.0), None)
    else {
        panic!("expected ok");
    };
    assert_eq!(
        pi_js::json::stringify(&streamed_details),
        pi_js::json::stringify(&expected_details)
    );
}

#[tokio::test]
async fn decodes_multibyte_chars_split_across_chunks_and_invalid_utf8() {
    let mut raw = b"a\xc3\xa9\xe2\x82\xac\xf0\x9f\x98\x80\n\xf0\x9f\x98\x80b\xf0\x90\x8d\x88\n".to_vec();
    raw.extend_from_slice(&[
        0x61, 0xc3, 0x0a, 0xe2, 0x82, 0x0a, 0xff, 0xfe, 0x80, 0xf0, 0x9f, 0x98, 0x0a, 0xed, 0xa0, 0x80,
    ]);
    let expected = reference_read(&String::from_utf8_lossy(&raw), "/tmp/u.txt", None, None);
    let raw = Arc::new(raw);
    for chunk_size in [1usize, 2, 3] {
        let def = create_read_tool(
            ".",
            Some(ReadToolOptions {
                operations: Some(Arc::new(ChunkedStreamOps {
                    bytes: Arc::clone(&raw),
                    chunk_size,
                    read_file_calls: Arc::new(AtomicUsize::new(0)),
                    stream_calls: Arc::new(AtomicUsize::new(0)),
                })),
                ..Default::default()
            }),
        );
        let actual =
            outcome_from_result((def.execute)("call".into(), json!({ "path": "/tmp/u.txt" }), None, None).await);
        assert_eq!(actual, expected, "chunk_size={chunk_size}");
    }
}

#[tokio::test]
async fn uses_read_file_without_stream_and_for_fractional_or_negative_ranges() {
    struct NoStreamOps(Vec<u8>, Arc<AtomicUsize>);
    impl ReadOperations for NoStreamOps {
        fn access(&self, _: &str) -> BoxFuture<pi_js::Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn read_file(&self, _: &str) -> BoxFuture<pi_js::Result<Vec<u8>>> {
            self.1.fetch_add(1, Ordering::SeqCst);
            let d = self.0.clone();
            Box::pin(async move { Ok(d) })
        }
    }
    let text = "one\ntwo\nthree\nfour";
    let read_calls = Arc::new(AtomicUsize::new(0));
    let def_no_stream = create_read_tool(
        ".",
        Some(ReadToolOptions {
            operations: Some(Arc::new(NoStreamOps(text.as_bytes().to_vec(), Arc::clone(&read_calls)))),
            ..Default::default()
        }),
    );
    for (o, l) in [(None, None), (Some(2.0), Some(1.0)), (Some(9.0), None)] {
        let mut args = json!({ "path": "/tmp/r.txt" });
        if let Some(ov) = o {
            args["offset"] = json!(ov);
        }
        if let Some(lv) = l {
            args["limit"] = json!(lv);
        }
        assert_eq!(
            outcome_from_result((def_no_stream.execute)("c".into(), args, None, None).await),
            reference_read(text, "/tmp/r.txt", o, l)
        );
    }
    assert_eq!(read_calls.load(Ordering::SeqCst), 3);

    let frac_read_calls = Arc::new(AtomicUsize::new(0));
    let frac_stream_calls = Arc::new(AtomicUsize::new(0));
    let def_with_stream = create_read_tool(
        ".",
        Some(ReadToolOptions {
            operations: Some(Arc::new(ChunkedStreamOps {
                bytes: Arc::new(text.as_bytes().to_vec()),
                chunk_size: 1,
                read_file_calls: Arc::clone(&frac_read_calls),
                stream_calls: Arc::clone(&frac_stream_calls),
            })),
            ..Default::default()
        }),
    );
    for (o, l) in [(Some(2.5), Some(1.0)), (None, Some(-1.0)), (Some(1.0), Some(1.5))] {
        let mut args = json!({ "path": "/tmp/r.txt" });
        if let Some(ov) = o {
            args["offset"] = json!(ov);
        }
        if let Some(lv) = l {
            args["limit"] = json!(lv);
        }
        let _ = (def_with_stream.execute)("c".into(), args, None, None).await;
    }
    assert_eq!(frac_stream_calls.load(Ordering::SeqCst), 0);
    assert_eq!(frac_read_calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn reads_local_files_through_default_stream_and_stops_at_open_size() {
    let temp = tempfile::tempdir().expect("tempdir");
    let local_path = temp.path().join("local.txt");
    let text = make_lines(3000, |i| format!("{i} {}", "w".repeat(i % 40)));
    std::fs::write(&local_path, &text).expect("write local");
    let def = create_read_tool(temp.path().to_str().expect("utf8"), None);
    let path_str = local_path.to_str().expect("utf8");
    for (o, l) in [(None, None), (Some(1500.0), Some(10.0))] {
        let mut args = json!({ "path": path_str });
        if let Some(ov) = o {
            args["offset"] = json!(ov);
        }
        if let Some(lv) = l {
            args["limit"] = json!(lv);
        }
        let res = outcome_from_result((def.execute)("c".into(), args, None, None).await);
        assert_eq!(res, reference_read(&text, path_str, o, l));
    }

    let dir_err = (def.execute)(
        "c".into(),
        json!({ "path": temp.path().to_str().expect("utf8") }),
        None,
        None,
    )
    .await
    .expect_err("expected dir read error");
    assert!(
        dir_err.to_string().contains("directory") || dir_err.to_string().contains("EISDIR"),
        "dir_err: {dir_err}"
    );

    let growing_path = temp.path().join("growing.txt");
    let initial = format!("head\n{}", "x".repeat(200_000));
    std::fs::write(&growing_path, &initial).expect("write growing");
    let local_ops = create_local_read_operations();
    let mut stream = local_ops
        .create_read_stream(growing_path.to_str().expect("utf8"), None)
        .expect("has stream")
        .await
        .expect("open stream");
    let first_chunk = stream.next().await.expect("first").expect("ok");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&growing_path)
        .expect("append open");
    file.write_all(b"\nadded").expect("append");
    drop(file);
    let mut total_len = first_chunk.len();
    while let Some(next_chunk) = stream.next().await {
        total_len += next_chunk.expect("chunk").len();
    }
    assert_eq!(total_len, initial.len());
}

#[tokio::test]
async fn reads_200mb_line_in_bounded_memory() {
    struct BigLineOps;
    impl ReadOperations for BigLineOps {
        fn access(&self, _: &str) -> BoxFuture<pi_js::Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn read_file(&self, _: &str) -> BoxFuture<pi_js::Result<Vec<u8>>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn create_read_stream(
            &self,
            _: &str,
            _: Option<pi_js::abort::AbortSignal>,
        ) -> Option<BoxFuture<pi_js::Result<ReadByteStream>>> {
            Some(Box::pin(async move {
                let chunk = vec![b'x'; 64 * 1024];
                let s = futures::stream::unfold(0usize, move |i| {
                    let c = chunk.clone();
                    async move { if i < 3200 { Some((Ok(c), i + 1)) } else { None } }
                });
                Ok(Box::pin(s) as ReadByteStream)
            }))
        }
    }
    let def = create_read_tool(
        ".",
        Some(ReadToolOptions {
            operations: Some(Arc::new(BigLineOps)),
            ..Default::default()
        }),
    );
    let res = (def.execute)("call".into(), json!({ "path": "big.log" }), None, None)
        .await
        .expect("200mb read");
    let text = match &res.content[0] {
        UserContentBlock::Text(t) => t.text.as_str(),
        _ => "",
    };
    assert_eq!(
        text,
        format!(
            "[Line 1 is 200.0MB, exceeds 50.0KB limit. Use bash: sed -n '1p' big.log | head -c {DEFAULT_MAX_BYTES}]"
        )
    );
}

#[tokio::test]
async fn stops_consuming_stream_when_aborted() {
    struct TrackedStream {
        chunks_read: Arc<AtomicUsize>,
        closed: Arc<AtomicBool>,
        controller: Arc<AbortController>,
    }
    struct Guard(Arc<AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    impl ReadOperations for TrackedStream {
        fn access(&self, _: &str) -> BoxFuture<pi_js::Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn read_file(&self, _: &str) -> BoxFuture<pi_js::Result<Vec<u8>>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn create_read_stream(
            &self,
            _: &str,
            _: Option<pi_js::abort::AbortSignal>,
        ) -> Option<BoxFuture<pi_js::Result<ReadByteStream>>> {
            let chunks_read = Arc::clone(&self.chunks_read);
            let closed = Arc::clone(&self.closed);
            let controller = Arc::clone(&self.controller);
            Some(Box::pin(async move {
                let guard = Guard(closed);
                let s = futures::stream::unfold(guard, move |g| {
                    let chunks_read = Arc::clone(&chunks_read);
                    let controller = Arc::clone(&controller);
                    async move {
                        let next = chunks_read.fetch_add(1, Ordering::SeqCst) + 1;
                        if next == 3 {
                            controller.abort(None);
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                        Some((Ok(b"line\n".to_vec()), g))
                    }
                });
                Ok(Box::pin(s) as ReadByteStream)
            }))
        }
    }

    let controller = Arc::new(AbortController::new());
    let chunks_read = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(AtomicBool::new(false));
    let def = create_read_tool(
        ".",
        Some(ReadToolOptions {
            operations: Some(Arc::new(TrackedStream {
                chunks_read: Arc::clone(&chunks_read),
                closed: Arc::clone(&closed),
                controller: Arc::clone(&controller),
            })),
            ..Default::default()
        }),
    );
    let err = (def.execute)(
        "call".into(),
        json!({ "path": "/tmp/abort.txt" }),
        Some(controller.signal()),
        None,
    )
    .await
    .expect_err("expected abort");
    assert!(err.to_string().contains("Operation aborted"), "err: {err}");
    assert!(closed.load(Ordering::SeqCst));
    assert_eq!(chunks_read.load(Ordering::SeqCst), 3);
}
