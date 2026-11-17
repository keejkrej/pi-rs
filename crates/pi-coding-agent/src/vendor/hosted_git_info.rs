//! Port of vendor hosted-git-info@9.0.3 (PORTING.md §2.6).

use std::sync::{LazyLock, Mutex};

use indexmap::IndexMap;
use url::Url;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostedGitOptions {
    pub no_committish: bool,
    pub no_git_plus: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHost {
    pub r#type: String,
    pub domain: String,
    pub treepath: Option<String>,
    pub blobpath: Option<String>,
    pub editpath: Option<String>,
    pub user: Option<String>,
    pub auth: Option<String>,
    pub project: String,
    pub committish: Option<String>,
    pub default_representation: Option<String>,
    pub opts: HostedGitOptions,
}

static CACHE: LazyLock<Mutex<IndexMap<String, Option<GitHost>>>> = LazyLock::new(|| Mutex::new(IndexMap::new()));

fn maybe_join(parts: &[Option<&str>]) -> String {
    if parts.iter().all(|p| p.is_some_and(|s| !s.is_empty())) {
        parts.iter().flatten().copied().collect::<Vec<_>>().join("")
    } else {
        String::new()
    }
}

fn maybe_join_owned(parts: &[String]) -> String {
    if parts.iter().all(|s| !s.is_empty()) {
        parts.join("")
    } else {
        String::new()
    }
}

fn maybe_encode(arg: Option<&str>) -> String {
    arg.filter(|s| !s.is_empty())
        .map(pi_js::uri::encode_uri_component)
        .unwrap_or_default()
}

fn format_hash_fragment(f: &str) -> String {
    let lower = f.to_ascii_lowercase();
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let trimmed_start = lower.trim_start_matches(|c: char| !is_word(c));
    let mut trimmed = trimmed_start.to_owned();
    // Strip trailing non-word chars that are preceded by a word char (matching `/(?<!\W)\W+$/`)
    while trimmed.chars().last().is_some_and(|c| !is_word(c)) && trimmed.chars().any(is_word) {
        trimmed.pop();
    }
    let no_slash = trimmed.replace('/', "");
    let mut out = String::with_capacity(no_slash.len());
    let mut in_nonword = false;
    for ch in no_slash.chars() {
        if is_word(ch) {
            if in_nonword {
                out.push('-');
                in_nonword = false;
            }
            out.push(ch);
        } else {
            in_nonword = true;
        }
    }
    if in_nonword {
        out.push('-');
    }
    out
}

fn is_known_shortcut(proto: &str) -> Option<&'static str> {
    match proto {
        "github:" => Some("github"),
        "bitbucket:" => Some("bitbucket"),
        "gitlab:" => Some("gitlab"),
        "gist:" => Some("gist"),
        "sourcehut:" => Some("sourcehut"),
        _ => None,
    }
}

fn host_by_domain(domain: &str) -> Option<&'static str> {
    let normalized = domain.strip_prefix("www.").unwrap_or(domain);
    match normalized {
        "github.com" => Some("github"),
        "bitbucket.org" => Some("bitbucket"),
        "gitlab.com" => Some("gitlab"),
        "gist.github.com" => Some("gist"),
        "git.sr.ht" => Some("sourcehut"),
        _ => None,
    }
}

fn host_domain(host_type: &str) -> &'static str {
    match host_type {
        "github" => "github.com",
        "bitbucket" => "bitbucket.org",
        "gitlab" => "gitlab.com",
        "gist" => "gist.github.com",
        "sourcehut" => "git.sr.ht",
        _ => "",
    }
}

fn protocol_has_auth(proto: &str) -> bool {
    matches!(proto, "git+https:" | "git:" | "http:" | "https:" | "git+http:")
}

fn protocol_name(proto: &str) -> &str {
    match proto {
        "git+ssh:" | "ssh:" => "sshurl",
        "git+https:" => "https",
        "github:" => "github",
        "bitbucket:" => "bitbucket",
        "gitlab:" => "gitlab",
        "gist:" => "gist",
        "sourcehut:" => "sourcehut",
        other => other.trim_end_matches(':'),
    }
}

fn host_accepts_protocol(host_type: &str, proto: &str) -> bool {
    match host_type {
        "github" => matches!(proto, "git:" | "http:" | "git+ssh:" | "git+https:" | "ssh:" | "https:"),
        "bitbucket" => matches!(proto, "git+ssh:" | "git+https:" | "ssh:" | "https:"),
        "gitlab" => matches!(proto, "git+ssh:" | "git+https:" | "ssh:" | "https:"),
        "gist" => matches!(proto, "git:" | "git+ssh:" | "git+https:" | "ssh:" | "https:"),
        "sourcehut" => matches!(proto, "git+ssh:" | "https:"),
        _ => false,
    }
}

fn last_index_of_before(s: &str, ch: char, before_ch: char) -> isize {
    let slice = match s.find(before_ch) {
        Some(pos) => &s[..pos],
        None => s,
    };
    slice.rfind(ch).map(|i| i as isize).unwrap_or(-1)
}

fn correct_protocol(arg: &str) -> String {
    let Some(first_colon) = arg.find(':') else {
        return arg.to_owned();
    };
    let proto = &arg[..=first_colon];
    if is_known_shortcut(proto).is_some()
        || matches!(
            proto,
            "git+ssh:" | "ssh:" | "git+https:" | "git:" | "http:" | "https:" | "git+http:"
        )
    {
        return arg.to_owned();
    }
    if arg[first_colon..].starts_with("://") {
        return arg.to_owned();
    }
    if let Some(first_at) = arg.find('@') {
        if first_at > first_colon {
            return format!("git+ssh://{arg}");
        }
        return arg.to_owned();
    }
    format!("{}//{}", &arg[..=first_colon], &arg[first_colon + 1..])
}

fn correct_url(mut giturl: String) -> String {
    let first_at = last_index_of_before(&giturl, '@', '#');
    let last_colon = last_index_of_before(&giturl, ':', '#');
    if last_colon > first_at && last_colon >= 0 {
        let idx = last_colon as usize;
        giturl = format!("{}/{}", &giturl[..idx], &giturl[idx + 1..]);
    }
    if last_index_of_before(&giturl, ':', '#') == -1 && !giturl.contains("//") {
        giturl = format!("git+ssh://{giturl}");
    }
    giturl
}

fn safe_url(raw: &str) -> Option<Url> {
    let parsed = Url::parse(raw).ok()?;
    if parsed.scheme().is_empty() {
        return None;
    }
    Some(parsed)
}

pub fn parse_url(giturl: &str) -> Option<Url> {
    safe_url(giturl).or_else(|| safe_url(&correct_url(giturl.to_owned())))
}

fn is_github_shorthand(arg: &str) -> bool {
    let first_hash = arg.find('#');
    let first_slash = arg.find('/');
    let Some(fs) = first_slash else {
        return false;
    };
    if fs == 0 {
        return false;
    }
    let second_slash = arg[fs + 1..].find('/').map(|i| fs + 1 + i);
    let first_colon = arg.find(':');
    let first_space = arg.find(char::is_whitespace);
    let first_at = arg.find('@');

    let space_ok = first_space.is_none_or(|p| first_hash.is_some_and(|h| p > h));
    let at_ok = first_at.is_none_or(|p| first_hash.is_some_and(|h| p > h));
    let colon_ok = first_colon.is_none_or(|p| first_hash.is_some_and(|h| p > h));
    let second_slash_ok = second_slash.is_none_or(|p| first_hash.is_some_and(|h| p > h));
    let does_not_end_with_slash = match first_hash {
        Some(h) => h == 0 || arg.as_bytes()[h - 1] != b'/',
        None => !arg.ends_with('/'),
    };
    let does_not_start_with_dot = !arg.starts_with('.');

    space_ok && does_not_end_with_slash && does_not_start_with_dot && at_ok && colon_ok && second_slash_ok
}

fn decode_component_opt(raw: &str) -> Result<Option<String>, ()> {
    if raw.is_empty() {
        return Ok(None);
    }
    pi_js::uri::decode_uri_component(raw).map(Some).map_err(|_| ())
}

fn extract_host_segments(host_type: &str, url: &Url) -> Option<(Option<String>, String, Option<String>)> {
    let hash_committish = url.fragment().filter(|f| !f.is_empty()).map(str::to_owned);
    let pathname = url.path();
    let mut parts = pathname.split('/');
    let _empty = parts.next();
    match host_type {
        "github" => {
            let user = parts.next()?.to_owned();
            let mut project = parts.next()?.to_owned();
            let aux = parts.next();
            let committish = parts.next().map(str::to_owned);
            if let Some(t) = aux
                && t != "tree"
            {
                return None;
            }
            if let Some(stripped) = project.strip_suffix(".git") {
                project = stripped.to_owned();
            }
            if user.is_empty() || project.is_empty() {
                return None;
            }
            let c = if aux.is_some() { committish } else { hash_committish };
            Some((Some(user), project, c))
        }
        "bitbucket" => {
            let user = parts.next()?.to_owned();
            let mut project = parts.next()?.to_owned();
            if parts.next() == Some("get") {
                return None;
            }
            if let Some(stripped) = project.strip_suffix(".git") {
                project = stripped.to_owned();
            }
            if user.is_empty() || project.is_empty() {
                return None;
            }
            Some((Some(user), project, hash_committish))
        }
        "gitlab" => {
            let path = pathname.strip_prefix('/').unwrap_or(pathname);
            if path.contains("/-/") || path.contains("/archive.tar.gz") {
                return None;
            }
            let mut segments: Vec<&str> = path.split('/').collect();
            let mut project = segments.pop()?.to_owned();
            if let Some(stripped) = project.strip_suffix(".git") {
                project = stripped.to_owned();
            }
            let user = segments.join("/");
            if user.is_empty() || project.is_empty() {
                return None;
            }
            Some((Some(user), project, hash_committish))
        }
        "gist" => {
            let first = parts.next().map(str::to_owned);
            let second = parts.next().map(str::to_owned);
            let aux = parts.next();
            if aux == Some("raw") {
                return None;
            }
            let (user, mut project) = match (first, second) {
                (Some(u), Some(p)) if !p.is_empty() => (Some(u), p),
                (Some(p), _) if !p.is_empty() => (None, p),
                _ => return None,
            };
            if let Some(stripped) = project.strip_suffix(".git") {
                project = stripped.to_owned();
            }
            Some((user, project, hash_committish))
        }
        "sourcehut" => {
            let user = parts.next()?.to_owned();
            let mut project = parts.next()?.to_owned();
            if parts.next() == Some("archive") {
                return None;
            }
            if let Some(stripped) = project.strip_suffix(".git") {
                project = stripped.to_owned();
            }
            if user.is_empty() || project.is_empty() {
                return None;
            }
            Some((Some(user), project, hash_committish))
        }
        _ => None,
    }
}

fn unknown_hosted_url(raw: &str) -> Option<String> {
    let parsed = Url::parse(raw).ok()?;
    let host = parsed.host_str()?;
    let proto = if parsed.scheme().ends_with("+http") || parsed.scheme() == "http" {
        "http:"
    } else {
        "https:"
    };
    let path = parsed.path().strip_suffix(".git").unwrap_or(parsed.path());
    Some(format!("{proto}//{host}{path}"))
}

impl GitHost {
    pub fn from_url(giturl: &str, opts: Option<HostedGitOptions>) -> Option<Self> {
        let opts = opts.unwrap_or_default();
        let key = format!("{}:{}:{}", giturl, opts.no_committish, opts.no_git_plus);
        if let Ok(cache) = CACHE.lock()
            && let Some(cached) = cache.get(&key)
        {
            return cached.clone();
        }

        let computed = Self::from_url_uncached(giturl, opts);
        if let Ok(mut cache) = CACHE.lock() {
            if cache.len() >= 1000 {
                cache.shift_remove_index(0);
            }
            cache.insert(key, computed.clone());
        }
        computed
    }

    fn from_url_uncached(giturl: &str, opts: HostedGitOptions) -> Option<Self> {
        if giturl.is_empty() {
            return None;
        }
        let corrected = if is_github_shorthand(giturl) {
            format!("github:{giturl}")
        } else {
            giturl.to_owned()
        };
        let with_proto = correct_protocol(&corrected);
        let parsed = parse_url(&with_proto)?;
        let proto = format!("{}:", parsed.scheme());
        let shortcut_host = is_known_shortcut(&proto);
        let domain_host = parsed.host_str().and_then(host_by_domain);
        let host_type = shortcut_host.or(domain_host)?;

        let auth = if protocol_has_auth(&proto) && (!parsed.username().is_empty() || parsed.password().is_some()) {
            Some(match parsed.password() {
                Some(pw) if !pw.is_empty() => format!("{}:{pw}", parsed.username()),
                _ => parsed.username().to_owned(),
            })
        } else {
            None
        };

        let (user, project, committish, default_rep) = if shortcut_host.is_some() {
            let mut pathname = parsed.path().strip_prefix('/').unwrap_or(parsed.path()).to_owned();
            // For `github:user/repo`, url crate parses `user` as path root if no `//` was given
            if let Some(at) = pathname.find('@') {
                pathname = pathname[at + 1..].to_owned();
            }
            let (u, mut p) = if let Some(last_slash) = pathname.rfind('/') {
                let u_dec = decode_component_opt(&pathname[..last_slash]).ok()?;
                let p_dec = decode_component_opt(&pathname[last_slash + 1..])
                    .ok()?
                    .unwrap_or_default();
                (u_dec.filter(|s| !s.is_empty()), p_dec)
            } else {
                let p_dec = decode_component_opt(&pathname).ok()?.unwrap_or_default();
                (None, p_dec)
            };
            if let Some(stripped) = p.strip_suffix(".git") {
                p = stripped.to_owned();
            }
            let c = parsed
                .fragment()
                .filter(|f| !f.is_empty())
                .and_then(|f| decode_component_opt(f).ok().flatten());
            (u, p, c, Some("shortcut".to_owned()))
        } else {
            if !host_accepts_protocol(host_type, &proto) {
                return None;
            }
            let (u_raw, p_raw, c_raw) = extract_host_segments(host_type, &parsed)?;
            let u = match u_raw {
                Some(s) => decode_component_opt(&s).ok()?,
                None => None,
            };
            let p = decode_component_opt(&p_raw).ok()?.unwrap_or_default();
            let c = match c_raw {
                Some(s) => decode_component_opt(&s).ok()?,
                None => None,
            };
            (u, p, c, Some(protocol_name(&proto).to_owned()))
        };

        let (treepath, blobpath, editpath) = match host_type {
            "github" => (
                Some("tree".to_owned()),
                Some("blob".to_owned()),
                Some("edit".to_owned()),
            ),
            "bitbucket" => (
                Some("src".to_owned()),
                Some("src".to_owned()),
                Some("?mode=edit".to_owned()),
            ),
            "gitlab" => (
                Some("tree".to_owned()),
                Some("tree".to_owned()),
                Some("-/edit".to_owned()),
            ),
            "gist" => (None, None, Some("edit".to_owned())),
            "sourcehut" => (Some("tree".to_owned()), Some("tree".to_owned()), None),
            _ => (None, None, None),
        };

        Some(GitHost {
            r#type: host_type.to_owned(),
            domain: host_domain(host_type).to_owned(),
            treepath,
            blobpath,
            editpath,
            user,
            auth,
            project,
            committish,
            default_representation: default_rep,
            opts,
        })
    }

    pub fn from_manifest_url(manifest_repo_url: &str, opts: Option<HostedGitOptions>) -> Option<Self> {
        let stripped = manifest_repo_url.strip_prefix("git+").unwrap_or(manifest_repo_url);
        if let Some(info) = Self::from_url(stripped, opts.clone()) {
            return Some(info);
        }
        let unk = unknown_hosted_url(manifest_repo_url)?;
        Self::from_url(&unk, opts)
    }

    fn effective_committish(&self) -> Option<&str> {
        if self.opts.no_committish {
            None
        } else {
            self.committish.as_deref()
        }
    }

    fn finish(&self, out: String) -> String {
        if self.opts.no_git_plus && out.starts_with("git+") {
            out[4..].to_owned()
        } else {
            out
        }
    }

    pub fn hash(&self) -> String {
        self.committish
            .as_deref()
            .filter(|c| !c.is_empty())
            .map(|c| format!("#{c}"))
            .unwrap_or_default()
    }

    pub fn ssh(&self) -> Option<String> {
        let c = self.effective_committish();
        let out = if self.r#type == "gist" {
            format!(
                "git@{}:{}.git{}",
                self.domain,
                self.project,
                maybe_join(&[Some("#"), c])
            )
        } else {
            let u = self.user.as_deref()?;
            format!(
                "git@{}:{u}/{}.git{}",
                self.domain,
                self.project,
                maybe_join(&[Some("#"), c])
            )
        };
        Some(self.finish(out))
    }

    pub fn sshurl(&self) -> Option<String> {
        let c = self.effective_committish();
        let out = if self.r#type == "gist" {
            format!(
                "git+ssh://git@{}/{}.git{}",
                self.domain,
                self.project,
                maybe_join(&[Some("#"), c])
            )
        } else {
            let u = self.user.as_deref()?;
            format!(
                "git+ssh://git@{}/{u}/{}.git{}",
                self.domain,
                self.project,
                maybe_join(&[Some("#"), c])
            )
        };
        Some(self.finish(out))
    }

    pub fn https(&self) -> Option<String> {
        let c = self.effective_committish();
        let out = if self.r#type == "gist" {
            format!(
                "git+https://{}/{}.git{}",
                self.domain,
                self.project,
                maybe_join(&[Some("#"), c])
            )
        } else if self.r#type == "sourcehut" {
            let u = self.user.as_deref()?;
            format!(
                "https://{}/{u}/{}{}",
                self.domain,
                self.project,
                maybe_join(&[Some("#"), c])
            )
        } else {
            let u = self.user.as_deref()?;
            format!(
                "git+https://{}{}/{u}/{}.git{}",
                maybe_join(&[self.auth.as_deref(), Some("@")]),
                self.domain,
                self.project,
                maybe_join(&[Some("#"), c])
            )
        };
        Some(self.finish(out))
    }

    pub fn git(&self) -> Option<String> {
        let c = self.effective_committish();
        let out = match self.r#type.as_str() {
            "github" => {
                let u = self.user.as_deref()?;
                format!(
                    "git://{}{}/{u}/{}.git{}",
                    maybe_join(&[self.auth.as_deref(), Some("@")]),
                    self.domain,
                    self.project,
                    maybe_join(&[Some("#"), c])
                )
            }
            "gist" => format!(
                "git://{}/{}.git{}",
                self.domain,
                self.project,
                maybe_join(&[Some("#"), c])
            ),
            _ => return None,
        };
        Some(self.finish(out))
    }

    pub fn shortcut(&self) -> String {
        let c = self.effective_committish();
        let out = if self.r#type == "gist" {
            format!("{}:{}{}", self.r#type, self.project, maybe_join(&[Some("#"), c]))
        } else {
            format!(
                "{}:{}/{}{}",
                self.r#type,
                self.user.as_deref().unwrap_or(""),
                self.project,
                maybe_join(&[Some("#"), c])
            )
        };
        self.finish(out)
    }

    pub fn path(&self) -> String {
        let c = self.effective_committish();
        let out = if self.r#type == "gist" {
            format!("{}{}", self.project, maybe_join(&[Some("#"), c]))
        } else {
            format!(
                "{}/{}{}",
                self.user.as_deref().unwrap_or(""),
                self.project,
                maybe_join(&[Some("#"), c])
            )
        };
        self.finish(out)
    }

    pub fn browse(&self, path: Option<&str>, fragment: Option<&str>) -> Option<String> {
        let c = self.effective_committish();
        match path {
            None => {
                let out = if self.r#type == "gist" {
                    format!(
                        "https://{}/{}{}",
                        self.domain,
                        self.project,
                        maybe_join_owned(&["/".to_owned(), maybe_encode(c)])
                    )
                } else {
                    let u = self.user.as_deref()?;
                    format!(
                        "https://{}/{u}/{}{}",
                        self.domain,
                        self.project,
                        maybe_join_owned(&[
                            "/".to_owned(),
                            self.treepath.clone().unwrap_or_default(),
                            "/".to_owned(),
                            maybe_encode(c)
                        ])
                    )
                };
                Some(self.finish(out))
            }
            Some(raw_path) => {
                let p = raw_path.strip_prefix('/').unwrap_or(raw_path);
                let out = if self.r#type == "gist" {
                    let hash = format!("file-{}", format_hash_fragment(p));
                    format!(
                        "https://{}/{}{}#{}",
                        self.domain,
                        self.project,
                        maybe_join_owned(&["/".to_owned(), maybe_encode(c)]),
                        hash
                    )
                } else {
                    let u = self.user.as_deref()?;
                    let tp = self.treepath.as_deref().unwrap_or("tree");
                    let frag = format_hash_fragment(fragment.unwrap_or(""));
                    format!(
                        "https://{}/{u}/{}/{tp}/{}/{p}{}",
                        self.domain,
                        self.project,
                        maybe_encode(Some(c.unwrap_or("HEAD"))),
                        maybe_join_owned(&["#".to_owned(), frag])
                    )
                };
                Some(self.finish(out))
            }
        }
    }

    pub fn bugs(&self) -> Option<String> {
        if self.r#type == "sourcehut" {
            return None;
        }
        let out = if self.r#type == "gist" {
            format!("https://{}/{}", self.domain, self.project)
        } else {
            let u = self.user.as_deref()?;
            format!("https://{}/{u}/{}/issues", self.domain, self.project)
        };
        Some(self.finish(out))
    }

    pub fn tarball(&self) -> Option<String> {
        let c = self.committish.as_deref();
        let out = match self.r#type.as_str() {
            "github" => format!(
                "https://codeload.{}/{}/{}/tar.gz/{}",
                self.domain,
                self.user.as_deref()?,
                self.project,
                maybe_encode(Some(c.unwrap_or("HEAD")))
            ),
            "bitbucket" => format!(
                "https://{}/{}/{}/get/{}.tar.gz",
                self.domain,
                self.user.as_deref()?,
                self.project,
                maybe_encode(Some(c.unwrap_or("HEAD")))
            ),
            "gitlab" => {
                let full = format!("{}/{}", self.user.as_deref()?, self.project);
                format!(
                    "https://{}/api/v4/projects/{}/repository/archive.tar.gz?sha={}",
                    self.domain,
                    maybe_encode(Some(&full)),
                    maybe_encode(Some(c.unwrap_or("HEAD")))
                )
            }
            "gist" => format!(
                "https://codeload.github.com/gist/{}/tar.gz/{}",
                self.project,
                maybe_encode(Some(c.unwrap_or("HEAD")))
            ),
            "sourcehut" => format!(
                "https://{}/{}/{}/archive/{}.tar.gz",
                self.domain,
                self.user.as_deref()?,
                self.project,
                c.map(|v| maybe_encode(Some(v)))
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "HEAD".to_owned())
            ),
            _ => return None,
        };
        Some(self.finish(out))
    }

    pub fn get_default_representation(&self) -> Option<&str> {
        self.default_representation.as_deref()
    }
}

pub fn from_url(giturl: &str, opts: Option<HostedGitOptions>) -> Option<GitHost> {
    GitHost::from_url(giturl, opts)
}
