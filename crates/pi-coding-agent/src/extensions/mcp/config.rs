//! Port of packages/coding-agent/src/extensions/mcp/config.ts

use std::path::Path;
use std::sync::LazyLock;

use indexmap::IndexMap;
use serde_json::{Map, Value};

pub use crate::core::extensions::types::mcp_wire::{
    McpExposure, McpHttpServerConfig, McpOAuthConfig, McpServerConfig, McpStdioServerConfig, mcp_namespace,
};
use crate::extensions::wire::{CONFIG_DIR_NAME, strip_bom};
pub use crate::extensions::wire::{get_mcp_tool_exposure, validate_mcp_server_config};
use pi_js::{Error, Result};

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerEntry {
    pub name: String,
    pub config: McpServerConfig,
    // WIRE(@mcp-w3): move into McpHttpServerConfig.auth once types.rs adds auth field
    pub auth_provider: Option<String>,
    /// Config file that defined the entry, or the path of the extension that registered it.
    pub source: String,
    /// `Global`, `Project`, or `Extension` (`pi.registerMcpServer()`).
    pub scope: Option<McpScope>,
}

#[derive(Clone, Debug, Default)]
pub struct LoadedMcpConfig {
    pub servers: Vec<McpServerEntry>,
    pub auto_enable_codemode: Option<bool>,
    pub errors: Vec<String>,
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
        let (config, auth_provider) = match validate_mcp_server_config(name, value) {
            Ok(pair) => pair,
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
        if scope == McpScope::Project && matches!(config, McpServerConfig::Http(_)) && auth_provider.is_some() {
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
                auth_provider,
                source: path.to_string(),
                scope: Some(scope),
            },
        );
    }
}

pub fn load_mcp_config(options: &LoadMcpConfigOptions) -> LoadedMcpConfig {
    let mut state = McpConfigState::default();
    let global_path = pi_js::path::join(&[&options.agent_dir, "mcp.json"]);
    read_config_file(&global_path, McpScope::Global, &mut state);
    if options.project_trusted {
        let project_path = pi_js::path::join(&[&options.cwd, CONFIG_DIR_NAME, "mcp.json"]);
        read_config_file(&project_path, McpScope::Project, &mut state);
    }
    LoadedMcpConfig {
        servers: state.servers.into_values().collect(),
        auto_enable_codemode: state.auto_enable_codemode,
        errors: state.errors,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct McpServerConfigPatch {
    pub enabled: Option<bool>,
    pub exposure: Option<McpExposure>,
}

static INDENT_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?m)^([ \t]+)\S").expect("valid indent regex"));

fn edit_mcp_servers<F>(path: &str, edit: F) -> Result<()>
where
    F: FnOnce(Option<&mut Map<String, Value>>, &mut Map<String, Value>) -> Result<bool>,
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
    let changed = edit(servers_opt.as_mut(), root)?;
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

pub fn update_mcp_server_config(path: &str, name: &str, patch: McpServerConfigPatch) -> Result<()> {
    edit_mcp_servers(path, |servers, _root| {
        let Some(servers_map) = servers else {
            return Err(Error::msg(format!("{path} does not define MCP server \"{name}\"")));
        };
        let Some(Value::Object(server)) = servers_map.get_mut(name) else {
            return Err(Error::msg(format!("{path} does not define MCP server \"{name}\"")));
        };
        if let Some(enabled) = patch.enabled {
            if enabled {
                server.shift_remove("enabled");
            } else {
                server.insert("enabled".to_string(), Value::Bool(false));
            }
        }
        if let Some(exposure) = patch.exposure {
            if exposure == McpExposure::Codemode {
                server.shift_remove("exposure");
            } else if let Ok(val) = pi_js::json::parse(&pi_js::json::stringify(&exposure)) {
                server.insert("exposure".to_string(), val);
            }
        }
        Ok(true)
    })
}

pub fn add_mcp_server_config(
    path: &str,
    name: &str,
    config: &McpServerConfig,
    auth_provider: Option<&str>,
) -> Result<bool> {
    let mut replaced = false;
    let serialized_str = match config {
        McpServerConfig::Stdio(s) => pi_js::json::stringify(s),
        McpServerConfig::Http(h) => pi_js::json::stringify(h),
    };
    let mut config_val = pi_js::json::parse(&serialized_str)?;
    if let Some(provider) = auth_provider
        && let Some(obj) = config_val.as_object_mut()
    {
        obj.insert("auth".to_string(), serde_json::json!({ "provider": provider }));
    }
    edit_mcp_servers(path, |servers, root| {
        let mut target = servers.cloned().unwrap_or_default();
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
        let Some(servers_map) = servers else {
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
