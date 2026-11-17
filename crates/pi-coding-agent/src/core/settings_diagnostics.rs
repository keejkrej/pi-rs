//! Port of packages/coding-agent/src/core/settings-diagnostics.ts

use indexmap::IndexSet;

pub use crate::core::diagnostics::{AgentSessionRuntimeDiagnostic, AgentSessionRuntimeDiagnosticLevel};
use crate::core::settings_manager::SettingsManager;

pub fn collect_settings_diagnostics(settings_manager: &SettingsManager) -> Vec<AgentSessionRuntimeDiagnostic> {
    settings_manager
        .drain_errors()
        .into_iter()
        .map(|err| {
            let scope = match err.scope {
                crate::core::settings_manager::SettingsScope::Global => "global",
                crate::core::settings_manager::SettingsScope::Project => "project",
            };
            let message = match err.path {
                Some(path) => format!("Invalid settings file {path}: {}", err.error),
                None => format!("Invalid {scope} settings: {}", err.error),
            };
            AgentSessionRuntimeDiagnostic {
                r#type: AgentSessionRuntimeDiagnosticLevel::Warning,
                message,
            }
        })
        .collect()
}

/// Remove duplicate type/message diagnostics while preserving their first occurrence.
pub fn deduplicate_diagnostics(diagnostics: &[AgentSessionRuntimeDiagnostic]) -> Vec<AgentSessionRuntimeDiagnostic> {
    let mut seen: IndexSet<String> = IndexSet::new();
    let mut out = Vec::with_capacity(diagnostics.len());
    for diagnostic in diagnostics {
        let tag = match diagnostic.r#type {
            AgentSessionRuntimeDiagnosticLevel::Info => "info",
            AgentSessionRuntimeDiagnosticLevel::Warning => "warning",
            AgentSessionRuntimeDiagnosticLevel::Error => "error",
        };
        let key = format!("{tag}\0{}", diagnostic.message);
        if seen.insert(key) {
            out.push(diagnostic.clone());
        }
    }
    out
}
