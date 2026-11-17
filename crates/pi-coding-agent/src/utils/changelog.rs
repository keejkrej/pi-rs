//! Port of packages/coding-agent/src/utils/changelog.ts

use std::sync::LazyLock;

use fancy_regex::Regex;

pub use crate::config::get_changelog_path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangelogEntry {
    pub major: i64,
    pub minor: i64,
    pub patch: i64,
    pub content: String,
}

const GITHUB_REPO: &str = "earendil-works/pi";
const CHANGELOG_LINK_BASE_PATH: &str = "packages/coding-agent";

static LEGACY_REPO_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https://github\.com/(?:badlogic|earendil-works)/pi-mono(?=/|$)").expect("valid regex")
});
static URL_SCHEME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[a-z][a-z0-9+.-]*:").expect("valid regex"));
static INLINE_MARKDOWN_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(!?\[[^\]\n]+\]\()([^\s)]+)((?:\s+[^)]*)?\))").expect("valid regex"));
static VERSION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"##\s+\[?(\d+)\.(\d+)\.(\d+)\]?").expect("valid regex"));

fn entry_version(entry: &ChangelogEntry) -> String {
    format!("{}.{}.{}", entry.major, entry.minor, entry.patch)
}

fn normalize_tag(version: &str) -> String {
    if version.starts_with('v') {
        version.to_owned()
    } else {
        format!("v{version}")
    }
}

fn split_local_target(target: &str) -> (&str, &str, &str) {
    let (before_hash, fragment) = match target.find('#') {
        Some(idx) => (&target[..idx], &target[idx..]),
        None => (target, ""),
    };
    let (path_part, query) = match before_hash.find('?') {
        Some(idx) => (&before_hash[..idx], &before_hash[idx..]),
        None => (before_hash, ""),
    };
    (fragment, path_part, query)
}

fn resolve_repository_path(target_path: &str) -> Option<String> {
    let normalized_target = target_path.replace('\\', "/");
    let joined = if normalized_target.starts_with('/') {
        pi_js::path::posix::normalize(normalized_target.trim_start_matches('/'))
    } else {
        let base_joined = pi_js::path::posix::join(&[CHANGELOG_LINK_BASE_PATH, &normalized_target]);
        pi_js::path::posix::normalize(&base_joined)
    };

    if joined == "." || joined.starts_with("../") || joined == ".." {
        return None;
    }

    Some(joined)
}

fn is_directory_target(original_path: &str, repository_path: &str) -> bool {
    if original_path.ends_with('/') {
        return true;
    }
    let basename = pi_js::path::posix::basename(repository_path, None);
    !basename.contains('.')
}

fn normalize_changelog_link_target(target: &str, tag: &str) -> String {
    let repo_url = format!("https://github.com/{GITHUB_REPO}");
    let mut canonical_target = LEGACY_REPO_RE.replace(target, repo_url.as_str()).into_owned();

    for route in ["blob", "tree"] {
        for branch in ["main", "master"] {
            let floating_ref_prefix = format!("{repo_url}/{route}/{branch}/");
            if let Some(rest) = canonical_target.strip_prefix(&floating_ref_prefix) {
                canonical_target = format!("{repo_url}/{route}/{tag}/{rest}");
            }
        }
    }

    if canonical_target.starts_with('#')
        || canonical_target.starts_with("//")
        || URL_SCHEME_RE.is_match(&canonical_target).unwrap_or(false)
    {
        return canonical_target;
    }

    let (fragment, path_part, query) = split_local_target(&canonical_target);
    if path_part.is_empty() {
        return canonical_target;
    }

    let Some(repository_path) = resolve_repository_path(path_part) else {
        return canonical_target;
    };

    let route = if is_directory_target(path_part, &repository_path) {
        "tree"
    } else {
        "blob"
    };
    let encoded = pi_js::uri::encode_uri(&repository_path);
    format!("https://github.com/{GITHUB_REPO}/{route}/{tag}/{encoded}{query}{fragment}")
}

pub fn normalize_changelog_links(markdown: &str, version: &str) -> String {
    let tag = normalize_tag(version);
    let mut out = String::with_capacity(markdown.len());
    let mut last = 0;
    for caps in INLINE_MARKDOWN_LINK_RE.captures_iter(markdown).flatten() {
        if let (Some(full), Some(prefix), Some(target), Some(suffix)) =
            (caps.get(0), caps.get(1), caps.get(2), caps.get(3))
        {
            out.push_str(&markdown[last..full.start()]);
            out.push_str(prefix.as_str());
            out.push_str(&normalize_changelog_link_target(target.as_str(), &tag));
            out.push_str(suffix.as_str());
            last = full.end();
        }
    }
    out.push_str(&markdown[last..]);
    out
}

pub fn normalize_changelog_links_for_entry(markdown: &str, entry: &ChangelogEntry) -> String {
    normalize_changelog_links(markdown, &entry_version(entry))
}

/// Parse changelog entries from CHANGELOG.md.
/// Scans for `## ` lines and collects content until next `## ` or EOF.
pub fn parse_changelog(changelog_path: &str) -> Vec<ChangelogEntry> {
    if !pi_js::fs::exists(changelog_path) {
        return Vec::new();
    }

    let content = match pi_js::fs::read_to_string(changelog_path) {
        Ok(c) => c,
        Err(err) => {
            pi_js::console::error(&format!("Warning: Could not parse changelog: {err}"));
            return Vec::new();
        }
    };

    let mut entries = Vec::new();
    let mut current_lines: Vec<String> = Vec::new();
    let mut current_version: Option<(i64, i64, i64)> = None;

    for line in content.split('\n') {
        if line.starts_with("## ") {
            if let Some((major, minor, patch)) = current_version.take()
                && !current_lines.is_empty()
            {
                entries.push(ChangelogEntry {
                    major,
                    minor,
                    patch,
                    content: pi_js::str16::trim(&current_lines.join("\n")).to_owned(),
                });
            }

            if let Ok(Some(caps)) = VERSION_RE.captures(line)
                && let (Some(maj), Some(min), Some(pat)) = (caps.get(1), caps.get(2), caps.get(3))
                && let (Ok(major), Ok(minor), Ok(patch)) = (
                    maj.as_str().parse::<i64>(),
                    min.as_str().parse::<i64>(),
                    pat.as_str().parse::<i64>(),
                )
            {
                current_version = Some((major, minor, patch));
                current_lines = vec![line.to_owned()];
            } else {
                current_version = None;
                current_lines.clear();
            }
        } else if current_version.is_some() {
            current_lines.push(line.to_owned());
        }
    }

    if let Some((major, minor, patch)) = current_version
        && !current_lines.is_empty()
    {
        entries.push(ChangelogEntry {
            major,
            minor,
            patch,
            content: pi_js::str16::trim(&current_lines.join("\n")).to_owned(),
        });
    }

    entries
}

/// Compare versions. Returns < 0 if v1 < v2, 0 if v1 === v2, > 0 if v1 > v2.
pub fn compare_versions(v1: &ChangelogEntry, v2: &ChangelogEntry) -> i64 {
    if v1.major != v2.major {
        return v1.major - v2.major;
    }
    if v1.minor != v2.minor {
        return v1.minor - v2.minor;
    }
    v1.patch - v2.patch
}

/// Get entries newer than `last_version`.
pub fn get_new_entries(entries: &[ChangelogEntry], last_version: &str) -> Vec<ChangelogEntry> {
    let mut parts = last_version.split('.');
    let parse_part = |s: Option<&str>| -> i64 {
        s.and_then(|v| {
            let n = pi_js::num::number(v);
            if n.is_finite() { Some(n as i64) } else { None }
        })
        .unwrap_or(0)
    };
    let last = ChangelogEntry {
        major: parse_part(parts.next()),
        minor: parse_part(parts.next()),
        patch: parse_part(parts.next()),
        content: String::new(),
    };

    entries
        .iter()
        .filter(|entry| compare_versions(entry, &last) > 0)
        .cloned()
        .collect()
}
