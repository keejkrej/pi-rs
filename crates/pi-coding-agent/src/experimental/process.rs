//! Port of packages/coding-agent/src/experimental/process.ts

use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use indexmap::IndexMap;
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::watch;

pub const INTERNAL_PROCESS_ENV: &str = "__PI_INTERNAL_SPAWN";
pub const INTERNAL_PROCESS_EXE_ENV: &str = "PI_INTERNAL_PROCESS_EXE";
pub const MAX_CONTROL_LINE_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InternalProcessRole {
    Coordinator,
    Server,
    SessionWorker,
}

impl InternalProcessRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Coordinator => "coordinator",
            Self::Server => "server",
            Self::SessionWorker => "session-worker",
        }
    }

    pub fn parse(role: &str) -> pi_js::Result<Self> {
        match role {
            "coordinator" => Ok(Self::Coordinator),
            "server" => Ok(Self::Server),
            "session-worker" => Ok(Self::SessionWorker),
            other => Err(pi_js::Error::msg(format!("Unsupported internal process role: {other}"))),
        }
    }
}

/// Read and validate an internal process role without consuming it.
pub fn get_internal_process_role() -> pi_js::Result<Option<InternalProcessRole>> {
    match pi_js::env::var(INTERNAL_PROCESS_ENV) {
        None => Ok(None),
        Some(role) => Ok(Some(InternalProcessRole::parse(&role)?)),
    }
}

/// Read, validate, and remove the role so descendants do not inherit it.
pub fn consume_internal_process_role() -> pi_js::Result<Option<InternalProcessRole>> {
    let role = get_internal_process_role()?;
    pi_js::env::remove_var(INTERNAL_PROCESS_ENV);
    Ok(role)
}

#[derive(Clone, Debug, Default)]
pub struct InternalProcessSpawnOptions {
    pub entry_path: Option<String>,
    pub entry_url: Option<url::Url>,
    pub env: Option<IndexMap<String, String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitOutcome {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl From<ExitStatus> for ExitOutcome {
    fn from(status: ExitStatus) -> Self {
        #[cfg(unix)]
        let signal = std::os::unix::process::ExitStatusExt::signal(&status);
        #[cfg(not(unix))]
        let signal = None;
        Self {
            code: status.code(),
            signal,
        }
    }
}

/// Handle to a spawned internal Pi child process.
#[derive(Clone)]
pub struct InternalChildProcess {
    inner: Arc<InternalChildProcessInner>,
}

struct InternalChildProcessInner {
    pid: Option<u32>,
    exit_rx: watch::Receiver<Option<ExitOutcome>>,
}

impl InternalChildProcess {
    pub fn pid(&self) -> Option<u32> {
        self.inner.pid
    }

    pub fn outcome(&self) -> Option<ExitOutcome> {
        *self.inner.exit_rx.borrow()
    }

    pub fn has_exited(&self) -> bool {
        self.inner.exit_rx.borrow().is_some()
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.inner.exit_rx.borrow().and_then(|o| o.code)
    }

    pub fn signal(&self) -> Option<i32> {
        self.inner.exit_rx.borrow().and_then(|o| o.signal)
    }

    pub async fn wait(&self) -> Option<ExitOutcome> {
        let mut rx = self.inner.exit_rx.clone();
        if let Some(outcome) = *rx.borrow() {
            return Some(outcome);
        }
        while rx.changed().await.is_ok() {
            if let Some(outcome) = *rx.borrow() {
                return Some(outcome);
            }
        }
        None
    }
}

pub type InternalProcessSpawnHook = Arc<
    dyn Fn(InternalProcessRole, &[String], &InternalProcessSpawnOptions) -> pi_js::Result<InternalChildProcess>
        + Send
        + Sync,
>;

static SPAWN_HOOK: Mutex<Option<InternalProcessSpawnHook>> = Mutex::new(None);

fn lock_hook() -> MutexGuard<'static, Option<InternalProcessSpawnHook>> {
    SPAWN_HOOK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Test/integration hook mirroring vitest spying on `spawnInternalProcess`.
pub fn set_internal_process_spawn_hook(hook: Option<InternalProcessSpawnHook>) -> Option<InternalProcessSpawnHook> {
    let mut guard = lock_hook();
    std::mem::replace(&mut *guard, hook)
}

fn resolve_executable(options: &InternalProcessSpawnOptions) -> pi_js::Result<PathBuf> {
    if let Some(ref entry_path) = options.entry_path {
        return Ok(PathBuf::from(entry_path));
    }
    if let Some(ref url) = options.entry_url {
        return url
            .to_file_path()
            .map_err(|()| pi_js::Error::msg(format!("Invalid entry file URL: {url}")));
    }
    if let Some(exe) = pi_js::env::var(INTERNAL_PROCESS_EXE_ENV) {
        return Ok(PathBuf::from(exe));
    }
    let current = std::env::current_exe().map_err(|err| pi_js::Error::from_io(err, "current_exe", None, None))?;
    if let Some(parent) = current.parent()
        && parent.file_name().is_some_and(|name| name == "deps")
        && let Some(target_dir) = parent.parent()
    {
        #[cfg(windows)]
        let candidate = target_dir.join("pi.exe");
        #[cfg(not(windows))]
        let candidate = target_dir.join("pi");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Ok(current)
}

/// Spawn a detached Pi-owned process consistently with role dispatch.
pub fn spawn_internal_process(
    role: InternalProcessRole,
    args: &[String],
    options: &InternalProcessSpawnOptions,
) -> pi_js::Result<InternalChildProcess> {
    if let Some(hook) = lock_hook().clone() {
        return hook(role, args, options);
    }
    spawn_internal_process_direct(role, args, options)
}

/// Direct spawn implementation bypassing any installed test hook.
pub fn spawn_internal_process_direct(
    role: InternalProcessRole,
    args: &[String],
    options: &InternalProcessSpawnOptions,
) -> pi_js::Result<InternalChildProcess> {
    let exe = resolve_executable(options)?;
    let mut cmd = Command::new(&exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    if let Ok(cwd) = std::env::current_dir() {
        cmd.current_dir(cwd);
    }
    for (k, v) in pi_js::env::vars() {
        cmd.env(k, v);
    }
    if let Some(ref env_overrides) = options.env {
        for (k, v) in env_overrides {
            cmd.env(k, v);
        }
    }
    cmd.env(INTERNAL_PROCESS_ENV, role.as_str());

    #[cfg(unix)]
    {
        std::os::unix::process::CommandExt::process_group(cmd.as_std_mut(), 0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.as_std_mut().creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }

    let mut child = cmd
        .spawn()
        .map_err(|err| pi_js::Error::from_io(err, "spawnInternalProcess", None, None))?;
    let pid = child.id();
    let (exit_tx, exit_rx) = watch::channel(None);
    tokio::spawn(async move {
        let status = child.wait().await.ok().map(ExitOutcome::from);
        let _ = exit_tx.send(status.or(Some(ExitOutcome {
            code: None,
            signal: None,
        })));
    });

    Ok(InternalChildProcess {
        inner: Arc::new(InternalChildProcessInner { pid, exit_rx }),
    })
}

/// Force a spawned internal process to exit and wait until it can no longer take ownership.
pub async fn terminate_internal_process(child: &InternalChildProcess) {
    if child.has_exited() {
        return;
    }
    let Some(pid) = child.pid() else {
        return;
    };
    #[cfg(unix)]
    {
        if let Ok(raw) = i32::try_from(pid) {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), nix::sys::signal::Signal::SIGKILL);
        }
    }
    let _ = child.wait().await;
}

pub fn encode_control_line(message: &Value) -> pi_js::Result<String> {
    let mut line = pi_js::json::stringify(message);
    line.push('\n');
    if line.len() > MAX_CONTROL_LINE_BYTES {
        return Err(pi_js::Error::msg("Internal control message is too large"));
    }
    Ok(line)
}
