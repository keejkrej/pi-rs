//! Port of packages/coding-agent/src/cli/config-selector.ts

//! TUI config selector for `pi config` command

use crate::core::package_manager::ResolvedPaths;
use crate::core::settings_manager::SettingsManager;

#[derive(Clone)]
pub struct ConfigSelectorOptions {
    pub resolved_paths_global: ResolvedPaths,
    pub resolved_paths_project: ResolvedPaths,
    pub settings_manager: SettingsManager,
    pub cwd: String,
    pub agent_dir: String,
    pub write_scope: crate::core::settings_manager::SettingsScope,
    pub project_mode_available: bool,
}

/// Show TUI config selector and return when closed
pub async fn select_config(_options: ConfigSelectorOptions) -> pi_js::Result<()> {
    // WIRE(@tui-w3): wire ConfigSelectorComponent and theme init/stop once modes/interactive lands in Stage 3B-2
    Err(pi_js::Error::msg(
        "Config selector TUI requires interactive mode (Stage 3B-2)",
    ))
}
