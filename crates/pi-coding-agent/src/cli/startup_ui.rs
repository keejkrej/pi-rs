//! Port of packages/coding-agent/src/cli/startup-ui.ts

use crate::config::{APP_NAME, CONFIG_DIR_NAME, ENV_AGENT_DIR, PACKAGE_NAME, get_settings_path};
use crate::core::experimental::are_experimental_features_enabled;
use crate::core::settings_manager::SettingsManager;

const OFFICIAL_PACKAGE_NAME: &str = "@earendil-works/pi-coding-agent";
const OFFICIAL_APP_NAME: &str = "pi";
const OFFICIAL_CONFIG_DIR_NAME: &str = ".pi";

#[derive(Clone, Debug)]
pub struct DistributionMetadata {
    pub package_name: String,
    pub app_name: String,
    pub config_dir_name: String,
}

thread_local! {
    static DISTRIBUTION_METADATA_OVERRIDE: pi_js::seam::Slot<dyn Fn() -> DistributionMetadata + Send + Sync> =
        pi_js::seam::Slot::new();
}

fn current_distribution_metadata() -> DistributionMetadata {
    if let Some(f) = DISTRIBUTION_METADATA_OVERRIDE.with(|s| s.get()) {
        return f();
    }
    DistributionMetadata {
        package_name: (*PACKAGE_NAME).to_string(),
        app_name: (*APP_NAME).to_string(),
        config_dir_name: (*CONFIG_DIR_NAME).to_string(),
    }
}

pub fn is_official_distribution(meta: &DistributionMetadata) -> bool {
    meta.package_name == OFFICIAL_PACKAGE_NAME
        && meta.app_name == OFFICIAL_APP_NAME
        && meta.config_dir_name == OFFICIAL_CONFIG_DIR_NAME
}

/// First-time setup runs when all of these hold:
/// - this is the official Pi distribution (not a fork/rebrand)
/// - experimental features are enabled (PI_EXPERIMENTAL=1)
/// - the default agent directory is used (no custom agent dir override)
/// - setup was not completed before (settings.json does not exist)
pub fn should_run_first_time_setup(settings_path: Option<&str>) -> bool {
    let meta = current_distribution_metadata();
    if !is_official_distribution(&meta) {
        return false;
    }
    if !are_experimental_features_enabled() {
        return false;
    }
    if pi_js::env::var(*ENV_AGENT_DIR).is_some_and(|v| !v.is_empty()) {
        return false;
    }
    let resolved = settings_path.map(str::to_string).unwrap_or_else(get_settings_path);
    !pi_js::fs::exists(&resolved)
}

pub async fn show_startup_selector<T: Clone + Send + Sync + 'static>(
    _settings_manager: &SettingsManager,
    _title: &str,
    _options: &[(String, T)],
) -> pi_js::Result<Option<T>> {
    // WIRE(@tui-w3): wire ExtensionSelectorComponent and TuiMainScreen once modes/interactive lands in Stage 3B-2
    Ok(None)
}

/// Show the first-time setup dialog and persist the result
pub async fn show_first_time_setup(_settings_manager: &SettingsManager) -> pi_js::Result<()> {
    // WIRE(@tui-w3): wire FirstTimeSetupComponent and TuiMainScreen once modes/interactive lands in Stage 3B-2
    Ok(())
}

pub async fn show_startup_input(
    _settings_manager: &SettingsManager,
    _title: &str,
    _placeholder: Option<&str>,
) -> pi_js::Result<Option<String>> {
    // WIRE(@tui-w3): wire ExtensionInputComponent and TuiMainScreen once modes/interactive lands in Stage 3B-2
    Ok(None)
}

pub mod testing {
    use std::sync::Arc;

    use super::DistributionMetadata;

    pub fn override_distribution_metadata(
        f: impl Fn() -> DistributionMetadata + Send + Sync + 'static,
    ) -> pi_js::seam::Guard {
        super::DISTRIBUTION_METADATA_OVERRIDE.with(|s| s.set(Arc::new(f)))
    }
}
