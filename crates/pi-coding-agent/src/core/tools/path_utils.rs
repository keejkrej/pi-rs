//! Port of packages/coding-agent/src/core/tools/path-utils.ts

use std::sync::LazyLock;

use regex::{Captures, Regex};

const NARROW_NO_BREAK_SPACE: &str = "\u{202F}";

static UNICODE_SPACES_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\u{00A0}\u{2000}-\u{200A}\u{202F}\u{205F}\u{3000}]").unwrap_or_else(|_| unreachable!())
});

static MACOS_AM_PM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i) (AM|PM)\b").unwrap_or_else(|_| unreachable!()));

fn file_exists(file_path: &str) -> bool {
    pi_js::fs::access(file_path, pi_js::fs::constants::F_OK).is_ok()
}

fn normalize_unicode_spaces(s: &str) -> String {
    UNICODE_SPACES_RE.replace_all(s, " ").into_owned()
}

fn normalize_at_prefix(s: &str) -> &str {
    s.strip_prefix('@').unwrap_or(s)
}

/// Expand `~`, strip optional leading `@`, and normalize Unicode spaces in a user-supplied path.
pub fn expand_path(file_path: &str) -> String {
    let normalized = normalize_unicode_spaces(normalize_at_prefix(file_path));
    if normalized == "~" {
        return pi_js::env::home_dir();
    }
    if let Some(rest) = normalized.strip_prefix("~/") {
        // home_dir + "/" + rest keeps exact string join parity with os.homedir() + normalized.slice(1)
        let home = pi_js::env::home_dir();
        if home.ends_with('/') || home.ends_with('\\') {
            return format!("{home}{rest}");
        }
        return format!("{home}/{rest}");
    }
    normalized
}

/// Resolve a path relative to `cwd` after `expand_path`.
pub fn resolve_to_cwd(file_path: &str, cwd: &str) -> String {
    let expanded = expand_path(file_path);
    if pi_js::path::is_absolute(&expanded) {
        expanded
    } else {
        pi_js::path::resolve(&[cwd, &expanded])
    }
}

/// Resolve a path for reading, trying macOS screenshot / NFD / curly-quote filename variants if needed.
pub fn resolve_read_path(file_path: &str, cwd: &str) -> String {
    let resolved = resolve_to_cwd(file_path, cwd);

    if file_exists(&resolved) {
        return resolved;
    }

    // macOS screenshots insert U+202F narrow no-break space before AM/PM
    let am_pm_variant = MACOS_AM_PM_RE
        .replace_all(&resolved, |caps: &Captures<'_>| {
            format!("{NARROW_NO_BREAK_SPACE}{}", &caps[1])
        })
        .into_owned();
    if am_pm_variant != resolved && file_exists(&am_pm_variant) {
        return am_pm_variant;
    }

    // macOS stores HFS+/APFS paths in NFD Unicode form
    let nfd_variant = pi_js::str16::normalize_nfd(&resolved);
    if nfd_variant != resolved && file_exists(&nfd_variant) {
        return nfd_variant;
    }

    // macOS French/typographic screenshots replace straight apostrophe with U+2019
    let curly_variant = resolved.replace('\'', "\u{2019}");
    if curly_variant != resolved && file_exists(&curly_variant) {
        return curly_variant;
    }

    let curly_nfd_variant = pi_js::str16::normalize_nfd(&curly_variant);
    if curly_nfd_variant != resolved && file_exists(&curly_nfd_variant) {
        return curly_nfd_variant;
    }

    resolved
}
