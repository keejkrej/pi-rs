//! Port of packages/coding-agent/test/experimental-internal-process.test.ts

use std::time::Duration;

use pi_coding_agent::experimental::process::{
    INTERNAL_PROCESS_EXE_ENV, InternalChildProcess, InternalProcessRole, InternalProcessSpawnOptions,
    spawn_internal_process, terminate_internal_process,
};
use serial_test::serial;
use tempfile::tempdir;
use tokio::net::UnixStream;

struct ProcessCleanup {
    children: Vec<InternalChildProcess>,
}

impl Drop for ProcessCleanup {
    fn drop(&mut self) {
        #[cfg(unix)]
        for child in &self.children {
            if !child.has_exited()
                && let Some(pid) = child.pid()
                && let Ok(raw) = i32::try_from(pid)
            {
                let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), nix::sys::signal::Signal::SIGTERM);
            }
        }
    }
}

struct ExeEnvGuard {
    original: Option<String>,
}

impl ExeEnvGuard {
    fn set_pi_bin() -> Self {
        let original = pi_js::env::var(INTERNAL_PROCESS_EXE_ENV);
        pi_js::env::set_var(INTERNAL_PROCESS_EXE_ENV, env!("CARGO_BIN_EXE_pi"));
        Self { original }
    }
}

impl Drop for ExeEnvGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(v) => pi_js::env::set_var(INTERNAL_PROCESS_EXE_ENV, v),
            None => pi_js::env::remove_var(INTERNAL_PROCESS_EXE_ENV),
        }
    }
}

async fn can_connect(path: &str) -> bool {
    UnixStream::connect(path).await.is_ok()
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn starts_the_coordinator_through_the_current_runtime() {
    let _exe = ExeEnvGuard::set_pi_bin();
    let dir = tempdir().expect("create temp dir");
    let public_path = dir.path().join("p.sock").to_string_lossy().into_owned();
    let control_path = dir.path().join("c.sock").to_string_lossy().into_owned();
    let mut cleanup = ProcessCleanup { children: Vec::new() };

    let child = spawn_internal_process(
        InternalProcessRole::Coordinator,
        &[public_path, control_path.clone()],
        &InternalProcessSpawnOptions::default(),
    )
    .expect("spawn coordinator");
    cleanup.children.push(child.clone());

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !can_connect(&control_path).await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for coordinator control socket {control_path}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    assert_ne!(child.pid(), Some(std::process::id()));
    terminate_internal_process(&child).await;
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn waits_for_a_failed_activation_child_to_terminate() {
    let _exe = ExeEnvGuard::set_pi_bin();
    let dir = tempdir().expect("create temp dir");
    let public_path = dir.path().join("p.sock").to_string_lossy().into_owned();
    let control_path = dir.path().join("c.sock").to_string_lossy().into_owned();
    let mut cleanup = ProcessCleanup { children: Vec::new() };

    let child = spawn_internal_process(
        InternalProcessRole::Coordinator,
        &[public_path, control_path],
        &InternalProcessSpawnOptions::default(),
    )
    .expect("spawn coordinator");
    cleanup.children.push(child.clone());

    assert!(child.pid().is_some());
    terminate_internal_process(&child).await;
    assert_eq!(child.signal(), Some(nix::sys::signal::Signal::SIGKILL as i32));
}
