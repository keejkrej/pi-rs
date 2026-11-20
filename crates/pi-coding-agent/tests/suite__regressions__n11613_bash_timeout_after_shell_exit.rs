//! Port of packages/coding-agent/test/suite/regressions/11613-bash-timeout-after-shell-exit.test.ts

#![cfg(unix)]

use pi_coding_agent::core::tools::bash::create_bash_tool;
use regex::Regex;
use serde_json::json;

async fn run_expecting_timeout(command: &str, timeout: f64) -> String {
    let tmp = pi_js::env::tmp_dir();
    let bash = create_bash_tool(&tmp, None);
    let err = (bash.execute)(
        "test-11613".to_string(),
        json!({ "command": command, "timeout": timeout }),
        None,
        None,
    )
    .await
    .expect_err("expected timeout error");
    err.to_string()
}

#[tokio::test]
async fn reports_the_shells_exit_code_when_background_processes_kept_writing_until_the_timeout() {
    let message = run_expecting_timeout(
        "(while true; do echo tick; sleep 0.02; done) & sleep 0.2; echo boom >&2; exit 3",
        1.0,
    )
    .await;
    assert!(message.contains("boom"), "message: {message}");
    let re = Regex::new(
        r"Command timed out after 1 seconds\. The shell exited with code 3 after 0\.\d seconds, but background processes it started kept writing output until the timeout\.$",
    )
    .unwrap();
    assert!(re.is_match(&message), "message did not match: {message}");
}

#[tokio::test]
async fn keeps_the_plain_timeout_message_when_the_shell_was_still_running_at_the_deadline() {
    let message = run_expecting_timeout("echo started; sleep 30", 0.3).await;
    let re = Regex::new(r"started\s+Command timed out after 0\.3 seconds$").unwrap();
    assert!(re.is_match(&message), "message did not match: {message}");
}
