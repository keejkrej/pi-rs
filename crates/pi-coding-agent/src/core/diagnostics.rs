//! Port of packages/coding-agent/src/core/diagnostics.ts

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResourceType {
    Extension,
    Skill,
    Prompt,
    Theme,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceCollision {
    pub resource_type: ResourceType,
    pub name: String,
    pub winner_path: String,
    pub loser_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loser_source: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResourceDiagnosticType {
    Warning,
    Error,
    Collision,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDiagnostic {
    pub r#type: ResourceDiagnosticType,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collision: Option<ResourceCollision>,
}

// WIRE(@ca-cli-w3): Shared diagnostic shape used by core/settings_diagnostics and core/agent_session_services.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentSessionRuntimeDiagnosticLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionRuntimeDiagnostic {
    pub r#type: AgentSessionRuntimeDiagnosticLevel,
    pub message: String,
}
