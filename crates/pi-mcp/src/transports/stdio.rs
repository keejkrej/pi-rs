//! Port of packages/mcp/src/transports/stdio.ts

#![allow(dead_code, unused_variables)]

use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use pi_js::{Error, Unsubscribe};

use crate::protocol::jsonrpc::{JsonRpcMessage, McpConnectionClosedError};
use crate::transports::lines::{JsonRpcLineReader, JsonRpcLineReaderOptions};
use crate::transports::transport::{
    McpTransport, McpTransportCloseListener, McpTransportErrorListener, McpTransportMessageListener, TransportEvents,
};

const DEFAULT_CLOSE_TIMEOUT_MS: i64 = 2_000;
/// How long a server gets to exit on its own after stdin closes, before it is sent SIGTERM.
const STDIN_CLOSE_GRACE_MS: i64 = 500;

/// Process groups of running servers, killed if the host exits without closing them.
static LIVE_PROCESS_GROUPS: LazyLock<Mutex<IndexSet<u32>>> = LazyLock::new(|| Mutex::new(IndexSet::new()));
static EXIT_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// `process.platform !== "win32"`. A function, not a const, so tests can override the platform.
fn use_process_groups() -> bool {
    pi_js::env::platform() != "win32"
}

fn kill_process_tree_pid(pid: u32, exited: bool, signal: &str) {
    #[cfg(unix)]
    {
        let sig = match signal {
            "SIGKILL" => nix::sys::signal::Signal::SIGKILL,
            _ => nix::sys::signal::Signal::SIGTERM,
        };
        let raw_pid = nix::unistd::Pid::from_raw(pid as i32);
        if use_process_groups() && nix::sys::signal::killpg(raw_pid, sig).is_ok() {
            return;
        }
        if exited {
            return;
        }
        let _ = nix::sys::signal::kill(raw_pid, sig);
    }
    #[cfg(windows)]
    {
        let _ = signal;
        if exited {
            return;
        }
        let pid_str = pid.to_string();
        let _ = std::process::Command::new("taskkill")
            .args(["/pid", &pid_str, "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

#[allow(dead_code)]
fn kill_process_tree(child: &mut tokio::process::Child, signal: &str) {
    let Some(pid) = child.id() else {
        return;
    };
    let exited = child.try_wait().ok().flatten().is_some();
    kill_process_tree_pid(pid, exited, signal);
}

fn install_exit_hook() {
    if EXIT_HOOK_INSTALLED.load(std::sync::atomic::Ordering::SeqCst) || !use_process_groups() {
        return;
    }
    EXIT_HOOK_INSTALLED.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StdioStderr {
    #[serde(rename = "pipe")]
    Pipe,
    #[serde(rename = "inherit")]
    Inherit,
}

#[derive(Clone)]
pub struct StdioTransportOptions {
    pub command: String,
    pub args: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub env: Option<IndexMap<String, String>>,
    pub inherit_env: Option<bool>,
    pub stderr: Option<StdioStderr>,
    pub on_stderr: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub max_message_bytes: Option<i64>,
    pub max_stderr_bytes: Option<i64>,
    /// Time to wait for the server to exit after SIGTERM before sending SIGKILL. Default: 2000.
    pub close_timeout_ms: Option<i64>,
}

struct StdioTransportState {
    pid: Option<u32>,
    process_exited: bool,
    exited: bool,
    started: bool,
    closed: bool,
}

struct StdioTransportInner {
    options: StdioTransportOptions,
    events: TransportEvents,
    lines: JsonRpcLineReader,
    state: Mutex<StdioTransportState>,
    stdin: tokio::sync::Mutex<Option<tokio::process::ChildStdin>>,
    exit_notify: tokio::sync::Notify,
}

#[derive(Clone)]
pub struct StdioTransport {
    inner: Arc<StdioTransportInner>,
}

impl StdioTransport {
    pub fn new(options: StdioTransportOptions) -> Self {
        let events = TransportEvents::new();
        let events_msg = events.clone();
        let events_err = events.clone();
        let lines = JsonRpcLineReader::new(JsonRpcLineReaderOptions {
            max_message_bytes: options.max_message_bytes,
            max_stderr_bytes: options.max_stderr_bytes,
            on_stderr: options.on_stderr.clone(),
            on_message: Arc::new(move |message| events_msg.emit_message(message)),
            on_error: Arc::new(move |error| events_err.emit_error(error)),
        });
        Self {
            inner: Arc::new(StdioTransportInner {
                options,
                events,
                lines,
                state: Mutex::new(StdioTransportState {
                    pid: None,
                    process_exited: false,
                    exited: false,
                    started: false,
                    closed: false,
                }),
                stdin: tokio::sync::Mutex::new(None),
                exit_notify: tokio::sync::Notify::new(),
            }),
        }
    }

    pub fn pid(&self) -> Option<u32> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).pid
    }

    pub fn stderr(&self) -> String {
        self.inner.lines.stderr()
    }

    pub fn options(&self) -> &StdioTransportOptions {
        &self.inner.options
    }

    fn close_timeout_ms(&self) -> i64 {
        self.inner
            .options
            .close_timeout_ms
            .unwrap_or(DEFAULT_CLOSE_TIMEOUT_MS)
            .max(0)
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn start(&self) -> pi_js::Result<()> {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.started {
                return Err(Error::msg("Stdio transport already started"));
            }
            if state.closed {
                return Err(Error::typed(
                    "McpConnectionClosedError",
                    McpConnectionClosedError::new(None),
                ));
            }
            state.started = true;
        }

        let mut env: IndexMap<String, String> = if self.inner.options.inherit_env == Some(false) {
            IndexMap::new()
        } else {
            pi_js::env::vars()
        };
        if let Some(extra) = &self.inner.options.env {
            for (k, v) in extra {
                env.insert(k.clone(), v.clone());
            }
        }

        let cwd_for_which = self.inner.options.cwd.as_deref().unwrap_or(".");
        let program = if !self.inner.options.command.contains('/') && !self.inner.options.command.contains('\\') {
            which::which_in(
                &self.inner.options.command,
                env.get("PATH").map(String::as_str),
                cwd_for_which,
            )
            .unwrap_or_else(|_| std::path::PathBuf::from(&self.inner.options.command))
        } else {
            std::path::PathBuf::from(&self.inner.options.command)
        };

        let mut cmd = tokio::process::Command::new(&program);
        cmd.args(self.inner.options.args.as_deref().unwrap_or(&[]));
        if let Some(cwd) = &self.inner.options.cwd {
            cmd.current_dir(cwd);
        }
        cmd.env_clear();
        cmd.envs(&env);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        match self.inner.options.stderr {
            Some(StdioStderr::Inherit) => {
                cmd.stderr(Stdio::inherit());
            }
            _ => {
                cmd.stderr(Stdio::piped());
            }
        }
        #[cfg(unix)]
        if use_process_groups() {
            cmd.process_group(0);
        }
        cmd.kill_on_drop(false);

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(io_err) => {
                let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                state.closed = true;
                return Err(Error::from_io(io_err, "spawn", Some(&self.inner.options.command), None));
            }
        };

        let pid = child.id();
        if let Some(pid) = pid
            && use_process_groups()
        {
            install_exit_hook();
            LIVE_PROCESS_GROUPS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(pid);
        }

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.pid = pid;
        }
        *self.inner.stdin.lock().await = stdin;

        let stdout_task = stdout.map(|mut out| {
            let this = self.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 8192];
                loop {
                    match out.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => this.inner.lines.read_stdout(&buf[..n]),
                    }
                }
            })
        });

        let stderr_task = stderr.map(|mut err| {
            let this = self.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 8192];
                loop {
                    match err.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => this.inner.lines.read_stderr(&buf[..n]),
                    }
                }
            })
        });

        let this = self.clone();
        tokio::spawn(async move {
            let _ = child.wait().await;
            let is_closed = {
                let mut state = this.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                state.process_exited = true;
                state.closed
            };
            if let Some(pid) = pid
                && is_closed
                && use_process_groups()
            {
                kill_process_tree_pid(pid, true, "SIGTERM");
            }
            if let Some(task) = stdout_task {
                let _ = task.await;
            }
            if let Some(task) = stderr_task {
                let _ = task.await;
            }
            // Not on wait/exit: children of the server can outlive it while still holding its pipes.
            if let Some(pid) = pid {
                LIVE_PROCESS_GROUPS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .shift_remove(&pid);
            }
            {
                let mut state = this.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                state.exited = true;
                state.closed = true;
            }
            let _ = this.inner.stdin.lock().await.take();
            this.inner.lines.end();
            this.inner.events.emit_close();
            this.inner.exit_notify.notify_waiters();
        });

        Ok(())
    }

    async fn send(&self, message: JsonRpcMessage) -> pi_js::Result<()> {
        {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.started || state.closed || state.exited {
                return Err(Error::typed(
                    "McpConnectionClosedError",
                    McpConnectionClosedError::new(None),
                ));
            }
        }
        let line = format!("{}\n", pi_js::json::stringify(&message));
        let mut stdin_guard = self.inner.stdin.lock().await;
        let Some(stdin) = stdin_guard.as_mut() else {
            return Err(Error::typed(
                "McpConnectionClosedError",
                McpConnectionClosedError::new(None),
            ));
        };
        if let Err(io_err) = stdin.write_all(line.as_bytes()).await {
            return Err(Error::from_io(io_err, "write", None, None));
        }
        if let Err(io_err) = stdin.flush().await {
            return Err(Error::from_io(io_err, "write", None, None));
        }
        Ok(())
    }

    async fn close(&self) -> pi_js::Result<()> {
        let notified = self.inner.exit_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let (pid, exited, process_exited) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed {
                return Ok(());
            }
            state.closed = true;
            (state.pid, state.exited, state.process_exited)
        };
        let _ = self.inner.stdin.lock().await.take();
        // With process groups, a server that already exited can leave children holding its pipes; the
        // group cleanup below still has to run for them.
        if pid.is_none() || exited || (!use_process_groups() && process_exited) {
            self.inner.events.emit_close();
            return Ok(());
        }
        let pid = pid.unwrap_or(0);
        let close_timeout_ms = self.close_timeout_ms();
        let stdin_grace_ms = STDIN_CLOSE_GRACE_MS.min(close_timeout_ms);

        let exited_during_grace = tokio::select! {
            () = &mut notified => true,
            _ = pi_js::time::sleep(stdin_grace_ms as u64, None) => false,
        };
        if !exited_during_grace {
            let exited_now = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .process_exited;
            kill_process_tree_pid(pid, exited_now, "SIGTERM");
            let exited_after_term = tokio::select! {
                () = &mut notified => true,
                _ = pi_js::time::sleep(close_timeout_ms as u64, None) => false,
            };
            if !exited_after_term {
                let exited_now = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .process_exited;
                kill_process_tree_pid(pid, exited_now, "SIGKILL");
                let _ = tokio::time::timeout(Duration::from_secs(2), &mut notified).await;
            }
        }
        Ok(())
    }

    fn on_message(&self, listener: McpTransportMessageListener) -> Unsubscribe {
        self.inner.events.on_message(listener)
    }

    fn on_error(&self, listener: McpTransportErrorListener) -> Unsubscribe {
        self.inner.events.on_error(listener)
    }

    fn on_close(&self, listener: McpTransportCloseListener) -> Unsubscribe {
        self.inner.events.on_close(listener)
    }
}
