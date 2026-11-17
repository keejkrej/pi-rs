//! Port of packages/coding-agent/test/clipboard-command.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pi_coding_agent::utils::clipboard_command::{ClipboardCommandOptions, run_clipboard_command};
use tempfile::TempDir;

#[tokio::test(flavor = "current_thread")]
async fn preserves_binary_output_and_distinguishes_empty_success_from_failure() {
    assert_eq!(
        run_clipboard_command(
            "/bin/sh",
            &["-c", "printf '\\000\\377\\012'"],
            ClipboardCommandOptions::default(),
        )
        .await,
        Some(vec![0, 255, 10])
    );
    assert_eq!(
        run_clipboard_command("/bin/sh", &["-c", ""], ClipboardCommandOptions::default()).await,
        Some(Vec::new())
    );
    assert_eq!(
        run_clipboard_command("/bin/sh", &["-c", "exit 1"], ClipboardCommandOptions::default()).await,
        None
    );
    assert_eq!(
        run_clipboard_command(
            "pi-clipboard-command-does-not-exist",
            &[],
            ClipboardCommandOptions::default(),
        )
        .await,
        None
    );
}

#[tokio::test(flavor = "current_thread")]
async fn sends_unicode_input_to_clipboard_writers() {
    let dir = TempDir::new().expect("tempdir");
    let out_file = dir.path().join("written.txt");
    let out_path = out_file.to_str().expect("utf8 path").to_owned();

    let res = run_clipboard_command(
        "/bin/sh",
        &["-c", "cat > \"$1\"", "_", &out_path],
        ClipboardCommandOptions {
            input: Some("café 日本語".to_owned()),
            timeout_ms: None,
            max_buffer_bytes: None,
        },
    )
    .await;

    assert_eq!(res, Some(Vec::new()));
    let written = std::fs::read_to_string(&out_file).expect("read captured stdin");
    assert_eq!(written, "café 日本語");
}

#[tokio::test(flavor = "current_thread")]
async fn times_out_without_blocking_the_event_loop() {
    let ticks = Arc::new(AtomicUsize::new(0));
    let ticks_bg = Arc::clone(&ticks);
    let stop = Arc::new(tokio::sync::Notify::new());
    let stop_bg = Arc::clone(&stop);

    let ticker = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(10));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    ticks_bg.fetch_add(1, Ordering::SeqCst);
                }
                () = stop_bg.notified() => break,
            }
        }
    });

    let result = run_clipboard_command(
        "/bin/sh",
        &["-c", "sleep 2"],
        ClipboardCommandOptions {
            input: None,
            timeout_ms: Some(150),
            max_buffer_bytes: None,
        },
    )
    .await;

    stop.notify_one();
    let _ = ticker.await;

    assert_eq!(result, None);
    assert!(
        ticks.load(Ordering::SeqCst) > 5,
        "expected event loop ticks > 5, got {}",
        ticks.load(Ordering::SeqCst)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_output_above_the_buffer_limit() {
    let result = run_clipboard_command(
        "/bin/sh",
        &["-c", "dd if=/dev/zero bs=1024 count=1 2>/dev/null"],
        ClipboardCommandOptions {
            input: None,
            timeout_ms: None,
            max_buffer_bytes: Some(16),
        },
    )
    .await;

    assert_eq!(result, None);
}
