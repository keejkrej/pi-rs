//! Port of packages/durable/src/env/node.ts

use std::collections::VecDeque;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use pi_chord::Context;
use pi_js::abort::AbortSignal;
use pi_js::error::Error as JsError;
use pi_js::text::Utf8StreamDecoder;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::time::Sleep;
use uuid::Uuid;

use super::{
    CreateDirOptions, CreateTempFileOptions, EnvCleanup, ExecutionEnv, ExecutionError,
    ExecutionErrorCode, FileContent, FileError, FileErrorCode, FileInfo, FileKind, FileSystem,
    ReadTextLinesOptions, RemoveOptions, Shell, ShellExecOptions, ShellExecResult, TextLine,
    TextLineReader,
};

const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;
const MAX_TIMEOUT_SECONDS: f64 = MAX_TIMEOUT_MS / 1000.0;
const EXIT_STDIO_GRACE_MS: u64 = 100;
const SPILL_HIGH_WATER_MARK: usize = 1024 * 1024;
const LINE_READER_CHUNK_SIZE: usize = 64 * 1024;

static LEGACY_WSL_BASH_RE: LazyLock<regress::Regex> = LazyLock::new(|| {
    pi_js::regex::ecma(r"^[a-z]:\\windows\\(?:system32|sysnative)\\bash\.exe$", "")
        .unwrap_or_else(|e| panic!("invalid static legacy WSL regex: {e}"))
});

pub type TempFileHook = Arc<
    dyn Fn(Option<&CreateTempFileOptions>) -> Option<std::result::Result<String, FileError>>
        + Send
        + Sync,
>;

#[derive(Clone, Default)]
pub struct SpillTestConfig {
    pub high_water_mark: Option<usize>,
    pub write_delay_ms: Option<u64>,
    pub rejected_writes: Option<Arc<AtomicUsize>>,
}

#[derive(Debug, Clone, Default)]
pub struct NodeExecutionEnvOptions {
    pub cwd: String,
    pub shell_path: Option<String>,
    pub shell_env: Option<IndexMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CommandTransport {
    Argv,
    Stdin,
}

#[derive(Debug, Clone)]
struct ShellConfig {
    shell: String,
    args: Vec<String>,
    command_transport: CommandTransport,
}

fn resolve_timeout_ms(timeout: Option<f64>) -> std::result::Result<Option<u64>, ExecutionError> {
    let Some(timeout) = timeout else {
        return Ok(None);
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(ExecutionError::new(
            ExecutionErrorCode::Timeout,
            "Invalid timeout: must be a finite number of seconds",
        ));
    }
    let timeout_ms = timeout * 1000.0;
    if timeout_ms > MAX_TIMEOUT_MS {
        return Err(ExecutionError::new(
            ExecutionErrorCode::Timeout,
            format!("Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"),
        ));
    }
    Ok(Some(timeout_ms.round().max(1.0) as u64))
}

fn resolve_path(cwd: &str, path: &str) -> String {
    let mut normalized = path.to_string();
    if normalized == "~" {
        normalized = pi_js::env::home_dir();
    } else if normalized.starts_with("~/")
        || (pi_js::env::platform() == "win32" && normalized.starts_with("~\\"))
    {
        normalized = pi_js::path::join(&[&pi_js::env::home_dir(), &normalized[2..]]);
    } else if normalized.starts_with("file://") {
        if let Ok(parsed) = url::Url::parse(&normalized)
            && let Ok(file_path) = parsed.to_file_path()
        {
            normalized = file_path.to_string_lossy().into_owned();
        }
    }
    if pi_js::path::is_absolute(&normalized) {
        pi_js::path::resolve(&[&normalized])
    } else {
        pi_js::path::resolve(&[cwd, &normalized])
    }
}

fn file_kind_from_stats(stats: &pi_js::fs::Stats) -> Option<FileKind> {
    if stats.is_file() {
        Some(FileKind::File)
    } else if stats.is_directory() {
        Some(FileKind::Directory)
    } else if stats.is_symbolic_link() {
        Some(FileKind::Symlink)
    } else {
        None
    }
}

fn file_info_from_stats(
    path: String,
    stats: &pi_js::fs::Stats,
) -> std::result::Result<FileInfo, FileError> {
    let Some(kind) = file_kind_from_stats(stats) else {
        return Err(FileError::new(
            FileErrorCode::Invalid,
            "Unsupported file type",
            Some(path),
        ));
    };
    Ok(FileInfo {
        name: pi_js::path::basename(&path, None),
        path,
        kind,
        size: stats.size,
        mtime_ms: stats.mtime_ms,
    })
}

fn to_file_error(error: JsError, fallback_path: Option<&str>) -> FileError {
    match error {
        JsError::Abort(reason) => {
            FileError::new(FileErrorCode::Aborted, reason.message, fallback_path.map(str::to_string))
        }
        JsError::Node(node) => {
            let path = node.path.or_else(|| fallback_path.map(str::to_string));
            let code = match node.code.as_str() {
                "ABORT_ERR" => FileErrorCode::Aborted,
                "ENOENT" => FileErrorCode::NotFound,
                "EACCES" | "EPERM" => FileErrorCode::PermissionDenied,
                "ENOTDIR" => FileErrorCode::NotDirectory,
                "EISDIR" => FileErrorCode::IsDirectory,
                "EINVAL" => FileErrorCode::Invalid,
                "ENOTSUP" | "EOPNOTSUPP" => FileErrorCode::NotSupported,
                _ => FileErrorCode::Unknown,
            };
            FileError::new(code, node.message, path)
        }
        JsError::Typed(name, err) => {
            if name == "FileError"
                && let Some(fe) = err.downcast_ref::<FileError>()
            {
                return fe.clone();
            }
            FileError::new(
                FileErrorCode::Unknown,
                err.to_string(),
                fallback_path.map(str::to_string),
            )
        }
        other => {
            let path = fallback_path.map(str::to_string);
            let code = match other.code() {
                Some("ABORT_ERR") => FileErrorCode::Aborted,
                Some("ENOENT") => FileErrorCode::NotFound,
                Some("EACCES") | Some("EPERM") => FileErrorCode::PermissionDenied,
                Some("ENOTDIR") => FileErrorCode::NotDirectory,
                Some("EISDIR") => FileErrorCode::IsDirectory,
                Some("EINVAL") => FileErrorCode::Invalid,
                Some("ENOTSUP") | Some("EOPNOTSUPP") => FileErrorCode::NotSupported,
                _ => FileErrorCode::Unknown,
            };
            FileError::new(code, other.message(), path)
        }
    }
}

fn abort_result<T>(
    signal: Option<&AbortSignal>,
    path: Option<&str>,
) -> Option<std::result::Result<T, FileError>> {
    signal.filter(|s| s.aborted()).map(|_| {
        Err(FileError::new(
            FileErrorCode::Aborted,
            "aborted",
            path.map(str::to_string),
        ))
    })
}

async fn path_exists(path: &str) -> bool {
    pi_js::fs::promises::access(path, pi_js::fs::constants::F_OK)
        .await
        .is_ok()
}

pub fn is_legacy_wsl_bash_path(path: &str) -> bool {
    let normalized = path.replace('/', "\\").to_lowercase();
    pi_js::regex::test(&LEGACY_WSL_BASH_RE, &normalized)
}

fn get_bash_shell_config(shell: String) -> ShellConfig {
    if is_legacy_wsl_bash_path(&shell) {
        ShellConfig {
            shell,
            args: vec!["-s".to_string()],
            command_transport: CommandTransport::Stdin,
        }
    } else {
        ShellConfig {
            shell,
            args: vec!["-c".to_string()],
            command_transport: CommandTransport::Argv,
        }
    }
}

async fn run_lookup_command(command: &str, args: &[&str], timeout_ms: u64) -> Option<String> {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .env_clear()
        .envs(pi_js::env::vars())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn().ok()?;
    let pid = child.id();
    let mut stdout = child.stdout.take();
    let work = async {
        let mut out = String::new();
        if let Some(mut reader) = stdout.take() {
            let mut buf = Vec::new();
            let _ = reader.read_to_end(&mut buf).await;
            out = String::from_utf8_lossy(&buf).into_owned();
        }
        let status = child.wait().await.ok()?;
        if status.success() { Some(out) } else { None }
    };

    match tokio::time::timeout(Duration::from_millis(timeout_ms), work).await {
        Ok(res) => res,
        Err(_) => {
            if let Some(pid) = pid {
                kill_process_tree(pid);
            }
            None
        }
    }
}

async fn find_bash_on_path() -> Option<String> {
    let output = if pi_js::env::platform() == "win32" {
        run_lookup_command("where", &["bash.exe"], 5000).await?
    } else {
        run_lookup_command("which", &["bash"], 5000).await?
    };
    let first_match = output.trim().lines().next()?.trim();
    if !first_match.is_empty() && path_exists(first_match).await {
        Some(first_match.to_string())
    } else {
        None
    }
}

async fn get_shell_config(
    custom_shell_path: Option<&str>,
) -> std::result::Result<ShellConfig, ExecutionError> {
    if let Some(custom) = custom_shell_path {
        if path_exists(custom).await {
            return Ok(get_bash_shell_config(custom.to_string()));
        }
        return Err(ExecutionError::new(
            ExecutionErrorCode::ShellUnavailable,
            format!("Custom shell path not found: {custom}"),
        ));
    }
    if pi_js::env::platform() == "win32" {
        let mut candidates = Vec::new();
        if let Some(program_files) = pi_js::env::var("ProgramFiles") {
            candidates.push(format!("{program_files}\\Git\\bin\\bash.exe"));
        }
        if let Some(program_files_x86) = pi_js::env::var("ProgramFiles(x86)") {
            candidates.push(format!("{program_files_x86}\\Git\\bin\\bash.exe"));
        }
        for candidate in &candidates {
            if path_exists(candidate).await {
                return Ok(get_bash_shell_config(candidate.clone()));
            }
        }
        if let Some(bash_on_path) = find_bash_on_path().await {
            return Ok(get_bash_shell_config(bash_on_path));
        }
        let searched = candidates
            .iter()
            .map(|p| format!("  {p}"))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(ExecutionError::new(
            ExecutionErrorCode::ShellUnavailable,
            format!(
                "No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Configure an explicit shellPath\n\nSearched Git Bash in:\n{searched}"
            ),
        ));
    }

    if path_exists("/bin/bash").await {
        return Ok(get_bash_shell_config("/bin/bash".to_string()));
    }
    if let Some(bash_on_path) = find_bash_on_path().await {
        return Ok(get_bash_shell_config(bash_on_path));
    }
    Ok(ShellConfig {
        shell: "sh".to_string(),
        args: vec!["-c".to_string()],
        command_transport: CommandTransport::Argv,
    })
}

fn get_shell_env(
    base_env: Option<&IndexMap<String, String>>,
    extra_env: Option<&IndexMap<String, String>>,
    inherit_env: bool,
) -> IndexMap<String, String> {
    let mut result = if inherit_env {
        let mut map = pi_js::env::vars();
        if let Some(base) = base_env {
            for (k, v) in base {
                map.insert(k.clone(), v.clone());
            }
        }
        map
    } else {
        IndexMap::new()
    };
    if let Some(extra) = extra_env {
        for (k, v) in extra {
            result.insert(k.clone(), v.clone());
        }
    }
    result
}

pub fn kill_process_tree(pid: u32) {
    if pi_js::env::platform() == "win32" {
        let system_root = pi_js::env::var("SystemRoot").unwrap_or_else(|| "C:\\Windows".to_string());
        let taskkill = pi_js::path::join(&[&system_root, "System32", "taskkill.exe"]);
        let mut cmd = std::process::Command::new(taskkill);
        cmd.args(["/F", "/T", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let _ = cmd.spawn();
        return;
    }

    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill, killpg};
        use nix::unistd::Pid;
        let Ok(raw_pid) = i32::try_from(pid) else {
            return;
        };
        if raw_pid > 0 && killpg(Pid::from_raw(raw_pid), Signal::SIGKILL).is_err() {
            let _ = kill(Pid::from_raw(raw_pid), Signal::SIGKILL);
        }
    }
}

async fn run_cancellable<T, F>(
    signal: Option<&AbortSignal>,
    path: Option<&str>,
    fut: F,
) -> std::result::Result<T, FileError>
where
    F: std::future::Future<Output = pi_js::Result<T>>,
{
    if let Some(err) = abort_result::<T>(signal, path) {
        return err;
    }
    let res = if let Some(sig) = signal {
        tokio::select! {
            biased;
            () = sig.cancelled() => {
                return Err(FileError::new(FileErrorCode::Aborted, "aborted", path.map(str::to_string)));
            }
            r = fut => r,
        }
    } else {
        fut.await
    };
    if let Some(err) = abort_result::<T>(signal, path) {
        return err;
    }
    res.map_err(|e| to_file_error(e, path))
}

/// Strict LF reader; preserves carriage returns and distinguishes EOF without trailing newline.
pub struct NodeTextLineReader {
    file: pi_js::fs::promises::FileHandle,
    path: String,
    decoder: Utf8StreamDecoder,
    chunk: Vec<u8>,
    byte_offset: u64,
    buffered: String,
    ended: bool,
    closed: bool,
    read_delay_ms: Option<u64>,
}

impl NodeTextLineReader {
    fn new(file: pi_js::fs::promises::FileHandle, path: String) -> Self {
        Self {
            file,
            path,
            decoder: Utf8StreamDecoder::new(),
            chunk: vec![0u8; LINE_READER_CHUNK_SIZE],
            byte_offset: 0,
            buffered: String::new(),
            ended: false,
            closed: false,
            read_delay_ms: None,
        }
    }

    /// Test hook: inject a delay inside the next underlying read call so cancellation tests can observe pending reads.
    #[doc(hidden)]
    pub fn set_read_delay_for_test(&mut self, delay_ms: Option<u64>) {
        self.read_delay_ms = delay_ms;
    }
}

#[async_trait]
impl TextLineReader for NodeTextLineReader {
    async fn read_line(
        &mut self,
        context: &Context,
    ) -> std::result::Result<Option<TextLine>, FileError> {
        let signal = context.abort_signal();
        if let Some(aborted) = abort_result(signal.as_ref(), Some(&self.path)) {
            return aborted;
        }
        if self.closed {
            return Err(FileError::new(
                FileErrorCode::Invalid,
                "Text line reader is closed",
                Some(self.path.clone()),
            ));
        }

        loop {
            if let Some(newline) = self.buffered.find('\n') {
                let text = self.buffered[..newline].to_string();
                self.buffered.drain(..=newline);
                return Ok(Some(TextLine {
                    text,
                    terminated: true,
                }));
            }
            if self.ended {
                if self.buffered.is_empty() {
                    return Ok(None);
                }
                let text = std::mem::take(&mut self.buffered);
                return Ok(Some(TextLine {
                    text,
                    terminated: false,
                }));
            }

            let delay = self.read_delay_ms.take();
            let offset = self.byte_offset;
            let bytes_read = run_cancellable(signal.as_ref(), Some(&self.path), async {
                if let Some(ms) = delay {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                }
                self.file.read(&mut self.chunk, Some(offset)).await
            })
            .await?;

            self.byte_offset += bytes_read as u64;
            if bytes_read == 0 {
                self.buffered.push_str(&self.decoder.decode(&[], false));
                self.ended = true;
            } else {
                self.buffered
                    .push_str(&self.decoder.decode(&self.chunk[..bytes_read], true));
            }
        }
    }

    async fn close(&mut self, _context: &Context) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.buffered.clear();
        let _ = self.file.close().await;
    }
}

struct ActiveChildGuard {
    pids: Arc<Mutex<IndexSet<u32>>>,
    pid: u32,
}

impl Drop for ActiveChildGuard {
    fn drop(&mut self) {
        self.pids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shift_remove(&self.pid);
    }
}

struct SpillState {
    path_rx: oneshot::Receiver<std::result::Result<String, ExecutionError>>,
    err_rx: oneshot::Receiver<ExecutionError>,
    tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    writer_handle: tokio::task::JoinHandle<()>,
    queued_bytes: Arc<AtomicUsize>,
    in_flight: Arc<AtomicUsize>,
    opening: Arc<AtomicBool>,
    notify: Arc<Notify>,
    high_water_mark: usize,
    rejected_writes: Option<Arc<AtomicUsize>>,
    backpressured: bool,
}

impl SpillState {
    fn is_backpressured(&self) -> bool {
        self.backpressured || self.opening.load(Ordering::SeqCst)
    }

    fn is_draining(&self) -> bool {
        self.opening.load(Ordering::SeqCst)
            || self.in_flight.load(Ordering::SeqCst) > 0
            || self.backpressured
    }

    fn update_backpressure(&mut self) {
        self.backpressured = self.queued_bytes.load(Ordering::SeqCst) >= self.high_water_mark;
    }

    fn send_chunk(&mut self, chunk: Vec<u8>) {
        if chunk.is_empty() {
            return;
        }
        if let Some(tx) = &self.tx {
            let len = chunk.len();
            self.in_flight.fetch_add(1, Ordering::SeqCst);
            let new_bytes = self.queued_bytes.fetch_add(len, Ordering::SeqCst) + len;
            if new_bytes >= self.high_water_mark {
                if let Some(counter) = &self.rejected_writes {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
                self.backpressured = true;
            }
            if tx.send(chunk).is_err() {
                self.in_flight.fetch_sub(1, Ordering::SeqCst);
                self.queued_bytes.fetch_sub(len, Ordering::SeqCst);
                self.update_backpressure();
            }
        }
    }
}

#[derive(Clone)]
pub struct NodeExecutionEnv {
    pub cwd: String,
    shell_path: Option<String>,
    shell_env: Option<IndexMap<String, String>>,
    active_child_pids: Arc<Mutex<IndexSet<u32>>>,
    temp_file_hook: Option<TempFileHook>,
    spill_test_config: SpillTestConfig,
}

impl std::fmt::Debug for NodeExecutionEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeExecutionEnv")
            .field("cwd", &self.cwd)
            .field("shell_path", &self.shell_path)
            .finish()
    }
}

impl NodeExecutionEnv {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self::with_options(NodeExecutionEnvOptions {
            cwd: cwd.into(),
            shell_path: None,
            shell_env: None,
        })
    }

    pub fn with_options(options: NodeExecutionEnvOptions) -> Self {
        Self {
            cwd: options.cwd,
            shell_path: options.shell_path,
            shell_env: options.shell_env,
            active_child_pids: Arc::new(Mutex::new(IndexSet::new())),
            temp_file_hook: None,
            spill_test_config: SpillTestConfig::default(),
        }
    }

    #[doc(hidden)]
    pub fn with_temp_file_hook(mut self, hook: TempFileHook) -> Self {
        self.temp_file_hook = Some(hook);
        self
    }

    #[doc(hidden)]
    pub fn with_spill_test_config(mut self, config: SpillTestConfig) -> Self {
        self.spill_test_config = config;
        self
    }

    #[doc(hidden)]
    pub async fn open_node_text_line_reader(
        &self,
        path: &str,
        context: &Context,
    ) -> std::result::Result<NodeTextLineReader, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        let signal = context.abort_signal();
        let handle = run_cancellable(
            signal.as_ref(),
            Some(&resolved),
            pi_js::fs::promises::open(&resolved, "r", None),
        )
        .await?;
        Ok(NodeTextLineReader::new(handle, resolved))
    }

    fn spawn_spill_task(
        &self,
        initial_prefix: VecDeque<Vec<u8>>,
        context: &Context,
    ) -> SpillState {
        let env = self.clone();
        let ctx = context.clone();
        let (path_tx, path_rx) = oneshot::channel();
        let (err_tx, err_rx) = oneshot::channel();
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let opening = Arc::new(AtomicBool::new(true));
        let notify = Arc::new(Notify::new());
        let high_water_mark = self
            .spill_test_config
            .high_water_mark
            .unwrap_or(SPILL_HIGH_WATER_MARK);
        let write_delay_ms = self.spill_test_config.write_delay_ms;
        let rejected_writes = self.spill_test_config.rejected_writes.clone();

        let qb = Arc::clone(&queued_bytes);
        let inf = Arc::clone(&in_flight);
        let op = Arc::clone(&opening);
        let nt = Arc::clone(&notify);

        let writer_handle = tokio::spawn(async move {
            let opts = CreateTempFileOptions {
                prefix: Some("pi-output-".to_string()),
                suffix: Some(".log".to_string()),
            };
            let created = env.create_temp_file(Some(opts), &ctx).await;
            let path = match created {
                Ok(path) => path,
                Err(error) => {
                    op.store(false, Ordering::SeqCst);
                    nt.notify_waiters();
                    let exec_err = ExecutionError::new(
                        ExecutionErrorCode::Unknown,
                        format!("Failed to preserve complete shell output: {error}"),
                    );
                    let _ = path_tx.send(Err(exec_err.clone()));
                    let _ = err_tx.send(exec_err);
                    return;
                }
            };

            let file_res = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await;
            let mut file = match file_res {
                Ok(f) => f,
                Err(error) => {
                    op.store(false, Ordering::SeqCst);
                    nt.notify_waiters();
                    let exec_err = ExecutionError::new(
                        ExecutionErrorCode::Unknown,
                        format!("Failed to preserve complete shell output: {error}"),
                    );
                    let _ = path_tx.send(Err(exec_err.clone()));
                    let _ = err_tx.send(exec_err);
                    return;
                }
            };

            let _ = path_tx.send(Ok(path));
            op.store(false, Ordering::SeqCst);
            nt.notify_waiters();

            for chunk in initial_prefix {
                if !chunk.is_empty() {
                    if let Some(delay) = write_delay_ms {
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                    }
                    if let Err(error) = file.write_all(&chunk).await {
                        let _ = err_tx.send(ExecutionError::new(
                            ExecutionErrorCode::Unknown,
                            format!("Failed to preserve complete shell output: {error}"),
                        ));
                        return;
                    }
                }
            }

            while let Some(chunk) = rx.recv().await {
                let len = chunk.len();
                if let Some(delay) = write_delay_ms {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                let write_res = file.write_all(&chunk).await;
                qb.fetch_sub(len, Ordering::SeqCst);
                inf.fetch_sub(1, Ordering::SeqCst);
                nt.notify_waiters();
                if let Err(error) = write_res {
                    let _ = err_tx.send(ExecutionError::new(
                        ExecutionErrorCode::Unknown,
                        format!("Failed to preserve complete shell output: {error}"),
                    ));
                    return;
                }
            }

            if let Err(error) = file.flush().await {
                let _ = err_tx.send(ExecutionError::new(
                    ExecutionErrorCode::Unknown,
                    format!("Failed to preserve complete shell output: {error}"),
                ));
            }
        });

        SpillState {
            path_rx,
            err_rx,
            tx: Some(tx),
            writer_handle,
            queued_bytes,
            in_flight,
            opening,
            notify,
            high_water_mark,
            rejected_writes: self.spill_test_config.rejected_writes.clone(),
            backpressured: true,
        }
    }
}

#[async_trait]
impl EnvCleanup for NodeExecutionEnv {
    async fn cleanup(&self, _context: &Context) {
        let pids: Vec<u32> = {
            let mut guard = self
                .active_child_pids
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            guard.drain(..).collect()
        };
        for pid in pids {
            kill_process_tree(pid);
        }
    }
}

#[async_trait]
impl FileSystem for NodeExecutionEnv {
    fn id(&self) -> &str {
        "node:local"
    }

    fn cwd(&self) -> &str {
        &self.cwd
    }

    async fn absolute_path(
        &self,
        path: &str,
        _context: &Context,
    ) -> std::result::Result<String, FileError> {
        Ok(resolve_path(&self.cwd, path))
    }

    async fn join_path(
        &self,
        parts: &[&str],
        _context: &Context,
    ) -> std::result::Result<String, FileError> {
        Ok(pi_js::path::join(parts))
    }

    async fn open_text_line_reader(
        &self,
        path: &str,
        context: &Context,
    ) -> std::result::Result<Box<dyn TextLineReader>, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        let signal = context.abort_signal();
        let file = run_cancellable(
            signal.as_ref(),
            Some(&resolved),
            pi_js::fs::promises::open(&resolved, "r", None),
        )
        .await;
        match file {
            Ok(handle) => Ok(Box::new(NodeTextLineReader::new(handle, resolved))),
            Err(error) => Err(error),
        }
    }

    async fn read_text_file(
        &self,
        path: &str,
        context: &Context,
    ) -> std::result::Result<String, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        let signal = context.abort_signal();
        run_cancellable(
            signal.as_ref(),
            Some(&resolved),
            pi_js::fs::promises::read_to_string(&resolved),
        )
        .await
    }

    async fn read_text_lines(
        &self,
        path: &str,
        options: Option<ReadTextLinesOptions>,
        context: &Context,
    ) -> std::result::Result<Vec<String>, FileError> {
        let max_lines = options.and_then(|o| o.max_lines);
        if max_lines == Some(0) {
            return Ok(Vec::new());
        }
        let mut reader = self.open_text_line_reader(path, context).await?;
        let mut lines = Vec::new();
        let res = async {
            while max_lines.is_none_or(|limit| lines.len() < limit) {
                match reader.read_line(context).await? {
                    Some(line) => lines.push(line.text),
                    None => break,
                }
            }
            Ok(lines)
        }
        .await;
        reader.close(context).await;
        res
    }

    async fn read_binary_file(
        &self,
        path: &str,
        context: &Context,
    ) -> std::result::Result<Vec<u8>, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        let signal = context.abort_signal();
        run_cancellable(
            signal.as_ref(),
            Some(&resolved),
            pi_js::fs::promises::read_file(&resolved),
        )
        .await
    }

    async fn write_file(
        &self,
        path: &str,
        content: FileContent<'_>,
        context: &Context,
    ) -> std::result::Result<(), FileError> {
        let resolved = resolve_path(&self.cwd, path);
        let signal = context.abort_signal();
        if let Some(aborted) = abort_result(signal.as_ref(), Some(&resolved)) {
            return aborted;
        }
        let parent = pi_js::path::resolve(&[&resolved, ".."]);
        run_cancellable(
            signal.as_ref(),
            Some(&resolved),
            pi_js::fs::promises::mkdir(&parent, true, None),
        )
        .await?;
        let data = content.as_bytes().to_vec();
        run_cancellable(
            signal.as_ref(),
            Some(&resolved),
            pi_js::fs::promises::write_file(&resolved, data, None),
        )
        .await
    }

    async fn append_file(
        &self,
        path: &str,
        content: FileContent<'_>,
        context: &Context,
    ) -> std::result::Result<(), FileError> {
        let resolved = resolve_path(&self.cwd, path);
        let signal = context.abort_signal();
        if let Some(aborted) = abort_result(signal.as_ref(), Some(&resolved)) {
            return aborted;
        }
        let parent = pi_js::path::resolve(&[&resolved, ".."]);
        run_cancellable(
            signal.as_ref(),
            Some(&resolved),
            pi_js::fs::promises::mkdir(&parent, true, None),
        )
        .await?;
        let data = content.as_bytes().to_vec();
        run_cancellable(
            signal.as_ref(),
            Some(&resolved),
            pi_js::fs::promises::append_file(&resolved, data),
        )
        .await
    }

    async fn truncate_file(
        &self,
        path: &str,
        size: u64,
        context: &Context,
    ) -> std::result::Result<(), FileError> {
        let resolved = resolve_path(&self.cwd, path);
        let signal = context.abort_signal();
        if let Some(aborted) = abort_result(signal.as_ref(), Some(&resolved)) {
            return aborted;
        }
        if size > pi_js::num::MAX_SAFE_INTEGER as u64 {
            return Err(FileError::new(
                FileErrorCode::Invalid,
                "File size must be a non-negative safe integer",
                Some(resolved),
            ));
        }
        let handle = pi_js::fs::promises::open(&resolved, "r+", None)
            .await
            .map_err(|e| to_file_error(e, Some(&resolved)))?;
        let res = handle
            .truncate(size)
            .await
            .map_err(|e| to_file_error(e, Some(&resolved)));
        let _ = handle.close().await;
        res?;
        if let Some(aborted) = abort_result(signal.as_ref(), Some(&resolved)) {
            return aborted;
        }
        Ok(())
    }

    async fn flush_file(
        &self,
        path: &str,
        context: &Context,
    ) -> std::result::Result<(), FileError> {
        let resolved = resolve_path(&self.cwd, path);
        let signal = context.abort_signal();
        if let Some(aborted) = abort_result(signal.as_ref(), Some(&resolved)) {
            return aborted;
        }
        let handle = pi_js::fs::promises::open(&resolved, "r+", None)
            .await
            .map_err(|e| to_file_error(e, Some(&resolved)))?;
        let res = handle
            .sync()
            .await
            .map_err(|e| to_file_error(e, Some(&resolved)));
        let _ = handle.close().await;
        res?;
        if let Some(aborted) = abort_result(signal.as_ref(), Some(&resolved)) {
            return aborted;
        }
        Ok(())
    }

    async fn rename_file(
        &self,
        source_path: &str,
        destination_path: &str,
        context: &Context,
    ) -> std::result::Result<(), FileError> {
        let source = resolve_path(&self.cwd, source_path);
        let destination = resolve_path(&self.cwd, destination_path);
        if let Some(aborted) = abort_result(context.abort_signal().as_ref(), Some(&destination)) {
            return aborted;
        }
        pi_js::fs::promises::rename(&source, &destination)
            .await
            .map_err(|e| to_file_error(e, Some(&source)))
    }

    async fn file_info(
        &self,
        path: &str,
        context: &Context,
    ) -> std::result::Result<FileInfo, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        if let Some(aborted) = abort_result(context.abort_signal().as_ref(), Some(&resolved)) {
            return aborted;
        }
        let stats = pi_js::fs::promises::lstat(&resolved)
            .await
            .map_err(|e| to_file_error(e, Some(&resolved)))?;
        file_info_from_stats(resolved, &stats)
    }

    async fn list_dir(
        &self,
        path: &str,
        context: &Context,
    ) -> std::result::Result<Vec<FileInfo>, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        let signal = context.abort_signal();
        if let Some(aborted) = abort_result(signal.as_ref(), Some(&resolved)) {
            return aborted;
        }
        let entries = pi_js::fs::promises::readdir_with_file_types(&resolved)
            .await
            .map_err(|e| to_file_error(e, Some(&resolved)))?;
        let mut infos = Vec::with_capacity(entries.len());
        for entry in entries {
            if let Some(aborted) = abort_result(signal.as_ref(), Some(&resolved)) {
                return aborted;
            }
            let entry_path = pi_js::path::resolve(&[&resolved, &entry.name]);
            let stats = pi_js::fs::promises::lstat(&entry_path)
                .await
                .map_err(|e| to_file_error(e, Some(&entry_path)))?;
            if let Ok(info) = file_info_from_stats(entry_path, &stats) {
                infos.push(info);
            }
        }
        Ok(infos)
    }

    async fn canonical_path(
        &self,
        path: &str,
        context: &Context,
    ) -> std::result::Result<String, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        if let Some(aborted) = abort_result(context.abort_signal().as_ref(), Some(&resolved)) {
            return aborted;
        }
        pi_js::fs::promises::realpath(&resolved)
            .await
            .map_err(|e| to_file_error(e, Some(&resolved)))
    }

    async fn exists(
        &self,
        path: &str,
        context: &Context,
    ) -> std::result::Result<bool, FileError> {
        match self.file_info(path, context).await {
            Ok(_) => Ok(true),
            Err(error) if error.code == FileErrorCode::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn create_dir(
        &self,
        path: &str,
        options: Option<CreateDirOptions>,
        context: &Context,
    ) -> std::result::Result<(), FileError> {
        let resolved = resolve_path(&self.cwd, path);
        if let Some(aborted) = abort_result(context.abort_signal().as_ref(), Some(&resolved)) {
            return aborted;
        }
        let recursive = options.and_then(|o| o.recursive).unwrap_or(true);
        pi_js::fs::promises::mkdir(&resolved, recursive, None)
            .await
            .map_err(|e| to_file_error(e, Some(&resolved)))
    }

    async fn remove(
        &self,
        path: &str,
        options: Option<RemoveOptions>,
        context: &Context,
    ) -> std::result::Result<(), FileError> {
        let resolved = resolve_path(&self.cwd, path);
        if let Some(aborted) = abort_result(context.abort_signal().as_ref(), Some(&resolved)) {
            return aborted;
        }
        let recursive = options.and_then(|o| o.recursive).unwrap_or(false);
        let force = options.and_then(|o| o.force).unwrap_or(false);
        pi_js::fs::promises::rm(&resolved, recursive, force)
            .await
            .map_err(|e| to_file_error(e, Some(&resolved)))
    }

    async fn create_temp_dir(
        &self,
        prefix: Option<&str>,
        context: &Context,
    ) -> std::result::Result<String, FileError> {
        if let Some(aborted) = abort_result(context.abort_signal().as_ref(), None) {
            return aborted;
        }
        let prefix = prefix.unwrap_or("tmp-");
        let template = pi_js::path::join(&[&pi_js::env::tmp_dir(), prefix]);
        pi_js::fs::promises::mkdtemp(&template)
            .await
            .map_err(|e| to_file_error(e, None))
    }

    async fn create_temp_file(
        &self,
        options: Option<CreateTempFileOptions>,
        context: &Context,
    ) -> std::result::Result<String, FileError> {
        if let Some(hook) = &self.temp_file_hook
            && let Some(override_result) = hook(options.as_ref())
        {
            return override_result;
        }
        let dir = self.create_temp_dir(Some("tmp-"), context).await?;
        let prefix = options
            .as_ref()
            .and_then(|o| o.prefix.as_deref())
            .unwrap_or("");
        let suffix = options
            .as_ref()
            .and_then(|o| o.suffix.as_deref())
            .unwrap_or("");
        let filename = format!("{prefix}{}{suffix}", Uuid::new_v4());
        let file_path = pi_js::path::join(&[&dir, &filename]);
        pi_js::fs::promises::write_file(&file_path, b"", None)
            .await
            .map_err(|e| to_file_error(e, Some(&file_path)))?;
        Ok(file_path)
    }
}

#[async_trait]
impl Shell for NodeExecutionEnv {
    async fn exec(
        &self,
        command: &str,
        options: Option<ShellExecOptions>,
        context: &Context,
    ) -> std::result::Result<ShellExecResult, ExecutionError> {
        let signal = context.abort_signal();
        if signal.as_ref().is_some_and(|s| s.aborted()) {
            return Err(ExecutionError::new(ExecutionErrorCode::Aborted, "aborted"));
        }

        let timeout_opt = options.as_ref().and_then(|o| o.timeout);
        let timeout_ms = resolve_timeout_ms(timeout_opt)?;

        let cwd = options
            .as_ref()
            .and_then(|o| o.cwd.as_deref())
            .map(|p| resolve_path(&self.cwd, p))
            .unwrap_or_else(|| self.cwd.clone());

        let shell_config = get_shell_config(self.shell_path.as_deref()).await?;

        if let Err(err) = pi_js::fs::promises::access(&cwd, pi_js::fs::constants::F_OK).await {
            return Err(ExecutionError::new(
                ExecutionErrorCode::SpawnError,
                format!("Working directory does not exist: {cwd}\nCannot execute bash commands."),
            )
            .with_cause(err.to_string()));
        }

        let env_map = get_shell_env(
            self.shell_env.as_ref(),
            options.as_ref().and_then(|o| o.env.as_ref()),
            options
                .as_ref()
                .and_then(|o| o.inherit_env)
                .unwrap_or(true),
        );

        let stdin_transport = shell_config.command_transport == CommandTransport::Stdin;
        let mut cmd = Command::new(&shell_config.shell);
        cmd.args(&shell_config.args);
        if !stdin_transport {
            cmd.arg(command);
        }
        cmd.current_dir(&cwd)
            .env_clear()
            .envs(env_map)
            .stdin(if stdin_transport {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| ExecutionError::new(ExecutionErrorCode::SpawnError, e.to_string()))?;
        let child_pid = child.id();
        let _active_guard = child_pid.map(|pid| {
            self.active_child_pids
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(pid);
            ActiveChildGuard {
                pids: Arc::clone(&self.active_child_pids),
                pid,
            }
        });

        if stdin_transport && let Some(mut stdin) = child.stdin.take() {
            let cmd_bytes = command.as_bytes().to_vec();
            tokio::spawn(async move {
                let _ = stdin.write_all(&cmd_bytes).await;
                let _ = stdin.shutdown().await;
            });
        }

        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let mut stdout_buf = vec![0u8; 64 * 1024];
        let mut stderr_buf = vec![0u8; 64 * 1024];
        let mut stdout_decoder = Utf8StreamDecoder::new();
        let mut stderr_decoder = Utf8StreamDecoder::new();

        let on_output = options.as_ref().and_then(|o| o.on_output.clone());
        let spill_opts = options.as_ref().and_then(|o| o.spill);

        let mut settled = false;
        let mut timed_out = false;
        let mut aborted = false;
        let mut callback_error: Option<ExecutionError> = None;
        let mut spill_error: Option<ExecutionError> = None;

        let mut seen_bytes: usize = 0;
        let mut seen_newlines: usize = 0;
        let mut spill_prefix: VecDeque<Vec<u8>> = VecDeque::new();
        let mut spill_state: Option<SpillState> = None;
        let mut spill_path: Option<String> = None;

        let mut exited = false;
        let mut exit_status: Option<std::process::ExitStatus> = None;
        let mut spawn_wait_err: Option<std::io::Error> = None;

        let kill_child = |pid_opt: Option<u32>| {
            if let Some(pid) = pid_opt {
                kill_process_tree(pid);
            }
        };

        let mut timeout_sleep: Option<Pin<Box<Sleep>>> =
            timeout_ms.map(|ms| Box::pin(tokio::time::sleep(Duration::from_millis(ms))));
        let mut grace_sleep: Option<Pin<Box<Sleep>>> = None;

        if signal.as_ref().is_some_and(|s| s.aborted()) {
            aborted = true;
            kill_child(child_pid);
        }

        let cancel_fut = async {
            if let Some(sig) = &signal {
                sig.cancelled().await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(cancel_fut);

        loop {
            if let Some(state) = &mut spill_state {
                if spill_path.is_none()
                    && let Ok(res) = state.path_rx.try_recv()
                {
                    match res {
                        Ok(p) => spill_path = Some(p),
                        Err(e) => {
                            if spill_error.is_none() {
                                spill_error = Some(e);
                                state.backpressured = false;
                                kill_child(child_pid);
                            }
                        }
                    }
                }
                if spill_error.is_none()
                    && let Ok(e) = state.err_rx.try_recv()
                {
                    spill_error = Some(e);
                    state.backpressured = false;
                    kill_child(child_pid);
                }
                state.update_backpressure();
            }

            if exited && stdout.is_none() && stderr.is_none() {
                break;
            }

            let paused_for_spill = spill_state.as_ref().is_some_and(|s| s.is_backpressured());
            let spill_notify = spill_state.as_ref().map(|s| Arc::clone(&s.notify));

            let mut chunk_to_process: Option<(bool, &[u8])> = None;
            let mut got_eof_stdout = false;
            let mut got_eof_stderr = false;

            tokio::select! {
                biased;

                () = &mut cancel_fut, if !aborted => {
                    aborted = true;
                    kill_child(child_pid);
                }

                () = async {
                    if let Some(ts) = &mut timeout_sleep {
                        ts.as_mut().await;
                    }
                }, if timeout_sleep.is_some() && !timed_out => {
                    timed_out = true;
                    timeout_sleep = None;
                    kill_child(child_pid);
                }

                res = child.wait(), if !exited => {
                    exited = true;
                    match res {
                        Ok(status) => exit_status = Some(status),
                        Err(err) => spawn_wait_err = Some(err),
                    }
                    if stdout.is_none() && stderr.is_none() {
                        break;
                    }
                    grace_sleep = Some(Box::pin(tokio::time::sleep(Duration::from_millis(EXIT_STDIO_GRACE_MS))));
                }

                () = async {
                    if let Some(notify) = &spill_notify {
                        notify.notified().await;
                    }
                }, if paused_for_spill && spill_notify.is_some() => {
                    if let Some(state) = &mut spill_state {
                        if let Ok(res) = state.path_rx.try_recv() {
                            match res {
                                Ok(p) => spill_path = Some(p),
                                Err(e) if spill_error.is_none() => {
                                    spill_error = Some(e);
                                    kill_child(child_pid);
                                }
                                Err(_) => {}
                            }
                        }
                        if let Ok(e) = state.err_rx.try_recv() && spill_error.is_none() {
                            spill_error = Some(e);
                            kill_child(child_pid);
                        }
                        state.update_backpressure();
                    }
                }

                res = async {
                    match &mut stdout {
                        Some(r) => r.read(&mut stdout_buf).await,
                        None => std::future::pending().await,
                    }
                }, if stdout.is_some() && !paused_for_spill => {
                    match res {
                        Ok(0) | Err(_) => got_eof_stdout = true,
                        Ok(n) => chunk_to_process = Some((true, &stdout_buf[..n])),
                    }
                }

                res = async {
                    match &mut stderr {
                        Some(r) => r.read(&mut stderr_buf).await,
                        None => std::future::pending().await,
                    }
                }, if stderr.is_some() && !paused_for_spill => {
                    match res {
                        Ok(0) | Err(_) => got_eof_stderr = true,
                        Ok(n) => chunk_to_process = Some((false, &stderr_buf[..n])),
                    }
                }

                () = async {
                    if let Some(gs) = &mut grace_sleep {
                        gs.as_mut().await;
                    }
                }, if exited && grace_sleep.is_some() => {
                    let draining = spill_error.is_none()
                        && spill_state.as_ref().is_some_and(|s| s.is_draining());
                    if draining {
                        grace_sleep = Some(Box::pin(tokio::time::sleep(Duration::from_millis(EXIT_STDIO_GRACE_MS))));
                    } else {
                        settled = true;
                        drop(stdout.take());
                        drop(stderr.take());
                        break;
                    }
                }
            }

            if got_eof_stdout {
                stdout = None;
            }
            if got_eof_stderr {
                stderr = None;
            }

            if let Some((is_stdout, chunk)) = chunk_to_process {
                if exited && !settled {
                    grace_sleep = Some(Box::pin(tokio::time::sleep(Duration::from_millis(
                        EXIT_STDIO_GRACE_MS,
                    ))));
                }
                let text = if is_stdout {
                    stdout_decoder.decode(chunk, true)
                } else {
                    stderr_decoder.decode(chunk, true)
                };
                if !settled
                    && !text.is_empty()
                    && callback_error.is_none()
                    && let Some(cb) = &on_output
                    && let Err(err) = cb(&text, context)
                {
                    callback_error = Some(
                        ExecutionError::new(ExecutionErrorCode::CallbackError, err.message())
                            .with_cause(err.to_string()),
                    );
                    kill_child(child_pid);
                }

                if let Some(spill) = spill_opts
                    && !chunk.is_empty()
                {
                    if let Some(state) = &mut spill_state {
                        state.send_chunk(chunk.to_vec());
                    } else {
                        seen_bytes = seen_bytes.saturating_add(chunk.len());
                        seen_newlines = seen_newlines
                            .saturating_add(chunk.iter().filter(|&&b| b == b'\n').count());
                        let lines = seen_newlines
                            .saturating_add(if chunk.last() == Some(&b'\n') { 0 } else { 1 });
                        if seen_bytes <= spill.after_bytes && lines <= spill.after_lines {
                            spill_prefix.push_back(chunk.to_vec());
                        } else {
                            let mut prefix = std::mem::take(&mut spill_prefix);
                            let mut state = self.spawn_spill_task(std::mem::take(&mut prefix), context);
                            // Wait for temp file path & initial prefix flush? Start opening now, send current chunk!
                            state.send_chunk(chunk.to_vec());
                            spill_state = Some(state);
                        }
                    }
                }
            }
        }

        if let Some(mut state) = spill_state {
            drop(state.tx.take());
            let _ = state.writer_handle.await;
            if spill_path.is_none()
                && let Ok(res) = state.path_rx.try_recv()
            {
                match res {
                    Ok(p) => spill_path = Some(p),
                    Err(e) if spill_error.is_none() => spill_error = Some(e),
                    Err(_) => {}
                }
            }
            if spill_error.is_none()
                && let Ok(e) = state.err_rx.try_recv()
            {
                spill_error = Some(e);
            }
        }

        let tail_out = stdout_decoder.decode(&[], false);
        if !settled
            && !tail_out.is_empty()
            && callback_error.is_none()
            && let Some(cb) = &on_output
            && let Err(err) = cb(&tail_out, context)
        {
            callback_error = Some(
                ExecutionError::new(ExecutionErrorCode::CallbackError, err.message())
                    .with_cause(err.to_string()),
            );
        }
        let tail_err = stderr_decoder.decode(&[], false);
        if !settled
            && !tail_err.is_empty()
            && callback_error.is_none()
            && let Some(cb) = &on_output
            && let Err(err) = cb(&tail_err, context)
        {
            callback_error = Some(
                ExecutionError::new(ExecutionErrorCode::CallbackError, err.message())
                    .with_cause(err.to_string()),
            );
        }

        if let Some(err) = callback_error {
            return Err(err);
        }
        if timed_out {
            let timeout_msg = timeout_opt
                .map(|t| format!("timeout:{t}"))
                .unwrap_or_else(|| "timeout".to_string());
            return Err(
                ExecutionError::new(ExecutionErrorCode::Timeout, timeout_msg)
                    .with_spill_path(spill_path),
            );
        }
        if aborted || signal.as_ref().is_some_and(|s| s.aborted()) {
            return Err(
                ExecutionError::new(ExecutionErrorCode::Aborted, "aborted")
                    .with_spill_path(spill_path),
            );
        }
        if let Some(err) = spill_error {
            return Err(err);
        }
        if let Some(err) = spawn_wait_err {
            return Err(ExecutionError::new(
                ExecutionErrorCode::SpawnError,
                err.to_string(),
            ));
        }

        let exit_code = exit_status
            .map(|status| {
                if let Some(code) = status.code() {
                    code
                } else {
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::ExitStatusExt;
                        status.signal().map(|s| 128 + s).unwrap_or(1)
                    }
                    #[cfg(not(unix))]
                    {
                        1
                    }
                }
            })
            .unwrap_or(1);

        Ok(ShellExecResult {
            exit_code,
            spill_path,
        })
    }
}
