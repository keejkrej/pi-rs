//! Port of packages/coding-agent/src/utils/child-process.ts (replaces cross-spawn via which + PATHEXT).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use indexmap::IndexMap;
use pi_js::{Error, Result};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

const EXIT_STDIO_GRACE_MS: u64 = 100;

/// Resolved executable path honoring `PATH` and Windows `PATHEXT` via `which` (PORTING.md §2.6).
pub fn resolve_command_path(command: &str, env: &IndexMap<String, String>) -> PathBuf {
    let path_var = env
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("PATH"))
        .map(|(_, v)| v.as_str());
    let cwd = pi_js::env::cwd();
    if let Some(p) = path_var {
        if let Ok(resolved) = which::which_in(command, Some(p), &cwd) {
            return resolved;
        }
    } else if let Ok(resolved) = which::which(command) {
        return resolved;
    }
    PathBuf::from(command)
}

#[derive(Clone, Debug, Default)]
pub struct SpawnProcessOptions {
    pub cwd: Option<String>,
    pub env: Option<IndexMap<String, String>>,
    pub stdin_piped: bool,
    pub stdout_piped: bool,
    pub stderr_piped: bool,
    pub windows_hide: bool,
    pub detached: bool,
}

#[derive(Clone, Debug)]
pub struct SyncProcessOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
    pub error_code: Option<String>,
}

pub fn spawn_process(command: &str, args: &[&str], options: SpawnProcessOptions) -> Result<Child> {
    let env_map = options.env.unwrap_or_else(pi_js::env::vars);
    let resolved = if pi_js::env::platform() == "win32" {
        resolve_command_path(command, &env_map)
    } else {
        PathBuf::from(command)
    };

    let mut cmd = Command::new(&resolved);
    cmd.args(args);
    cmd.env_clear().envs(&env_map);
    if let Some(ref cwd) = options.cwd {
        cmd.current_dir(cwd);
    }
    cmd.stdin(if options.stdin_piped {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(if options.stdout_piped {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stderr(if options.stderr_piped {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    #[cfg(unix)]
    if options.detached {
        cmd.process_group(0);
    }
    #[cfg(windows)]
    if options.windows_hide {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    cmd.spawn()
        .map_err(|err| Error::from_io(err, "spawn", Some(command), None))
}

pub fn spawn_process_sync(command: &str, args: &[&str], options: SpawnProcessOptions) -> SyncProcessOutput {
    let env_map = options.env.unwrap_or_else(pi_js::env::vars);
    let resolved = if pi_js::env::platform() == "win32" {
        resolve_command_path(command, &env_map)
    } else {
        PathBuf::from(command)
    };

    let mut cmd = std::process::Command::new(&resolved);
    cmd.args(args);
    cmd.env_clear().envs(&env_map);
    if let Some(ref cwd) = options.cwd {
        cmd.current_dir(cwd);
    }
    cmd.stdin(if options.stdin_piped {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(if options.stdout_piped {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stderr(if options.stderr_piped {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    #[cfg(windows)]
    if options.windows_hide {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    match cmd.output() {
        Ok(out) => SyncProcessOutput {
            status: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            error: None,
            error_code: None,
        },
        Err(err) => {
            let wrapped = Error::from_io(err, "spawn", Some(command), None);
            let code = wrapped.code().map(str::to_owned);
            SyncProcessOutput {
                status: None,
                stdout: String::new(),
                stderr: String::new(),
                error: Some(wrapped.message()),
                error_code: code,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ChildProcessExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl ChildProcessExit {
    /// Exit code, mapping a signal termination on Unix to `128 + sig`.
    #[must_use]
    pub fn into_shell_code(self) -> Option<i32> {
        self.code.or_else(|| self.signal.map(|sig| 128 + sig))
    }
}

fn exit_status_to_child_exit(status: &std::process::ExitStatus) -> ChildProcessExit {
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(status);
    #[cfg(not(unix))]
    let signal = None;
    ChildProcessExit {
        code: status.code(),
        signal,
    }
}

/// Wait for a child process to terminate without hanging on inherited stdio handles.
///
/// A short-lived child can `exit` while a detached descendant keeps its stdout/stderr
/// pipe open. After `exit`, waits for pipes to fall idle with a re-armed 100ms grace
/// timer on every received chunk.
pub async fn wait_for_child_process(
    child: Child,
    on_stdout: Option<Box<dyn FnMut(&[u8]) + Send>>,
    on_stderr: Option<Box<dyn FnMut(&[u8]) + Send>>,
) -> Result<Option<i32>> {
    wait_for_child_process_status(child, on_stdout, on_stderr)
        .await
        .map(|status| status.code)
}

/// Wait for a child process to terminate without hanging on inherited stdio handles,
/// returning both exit code and (on Unix) terminating signal.
pub async fn wait_for_child_process_status(
    mut child: Child,
    on_stdout: Option<Box<dyn FnMut(&[u8]) + Send>>,
    on_stderr: Option<Box<dyn FnMut(&[u8]) + Send>>,
) -> Result<ChildProcessExit> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(bool, Vec<u8>)>();
    let mut stdout_ended = stdout.is_none();
    let mut stderr_ended = stderr.is_none();

    async fn pump<R: AsyncRead + Unpin>(
        mut reader: R,
        is_stdout: bool,
        tx: tokio::sync::mpsc::UnboundedSender<(bool, Vec<u8>)>,
    ) {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => {
                    let _ = tx.send((is_stdout, Vec::new()));
                    break;
                }
                Ok(n) => {
                    let _ = tx.send((is_stdout, buf[..n].to_vec()));
                }
            }
        }
    }

    if let Some(out) = stdout {
        tokio::spawn(pump(out, true, tx.clone()));
    }
    if let Some(err) = stderr {
        tokio::spawn(pump(err, false, tx.clone()));
    }
    drop(tx);

    let mut on_stdout = on_stdout;
    let mut on_stderr = on_stderr;
    let mut exited = false;
    let mut exit_status = ChildProcessExit::default();
    let grace = tokio::time::sleep(Duration::from_millis(EXIT_STDIO_GRACE_MS));
    tokio::pin!(grace);

    loop {
        tokio::select! {
            status_res = child.wait(), if !exited => {
                let status = status_res.map_err(|err| Error::from_io(err, "wait", None, None))?;
                exited = true;
                exit_status = exit_status_to_child_exit(&status);
                if stdout_ended && stderr_ended {
                    return Ok(exit_status);
                }
                grace.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(EXIT_STDIO_GRACE_MS));
            }
            maybe_chunk = rx.recv(), if !(stdout_ended && stderr_ended) => {
                match maybe_chunk {
                    Some((is_stdout, chunk)) => {
                        if chunk.is_empty() {
                            if is_stdout { stdout_ended = true; } else { stderr_ended = true; }
                            if exited && stdout_ended && stderr_ended {
                                return Ok(exit_status);
                            }
                        } else {
                            if is_stdout {
                                if let Some(ref mut cb) = on_stdout { cb(&chunk); }
                            } else if let Some(ref mut cb) = on_stderr {
                                cb(&chunk);
                            }
                            if exited {
                                grace.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(EXIT_STDIO_GRACE_MS));
                            }
                        }
                    }
                    None => {
                        stdout_ended = true;
                        stderr_ended = true;
                        if exited {
                            return Ok(exit_status);
                        }
                    }
                }
            }
            () = &mut grace, if exited => {
                return Ok(exit_status);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_for_child_process_status_captures_unix_signal() {
        let child = spawn_process(
            "sh",
            &["-c", "kill -KILL $$"],
            SpawnProcessOptions {
                stdout_piped: true,
                stderr_piped: true,
                ..Default::default()
            },
        )
        .expect("spawn sh");
        let status = wait_for_child_process_status(child, None, None)
            .await
            .expect("wait status");
        assert_eq!(status.code, None);
        assert_eq!(status.signal, Some(9));
        assert_eq!(status.into_shell_code(), Some(137));
    }
}
