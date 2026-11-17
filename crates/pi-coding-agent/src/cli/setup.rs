//! Port of packages/coding-agent/src/cli/setup.ts

use crate::core::http_dispatcher::configure_http_dispatcher;

pub fn setup_cli() {
    pi_js::env::set_var("PI_CODING_AGENT", "true");
    pi_js::env::set_var("AI_AGENT", "pi");

    // Configure HTTP client before provider SDKs issue requests. Settings are applied
    // once SettingsManager has loaded global/project configuration.
    let _ = configure_http_dispatcher(None);
}
