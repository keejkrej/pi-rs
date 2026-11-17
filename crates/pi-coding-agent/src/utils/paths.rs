//! Port of packages/coding-agent/src/utils/paths.ts

use std::sync::LazyLock;

use fancy_regex::Regex;

use crate::utils::child_process::{SpawnProcessOptions, spawn_process_sync};

static UNICODE_SPACES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\u{00A0}\u{2000}-\u{200A}\u{202F}\u{205F}\u{3000}]").expect("valid regex"));
static WIN_SHELL_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^/(?:mnt/|cygdrive/)?([a-z])(?:/(.*))?$").expect("valid regex"));

#[derive(Clone, Debug, Default)]
pub struct PathInputOptions {
    /// Trim leading/trailing whitespace before normalization.
    pub trim: Option<bool>,
    /// Expand leading `~` to a home directory. Defaults to true.
    pub expand_tilde: Option<bool>,
    /// Home directory used for `~` expansion. Defaults to `pi_js::env::home_dir()`.
    pub home_dir: Option<String>,
    /// Strip a leading `@`, used for CLI `@file` paths.
    pub strip_at_prefix: Option<bool>,
    /// Normalize unicode space variants to regular spaces.
    pub normalize_unicode_spaces: Option<bool>,
}

/// Resolve a path to its canonical (real) form, following symlinks.
/// Falls back to the raw path if resolution fails (e.g. the target does
/// not exist yet), so that callers never crash on missing filesystem
/// entries.
pub fn canonicalize_path(path: &str) -> String {
    pi_js::fs::realpath(path).unwrap_or_else(|_| path.to_owned())
}

fn resolve_for_platform(path: &str) -> String {
    if pi_js::env::platform() == "win32" {
        pi_js::path::win32::resolve(&[path])
    } else {
        pi_js::path::posix::resolve(&[path])
    }
}

fn sep_for_platform() -> &'static str {
    if pi_js::env::platform() == "win32" {
        pi_js::path::win32::sep()
    } else {
        pi_js::path::posix::sep()
    }
}

pub fn get_path_comparison_key(path: &str) -> String {
    let resolved = resolve_for_platform(path);
    if pi_js::env::platform() == "win32" {
        resolved.to_ascii_lowercase()
    } else {
        resolved
    }
}

pub fn paths_equal(left: &str, right: &str) -> bool {
    get_path_comparison_key(left) == get_path_comparison_key(right)
}

pub fn is_path_within(parent: &str, child: &str) -> bool {
    let parent_key = get_path_comparison_key(parent);
    let child_key = get_path_comparison_key(child);
    if child_key == parent_key {
        return true;
    }
    let path_sep = sep_for_platform();
    let prefix = if parent_key.ends_with(path_sep) {
        parent_key
    } else {
        format!("{parent_key}{path_sep}")
    };
    child_key.starts_with(&prefix)
}

pub fn shorten_home_path(path: &str, home_dir: Option<&str>) -> String {
    let owned_home;
    let home = match home_dir {
        Some(h) => h,
        None => {
            owned_home = pi_js::env::home_dir();
            &owned_home
        }
    };
    let is_win = pi_js::env::platform() == "win32";
    let is_abs = if is_win {
        pi_js::path::win32::is_absolute(path)
    } else {
        pi_js::path::posix::is_absolute(path)
    };
    if home.is_empty() || !is_abs || !is_path_within(home, path) {
        return path.to_owned();
    }
    let rel = if is_win {
        pi_js::path::win32::relative(home, path)
    } else {
        pi_js::path::posix::relative(home, path)
    };
    let sep = sep_for_platform();
    if rel.is_empty() {
        "~".to_owned()
    } else {
        format!("~{sep}{rel}")
    }
}

pub fn get_file_revision(path: &str) -> Option<String> {
    let stats = pi_js::fs::stat(path).ok()?;
    Some(format!(
        "{}:{}:{}:{}:{}",
        stats.dev, stats.ino, stats.size, stats.mtime_ns, stats.ctime_ns
    ))
}

/// Returns true if the value is NOT a package source (npm:, git:, etc.),
/// a built-in extension (builtin:), or a remote URL protocol. Bare names,
/// relative paths, and file: URLs are considered local.
pub fn is_local_path(value: &str) -> bool {
    let trimmed = value.trim();
    !(trimmed.starts_with("npm:")
        || trimmed.starts_with("git:")
        || trimmed.starts_with("github:")
        || trimmed.starts_with("http:")
        || trimmed.starts_with("https:")
        || trimmed.starts_with("ssh:")
        || trimmed.starts_with("builtin:"))
}

/// Convert Git Bash, MSYS, Cygwin, and WSL drive paths to a form native Windows APIs accept.
pub fn normalize_windows_shell_path(file_path: &str) -> String {
    if !file_path.starts_with('/') || file_path.starts_with("//") || file_path.contains('\\') {
        return file_path.to_owned();
    }
    let Ok(Some(caps)) = WIN_SHELL_PATH_RE.captures(file_path) else {
        return file_path.to_owned();
    };
    let Some(drive) = caps.get(1).map(|m| m.as_str().to_ascii_uppercase()) else {
        return file_path.to_owned();
    };
    let suffix = caps.get(2).map(|m| m.as_str().replace('/', "\\")).unwrap_or_default();
    format!("{drive}:\\{suffix}")
}

pub fn normalize_path(input: &str, options: PathInputOptions) -> String {
    let mut normalized = if options.trim.unwrap_or(false) {
        pi_js::str16::trim(input).to_owned()
    } else {
        input.to_owned()
    };

    if options.normalize_unicode_spaces.unwrap_or(false) {
        normalized = UNICODE_SPACES.replace_all(&normalized, " ").into_owned();
    }
    if options.strip_at_prefix.unwrap_or(false) && normalized.starts_with('@') {
        normalized = normalized[1..].to_owned();
    }
    if pi_js::env::platform() == "win32" {
        normalized = normalize_windows_shell_path(&normalized);
    }

    if options.expand_tilde.unwrap_or(true) {
        let home = options.home_dir.unwrap_or_else(pi_js::env::home_dir);
        if normalized == "~" {
            return home;
        }
        if let Some(rest) = normalized.strip_prefix("~/") {
            return pi_js::path::join(&[&home, rest]);
        }
        if pi_js::env::platform() == "win32"
            && let Some(rest) = normalized.strip_prefix("~\\")
        {
            return pi_js::path::join(&[&home, rest]);
        }
    }

    if normalized.starts_with("file://")
        && let Ok(parsed) = url::Url::parse(&normalized)
        && let Ok(p) = parsed.to_file_path()
    {
        return p.to_string_lossy().into_owned();
    }

    normalized
}

pub fn resolve_path(input: &str, base_dir: Option<&str>, options: PathInputOptions) -> String {
    let normalized = normalize_path(input, options);
    let base_owned;
    let base = match base_dir {
        Some(dir) => dir,
        None => {
            base_owned = pi_js::env::cwd();
            &base_owned
        }
    };
    let normalized_base_dir = normalize_path(base, PathInputOptions::default());
    if pi_js::path::is_absolute(&normalized) {
        pi_js::path::resolve(&[&normalized])
    } else {
        pi_js::path::resolve(&[&normalized_base_dir, &normalized])
    }
}

pub fn get_cwd_relative_path(file_path: &str, cwd: &str) -> Option<String> {
    let resolved_cwd = resolve_path(cwd, None, PathInputOptions::default());
    let resolved_path = resolve_path(file_path, Some(&resolved_cwd), PathInputOptions::default());
    let relative_path = pi_js::path::relative(&resolved_cwd, &resolved_path);
    let sep = pi_js::path::sep();
    let is_inside_cwd = relative_path.is_empty()
        || (relative_path != ".."
            && !relative_path.starts_with(&format!("..{sep}"))
            && !pi_js::path::is_absolute(&relative_path));

    if is_inside_cwd {
        Some(if relative_path.is_empty() {
            ".".to_owned()
        } else {
            relative_path
        })
    } else {
        None
    }
}

pub fn format_path_relative_to_cwd_or_absolute(file_path: &str, cwd: &str) -> String {
    let absolute_path = resolve_path(file_path, Some(cwd), PathInputOptions::default());
    let candidate = get_cwd_relative_path(&absolute_path, cwd).unwrap_or(absolute_path);
    let sep = pi_js::path::sep();
    if sep == "/" {
        candidate
    } else {
        candidate.split(sep).collect::<Vec<_>>().join("/")
    }
}

pub fn mark_path_ignored_by_cloud_sync(path: &str) {
    let platform = pi_js::env::platform();
    let attrs: &[&str] = match platform {
        "darwin" => &["com.dropbox.ignored", "com.apple.fileprovider.ignore#P"],
        "linux" => &["user.com.dropbox.ignored"],
        _ => &[],
    };

    for &attr in attrs {
        if platform == "darwin" {
            let _ = spawn_process_sync("xattr", &["-w", attr, "1", path], SpawnProcessOptions::default());
        } else {
            let _ = spawn_process_sync(
                "setfattr",
                &["-n", attr, "-v", "1", path],
                SpawnProcessOptions::default(),
            );
        }
    }
}
