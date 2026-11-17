//! Port of packages/coding-agent/test/settings-diagnostics.test.ts

use std::sync::Arc;

use pi_coding_agent::core::settings_diagnostics::{
    AgentSessionRuntimeDiagnostic, AgentSessionRuntimeDiagnosticLevel, collect_settings_diagnostics,
    deduplicate_diagnostics,
};
use pi_coding_agent::core::settings_manager::{SettingsManager, SettingsScope, SettingsStorage};
use pi_js::{Error, Result};
use tempfile::TempDir;

#[test]
fn includes_the_settings_file_path_for_file_backed_storage() {
    let temp = TempDir::new().expect("temp dir");
    let agent_dir = temp.path().join("agent");
    let settings_path = agent_dir.join("settings.json");
    std::fs::create_dir_all(&agent_dir).expect("mkdir agent_dir");
    std::fs::write(&settings_path, "{").expect("write invalid settings.json");

    let manager = SettingsManager::create(
        temp.path().to_str().expect("utf8"),
        Some(agent_dir.to_str().expect("utf8")),
        None,
    );
    let diagnostics = collect_settings_diagnostics(&manager);

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].r#type, AgentSessionRuntimeDiagnosticLevel::Warning);
    assert!(
        diagnostics[0].message.contains(&format!(
            "Invalid settings file {}:",
            settings_path.to_str().expect("utf8")
        )),
        "unexpected message: {}",
        diagnostics[0].message
    );
}

struct FailingGlobalStorage;

impl SettingsStorage for FailingGlobalStorage {
    fn with_lock_dyn(
        &self,
        scope: SettingsScope,
        f: &mut dyn FnMut(Option<&str>) -> Result<Option<String>>,
    ) -> Result<()> {
        if scope == SettingsScope::Global {
            return Err(Error::msg("backend failed"));
        }
        let _ = f(None)?;
        Ok(())
    }
}

#[test]
fn falls_back_to_the_settings_scope_for_storage_without_file_paths() {
    let manager = SettingsManager::from_storage(Arc::new(FailingGlobalStorage), None);
    let diagnostics = collect_settings_diagnostics(&manager);

    pretty_assertions::assert_eq!(
        diagnostics,
        vec![AgentSessionRuntimeDiagnostic {
            r#type: AgentSessionRuntimeDiagnosticLevel::Warning,
            message: "Invalid global settings: backend failed".to_owned(),
        }]
    );
}

#[test]
fn deduplicates_diagnostics_by_type_and_message() {
    let warning = AgentSessionRuntimeDiagnostic {
        r#type: AgentSessionRuntimeDiagnosticLevel::Warning,
        message: "Invalid settings file /tmp/settings.json".to_owned(),
    };
    let error = AgentSessionRuntimeDiagnostic {
        r#type: AgentSessionRuntimeDiagnosticLevel::Error,
        message: warning.message.clone(),
    };

    pretty_assertions::assert_eq!(
        deduplicate_diagnostics(&[warning.clone(), warning.clone(), error.clone()]),
        vec![warning, error]
    );
}
