//! Port of packages/ai/src/utils/pi-user-agent.ts

pub fn get_pi_user_agent() -> String {
    format!(
        "pi ({} {}; {})",
        pi_js::env::platform(),
        pi_js::env::os_release(),
        pi_js::env::arch(),
    )
}
