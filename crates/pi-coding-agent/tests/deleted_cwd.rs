//! Port of packages/coding-agent/test/deleted-cwd.test.ts (#11509)

#![cfg(unix)]

use std::path::PathBuf;

use pi_coding_agent::main_::main;
use serial_test::serial;

struct RestoreCwdGuard(PathBuf);

impl Drop for RestoreCwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.0);
    }
}

#[tokio::test]
#[serial]
async fn prints_actionable_error_and_exits_1_when_current_working_directory_no_longer_exists() {
    let original_cwd = std::env::current_dir().expect("original cwd");
    let _restore = RestoreCwdGuard(original_cwd);

    let temp = tempfile::tempdir().expect("tempdir");
    let gone = temp.path().join("gone");
    std::fs::create_dir_all(&gone).expect("create subdir");
    std::env::set_current_dir(&gone).expect("chdir to gone");
    std::fs::remove_dir(&gone).expect("remove cwd while inside it");

    let cap = pi_js::console::testing::capture();
    let args = vec!["--no-extensions".to_string(), "--version".to_string()];
    let res = main(&args, None).await;
    let stderr = cap.stderr();

    assert!(
        matches!(res, Err(pi_js::Error::Exit(1))),
        "expected Exit(1), got {res:?}"
    );
    assert!(
        stderr.contains("The current working directory no longer exists"),
        "stderr: {stderr}"
    );
}
