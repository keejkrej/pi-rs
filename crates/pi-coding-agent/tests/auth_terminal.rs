//! Port of packages/coding-agent/test/auth-terminal.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_coding_agent::cli::auth_terminal::create_process_login_terminal;
use pi_js::abort::{AbortController, AbortSignal};
use serial_test::serial;
use tokio::io::{AsyncWriteExt, BufReader};

fn setup() -> (
    Arc<AbortController>,
    Arc<AtomicUsize>,
    pi_coding_agent::cli::auth_terminal::ProcessLoginTerminal,
) {
    let controller = Arc::new(AbortController::new());
    let interrupts = Arc::new(AtomicUsize::new(0));
    let c_c = Arc::clone(&controller);
    let i_c = Arc::clone(&interrupts);
    let terminal = create_process_login_terminal(Arc::new(move || {
        i_c.fetch_add(1, Ordering::SeqCst);
        c_c.abort(None);
    }));
    (controller, interrupts, terminal)
}

#[tokio::test]
#[serial]
async fn rejects_and_cancels_the_login_when_stdin_closes_before_an_answer() {
    let _cap = pi_js::console::testing::capture();
    let (controller, interrupts, terminal) = setup();
    let sig = controller.signal();
    let (writer, reader) = tokio::io::duplex(64);
    drop(writer);

    let err = terminal
        .question_with_reader("Enter domain: ", BufReader::new(reader), &sig)
        .await
        .expect_err("expected EOF cancellation");
    assert_eq!(err.name(), "AbortError");
    assert_eq!(interrupts.load(Ordering::SeqCst), 1);
    assert!(controller.signal().aborted());
}

#[tokio::test]
#[serial]
async fn normal_cleanup_does_not_cancel_the_login_after_an_answer() {
    let _cap = pi_js::console::testing::capture();
    let (controller, interrupts, terminal) = setup();
    let sig = controller.signal();
    let (mut writer, reader) = tokio::io::duplex(64);
    writer.write_all(b"example.com\n").await.expect("write");

    let ans = terminal
        .question_with_reader("Enter domain: ", BufReader::new(reader), &sig)
        .await
        .expect("answer");
    assert_eq!(ans, "example.com");
    assert_eq!(interrupts.load(Ordering::SeqCst), 0);
    assert!(!controller.signal().aborted());
}

#[tokio::test]
#[serial]
async fn keeps_an_answer_delivered_immediately_before_eof() {
    let _cap = pi_js::console::testing::capture();
    let (controller, interrupts, terminal) = setup();
    let sig = controller.signal();
    let (mut writer, reader) = tokio::io::duplex(64);
    writer.write_all(b"example.com\n").await.expect("write");
    drop(writer);

    let ans = terminal
        .question_with_reader("Enter domain: ", BufReader::new(reader), &sig)
        .await
        .expect("answer before EOF");
    assert_eq!(ans, "example.com");
    assert_eq!(interrupts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
#[serial]
async fn prompt_cancellation_does_not_cancel_the_whole_login() {
    let _cap = pi_js::console::testing::capture();
    let (controller, interrupts, terminal) = setup();
    let prompt = Arc::new(AbortController::new());
    let any_sig = AbortSignal::any(&[controller.signal(), prompt.signal()]);
    let (_writer, reader) = tokio::io::duplex(64);

    let handle = tokio::spawn(async move {
        terminal
            .question_with_reader("Paste code: ", BufReader::new(reader), &any_sig)
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    prompt.abort(None);

    let err = handle.await.expect("join").expect_err("expected abort");
    assert_eq!(err.name(), "AbortError");
    assert_eq!(interrupts.load(Ordering::SeqCst), 0);
    assert!(!controller.signal().aborted());
}
