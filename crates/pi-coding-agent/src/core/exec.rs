//! Shared command execution utilities for extensions and custom tools.
//!
//! Port of `packages/coding-agent/src/core/exec.ts`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pi_js::abort::AbortSignal;
use pi_js::text::Utf8StreamDecoder;
use tokio::io::AsyncReadExt;

use crate::utils::child_process::{SpawnProcessOptions, spawn_process};

const DEFAULT_EXEC_MAX_BUFFER: usize = 256 * 1024 * 1024;
const FORCE_KILL_DELAY_MS: u64 = 5000;

#[derive(Clone, Debug, Default)]
pub struct ExecOptions {
    pub signal: Option<AbortSignal>,
    pub timeout: Option<u64>,
    pub cwd: Option<String>,
    /// Combined stdout/stderr byte limit; exceeding it terminates the command with `truncated: true`.
    pub max_buffer: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
    pub killed: bool,
    /// Output exceeded `max_buffer`; stdout and stderr are incomplete and `code` is nonzero.
    pub truncated: bool,
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
    let max_buffer = opts.max_buffer.unwrap_or(DEFAULT_EXEC_MAX_BUFFER);
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
        Err(err) => {
            return ExecResult {
                stdout: String::new(),
                stderr: err.message().to_owned(),
                code: 1,
                killed: false,
                truncated: false,
            };
        }
    };

    let pid = proc_child.id();
    let killed = Arc::new(AtomicBool::new(false));
    let exited = Arc::new(AtomicBool::new(false));

    let kill_process: Arc<dyn Fn() + Send + Sync> = {
        let killed = killed.clone();
        let exited = exited.clone();
        Arc::new(move || {
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
                pi_js::time::set_timeout(FORCE_KILL_DELAY_MS, move || {
                    if !exited_inner.load(Ordering::Acquire) {
                        #[cfg(unix)]
                        send_sig(p, nix::sys::signal::Signal::SIGKILL);
                        #[cfg(windows)]
                        send_sig(p, true);
                    }
                });
            }
        })
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

    struct CapState {
        output_bytes: usize,
        truncated: bool,
        max_buffer: usize,
    }

    fn spawn_pipe_reader<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
        pipe: Option<R>,
        cap: Arc<std::sync::Mutex<CapState>>,
        mut rx: tokio::sync::watch::Receiver<bool>,
        tx: tokio::sync::watch::Sender<bool>,
        kill: Arc<dyn Fn() + Send + Sync>,
        shared_out: Arc<std::sync::Mutex<String>>,
    ) -> tokio::task::JoinHandle<String> {
        tokio::spawn(async move {
            let Some(mut stream) = pipe else {
                return String::new();
            };
            let mut decoder = Utf8StreamDecoder::new();
            let mut out = String::new();
            let mut buf = [0u8; 64 * 1024];
            loop {
                tokio::select! {
                    biased;
                    changed = rx.changed() => {
                        if changed.is_err() || *rx.borrow() {
                            break;
                        }
                    }
                    read_res = stream.read(&mut buf) => {
                        let Ok(n) = read_res else {
                            break;
                        };
                        if n == 0 {
                            break;
                        }
                        let (accepted_len, did_overflow) = {
                            let mut st = cap.lock().unwrap_or_else(|e| e.into_inner());
                            if st.truncated {
                                (0, true)
                            } else {
                                let remaining = st.max_buffer.saturating_sub(st.output_bytes);
                                if n <= remaining {
                                    st.output_bytes += n;
                                    (n, false)
                                } else {
                                    st.truncated = true;
                                    (remaining, true)
                                }
                            }
                        };
                        if accepted_len > 0 {
                            let decoded = decoder.decode(&buf[..accepted_len], true);
                            if !decoded.is_empty() {
                                out.push_str(&decoded);
                                *shared_out.lock().unwrap_or_else(|e| e.into_inner()) = out.clone();
                            }
                        }
                        if did_overflow {
                            let _ = tx.send(true);
                            kill();
                            break;
                        }
                    }
                }
            }
            drop(stream);
            let was_truncated = cap.lock().unwrap_or_else(|e| e.into_inner()).truncated;
            if !was_truncated {
                let flushed = decoder.decode(&[], false);
                if !flushed.is_empty() {
                    out.push_str(&flushed);
                }
            }
            *shared_out.lock().unwrap_or_else(|e| e.into_inner()) = out.clone();
            out
        })
    }

    let cap_state = Arc::new(std::sync::Mutex::new(CapState {
        output_bytes: 0,
        truncated: false,
        max_buffer,
    }));
    let (overflow_tx, overflow_rx) = tokio::sync::watch::channel(false);
    let shared_stdout = Arc::new(std::sync::Mutex::new(String::new()));
    let shared_stderr = Arc::new(std::sync::Mutex::new(String::new()));

    let stdout_task = spawn_pipe_reader(
        proc_child.stdout.take(),
        Arc::clone(&cap_state),
        overflow_rx.clone(),
        overflow_tx.clone(),
        Arc::clone(&kill_process),
        Arc::clone(&shared_stdout),
    );
    let stderr_task = spawn_pipe_reader(
        proc_child.stderr.take(),
        Arc::clone(&cap_state),
        overflow_rx,
        overflow_tx,
        Arc::clone(&kill_process),
        Arc::clone(&shared_stderr),
    );

    let exit_res = proc_child.wait().await;
    exited.store(true, Ordering::Release);
    if let Some(t) = timeout_handle {
        t.clear();
    }
    drop(_abort_guard);

    // Brief grace window matching waitForChildProcess for descendant-held pipes
    let (stdout, stderr) = tokio::select! {
        res = async { (stdout_task.await.unwrap_or_default(), stderr_task.await.unwrap_or_default()) } => res,
        () = tokio::time::sleep(Duration::from_millis(100)) => (
            shared_stdout.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            shared_stderr.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        ),
    };

    let was_killed = killed.load(Ordering::Acquire);
    let was_truncated = cap_state.lock().unwrap_or_else(|e| e.into_inner()).truncated;
    let raw_code = match exit_res {
        Ok(status) => status.code().unwrap_or(1),
        Err(_) => 1,
    };
    let code = if was_truncated && raw_code == 0 { 1 } else { raw_code };

    ExecResult {
        stdout,
        stderr,
        code,
        killed: was_killed,
        truncated: was_truncated,
    }
}
