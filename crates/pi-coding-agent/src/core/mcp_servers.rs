//! Port of packages/coding-agent/src/core/mcp-servers.ts

use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;
pub use pi_mcp::oauth::client_settings::McpClientRegistration;
use pi_mcp::{
    McpServerConfigOptions as PiMcpServerConfigOptions, McpServerConfigResult as PiMcpServerConfigResult,
    validate_mcp_server_config as validate_pi_mcp_server_config,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const LIST_MCP_RESOURCES_TOOL: &str = "list_mcp_resources";
pub const LIST_MCP_RESOURCE_TEMPLATES_TOOL: &str = "list_mcp_resource_templates";
pub const READ_MCP_RESOURCE_TOOL: &str = "read_mcp_resource";

/// Whether a tool comes from MCP: a server tool (`mcp__<server>__<tool>`) or a resource tool.
pub fn is_mcp_tool_name(name: &str) -> bool {
    name.starts_with("mcp__")
        || matches!(
            name,
            LIST_MCP_RESOURCES_TOOL | LIST_MCP_RESOURCE_TEMPLATES_TOOL | READ_MCP_RESOURCE_TOOL
        )
}

/// Whether a tool name matches any of the entries, each an exact name or a `*` wildcard pattern.
pub fn create_tool_name_matcher(entries: &[String]) -> Arc<dyn Fn(&str) -> bool + Send + Sync> {
    let mut exact = std::collections::HashSet::new();
    let mut patterns = Vec::new();
    for entry in entries {
        if entry.contains('*') {
            patterns.push(entry.clone());
        } else {
            exact.insert(entry.clone());
        }
    }
    Arc::new(move |name: &str| exact.contains(name) || patterns.iter().any(|pat| pattern_matches(pat, name)))
}

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
    pub client_registration: Option<McpClientRegistration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_server_metadata_url: Option<String>,
}

/// Whether pi's Client ID Metadata Documents list a redirect URI for the loopback `callbackUrl`: they list
/// `/callback` on localhost and 127.0.0.1, on any port. Without `callbackUrl`, pi uses such a URI.
pub fn is_client_metadata_document_callback(callback_url: Option<&str>) -> bool {
    let Some(raw) = callback_url else {
        return true;
    };
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    url.host_str() != Some("[::1]") && url.path() == "/callback"
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
    pi_mcp::mcp_namespace(server)
}

pub fn is_loopback_redirect_uri(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    let host = url.host_str().unwrap_or("");
    url.scheme() == "http" && LOOPBACK_HOSTS.contains(&host) && url.query().is_none() && url.fragment().is_none()
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

fn check_exposures(extra: &IndexMap<String, Value>) -> Option<String> {
    if let Some(exp) = extra.get("exposure")
        && exp.as_str().and_then(McpExposure::from_str_or_alias).is_none()
    {
        return Some(format!("exposure must be one of {MCP_EXPOSURES_STR}"));
    }
    if let Some(te) = extra.get("toolExposure") {
        let Some(te_obj) = te.as_object() else {
            return Some("toolExposure must map tool names to exposures".to_owned());
        };
        for (tool, entry) in te_obj {
            if entry.as_str().and_then(McpExposure::from_str_or_alias).is_none() {
                return Some(format!("toolExposure \"{tool}\" must be one of {MCP_EXPOSURES_STR}"));
            }
        }
    }
    None
}

fn check_auth(auth: Option<&Value>, url_str: &str) -> Option<String> {
    let auth = auth?;
    let valid_provider = auth
        .as_object()
        .and_then(|o| o.get("provider"))
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if !valid_provider {
        return Some("auth.provider must be a provider name".to_owned());
    }
    if let Ok(parsed) = url::Url::parse(url_str) {
        let host = parsed.host_str().unwrap_or("");
        if parsed.scheme() != "https" && !LOOPBACK_HOSTS.contains(&host) {
            return Some("auth requires an https URL, or http on localhost, 127.0.0.1, or [::1]".to_owned());
        }
    }
    None
}

/// Validate one server entry of the `mcpServers` shape: pi-mcp's shared fields and pi's own.
/// Returns a copy of the config with exposure aliases resolved, or an error message.
pub fn validate_mcp_server_config(name: &str, raw: &Value) -> std::result::Result<McpServerConfig, String> {
    let resolved_storage;
    let candidate = match raw.as_object() {
        Some(obj) => {
            resolved_storage = Value::Object(resolve_exposure_aliases(obj));
            &resolved_storage
        }
        None => raw,
    };
    let opts = PiMcpServerConfigOptions {
        check_metadata_document_callback: Some(Arc::new(|callback_url| {
            if is_client_metadata_document_callback(callback_url) {
                None
            } else {
                Some(
                    "oauth.clientRegistration \"cimd\" requires oauth.callbackUrl on localhost or 127.0.0.1 with path /callback"
                        .to_owned(),
                )
            }
        })),
    };
    match validate_pi_mcp_server_config(name, candidate, Some(&opts)) {
        PiMcpServerConfigResult::Invalid { error } => Err(error),
        PiMcpServerConfigResult::Http(http_cfg) => {
            if let Some(err) =
                check_exposures(&http_cfg.extra).or_else(|| check_auth(http_cfg.extra.get("auth"), &http_cfg.url))
            {
                return Err(format!("server \"{name}\": {err}"));
            }
            let val = serde_json::to_value(&http_cfg).map_err(|err| format!("server \"{name}\": {err}"))?;
            let parsed: McpHttpServerConfig =
                serde_json::from_value(val).map_err(|err| format!("server \"{name}\": {err}"))?;
            Ok(McpServerConfig::Http(parsed))
        }
        PiMcpServerConfigResult::Stdio(stdio_cfg) => {
            if let Some(err) = check_exposures(&stdio_cfg.extra) {
                return Err(format!("server \"{name}\": {err}"));
            }
            let val = serde_json::to_value(&stdio_cfg).map_err(|err| format!("server \"{name}\": {err}"))?;
            let parsed: McpStdioServerConfig =
                serde_json::from_value(val).map_err(|err| format!("server \"{name}\": {err}"))?;
            Ok(McpServerConfig::Stdio(parsed))
        }
    }
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
