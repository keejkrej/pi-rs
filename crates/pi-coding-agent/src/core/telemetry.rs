//! Port of packages/coding-agent/src/core/telemetry.ts

use crate::core::settings_manager::SettingsManager;

fn is_truthy_env_flag(value: Option<&str>) -> bool {
    let Some(val) = value else {
        return false;
    };
    if val.is_empty() {
        return false;
    }
    val == "1" || val.eq_ignore_ascii_case("true") || val.eq_ignore_ascii_case("yes")
}

pub fn is_install_telemetry_enabled(settings_manager: &SettingsManager, telemetry_env: Option<&str>) -> bool {
    match telemetry_env {
        Some(val) => is_truthy_env_flag(Some(val)),
        None => {
            let env_val = pi_js::env::var("PI_TELEMETRY");
            if let Some(ref v) = env_val {
                is_truthy_env_flag(Some(v))
            } else {
                settings_manager.get_enable_install_telemetry()
            }
        }
    }
}
