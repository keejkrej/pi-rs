//! Port of packages/coding-agent/src/core/client-identity.ts

use pi_ai::utils::identity::{ClientIdentity, UserAgentMode, pi_attribution};

use crate::config::VERSION;
use crate::core::settings_manager::SettingsManager;
use crate::core::telemetry::is_install_telemetry_enabled;

/// How pi's requests identify it to providers: a detailed `User-Agent` with pi's version,
/// and attribution to pi as a CLI agent while install telemetry is enabled.
pub fn get_client_identity(settings_manager: &SettingsManager) -> ClientIdentity {
    ClientIdentity {
        user_agent: Some(UserAgentMode::DETAILED),
        version: Some((*VERSION).to_owned()),
        attribution: Some(if is_install_telemetry_enabled(settings_manager, None) {
            Some(pi_attribution())
        } else {
            None
        }),
    }
}
