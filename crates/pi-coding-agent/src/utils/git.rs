//! Port of packages/coding-agent/src/utils/git.ts

use std::sync::LazyLock;

use fancy_regex::Regex;

use crate::vendor::hosted_git_info::from_url as hosted_git_from_url;

static SCP_LIKE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^git@([^:]+):(.+)$").expect("valid regex"));
static EXPLICIT_PROTOCOL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(https?|ssh|git)://").expect("valid regex"));

/// Parsed git URL information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitSource {
    pub r#type: String,
    pub repo: String,
    pub host: String,
    pub path: String,
    pub ref_name: Option<String>,
    pub pinned: bool,
}

fn split_ref(raw: &str) -> (String, Option<String>) {
    if let Ok(Some(caps)) = SCP_LIKE_RE.captures(raw)
        && let (Some(host), Some(path_with_maybe_ref)) = (caps.get(1), caps.get(2))
    {
        let p = path_with_maybe_ref.as_str();
        if let Some(sep_idx) = p.find('@') {
            let repo_path = &p[..sep_idx];
            let r = &p[sep_idx + 1..];
            if !repo_path.is_empty() && !r.is_empty() {
                return (format!("git@{}:{repo_path}", host.as_str()), Some(r.to_owned()));
            }
        }
        return (raw.to_owned(), None);
    }

    if raw.contains("://") {
        if let Ok(mut parsed) = url::Url::parse(raw) {
            let path_with_maybe_ref = parsed.path().trim_start_matches('/').to_owned();
            if let Some(sep_idx) = path_with_maybe_ref.find('@') {
                let repo_path = &path_with_maybe_ref[..sep_idx];
                let r = &path_with_maybe_ref[sep_idx + 1..];
                if !repo_path.is_empty() && !r.is_empty() {
                    parsed.set_path(&format!("/{repo_path}"));
                    let repo = parsed.to_string().trim_end_matches('/').to_owned();
                    return (repo, Some(r.to_owned()));
                }
            }
        }
        return (raw.to_owned(), None);
    }

    let Some(slash_idx) = raw.find('/') else {
        return (raw.to_owned(), None);
    };
    let host = &raw[..slash_idx];
    let path_with_maybe_ref = &raw[slash_idx + 1..];
    let Some(sep_idx) = path_with_maybe_ref.find('@') else {
        return (raw.to_owned(), None);
    };
    let repo_path = &path_with_maybe_ref[..sep_idx];
    let r = &path_with_maybe_ref[sep_idx + 1..];
    if repo_path.is_empty() || r.is_empty() {
        return (raw.to_owned(), None);
    }
    (format!("{host}/{repo_path}"), Some(r.to_owned()))
}

fn has_unsafe_git_install_part(value: &str, allow_slash: bool) -> bool {
    let Ok(decoded) = pi_js::uri::decode_uri_component(value) else {
        return true;
    };
    for candidate in [value, decoded.as_str()] {
        if candidate.contains('\0') || candidate.contains('\\') || candidate.starts_with('/') {
            return true;
        }
        if !allow_slash && candidate.contains('/') {
            return true;
        }
        if candidate.split('/').any(|part| part == "..") {
            return true;
        }
    }
    false
}

fn build_git_source(repo: String, host: String, path: &str, ref_name: Option<String>) -> Option<GitSource> {
    if path.starts_with('/') {
        return None;
    }
    let normalized_path = path.strip_suffix(".git").unwrap_or(path).trim_start_matches('/');
    if host.is_empty() || normalized_path.is_empty() || normalized_path.split('/').count() < 2 {
        return None;
    }
    if has_unsafe_git_install_part(&host, false) || has_unsafe_git_install_part(normalized_path, true) {
        return None;
    }
    let pinned = ref_name.is_some();
    Some(GitSource {
        r#type: "git".to_owned(),
        repo,
        host,
        path: normalized_path.to_owned(),
        ref_name,
        pinned,
    })
}

fn parse_generic_git_url(raw: &str) -> Option<GitSource> {
    let (mut repo, ref_name) = split_ref(raw);
    let host: String;
    let path: String;

    if let Ok(Some(caps)) = SCP_LIKE_RE.captures(&repo) {
        host = caps.get(1).map(|m| m.as_str().to_owned()).unwrap_or_default();
        path = caps.get(2).map(|m| m.as_str().to_owned()).unwrap_or_default();
    } else if repo.starts_with("https://")
        || repo.starts_with("http://")
        || repo.starts_with("ssh://")
        || repo.starts_with("git://")
    {
        let parsed = url::Url::parse(&repo).ok()?;
        host = parsed.host_str().unwrap_or("").to_owned();
        path = parsed.path().trim_start_matches('/').to_owned();
    } else {
        let slash_idx = repo.find('/')?;
        host = repo[..slash_idx].to_owned();
        path = repo[slash_idx + 1..].to_owned();
        if !host.contains('.') && host != "localhost" {
            return None;
        }
        repo = format!("https://{repo}");
    }

    build_git_source(repo, host, &path, ref_name)
}

/// Parse git source into a [`GitSource`].
///
/// Rules:
/// - With `git:` prefix, accept all historical shorthand forms.
/// - Without `git:` prefix, only accept explicit protocol URLs.
pub fn parse_git_url(source: &str) -> Option<GitSource> {
    let trimmed = source.trim();
    let has_git_prefix = trimmed.starts_with("git:");
    let url_str = if has_git_prefix { trimmed[4..].trim() } else { trimmed };

    if !has_git_prefix && !EXPLICIT_PROTOCOL_RE.is_match(url_str).unwrap_or(false) {
        return None;
    }

    let (split_repo, split_ref_val) = split_ref(url_str);

    let mut hosted_candidates = Vec::new();
    if let Some(ref r) = split_ref_val {
        hosted_candidates.push(format!("{split_repo}#{r}"));
    }
    hosted_candidates.push(url_str.to_owned());

    for candidate in hosted_candidates {
        if let Some(info) = hosted_git_from_url(&candidate, None) {
            if split_ref_val.is_some() && info.project.contains('@') {
                continue;
            }
            let use_https_prefix = !(split_repo.starts_with("http://")
                || split_repo.starts_with("https://")
                || split_repo.starts_with("ssh://")
                || split_repo.starts_with("git://")
                || split_repo.starts_with("git@"));
            let repo = if use_https_prefix {
                format!("https://{split_repo}")
            } else {
                split_repo.clone()
            };
            let path = format!("{}/{}", info.user.unwrap_or_default(), info.project);
            let final_ref = info.committish.or_else(|| split_ref_val.clone());
            return build_git_source(repo, info.domain, &path, final_ref);
        }
    }

    let mut https_candidates = Vec::new();
    if let Some(ref r) = split_ref_val {
        https_candidates.push(format!("https://{split_repo}#{r}"));
    }
    https_candidates.push(format!("https://{url_str}"));

    for candidate in https_candidates {
        if let Some(info) = hosted_git_from_url(&candidate, None) {
            if split_ref_val.is_some() && info.project.contains('@') {
                continue;
            }
            let path = format!("{}/{}", info.user.unwrap_or_default(), info.project);
            let final_ref = info.committish.or_else(|| split_ref_val.clone());
            return build_git_source(format!("https://{split_repo}"), info.domain, &path, final_ref);
        }
    }

    parse_generic_git_url(url_str)
}
