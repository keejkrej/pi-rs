//! Port of packages/coding-agent/src/utils/shell.ts

use std::sync::{Arc, LazyLock, Mutex};

use indexmap::{IndexMap, IndexSet};
use pi_js::seam::{Guard, Slot};
use pi_js::{Error, Result};

use crate::config::get_bin_dir;

pub type TaskkillSpawnFn = Arc<dyn Fn(&str, &[String]) -> std::io::Result<()> + Send + Sync>;
pub type ShellPathExistsFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;
pub type FindExecutableFn = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
pub type UnixSignalFn = Arc<dyn Fn(i32, &str) -> std::result::Result<(), ()> + Send + Sync>;

static TASKKILL_SPAWN_SLOT: LazyLock<Slot<TaskkillSpawnFn>> = LazyLock::new(Slot::new);
static SHELL_PATH_EXISTS_SLOT: LazyLock<Slot<ShellPathExistsFn>> = LazyLock::new(Slot::new);
static FIND_EXECUTABLE_SLOT: LazyLock<Slot<FindExecutableFn>> = LazyLock::new(Slot::new);
static UNIX_SIGNAL_SLOT: LazyLock<Slot<UnixSignalFn>> = LazyLock::new(Slot::new);

#[doc(hidden)]
pub mod testing {
    use super::*;

    pub fn override_taskkill_spawn(func: TaskkillSpawnFn) -> Guard {
        TASKKILL_SPAWN_SLOT.set(Arc::new(func))
    }

    pub fn override_shell_path_exists(func: ShellPathExistsFn) -> Guard {
        SHELL_PATH_EXISTS_SLOT.set(Arc::new(func))
    }

    pub fn override_find_executable(func: FindExecutableFn) -> Guard {
        FIND_EXECUTABLE_SLOT.set(Arc::new(func))
    }

    pub fn override_unix_signal(func: UnixSignalFn) -> Guard {
        UNIX_SIGNAL_SLOT.set(Arc::new(func))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandTransport {
    Argv,
    Stdin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellConfig {
    pub shell: String,
    pub args: Vec<String>,
    pub command_transport: Option<CommandTransport>,
}

static TRACKED_DETACHED_CHILD_PIDS: LazyLock<Mutex<IndexSet<u32>>> = LazyLock::new(|| Mutex::new(IndexSet::new()));

fn is_legacy_wsl_bash_path(path: &str) -> bool {
    let normalized = pi_js::path::win32::normalize(path).to_ascii_lowercase();
    if let Some(system_root) = pi_js::env::var("SystemRoot").filter(|s| !s.is_empty()) {
        let bytes = system_root.as_bytes();
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || (bytes[2] != b'\\' && bytes[2] != b'/')
        {
            return false;
        }
        return ["System32", "Sysnative"].iter().any(|directory| {
            normalized == pi_js::path::win32::join(&[&system_root, directory, "bash.exe"]).to_ascii_lowercase()
        });
    }
    // Fall back to conventional launcher paths only when the Windows directory is unknown.
    let bytes = normalized.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && &bytes[1..3] == b":\\"
        && (normalized[3..].eq("windows\\system32\\bash.exe") || normalized[3..].eq("windows\\sysnative\\bash.exe"))
}

fn shell_path_exists(path: &str) -> bool {
    if let Some(hook) = SHELL_PATH_EXISTS_SLOT.get() {
        return hook(path);
    }
    pi_js::fs::access(path, 0).is_ok()
}

fn get_bash_shell_config(shell: &str) -> ShellConfig {
    if is_legacy_wsl_bash_path(shell) {
        ShellConfig {
            shell: shell.to_owned(),
            args: vec!["-s".to_owned()],
            command_transport: Some(CommandTransport::Stdin),
        }
    } else {
        ShellConfig {
            shell: shell.to_owned(),
            args: vec!["-c".to_owned()],
            command_transport: None,
        }
    }
}

fn find_executable_on_path(executable: &str) -> Option<String> {
    if let Some(hook) = FIND_EXECUTABLE_SLOT.get() {
        return hook(executable);
    }
    let vars = pi_js::env::vars();
    let path_var = vars
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("PATH"))
        .map(|(_, v)| v.as_str());
    let cwd = pi_js::env::cwd();
    which::which_in(executable, path_var, &cwd)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Resolve shell configuration based on platform and an optional explicit shell path.
/// Resolution order:
/// 1. User-specified `shell_path`
/// 2. On Windows: Git Bash in known locations, then bash on PATH
/// 3. On Unix: `/bin/bash`, then bash on PATH, then fallback to `sh`
pub fn get_shell_config(custom_shell_path: Option<&str>) -> Result<ShellConfig> {
    if let Some(custom) = custom_shell_path.filter(|s| !s.is_empty()) {
        if shell_path_exists(custom) {
            return Ok(get_bash_shell_config(custom));
        }
        return Err(Error::msg(format!("Custom shell path not found: {custom}")));
    }

    if pi_js::env::platform() == "win32" {
        let mut paths = Vec::new();
        if let Some(program_files) = pi_js::env::var("ProgramFiles").filter(|s| !s.is_empty()) {
            paths.push(format!("{program_files}\\Git\\bin\\bash.exe"));
        }
        if let Some(program_files_x86) = pi_js::env::var("ProgramFiles(x86)").filter(|s| !s.is_empty()) {
            paths.push(format!("{program_files_x86}\\Git\\bin\\bash.exe"));
        }

        for path in &paths {
            if shell_path_exists(path) {
                return Ok(get_bash_shell_config(path));
            }
        }

        if let Some(bash_on_path) = find_executable_on_path("bash.exe") {
            return Ok(get_bash_shell_config(&bash_on_path));
        }

        let searched = paths.iter().map(|p| format!("  {p}")).collect::<Vec<_>>().join("\n");
        return Err(Error::msg(format!(
            "No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Set shellPath in settings.json\n\nSearched Git Bash in:\n{searched}"
        )));
    }

    if shell_path_exists("/bin/bash") {
        return Ok(get_bash_shell_config("/bin/bash"));
    }

    if let Some(bash_on_path) = find_executable_on_path("bash") {
        return Ok(get_bash_shell_config(&bash_on_path));
    }

    Ok(ShellConfig {
        shell: "sh".to_owned(),
        args: vec!["-c".to_owned()],
        command_transport: None,
    })
}

pub const POWERSHELL_ARGS: &[&str] = &[
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy",
    "Bypass",
    "-Command",
];

/// Resolve PowerShell on Windows, preferring PowerShell 7 when available.
pub fn get_power_shell_config() -> Result<ShellConfig> {
    if pi_js::env::platform() != "win32" {
        return Err(Error::msg("The powershell tool is only available on Windows."));
    }

    let mut shell = find_executable_on_path("pwsh.exe");
    // Installation directories may be on any drive and need not be on PATH.
    if shell.is_none()
        && let Some(program_files) = pi_js::env::var("ProgramFiles").filter(|s| !s.is_empty())
    {
        let pwsh7 = pi_js::path::win32::join(&[&program_files, "PowerShell", "7", "pwsh.exe"]);
        if shell_path_exists(&pwsh7) {
            shell = Some(pwsh7);
        }
    }

    if shell.is_none() {
        shell = find_executable_on_path("powershell.exe");
    }
    if shell.is_none()
        && let Some(system_root) = pi_js::env::var("SystemRoot").filter(|s| !s.is_empty())
    {
        let windows_ps =
            pi_js::path::win32::join(&[&system_root, "System32", "WindowsPowerShell", "v1.0", "powershell.exe"]);
        if shell_path_exists(&windows_ps) {
            shell = Some(windows_ps);
        }
    }

    let Some(shell) = shell else {
        return Err(Error::msg(
            "No PowerShell executable found. Install PowerShell or add powershell.exe/pwsh.exe to PATH.",
        ));
    };

    Ok(ShellConfig {
        shell,
        args: POWERSHELL_ARGS.iter().map(|s| (*s).to_owned()).collect(),
        command_transport: None,
    })
}

pub fn get_shell_env() -> IndexMap<String, String> {
    let bin_dir = get_bin_dir();
    let mut env = pi_js::env::vars();
    let path_key = env
        .keys()
        .find(|key| key.eq_ignore_ascii_case("path"))
        .cloned()
        .unwrap_or_else(|| "PATH".to_owned());
    let current_path = env.get(&path_key).cloned().unwrap_or_default();
    let delim = pi_js::path::delimiter();
    let has_bin_dir = current_path.split(delim).any(|entry| entry == bin_dir);
    let updated_path = if has_bin_dir {
        current_path
    } else if current_path.is_empty() {
        bin_dir
    } else {
        format!("{bin_dir}{delim}{current_path}")
    };
    env.insert(path_key, updated_path);
    env
}

/// Sanitize binary output for display/storage.
/// Removes characters that crash string-width or cause display issues:
/// - Control characters (except tab, newline, carriage return)
/// - Unicode interlinear annotation characters U+FFF9..U+FFFB
pub fn sanitize_binary_output(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            let code = c as u32;
            !((code <= 0x08)
                || code == 0x0B
                || code == 0x0C
                || (0x0E..=0x1F).contains(&code)
                || (0xFFF9..=0xFFFB).contains(&code))
        })
        .collect()
}

pub fn track_detached_child_pid(pid: u32) {
    if let Ok(mut guard) = TRACKED_DETACHED_CHILD_PIDS.lock() {
        guard.insert(pid);
    }
}

pub fn untrack_detached_child_pid(pid: u32) {
    if let Ok(mut guard) = TRACKED_DETACHED_CHILD_PIDS.lock() {
        guard.shift_remove(&pid);
    }
}

pub fn kill_tracked_detached_children() {
    let pids: Vec<u32> = match TRACKED_DETACHED_CHILD_PIDS.lock() {
        Ok(mut guard) => {
            let list: Vec<u32> = guard.iter().copied().collect();
            guard.clear();
            list
        }
        Err(_) => return,
    };
    for pid in pids {
        kill_process_tree(pid);
    }
}

/// Kill a process and all its children (cross-platform).
pub fn kill_process_tree(pid: u32) {
    if pi_js::env::platform() == "win32" {
        let sys_root = pi_js::env::var("SystemRoot")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "C:\\Windows".to_owned());
        let taskkill = pi_js::path::win32::join(&[&sys_root, "System32", "taskkill.exe"]);
        let args = vec!["/F".to_owned(), "/T".to_owned(), "/PID".to_owned(), pid.to_string()];
        if let Some(hook) = TASKKILL_SPAWN_SLOT.get() {
            let _ = hook(&taskkill, &args);
            return;
        }
        let mut cmd = std::process::Command::new(taskkill);
        cmd.args(&args)
            .env_clear()
            .envs(pi_js::env::vars())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let _ = cmd.spawn();
    } else {
        let raw_pid = pid as i32;
        if let Some(hook) = UNIX_SIGNAL_SLOT.get() {
            if pi_js::env::platform() == "darwin" {
                let _ = hook(-raw_pid, "SIGSTOP");
            }
            if hook(-raw_pid, "SIGKILL").is_err() {
                let _ = hook(raw_pid, "SIGKILL");
            }
            return;
        }
        #[cfg(unix)]
        {
            use nix::sys::signal::{Signal, killpg};
            use nix::unistd::Pid;
            if pi_js::env::platform() == "darwin" {
                // macOS signals group members individually after releasing the group lock; stop first to reduce
                // the race where a killed child wakes a waiting shell (`a; b` starting `b` during cancel, #11252).
                let _ = killpg(Pid::from_raw(raw_pid), Signal::SIGSTOP);
            }
            if killpg(Pid::from_raw(raw_pid), Signal::SIGKILL).is_err() {
                let _ = nix::sys::signal::kill(Pid::from_raw(raw_pid), Signal::SIGKILL);
            }
        }
    }
}
