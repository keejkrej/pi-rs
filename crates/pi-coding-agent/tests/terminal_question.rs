//! Port of packages/coding-agent/test/terminal-question.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_coding_agent::utils::terminal_question::question_terminal_with_reader;
use pi_js::abort::AbortController;
use serial_test::serial;
use tokio::io::{AsyncWriteExt, BufReader};

#[tokio::test]
#[serial]
async fn writes_prompt_and_resolves_with_trimmed_answer_without_calling_interrupt() {
    let cap = pi_js::console::testing::capture();
    let (mut writer, reader) = tokio::io::duplex(256);
    let interrupts = Arc::new(AtomicUsize::new(0));
    let int_c = Arc::clone(&interrupts);
    let cb = move || {
        int_c.fetch_add(1, Ordering::SeqCst);
    };
    let controller = AbortController::new();
    let sig = controller.signal();

    let handle = tokio::spawn(async move {
        question_terminal_with_reader("Paste URL: ", BufReader::new(reader), Some(&sig), Some(&cb)).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert_eq!(cap.stderr(), "Paste URL: ");

    writer
        .write_all(b"https://example.test/callback\n")
        .await
        .expect("write");
    let ans = handle.await.expect("join").expect("answer");
    assert_eq!(ans, "https://example.test/callback");
    assert_eq!(interrupts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
#[serial]
async fn rejects_on_eof_without_requiring_an_interrupt_callback() {
    let _cap = pi_js::console::testing::capture();
    let (writer, reader) = tokio::io::duplex(64);
    drop(writer);
    let controller = AbortController::new();
    let sig = controller.signal();

    let err = question_terminal_with_reader("Paste URL: ", BufReader::new(reader), Some(&sig), None)
        .await
        .expect_err("expected EOF abort error");
    assert!(err.to_string().contains("Input stream closed") || err.name() == "AbortError");
}

#[tokio::test]
#[serial]
async fn caller_cancellation_does_not_report_an_interrupt() {
    let _cap = pi_js::console::testing::capture();
    let (_writer, reader) = tokio::io::duplex(64);
    let interrupts = Arc::new(AtomicUsize::new(0));
    let int_c = Arc::clone(&interrupts);
    let cb = move || {
        int_c.fetch_add(1, Ordering::SeqCst);
    };
    let controller = Arc::new(AbortController::new());
    let sig = controller.signal();

    let handle = tokio::spawn(async move {
        question_terminal_with_reader("Paste URL: ", BufReader::new(reader), Some(&sig), Some(&cb)).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    controller.abort(None);

    let err = handle.await.expect("join").expect_err("expected abort");
    assert_eq!(err.name(), "AbortError");
    assert_eq!(interrupts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
#[serial]
async fn rejects_an_already_aborted_prompt() {
    let _cap = pi_js::console::testing::capture();
    let (_writer, reader) = tokio::io::duplex(64);
    let interrupts = Arc::new(AtomicUsize::new(0));
    let int_c = Arc::clone(&interrupts);
    let cb = move || {
        int_c.fetch_add(1, Ordering::SeqCst);
    };
    let controller = AbortController::new();
    controller.abort(None);
    let sig = controller.signal();

    let err = question_terminal_with_reader("Paste URL: ", BufReader::new(reader), Some(&sig), Some(&cb))
        .await
        .expect_err("expected immediate abort");
    assert_eq!(err.name(), "AbortError");
    assert_eq!(interrupts.load(Ordering::SeqCst), 0);
}
