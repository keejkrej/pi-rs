//! Port of packages/coding-agent/test/experimental-cli-entry.test.ts

use std::process::Command;

use pi_coding_agent::config::VERSION;
use pi_coding_agent::experimental::commands::run_experimental_command;
use serial_test::serial;
use tempfile::tempdir;

struct EnvRestore {
    key: &'static str,
    original: Option<String>,
}

impl EnvRestore {
    fn set(key: &'static str, value: &str) -> Self {
        let original = pi_js::env::var(key);
        pi_js::env::set_var(key, value);
        Self { key, original }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        match &self.original {
            Some(v) => pi_js::env::set_var(self.key, v),
            None => pi_js::env::remove_var(self.key),
        }
    }
}

fn run_stable_binary(experimental: bool) -> std::process::Output {
    let dir = tempdir().expect("create temp dir");
    let dir_path = dir.path();
    Command::new(env!("CARGO_BIN_EXE_pi"))
        .args(["server", "--server-id", "invalid", "--version"])
        .current_dir(dir_path)
        .env("HOME", dir_path)
        .env("USERPROFILE", dir_path)
        .env("PI_CODING_AGENT_DIR", dir_path.join("agent"))
        .env("PI_OFFLINE", "1")
        .env("PI_EXPERIMENTAL", if experimental { "1" } else { "0" })
        .output()
        .expect("spawn pi binary")
}

#[test]
fn does_not_dispatch_experimental_commands_from_the_stable_entrypoint() {
    let output = run_stable_binary(true);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(stdout.trim(), *VERSION);
}

#[tokio::test]
#[serial]
async fn keeps_experimental_dispatch_in_the_development_entrypoint() {
    let _exp = EnvRestore::set("PI_EXPERIMENTAL", "1");
    let args = vec![
        "server".to_string(),
        "--server-id".to_string(),
        "invalid".to_string(),
        "--version".to_string(),
    ];
    let code = run_experimental_command(&args).await;
    assert_eq!(code, Some(1));
}

#[tokio::test]
#[serial]
async fn falls_back_to_the_stable_cli_when_experiments_are_disabled() {
    let _exp = EnvRestore::set("PI_EXPERIMENTAL", "0");
    let args = vec![
        "server".to_string(),
        "--server-id".to_string(),
        "invalid".to_string(),
        "--version".to_string(),
    ];
    assert_eq!(run_experimental_command(&args).await, None);
    let output = run_stable_binary(false);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(stdout.trim(), *VERSION);
}
