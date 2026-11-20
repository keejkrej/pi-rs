//! Port of packages/coding-agent/src/extensions/mcp/config.ts

use std::path::Path;
use std::sync::LazyLock;

use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::config::CONFIG_DIR_NAME;
pub use crate::core::mcp_servers::{
    McpExposure, McpHttpServerConfig, McpOAuthConfig, McpProviderAuthConfig, McpServerConfig, McpStdioServerConfig,
    get_mcp_tool_exposure, validate_mcp_server_config,
};
use crate::utils::text::strip_bom;
use pi_js::{Error, Result};
pub use pi_mcp::mcp_namespace;

const OVERRIDE_KEYS: &[&str] = &["enabled", "exposure", "toolExposure"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpScope {
    Global,
    Project,
    Extension,
}

impl McpScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
            Self::Extension => "extension",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct McpServerEntry {
    pub name: String,
    pub config: McpServerConfig,
    /// Config file that defined the entry, or the path of the extension that registered it.
    pub source: String,
    /// `Global`, `Project`, or `Extension` (`pi.registerMcpServer()`).
    pub scope: Option<McpScope>,
    /// Project `mcp.json` with an override of this global server's `enabled`, `exposure`, or `toolExposure`.
    pub override_path: Option<String>,
}

impl McpServerEntry {
    pub fn auth_provider(&self) -> Option<&str> {
        match &self.config {
            McpServerConfig::Http(h) => h.auth.as_ref().map(|a| a.provider.as_str()),
            McpServerConfig::Stdio(_) => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct LoadedMcpConfig {
    pub servers: Vec<McpServerEntry>,
    pub auto_enable_codemode: Option<bool>,
    pub errors: Vec<String>,
    /// The project `mcp.json` when the project is trusted, where `/mcp` saves project overrides.
    pub project_config: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct LoadMcpConfigOptions {
    pub agent_dir: String,
    pub cwd: String,
    pub project_trusted: bool,
}

#[derive(Default)]
struct McpConfigState {
    servers: IndexMap<String, McpServerEntry>,
    auto_enable_codemode: Option<bool>,
    errors: Vec<String>,
}

fn read_config_file(path: &str, scope: McpScope, state: &mut McpConfigState) {
    if !Path::new(path).exists() {
        return;
    }
    let raw = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) => {
            state.errors.push(format!("{path}: {err}"));
            return;
        }
    };
    let parsed: Value = match pi_js::json::parse(strip_bom(&raw)) {
        Ok(v) => v,
        Err(err) => {
            state.errors.push(format!("{path}: {}", err.message()));
            return;
        }
    };
    let Some(obj) = parsed.as_object() else {
        state
            .errors
            .push(format!("{path}: expected an object with an \"mcpServers\" object"));
        return;
    };
    if let Some(servers_val) = obj.get("mcpServers")
        && !servers_val.is_object()
    {
        state
            .errors
            .push(format!("{path}: expected an object with an \"mcpServers\" object"));
        return;
    }
    match obj.get("autoEnableCodemode") {
        Some(Value::Bool(b)) => state.auto_enable_codemode = Some(*b),
        Some(_) => state
            .errors
            .push(format!("{path}: autoEnableCodemode must be a boolean")),
        None => {}
    }
    let Some(mcp_servers) = obj.get("mcpServers").and_then(Value::as_object) else {
        return;
    };
    for (name, value) in mcp_servers {
        if scope == McpScope::Project
            && let Some(val_obj) = value.as_object()
            && pi_mcp::is_mcp_server_override(val_obj)
        {
            let extra: Vec<&str> = val_obj
                .keys()
                .map(String::as_str)
                .filter(|k| !OVERRIDE_KEYS.contains(k))
                .collect();
            let Some(base) = state.servers.get(name).cloned() else {
                state.errors.push(format!(
                    "{path}: server \"{name}\" needs \"command\" or \"url\", or a global server to override"
                ));
                continue;
            };
            if !extra.is_empty() {
                state.errors.push(format!(
                    "{path}: server \"{name}\": an override can only set {}",
                    OVERRIDE_KEYS.join(", ")
                ));
                continue;
            }
            let base_json = match &base.config {
                McpServerConfig::Stdio(s) => pi_js::json::stringify(s),
                McpServerConfig::Http(h) => pi_js::json::stringify(h),
            };
            let mut merged = pi_js::json::parse(&base_json).unwrap_or_else(|_| Value::Object(Map::new()));
            if let Some(merged_obj) = merged.as_object_mut() {
                for (k, v) in val_obj {
                    merged_obj.insert(k.clone(), v.clone());
                }
            }
            match validate_mcp_server_config(name, &merged) {
                Ok(cfg) => {
                    state.servers.insert(
                        name.clone(),
                        McpServerEntry {
                            name: base.name,
                            config: cfg,
                            source: base.source,
                            scope: base.scope,
                            override_path: Some(path.to_string()),
                        },
                    );
                }
                Err(err) => state.errors.push(format!("{path}: {err}")),
            }
            continue;
        }
        let config = match validate_mcp_server_config(name, value) {
            Ok(cfg) => cfg,
            Err(err) => {
                state.errors.push(format!("{path}: {err}"));
                continue;
            }
        };
        let ns = mcp_namespace(name);
        if let Some(clash) = state
            .servers
            .keys()
            .find(|other| other.as_str() != name && mcp_namespace(other) == ns)
            .cloned()
        {
            state
                .errors
                .push(format!("{path}: server \"{name}\" conflicts with \"{clash}\""));
            continue;
        }
        if scope == McpScope::Project && matches!(&config, McpServerConfig::Http(h) if h.auth.is_some()) {
            state.errors.push(format!(
                "{path}: server \"{name}\": auth is only allowed in the global mcp.json"
            ));
            continue;
        }
        state.servers.insert(
            name.clone(),
            McpServerEntry {
                name: name.clone(),
                config,
                source: path.to_string(),
                scope: Some(scope),
                override_path: None,
            },
        );
    }
}

pub fn load_mcp_config(options: &LoadMcpConfigOptions) -> LoadedMcpConfig {
    let mut state = McpConfigState::default();
    let global_path = pi_js::path::join(&[&options.agent_dir, "mcp.json"]);
    read_config_file(&global_path, McpScope::Global, &mut state);
    let project_config = if options.project_trusted {
        let project_path = pi_js::path::join(&[&options.cwd, *CONFIG_DIR_NAME, "mcp.json"]);
        read_config_file(&project_path, McpScope::Project, &mut state);
        Some(project_path)
    } else {
        None
    };
    LoadedMcpConfig {
        servers: state.servers.into_values().collect(),
        auto_enable_codemode: state.auto_enable_codemode,
        errors: state.errors,
        project_config,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct McpServerConfigPatch {
    pub enabled: Option<bool>,
    pub exposure: Option<McpExposure>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdateMcpConfigOptions {
    pub r#override: bool,
}

static INDENT_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?m)^([ \t]+)\S").expect("valid indent regex"));

fn edit_mcp_servers<F>(path: &str, edit: F) -> Result<()>
where
    F: FnOnce(&mut Option<Map<String, Value>>, &mut Map<String, Value>) -> Result<bool>,
{
    let text = if Path::new(path).exists() {
        let raw = std::fs::read_to_string(path).map_err(|e| Error::from_io(e, "readFile", Some(path), None))?;
        Some(strip_bom(&raw).to_string())
    } else {
        None
    };
    let mut parsed_val = match &text {
        None => Value::Object(Map::new()),
        Some(t) => pi_js::json::parse(t)?,
    };
    let Value::Object(ref mut root) = parsed_val else {
        return Err(Error::msg(format!(
            "{path}: expected an object with an \"mcpServers\" object"
        )));
    };
    if let Some(ms) = root.get("mcpServers")
        && !ms.is_object()
    {
        return Err(Error::msg(format!(
            "{path}: expected an object with an \"mcpServers\" object"
        )));
    }

    let mut servers_opt = root.shift_remove("mcpServers").and_then(|v| match v {
        Value::Object(m) => Some(m),
        _ => None,
    });
    let changed = edit(&mut servers_opt, root)?;
    if let Some(servers_map) = servers_opt
        && !root.contains_key("mcpServers")
    {
        root.insert("mcpServers".to_string(), Value::Object(servers_map));
    }
    if !changed {
        return Ok(());
    }
    let indent = text
        .as_deref()
        .and_then(|t| INDENT_RE.captures(t))
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
        .unwrap_or("  ");
    if let Some(parent) = Path::new(path).parent()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::from_io(e, "mkdir", Some(&parent.to_string_lossy()), None))?;
    }
    let serialized = format!("{}\n", pi_js::json::stringify_pretty(&parsed_val, indent));
    std::fs::write(path, serialized).map_err(|e| Error::from_io(e, "writeFile", Some(path), None))?;
    Ok(())
}

pub fn update_mcp_server_config(
    path: &str,
    name: &str,
    patch: McpServerConfigPatch,
    options: Option<UpdateMcpConfigOptions>,
) -> Result<()> {
    let override_opt = options.is_some_and(|o| o.r#override);
    edit_mcp_servers(path, |servers, _root| {
        if override_opt {
            let map = servers.get_or_insert_with(Map::new);
            if !map.contains_key(name) {
                map.insert(name.to_string(), Value::Object(Map::new()));
            }
        }
        let Some(servers_map) = servers.as_mut() else {
            return Err(Error::msg(format!("{path} does not define MCP server \"{name}\"")));
        };
        let Some(Value::Object(server)) = servers_map.get_mut(name) else {
            return Err(Error::msg(format!("{path} does not define MCP server \"{name}\"")));
        };
        let keep_defaults = pi_mcp::is_mcp_server_override(server);
        if let Some(enabled) = patch.enabled {
            if enabled && !keep_defaults {
                server.shift_remove("enabled");
            } else {
                server.insert("enabled".to_string(), Value::Bool(enabled));
            }
        }
        if let Some(exposure) = patch.exposure {
            if exposure == McpExposure::Codemode && !keep_defaults {
                server.shift_remove("exposure");
            } else if let Ok(val) = pi_js::json::parse(&pi_js::json::stringify(&exposure)) {
                server.insert("exposure".to_string(), val);
            }
        }
        Ok(true)
    })
}

pub fn add_mcp_server_config(path: &str, name: &str, config: &McpServerConfig) -> Result<bool> {
    let mut replaced = false;
    let serialized_str = match config {
        McpServerConfig::Stdio(s) => pi_js::json::stringify(s),
        McpServerConfig::Http(h) => pi_js::json::stringify(h),
    };
    let config_val = pi_js::json::parse(&serialized_str)?;
    edit_mcp_servers(path, |servers, root| {
        let mut target = servers.take().unwrap_or_default();
        replaced = target.contains_key(name);
        target.insert(name.to_string(), config_val);
        root.insert("mcpServers".to_string(), Value::Object(target));
        Ok(true)
    })?;
    Ok(replaced)
}

pub fn remove_mcp_server_config(path: &str, name: &str) -> Result<bool> {
    if !Path::new(path).exists() {
        return Ok(false);
    }
    let mut removed = false;
    edit_mcp_servers(path, |servers, _root| {
        let Some(servers_map) = servers.as_mut() else {
            return Ok(false);
        };
        if !servers_map.contains_key(name) {
            return Ok(false);
        }
        servers_map.shift_remove(name);
        removed = true;
        Ok(true)
    })?;
    Ok(removed)
}
