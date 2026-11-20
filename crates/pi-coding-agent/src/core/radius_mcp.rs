//! Port of packages/coding-agent/src/core/radius-mcp.ts

use indexmap::IndexSet;
use pi_mcp::naming::mcp_namespace;

use crate::core::mcp_servers::{McpHttpServerConfig, McpProviderAuthConfig, McpServerConfig};
use crate::core::radius::RADIUS_MCP_URL;
use crate::extensions::mcp::config::{LoadMcpConfigOptions, load_mcp_config};

/// Change to the global `mcp.json` that points the Radius MCP server at a Radius login.
#[derive(Clone, Debug, PartialEq)]
pub struct RadiusMcpServerPlan {
    pub mcp_path: String,
    pub name: String,
    pub config: McpHttpServerConfig,
}

fn normalize_url(url: &str) -> &str {
    url.trim_end_matches('/')
}

/// The global `mcp.json` entry that makes the Radius MCP server use the login of `provider_id`,
/// adding the server when missing, or `None` when a global server already uses that login.
pub fn plan_radius_mcp_server(provider_id: &str, agent_dir: &str, cwd: &str) -> Option<RadiusMcpServerPlan> {
    let target_url = (*RADIUS_MCP_URL).clone();
    let loaded = load_mcp_config(&LoadMcpConfigOptions {
        agent_dir: agent_dir.to_owned(),
        cwd: cwd.to_owned(),
        project_trusted: false,
    });
    let existing = loaded.servers.iter().find(|server| match &server.config {
        McpServerConfig::Http(http) => normalize_url(&http.url) == normalize_url(&target_url),
        McpServerConfig::Stdio(_) => false,
    });
    if let Some(entry) = existing
        && let McpServerConfig::Http(ref http) = entry.config
        && http.auth.as_ref().is_some_and(|auth| auth.provider == provider_id)
    {
        return None;
    }

    let name = match existing {
        Some(entry) => entry.name.clone(),
        None => {
            let namespaces: IndexSet<String> = loaded.servers.iter().map(|s| mcp_namespace(&s.name)).collect();
            let mut candidate = "radius".to_owned();
            let mut suffix = 1usize;
            while namespaces.contains(&mcp_namespace(&candidate)) {
                candidate = if suffix == 1 {
                    "radius-mcp".to_owned()
                } else {
                    format!("radius-mcp-{suffix}")
                };
                suffix += 1;
            }
            candidate
        }
    };

    let mut config = match existing {
        Some(entry) => match &entry.config {
            McpServerConfig::Http(http) => http.clone(),
            McpServerConfig::Stdio(_) => McpHttpServerConfig {
                r#type: None,
                exposure: None,
                description: None,
                tool_exposure: None,
                enabled: None,
                timeout: None,
                url: (*RADIUS_MCP_URL).clone(),
                headers: None,
                oauth: None,
                auth: None,
            },
        },
        None => McpHttpServerConfig {
            r#type: None,
            exposure: None,
            description: None,
            tool_exposure: None,
            enabled: None,
            timeout: None,
            url: target_url,
            headers: None,
            oauth: None,
            auth: None,
        },
    };
    config.auth = Some(McpProviderAuthConfig {
        provider: provider_id.to_owned(),
    });
    // `auth` replaces the MCP OAuth sign-in.
    config.oauth = None;

    Some(RadiusMcpServerPlan {
        mcp_path: pi_js::path::join(&[agent_dir, "mcp.json"]),
        name,
        config,
    })
}
