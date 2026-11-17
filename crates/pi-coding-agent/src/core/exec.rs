//! Port of packages/coding-agent/src/core/exec.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pi_js::abort::AbortSignal;
use tokio::io::AsyncReadExt;

use crate::utils::child_process::{SpawnProcessOptions, spawn_process};

#[derive(Clone, Debug, Default)]
pub struct ExecOptions {
    pub signal: Option<AbortSignal>,
    pub timeout: Option<u64>,
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
    pub killed: bool,
}

#[cfg(unix)]
fn send_sig(pid: u32, sig: nix::sys::signal::Signal) {
    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), sig);
}

#[cfg(windows)]
fn send_sig(pid: u32, force: bool) {
    if force {
        crate::utils::shell::kill_process_tree(pid);
    }
}

/// Execute a shell command and return stdout/stderr/code.
/// Supports timeout and abort signal.
pub async fn exec_command(command: &str, args: &[String], cwd: &str, options: Option<ExecOptions>) -> ExecResult {
    let opts = options.unwrap_or_default();
    let effective_cwd = opts.cwd.unwrap_or_else(|| cwd.to_owned());
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

    let mut proc_child = match spawn_process(
        command,
        &arg_refs,
        SpawnProcessOptions {
            cwd: Some(effective_cwd),
            stdout_piped: true,
            stderr_piped: true,
            ..Default::default()
        },
    ) {
        Ok(c) => c,
        Err(_) => {
            return ExecResult {
                stdout: String::new(),
                stderr: String::new(),
                code: 1,
                killed: false,
            };
        }
    };

    let pid = proc_child.id();
    let killed = Arc::new(AtomicBool::new(false));
    let exited = Arc::new(AtomicBool::new(false));

    let kill_process = {
        let killed = killed.clone();
        let exited = exited.clone();
        move || {
            if killed.swap(true, Ordering::AcqRel) {
                return;
            }
            if let Some(p) = pid {
                #[cfg(unix)]
                {
                    send_sig(p, nix::sys::signal::Signal::SIGTERM);
                }
                #[cfg(windows)]
                {
                    send_sig(p, false);
                }
                let exited_inner = exited.clone();
                pi_js::time::set_timeout(5000, move || {
                    if !exited_inner.load(Ordering::Acquire) {
                        #[cfg(unix)]
                        send_sig(p, nix::sys::signal::Signal::SIGKILL);
                        #[cfg(windows)]
                        send_sig(p, true);
                    }
                });
            }
        }
    };

    let mut _abort_guard = None;
    if let Some(ref sig) = opts.signal {
        if sig.aborted() {
            kill_process();
        } else {
            let k = kill_process.clone();
            _abort_guard = Some(sig.on_abort(move |_| k()));
        }
    }

    let timeout_handle = opts.timeout.filter(|&t| t > 0).map(|t| {
        let k = kill_process.clone();
        pi_js::time::set_timeout(t, move || k())
    });

    let stdout_pipe = proc_child.stdout.take();
    let stderr_pipe = proc_child.stderr.take();

    let stdout_task = tokio::spawn(async move {
        let mut out = Vec::new();
        if let Some(mut s) = stdout_pipe {
            let _ = s.read_to_end(&mut out).await;
        }
        String::from_utf8_lossy(&out).into_owned()
    });
    let stderr_task = tokio::spawn(async move {
        let mut out = Vec::new();
        if let Some(mut s) = stderr_pipe {
            let _ = s.read_to_end(&mut out).await;
        }
        String::from_utf8_lossy(&out).into_owned()
    });

    let exit_res = proc_child.wait().await;
    exited.store(true, Ordering::Release);
    if let Some(t) = timeout_handle {
        t.clear();
    }
    drop(_abort_guard);

    // Brief grace window matching waitForChildProcess for descendant-held pipes
    let (stdout, stderr) = tokio::select! {
        res = async { (stdout_task.await.unwrap_or_default(), stderr_task.await.unwrap_or_default()) } => res,
        () = tokio::time::sleep(Duration::from_millis(100)) => (String::new(), String::new()),
    };

    let was_killed = killed.load(Ordering::Acquire);
    let code = match exit_res {
        Ok(status) => status.code().unwrap_or(0),
        Err(_) => 1,
    };

    ExecResult {
        stdout,
        stderr,
        code,
        killed: was_killed,
    }
}
