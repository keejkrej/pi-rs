//! Port of packages/coding-agent/examples/extensions/subagent/agents.ts

use indexmap::IndexMap;
use pi_coding_agent::config::{CONFIG_DIR_NAME, get_agent_dir};
use pi_coding_agent::utils::frontmatter::parse_frontmatter;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentScope {
    User,
    Project,
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentSource {
    User,
    Project,
}

impl AgentSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub system_prompt: String,
    pub source: AgentSource,
    pub file_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentDiscoveryResult {
    pub agents: Vec<AgentConfig>,
    pub project_agents_dir: Option<String>,
}

fn parse_tool_list(value: Option<&Value>) -> Option<Vec<String>> {
    let raw: Vec<String> = match value {
        Some(Value::Array(arr)) => arr.iter().filter_map(|v| v.as_str().map(ToString::to_string)).collect(),
        Some(Value::String(s)) => s.split(',').map(ToString::to_string).collect(),
        _ => Vec::new(),
    };
    let tools: Vec<String> = raw
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if tools.is_empty() { None } else { Some(tools) }
}

fn load_agents_from_dir(dir: &str, source: AgentSource) -> Vec<AgentConfig> {
    let mut agents = Vec::new();
    if !pi_js::fs::exists(dir) {
        return agents;
    }

    let Ok(entries) = pi_js::fs::readdir_with_file_types(dir) else {
        return agents;
    };

    for entry in entries {
        if !entry.name.ends_with(".md") {
            continue;
        }
        if !entry.is_file() && !entry.is_symbolic_link() {
            continue;
        }

        let file_path = pi_js::path::join(&[dir, &entry.name]);
        let Ok(content) = pi_js::fs::read_to_string(&file_path) else {
            continue;
        };

        let parsed = parse_frontmatter(&content);
        let Some(name) = parsed.frontmatter.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(description) = parsed.frontmatter.get("description").and_then(|v| v.as_str()) else {
            continue;
        };

        let model = parsed
            .frontmatter
            .get("model")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);

        agents.push(AgentConfig {
            name: name.to_string(),
            description: description.to_string(),
            tools: parse_tool_list(parsed.frontmatter.get("tools")),
            model,
            system_prompt: parsed.body,
            source,
            file_path,
        });
    }

    agents
}

fn is_directory(p: &str) -> bool {
    pi_js::fs::stat(p).is_ok_and(|s| s.is_directory())
}

fn find_nearest_project_agents_dir(cwd: &str) -> Option<String> {
    let mut current_dir = cwd.to_string();
    loop {
        let candidate = pi_js::path::join(&[&current_dir, *CONFIG_DIR_NAME, "agents"]);
        if is_directory(&candidate) {
            return Some(candidate);
        }
        let parent_dir = pi_js::path::dirname(&current_dir);
        if parent_dir == current_dir {
            return None;
        }
        current_dir = parent_dir;
    }
}

pub fn discover_agents(cwd: &str, scope: AgentScope) -> AgentDiscoveryResult {
    let user_dir = pi_js::path::join(&[&get_agent_dir(), "agents"]);
    let project_agents_dir = find_nearest_project_agents_dir(cwd);

    let user_agents = if scope == AgentScope::Project {
        Vec::new()
    } else {
        load_agents_from_dir(&user_dir, AgentSource::User)
    };
    let project_agents = if scope == AgentScope::User {
        Vec::new()
    } else if let Some(dir) = project_agents_dir.as_deref() {
        load_agents_from_dir(dir, AgentSource::Project)
    } else {
        Vec::new()
    };

    let mut agent_map: IndexMap<String, AgentConfig> = IndexMap::new();
    match scope {
        AgentScope::Both => {
            for agent in user_agents {
                agent_map.insert(agent.name.clone(), agent);
            }
            for agent in project_agents {
                agent_map.insert(agent.name.clone(), agent);
            }
        }
        AgentScope::User => {
            for agent in user_agents {
                agent_map.insert(agent.name.clone(), agent);
            }
        }
        AgentScope::Project => {
            for agent in project_agents {
                agent_map.insert(agent.name.clone(), agent);
            }
        }
    }

    AgentDiscoveryResult {
        agents: agent_map.into_values().collect(),
        project_agents_dir,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormattedAgentList {
    pub text: String,
    pub remaining: usize,
}

pub fn format_agent_list(agents: &[AgentConfig], max_items: usize) -> FormattedAgentList {
    if agents.is_empty() {
        return FormattedAgentList {
            text: "none".to_string(),
            remaining: 0,
        };
    }
    let listed = &agents[..agents.len().min(max_items)];
    let remaining = agents.len().saturating_sub(listed.len());
    let text = listed
        .iter()
        .map(|a| format!("{} ({}): {}", a.name, a.source.as_str(), a.description))
        .collect::<Vec<_>>()
        .join("; ");
    FormattedAgentList { text, remaining }
}
