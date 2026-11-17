//! Port of packages/coding-agent/src/extensions/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod codemode;
pub mod llama;
pub mod mcp;
pub mod tool_search;
// @generated-mods end

use crate::core::extensions::types::{InlineExtension, NamedInlineExtension};

/// Built-in inline extensions loaded alongside user and workspace extensions.
pub fn built_in_extensions() -> Vec<InlineExtension> {
    vec![
        InlineExtension::Named(NamedInlineExtension {
            name: "llama.cpp".to_string(),
            factory: llama::default_export(),
            hidden: false,
            replaceable: false,
            builtin: true,
        }),
        // Replaceable: an extension that registers `codemode`, `tool_search`, or `/mcp` (such as
        // a third-party MCP extension) takes over instead of running alongside the built-in one.
        InlineExtension::Named(NamedInlineExtension {
            name: "codemode".to_string(),
            factory: codemode::default_export(),
            hidden: false,
            replaceable: true,
            builtin: true,
        }),
        InlineExtension::Named(NamedInlineExtension {
            name: "tool-search".to_string(),
            factory: tool_search::default_export(),
            hidden: false,
            replaceable: true,
            builtin: true,
        }),
        InlineExtension::Named(NamedInlineExtension {
            name: "mcp".to_string(),
            factory: mcp::default_export(),
            hidden: false,
            replaceable: true,
            builtin: true,
        }),
    ]
}

/// Adapters for Stage 3A / Stage 3B modules that are not yet landed in this workspace.
// WIRE(@mcp-w3): demote back to pub(crate) once core::auth_storage and utils land
#[doc(hidden)]
pub mod wire {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, LazyLock, Mutex};
    use std::time::{Duration, SystemTime};

    use indexmap::IndexMap;
    use serde_json::Value;

    use crate::core::extensions::types::Theme;
    use crate::core::extensions::types::mcp_wire::{
        McpExposure, McpHttpServerConfig, McpOAuthConfig, McpServerConfig, McpStdioServerConfig, get_agent_dir_wire,
        resolve_path_wire,
    };
    use pi_ai::{Usage, UsageCost};
    use pi_js::abort::AbortSignal;
    use pi_js::{BoxFuture, Error, Result};
    use pi_tui::components::select_list::{SelectListStyleFn, SelectListTheme};
    use pi_tui::components::text::Text;
    use pi_tui::keybindings::get_keybindings;
    use pi_tui::tui::{Component, TuiMouseEvent, TuiMouseEventResult};
    use pi_tui::utils::truncate_to_width;

    // WIRE(@mcp-w3): wire to crate::config once Stage 3A lands
    pub const APP_NAME: &str = "pi";
    pub const VERSION: &str = env!("CARGO_PKG_VERSION");
    pub const CONFIG_DIR_NAME: &str = ".pi";

    pub fn get_agent_dir() -> String {
        get_agent_dir_wire()
    }

    pub fn get_docs_path() -> String {
        if let Some(dir) = option_env!("CARGO_MANIFEST_DIR") {
            let candidate = Path::new(dir).join("docs");
            if candidate.exists() {
                return candidate.to_string_lossy().into_owned();
            }
        }
        pi_js::path::join(&[&pi_js::env::cwd(), "docs"])
    }

    // WIRE(@mcp-w3): wire to crate::utils::text::strip_bom once landed
    pub fn strip_bom(content: &str) -> &str {
        content.strip_prefix('\u{FEFF}').unwrap_or(content)
    }

    pub fn is_record(value: &Value) -> bool {
        matches!(value, Value::Object(_))
    }

    // WIRE(@mcp-w3): wire to crate::utils::paths once landed
    pub fn resolve_path(input: &str, base_dir: &str) -> String {
        resolve_path_wire(input, base_dir)
    }

    pub fn canonicalize_path(path: &str) -> String {
        let p = Path::new(path);
        if let Ok(canon) = std::fs::canonicalize(p) {
            return canon.to_string_lossy().into_owned();
        }
        let mut remainder: Vec<std::ffi::OsString> = Vec::new();
        let mut current = p.to_path_buf();
        while let Some(name) = current.file_name() {
            remainder.push(name.to_os_string());
            if !current.pop() {
                break;
            }
            if let Ok(mut canon) = std::fs::canonicalize(&current) {
                for part in remainder.into_iter().rev() {
                    canon.push(part);
                }
                return canon.to_string_lossy().into_owned();
            }
        }
        pi_js::path::normalize(path)
    }

    // WIRE(@mcp-w3): replace directory lock with shared proper-lockfile utility once landed
    pub struct DirLockGuard {
        lock_dir: PathBuf,
    }

    impl Drop for DirLockGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir(&self.lock_dir);
        }
    }

    fn try_remove_stale_lock(lock_dir: &Path, stale_ms: u64) {
        if let Ok(meta) = std::fs::metadata(lock_dir)
            && let Ok(modified) = meta.modified()
            && let Ok(age) = SystemTime::now().duration_since(modified)
            && age >= Duration::from_millis(stale_ms)
        {
            let _ = std::fs::remove_dir(lock_dir);
        }
    }

    pub fn acquire_lock_sync(path: &Path) -> Result<DirLockGuard> {
        let lock_dir = PathBuf::from(format!("{}.lock", path.display()));
        let max_attempts = 200;
        for attempt in 1..=max_attempts {
            try_remove_stale_lock(&lock_dir, 30_000);
            match std::fs::create_dir(&lock_dir) {
                Ok(()) => return Ok(DirLockGuard { lock_dir }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && attempt < max_attempts => {
                    std::thread::yield_now();
                }
                Err(err) => {
                    return Err(Error::from_io(err, "lock", Some(&lock_dir.to_string_lossy()), None));
                }
            }
        }
        Err(Error::msg(format!("Failed to acquire lock for {}", path.display())))
    }

    pub async fn acquire_lock_async(path: &Path, signal: Option<&AbortSignal>) -> Result<DirLockGuard> {
        let lock_dir = PathBuf::from(format!("{}.lock", path.display()));
        let stale_ms = 30_000i64;
        let deadline = pi_js::time::now_ms() + stale_ms;
        let mut retry: u32 = 0;
        loop {
            if let Some(sig) = signal
                && sig.aborted()
            {
                return Err(Error::js("AbortError", "Aborted"));
            }
            try_remove_stale_lock(&lock_dir, stale_ms as u64);
            match std::fs::create_dir(&lock_dir) {
                Ok(()) => return Ok(DirLockGuard { lock_dir }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    let remaining = deadline - pi_js::time::now_ms();
                    if remaining <= 0 {
                        return Err(Error::from_io(err, "lock", Some(&lock_dir.to_string_lossy()), None));
                    }
                    let base = (10u64.saturating_mul(1u64.checked_shl(retry.min(10)).unwrap_or(1024))).min(1_000);
                    retry = retry.saturating_add(1);
                    let delay_ms = base.min(remaining as u64);
                    pi_js::time::sleep(delay_ms, signal).await?;
                }
                Err(err) => {
                    return Err(Error::from_io(err, "lock", Some(&lock_dir.to_string_lossy()), None));
                }
            }
        }
    }

    // WIRE(@mcp-w3): wire to crate::core::auth_storage once landed
    pub trait AuthStorageBackend: Send + Sync {
        fn run_locked(&self, cb: &mut dyn FnMut(Option<&str>) -> Result<Option<String>>) -> Result<()>;

        fn run_locked_async(
            &self,
            cb: Box<dyn FnOnce(Option<String>) -> BoxFuture<Result<Option<String>>> + Send>,
            signal: Option<AbortSignal>,
        ) -> BoxFuture<Result<()>>;
    }

    pub fn auth_with_lock<T, F>(backend: &dyn AuthStorageBackend, mut f: F) -> Result<T>
    where
        F: FnMut(Option<&str>) -> Result<(T, Option<String>)>,
    {
        let mut out: Option<T> = None;
        backend.run_locked(&mut |current| {
            let (result, next) = f(current)?;
            out = Some(result);
            Ok(next)
        })?;
        out.ok_or_else(|| Error::msg("Auth lock callback did not return a result"))
    }

    pub async fn auth_with_lock_async<T, F>(
        backend: &dyn AuthStorageBackend,
        signal: Option<AbortSignal>,
        f: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Option<String>) -> BoxFuture<Result<(T, Option<String>)>> + Send + 'static,
    {
        let slot: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
        let slot_cb = Arc::clone(&slot);
        backend
            .run_locked_async(
                Box::new(move |current| {
                    Box::pin(async move {
                        let (result, next) = f(current).await?;
                        *slot_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
                        Ok(next)
                    })
                }),
                signal,
            )
            .await?;
        let value = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        value.ok_or_else(|| Error::msg("Async auth lock callback did not return a result"))
    }

    #[derive(Clone, Debug)]
    pub struct FileAuthStorageBackend {
        auth_path: PathBuf,
    }

    impl FileAuthStorageBackend {
        pub fn new(auth_path: Option<&str>) -> Self {
            let raw = auth_path
                .map(|s| s.to_string())
                .unwrap_or_else(|| pi_js::path::join(&[&get_agent_dir(), "auth.json"]));
            let normalized = pi_js::path::normalize(&raw);
            Self {
                auth_path: PathBuf::from(normalized),
            }
        }

        fn ensure_parent_dir(&self) -> Result<()> {
            if let Some(dir) = self.auth_path.parent()
                && !dir.exists()
            {
                std::fs::create_dir_all(dir)
                    .map_err(|e| Error::from_io(e, "mkdir", Some(&dir.to_string_lossy()), None))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
                }
            }
            Ok(())
        }

        fn write_secure(&self, content: &str) -> Result<()> {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut file = opts
                .open(&self.auth_path)
                .map_err(|e| Error::from_io(e, "open", Some(&self.auth_path.to_string_lossy()), None))?;
            file.write_all(content.as_bytes())
                .map_err(|e| Error::from_io(e, "write", Some(&self.auth_path.to_string_lossy()), None))
        }

        fn ensure_file_exists(&self) -> Result<()> {
            if !self.auth_path.exists() {
                self.write_secure("{}")?;
            }
            Ok(())
        }
    }

    impl AuthStorageBackend for FileAuthStorageBackend {
        fn run_locked(&self, cb: &mut dyn FnMut(Option<&str>) -> Result<Option<String>>) -> Result<()> {
            self.ensure_parent_dir()?;
            self.ensure_file_exists()?;
            let _guard = acquire_lock_sync(&self.auth_path)?;
            let current = std::fs::read_to_string(&self.auth_path).ok();
            if let Some(next) = cb(current.as_deref())? {
                self.write_secure(&next)?;
            }
            Ok(())
        }

        fn run_locked_async(
            &self,
            cb: Box<dyn FnOnce(Option<String>) -> BoxFuture<Result<Option<String>>> + Send>,
            signal: Option<AbortSignal>,
        ) -> BoxFuture<Result<()>> {
            let this = self.clone();
            Box::pin(async move {
                this.ensure_parent_dir()?;
                this.ensure_file_exists()?;
                let _guard = acquire_lock_async(&this.auth_path, signal.as_ref()).await?;
                if let Some(sig) = signal.as_ref()
                    && sig.aborted()
                {
                    return Err(Error::js("AbortError", "Aborted"));
                }
                let current = std::fs::read_to_string(&this.auth_path).ok();
                let next = cb(current).await?;
                if let Some(sig) = signal.as_ref()
                    && sig.aborted()
                {
                    return Err(Error::js("AbortError", "Aborted"));
                }
                if let Some(next) = next {
                    this.write_secure(&next)?;
                }
                Ok(())
            })
        }
    }

    #[derive(Clone, Default)]
    pub struct InMemoryAuthStorageBackend {
        content: Arc<Mutex<Option<String>>>,
        async_mutex: Arc<tokio::sync::Mutex<()>>,
    }

    impl InMemoryAuthStorageBackend {
        pub fn new() -> Self {
            Self::default()
        }
    }

    impl AuthStorageBackend for InMemoryAuthStorageBackend {
        fn run_locked(&self, cb: &mut dyn FnMut(Option<&str>) -> Result<Option<String>>) -> Result<()> {
            let mut guard = self.content.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(next) = cb(guard.as_deref())? {
                *guard = Some(next);
            }
            Ok(())
        }

        fn run_locked_async(
            &self,
            cb: Box<dyn FnOnce(Option<String>) -> BoxFuture<Result<Option<String>>> + Send>,
            signal: Option<AbortSignal>,
        ) -> BoxFuture<Result<()>> {
            let content = Arc::clone(&self.content);
            let async_mutex = Arc::clone(&self.async_mutex);
            Box::pin(async move {
                let _async_guard = async_mutex.lock().await;
                if let Some(sig) = signal.as_ref()
                    && sig.aborted()
                {
                    return Err(Error::js("AbortError", "Aborted"));
                }
                let current = content.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let next = cb(current).await?;
                if let Some(sig) = signal.as_ref()
                    && sig.aborted()
                {
                    return Err(Error::js("AbortError", "Aborted"));
                }
                if let Some(next) = next {
                    *content.lock().unwrap_or_else(|e| e.into_inner()) = Some(next);
                }
                Ok(())
            })
        }
    }

    // WIRE(@mcp-w3): wire to crate::core::trust_manager::ProjectTrustStore once landed
    #[derive(Clone, Debug)]
    pub struct ProjectTrustStore {
        trust_path: PathBuf,
    }

    impl ProjectTrustStore {
        pub fn new(agent_dir: &str) -> Self {
            let cwd = pi_js::env::cwd();
            let resolved = resolve_path(agent_dir, &cwd);
            Self {
                trust_path: PathBuf::from(resolved).join("trust.json"),
            }
        }

        fn normalize_cwd(cwd: &str) -> String {
            canonicalize_path(&resolve_path(cwd, &pi_js::env::cwd()))
        }

        fn read_map(&self) -> IndexMap<String, Option<bool>> {
            let Ok(raw) = std::fs::read_to_string(&self.trust_path) else {
                return IndexMap::new();
            };
            let Ok(Value::Object(map)) = pi_js::json::parse(strip_bom(&raw)) else {
                return IndexMap::new();
            };
            let mut out = IndexMap::new();
            for (k, v) in map {
                match v {
                    Value::Bool(b) => {
                        out.insert(k, Some(b));
                    }
                    Value::Null => {
                        out.insert(k, None);
                    }
                    _ => {}
                }
            }
            out
        }

        pub fn get(&self, cwd: &str) -> Option<bool> {
            let Ok(_guard) = acquire_lock_sync(&self.trust_path) else {
                return None;
            };
            let data = self.read_map();
            let mut current = Self::normalize_cwd(cwd);
            loop {
                if let Some(Some(decision)) = data.get(&current) {
                    return Some(*decision);
                }
                let parent = pi_js::path::dirname(&current);
                if parent == current {
                    return None;
                }
                current = parent;
            }
        }

        pub fn set(&self, cwd: &str, decision: Option<bool>) -> Result<()> {
            if let Some(parent) = self.trust_path.parent()
                && !parent.exists()
            {
                std::fs::create_dir_all(parent)
                    .map_err(|e| Error::from_io(e, "mkdir", Some(&parent.to_string_lossy()), None))?;
            }
            let _guard = acquire_lock_sync(&self.trust_path)?;
            let mut data = self.read_map();
            let key = Self::normalize_cwd(cwd);
            match decision {
                Some(b) => {
                    data.insert(key, Some(b));
                }
                None => {
                    data.shift_remove(&key);
                }
            }
            let mut obj = serde_json::Map::new();
            for (k, v) in data {
                if let Some(b) = v {
                    obj.insert(k, Value::Bool(b));
                }
            }
            let json = format!("{}\n", pi_js::json::stringify_pretty(&Value::Object(obj), "  "));
            std::fs::write(&self.trust_path, json)
                .map_err(|e| Error::from_io(e, "write", Some(&self.trust_path.to_string_lossy()), None))?;
            Ok(())
        }
    }

    // WIRE(@mcp-w3): wire to crate::core::resolve_config_value once landed
    static ENV_VAR_NAME_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("valid env var regex"));
    static ENV_VAR_PREFIX_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*").expect("valid env prefix regex"));

    enum TemplatePart {
        Literal(String),
        Env(String),
    }

    fn push_literal(parts: &mut Vec<TemplatePart>, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(TemplatePart::Literal(prev)) = parts.last_mut() {
            prev.push_str(text);
        } else {
            parts.push(TemplatePart::Literal(text.to_string()));
        }
    }

    fn parse_template(config: &str) -> Vec<TemplatePart> {
        let mut parts = Vec::new();
        let mut index = 0usize;
        while index < config.len() {
            let Some(rel_dollar) = config[index..].find('$') else {
                push_literal(&mut parts, &config[index..]);
                break;
            };
            let dollar = index + rel_dollar;
            push_literal(&mut parts, &config[index..dollar]);
            let rest = &config[dollar + 1..];
            if let Some(next_char) = rest.chars().next() {
                if next_char == '$' || next_char == '!' {
                    push_literal(&mut parts, &next_char.to_string());
                    index = dollar + 1 + next_char.len_utf8();
                    continue;
                }
                if next_char == '{'
                    && let Some(rel_end) = rest[1..].find('}')
                {
                    let end = dollar + 2 + rel_end;
                    let name = &config[dollar + 2..end];
                    if ENV_VAR_NAME_RE.is_match(name) {
                        parts.push(TemplatePart::Env(name.to_string()));
                    } else {
                        push_literal(&mut parts, &config[dollar..=end]);
                    }
                    index = end + 1;
                    continue;
                }
                if let Some(m) = ENV_VAR_PREFIX_RE.find(rest) {
                    parts.push(TemplatePart::Env(m.as_str().to_string()));
                    index = dollar + 1 + m.end();
                    continue;
                }
            }
            push_literal(&mut parts, "$");
            index = dollar + 1;
        }
        parts
    }

    fn resolve_env_var(name: &str, env: Option<&IndexMap<String, String>>) -> Option<String> {
        if let Some(map) = env
            && let Some(val) = map.get(name)
            && !val.is_empty()
        {
            return Some(val.clone());
        }
        pi_js::env::var(name).filter(|v| !v.is_empty())
    }

    fn execute_command_uncached(command_config: &str) -> Option<String> {
        let cmd = command_config.strip_prefix('!')?;
        #[cfg(windows)]
        let mut command = {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", cmd]);
            c
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut c = std::process::Command::new("sh");
            c.args(["-c", cmd]);
            c
        };
        let output = command.output().ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = pi_js::str16::trim(&stdout);
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    pub fn resolve_config_value_uncached(config: &str, env: Option<&IndexMap<String, String>>) -> Option<String> {
        if config.starts_with('!') {
            return execute_command_uncached(config);
        }
        let parts = parse_template(config);
        let mut out = String::new();
        for part in parts {
            match part {
                TemplatePart::Literal(lit) => out.push_str(&lit),
                TemplatePart::Env(name) => {
                    let val = resolve_env_var(&name, env)?;
                    out.push_str(&val);
                }
            }
        }
        Some(out)
    }

    pub fn resolve_config_value_or_throw(
        config: &str,
        description: &str,
        env: Option<&IndexMap<String, String>>,
    ) -> Result<String> {
        if let Some(resolved) = resolve_config_value_uncached(config, env) {
            return Ok(resolved);
        }
        if let Some(cmd) = config.strip_prefix('!') {
            return Err(Error::msg(format!(
                "Failed to resolve {description} from shell command: {cmd}"
            )));
        }
        let mut missing = Vec::new();
        for part in parse_template(config) {
            if let TemplatePart::Env(name) = part
                && !missing.contains(&name)
                && resolve_env_var(&name, env).is_none()
            {
                missing.push(name);
            }
        }
        if missing.len() == 1 {
            return Err(Error::msg(format!(
                "Failed to resolve {description} from environment variable: {}",
                missing[0]
            )));
        }
        if missing.len() > 1 {
            return Err(Error::msg(format!(
                "Failed to resolve {description} from environment variables: {}",
                missing.join(", ")
            )));
        }
        Err(Error::msg(format!("Failed to resolve {description}")))
    }

    pub fn resolve_headers_or_throw(
        headers: Option<&IndexMap<String, String>>,
        description: &str,
        env: Option<&IndexMap<String, String>>,
    ) -> Result<Option<IndexMap<String, String>>> {
        let Some(headers) = headers else {
            return Ok(None);
        };
        let mut resolved = IndexMap::new();
        for (k, v) in headers {
            resolved.insert(
                k.clone(),
                resolve_config_value_or_throw(v, &format!("{description} header \"{k}\""), env)?,
            );
        }
        if resolved.is_empty() {
            Ok(None)
        } else {
            Ok(Some(resolved))
        }
    }

    // WIRE(@mcp-w3): wire to crate::core::mcp_servers once landed
    static SERVER_NAME_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^[A-Za-z0-9_-]+$").expect("valid server name regex"));

    pub fn is_loopback_host(host: &str) -> bool {
        matches!(host, "localhost" | "127.0.0.1" | "[::1]")
    }

    pub fn is_loopback_redirect_uri(value: &str) -> bool {
        let Ok(url) = url::Url::parse(value) else {
            return false;
        };
        url.scheme() == "http"
            && url.host_str().is_some_and(is_loopback_host)
            && url.query().is_none()
            && url.fragment().is_none()
    }

    fn resolve_exposure_alias(raw: &Value) -> std::result::Result<McpExposure, String> {
        let Some(s) = raw.as_str() else {
            return Err("invalid".to_string());
        };
        match s {
            "codemode" | "codemode-deferred" => Ok(McpExposure::Codemode),
            "deferred" => Ok(McpExposure::Deferred),
            "direct" => Ok(McpExposure::Direct),
            "hidden" => Ok(McpExposure::Hidden),
            _ => Err("invalid".to_string()),
        }
    }

    fn validate_oauth_value(value: Option<&Value>) -> std::result::Result<Option<McpOAuthConfig>, String> {
        let Some(val) = value else {
            return Ok(None);
        };
        let Some(obj) = val.as_object() else {
            return Err("oauth must be an object".to_string());
        };
        let client_id = match obj.get("clientId") {
            None => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return Err("oauth.clientId must be a string".to_string()),
        };
        let client_secret = match obj.get("clientSecret") {
            None => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return Err("oauth.clientSecret must be a string".to_string()),
        };
        let callback_port = match obj.get("callbackPort") {
            None => None,
            Some(v) => {
                let Some(n) = v.as_f64() else {
                    return Err("oauth.callbackPort must be a port number".to_string());
                };
                if n.fract() != 0.0 || !(1.0..=65535.0).contains(&n) {
                    return Err("oauth.callbackPort must be a port number".to_string());
                }
                Some(n as i64)
            }
        };
        let callback_url = match obj.get("callbackUrl") {
            None => None,
            Some(v) => {
                let Some(s) = v.as_str() else {
                    return Err(
                        "oauth.callbackUrl must be an http URI on localhost, 127.0.0.1, or [::1] without query or fragment"
                            .to_string(),
                    );
                };
                if !is_loopback_redirect_uri(s) {
                    return Err(
                        "oauth.callbackUrl must be an http URI on localhost, 127.0.0.1, or [::1] without query or fragment"
                            .to_string(),
                    );
                }
                if let Ok(parsed) = url::Url::parse(s)
                    && let (Some(url_port), Some(cfg_port)) = (parsed.port(), callback_port)
                    && i64::from(url_port) != cfg_port
                {
                    return Err("oauth.callbackUrl and oauth.callbackPort name different ports".to_string());
                }
                Some(s.to_string())
            }
        };
        let scope = match obj.get("scope") {
            None => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return Err("oauth.scope must be a string".to_string()),
        };
        let client_name = match obj.get("clientName") {
            None => None,
            Some(Value::String(s)) if !pi_js::str16::trim(s).is_empty() => Some(s.clone()),
            Some(_) => return Err("oauth.clientName must be a non-empty string".to_string()),
        };
        let auth_server_metadata_url = match obj.get("authServerMetadataUrl") {
            None => None,
            Some(v) => {
                let parsed = v.as_str().and_then(|s| url::Url::parse(s).ok());
                let valid = parsed.as_ref().is_some_and(|u| {
                    u.scheme() == "https" || (u.scheme() == "http" && u.host_str().is_some_and(is_loopback_host))
                });
                if !valid {
                    return Err(
                        "oauth.authServerMetadataUrl must be an https URL, or http on localhost, 127.0.0.1, or [::1]"
                            .to_string(),
                    );
                }
                v.as_str().map(String::from)
            }
        };
        Ok(Some(McpOAuthConfig {
            client_id,
            client_secret,
            callback_port,
            callback_url,
            scope,
            client_name,
            auth_server_metadata_url,
        }))
    }

    pub fn validate_mcp_server_config(
        name: &str,
        raw: &Value,
    ) -> std::result::Result<(McpServerConfig, Option<String>), String> {
        if !SERVER_NAME_RE.is_match(name) {
            return Err(format!(
                "invalid server name \"{name}\" (use letters, digits, \"_\" and \"-\")"
            ));
        }
        let Some(obj) = raw.as_object() else {
            return Err(format!("server \"{name}\" must be an object"));
        };
        let exposures = "\"codemode\", \"deferred\", \"direct\", \"hidden\"";
        let exposure = match obj.get("exposure") {
            None => None,
            Some(v) => Some(
                resolve_exposure_alias(v)
                    .map_err(|_| format!("server \"{name}\": exposure must be one of {exposures}"))?,
            ),
        };
        let tool_exposure = match obj.get("toolExposure") {
            None => None,
            Some(Value::Object(map)) => {
                let mut out = IndexMap::new();
                for (tool, entry) in map {
                    let exp = resolve_exposure_alias(entry).map_err(|_| {
                        format!("server \"{name}\": toolExposure \"{tool}\" must be one of {exposures}")
                    })?;
                    out.insert(tool.clone(), exp);
                }
                Some(out)
            }
            Some(_) => {
                return Err(format!(
                    "server \"{name}\": toolExposure must map tool names to exposures"
                ));
            }
        };
        let enabled = match obj.get("enabled") {
            None => None,
            Some(Value::Bool(b)) => Some(*b),
            Some(_) => return Err(format!("server \"{name}\": enabled must be a boolean")),
        };
        let description = match obj.get("description") {
            None => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return Err(format!("server \"{name}\": description must be a string")),
        };
        let timeout = match obj.get("timeout") {
            None => None,
            Some(v) => {
                let Some(n) = v.as_f64() else {
                    return Err(format!(
                        "server \"{name}\": timeout must be a positive number of seconds"
                    ));
                };
                if !(n > 0.0) {
                    return Err(format!(
                        "server \"{name}\": timeout must be a positive number of seconds"
                    ));
                }
                Some(n as i64)
            }
        };

        let r#type = obj.get("type").and_then(Value::as_str);
        if r#type == Some("sse") {
            return Err(format!(
                "server \"{name}\": legacy SSE transport is not supported; use the streamable HTTP URL"
            ));
        }

        if let Some(Value::String(url_str)) = obj.get("url")
            && matches!(r#type, None | Some("http") | Some("streamable-http"))
        {
            let Ok(parsed) = url::Url::parse(url_str) else {
                return Err(format!("server \"{name}\": url must be an http or https URL"));
            };
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(format!("server \"{name}\": url must be an http or https URL"));
            }
            let headers = match obj.get("headers") {
                None => None,
                Some(Value::Object(map)) => {
                    let mut out = IndexMap::new();
                    for (k, v) in map {
                        let Some(s) = v.as_str() else {
                            return Err(format!("server \"{name}\": headers must map names to strings"));
                        };
                        out.insert(k.clone(), s.to_string());
                    }
                    Some(out)
                }
                Some(_) => return Err(format!("server \"{name}\": headers must map names to strings")),
            };
            let oauth = validate_oauth_value(obj.get("oauth")).map_err(|e| format!("server \"{name}\": {e}"))?;
            let auth_provider = if let Some(auth_val) = obj.get("auth") {
                let provider = auth_val
                    .as_object()
                    .and_then(|a| a.get("provider"))
                    .and_then(Value::as_str)
                    .filter(|p| !p.is_empty())
                    .ok_or_else(|| format!("server \"{name}\": auth.provider must be a provider name"))?;
                if parsed.scheme() != "https" && !parsed.host_str().is_some_and(is_loopback_host) {
                    return Err(format!(
                        "server \"{name}\": auth requires an https URL, or http on localhost, 127.0.0.1, or [::1]"
                    ));
                }
                Some(provider.to_string())
            } else {
                None
            };
            return Ok((
                McpServerConfig::Http(McpHttpServerConfig {
                    exposure,
                    description,
                    tool_exposure,
                    enabled,
                    timeout,
                    r#type: r#type.map(String::from),
                    url: url_str.clone(),
                    headers,
                    oauth,
                }),
                auth_provider,
            ));
        }

        if let Some(Value::String(cmd)) = obj.get("command")
            && matches!(r#type, None | Some("stdio"))
        {
            let args = match obj.get("args") {
                None => None,
                Some(Value::Array(arr)) => {
                    let mut out = Vec::with_capacity(arr.len());
                    for item in arr {
                        let Some(s) = item.as_str() else {
                            return Err(format!("server \"{name}\": args must be an array of strings"));
                        };
                        out.push(s.to_string());
                    }
                    Some(out)
                }
                Some(_) => return Err(format!("server \"{name}\": args must be an array of strings")),
            };
            let env = match obj.get("env") {
                None => None,
                Some(Value::Object(map)) => {
                    let mut out = IndexMap::new();
                    for (k, v) in map {
                        let Some(s) = v.as_str() else {
                            return Err(format!("server \"{name}\": env must map names to strings"));
                        };
                        out.insert(k.clone(), s.to_string());
                    }
                    Some(out)
                }
                Some(_) => return Err(format!("server \"{name}\": env must map names to strings")),
            };
            let cwd = match obj.get("cwd") {
                None => None,
                Some(Value::String(s)) => Some(s.clone()),
                Some(_) => return Err(format!("server \"{name}\": cwd must be a string")),
            };
            return Ok((
                McpServerConfig::Stdio(McpStdioServerConfig {
                    exposure,
                    description,
                    tool_exposure,
                    enabled,
                    timeout,
                    r#type: r#type.map(String::from),
                    command: cmd.clone(),
                    args,
                    env,
                    cwd,
                }),
                None,
            ));
        }

        Err(format!(
            "server \"{name}\" needs either \"command\" (stdio) or \"url\" (streamable HTTP)"
        ))
    }

    fn wildcard_matches(pattern: &str, text: &str) -> bool {
        let regex_pattern = pattern.split('*').map(regex::escape).collect::<Vec<_>>().join(".*");
        regex::Regex::new(&format!("^{regex_pattern}$")).is_ok_and(|re| re.is_match(text))
    }

    pub fn get_mcp_tool_exposure(config: &McpServerConfig, tool_name: &str) -> McpExposure {
        let (exposure, tool_exposure) = match config {
            McpServerConfig::Stdio(s) => (s.exposure, s.tool_exposure.as_ref()),
            McpServerConfig::Http(h) => (h.exposure, h.tool_exposure.as_ref()),
        };
        if let Some(overrides) = tool_exposure {
            if let Some(exact) = overrides.get(tool_name) {
                return *exact;
            }
            for (pattern, exp) in overrides {
                if pattern.contains('*') && wildcard_matches(pattern, tool_name) {
                    return *exp;
                }
            }
        }
        exposure.unwrap_or(McpExposure::Codemode)
    }

    // WIRE(@mcp-w3): wire to crate::core::usage_totals::combine_usage once landed
    pub fn combine_usage(first: &Usage, second: &Usage) -> Usage {
        Usage {
            input: first.input + second.input,
            output: first.output + second.output,
            cache_read: first.cache_read + second.cache_read,
            cache_write: first.cache_write + second.cache_write,
            cache_write_1h: if first.cache_write_1h.is_some() || second.cache_write_1h.is_some() {
                Some(first.cache_write_1h.unwrap_or(0) + second.cache_write_1h.unwrap_or(0))
            } else {
                None
            },
            reasoning: if first.reasoning.is_some() || second.reasoning.is_some() {
                Some(first.reasoning.unwrap_or(0) + second.reasoning.unwrap_or(0))
            } else {
                None
            },
            total_tokens: first.total_tokens + second.total_tokens,
            cost: UsageCost {
                input: first.cost.input + second.cost.input,
                output: first.cost.output + second.cost.output,
                cache_read: first.cost.cache_read + second.cost.cache_read,
                cache_write: first.cost.cache_write + second.cost.cache_write,
                total: first.cost.total + second.cost.total,
            },
        }
    }

    // WIRE(@w3-b): open URL in user's default browser cleanly
    pub fn open_browser(url: &str) -> bool {
        #[cfg(target_os = "macos")]
        let cmd = ("open", vec![url]);
        #[cfg(target_os = "windows")]
        let cmd = ("cmd", vec!["/c", "start", "", url]);
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        let cmd = ("xdg-open", vec![url]);

        std::process::Command::new(cmd.0)
            .args(&cmd.1)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
    }

    // WIRE(@w3-b): wire to crate::modes::interactive::components::DynamicBorder once landed
    pub struct DynamicBorder {
        color: Arc<dyn Fn(&str) -> String + Send + Sync>,
    }

    impl DynamicBorder {
        pub fn new(color: Arc<dyn Fn(&str) -> String + Send + Sync>) -> Self {
            Self { color }
        }
    }

    impl Component for DynamicBorder {
        fn render(&self, width: usize) -> Vec<String> {
            vec![(self.color)(&"─".repeat(width.max(1)))]
        }

        fn handle_input(&self, _data: &str) {}

        fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
            None
        }

        fn invalidate(&self) {}
    }

    // WIRE(@w3-b): wire to crate::modes::interactive::components::visual_truncate once landed
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum VisualTruncateKeep {
        Start,
        End,
    }

    pub struct VisualTruncateResult {
        pub visual_lines: Vec<String>,
        pub skipped_count: usize,
    }

    pub fn truncate_to_visual_lines(
        text: &str,
        max_visual_lines: usize,
        width: usize,
        padding_x: i64,
        keep: VisualTruncateKeep,
    ) -> VisualTruncateResult {
        if text.is_empty() {
            return VisualTruncateResult {
                visual_lines: Vec::new(),
                skipped_count: 0,
            };
        }
        let temp_text = Text::new(Some(text), Some(padding_x), Some(0), None);
        let all_visual_lines = temp_text.render(width);
        if all_visual_lines.len() <= max_visual_lines {
            return VisualTruncateResult {
                visual_lines: all_visual_lines,
                skipped_count: 0,
            };
        }
        let skipped_count = all_visual_lines.len() - max_visual_lines;
        let truncated_lines = match keep {
            VisualTruncateKeep::Start => all_visual_lines[..max_visual_lines].to_vec(),
            VisualTruncateKeep::End => all_visual_lines[all_visual_lines.len() - max_visual_lines..].to_vec(),
        };
        VisualTruncateResult {
            visual_lines: truncated_lines,
            skipped_count,
        }
    }

    struct VisualLinePreviewState {
        cached_width: Option<usize>,
        cached_lines: Option<Vec<String>>,
    }

    pub struct VisualLinePreview {
        text: String,
        max_visual_lines: usize,
        keep: VisualTruncateKeep,
        format_hint: Arc<dyn Fn(usize) -> String + Send + Sync>,
        state: Mutex<VisualLinePreviewState>,
    }

    impl VisualLinePreview {
        pub fn new(
            text: impl Into<String>,
            max_visual_lines: usize,
            keep: VisualTruncateKeep,
            format_hint: Arc<dyn Fn(usize) -> String + Send + Sync>,
        ) -> Self {
            Self {
                text: text.into(),
                max_visual_lines,
                keep,
                format_hint,
                state: Mutex::new(VisualLinePreviewState {
                    cached_width: None,
                    cached_lines: None,
                }),
            }
        }
    }

    impl Component for VisualLinePreview {
        fn render(&self, width: usize) -> Vec<String> {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.cached_lines.is_none() || st.cached_width != Some(width) {
                let preview = truncate_to_visual_lines(&self.text, self.max_visual_lines, width, 0, self.keep);
                let mut lines = preview.visual_lines;
                if preview.skipped_count > 0 {
                    let hint = truncate_to_width(
                        &(self.format_hint)(preview.skipped_count),
                        width as i64,
                        Some("..."),
                        None,
                    );
                    match self.keep {
                        VisualTruncateKeep::Start => lines.push(hint),
                        VisualTruncateKeep::End => lines.insert(0, hint),
                    }
                }
                st.cached_lines = Some(lines);
                st.cached_width = Some(width);
            }
            st.cached_lines.clone().unwrap_or_default()
        }

        fn handle_input(&self, _data: &str) {}

        fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
            None
        }

        fn invalidate(&self) {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.cached_width = None;
            st.cached_lines = None;
        }
    }

    // WIRE(@w3-b): wire to crate::modes::interactive::components::keybinding_hints once landed
    fn format_key_part(part: &str) -> String {
        if pi_js::env::platform() == "darwin" && part.eq_ignore_ascii_case("alt") {
            "option".to_string()
        } else {
            part.to_string()
        }
    }

    pub fn format_key_text(key: &str) -> String {
        key.split('/')
            .map(|k| k.split('+').map(format_key_part).collect::<Vec<_>>().join("+"))
            .collect::<Vec<_>>()
            .join("/")
    }

    pub fn key_hint(keybinding: &str, description: &str, theme: &Theme) -> String {
        let keys = get_keybindings().get_keys(keybinding);
        let key_text = if keys.is_empty() {
            // Fallback before coding-agent initializes app keybindings in headless tests.
            match keybinding {
                "app.tools.expand" => "ctrl+o".to_string(),
                _ => String::new(),
            }
        } else {
            format_key_text(&keys.join("/"))
        };
        format!(
            "{}{}",
            theme.fg("dim", &key_text),
            theme.fg("muted", &format!(" {description}"))
        )
    }

    // WIRE(@w3-b): wire to crate::modes::interactive::theme::highlight_code once landed
    pub fn highlight_code(code: &str, _lang: Option<&str>, theme: &Theme) -> Vec<String> {
        code.split('\n').map(|line| theme.fg("mdCodeBlock", line)).collect()
    }

    // WIRE(@w3-b): wire to crate::modes::interactive::theme::get_select_list_theme once landed
    pub fn get_select_list_theme(theme: &Theme) -> SelectListTheme {
        let t_prefix = theme.clone();
        let t_selected = theme.clone();
        let t_desc = theme.clone();
        let t_scroll = theme.clone();
        let t_none = theme.clone();
        let selected_prefix: SelectListStyleFn = Arc::new(move |s| t_prefix.fg("accent", s));
        let selected_text: SelectListStyleFn = Arc::new(move |s| t_selected.fg("accent", s));
        let description: SelectListStyleFn = Arc::new(move |s| t_desc.fg("muted", s));
        let scroll_info: SelectListStyleFn = Arc::new(move |s| t_scroll.fg("muted", s));
        let no_match: SelectListStyleFn = Arc::new(move |s| t_none.fg("muted", s));
        SelectListTheme {
            selected_prefix,
            selected_text,
            description,
            scroll_info,
            no_match,
        }
    }
}
