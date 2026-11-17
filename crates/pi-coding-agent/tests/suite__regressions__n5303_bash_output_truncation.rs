//! Port of packages/coding-agent/test/suite/regressions/5303-bash-output-truncation.test.ts

use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_coding_agent::utils::child_process::wait_for_child_process;
use pi_coding_agent::utils::shell::kill_process_tree;
use tempfile::TempDir;
use tokio::process::Command;

#[tokio::test]
async fn captures_output_emitted_after_exit_while_descendant_holds_stdout_open() {
    if pi_js::env::platform() == "win32" {
        return;
    }

    let script = "\
        printf 'HEAD\\n'; \
        (sleep 0.04; printf 'TICK1\\n'; sleep 0.04; printf 'TICK2\\n'; sleep 0.04; printf 'TICK3\\n'; sleep 0.04; printf 'TICK4\\n') & \
        exit 0";

    let child = Command::new("/bin/sh")
        .args(["-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sh");

    let output = Arc::new(Mutex::new(String::new()));
    let output_cb = Arc::clone(&output);
    let code = tokio::time::timeout(
        Duration::from_secs(3),
        wait_for_child_process(
            child,
            Some(Box::new(move |chunk: &[u8]| {
                output_cb
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_str(&String::from_utf8_lossy(chunk));
            })),
            None,
        ),
    )
    .await
    .expect("wait_for_child_process timed out")
    .expect("wait_for_child_process ok");

    assert_eq!(code, Some(0));
    let collected = output.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert!(collected.contains("HEAD"), "{collected}");
    assert!(collected.contains("TICK4"), "missing post-exit output: {collected}");
}

#[tokio::test]
async fn resolves_after_grace_when_descendant_holds_stdout_open_and_stays_quiet() {
    if pi_js::env::platform() == "win32" {
        return;
    }

    let dir = TempDir::new().unwrap();
    let pid_file = dir.path().join("sleeper.pid");
    let script = format!(
        "printf 'DONE\\n'; (sleep 2) & echo $! > '{}'; exit 0",
        pid_file.to_string_lossy()
    );

    let child = Command::new("/bin/sh")
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sh");

    let started = pi_js::time::now_ms();
    let output = Arc::new(Mutex::new(String::new()));
    let output_cb = Arc::clone(&output);
    let code = tokio::time::timeout(
        Duration::from_millis(800),
        wait_for_child_process(
            child,
            Some(Box::new(move |chunk: &[u8]| {
                output_cb
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_str(&String::from_utf8_lossy(chunk));
            })),
            None,
        ),
    )
    .await
    .expect("grace timer did not release idle descendant stdout")
    .expect("wait_for_child_process ok");
    let elapsed = pi_js::time::now_ms() - started;

    if let Ok(txt) = std::fs::read_to_string(&pid_file)
        && let Ok(pid) = txt.trim().parse::<u32>()
    {
        kill_process_tree(pid);
    }

    assert_eq!(code, Some(0));
    assert!(elapsed < 1000, "took too long ({elapsed}ms)");
    let collected = output.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert!(collected.contains("DONE"), "{collected}");
}
