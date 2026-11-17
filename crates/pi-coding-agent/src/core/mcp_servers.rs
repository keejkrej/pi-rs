//! Port of packages/coding-agent/src/core/mcp-servers.ts

use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpExposure {
    Codemode,
    Deferred,
    Direct,
    Hidden,
}

impl McpExposure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codemode => "codemode",
            Self::Deferred => "deferred",
            Self::Direct => "direct",
            Self::Hidden => "hidden",
        }
    }

    pub fn from_str_or_alias(s: &str) -> Option<Self> {
        match s {
            "codemode" | "codemode-deferred" => Some(Self::Codemode),
            "deferred" => Some(Self::Deferred),
            "direct" => Some(Self::Direct),
            "hidden" => Some(Self::Hidden),
            _ => None,
        }
    }
}

const MCP_EXPOSURES_STR: &str = "\"codemode\", \"deferred\", \"direct\", \"hidden\"";
const LOOPBACK_HOSTS: &[&str] = &["localhost", "127.0.0.1", "[::1]"];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_port: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_server_metadata_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpProviderAuthConfig {
    pub provider: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStdioServerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure: Option<McpExposure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_exposure: Option<IndexMap<String, McpExposure>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<IndexMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpHttpServerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure: Option<McpExposure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_exposure: Option<IndexMap<String, McpExposure>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<McpProviderAuthConfig>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpServerConfig {
    Http(McpHttpServerConfig),
    Stdio(McpStdioServerConfig),
}

impl McpServerConfig {
    pub fn exposure(&self) -> Option<McpExposure> {
        match self {
            Self::Http(c) => c.exposure,
            Self::Stdio(c) => c.exposure,
        }
    }

    pub fn tool_exposure(&self) -> Option<&IndexMap<String, McpExposure>> {
        match self {
            Self::Http(c) => c.tool_exposure.as_ref(),
            Self::Stdio(c) => c.tool_exposure.as_ref(),
        }
    }

    pub fn enabled(&self) -> Option<bool> {
        match self {
            Self::Http(c) => c.enabled,
            Self::Stdio(c) => c.enabled,
        }
    }

    pub fn timeout(&self) -> Option<f64> {
        match self {
            Self::Http(c) => c.timeout,
            Self::Stdio(c) => c.timeout,
        }
    }

    pub fn description(&self) -> Option<&str> {
        match self {
            Self::Http(c) => c.description.as_deref(),
            Self::Stdio(c) => c.description.as_deref(),
        }
    }
}

pub fn mcp_namespace(server: &str) -> String {
    format!("mcp__{}", server.replace('-', "_"))
}

pub fn is_loopback_redirect_uri(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    let host = url.host_str().unwrap_or("");
    url.scheme() == "http" && LOOPBACK_HOSTS.contains(&host) && url.query().is_none() && url.fragment().is_none()
}

fn is_valid_server_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn is_string_record(val: &Value) -> bool {
    let Some(obj) = val.as_object() else {
        return false;
    };
    obj.values().all(Value::is_string)
}

fn validate_oauth(value: &Value) -> Option<String> {
    let Some(obj) = value.as_object() else {
        return Some("oauth must be an object".to_owned());
    };
    if let Some(v) = obj.get("clientId")
        && !v.is_string()
    {
        return Some("oauth.clientId must be a string".to_owned());
    }
    if let Some(v) = obj.get("clientSecret")
        && !v.is_string()
    {
        return Some("oauth.clientSecret must be a string".to_owned());
    }
    let port_num: Option<u16> = if let Some(port) = obj.get("callbackPort") {
        let Some(n) = port.as_f64() else {
            return Some("oauth.callbackPort must be a port number".to_owned());
        };
        if n.fract() != 0.0 || !(1.0..=65535.0).contains(&n) {
            return Some("oauth.callbackPort must be a port number".to_owned());
        }
        Some(n as u16)
    } else {
        None
    };
    if let Some(cb) = obj.get("callbackUrl") {
        let Some(cb_str) = cb.as_str() else {
            return Some(
                "oauth.callbackUrl must be an http URI on localhost, 127.0.0.1, or [::1] without query or fragment"
                    .to_owned(),
            );
        };
        if !is_loopback_redirect_uri(cb_str) {
            return Some(
                "oauth.callbackUrl must be an http URI on localhost, 127.0.0.1, or [::1] without query or fragment"
                    .to_owned(),
            );
        }
        if let Ok(parsed) = url::Url::parse(cb_str)
            && let Some(uport) = parsed.port()
            && let Some(p) = port_num
            && uport != p
        {
            return Some("oauth.callbackUrl and oauth.callbackPort name different ports".to_owned());
        }
    }
    if let Some(v) = obj.get("scope")
        && !v.is_string()
    {
        return Some("oauth.scope must be a string".to_owned());
    }
    if let Some(v) = obj.get("clientName")
        && !v.as_str().is_some_and(|s| !pi_js::str16::trim(s).is_empty())
    {
        return Some("oauth.clientName must be a non-empty string".to_owned());
    }
    if let Some(meta) = obj.get("authServerMetadataUrl") {
        let parsed = meta.as_str().and_then(|s| url::Url::parse(s).ok());
        let ok = parsed.is_some_and(|u| {
            u.scheme() == "https" || (u.scheme() == "http" && u.host_str().is_some_and(|h| LOOPBACK_HOSTS.contains(&h)))
        });
        if !ok {
            return Some(
                "oauth.authServerMetadataUrl must be an https URL, or http on localhost, 127.0.0.1, or [::1]"
                    .to_owned(),
            );
        }
    }
    None
}

fn resolve_exposure_alias_value(value: &Value) -> Value {
    match value.as_str() {
        Some("codemode-deferred") => Value::String("codemode".to_owned()),
        _ => value.clone(),
    }
}

fn resolve_exposure_aliases(raw: &Map<String, Value>) -> Map<String, Value> {
    let mut resolved = raw.clone();
    if let Some(exp) = raw.get("exposure") {
        resolved.insert("exposure".to_owned(), resolve_exposure_alias_value(exp));
    }
    if let Some(Value::Object(te)) = raw.get("toolExposure") {
        let mut next = Map::new();
        for (tool, entry) in te {
            next.insert(tool.clone(), resolve_exposure_alias_value(entry));
        }
        resolved.insert("toolExposure".to_owned(), Value::Object(next));
    }
    resolved
}

fn pattern_matches(pattern: &str, tool_name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == tool_name;
    }
    let mut rest = tool_name;
    let Some(first) = parts.first() else {
        return false;
    };
    if !rest.starts_with(first) {
        return false;
    }
    rest = &rest[first.len()..];
    for middle in &parts[1..parts.len() - 1] {
        if let Some(idx) = rest.find(middle) {
            rest = &rest[idx + middle.len()..];
        } else {
            return false;
        }
    }
    let last = parts[parts.len() - 1];
    rest.ends_with(last)
}

pub fn get_mcp_tool_exposure(config: &McpServerConfig, tool_name: &str) -> McpExposure {
    if let Some(overrides) = config.tool_exposure() {
        if let Some(exact) = overrides.get(tool_name) {
            return *exact;
        }
        for (pattern, exposure) in overrides {
            if pattern.contains('*') && pattern_matches(pattern, tool_name) {
                return *exposure;
            }
        }
    }
    config.exposure().unwrap_or(McpExposure::Codemode)
}

pub fn validate_mcp_server_config(name: &str, raw: &Value) -> std::result::Result<McpServerConfig, String> {
    if !is_valid_server_name(name) {
        return Err(format!(
            "invalid server name \"{name}\" (use letters, digits, \"_\" and \"-\")"
        ));
    }
    let Some(raw_obj) = raw.as_object() else {
        return Err(format!("server \"{name}\" must be an object"));
    };
    let value = resolve_exposure_aliases(raw_obj);

    if let Some(exp) = value.get("exposure") {
        let ok = exp.as_str().and_then(McpExposure::from_str_or_alias).is_some();
        if !ok {
            return Err(format!(
                "server \"{name}\": exposure must be one of {MCP_EXPOSURES_STR}"
            ));
        }
    }
    if let Some(te) = value.get("toolExposure") {
        let Some(te_obj) = te.as_object() else {
            return Err(format!(
                "server \"{name}\": toolExposure must map tool names to exposures"
            ));
        };
        for (tool, entry) in te_obj {
            let ok = entry.as_str().and_then(McpExposure::from_str_or_alias).is_some();
            if !ok {
                return Err(format!(
                    "server \"{name}\": toolExposure \"{tool}\" must be one of {MCP_EXPOSURES_STR}"
                ));
            }
        }
    }
    if let Some(enabled) = value.get("enabled")
        && !enabled.is_boolean()
    {
        return Err(format!("server \"{name}\": enabled must be a boolean"));
    }
    if let Some(desc) = value.get("description")
        && !desc.is_string()
    {
        return Err(format!("server \"{name}\": description must be a string"));
    }
    if let Some(timeout) = value.get("timeout") {
        let ok = timeout.as_f64().is_some_and(|n| n > 0.0);
        if !ok {
            return Err(format!(
                "server \"{name}\": timeout must be a positive number of seconds"
            ));
        }
    }

    let server_type = value.get("type").and_then(Value::as_str);
    if server_type == Some("sse") {
        return Err(format!(
            "server \"{name}\": legacy SSE transport is not supported; use the streamable HTTP URL"
        ));
    }

    if let Some(url_str) = value.get("url").and_then(Value::as_str)
        && matches!(server_type, None | Some("http") | Some("streamable-http"))
    {
        let Ok(parsed_url) = url::Url::parse(url_str) else {
            return Err(format!("server \"{name}\": url must be an http or https URL"));
        };
        if !matches!(parsed_url.scheme(), "http" | "https") {
            return Err(format!("server \"{name}\": url must be an http or https URL"));
        }
        if let Some(headers) = value.get("headers")
            && !is_string_record(headers)
        {
            return Err(format!("server \"{name}\": headers must map names to strings"));
        }
        if let Some(oauth) = value.get("oauth")
            && let Some(oauth_err) = validate_oauth(oauth)
        {
            return Err(format!("server \"{name}\": {oauth_err}"));
        }
        if let Some(auth) = value.get("auth") {
            let valid_provider = auth
                .as_object()
                .and_then(|o| o.get("provider"))
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
            if !valid_provider {
                return Err(format!("server \"{name}\": auth.provider must be a provider name"));
            }
            let host = parsed_url.host_str().unwrap_or("");
            if parsed_url.scheme() != "https" && !LOOPBACK_HOSTS.contains(&host) {
                return Err(format!(
                    "server \"{name}\": auth requires an https URL, or http on localhost, 127.0.0.1, or [::1]"
                ));
            }
        }
        let parsed: McpHttpServerConfig =
            serde_json::from_value(Value::Object(value)).map_err(|err| format!("server \"{name}\": {err}"))?;
        return Ok(McpServerConfig::Http(parsed));
    }

    if value.get("command").is_some_and(Value::is_string) && matches!(server_type, None | Some("stdio")) {
        if let Some(args) = value.get("args")
            && !args.as_array().is_some_and(|a| a.iter().all(Value::is_string))
        {
            return Err(format!("server \"{name}\": args must be an array of strings"));
        }
        if let Some(env) = value.get("env")
            && !is_string_record(env)
        {
            return Err(format!("server \"{name}\": env must map names to strings"));
        }
        if let Some(cwd) = value.get("cwd")
            && !cwd.is_string()
        {
            return Err(format!("server \"{name}\": cwd must be a string"));
        }
        let parsed: McpStdioServerConfig =
            serde_json::from_value(Value::Object(value)).map_err(|err| format!("server \"{name}\": {err}"))?;
        return Ok(McpServerConfig::Stdio(parsed));
    }

    Err(format!(
        "server \"{name}\" needs either \"command\" (stdio) or \"url\" (streamable HTTP)"
    ))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisteredMcpServer {
    pub name: String,
    pub config: McpServerConfig,
    pub extension_path: String,
}

#[derive(Clone, Default)]
pub struct McpServerRegistry {
    servers: Arc<Mutex<IndexMap<String, RegisteredMcpServer>>>,
    change_listener: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
}

impl McpServerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, server: RegisteredMcpServer) {
        if let Ok(mut guard) = self.servers.lock() {
            guard.insert(server.name.clone(), server);
        }
        let listener = self.change_listener.lock().ok().and_then(|g| g.clone());
        if let Some(cb) = listener {
            cb();
        }
    }

    pub fn unregister(&self, name: &str, extension_path: &str) {
        let changed = if let Ok(mut guard) = self.servers.lock() {
            if guard.get(name).map(|s| s.extension_path.as_str()) == Some(extension_path) {
                guard.shift_remove(name);
                true
            } else {
                false
            }
        } else {
            false
        };
        if changed {
            let listener = self.change_listener.lock().ok().and_then(|g| g.clone());
            if let Some(cb) = listener {
                cb();
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<RegisteredMcpServer> {
        self.servers.lock().ok().and_then(|g| g.get(name).cloned())
    }

    pub fn list(&self) -> Vec<RegisteredMcpServer> {
        self.servers
            .lock()
            .ok()
            .map(|g| g.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn set_change_listener(&self, listener: Option<Arc<dyn Fn() + Send + Sync>>) {
        if let Ok(mut guard) = self.change_listener.lock() {
            *guard = listener;
        }
    }
}

static _UNUSED_LAZYLOCK: LazyLock<()> = LazyLock::new(|| ());
