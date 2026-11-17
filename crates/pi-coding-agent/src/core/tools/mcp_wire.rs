//! WIRE(@mcp-w3): Temporary local adapter mirroring `utils/*`, `config`, and `vendor/minimatch`
//! until `@mcp-w3` is merged into this branch. Keep public-within-crate surface narrow and follow
//! `PORTING.md` §13.3 invariants.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::future::BoxFuture;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{ColorType, ImageEncoder};
use indexmap::{IndexMap, IndexSet};
use pi_ai::types::ImageContent;
use pi_js::seam::{Guard, Slot};
use regex::Regex;
use tokio::io::AsyncReadExt;
use tokio::sync::{Mutex, Notify};

pub const POWERSHELL_ARGS: [&str; 5] = [
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy",
    "Bypass",
    "-Command",
];

const EXIT_STDIO_GRACE_MS: u64 = 100;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedTool {
    Fd,
    Rg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResizeImageOptions {
    pub max_width: u32,
    pub max_height: u32,
    pub max_bytes: usize,
    pub jpeg_quality: u8,
}

impl Default for ResizeImageOptions {
    fn default() -> Self {
        Self {
            max_width: 2000,
            max_height: 2000,
            // 4.5 MiB (below 5 MB Anthropic image limit after base64/header overhead)
            max_bytes: 4_718_592,
            jpeg_quality: 80,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResizedImage {
    pub data: String,
    pub mime_type: String,
    pub original_width: u32,
    pub original_height: u32,
    pub width: u32,
    pub height: u32,
    pub was_resized: bool,
}

// ---------------------------------------------------------------------------
// Test seams (WIRE(@mcp-w3))
// ---------------------------------------------------------------------------

pub type ShellConfigOverrideFn = Arc<dyn Fn(Option<&str>) -> Result<ShellConfig, pi_js::Error> + Send + Sync>;
pub type ResizeImageOverrideFn = Arc<
    dyn Fn(ImageContent, ResizeImageOptions) -> BoxFuture<'static, Result<Option<ResizedImage>, pi_js::Error>>
        + Send
        + Sync,
>;

static SHELL_CONFIG_SLOT: LazyLock<Slot<ShellConfigOverrideFn>> = LazyLock::new(Slot::new);
static RESIZE_IMAGE_SLOT: LazyLock<Slot<ResizeImageOverrideFn>> = LazyLock::new(Slot::new);

pub mod testing {
    use super::*;

    pub fn override_get_shell_config(func: ShellConfigOverrideFn) -> Guard {
        SHELL_CONFIG_SLOT.set(Arc::new(func))
    }

    pub fn override_resize_image(func: ResizeImageOverrideFn) -> Guard {
        RESIZE_IMAGE_SLOT.set(Arc::new(func))
    }
}

// ---------------------------------------------------------------------------
// Config paths
// ---------------------------------------------------------------------------

pub fn get_bin_dir() -> String {
    if let Some(dir) = pi_js::env::var("PI_BIN_DIR").filter(|s| !s.is_empty()) {
        return dir;
    }
    if let Some(agent_dir) = pi_js::env::var("PI_CODING_AGENT_DIR").filter(|s| !s.is_empty()) {
        return pi_js::path::join(&[&agent_dir, "bin"]);
    }
    pi_js::path::join(&[&pi_js::env::home_dir(), ".pi", "agent", "bin"])
}

pub fn get_package_dir() -> String {
    if let Some(dir) = pi_js::env::var("PI_PACKAGE_DIR").filter(|s| !s.is_empty()) {
        return dir;
    }
    pi_js::path::join(&[env!("CARGO_MANIFEST_DIR"), "..", ".."])
}

pub fn get_readme_path() -> String {
    pi_js::path::resolve(&[&get_package_dir(), "README.md"])
}

pub fn get_docs_path() -> String {
    pi_js::path::resolve(&[&get_package_dir(), "docs"])
}

pub fn get_examples_path() -> String {
    pi_js::path::resolve(&[&get_package_dir(), "examples"])
}

// ---------------------------------------------------------------------------
// Paths & Text
// ---------------------------------------------------------------------------

pub fn normalize_path(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if pi_js::env::platform() == "win32" {
        trimmed.replace('/', "\\")
    } else {
        trimmed.replace('\\', "/")
    }
}

pub fn resolve_path(input: &str, cwd: &str) -> String {
    let normalized = normalize_path(input);
    if pi_js::path::is_absolute(&normalized) {
        pi_js::path::normalize(&normalized)
    } else {
        pi_js::path::resolve(&[cwd, &normalized])
    }
}

pub fn format_path_relative_to_cwd_or_absolute(file_path: &str, cwd: &str) -> String {
    if cwd.is_empty() {
        return file_path.to_string();
    }
    let resolved_file = pi_js::path::resolve(&[cwd, file_path]);
    let resolved_cwd = pi_js::path::resolve(&[cwd]);
    let rel = pi_js::path::relative(&resolved_cwd, &resolved_file);
    if rel.is_empty() {
        return ".".to_string();
    }
    if rel == ".." || rel.starts_with("../") || rel.starts_with("..\\") || pi_js::path::is_absolute(&rel) {
        resolved_file
    } else {
        rel.replace('\\', "/")
    }
}

pub fn split_bom(content: &str) -> (&str, &str) {
    if let Some(rest) = content.strip_prefix('\u{FEFF}') {
        ("\u{FEFF}", rest)
    } else {
        ("", content)
    }
}

pub fn sanitize_binary_output(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            let code = c as u32;
            let is_removed_c0 = (code <= 0x08) || code == 0x0B || code == 0x0C || (0x0E..=0x1F).contains(&code);
            let is_interlinear = (0xFFF9..=0xFFFB).contains(&code);
            !is_removed_c0 && !is_interlinear
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Shell configuration & child process helpers
// ---------------------------------------------------------------------------

static TRACKED_DETACHED_PIDS: LazyLock<Mutex<IndexSet<u32>>> = LazyLock::new(|| Mutex::new(IndexSet::new()));

fn is_legacy_wsl_bash_path(path: &str) -> bool {
    let normalized = pi_js::path::win32::normalize(path).to_lowercase();
    if let Some(system_root) = pi_js::env::var("SystemRoot")
        && let Some(first) = system_root.chars().next()
        && first.is_ascii_alphabetic()
        && (system_root[first.len_utf8()..].starts_with(":\\") || system_root[first.len_utf8()..].starts_with(":/"))
    {
        for dir in ["System32", "Sysnative"] {
            let candidate = pi_js::path::win32::join(&[&system_root, dir, "bash.exe"]).to_lowercase();
            if normalized == candidate {
                return true;
            }
        }
        return false;
    }
    static LEGACY_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[a-z]:\\windows\\(?:system32|sysnative)\\bash\.exe$").unwrap_or_else(|_| unreachable!())
    });
    LEGACY_RE.is_match(&normalized)
}

fn get_bash_shell_config(shell: String) -> ShellConfig {
    if is_legacy_wsl_bash_path(&shell) {
        ShellConfig {
            shell,
            args: vec!["-s".to_string()],
            command_transport: Some(CommandTransport::Stdin),
        }
    } else {
        ShellConfig {
            shell,
            args: vec!["-c".to_string()],
            command_transport: None,
        }
    }
}

fn shell_path_exists(path: &str) -> bool {
    pi_js::fs::access(path, pi_js::fs::constants::F_OK).is_ok()
}

fn find_executable_on_path(executable: &str) -> Option<String> {
    if pi_js::env::platform() == "win32" {
        if let Ok(output) = std::process::Command::new("where").arg(executable).output()
            && output.status.success()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(first) = text.trim().lines().next()
                && !first.is_empty()
                && shell_path_exists(first)
            {
                return Some(first.to_string());
            }
        }
        None
    } else {
        if let Ok(found) = which::which(executable) {
            return Some(found.to_string_lossy().into_owned());
        }
        if let Ok(output) = std::process::Command::new("which").arg(executable).output()
            && output.status.success()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(first) = text.trim().lines().next()
                && !first.is_empty()
            {
                return Some(first.to_string());
            }
        }
        None
    }
}

pub fn get_shell_config(custom_shell_path: Option<&str>) -> Result<ShellConfig, pi_js::Error> {
    if let Some(override_fn) = SHELL_CONFIG_SLOT.get() {
        return override_fn(custom_shell_path);
    }

    if let Some(custom) = custom_shell_path {
        if shell_path_exists(custom) {
            return Ok(get_bash_shell_config(custom.to_string()));
        }
        return Err(pi_js::Error::msg(format!("Custom shell path not found: {custom}")));
    }

    if pi_js::env::platform() == "win32" {
        let mut paths = Vec::new();
        if let Some(program_files) = pi_js::env::var("ProgramFiles") {
            paths.push(format!("{program_files}\\Git\\bin\\bash.exe"));
        }
        if let Some(program_files_x86) = pi_js::env::var("ProgramFiles(x86)") {
            paths.push(format!("{program_files_x86}\\Git\\bin\\bash.exe"));
        }
        for path in &paths {
            if shell_path_exists(path) {
                return Ok(get_bash_shell_config(path.clone()));
            }
        }
        if let Some(bash_on_path) = find_executable_on_path("bash.exe") {
            return Ok(get_bash_shell_config(bash_on_path));
        }
        let searched = paths.iter().map(|p| format!("  {p}")).collect::<Vec<_>>().join("\n");
        return Err(pi_js::Error::msg(format!(
            "No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Set shellPath in settings.json\n\nSearched Git Bash in:\n{searched}"
        )));
    }

    if shell_path_exists("/bin/bash") {
        return Ok(get_bash_shell_config("/bin/bash".to_string()));
    }
    if let Some(bash_on_path) = find_executable_on_path("bash") {
        return Ok(get_bash_shell_config(bash_on_path));
    }
    Ok(ShellConfig {
        shell: "sh".to_string(),
        args: vec!["-c".to_string()],
        command_transport: None,
    })
}

pub fn get_power_shell_config() -> Result<ShellConfig, pi_js::Error> {
    if pi_js::env::platform() != "win32" {
        return Err(pi_js::Error::msg("The powershell tool is only available on Windows."));
    }

    let mut shell = find_executable_on_path("pwsh.exe");
    if shell.is_none()
        && let Some(program_files) = pi_js::env::var("ProgramFiles")
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
        && let Some(system_root) = pi_js::env::var("SystemRoot")
    {
        let windows_ps =
            pi_js::path::win32::join(&[&system_root, "System32", "WindowsPowerShell", "v1.0", "powershell.exe"]);
        if shell_path_exists(&windows_ps) {
            shell = Some(windows_ps);
        }
    }

    let Some(shell) = shell else {
        return Err(pi_js::Error::msg(
            "No PowerShell executable found. Install PowerShell or add powershell.exe/pwsh.exe to PATH.",
        ));
    };

    Ok(ShellConfig {
        shell,
        args: POWERSHELL_ARGS.iter().map(|s| (*s).to_string()).collect(),
        command_transport: None,
    })
}

pub fn get_shell_env() -> IndexMap<String, String> {
    let mut env_map = IndexMap::new();
    for (k, v) in pi_js::env::vars() {
        env_map.insert(k, v);
    }

    let bin_dir = get_bin_dir();
    let path_key = env_map
        .keys()
        .find(|k| k.eq_ignore_ascii_case("PATH"))
        .cloned()
        .unwrap_or_else(|| "PATH".to_string());
    let current_path = env_map.get(&path_key).cloned().unwrap_or_default();
    let sep = pi_js::path::delimiter();
    let has_bin_dir = current_path.split(sep).any(|entry| entry == bin_dir);
    let updated_path = if has_bin_dir {
        current_path
    } else if current_path.is_empty() {
        bin_dir
    } else {
        format!("{bin_dir}{sep}{current_path}")
    };
    env_map.insert(path_key, updated_path);
    env_map
}

pub fn track_detached_child_pid(pid: u32) {
    if let Ok(mut guard) = TRACKED_DETACHED_PIDS.try_lock() {
        guard.insert(pid);
    }
}

pub fn untrack_detached_child_pid(pid: u32) {
    if let Ok(mut guard) = TRACKED_DETACHED_PIDS.try_lock() {
        guard.shift_remove(&pid);
    }
}

pub fn kill_process_tree(pid: u32) {
    if pi_js::env::platform() == "win32" {
        let system_root = pi_js::env::var("SystemRoot").unwrap_or_else(|| "C:\\Windows".to_string());
        let taskkill = pi_js::path::join(&[&system_root, "System32", "taskkill.exe"]);
        let _ = std::process::Command::new(taskkill)
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        return;
    }

    let neg_pid = format!("-{pid}");
    if pi_js::env::platform() == "darwin" {
        let _ = std::process::Command::new("/bin/kill")
            .args(["-s", "STOP", &neg_pid])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    let group_res = std::process::Command::new("/bin/kill")
        .args(["-s", "KILL", &neg_pid])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if !matches!(group_res, Ok(s) if s.success()) {
        let _ = std::process::Command::new("/bin/kill")
            .args(["-s", "KILL", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

pub fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub struct StreamActivity {
    last_chunk_ms: AtomicU64,
    notify: Notify,
}

impl StreamActivity {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            last_chunk_ms: AtomicU64::new(0),
            notify: Notify::new(),
        })
    }

    pub fn bump(&self) {
        self.last_chunk_ms.store(now_epoch_ms(), Ordering::Release);
        self.notify.notify_waiters();
    }
}

pub async fn wait_for_child_process(
    child: &mut tokio::process::Child,
    stdout_task: Option<tokio::task::JoinHandle<()>>,
    stderr_task: Option<tokio::task::JoinHandle<()>>,
    activity: Arc<StreamActivity>,
) -> Result<Option<i32>, pi_js::Error> {
    let status = child
        .wait()
        .await
        .map_err(|e| pi_js::Error::msg(format!("Failed to wait for child process: {e}")))?;

    let drain_fut = async {
        if let Some(h) = stdout_task {
            let _ = h.await;
        }
        if let Some(h) = stderr_task {
            let _ = h.await;
        }
    };
    tokio::pin!(drain_fut);

    let mut settled_streams = false;
    loop {
        let sleep_fut = tokio::time::sleep(Duration::from_millis(EXIT_STDIO_GRACE_MS));
        tokio::pin!(sleep_fut);
        let notified = activity.notify.notified();
        tokio::pin!(notified);

        tokio::select! {
            _ = &mut drain_fut => {
                settled_streams = true;
                break;
            }
            _ = &mut notified => {
                continue;
            }
            _ = &mut sleep_fut => {
                break;
            }
        }
    }

    let _ = settled_streams;
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        Ok(status.code().or_else(|| status.signal().map(|sig| 128 + sig)))
    }
    #[cfg(not(unix))]
    {
        Ok(status.code())
    }
}

// ---------------------------------------------------------------------------
// External tools discovery (fd, rg)
// ---------------------------------------------------------------------------

pub async fn ensure_tool(tool: ManagedTool, _silent: bool) -> Result<Option<String>, pi_js::Error> {
    let binary = match tool {
        ManagedTool::Fd => "fd",
        ManagedTool::Rg => "rg",
    };
    let exe_name = if pi_js::env::platform() == "win32" {
        format!("{binary}.exe")
    } else {
        binary.to_string()
    };

    let managed = pi_js::path::join(&[&get_bin_dir(), &exe_name]);
    if pi_js::fs::access(&managed, pi_js::fs::constants::F_OK).is_ok() {
        return Ok(Some(managed));
    }

    if let Some(on_path) = find_executable_on_path(&exe_name) {
        return Ok(Some(on_path));
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// Image detection & resizing
// ---------------------------------------------------------------------------

pub async fn detect_supported_image_mime_type_from_file(path: &str) -> Result<Option<String>, pi_js::Error> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| pi_js::Error::msg(format!("Failed to open image file {path}: {e}")))?;
    let mut buf = [0u8; 16];
    let n = file
        .read(&mut buf)
        .await
        .map_err(|e| pi_js::Error::msg(format!("Failed to read image header {path}: {e}")))?;
    Ok(detect_supported_image_mime_type(&buf[..n]))
}

pub fn detect_supported_image_mime_type(bytes: &[u8]) -> Option<String> {
    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return Some("image/jpeg".to_string());
    }
    if bytes.len() >= 8 && bytes[..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A] {
        return Some("image/png".to_string());
    }
    if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        return Some("image/gif".to_string());
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp".to_string());
    }
    None
}

pub fn format_dimension_note(result: &ResizedImage) -> Option<String> {
    if !result.was_resized || result.width == 0 {
        return None;
    }
    let scale = result.original_width as f64 / result.width as f64;
    Some(format!(
        "[Image: original {}x{}, displayed at {}x{}. Multiply coordinates by {:.2} to map to original image.]",
        result.original_width, result.original_height, result.width, result.height, scale
    ))
}

fn encode_png(img: &image::DynamicImage) -> Result<Vec<u8>, pi_js::Error> {
    let rgba = img.to_rgba8();
    let mut out = Vec::new();
    let encoder = PngEncoder::new(&mut out);
    encoder
        .write_image(&rgba, img.width(), img.height(), ColorType::Rgba8.into())
        .map_err(|e| pi_js::Error::msg(format!("Failed to encode PNG: {e}")))?;
    Ok(out)
}

fn encode_jpeg(img: &image::DynamicImage, quality: u8) -> Result<Vec<u8>, pi_js::Error> {
    let rgb = img.to_rgb8();
    let mut out = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
    encoder
        .encode(&rgb, img.width(), img.height(), ColorType::Rgb8.into())
        .map_err(|e| pi_js::Error::msg(format!("Failed to encode JPEG: {e}")))?;
    Ok(out)
}

pub async fn resize_image(
    img: ImageContent,
    options: ResizeImageOptions,
) -> Result<Option<ResizedImage>, pi_js::Error> {
    if let Some(override_fn) = RESIZE_IMAGE_SLOT.get() {
        return override_fn(img, options).await;
    }

    let raw_bytes = pi_js::b64::decode(&img.data);

    let decoded = match image::load_from_memory(&raw_bytes) {
        Ok(d) => d,
        Err(_) => {
            return Ok(Some(ResizedImage {
                data: img.data,
                mime_type: img.mime_type,
                original_width: 0,
                original_height: 0,
                width: 0,
                height: 0,
                was_resized: false,
            }));
        }
    };

    let orig_w = decoded.width();
    let orig_h = decoded.height();

    if orig_w <= options.max_width && orig_h <= options.max_height && raw_bytes.len() <= options.max_bytes {
        return Ok(Some(ResizedImage {
            data: img.data,
            mime_type: img.mime_type,
            original_width: orig_w,
            original_height: orig_h,
            width: orig_w,
            height: orig_h,
            was_resized: false,
        }));
    }

    let mut target_w = orig_w;
    let mut target_h = orig_h;
    if target_w > options.max_width && target_w > 0 {
        target_h = ((target_h as u64 * options.max_width as u64) / target_w as u64).max(1) as u32;
        target_w = options.max_width;
    }
    if target_h > options.max_height && target_h > 0 {
        target_w = ((target_w as u64 * options.max_height as u64) / target_h as u64).max(1) as u32;
        target_h = options.max_height;
    }

    let try_encode = |w: u32, h: u32, quality: u8| -> Result<(Vec<u8>, String), pi_js::Error> {
        let resized = decoded.resize_exact(w, h, FilterType::Lanczos3);
        let png_bytes = encode_png(&resized)?;
        let jpeg_bytes = encode_jpeg(&resized, quality)?;
        if png_bytes.len() <= jpeg_bytes.len() {
            Ok((png_bytes, "image/png".to_string()))
        } else {
            Ok((jpeg_bytes, "image/jpeg".to_string()))
        }
    };

    let (mut best_bytes, mut best_mime) = try_encode(target_w, target_h, options.jpeg_quality)?;
    let mut best_w = target_w;
    let mut best_h = target_h;

    if best_bytes.len() > options.max_bytes {
        let quality_steps = [options.jpeg_quality, 85, 70, 55, 40];
        let mut found = false;
        for q in quality_steps {
            let resized = decoded.resize_exact(best_w, best_h, FilterType::Lanczos3);
            let jpeg_bytes = encode_jpeg(&resized, q)?;
            best_bytes = jpeg_bytes;
            best_mime = "image/jpeg".to_string();
            if best_bytes.len() <= options.max_bytes {
                found = true;
                break;
            }
        }
        if !found {
            let mut scale = 0.75_f64;
            while scale >= 0.25 {
                let w = ((target_w as f64) * scale).round().max(1.0) as u32;
                let h = ((target_h as f64) * scale).round().max(1.0) as u32;
                let resized = decoded.resize_exact(w, h, FilterType::Lanczos3);
                for q in [options.jpeg_quality, 70, 55, 40] {
                    let jpeg_bytes = encode_jpeg(&resized, q)?;
                    best_bytes = jpeg_bytes;
                    best_mime = "image/jpeg".to_string();
                    best_w = w;
                    best_h = h;
                    if best_bytes.len() <= options.max_bytes {
                        found = true;
                        break;
                    }
                }
                if found {
                    break;
                }
                scale -= 0.25;
            }
        }
    }

    if best_bytes.len() > options.max_bytes {
        return Ok(None);
    }

    Ok(Some(ResizedImage {
        data: pi_js::b64::encode(&best_bytes),
        mime_type: best_mime,
        original_width: orig_w,
        original_height: orig_h,
        width: best_w,
        height: best_h,
        was_resized: true,
    }))
}

// ---------------------------------------------------------------------------
// Minimatch glob matcher
// ---------------------------------------------------------------------------

pub struct Minimatch {
    regex: fancy_regex::Regex,
}

impl Minimatch {
    pub fn new(pattern: &str, dot: bool) -> Result<Self, pi_js::Error> {
        let expanded = expand_braces(pattern);
        let mut alt_parts = Vec::with_capacity(expanded.len());
        for part in expanded {
            alt_parts.push(glob_to_regex(&part, dot));
        }
        let combined = format!("^(?:{})$", alt_parts.join("|"));
        let regex = fancy_regex::Regex::new(&combined)
            .map_err(|e| pi_js::Error::msg(format!("Invalid glob pattern {pattern:?}: {e}")))?;
        Ok(Self { regex })
    }

    pub fn is_match(&self, input: &str) -> bool {
        let normalized = input.replace('\\', "/");
        self.regex.is_match(&normalized).unwrap_or(false)
    }
}

fn expand_braces(pattern: &str) -> Vec<String> {
    let bytes = pattern.as_bytes();
    let mut depth = 0usize;
    let mut open_idx = None;
    let mut comma_indices = Vec::new();
    let mut close_idx = None;

    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && bytes[i - 1] == b'\\' {
            continue;
        }
        if b == b'{' {
            if depth == 0 {
                open_idx = Some(i);
                comma_indices.clear();
            }
            depth += 1;
        } else if b == b',' && depth == 1 {
            comma_indices.push(i);
        } else if b == b'}' && depth > 0 {
            depth -= 1;
            if depth == 0 && open_idx.is_some() {
                close_idx = Some(i);
                break;
            }
        }
    }

    if let (Some(open), Some(close)) = (open_idx, close_idx)
        && !comma_indices.is_empty()
    {
        let prefix = &pattern[..open];
        let suffix = &pattern[close + 1..];
        let mut results = Vec::new();
        let mut prev = open + 1;
        for c in comma_indices {
            let item = &pattern[prev..c];
            for r in expand_braces(&format!("{prefix}{item}{suffix}")) {
                results.push(r);
            }
            prev = c + 1;
        }
        let last_item = &pattern[prev..close];
        for r in expand_braces(&format!("{prefix}{last_item}{suffix}")) {
            results.push(r);
        }
        return results;
    }

    vec![pattern.to_string()]
}

fn glob_to_regex(pattern: &str, dot: bool) -> String {
    let chars: Vec<char> = pattern.replace('\\', "/").chars().collect();
    let mut out = String::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        match c {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    let prev_slash_or_start = i == 0 || chars[i - 1] == '/';
                    let next_slash = i + 2 < chars.len() && chars[i + 2] == '/';
                    if prev_slash_or_start && next_slash {
                        if dot {
                            out.push_str("(?:.*/)?");
                        } else {
                            out.push_str("(?:(?:[^/.][^/]*/)*)");
                        }
                        i += 3;
                        continue;
                    }
                    if dot {
                        out.push_str(".*");
                    } else {
                        out.push_str("(?:(?!(?:^|/)\\.)[^])*");
                    }
                    i += 2;
                    continue;
                }
                let at_segment_start = i == 0 || chars[i - 1] == '/';
                if !dot && at_segment_start {
                    out.push_str("(?!\\.)[^/]*");
                } else {
                    out.push_str("[^/]*");
                }
                i += 1;
            }
            '?' => {
                let at_segment_start = i == 0 || chars[i - 1] == '/';
                if !dot && at_segment_start {
                    out.push_str("(?!\\.)[^/]");
                } else {
                    out.push_str("[^/]");
                }
                i += 1;
            }
            '[' => {
                if let Some(rel_end) = chars[i + 1..].iter().position(|&x| x == ']') {
                    let end = i + 1 + rel_end;
                    let inside: String = chars[i + 1..end].iter().collect();
                    let body = if let Some(rest) = inside.strip_prefix('!') {
                        format!("[^{rest}]")
                    } else {
                        format!("[{inside}]")
                    };
                    out.push_str(&body);
                    i = end + 1;
                } else {
                    out.push_str("\\[");
                    i += 1;
                }
            }
            '.' | '+' | '^' | '$' | '(' | ')' | '{' | '}' | '|' => {
                out.push('\\');
                out.push(c);
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }

    let _ = Path::new("");
    out
}
