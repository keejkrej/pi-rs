//! pi_js::env (Rust-only; API contract: PORTING.md Appendix A).
//!
//! `process.env`, `os.homedir()`, `os.tmpdir()`, `process.cwd()`, `process.chdir()`,
//! `process.platform`, `process.arch`, `os.release()`, `os.version()` and `process.pid`.
//!
//! Environment model (three layers, last wins):
//! 1. the real OS environment read through `std::env::vars_os` (never mutated: the
//!    workspace forbids mutating the real process environment);
//! 2. a process-global overlay written by [`set_var`] / [`remove_var`] (like assigning to
//!    `process.env` in Node, the writes are visible to every thread and flow into child
//!    processes through [`vars`]);
//! 3. a thread-local layer installed by [`testing::EnvGuard`] (`vi.stubEnv`). While at
//!    least one `EnvGuard` is alive on the current thread, [`set_var`] / [`remove_var`]
//!    write to this thread-local layer as well, so code under test cannot leak env
//!    changes into other tests. The layer is discarded when the last guard drops.
//!
//! On Windows keys compare case-insensitively (as `process.env` does), and keys starting
//! with `=` (per-drive cwd entries such as `=C:`) are readable through [`var`] but not
//! enumerated by [`vars`], matching Node.

use std::cell::{Cell, RefCell};
use std::sync::{LazyLock, Mutex};

use indexmap::IndexMap;

use crate::error::{Error, NodeError, Result};

/// Normalized lookup key: uppercase on Windows, unchanged elsewhere.
fn norm_key(k: &str) -> String {
    if cfg!(windows) { k.to_uppercase() } else { k.to_string() }
}

/// One overlay entry: original-case key plus value (`None` = deleted).
#[derive(Clone, Debug)]
struct Entry {
    key: String,
    value: Option<String>,
}

static OVERLAY: LazyLock<Mutex<IndexMap<String, Entry>>> = LazyLock::new(|| Mutex::new(IndexMap::new()));

thread_local! {
    static TL_LAYER: RefCell<IndexMap<String, Entry>> = RefCell::new(IndexMap::new());
    static TL_GUARDS: Cell<usize> = const { Cell::new(0) };
    static TL_PLATFORM: Cell<Option<&'static str>> = const { Cell::new(None) };
    static TL_CWD: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn os_var(k: &str) -> Option<String> {
    // `getenv` cannot see keys that are empty or contain NUL; on unix a key containing
    // `=` can never be a variable name either.
    if k.is_empty() || k.contains('\0') || (!cfg!(windows) && k.contains('=')) {
        return None;
    }
    std::env::var_os(k).map(|v| v.to_string_lossy().into_owned())
}

/// `process.env[k]`.
pub fn var(k: &str) -> Option<String> {
    let nk = norm_key(k);
    if let Some(e) = TL_LAYER.with(|l| l.borrow().get(&nk).cloned()) {
        return e.value;
    }
    if let Some(e) = OVERLAY.lock().unwrap().get(&nk).cloned() {
        return e.value;
    }
    os_var(k)
}

fn apply_layer(out: &mut IndexMap<String, (String, String)>, layer: &IndexMap<String, Entry>) {
    for (nk, e) in layer {
        match &e.value {
            Some(v) => {
                if let Some(slot) = out.get_mut(nk) {
                    // Existing keys keep their position (JS property order).
                    slot.1 = v.clone();
                } else {
                    out.insert(nk.clone(), (e.key.clone(), v.clone()));
                }
            }
            None => {
                out.shift_remove(nk);
            }
        }
    }
}

/// `{ ...process.env }`: every visible variable in enumeration order.
pub fn vars() -> IndexMap<String, String> {
    let mut out: IndexMap<String, (String, String)> = IndexMap::new();
    for (k, v) in std::env::vars_os() {
        let k = k.to_string_lossy().into_owned();
        if cfg!(windows) && k.starts_with('=') {
            continue;
        }
        let v = v.to_string_lossy().into_owned();
        out.insert(norm_key(&k), (k, v));
    }
    apply_layer(&mut out, &OVERLAY.lock().unwrap());
    TL_LAYER.with(|l| apply_layer(&mut out, &l.borrow()));
    out.into_values().collect()
}

fn write_entry(k: &str, value: Option<String>) {
    let nk = norm_key(k);
    let entry = Entry {
        key: k.to_string(),
        value,
    };
    if TL_GUARDS.with(|g| g.get()) > 0 {
        TL_LAYER.with(|l| {
            l.borrow_mut().insert(nk, entry);
        });
    } else {
        OVERLAY.lock().unwrap().insert(nk, entry);
    }
}

/// `process.env[k] = v`.
pub fn set_var(k: &str, v: &str) {
    write_entry(k, Some(v.to_string()));
}

/// `delete process.env[k]`.
pub fn remove_var(k: &str) {
    write_entry(k, None);
}

/// `os.homedir()`.
///
/// libuv returns `HOME` (`USERPROFILE` on Windows) whenever it is set, even when empty,
/// and otherwise falls back to the password database / user profile directory.
pub fn home_dir() -> String {
    if cfg!(windows) {
        if let Some(h) = var("USERPROFILE") {
            return h;
        }
    } else if let Some(h) = var("HOME") {
        return h;
    }
    home_dir_fallback()
}

#[cfg(unix)]
fn home_dir_fallback() -> String {
    match nix::unistd::User::from_uid(nix::unistd::getuid()) {
        Ok(Some(user)) => user.dir.to_string_lossy().into_owned(),
        _ => String::new(),
    }
}

#[cfg(not(unix))]
fn home_dir_fallback() -> String {
    std::env::home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `os.tmpdir()`.
pub fn tmp_dir() -> String {
    if platform_is_windows_build() {
        let non_empty = |k: &str| var(k).filter(|v| !v.is_empty());
        let path = non_empty("TEMP").or_else(|| non_empty("TMP")).unwrap_or_else(|| {
            let root = non_empty("SystemRoot")
                .or_else(|| non_empty("windir"))
                .unwrap_or_else(|| "undefined".to_string());
            format!("{root}\\temp")
        });
        let units: Vec<u16> = path.encode_utf16().collect();
        if units.len() > 1 && units[units.len() - 1] == u16::from(b'\\') && units[units.len() - 2] != u16::from(b':') {
            return String::from_utf16_lossy(&units[..units.len() - 1]);
        }
        return path;
    }
    for k in ["TMPDIR", "TMP", "TEMP"] {
        if let Some(mut dir) = var(k).filter(|v| !v.is_empty()) {
            if dir.len() > 1 && dir.ends_with('/') {
                dir.pop();
            }
            return dir;
        }
    }
    "/tmp".to_string()
}

fn platform_is_windows_build() -> bool {
    cfg!(windows)
}

/// `process.cwd()`. A [`testing::CwdGuard`] on the current thread overrides it.
pub fn cwd() -> String {
    if let Some(c) = TL_CWD.with(|c| c.borrow().clone()) {
        return c;
    }
    real_cwd()
}

fn real_cwd() -> String {
    match std::env::current_dir() {
        Ok(p) => p.to_string_lossy().into_owned(),
        // PORT: Node throws `ENOENT: ..., uv_cwd` when the cwd was deleted; the contract
        // returns a String, so fall back to $PWD, then the root.
        Err(_) => var("PWD")
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| if cfg!(windows) { "C:\\".into() } else { "/".into() }),
    }
}

fn chdir_error(e: &std::io::Error, from: &str, to: &str) -> Error {
    let (code, errno, desc) = crate::fs::uv_error_info(e);
    Error::Node(NodeError {
        code: code.to_string(),
        errno,
        syscall: "chdir".to_string(),
        path: Some(from.to_string()),
        dest: Some(to.to_string()),
        message: format!("{code}: {desc}, chdir '{from}' -> '{to}'"),
    })
}

/// `process.chdir(p)`.
///
/// Errors use Node's format, e.g. `ENOENT: no such file or directory, chdir '/cur' -> 'x'`.
/// While a [`testing::CwdGuard`] is active on this thread, only the thread-local cwd moves
/// (to the canonical path of the target, like `uv_cwd` after a real `chdir`).
pub fn chdir(p: &str) -> Result<()> {
    let guarded = TL_CWD.with(|c| c.borrow().clone());
    match guarded {
        Some(current) => {
            let target = if crate::path::is_absolute(p) {
                p.to_string()
            } else {
                crate::path::resolve(&[&current, p])
            };
            let result = if p.is_empty() {
                Err(std::io::Error::from_raw_os_error(enoent_raw()))
            } else {
                std::fs::metadata(&target).and_then(|m| {
                    if m.is_dir() {
                        std::fs::canonicalize(&target)
                    } else {
                        Err(std::io::Error::from_raw_os_error(enotdir_raw()))
                    }
                })
            };
            match result {
                Ok(canon) => {
                    let canon = strip_verbatim(&canon.to_string_lossy());
                    TL_CWD.with(|c| *c.borrow_mut() = Some(canon));
                    Ok(())
                }
                Err(e) => Err(chdir_error(&e, &current, p)),
            }
        }
        None => {
            let res = if p.is_empty() {
                Err(std::io::Error::from_raw_os_error(enoent_raw()))
            } else {
                std::env::set_current_dir(p)
            };
            res.map_err(|e| chdir_error(&e, &real_cwd(), p))
        }
    }
}

fn strip_verbatim(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{rest}")
    } else if let Some(rest) = p.strip_prefix("\\\\?\\") {
        rest.to_string()
    } else {
        p.to_string()
    }
}

fn enoent_raw() -> i32 {
    #[cfg(unix)]
    {
        libc::ENOENT
    }
    #[cfg(not(unix))]
    {
        2 // ERROR_FILE_NOT_FOUND
    }
}

fn enotdir_raw() -> i32 {
    #[cfg(unix)]
    {
        libc::ENOTDIR
    }
    #[cfg(not(unix))]
    {
        267 // ERROR_DIRECTORY
    }
}

/// `process.platform` (`"darwin" | "linux" | "win32" | ...`), overridable per thread with
/// [`testing::PlatformGuard`].
pub fn platform() -> &'static str {
    if let Some(p) = TL_PLATFORM.with(|p| p.get()) {
        return p;
    }
    native_platform()
}

fn native_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" | "ios" => "darwin",
        "windows" => "win32",
        "solaris" | "illumos" => "sunos",
        other => other,
    }
}

/// `process.arch` (`"x64" | "arm64" | "ia32" | ...`).
pub fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        "powerpc" => "ppc",
        "powerpc64" => "ppc64",
        "loongarch64" => "loong64",
        "mips64" => "mips64el",
        "mips" => "mipsel",
        other => other,
    }
}

/// `os.release()` (e.g. `"24.1.0"` on macOS, `"6.8.0-45-generic"` on Linux,
/// `"10.0.22631"` on Windows).
pub fn os_release() -> String {
    os_info::release()
}

/// `os.version()` (the kernel version string, e.g. `"Darwin Kernel Version 24.1.0: ..."`).
pub fn os_version() -> String {
    os_info::version()
}

/// `os.hostname()`.
pub fn hostname() -> String {
    os_info::hostname()
}

#[cfg(unix)]
mod os_info {
    pub fn release() -> String {
        nix::sys::utsname::uname()
            .map(|u| u.release().to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn version() -> String {
        nix::sys::utsname::uname()
            .map(|u| u.version().to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn hostname() -> String {
        nix::unistd::gethostname()
            .map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

#[cfg(not(unix))]
mod os_info {
    use std::sync::LazyLock;

    /// `cmd /c ver` prints e.g. `Microsoft Windows [Version 10.0.22631.4317]`.
    static VER: LazyLock<String> = LazyLock::new(|| {
        std::process::Command::new("cmd")
            .args(["/d", "/c", "ver"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    });

    pub fn release() -> String {
        // PORT: libuv reads RtlGetVersion (`major.minor.build`); without unsafe FFI we parse `ver`.
        let ver = VER.as_str();
        let digits: String = ver
            .split("Version ")
            .nth(1)
            .unwrap_or("")
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        digits.split('.').take(3).collect::<Vec<_>>().join(".")
    }

    pub fn version() -> String {
        // PORT: libuv reports the product name from the registry (e.g. "Windows 11 Pro").
        let ver = VER.as_str();
        ver.split(" [").next().unwrap_or("").to_string()
    }

    pub fn hostname() -> String {
        super::var("COMPUTERNAME").unwrap_or_default()
    }
}

/// `process.pid`.
pub fn pid() -> u32 {
    std::process::id()
}

/// Test seams for `vi.stubEnv`, `Object.defineProperty(process, "platform")` and
/// `process.chdir`. All guards are thread-local and restore the previous state on drop.
pub mod testing {
    use super::*;

    /// `vi.stubEnv(k, v)`: a thread-local env override (`None` hides the variable).
    ///
    /// While any `EnvGuard` is alive on the thread, `set_var` / `remove_var` also write to
    /// the thread-local layer; everything is discarded when the last guard drops.
    #[must_use = "the override is removed when the guard is dropped"]
    pub struct EnvGuard {
        saved: Vec<(String, Option<Entry>)>,
    }

    impl EnvGuard {
        /// Overrides `k` with `v` (or hides it when `v` is `None`).
        pub fn set(k: &str, v: Option<&str>) -> EnvGuard {
            let mut g = EnvGuard::empty();
            g.stub(k, v);
            g
        }

        /// Same as [`EnvGuard::set`].
        pub fn new(k: &str, v: Option<&str>) -> EnvGuard {
            EnvGuard::set(k, v)
        }

        /// Overrides several variables at once.
        pub fn set_many(pairs: &[(&str, Option<&str>)]) -> EnvGuard {
            let mut g = EnvGuard::empty();
            for (k, v) in pairs {
                g.stub(k, *v);
            }
            g
        }

        /// A guard without overrides; it still routes `set_var` to the thread-local layer.
        pub fn empty() -> EnvGuard {
            TL_GUARDS.with(|g| g.set(g.get() + 1));
            EnvGuard { saved: Vec::new() }
        }

        /// Adds one more override to this guard.
        pub fn stub(&mut self, k: &str, v: Option<&str>) -> &mut Self {
            let nk = norm_key(k);
            let prev = TL_LAYER.with(|l| {
                l.borrow_mut().insert(
                    nk.clone(),
                    Entry {
                        key: k.to_string(),
                        value: v.map(str::to_string),
                    },
                )
            });
            self.saved.push((nk, prev));
            self
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            TL_LAYER.with(|l| {
                let mut l = l.borrow_mut();
                for (nk, prev) in self.saved.drain(..).rev() {
                    match prev {
                        Some(e) => {
                            l.insert(nk, e);
                        }
                        None => {
                            l.shift_remove(&nk);
                        }
                    }
                }
            });
            let remaining = TL_GUARDS.with(|g| {
                let n = g.get().saturating_sub(1);
                g.set(n);
                n
            });
            if remaining == 0 {
                TL_LAYER.with(|l| l.borrow_mut().clear());
            }
        }
    }

    /// Overrides `process.platform` on this thread.
    #[must_use = "the override is removed when the guard is dropped"]
    pub struct PlatformGuard {
        prev: Option<&'static str>,
    }

    impl PlatformGuard {
        pub fn set(platform: &'static str) -> PlatformGuard {
            let prev = TL_PLATFORM.with(|p| p.replace(Some(platform)));
            PlatformGuard { prev }
        }

        pub fn new(platform: &'static str) -> PlatformGuard {
            PlatformGuard::set(platform)
        }
    }

    impl Drop for PlatformGuard {
        fn drop(&mut self) {
            TL_PLATFORM.with(|p| p.set(self.prev));
        }
    }

    /// Overrides `process.cwd()` on this thread (the real process cwd is untouched).
    /// Relative paths passed to `pi_js::fs` and `pi_js::path::resolve` resolve against it.
    #[must_use = "the override is removed when the guard is dropped"]
    pub struct CwdGuard {
        prev: Option<String>,
    }

    impl CwdGuard {
        pub fn set(dir: impl Into<String>) -> CwdGuard {
            let prev = TL_CWD.with(|c| c.replace(Some(dir.into())));
            CwdGuard { prev }
        }

        pub fn new(dir: impl Into<String>) -> CwdGuard {
            CwdGuard::set(dir)
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let prev = self.prev.take();
            TL_CWD.with(|c| *c.borrow_mut() = prev);
        }
    }

    /// Whether a [`CwdGuard`] is active on this thread.
    pub fn cwd_overridden() -> bool {
        TL_CWD.with(|c| c.borrow().is_some())
    }
}
