//! Port of packages/mcp/src/config.ts

use std::sync::Arc;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::oauth::client_settings::McpClientRegistration;
use crate::oauth::types::is_http_url;

/// A server started as a local program that speaks MCP over stdin and stdout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStdioServerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Added to the environment. The application decides whether values may reference variables (`${NAME}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<IndexMap<String, String>>,
    /// The application decides what relative paths resolve against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// What the server offers, in a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Set to false to keep the entry without connecting. Default: true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Per-request timeout in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    #[serde(default, flatten)]
    pub extra: IndexMap<String, Value>,
}

/// OAuth client settings for servers that do not support dynamic client registration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthConfig {
    /// Pre-registered client id. Without it, the application registers a client with the authorization server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// As configured; the application decides whether it may reference variables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// Port of the loopback callback server, for clients registered with a fixed redirect URI. Without
    /// `callbackUrl`, the redirect URI is `http://127.0.0.1:<port>/callback`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_port: Option<i64>,
    /// Redirect URI registered for `clientId`, for example `http://localhost:8080/oauth/callback`. It must
    /// be an `http` URI on `localhost`, `127.0.0.1`, or `[::1]`. Without a port, the callback server
    /// listens on `callbackPort` or a free port, which is added to the URI (RFC 8252).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_url: Option<String>,
    /// Scopes to request, separated by spaces. Default: the scopes the server advertises.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// `client_name` sent with dynamic client registration. Default: the application's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    /// How the application identifies itself without `clientId`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_registration: Option<McpClientRegistration>,
    /// Authorization server metadata document (RFC 8414 or OpenID Connect discovery) to use instead of
    /// discovery through the server, for servers that advertise a wrong authorization server or none.
    /// The document is trusted as configured. Must use https, except on loopback hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_server_metadata_url: Option<String>,
    #[serde(default, flatten)]
    pub extra: IndexMap<String, Value>,
}

/// A server reached over Streamable HTTP.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpHttpServerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    pub url: String,
    /// Sent with every request. The application decides whether values may reference variables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthConfig>,
    /// What the server offers, in a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Set to false to keep the entry without connecting. Default: true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Per-request timeout in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    #[serde(default, flatten)]
    pub extra: IndexMap<String, Value>,
}

/// One entry of the `mcpServers` shape: a stdio or a Streamable HTTP server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpServerConfig {
    Stdio(McpStdioServerConfig),
    Http(McpHttpServerConfig),
}

/// A validated server entry, tagged with its transport, or why it is invalid. Unknown fields are preserved
/// in each config's `extra` map for the caller to check afterwards.
#[derive(Clone, Debug, PartialEq)]
pub enum McpServerConfigResult {
    Stdio(McpStdioServerConfig),
    Http(McpHttpServerConfig),
    Invalid { error: String },
}

pub type CheckMetadataDocumentCallback = Arc<dyn Fn(Option<&str>) -> Option<String> + Send + Sync>;

/// What the application adds to the validation of server entries.
#[derive(Clone, Default)]
pub struct McpServerConfigOptions {
    /// Checks the `oauth.callbackUrl` (`None`: none configured) of an entry with
    /// `oauth.clientRegistration: "cimd"`: the application's Client ID Metadata Documents must list it.
    /// Returns an error message. Without this check, `"cimd"` is refused.
    pub check_metadata_document_callback: Option<CheckMetadataDocumentCallback>,
}

const LOOPBACK_HOSTS: &[&str] = &["localhost", "127.0.0.1", "[::1]"];

fn is_valid_server_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Validate one entry of the `mcpServers` shape (configuration files are a trust boundary). Server
/// names are letters, digits, `_` and `-`, which tool names and namespaces are built from.
pub fn validate_mcp_server_config(
    name: &str,
    raw: &Value,
    options: Option<&McpServerConfigOptions>,
) -> McpServerConfigResult {
    let invalid = |error: String| McpServerConfigResult::Invalid { error };
    if !is_valid_server_name(name) {
        return invalid(format!(
            "invalid server name \"{name}\" (use letters, digits, \"_\" and \"-\")"
        ));
    }
    let Some(entry) = raw.as_object() else {
        return invalid(format!("server \"{name}\" must be an object"));
    };
    if let Some(fields_error) = validate_base_fields(entry) {
        return invalid(format!("server \"{name}\": {fields_error}"));
    }
    let entry_type = entry.get("type").and_then(Value::as_str);
    let has_type = entry.contains_key("type");
    if entry_type == Some("sse") {
        return invalid(format!(
            "server \"{name}\": legacy SSE transport is not supported; use the streamable HTTP URL"
        ));
    }
    let default_options = McpServerConfigOptions::default();
    let options = options.unwrap_or(&default_options);

    if let Some(url) = entry.get("url").and_then(Value::as_str)
        && (!has_type || matches!(entry_type, Some("http" | "streamable-http")))
    {
        if let Some(err) = validate_http_fields(entry, url, options) {
            return invalid(format!("server \"{name}\": {err}"));
        }
        return match build_http_config(entry) {
            Ok(config) => McpServerConfigResult::Http(config),
            Err(err) => invalid(format!("server \"{name}\": {err}")),
        };
    }

    if entry.get("command").and_then(Value::as_str).is_some() && (!has_type || entry_type == Some("stdio")) {
        if let Some(err) = validate_stdio_fields(entry) {
            return invalid(format!("server \"{name}\": {err}"));
        }
        return match build_stdio_config(entry) {
            Ok(config) => McpServerConfigResult::Stdio(config),
            Err(err) => invalid(format!("server \"{name}\": {err}")),
        };
    }

    invalid(format!(
        "server \"{name}\" needs either \"command\" (stdio) or \"url\" (streamable HTTP)"
    ))
}

fn validate_base_fields(entry: &Map<String, Value>) -> Option<String> {
    if let Some(enabled) = entry.get("enabled")
        && !enabled.is_boolean()
    {
        return Some("enabled must be a boolean".to_string());
    }
    if let Some(description) = entry.get("description")
        && !description.is_string()
    {
        return Some("description must be a string".to_string());
    }
    if let Some(timeout) = entry.get("timeout") {
        let valid = timeout.as_f64().is_some_and(|n| n.is_finite() && n > 0.0);
        if !valid {
            return Some("timeout must be a positive number of seconds".to_string());
        }
    }
    None
}

fn validate_http_fields(entry: &Map<String, Value>, url: &str, options: &McpServerConfigOptions) -> Option<String> {
    let Ok(parsed) = url::Url::parse(url) else {
        return Some("url must be an http or https URL".to_string());
    };
    if !is_http_url(&parsed) {
        return Some("url must be an http or https URL".to_string());
    }
    if let Some(headers) = entry.get("headers")
        && !is_string_record(headers)
    {
        return Some("headers must map names to strings".to_string());
    }
    if let Some(oauth) = entry.get("oauth") {
        return validate_oauth(oauth, options);
    }
    None
}

fn validate_stdio_fields(entry: &Map<String, Value>) -> Option<String> {
    if let Some(args) = entry.get("args") {
        let valid = args.as_array().is_some_and(|arr| arr.iter().all(Value::is_string));
        if !valid {
            return Some("args must be an array of strings".to_string());
        }
    }
    if let Some(env) = entry.get("env")
        && !is_string_record(env)
    {
        return Some("env must map names to strings".to_string());
    }
    if let Some(cwd) = entry.get("cwd")
        && !cwd.is_string()
    {
        return Some("cwd must be a string".to_string());
    }
    None
}

fn is_string_record(value: &Value) -> bool {
    value.as_object().is_some_and(|obj| obj.values().all(Value::is_string))
}

fn parse_port(value: &Value) -> Option<i64> {
    if let Some(p) = value.as_i64() {
        return (1..=65535).contains(&p).then_some(p);
    }
    if let Some(f) = value.as_f64()
        && f.is_finite()
        && f.fract() == 0.0
        && (1.0..=65535.0).contains(&f)
    {
        return Some(f as i64);
    }
    None
}

fn validate_oauth(value: &Value, options: &McpServerConfigOptions) -> Option<String> {
    let Some(obj) = value.as_object() else {
        return Some("oauth must be an object".to_string());
    };
    if let Some(id) = obj.get("clientId")
        && !id.is_string()
    {
        return Some("oauth.clientId must be a string".to_string());
    }
    if let Some(secret) = obj.get("clientSecret")
        && !secret.is_string()
    {
        return Some("oauth.clientSecret must be a string".to_string());
    }
    let port = if let Some(raw_port) = obj.get("callbackPort") {
        let Some(p) = parse_port(raw_port) else {
            return Some("oauth.callbackPort must be a port number".to_string());
        };
        Some(p)
    } else {
        None
    };
    if let Some(raw_cb) = obj.get("callbackUrl") {
        let Some(cb_str) = raw_cb.as_str() else {
            return Some(
                "oauth.callbackUrl must be an http URI on localhost, 127.0.0.1, or [::1] without query or fragment"
                    .to_string(),
            );
        };
        let Some(parsed) = parse_loopback_redirect_uri(cb_str) else {
            return Some(
                "oauth.callbackUrl must be an http URI on localhost, 127.0.0.1, or [::1] without query or fragment"
                    .to_string(),
            );
        };
        if let (Some(url_port), Some(cfg_port)) = (parsed.port(), port)
            && i64::from(url_port) != cfg_port
        {
            return Some("oauth.callbackUrl and oauth.callbackPort name different ports".to_string());
        }
    }
    if let Some(scope) = obj.get("scope")
        && !scope.is_string()
    {
        return Some("oauth.scope must be a string".to_string());
    }
    if let Some(client_name) = obj.get("clientName")
        && !client_name.as_str().is_some_and(|s| !pi_js::str16::trim(s).is_empty())
    {
        return Some("oauth.clientName must be a non-empty string".to_string());
    }
    if let Some(reg_val) = obj.get("clientRegistration") {
        let reg = reg_val.as_str();
        if !matches!(reg, Some("auto" | "dcr")) {
            if reg != Some("cimd") {
                return Some("oauth.clientRegistration must be \"auto\", \"dcr\", or \"cimd\"".to_string());
            }
            if obj.contains_key("clientId") || obj.contains_key("clientName") {
                return Some(
                    "oauth.clientRegistration \"cimd\" cannot be combined with oauth.clientId or oauth.clientName"
                        .to_string(),
                );
            }
            let Some(check) = &options.check_metadata_document_callback else {
                return Some(
                    "oauth.clientRegistration \"cimd\" is not supported: there are no Client ID Metadata Documents"
                        .to_string(),
                );
            };
            let cb_url = obj.get("callbackUrl").and_then(Value::as_str);
            if let Some(err) = check(cb_url) {
                return Some(err);
            }
        }
    }
    if let Some(raw_meta) = obj.get("authServerMetadataUrl") {
        let valid = raw_meta
            .as_str()
            .and_then(|s| url::Url::parse(s).ok())
            .is_some_and(|u| {
                u.scheme() == "https" || (u.scheme() == "http" && LOOPBACK_HOSTS.contains(&u.host_str().unwrap_or("")))
            });
        if !valid {
            return Some(
                "oauth.authServerMetadataUrl must be an https URL, or http on localhost, 127.0.0.1, or [::1]"
                    .to_string(),
            );
        }
    }
    None
}

/// Whether a redirect URI can be served by a loopback callback server.
fn parse_loopback_redirect_uri(value: &str) -> Option<url::Url> {
    let url = url::Url::parse(value).ok()?;
    let host = url.host_str().unwrap_or("");
    let ok =
        url.scheme() == "http" && LOOPBACK_HOSTS.contains(&host) && url.query().is_none() && url.fragment().is_none();
    ok.then_some(url)
}

fn string_map(value: &Value) -> IndexMap<String, String> {
    let mut map = IndexMap::new();
    if let Some(obj) = value.as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                map.insert(k.clone(), s.to_string());
            }
        }
    }
    map
}

fn build_oauth_config(obj: &Map<String, Value>) -> Result<McpOAuthConfig, String> {
    let mut cfg = McpOAuthConfig {
        client_id: obj.get("clientId").and_then(Value::as_str).map(str::to_string),
        client_secret: obj.get("clientSecret").and_then(Value::as_str).map(str::to_string),
        callback_port: obj.get("callbackPort").and_then(parse_port),
        callback_url: obj.get("callbackUrl").and_then(Value::as_str).map(str::to_string),
        scope: obj.get("scope").and_then(Value::as_str).map(str::to_string),
        client_name: obj.get("clientName").and_then(Value::as_str).map(str::to_string),
        client_registration: match obj.get("clientRegistration").and_then(Value::as_str) {
            Some("auto") => Some(McpClientRegistration::Auto),
            Some("dcr") => Some(McpClientRegistration::Dcr),
            Some("cimd") => Some(McpClientRegistration::Cimd),
            _ => None,
        },
        auth_server_metadata_url: obj
            .get("authServerMetadataUrl")
            .and_then(Value::as_str)
            .map(str::to_string),
        extra: IndexMap::new(),
    };
    for (k, v) in obj {
        if !matches!(
            k.as_str(),
            "clientId"
                | "clientSecret"
                | "callbackPort"
                | "callbackUrl"
                | "scope"
                | "clientName"
                | "clientRegistration"
                | "authServerMetadataUrl"
        ) {
            cfg.extra.insert(k.clone(), v.clone());
        }
    }
    Ok(cfg)
}

fn build_http_config(entry: &Map<String, Value>) -> Result<McpHttpServerConfig, String> {
    let url = entry
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| "url is required".to_string())?
        .to_string();
    let oauth = match entry.get("oauth").and_then(Value::as_object) {
        Some(obj) => Some(build_oauth_config(obj)?),
        None => None,
    };
    let mut extra = IndexMap::new();
    for (k, v) in entry {
        if !matches!(
            k.as_str(),
            "type" | "url" | "headers" | "oauth" | "description" | "enabled" | "timeout"
        ) {
            extra.insert(k.clone(), v.clone());
        }
    }
    Ok(McpHttpServerConfig {
        r#type: entry.get("type").and_then(Value::as_str).map(str::to_string),
        url,
        headers: entry.get("headers").map(string_map),
        oauth,
        description: entry.get("description").and_then(Value::as_str).map(str::to_string),
        enabled: entry.get("enabled").and_then(Value::as_bool),
        timeout: entry.get("timeout").and_then(Value::as_f64),
        extra,
    })
}

fn build_stdio_config(entry: &Map<String, Value>) -> Result<McpStdioServerConfig, String> {
    let command = entry
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| "command is required".to_string())?
        .to_string();
    let args = entry
        .get("args")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect());
    let mut extra = IndexMap::new();
    for (k, v) in entry {
        if !matches!(
            k.as_str(),
            "type" | "command" | "args" | "env" | "cwd" | "description" | "enabled" | "timeout"
        ) {
            extra.insert(k.clone(), v.clone());
        }
    }
    Ok(McpStdioServerConfig {
        r#type: entry.get("type").and_then(Value::as_str).map(str::to_string),
        command,
        args,
        env: entry.get("env").map(string_map),
        cwd: entry.get("cwd").and_then(Value::as_str).map(str::to_string),
        description: entry.get("description").and_then(Value::as_str).map(str::to_string),
        enabled: entry.get("enabled").and_then(Value::as_bool),
        timeout: entry.get("timeout").and_then(Value::as_f64),
        extra,
    })
}

/// Whether an entry overrides a server defined elsewhere instead of defining one: it has no `command`,
/// `url` or `type`. Applications let a less trusted file (a project's) override a few fields, such as
/// `enabled`, of a server a trusted file defines; which fields is theirs to decide.
pub fn is_mcp_server_override(entry: &Map<String, Value>) -> bool {
    !entry.contains_key("command") && !entry.contains_key("url") && !entry.contains_key("type")
}
